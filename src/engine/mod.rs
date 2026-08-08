//! The RPN calculator engine: an [`Engine`] wrapping a stack of values (and,
//! later, evaluation settings). No I/O and no history. The individual ops take
//! `&mut self` and return `Result<(), ErrorKind>` — they pop, mutate in place,
//! and bail with `?`. [`Engine::apply`] runs a whole program (a slice of
//! [`Element`]s) and is the consuming boundary: it threads one engine through
//! the batch and, on the first failure, attaches the [`Trace`] to make a
//! [`CalcError`]. Turning a line of text into a program is a frontend concern,
//! handled by [`parse`].
//!
//! **Atomicity is the caller's, not the op's.** An op may leave the stack
//! half-consumed when it fails (it pops before it type-checks), so there is no
//! "operands intact on error" guarantee. That doesn't matter, because every
//! caller applies to a *copy* and commits only on `Ok` (see the `history`
//! module) — a failed batch's damage is confined to a discarded clone, and the
//! caller's own engine is untouched. This is the standard transactional model.

use std::rc::Rc;

mod error;
mod ops;
mod program;
mod token;
mod value;

pub use error::{CalcError, ErrorKind, Outcome, ParseError, ParseErrorKind, Trace};
pub use program::{parse, Element, Region};
pub use token::Span;
pub use value::{MarkKind, Value};

// The words the TUI dispatches directly from its operator keys, re-exported so
// `crate::engine::ADD` still resolves; the vocabulary otherwise lives in `ops`.
pub(crate) use ops::{ADD, DIV, DUP, MUL, SUB};

/// The stack of values, bottom-to-top: the top of stack is the last element.
/// Internal — the public handle is [`Engine`].
type Stack = Vec<Value>;

/// A primitive operation: a name paired with its dispatch target — a free
/// function over the [`Engine`]. A primitive is *data*, not an enum tag, so
/// adding one is a single table row (see the [`ops`] modules) and the prelude,
/// word resolution, and the TUI all reach it uniformly. `Copy` (a `&'static
/// str` plus a fn pointer), so it rides in a [`Value`] cheaply. Reached only by
/// resolving an [`Element::Word`] (a bare word, or the TUI dispatching one
/// directly), never present in a program. Once functions land, the derived
/// words (`over`, `rot`, `nip`, …) move out of these tables into an in-language
/// prelude, leaving only the true primitives.
#[derive(Clone, Copy)]
pub struct Primitive {
    name: &'static str,
    run: fn(&mut Engine) -> Result<(), ErrorKind>,
}

/// Two primitives are equal when they name the same word — names are unique
/// across the table, and a fn pointer has no equality worth relying on.
impl PartialEq for Primitive {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl std::fmt::Debug for Primitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Primitive").field(&self.name).finish()
    }
}

impl std::fmt::Display for Primitive {
    /// The canonical word — so a captured primitive prints re-readably, and a
    /// directly-dispatched op (a TUI operator) can be labelled in the info bar.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

/// The calculator engine: the RPN stack, plus (later) evaluation settings such
/// as angle mode, display precision, or named registers.
///
/// [`Engine::apply`] consumes `self` and threads it through the batch;
/// individual ops take `&mut self` and mutate in place. The caller keeps its own
/// copy for undo and commits the result only on `Ok` — see the `history` module.
#[derive(Debug, Clone)]
pub struct Engine {
    stack: Stack,
    /// User bindings — the mutable top frame, grown by `set` and cloned per
    /// snapshot. A lookup falls through to `base`. A single flat frame for now
    /// (the REPL/module scope); the frame *chain* arrives with functions.
    top: Frame,
    /// The prelude: the builtin vocabulary as first-class [`Value::Builtin`]s.
    /// Shared and immutable, so a clone is one refcount bump rather than a copy
    /// of the whole map — and it never enters equality (see [`PartialEq`]).
    base: Rc<Frame>,
}

/// A single environment frame: names bound to values.
type Frame = std::collections::HashMap<Rc<str>, Value>;

/// Build the prelude frame — every primitive the [`ops`] modules define, as a
/// first-class [`Value::Builtin`] under its canonical word. Each engine holds
/// this behind an `Rc`, so snapshots share one immutable copy.
fn prelude() -> Rc<Frame> {
    Rc::new(
        ops::primitives()
            .map(|&p| (Rc::from(p.name), Value::Builtin(p)))
            .collect(),
    )
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            stack: Stack::new(),
            top: Frame::new(),
            base: prelude(),
        }
    }
}

/// Two engines are equal when their stacks and user bindings match. The prelude
/// (`base`) is invariant — every engine shares the same immutable vocabulary —
/// so excluding it keeps the per-keystroke change check (in `update`) off the
/// prelude map entirely.
impl PartialEq for Engine {
    fn eq(&self, other: &Self) -> bool {
        self.stack == other.stack && self.top == other.top
    }
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// The current stack, bottom-to-top (top of stack is the last element).
    pub fn stack(&self) -> &[Value] {
        &self.stack
    }

    /// Apply a program (a slice of elements) in order, threading one engine
    /// through the whole batch (this is the consuming boundary). The first failure
    /// short-circuits and is wrapped with a [`Trace`] of the batch plus the
    /// index that failed — "here's what was running." A partially-applied engine
    /// is simply dropped; the caller kept its own copy (see the module docs).
    pub fn apply(mut self, program: &[Element]) -> Outcome {
        for (index, element) in program.iter().enumerate() {
            if let Err(kind) = self.apply_one(element) {
                return Err(CalcError {
                    kind,
                    trace: Some(Trace {
                        program: program.to_vec(),
                        index,
                    }),
                });
            }
        }
        Ok(self)
    }

    /// Apply one program element: push a literal, resolve a word, fetch a
    /// binding unapplied, or run a region's opener/closer. The parser accepts
    /// the whole v2 surface, so the elements evaluation doesn't reach yet — a
    /// template (V3), a dict region or an attribute (V5) — report
    /// [`ErrorKind::Unimplemented`] rather than being absent from the tree.
    fn apply_one(&mut self, element: &Element) -> Result<(), ErrorKind> {
        match element {
            Element::Literal(value) => {
                self.stack.push(value.clone());
                Ok(())
            }
            Element::Word(name) => self.resolve_word(name),
            // `&f` is application's reflective inverse: the binding is pushed,
            // not run. Unlike `'f`, it requires `f` to be bound (§4).
            Element::Fetch(name) => match self.lookup(name) {
                Some(value) => {
                    self.stack.push(value);
                    Ok(())
                }
                None => Err(ErrorKind::UnboundName(name.to_string())),
            },
            // `[` and `]` are fixed elements, not words — the lookup every other
            // token gets, these skip, so they can't be rebound or shadowed. Only
            // the *dispatch* moved here; the mark discipline is unchanged (§6).
            Element::Open(Region::List) => {
                self.stack.push(Value::Mark(MarkKind::List));
                Ok(())
            }
            Element::Close(Region::List) => self.close_list(),
            Element::Template(_) => Err(ErrorKind::Unimplemented("functions")),
            Element::Open(Region::Dict) | Element::Close(Region::Dict) => {
                Err(ErrorKind::Unimplemented("dicts"))
            }
            Element::Attr(_) | Element::AttrFetch(_) => {
                Err(ErrorKind::Unimplemented("attribute access"))
            }
        }
    }

    /// Run a primitive — invoke its dispatch target. Reached by
    /// [`Engine::resolve_word`] (bare-word application), or called directly by
    /// the TUI for its operator keys. The behavior lives in the [`ops`] modules,
    /// not here.
    pub(crate) fn run_builtin(&mut self, primitive: Primitive) -> Result<(), ErrorKind> {
        (primitive.run)(self)
    }

    // --- The stack-machine API. The word vocabulary (`ops`) is a layer of free
    // functions built on these; the machine methods are the only things that
    // touch the stack `Vec` and the frames directly. Each pops what it needs and
    // bails with `?` on the first bad pop — so a failure may leave the stack
    // half-consumed (the caller's transaction, not the op, is atomic). ---

    /// Pop the top value, or `StackUnderflow` if the stack is empty.
    pub(crate) fn pop(&mut self) -> Result<Value, ErrorKind> {
        self.stack.pop().ok_or(ErrorKind::StackUnderflow)
    }

    /// Push a value onto the top of the stack.
    pub(crate) fn push(&mut self, value: Value) {
        self.stack.push(value);
    }

    /// Pop and widen to a number (an `Int` is accepted). Underflow, or a type
    /// error naming what was found.
    pub(crate) fn pop_num(&mut self) -> Result<f64, ErrorKind> {
        self.pop()?.as_num()
    }

    /// Pop a boolean, or underflow / type error.
    pub(crate) fn pop_bool(&mut self) -> Result<bool, ErrorKind> {
        self.pop()?.as_bool()
    }

    /// Pop a list (the shared `Rc` handle), or underflow / type error. Callers
    /// that mutate use `Rc::make_mut` for copy-on-write.
    pub(crate) fn pop_list(&mut self) -> Result<Rc<Vec<Value>>, ErrorKind> {
        match self.pop()? {
            Value::List(items) => Ok(items),
            other => Err(ErrorKind::TypeError {
                expected: "list",
                found: other.type_name(),
            }),
        }
    }

    /// Pop a name, or underflow / type error. `set`/`get` funnel their name
    /// operand through this.
    pub(crate) fn pop_name(&mut self) -> Result<Rc<str>, ErrorKind> {
        match self.pop()? {
            Value::Name(n) => Ok(n),
            other => Err(ErrorKind::TypeError {
                expected: "name",
                found: other.type_name(),
            }),
        }
    }

    /// The `Vec` index for a 1-based level (level 1 == top of stack), or `None`
    /// if the level is out of range. Callers turn `None` into a `StackUnderflow`
    /// with a `let-else` early return. A mark counts as an ordinary level — the
    /// shuffles move and copy marks like any other value, so a collection is not
    /// a sealed scope.
    fn index_of_level(&self, level: usize) -> Option<usize> {
        let len = self.stack.len();
        (1..=len).contains(&level).then(|| len - level)
    }

    /// Copy the value at `level` to the top (`dup` = 1, `over` = 2, `pickn`).
    pub(crate) fn pick_at(&mut self, level: usize) -> Result<(), ErrorKind> {
        let i = self
            .index_of_level(level)
            .ok_or(ErrorKind::StackUnderflow)?;
        let v = self.stack[i].clone();
        self.stack.push(v);
        Ok(())
    }

    /// Remove the value at `level` (`drop` = 1, `nip` = 2, `dropn`).
    pub(crate) fn drop_at(&mut self, level: usize) -> Result<(), ErrorKind> {
        let i = self
            .index_of_level(level)
            .ok_or(ErrorKind::StackUnderflow)?;
        self.stack.remove(i);
        Ok(())
    }

    /// Exchange the value at `level` with the one just below it. `swap` = 1.
    pub(crate) fn swap_at(&mut self, level: usize) -> Result<(), ErrorKind> {
        let i = self
            .index_of_level(level)
            .ok_or(ErrorKind::StackUnderflow)?;
        let j = self
            .index_of_level(level + 1)
            .ok_or(ErrorKind::StackUnderflow)?;
        self.stack.swap(i, j);
        Ok(())
    }

    /// Move the value at `level` up to the top. `rot` = 3, `rolln`.
    pub(crate) fn roll_at(&mut self, level: usize) -> Result<(), ErrorKind> {
        let i = self
            .index_of_level(level)
            .ok_or(ErrorKind::StackUnderflow)?;
        let v = self.stack.remove(i);
        self.stack.push(v);
        Ok(())
    }

    /// Move the top value down to `level` — the inverse of `roll_at`.
    /// `unrot` = 3, `rolldn`.
    pub(crate) fn rolld_at(&mut self, level: usize) -> Result<(), ErrorKind> {
        let dest = self
            .index_of_level(level)
            .ok_or(ErrorKind::StackUnderflow)?;
        // `dest` is where the top must land. Popping first leaves every index
        // ≤ dest unchanged (dest ≤ len - 1), so we can insert straight in.
        let v = self
            .stack
            .pop()
            .expect("level ≥ 1 implies a non-empty stack");
        self.stack.insert(dest, v);
        Ok(())
    }

    /// `]`: collect the values above the topmost mark into a `List`, consuming
    /// the mark. Fails with `UnmatchedClose` when no collection is open. The
    /// collected values are, by the region discipline, all non-marks — so the
    /// list never contains a mark. (When `{` arrives, this will also reject a
    /// mark of the wrong kind.)
    pub(crate) fn close_list(&mut self) -> Result<(), ErrorKind> {
        let mark = self
            .stack
            .iter()
            .rposition(|v| matches!(v, Value::Mark(_)))
            .ok_or(ErrorKind::UnmatchedClose)?;
        let items: Vec<Value> = self.stack.drain(mark + 1..).collect();
        self.stack.pop(); // the mark, now on top
        self.stack.push(Value::List(Rc::new(items)));
        Ok(())
    }

    /// `clear`: empty the stack.
    pub(crate) fn clear(&mut self) -> Result<(), ErrorKind> {
        self.stack.clear();
        Ok(())
    }

    /// Resolve a bare word: a user binding (which shadows) pushes its value;
    /// otherwise a builtin from the prelude is applied; otherwise `UnboundName`.
    /// For a plain value this "application" is just a push — a value is a
    /// nullary function (§1); once functions land, a function binding runs.
    fn resolve_word(&mut self, name: &Rc<str>) -> Result<(), ErrorKind> {
        match self.lookup(name) {
            Some(value) => self.apply_value(value),
            None => Err(ErrorKind::UnboundName(name.to_string())),
        }
    }

    /// Look up a name — the user frame shadows the prelude. Returns a clone (an
    /// `Rc` bump for heap values), leaving the binding in place. `get` reaches
    /// the prelude through this too.
    pub(crate) fn lookup(&self, name: &str) -> Option<Value> {
        self.top.get(name).or_else(|| self.base.get(name)).cloned()
    }

    /// Apply a looked-up value: a callable (a builtin, later a function) runs;
    /// anything else is data and is pushed. This is what makes a bare word *do*
    /// its op while a word bound to a number just lands it on the stack.
    fn apply_value(&mut self, value: Value) -> Result<(), ErrorKind> {
        match value {
            Value::Builtin(primitive) => self.run_builtin(primitive),
            data => {
                self.stack.push(data);
                Ok(())
            }
        }
    }

    /// Bind `name` to `value` in the user frame, shadowing any prior binding
    /// (including a prelude builtin). `set` funnels through this; the shared
    /// prelude is never touched.
    pub(crate) fn bind(&mut self, name: Rc<str>, value: Value) {
        self.top.insert(name, value);
    }
}

#[cfg(test)]
mod tests;
