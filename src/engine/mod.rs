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
mod program;
mod value;

pub use error::{CalcError, ErrorKind, Outcome, Trace};
pub use program::{parse, Element};
pub use value::{MarkKind, Value};

/// The stack of values, bottom-to-top: the top of stack is the last element.
/// Internal — the public handle is [`Engine`].
type Stack = Vec<Value>;

/// A primitive operation: a name paired with its dispatch target — a host
/// function over the [`Engine`]. The whole vocabulary is one flat table
/// ([`PRIMITIVES`]); a primitive is *data*, not an enum tag, so adding one is a
/// single row and the prelude, word resolution, and the TUI all reach it
/// uniformly. `Copy` (a `&'static str` plus a fn pointer), so it rides in a
/// [`Value`] cheaply. Reached only by resolving an [`Element::Word`] (a bare
/// word, or the TUI dispatching one directly), never present in a program. Once
/// functions land, the derived words (`over`, `rot`, `nip`, …) move out of this
/// table into an in-language prelude, leaving only the true primitives.
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

// Arithmetic operators are named so the TUI can dispatch them directly from its
// operator keys, unshadowably — the same constants the table installs by word.
pub(crate) const ADD: Primitive = Primitive {
    name: "+",
    run: Engine::add,
};
pub(crate) const SUB: Primitive = Primitive {
    name: "-",
    run: |e| e.arith(i64::checked_sub, |a, b| a - b),
};
pub(crate) const MUL: Primitive = Primitive {
    name: "*",
    run: |e| e.arith(i64::checked_mul, |a, b| a * b),
};
pub(crate) const DIV: Primitive = Primitive {
    name: "/",
    run: Engine::div,
};
/// `dup`, dispatched directly for the empty-Enter shortcut.
pub(crate) const DUP: Primitive = Primitive {
    name: "dup",
    run: |e| e.pick_at(1),
};

/// The builtin vocabulary: every primitive under its canonical word. The single
/// source of both names and behavior — [`prelude`] maps it straight into the
/// base frame, and it is the only place a primitive is declared. Each `run` is
/// the word's dispatch target: an [`Engine`] method, or a small closure over
/// one. Not exhaustiveness-checked (there's no enum), but there's nothing to
/// forget — a row *is* a primitive, name and behavior together.
///
/// `rustfmt::skip` keeps this as a hand-aligned table; without it each row
/// explodes to four lines.
#[rustfmt::skip]
static PRIMITIVES: &[Primitive] = &[
    ADD,
    SUB,
    MUL,
    DIV,
    Primitive { name: "neg", run: Engine::negate },
    // Equality pops two values -> Bool; comparisons pop two numbers -> Bool.
    Primitive { name: "=", run: Engine::equality },
    Primitive { name: "<", run: |e| e.num_compare(|a, b| a < b) },
    Primitive { name: ">", run: |e| e.num_compare(|a, b| a > b) },
    Primitive { name: "<=", run: |e| e.num_compare(|a, b| a <= b) },
    Primitive { name: ">=", run: |e| e.num_compare(|a, b| a >= b) },
    // Boolean ops — Bools only (no truthiness rule).
    Primitive { name: "not", run: |e| e.bool_unary(|a| !a) },
    Primitive { name: "and", run: |e| e.bool_binary(|a, b| a && b) },
    Primitive { name: "or", run: |e| e.bool_binary(|a, b| a || b) },
    Primitive { name: "length", run: Engine::length },
    Primitive { name: "to_str", run: Engine::stringify },
    // Fixed shuffles — several are just a fixed level of an indexed op.
    DUP, // a -- a a
    Primitive { name: "drop", run: |e| e.drop_at(1) }, // a --
    Primitive { name: "swap", run: |e| e.swap_at(1) }, // a b -- b a
    Primitive { name: "over", run: |e| e.pick_at(2) }, // a b -- a b a
    Primitive { name: "rot", run: |e| e.roll_at(3) },  // a b c -- b c a
    Primitive { name: "unrot", run: |e| e.rolld_at(3) }, // a b c -- c a b
    Primitive { name: "nip", run: |e| e.drop_at(2) },  // a b -- b
    Primitive { name: "tuck", run: Engine::tuck },     // a b -- b a b
    Primitive { name: "dupd", run: Engine::dupd },     // a b -- a a b
    Primitive { name: "2dup", run: Engine::two_dup },  // a b -- a b a b
    Primitive { name: "2drop", run: Engine::two_drop }, // a b --
    // Indexed ops: the 1-based level is popped off the stack (`n rolln`).
    Primitive { name: "pickn", run: |e| e.indexed(Engine::pick_at) },
    Primitive { name: "rolln", run: |e| e.indexed(Engine::roll_at) },
    Primitive { name: "rolldn", run: |e| e.indexed(Engine::rolld_at) },
    Primitive { name: "dropn", run: |e| e.indexed(Engine::drop_at) },
    Primitive { name: "swapn", run: |e| e.indexed(Engine::swap_at) },
    // Lists.
    Primitive { name: "[", run: Engine::open_list },   // push a list mark
    Primitive { name: "]", run: Engine::close_list },  // collect to the mark
    Primitive { name: "first", run: Engine::first },   // [a b c] -- a
    Primitive { name: "rest", run: Engine::rest },     // [a b c] -- [b c]
    Primitive { name: "cons", run: Engine::cons },     // x [b c] -- [x b c]
    Primitive { name: "append", run: Engine::append }, // [a b] [c d] -- [a b c d]
    Primitive { name: "nth", run: Engine::nth },       // [a b c] n -- (0-based)
    // Environment.
    Primitive { name: "set", run: Engine::set },       // value name --
    Primitive { name: "get", run: Engine::get },       // name -- value
    Primitive { name: "clear", run: Engine::clear },
];

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

/// Build the prelude frame — every [`PRIMITIVES`] entry as a first-class
/// [`Value::Builtin`] under its canonical word. Each engine holds this behind an
/// `Rc`, so snapshots share one immutable copy.
fn prelude() -> Rc<Frame> {
    Rc::new(
        PRIMITIVES
            .iter()
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

    /// Apply one program element: push a literal, or resolve a word.
    fn apply_one(&mut self, element: &Element) -> Result<(), ErrorKind> {
        match element {
            Element::Literal(value) => {
                self.stack.push(value.clone());
                Ok(())
            }
            Element::Word(name) => self.resolve_word(name),
        }
    }

    /// Run a primitive — invoke its dispatch target. Reached by
    /// [`Engine::resolve_word`] (bare-word application), or called directly by
    /// the TUI for its operator keys. The behavior lives in the [`PRIMITIVES`]
    /// table, not here.
    pub(crate) fn run_builtin(&mut self, primitive: Primitive) -> Result<(), ErrorKind> {
        (primitive.run)(self)
    }

    // Stack transforms. Each takes `&mut self`, pops what it needs, mutates in
    // place, and bails with `?` on the first bad pop — so a failure may leave the
    // stack half-consumed (the caller's transaction, not the op, is atomic).

    /// Pop the top value, or `StackUnderflow` if the stack is empty.
    fn pop(&mut self) -> Result<Value, ErrorKind> {
        self.stack.pop().ok_or(ErrorKind::StackUnderflow)
    }

    /// Pop and widen to a number (an `Int` is accepted). Underflow, or a type
    /// error naming what was found.
    fn pop_num(&mut self) -> Result<f64, ErrorKind> {
        self.pop()?.as_num()
    }

    /// Pop a boolean, or underflow / type error.
    fn pop_bool(&mut self) -> Result<bool, ErrorKind> {
        self.pop()?.as_bool()
    }

    /// Pop a list (the shared `Rc` handle), or underflow / type error. Callers
    /// that mutate use `Rc::make_mut` for copy-on-write.
    fn pop_list(&mut self) -> Result<Rc<Vec<Value>>, ErrorKind> {
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
    fn pop_name(&mut self) -> Result<Rc<str>, ErrorKind> {
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

    /// Two-operand op whose result is always a float. `a` is the deeper operand,
    /// `b` the top, so `a b <op>` reads left-to-right as `a <op> b`. Both widen
    /// via [`Value::as_num`] (so `Int`s are accepted); the op may still reject
    /// them (divide-by-zero). Used by `/`; integer-preserving ops use `arith`.
    fn num_binary(
        &mut self,
        op: impl FnOnce(f64, f64) -> Result<f64, ErrorKind>,
    ) -> Result<(), ErrorKind> {
        let b = self.pop_num()?;
        let a = self.pop_num()?;
        self.stack.push(Value::Num(op(a, b)?));
        Ok(())
    }

    /// Combine two owned values with integer-preserving arithmetic: two `Int`s
    /// stay an `Int` via `checked` (promoting to `f64` on overflow), else both
    /// widen to `f64`. Shared by `+ - *`; a bool operand is a `TypeError`.
    fn arith_values(
        a: Value,
        b: Value,
        checked: impl FnOnce(i64, i64) -> Option<i64>,
        float: impl FnOnce(f64, f64) -> f64,
    ) -> Result<Value, ErrorKind> {
        match (&a, &b) {
            (Value::Int(x), Value::Int(y)) => Ok(checked(*x, *y)
                .map(Value::Int)
                .unwrap_or_else(|| Value::Num(float(*x as f64, *y as f64)))),
            _ => Ok(Value::Num(float(a.as_num()?, b.as_num()?))),
        }
    }

    /// Integer-preserving binary arithmetic (`- *`, and the numeric branch of
    /// `+`).
    fn arith(
        &mut self,
        checked: impl FnOnce(i64, i64) -> Option<i64>,
        float: impl FnOnce(f64, f64) -> f64,
    ) -> Result<(), ErrorKind> {
        let b = self.pop()?;
        let a = self.pop()?;
        let v = Self::arith_values(a, b, checked, float)?;
        self.stack.push(v);
        Ok(())
    }

    /// Negate the top of stack, preserving `Int` (falling back to a float only
    /// on the `i64::MIN` overflow).
    fn negate(&mut self) -> Result<(), ErrorKind> {
        let v = match self.pop()? {
            Value::Int(i) => i
                .checked_neg()
                .map(Value::Int)
                .unwrap_or_else(|| Value::Num(-(i as f64))),
            Value::Num(x) => Value::Num(-x),
            other => {
                return Err(ErrorKind::TypeError {
                    expected: "number",
                    found: other.type_name(),
                })
            }
        };
        self.stack.push(v);
        Ok(())
    }

    /// Two-operand numeric comparison, pushing a `Bool` (`< > <= >=`).
    fn num_compare(&mut self, op: impl FnOnce(f64, f64) -> bool) -> Result<(), ErrorKind> {
        let b = self.pop_num()?;
        let a = self.pop_num()?;
        self.stack.push(Value::Bool(op(a, b)));
        Ok(())
    }

    /// Equality of the top two values, pushing a `Bool`. Takes any two values —
    /// numbers compare by value across the int/float split, so `2 2.0 =` is
    /// true; anything else falls back to structural equality.
    fn equality(&mut self) -> Result<(), ErrorKind> {
        let b = self.pop()?;
        let a = self.pop()?;
        let eq = match (a.as_num(), b.as_num()) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        };
        self.stack.push(Value::Bool(eq));
        Ok(())
    }

    /// Two-operand boolean op (`and`/`or`). Both operands must be `Bool` — no
    /// truthiness rule, so a number is a `TypeError`.
    fn bool_binary(&mut self, op: impl FnOnce(bool, bool) -> bool) -> Result<(), ErrorKind> {
        let b = self.pop_bool()?;
        let a = self.pop_bool()?;
        self.stack.push(Value::Bool(op(a, b)));
        Ok(())
    }

    /// One-operand boolean op (`not`).
    fn bool_unary(&mut self, op: impl FnOnce(bool) -> bool) -> Result<(), ErrorKind> {
        let a = self.pop_bool()?;
        self.stack.push(Value::Bool(op(a)));
        Ok(())
    }

    /// `+`: concatenate two strings, or add two numbers. A string and a number
    /// is a type error from the numeric path (no implicit `to_str`).
    fn add(&mut self) -> Result<(), ErrorKind> {
        let b = self.pop()?;
        let a = self.pop()?;
        let v = match (a, b) {
            (Value::Str(mut a), Value::Str(b)) => {
                Rc::make_mut(&mut a).push_str(&b);
                Value::Str(a)
            }
            (a, b) => Self::arith_values(a, b, i64::checked_add, |a, b| a + b)?,
        };
        self.stack.push(v);
        Ok(())
    }

    /// `/`: division, always yielding a float — `1 2 /` is `0.5`, not `0`.
    fn div(&mut self) -> Result<(), ErrorKind> {
        self.num_binary(|a, b| {
            if b == 0.0 {
                Err(ErrorKind::DivideByZero)
            } else {
                Ok(a / b)
            }
        })
    }

    /// `[`: push a list mark, opening a collection (§13 mark discipline).
    fn open_list(&mut self) -> Result<(), ErrorKind> {
        self.stack.push(Value::Mark(MarkKind::List));
        Ok(())
    }

    /// `clear`: empty the stack.
    fn clear(&mut self) -> Result<(), ErrorKind> {
        self.stack.clear();
        Ok(())
    }

    /// `length`: the element count of the top string (characters) or list.
    fn length(&mut self) -> Result<(), ErrorKind> {
        let len = match self.pop()? {
            Value::Str(s) => s.chars().count() as i64,
            Value::List(items) => items.len() as i64,
            other => {
                return Err(ErrorKind::TypeError {
                    expected: "string or list",
                    found: other.type_name(),
                })
            }
        };
        self.stack.push(Value::Int(len));
        Ok(())
    }

    /// `to_str`: replace the top value with its string content (no quotes).
    /// Total — every value has a string form. (Named `stringify`, not `to_str`,
    /// to avoid the `to_*`-should-borrow lint.)
    fn stringify(&mut self) -> Result<(), ErrorKind> {
        let s = self.pop()?.content_string();
        self.stack.push(Value::Str(Rc::new(s)));
        Ok(())
    }

    /// Run an indexed op with its level popped off the stack (`n rolln`).
    fn indexed(
        &mut self,
        op: impl FnOnce(&mut Engine, usize) -> Result<(), ErrorKind>,
    ) -> Result<(), ErrorKind> {
        let level = self.pop()?.as_index()?;
        op(self, level)
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
    fn rolld_at(&mut self, level: usize) -> Result<(), ErrorKind> {
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

    /// `tuck` ( a b -- b a b ): insert a copy of the top below the second.
    fn tuck(&mut self) -> Result<(), ErrorKind> {
        let n = self.stack.len();
        if n < 2 {
            return Err(ErrorKind::StackUnderflow);
        }
        let top = self.stack[n - 1].clone();
        self.stack.insert(n - 2, top);
        Ok(())
    }

    /// `dupd` ( a b -- a a b ): duplicate the second element in place.
    fn dupd(&mut self) -> Result<(), ErrorKind> {
        let n = self.stack.len();
        if n < 2 {
            return Err(ErrorKind::StackUnderflow);
        }
        let second = self.stack[n - 2].clone();
        self.stack.insert(n - 1, second);
        Ok(())
    }

    /// `2dup` ( a b -- a b a b ): copy the top two, order preserved.
    fn two_dup(&mut self) -> Result<(), ErrorKind> {
        let n = self.stack.len();
        if n < 2 {
            return Err(ErrorKind::StackUnderflow);
        }
        let a = self.stack[n - 2].clone();
        let b = self.stack[n - 1].clone();
        self.stack.push(a);
        self.stack.push(b);
        Ok(())
    }

    /// `2drop` ( a b -- ): drop the top two.
    fn two_drop(&mut self) -> Result<(), ErrorKind> {
        let n = self.stack.len();
        if n < 2 {
            return Err(ErrorKind::StackUnderflow);
        }
        self.stack.truncate(n - 2);
        Ok(())
    }

    /// `]`: collect the values above the topmost mark into a `List`, consuming
    /// the mark. Fails with `UnmatchedClose` when no collection is open. The
    /// collected values are, by the region discipline, all non-marks — so the
    /// list never contains a mark. (When `{` arrives, this will also reject a
    /// mark of the wrong kind.)
    fn close_list(&mut self) -> Result<(), ErrorKind> {
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

    /// `first` ( [a b c] -- a ): the head of the top list; empty is out of range.
    fn first(&mut self) -> Result<(), ErrorKind> {
        let head = self
            .pop_list()?
            .first()
            .cloned()
            .ok_or(ErrorKind::IndexOutOfRange)?;
        self.stack.push(head);
        Ok(())
    }

    /// `rest` ( [a b c] -- [b c] ): the top list without its head; empty is out
    /// of range.
    fn rest(&mut self) -> Result<(), ErrorKind> {
        let mut items = self.pop_list()?;
        if items.is_empty() {
            return Err(ErrorKind::IndexOutOfRange);
        }
        Rc::make_mut(&mut items).remove(0);
        self.stack.push(Value::List(items));
        Ok(())
    }

    /// `cons` ( x [b c] -- [x b c] ): prepend the element below to the top list.
    fn cons(&mut self) -> Result<(), ErrorKind> {
        let mut items = self.pop_list()?;
        let x = self.pop()?;
        Rc::make_mut(&mut items).insert(0, x);
        self.stack.push(Value::List(items));
        Ok(())
    }

    /// `append` ( [a b] [c d] -- [a b c d] ): concatenate two lists.
    fn append(&mut self) -> Result<(), ErrorKind> {
        let b = self.pop_list()?;
        let mut a = self.pop_list()?;
        Rc::make_mut(&mut a).extend(b.iter().cloned());
        self.stack.push(Value::List(a));
        Ok(())
    }

    /// `nth` ( [a b c] i -- x ): the 0-based `i`th element. List indexing is
    /// 0-based (other-language convention), unlike the 1-based `pickn`/`rolln`.
    fn nth(&mut self) -> Result<(), ErrorKind> {
        let idx = match self.pop()? {
            Value::Int(i) if i >= 0 => i as usize,
            Value::Int(_) => return Err(ErrorKind::IndexOutOfRange),
            Value::Num(_) => {
                return Err(ErrorKind::TypeError {
                    expected: "integer",
                    found: "float",
                })
            }
            other => {
                return Err(ErrorKind::TypeError {
                    expected: "integer",
                    found: other.type_name(),
                })
            }
        };
        let item = self
            .pop_list()?
            .get(idx)
            .cloned()
            .ok_or(ErrorKind::IndexOutOfRange)?;
        self.stack.push(item);
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
    /// `Rc` bump for heap values), leaving the binding in place.
    fn lookup(&self, name: &str) -> Option<Value> {
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

    /// `set` ( value name -- ): bind `name` to `value` in the user frame,
    /// shadowing any prior binding (including a prelude builtin). The name is on
    /// top (`3 'x set`). Never touches the shared prelude.
    fn set(&mut self) -> Result<(), ErrorKind> {
        let name = self.pop_name()?;
        let value = self.pop()?;
        self.top.insert(name, value);
        Ok(())
    }

    /// `get` ( name -- value ): push the value bound to `name` — a user binding
    /// or a prelude builtin (so `'+ get` captures the op) — or fail with
    /// `UnboundName`. The value is *pushed*, not run: this is the capture that
    /// mirrors bare-word application. A later mutation copies-on-write.
    fn get(&mut self) -> Result<(), ErrorKind> {
        let name = self.pop_name()?;
        let value = self
            .lookup(&name)
            .ok_or_else(|| ErrorKind::UnboundName(name.to_string()))?;
        self.stack.push(value);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
