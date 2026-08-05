//! Errors: the state-independent [`ErrorKind`], and the [`CalcError`] that pairs
//! it with a [`Trace`] of the program that was running. [`Outcome`] is what
//! [`Engine::apply`](super::Engine::apply) returns.

use super::{Element, Engine};

/// What went wrong — the semantic error, independent of any engine state. This
/// is what the pure parsing/index helpers produce; a failing engine op pairs it
/// with the engine to form a [`CalcError`].
#[derive(Debug, Clone, PartialEq)]
pub enum ErrorKind {
    /// An operation needed more operands than the stack held.
    StackUnderflow,
    /// Division with a zero divisor.
    DivideByZero,
    /// A `"` string literal ran to end-of-input without a closing `"`.
    UnterminatedString,
    /// A `]` with no open collection to close (or, later, one whose open mark is
    /// the wrong kind — a `]` closing a `{`).
    UnmatchedClose,
    /// A list index (or `first`/`rest` on an empty list) fell outside the list.
    IndexOutOfRange,
    /// `get` on a name with no binding in the environment.
    UnboundName(String),
    /// An operation got a value of the wrong type — e.g. `+` on a bool. The
    /// failing word is named by the surrounding [`Trace`], so this only records
    /// the type mismatch itself.
    TypeError {
        expected: &'static str,
        found: &'static str,
    },
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErrorKind::StackUnderflow => write!(f, "too few arguments"),
            ErrorKind::DivideByZero => write!(f, "divide by zero"),
            ErrorKind::UnterminatedString => write!(f, "unterminated string"),
            ErrorKind::UnmatchedClose => write!(f, "unmatched ]"),
            ErrorKind::IndexOutOfRange => write!(f, "index out of range"),
            ErrorKind::UnboundName(n) => write!(f, "unbound name: {n}"),
            ErrorKind::TypeError { expected, found } => {
                write!(f, "expected {expected}, found {found}")
            }
        }
    }
}

/// The program that was executing when an error struck, and the index of the
/// element that failed — "here's what was running."
#[derive(Debug, Clone, PartialEq)]
pub struct Trace {
    /// The whole program being applied.
    pub program: Vec<Element>,
    /// The 0-based index within `program` of the element that failed.
    pub index: usize,
}

/// A semantic error plus the context to show it: what went wrong, and (for a
/// runtime error) the program it was running.
#[derive(Debug, Clone, PartialEq)]
pub struct CalcError {
    /// What went wrong.
    pub kind: ErrorKind,
    /// The program being run and which element failed, for a runtime error.
    /// `None` for parse errors (whose offending token is already named in
    /// `kind`) and for bare-op failures that carry no program.
    pub trace: Option<Trace>,
}

/// A bare error with no batch context — used where a single op fails outside a
/// program (e.g. the TUI's cursor edits).
impl From<ErrorKind> for CalcError {
    fn from(kind: ErrorKind) -> Self {
        Self { kind, trace: None }
    }
}

impl std::fmt::Display for CalcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.kind)?;
        if let Some(trace) = &self.trace {
            // Show the program with the failing element bracketed, e.g.
            // `1 2 + [/]`.
            write!(f, " in `")?;
            for (i, element) in trace.program.iter().enumerate() {
                match (i, i == trace.index) {
                    (0, true) => write!(f, "[{element}]")?,
                    (0, false) => write!(f, "{element}")?,
                    (_, true) => write!(f, " [{element}]")?,
                    (_, false) => write!(f, " {element}")?,
                }
            }
            write!(f, "`")?;
        }
        Ok(())
    }
}

impl std::error::Error for CalcError {}

/// The result of [`Engine::apply`]: the engine threaded through the whole batch,
/// or a [`CalcError`] naming what failed.
pub type Outcome = Result<Engine, CalcError>;
