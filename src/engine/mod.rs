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
//! half-consumed when it fails (it pops before it type-checks), and a batch may
//! have bound names before failing, so there is no "operands intact on error"
//! guarantee. Instead the caller **clones the engine** before applying and puts
//! the copy back on failure.
//!
//! That works — and is cheap — because a closure names its frame by [`FrameId`]
//! rather than pointing at it (`memory-model.md` §0). Cloning an [`Env`] is a
//! pointer bump per frame; the frames stay shared until one engine writes to
//! one, and then only that frame is copied. And because an id means something
//! only *within* the environment it was minted in, a whole-engine copy is
//! internally consistent with nothing left to reconcile.

use std::collections::HashMap;
use std::rc::Rc;

mod error;
mod frame;
mod ops;
mod program;
mod token;
mod value;

pub use error::{CalcError, ErrorKind, Outcome, ParseError, ParseErrorKind, Trace};
pub use frame::{Bindings, Env, FrameId};
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
/// Ops take `&mut self` and mutate in place; the caller clones one beforehand
/// and puts it back on failure — see the module docs.
///
/// **`Clone` is the snapshot**, and cheap: the environment is a map of shared
/// frames, so a copy is one pointer bump each and a frame is only duplicated
/// when one of the two engines writes to it. Snapshotting the whole engine
/// rather than a hand-picked subset is deliberate — a state that had to be kept
/// in step with the fields by hand would silently stop covering the next one
/// added (an angle mode, a display precision), and that failure is invisible.
#[derive(Debug, Clone, PartialEq)]
pub struct Engine {
    stack: Stack,
    /// The call stack — see [`Activation`]. **Never empty**: the bottom entry is
    /// the module activation, which every line is loaded into and which never
    /// returns.
    calls: Vec<Activation>,
    /// Every frame that exists, by id.
    env: Env,
    /// The next frame id to hand out. Rewound by a snapshot like everything
    /// else, which is safe because **an id only means anything inside the
    /// environment it was minted in** — a value and the `Env` it names are
    /// always copied and restored together, so a reused id after an undo lands
    /// in a map where the old frame never existed.
    next_frame: FrameId,
}

/// One level of the **dynamic** call stack: a template, how far through it
/// evaluation has got, and the frame its code binds and resolves in.
///
/// The two chains stay distinct. An activation is walked by *returning*; a frame
/// is walked by *name lookup*, following `parent` — which a call sets to the
/// function's captured environment, never to its caller (§8).
///
/// The bottom activation is the **module**, and it is special in exactly one
/// way: it never returns. A REPL line is loaded into it rather than pushed above
/// it, which is §9's "a module is not a call frame" made structural — a line is
/// a call that reuses the module's frame instead of allocating one. Between
/// lines it holds an empty template, so two engines at rest compare equal.
#[derive(Debug, Clone, PartialEq)]
struct Activation {
    template: Rc<[Element]>,
    ip: usize,
    frame: FrameId,
}

/// The prelude's bindings — every primitive the [`ops`] modules define as a
/// first-class [`Value::Builtin`] under its canonical word, plus the constants
/// (`true`, `false`), which are plain values. These fill the **global frame**,
/// the root of every chain (§8).
fn prelude() -> HashMap<Rc<str>, Value> {
    ops::primitives()
        .map(|&p| (Rc::from(p.name), Value::Builtin(p)))
        .chain(ops::constants().map(|(word, value)| (Rc::from(word), value)))
        .collect()
}

/// An activation with nothing to run — what the module holds between lines.
fn no_code() -> Rc<[Element]> {
    Rc::from(Vec::new())
}

/// The global frame's id. Every chain ends here, and it is the one frame that
/// will never be a collection candidate — reachable as a root from everywhere.
const GLOBAL: FrameId = 0;
/// The module frame's id — the REPL's scope, installed under the global frame.
const MODULE: FrameId = 1;

impl Default for Engine {
    fn default() -> Self {
        let mut env = Env::default();
        env.insert(GLOBAL, None, prelude());
        env.insert(MODULE, Some(GLOBAL), Bindings::new());
        Self {
            stack: Stack::new(),
            calls: vec![Activation {
                template: no_code(),
                ip: 0,
                frame: MODULE,
            }],
            env,
            next_frame: MODULE + 1,
        }
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

    /// Apply a program, mutating the engine in place. The first failure
    /// short-circuits and is wrapped with a [`Trace`] of the batch plus the
    /// index that failed — "here's what was running."
    ///
    /// A failed batch leaves the engine part-way through: the caller took a
    /// [`State`] first and puts it back (see the module docs).
    pub fn apply(&mut self, program: &[Element]) -> Outcome {
        match self.run(Rc::from(program)) {
            Ok(()) => Ok(()),
            Err((kind, index)) => Err(CalcError {
                kind,
                trace: Some(Trace {
                    program: program.to_vec(),
                    index,
                }),
            }),
        }
    }

    /// Load a line into the module activation and run until it is exhausted.
    /// Either way the module is left holding no code, which is the engine's
    /// resting shape.
    fn run(&mut self, template: Rc<[Element]>) -> Result<(), (ErrorKind, usize)> {
        let module = self.module_mut();
        module.template = template;
        module.ip = 0;
        let outcome = self.execute();
        self.clear_calls();
        outcome
    }

    /// The evaluation loop: advance the top activation one element at a time,
    /// popping it when its template is exhausted, and stopping when the module
    /// activation runs out — the module never returns, so "done" is a depth of
    /// one rather than an empty stack.
    ///
    /// **An explicit machine, not a recursive walk.** Nothing needs it yet —
    /// with no functions there is only ever the module activation — but
    /// iteration in this language is recursion over combinators, so calls must
    /// run *flat* or depth is bounded by the Rust stack. Making the loop
    /// explicit is what lets a tail call replace the top activation instead of
    /// nesting under it (`direction-v2.md`, "the evaluator is an explicit VM").
    ///
    /// The element is cloned out before dispatch — an `Rc` bump for a template,
    /// a `Value` clone otherwise — because the op it runs needs `&mut self`, and
    /// the activation it came from lives in `self`.
    fn execute(&mut self) -> Result<(), (ErrorKind, usize)> {
        loop {
            let top = self.top();
            if top.ip >= top.template.len() {
                if self.calls.len() == 1 {
                    return Ok(()); // the module is out of code, not returning
                }
                self.calls.pop();
                continue;
            }
            let top = self.top_mut();
            let element = top.template[top.ip].clone();
            let index = top.ip;
            top.ip += 1;
            if let Err(kind) = self.apply_one(&element) {
                return Err((kind, index));
            }
        }
    }

    /// The running activation. Never absent — the module activation is the floor.
    fn top(&self) -> &Activation {
        self.calls.last().expect("the module activation")
    }

    fn top_mut(&mut self) -> &mut Activation {
        self.calls.last_mut().expect("the module activation")
    }

    fn module_mut(&mut self) -> &mut Activation {
        self.calls.first_mut().expect("the module activation")
    }

    /// Return to the resting shape: only the module activation, holding no code.
    /// Run after every batch, so a failed line leaves nothing half-executed and
    /// two engines between lines differ only where their *state* differs.
    fn clear_calls(&mut self) {
        self.calls.truncate(1);
        let module = self.module_mut();
        module.template = no_code();
        module.ip = 0;
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
            // A template's `names :` list. The same binding `set` performs, but
            // reached without a lookup — so fixed syntax can't be broken by
            // rebinding a word (§5).
            Element::Bind(name) => {
                let value = self.pop()?;
                self.bind(name.clone(), value);
                Ok(())
            }
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

    /// Look up a name, walking the chain outward from the current frame — a
    /// nearer binding shadows a further one, and the global frame's prelude is
    /// the floor. Returns a clone (an `Rc` bump for heap values), leaving the
    /// binding in place. `get` and `&f` reach builtins through this too.
    pub(crate) fn lookup(&self, name: &str) -> Option<Value> {
        self.env.lookup(self.frame(), name)
    }

    /// The frame lookup starts from and binding lands in: the running
    /// activation's, with no special case for top level — between lines that is
    /// the module activation, and during a call it is the callee's.
    fn frame(&self) -> FrameId {
        self.top().frame
    }

    /// Allocate a frame enclosed by `parent`, and return its id. Every
    /// application makes one (§5), so this is the allocation the collector will
    /// eventually have to answer for — and the one lazy allocation would skip
    /// when a call neither binds nor captures.
    ///
    /// The id comes from a counter that no snapshot rewinds, so an id is unique
    /// for the life of the session even across an undo.
    // Nothing calls this until applying a function does; it exists now so the id
    // discipline it enforces is testable before there is a caller to get it wrong.
    #[allow(dead_code)]
    pub(crate) fn new_frame(&mut self, parent: Option<FrameId>) -> FrameId {
        let id = self.next_frame;
        self.next_frame += 1;
        self.env.insert(id, parent, Bindings::new());
        id
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

    /// Bind `name` to `value` in the *current* frame, shadowing any binding
    /// further out (including a prelude builtin). Binding never walks the chain,
    /// so a shadowed builtin is still there to fall back to.
    pub(crate) fn bind(&mut self, name: Rc<str>, value: Value) {
        self.env.bind(self.frame(), name, value);
    }
}

#[cfg(test)]
mod conformance;
#[cfg(test)]
mod tests;
