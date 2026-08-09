//! Errors, split by phase (`language-v2.md` §3: "syntax errors are free;
//! semantic errors are transactional"):
//!
//! - [`ParseError`] — a [`ParseErrorKind`] plus the [`Span`] of the offending
//!   text. Raised before evaluation, so there is no state to restore: an
//!   unbalanced `{` costs nothing.
//! - [`ErrorKind`] — the state-independent *semantic* error, paired by
//!   [`CalcError`] with a [`Trace`] of the program that was running.
//!
//! [`Outcome`] is what [`Engine::apply`](super::Engine::apply) returns.

use super::{Element, Span};

/// What went wrong — the semantic error, independent of any engine state. This
/// is what the pure index helpers produce; a failing engine op pairs it with the
/// engine to form a [`CalcError`]. Syntax has its own type ([`ParseError`]).
#[derive(Debug, Clone, PartialEq)]
pub enum ErrorKind {
    /// An operation needed more operands than the stack held.
    StackUnderflow,
    /// Division with a zero divisor.
    DivideByZero,
    /// A `]` reached with no mark on the stack. The parser already pairs the
    /// brackets in the text, so this is the *runtime* half of the discipline:
    /// which mark a closer consumes is settled by permutation, not by the text
    /// (§6).
    UnmatchedClose,
    /// A parsed construct the evaluator doesn't run yet — functions (V3), dicts
    /// and attribute access (V5). The parser accepts the whole v2 surface ahead
    /// of the evaluator, so this names the milestone that will retire it.
    Unimplemented(&'static str),
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
            ErrorKind::UnmatchedClose => write!(f, "no open collection to close"),
            ErrorKind::Unimplemented(what) => write!(f, "not yet implemented: {what}"),
            ErrorKind::IndexOutOfRange => write!(f, "index out of range"),
            ErrorKind::UnboundName(n) => write!(f, "unbound name: {n}"),
            ErrorKind::TypeError { expected, found } => {
                write!(f, "expected {expected}, found {found}")
            }
        }
    }
}

/// A syntax error: what is wrong, and where. Every kind here is detectable
/// before evaluation, and all but [`ParseErrorKind::UnclosedOpen`] are
/// detectable *at* the offending token rather than at end of input
/// (`language-v2.md` §3).
#[derive(Debug, Clone, PartialEq)]
pub enum ParseErrorKind {
    /// A `"` literal ran to end-of-input without a closing `"`. The tokenizer's
    /// error; every other kind is the parser's.
    UnterminatedString,
    /// A closer with nothing of its kind open: `1 2 ]`.
    UnmatchedClose(char),
    /// An opener never closed: `[ 1 2`. The one error that can only be raised at
    /// end of input, so its span points back at the opener.
    UnclosedOpen(char),
    /// A closer that crosses a region opened inside another: `{ [ } ]`. Regions
    /// must nest, so the `}` here would close across the still-open `[`.
    CrossingClose { closer: char, crossed: char },
    /// A sigil with nothing usable following it: a trailing `'`, or `&{`. The
    /// ten fixed characters can't appear in a name, so only a word will do.
    ExpectedName { after: char },
    /// A `:` outside a template's leading parameter list — the one construct the
    /// parser recognizes by position (§5), so `:` anywhere else is an error.
    MisplacedColon,
    /// A parameter that isn't a name: `{x 3: …}`. A parameter list is syntax, so
    /// it can be strict where a name *datum* can't — `'3 set` stays legal, since
    /// there the name is a value the program chose.
    InvalidParameter,
    /// Templates nested past the parser's recursion limit. Not a language rule —
    /// an implementation bound, so that pathological input is a *diagnostic*
    /// rather than a stack overflow that would abort the process and take the
    /// session's stack and history with it.
    TooDeeplyNested,
}

impl std::fmt::Display for ParseErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseErrorKind::UnterminatedString => write!(f, "unterminated string"),
            ParseErrorKind::UnmatchedClose(c) => write!(f, "unmatched `{c}`"),
            ParseErrorKind::UnclosedOpen(c) => write!(f, "unclosed `{c}`"),
            ParseErrorKind::CrossingClose { closer, crossed } => {
                write!(f, "`{closer}` crosses an open `{crossed}`")
            }
            ParseErrorKind::ExpectedName { after } => write!(f, "expected a name after `{after}`"),
            ParseErrorKind::MisplacedColon => {
                write!(f, "`:` is only valid after a template's parameter names")
            }
            ParseErrorKind::InvalidParameter => write!(f, "not a name, so not a parameter"),
            ParseErrorKind::TooDeeplyNested => write!(f, "templates nested too deeply"),
        }
    }
}

/// A syntax error paired with the [`Span`] of the text to blame. The span is
/// what lets a caller underline the offending characters; `Display` is the
/// message alone, since the source isn't the error's to hold.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub kind: ParseErrorKind,
    pub span: Span,
}

impl ParseError {
    pub(crate) fn new(kind: ParseErrorKind, span: Span) -> Self {
        Self { kind, span }
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.kind)
    }
}

impl std::error::Error for ParseError {}

/// The call chain when an error struck — "here's what was running", at every
/// depth rather than only the outermost.
///
/// A flat program-plus-index cannot describe a failure inside a function: the
/// index would point into *that function's* template, which means nothing
/// against the line the user typed. So a trace is one [`Call`] per live
/// activation, **outermost first** — `calls[0]` is always the line itself.
///
/// A tail call leaves no level here, because it left no activation: it replaced
/// its caller rather than stacking on it. That is the usual bargain — the same
/// one every language with proper tail calls makes.
#[derive(Debug, Clone, PartialEq)]
pub struct Trace {
    pub calls: Vec<Call>,
}

/// One level of a [`Trace`]: what was running, and where in it.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    /// The template being run — a line, or a function's body.
    pub template: std::rc::Rc<[Element]>,
    /// The 0-based index within `template` of the element that was running:
    /// the one that failed at the innermost level, and the call that led
    /// inward at every other.
    pub index: usize,
}

/// A semantic error plus the context to show it: what went wrong, and (for a
/// runtime error) the program it was running.
#[derive(Debug, Clone, PartialEq)]
pub struct CalcError {
    /// What went wrong.
    pub kind: ErrorKind,
    /// The program being run and which element failed, for a runtime error.
    /// `None` for a bare-op failure that carries no program (a TUI operator key,
    /// a cursor edit). A syntax error never gets here — it is a [`ParseError`],
    /// raised before there is any program to trace.
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
        // Innermost first — what failed, then outward to the line that reached
        // it: `too few arguments in `dup [*]`, called from `4 [sq]``.
        for (depth, call) in self
            .trace
            .iter()
            .flat_map(|t| t.calls.iter().rev())
            .enumerate()
        {
            f.write_str(match depth {
                0 => " in `",
                _ => ", called from `",
            })?;
            for (i, element) in call.template.iter().enumerate() {
                match (i, i == call.index) {
                    (0, true) => write!(f, "[{element}]")?,
                    (0, false) => write!(f, "{element}")?,
                    (_, true) => write!(f, " [{element}]")?,
                    (_, false) => write!(f, " {element}")?,
                }
            }
            f.write_str("`")?;
        }
        Ok(())
    }
}

impl std::error::Error for CalcError {}

/// The result of [`Engine::apply`]: the engine mutated in place, or a
/// [`CalcError`] naming what failed. A failure leaves the engine part-way
/// through the batch — the caller restores the [`State`](super::State) it took
/// beforehand.
pub type Outcome = Result<(), CalcError>;
