//! [`Value`] — the stack's element type — and its conversions, `Display`, and
//! type-extraction helpers. A small immutable sum type: scalars by value, heap
//! variants (`Str`/`List`) shared via `Rc` (copy-on-write on mutation), plus a
//! captured [`Primitive`] for first-class words.

use std::rc::Rc;

use super::{Element, ErrorKind, FrameId, Primitive, Template};

/// The kind of an open collection, carried by its [`Value::Mark`]. Only lists
/// for now; `{` will add a function mark (carrying the captured environment) in
/// the next milestone. Typed so a `]` closing a `{` can be caught as a mismatch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MarkKind {
    List,
}

/// A value on the stack. Started as a bare `f64`; now a small sum type so the
/// stack can hold more than numbers — including [`Value::Function`], which is
/// what makes a word and a value the same kind of thing. No longer `Copy`:
/// `Str`/`List` own heap data. Still to come is the numeric tower (bignum,
/// exact reals, complex, matrix); see `todo.md`.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// An integer. Preserved through `+ - *` and `neg` when both operands are
    /// integers; any float operand (or overflow) promotes to [`Value::Num`].
    Int(i64),
    /// A float. `/` always yields one, and mixed int/float arithmetic promotes
    /// to it. A real numeric tower (rationals, complex) comes later.
    Num(f64),
    /// A boolean — a genuine type, not Forth's 0/-1. Produced by comparisons
    /// and the boolean words, and consumed by `if`.
    Bool(bool),
    /// A string. Heap-shared via `Rc`, so a clone (a `dup`, a lookup) is a
    /// refcount bump; concatenation copies-on-write via `Rc::make_mut`. Built by
    /// the tokenizer's `"…"` literals and by `to_str`; concatenated with `+`.
    Str(Rc<String>),
    /// A list — a growable, heterogeneous sequence, `Rc`-shared like `Str`.
    /// Built by the `[ … ]` words via the mark discipline, never an
    /// `Element::Literal`; the list ops copy-on-write.
    List(Rc<Vec<Value>>),
    /// A name — an environment key. Pushed by the `'x` sigil, consumed by
    /// `set`/`get`. Compares and hashes by its text (not yet interned).
    Name(Rc<str>),
    /// A collection mark: a typed stack sentinel, *not* a first-class value.
    /// `[` pushes one and `]` collects the values above it into a [`Value::List`].
    /// The value words reject it with a type error (so `[ 1 +` is a type error),
    /// but the shuffles move and copy it like any other stack item — a collection
    /// is a manipulable region, not a sealed scope (see `language.md` §13).
    Mark(MarkKind),
    /// A primitive op, as the environment holds it. **Never on the stack**:
    /// `&name` yields `{name}` rather than extracting the word, so this is the
    /// prelude's representation of a builtin and
    /// [`Engine::apply_value`](super::Engine::apply_value) its only consumer —
    /// which is what lets a word move between the Rust and in-language halves of
    /// the prelude without a caller noticing.
    ///
    /// Held **by reference**: the tables are `'static`, so this is a pointer
    /// rather than the `&str`-plus-fn-pointer pair, which would otherwise make
    /// every `Value` in the system 24 bytes wide to hold a builtin almost none
    /// of them contain.
    Builtin(&'static Primitive),
    /// A function: a parse-time template paired with the environment it captured
    /// (§5). Produced by evaluating a `{ … }`, which is why instantiation is
    /// cheap — a pointer and an id — and why `{ {*} }` doesn't re-parse its
    /// inner template per call.
    ///
    /// **`env` is an id, not a pointer**, and that is what lets a function be an
    /// ordinary value: it can be copied, snapshotted, and stored inside the very
    /// frame it captured without forming a cycle (`memory-model.md` §0). It also
    /// keeps binding *late* — the id resolves against the environment as it is
    /// when the function runs, not as it was when the function was made, which
    /// is what makes recursion work with no forward declaration.
    Function { template: Template, env: FrameId },
}

impl Value {
    /// The type's name, for error messages ("expected number, found bool").
    /// `Int` and `Num` are both "number" — the split is invisible to the type
    /// errors, since the arithmetic words accept either.
    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            Value::Int(_) | Value::Num(_) => "number",
            Value::Bool(_) => "bool",
            Value::Str(_) => "string",
            Value::List(_) => "list",
            Value::Name(_) => "name",
            Value::Builtin(_) => "builtin",
            Value::Function { .. } => "function",
            // The open-collection sentinel isn't a first-class value: the value
            // words reject it, naming it as an "open list" in the error.
            Value::Mark(MarkKind::List) => "open list",
        }
    }

    /// Widen to `f64`, or a [`ErrorKind::TypeError`] naming what was found.
    /// Comparisons, division, and mixed arithmetic funnel operands through this,
    /// so an `Int` is accepted wherever a number is wanted. A `Mark` is not a
    /// value — it falls through to the type error, so `[ 1 +` is a type error.
    pub(crate) fn as_num(&self) -> Result<f64, ErrorKind> {
        match self {
            Value::Int(i) => Ok(*i as f64),
            Value::Num(n) => Ok(*n),
            other => Err(ErrorKind::TypeError {
                expected: "number",
                found: other.type_name(),
            }),
        }
    }

    /// The integer value, or a [`ErrorKind::TypeError`]. A float is rejected
    /// outright (no rounding), so `3.5 rot-to` and `3.5 nth` error rather than
    /// guessing.
    ///
    /// Only the *type* check is shared — the range policy is the caller's,
    /// because the two indexing conventions disagree about a negative:
    /// [`Value::as_index`] clamps it for a 1-based stack level, while `nth`
    /// reports it as out of range.
    pub(crate) fn as_int(&self) -> Result<i64, ErrorKind> {
        match self {
            Value::Int(i) => Ok(*i),
            Value::Num(_) => Err(ErrorKind::TypeError {
                expected: "integer",
                found: "float",
            }),
            other => Err(ErrorKind::TypeError {
                expected: "integer",
                found: other.type_name(),
            }),
        }
    }

    /// Interpret as a 1-based stack level: a positive `Int`. A non-positive one
    /// clamps to 0, which the range check then reports as underflow. The indexed
    /// stack words funnel their level operand through this.
    pub(crate) fn as_index(&self) -> Result<usize, ErrorKind> {
        Ok(self.as_int()?.max(0) as usize)
    }

    /// The plain content string, no quotes — what `to_str` produces. For a
    /// `Str` that's the content itself; for anything else it's the `Display`
    /// form, so `3 to_str` is `"3"` and `true to_str` is `"true"`.
    pub(crate) fn content_string(&self) -> String {
        match self {
            Value::Str(s) => s.as_ref().clone(),
            // The bare name text, not the `'x` display form.
            Value::Name(n) => n.to_string(),
            other => other.to_string(),
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(i) => write!(f, "{i}"),
            Value::Num(n) => write!(f, "{n}"),
            Value::Bool(b) => write!(f, "{b}"),
            // Quoted (and escaped) so a string is visibly distinct from a
            // number on the stack, and so a literal renders re-parseably in a
            // trace. `to_str` uses `content_string` for the unquoted form.
            Value::Str(s) => write!(f, "{s:?}"),
            // Space-padded so the brackets are their own tokens (`[ 1 2 ]`),
            // matching how a list is typed. Empty renders `[ ]`.
            Value::List(items) => {
                write!(f, "[")?;
                for item in items.iter() {
                    write!(f, " {item}")?;
                }
                write!(f, " ]")
            }
            // Names print *with* the quote (`'x`) — otherwise a name and a
            // look-alike number/string are indistinguishable on the stack, and
            // this form is also re-parseable. (A deliberate departure from §3.)
            Value::Name(n) => write!(f, "'{n}"),
            // A captured op shows as its word — a display choice to revisit
            // when functions get their own rendering.
            Value::Builtin(b) => write!(f, "{b}"),
            // A function shows as its source: the same rendering a `Template`
            // element gets, reused rather than restated so the two can't drift
            // (it reconstructs a parameter list, among other things). The
            // captured environment is deliberately absent — §11's "closures
            // aren't plain data", visible in the display.
            Value::Function { template, .. } => {
                write!(f, "{}", Element::Template(Rc::clone(template)))
            }
            // A lone, still-open mark — shown so an unclosed `[` is visible on
            // the stack. Distinct from the empty list's `[ ]`.
            Value::Mark(MarkKind::List) => write!(f, "["),
        }
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Int(i)
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Value::Num(n)
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Str(Rc::new(s))
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(Rc::new(s.to_string()))
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

/// Ergonomic equality against a bare number, so callers and tests can write
/// `stack == &[1.0, 2.0]` without wrapping every literal. Matches by numeric
/// value, so an `Int(2)` equals `2.0`; a `Bool` never does.
impl PartialEq<f64> for Value {
    fn eq(&self, other: &f64) -> bool {
        match self {
            Value::Int(i) => (*i as f64) == *other,
            Value::Num(n) => n == other,
            _ => false,
        }
    }
}

/// Likewise against a bare bool: `stack == &[true]`.
impl PartialEq<bool> for Value {
    fn eq(&self, other: &bool) -> bool {
        matches!(self, Value::Bool(b) if b == other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_display_without_type_noise() {
        assert_eq!(Value::Int(3).to_string(), "3");
        assert_eq!(Value::Num(3.5).to_string(), "3.5");
        assert_eq!(Value::Bool(true).to_string(), "true");
        assert_eq!(Value::Bool(false).to_string(), "false");
    }

    #[test]
    fn strings_display_quoted_on_the_stack() {
        // Display quotes and escapes, so a string is visibly not a number;
        // `to_str` / `content_string` give the bare content.
        assert_eq!(Value::from("hi").to_string(), r#""hi""#);
        assert_eq!(Value::from("a\nb").to_string(), r#""a\nb""#);
        assert_eq!(Value::from("hi").content_string(), "hi");
    }

    #[test]
    fn names_display_with_their_quote() {
        // `'x` on the stack, so a name and a look-alike number/string are
        // distinguishable; `content_string` drops the quote.
        let name = Value::Name(Rc::from("x"));
        assert_eq!(name.to_string(), "'x");
        assert_eq!(name.content_string(), "x");
    }
}
