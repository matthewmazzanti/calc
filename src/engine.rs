//! The RPN calculator engine: an [`Engine`] wrapping a stack of values (and,
//! later, evaluation settings). No I/O and no history. The individual ops take
//! `&mut self` and return `Result<(), ErrorKind>` — they pop, mutate in place,
//! and bail with `?`. [`Engine::apply`] runs a whole slice of commands and is
//! the consuming boundary: it threads one engine through the batch and, on the
//! first failure, attaches the [`Trace`] to make a [`CalcError`]. Turning a line
//! of text into commands is a frontend concern, handled by [`parse`].
//!
//! **Atomicity is the caller's, not the op's.** An op may leave the stack
//! half-consumed when it fails (it pops before it type-checks), so there is no
//! "operands intact on error" guarantee. That doesn't matter, because every
//! caller applies to a *copy* and commits only on `Ok` (see the `history`
//! module) — a failed batch's damage is confined to a discarded clone, and the
//! caller's own engine is untouched. This is the standard transactional model.

use std::rc::Rc;

/// The kind of an open collection, carried by its [`Value::Mark`]. Only lists
/// for now; `{` will add a function mark (carrying the captured environment) in
/// the next milestone. Typed so a `]` closing a `{` can be caught as a mismatch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MarkKind {
    List,
}

/// A value on the stack. Started as a bare `f64`; now a small sum type so the
/// stack can hold more than numbers. Grows further later (functions). No longer
/// `Copy` — `Str`/`List` own heap data.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// An integer. Preserved through `+ - *` and `neg` when both operands are
    /// integers; any float operand (or overflow) promotes to [`Value::Num`].
    Int(i64),
    /// A float. `/` always yields one, and mixed int/float arithmetic promotes
    /// to it. A real numeric tower (rationals, complex) comes later.
    Num(f64),
    /// A boolean — a genuine type, not Forth's 0/-1. Produced by comparisons
    /// and the boolean words, and (later) consumed by `if`.
    Bool(bool),
    /// A string. Heap-shared via `Rc`, so a clone (a `dup`, a lookup) is a
    /// refcount bump; concatenation copies-on-write via `Rc::make_mut`. Built by
    /// the tokenizer's `"…"` literals and by `to_str`; concatenated with `+`.
    Str(Rc<String>),
    /// A list — a growable, heterogeneous sequence, `Rc`-shared like `Str`.
    /// Built by the `[ … ]` words via the mark discipline, never a `Push`
    /// literal; the list ops copy-on-write.
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
    /// A captured primitive op — a first-class word. A *bare* word runs its op;
    /// `'name get` instead pushes it here, so a builtin can be stored, passed,
    /// and later applied. `Builtin` is `Copy`, so this stays cheap. Functions
    /// (`{ … }`) will join it as the other callable value.
    Builtin(Builtin),
}

impl Value {
    /// The type's name, for error messages ("expected number, found bool").
    /// `Int` and `Num` are both "number" — the split is invisible to the type
    /// errors, since the arithmetic words accept either.
    fn type_name(&self) -> &'static str {
        match self {
            Value::Int(_) | Value::Num(_) => "number",
            Value::Bool(_) => "bool",
            Value::Str(_) => "string",
            Value::List(_) => "list",
            Value::Name(_) => "name",
            Value::Builtin(_) => "builtin",
            // The open-collection sentinel isn't a first-class value: the value
            // words reject it, naming it as an "open list" in the error.
            Value::Mark(MarkKind::List) => "open list",
        }
    }

    /// Widen to `f64`, or a [`ErrorKind::TypeError`] naming what was found.
    /// Comparisons, division, and mixed arithmetic funnel operands through this,
    /// so an `Int` is accepted wherever a number is wanted. A `Mark` is not a
    /// value — it falls through to the type error, so `[ 1 +` is a type error.
    fn as_num(&self) -> Result<f64, ErrorKind> {
        match self {
            Value::Int(i) => Ok(*i as f64),
            Value::Num(n) => Ok(*n),
            other => Err(ErrorKind::TypeError {
                expected: "number",
                found: other.type_name(),
            }),
        }
    }

    /// Extract a boolean, or a [`ErrorKind::TypeError`]. The boolean words
    /// (`not`/`and`/`or`) funnel their operands through this.
    fn as_bool(&self) -> Result<bool, ErrorKind> {
        match self {
            Value::Bool(b) => Ok(*b),
            other => Err(ErrorKind::TypeError {
                expected: "bool",
                found: other.type_name(),
            }),
        }
    }

    /// Interpret as a 1-based stack level: a positive `Int`. A float is
    /// rejected outright (no rounding) so `3.5 roll` errors rather than
    /// guessing; a non-positive `Int` clamps to 0, which the range check then
    /// reports as underflow. The indexed stack words funnel their level operand
    /// through this.
    fn as_index(&self) -> Result<usize, ErrorKind> {
        match self {
            Value::Int(i) => Ok((*i).max(0) as usize),
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

    /// The plain content string, no quotes — what `to_str` produces. For a
    /// `Str` that's the content itself; for anything else it's the `Display`
    /// form, so `3 to_str` is `"3"` and `true to_str` is `"true"`.
    fn content_string(&self) -> String {
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
            // number on the stack, and so a `Push` renders re-parseably in a
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

/// The stack of values, bottom-to-top: the top of stack is the last element.
/// Internal — the public handle is [`Engine`].
type Stack = Vec<Value>;

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

/// A program element: a literal to push, or a word to resolve at runtime
/// (language.md §12 — "a word reference or a literal"). `parse` produces a flat
/// `Vec<Element>` — no AST, since RPN has no nesting — and a function body will
/// be one too. This is the *only* thing a program contains; the primitive ops
/// ([`Builtin`]) are reached only by resolving a `Word`.
#[derive(Debug, Clone, PartialEq)]
pub enum Element {
    /// A literal value: a number, string, name, or boolean.
    Literal(Value),
    /// A bare word, resolved against the environment at runtime: a user binding
    /// (which shadows), else a builtin from the prelude, else `UnboundName`.
    Word(Rc<str>),
}

/// A primitive operation — the builtin vocabulary. Reached only by resolving a
/// [`Element::Word`] (a bare word, or the TUI dispatching one directly), never
/// present in a program. `Copy`, since none carry data.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Builtin {
    Add,
    Sub,
    Mul,
    Div,
    Neg,
    /// Equality — pops two values, pushes a `Bool` (a number never equals a
    /// bool); inequality is `=` then `not`.
    Eq,
    /// Ordering comparisons — pop two numbers, push a `Bool`.
    Lt,
    Gt,
    Le,
    Ge,
    /// Boolean ops — `Bool`s only (no truthiness rule).
    Not,
    And,
    Or,
    /// The element count of a string (characters) or list, as an `Int`.
    Length,
    /// Convert the top value to its string content (no quotes).
    ToStr,

    // Fixed shuffles.
    Dup,     // a -- a a
    Drop,    // a --
    Swap,    // a b -- b a
    Over,    // a b -- a b a
    Rot,     // a b c -- b c a
    Unrot,   // a b c -- c a b
    Nip,     // a b -- b
    Tuck,    // a b -- b a b
    Dupd,    // a b -- a a b
    TwoDup,  // a b -- a b a b   (2dup)
    TwoDrop, // a b --           (2drop)

    // Indexed ops: the 1-based level is popped off the stack (`n rolln`).
    PickN,
    RollN,
    RolldN,
    DropN,
    SwapN,

    // Lists.
    OpenList,  // [
    CloseList, // ]
    First,     // [a b c] -- a
    Rest,      // [a b c] -- [b c]
    Cons,      // x [b c] -- [x b c]
    Append,    // [a b] [c d] -- [a b c d]
    Nth,       // [a b c] n -- (0-based nth element)

    // Environment.
    Set, // value name --
    Get, // name -- value

    Clear,
}

impl Element {
    /// Parse one whitespace-delimited token into an `Element`. A number, a
    /// `'x` name, or `true`/`false` becomes a `Literal`; every other token is a
    /// [`Element::Word`], resolved against the environment at runtime. So parsing
    /// never fails on an unknown word — that's a runtime `UnboundName`.
    pub fn parse(token: &str) -> Element {
        // The `'` sigil: `'x` pushes the name `x` (§3). Owned here rather than
        // as a builtin word so it can't be shadowed.
        if let Some(name) = token.strip_prefix('\'') {
            return Element::Literal(Value::Name(Rc::from(name)));
        }
        // Boolean literals — like numbers, they're literals, not words.
        match token {
            "true" => return Element::Literal(Value::Bool(true)),
            "false" => return Element::Literal(Value::Bool(false)),
            _ => {}
        }
        // Integer first, then float: `3` is an `Int`, but `3.0`/`2e3`/`1e-2`
        // (anything with a `.`, exponent, or out of i64 range) is a `Num`.
        if let Ok(i) = token.parse::<i64>() {
            return Element::Literal(Value::Int(i));
        }
        if let Ok(n) = token.parse::<f64>() {
            return Element::Literal(Value::Num(n));
        }
        Element::Word(Rc::from(token))
    }
}

impl Builtin {
    /// Every builtin, in declaration order — the single enumeration of the
    /// vocabulary, used to build the prelude frame ([`prelude`]). `run_builtin`
    /// and [`Display`](std::fmt::Display) are exhaustiveness-checked, so a new
    /// variant makes them fail to compile; *this* list is not, so keep it
    /// complete (the `every_builtin_is_in_the_prelude` test guards it).
    const ALL: &'static [Builtin] = &[
        Builtin::Add,
        Builtin::Sub,
        Builtin::Mul,
        Builtin::Div,
        Builtin::Neg,
        Builtin::Eq,
        Builtin::Lt,
        Builtin::Gt,
        Builtin::Le,
        Builtin::Ge,
        Builtin::Not,
        Builtin::And,
        Builtin::Or,
        Builtin::Length,
        Builtin::ToStr,
        Builtin::Dup,
        Builtin::Drop,
        Builtin::Swap,
        Builtin::Over,
        Builtin::Rot,
        Builtin::Unrot,
        Builtin::Nip,
        Builtin::Tuck,
        Builtin::Dupd,
        Builtin::TwoDup,
        Builtin::TwoDrop,
        Builtin::PickN,
        Builtin::RollN,
        Builtin::RolldN,
        Builtin::DropN,
        Builtin::SwapN,
        Builtin::OpenList,
        Builtin::CloseList,
        Builtin::First,
        Builtin::Rest,
        Builtin::Cons,
        Builtin::Append,
        Builtin::Nth,
        Builtin::Set,
        Builtin::Get,
        Builtin::Clear,
    ];
}

impl std::fmt::Display for Element {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Element::Literal(v) => write!(f, "{v}"),
            Element::Word(name) => write!(f, "{name}"),
        }
    }
}

impl std::fmt::Display for Builtin {
    /// The canonical word, so a directly-dispatched op (a TUI operator) can be
    /// labelled in the info bar.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Builtin::Add => write!(f, "+"),
            Builtin::Sub => write!(f, "-"),
            Builtin::Mul => write!(f, "*"),
            Builtin::Div => write!(f, "/"),
            Builtin::Neg => write!(f, "neg"),
            Builtin::Eq => write!(f, "="),
            Builtin::Lt => write!(f, "<"),
            Builtin::Gt => write!(f, ">"),
            Builtin::Le => write!(f, "<="),
            Builtin::Ge => write!(f, ">="),
            Builtin::Not => write!(f, "not"),
            Builtin::And => write!(f, "and"),
            Builtin::Or => write!(f, "or"),
            Builtin::Length => write!(f, "length"),
            Builtin::ToStr => write!(f, "to_str"),
            Builtin::Dup => write!(f, "dup"),
            Builtin::Drop => write!(f, "drop"),
            Builtin::Swap => write!(f, "swap"),
            Builtin::Over => write!(f, "over"),
            Builtin::Rot => write!(f, "rot"),
            Builtin::Unrot => write!(f, "unrot"),
            Builtin::Nip => write!(f, "nip"),
            Builtin::Tuck => write!(f, "tuck"),
            Builtin::Dupd => write!(f, "dupd"),
            Builtin::TwoDup => write!(f, "2dup"),
            Builtin::TwoDrop => write!(f, "2drop"),
            Builtin::PickN => write!(f, "pickn"),
            Builtin::RollN => write!(f, "rolln"),
            Builtin::RolldN => write!(f, "rolldn"),
            Builtin::DropN => write!(f, "dropn"),
            Builtin::SwapN => write!(f, "swapn"),
            Builtin::OpenList => write!(f, "["),
            Builtin::CloseList => write!(f, "]"),
            Builtin::First => write!(f, "first"),
            Builtin::Rest => write!(f, "rest"),
            Builtin::Cons => write!(f, "cons"),
            Builtin::Append => write!(f, "append"),
            Builtin::Nth => write!(f, "nth"),
            Builtin::Set => write!(f, "set"),
            Builtin::Get => write!(f, "get"),
            Builtin::Clear => write!(f, "clear"),
        }
    }
}

/// Parse a line into a program (a `Vec<Element>`), or fail on an unterminated
/// string — the one lexical error. An unknown *word* is not a parse error; it
/// becomes an `Element::Word` and fails (if unbound) at runtime.
///
/// Mostly a whitespace split, but with the §4 lookahead: a `"` opens a string
/// literal that runs (across spaces) to its closing `"`, so strings are the one
/// thing [`Element::parse`] never sees — the tokenizer owns them. Every other
/// token is handed to [`Element::parse`] word-for-word.
pub fn parse(input: &str) -> Result<Vec<Element>, ErrorKind> {
    let mut program = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' {
            program.push(Element::Literal(Value::Str(Rc::new(read_string(&mut chars)?))));
        } else {
            // A plain word: everything up to the next whitespace.
            let mut word = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                word.push(c);
                chars.next();
            }
            program.push(Element::parse(&word));
        }
    }
    Ok(program)
}

/// Read a `"…"` literal, the opening quote still unconsumed. Supports the
/// escapes `\"`, `\\`, `\n`, `\t`; an unknown escape keeps both characters
/// verbatim. Fails with [`ErrorKind::UnterminatedString`] at end-of-input.
fn read_string(
    chars: &mut std::iter::Peekable<std::str::Chars>,
) -> Result<String, ErrorKind> {
    chars.next(); // opening quote
    let mut s = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Ok(s),
            '\\' => match chars.next() {
                Some('"') => s.push('"'),
                Some('\\') => s.push('\\'),
                Some('n') => s.push('\n'),
                Some('t') => s.push('\t'),
                Some(other) => {
                    s.push('\\');
                    s.push(other);
                }
                None => return Err(ErrorKind::UnterminatedString),
            },
            _ => s.push(c),
        }
    }
    Err(ErrorKind::UnterminatedString)
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
/// runtime error) the command sequence it was running.
#[derive(Debug, Clone, PartialEq)]
pub struct CalcError {
    /// What went wrong.
    pub kind: ErrorKind,
    /// The command sequence being run and which command failed, for a runtime
    /// error. `None` for parse errors (whose offending token is already named in
    /// `kind`) and for bare-op failures that carry no batch.
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
            // Show the batch with the failing command bracketed, e.g.
            // `1 2 + [/]`.
            write!(f, " in `")?;
            for (i, command) in trace.program.iter().enumerate() {
                match (i, i == trace.index) {
                    (0, true) => write!(f, "[{command}]")?,
                    (0, false) => write!(f, "{command}")?,
                    (_, true) => write!(f, " [{command}]")?,
                    (_, false) => write!(f, " {command}")?,
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

/// Build the prelude frame — every builtin as a first-class [`Value::Builtin`]
/// under its canonical word (from [`Display`](std::fmt::Display), the single
/// source of names). Each engine holds this behind an `Rc`, so snapshots share
/// one immutable copy.
fn prelude() -> Rc<Frame> {
    Rc::new(
        Builtin::ALL
            .iter()
            .map(|&b| (Rc::from(b.to_string()), Value::Builtin(b)))
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

    /// Apply a batch of commands in order, threading one engine through the
    /// whole batch (this is the consuming boundary). The first failure
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

    /// Dispatch a primitive op. Reached by [`Engine::resolve_word`], or called
    /// directly by the TUI for its operator keys. A total match, so a new
    /// [`Builtin`] variant is a compile error until handled.
    pub(crate) fn run_builtin(&mut self, builtin: Builtin) -> Result<(), ErrorKind> {
        match builtin {
            // `+` concatenates two strings, else adds numbers.
            Builtin::Add => self.add(),
            Builtin::Sub => self.arith(i64::checked_sub, |a, b| a - b),
            Builtin::Mul => self.arith(i64::checked_mul, |a, b| a * b),
            // Division always yields a float — `1 2 /` is `0.5`, not `0`.
            Builtin::Div => self.num_binary(|a, b| {
                if b == 0.0 {
                    Err(ErrorKind::DivideByZero)
                } else {
                    Ok(a / b)
                }
            }),
            Builtin::Neg => self.negate(),
            Builtin::Eq => self.equality(),
            Builtin::Lt => self.num_compare(|a, b| a < b),
            Builtin::Gt => self.num_compare(|a, b| a > b),
            Builtin::Le => self.num_compare(|a, b| a <= b),
            Builtin::Ge => self.num_compare(|a, b| a >= b),
            Builtin::Not => self.bool_unary(|a| !a),
            Builtin::And => self.bool_binary(|a, b| a && b),
            Builtin::Or => self.bool_binary(|a, b| a || b),
            Builtin::Length => self.length(),
            Builtin::ToStr => self.stringify(),
            // Fixed shuffles — several are just a fixed level of an indexed op.
            Builtin::Dup => self.pick_at(1),
            Builtin::Over => self.pick_at(2),
            Builtin::Rot => self.roll_at(3),
            Builtin::Unrot => self.rolld_at(3),
            Builtin::Drop => self.drop_at(1),
            Builtin::Nip => self.drop_at(2),
            Builtin::Swap => self.swap_at(1),
            Builtin::Tuck => self.tuck(),
            Builtin::Dupd => self.dupd(),
            Builtin::TwoDup => self.two_dup(),
            Builtin::TwoDrop => self.two_drop(),
            // Indexed: pop the level, then run the op.
            Builtin::PickN => self.indexed(Engine::pick_at),
            Builtin::RollN => self.indexed(Engine::roll_at),
            Builtin::RolldN => self.indexed(Engine::rolld_at),
            Builtin::DropN => self.indexed(Engine::drop_at),
            Builtin::SwapN => self.indexed(Engine::swap_at),
            Builtin::OpenList => {
                self.stack.push(Value::Mark(MarkKind::List));
                Ok(())
            }
            Builtin::CloseList => self.close_list(),
            Builtin::First => self.first(),
            Builtin::Rest => self.rest(),
            Builtin::Cons => self.cons(),
            Builtin::Append => self.append(),
            Builtin::Nth => self.nth(),
            Builtin::Set => self.set(),
            Builtin::Get => self.get(),
            Builtin::Clear => {
                self.stack.clear();
                Ok(())
            }
        }
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
        let i = self.index_of_level(level).ok_or(ErrorKind::StackUnderflow)?;
        let v = self.stack[i].clone();
        self.stack.push(v);
        Ok(())
    }

    /// Remove the value at `level` (`drop` = 1, `nip` = 2, `dropn`).
    pub(crate) fn drop_at(&mut self, level: usize) -> Result<(), ErrorKind> {
        let i = self.index_of_level(level).ok_or(ErrorKind::StackUnderflow)?;
        self.stack.remove(i);
        Ok(())
    }

    /// Exchange the value at `level` with the one just below it. `swap` = 1.
    pub(crate) fn swap_at(&mut self, level: usize) -> Result<(), ErrorKind> {
        let i = self.index_of_level(level).ok_or(ErrorKind::StackUnderflow)?;
        let j = self
            .index_of_level(level + 1)
            .ok_or(ErrorKind::StackUnderflow)?;
        self.stack.swap(i, j);
        Ok(())
    }

    /// Move the value at `level` up to the top. `rot` = 3, `rolln`.
    pub(crate) fn roll_at(&mut self, level: usize) -> Result<(), ErrorKind> {
        let i = self.index_of_level(level).ok_or(ErrorKind::StackUnderflow)?;
        let v = self.stack.remove(i);
        self.stack.push(v);
        Ok(())
    }

    /// Move the top value down to `level` — the inverse of `roll_at`.
    /// `unrot` = 3, `rolldn`.
    fn rolld_at(&mut self, level: usize) -> Result<(), ErrorKind> {
        let dest = self.index_of_level(level).ok_or(ErrorKind::StackUnderflow)?;
        // `dest` is where the top must land. Popping first leaves every index
        // ≤ dest unchanged (dest ≤ len - 1), so we can insert straight in.
        let v = self.stack.pop().expect("level ≥ 1 implies a non-empty stack");
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
            Value::Builtin(builtin) => self.run_builtin(builtin),
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
mod tests {
    use super::*;

    /// Parse and run `input` from a fresh engine and return the result.
    fn run(input: &str) -> Engine {
        Engine::new().apply(&parse(input).unwrap()).unwrap()
    }

    #[test]
    fn pushes_numbers() {
        assert_eq!(run("1 2 3").stack(), &[1.0, 2.0, 3.0]);
    }

    #[test]
    fn parses_negatives_and_decimals() {
        assert_eq!(run("-1.5 2e3").stack(), &[-1.5, 2000.0]);
    }

    #[test]
    fn arithmetic() {
        assert_eq!(run("1 2 +").stack(), &[3.0]);
        assert_eq!(run("10 3 -").stack(), &[7.0]);
        assert_eq!(run("4 5 *").stack(), &[20.0]);
        assert_eq!(run("20 4 /").stack(), &[5.0]);
    }

    #[test]
    fn operand_order_is_left_to_right() {
        // `7 3 -` is 7 - 3, not 3 - 7.
        assert_eq!(run("7 3 -").stack(), &[4.0]);
        assert_eq!(run("8 2 /").stack(), &[4.0]);
    }

    /// The `ErrorKind` from running `input` against a fresh engine.
    fn run_err(input: &str) -> ErrorKind {
        Engine::new().apply(&parse(input).unwrap()).unwrap_err().kind
    }

    #[test]
    fn true_and_false_are_literals() {
        assert_eq!(run("true false").stack(), &[true, false]);
    }

    #[test]
    fn comparisons_push_a_bool() {
        assert_eq!(run("1 2 <").stack(), &[true]);
        assert_eq!(run("1 2 >").stack(), &[false]);
        assert_eq!(run("2 2 <=").stack(), &[true]);
        assert_eq!(run("2 2 >=").stack(), &[true]);
        assert_eq!(run("3 2 >=").stack(), &[true]);
    }

    #[test]
    fn equality_works_across_types() {
        assert_eq!(run("2 2 =").stack(), &[true]);
        assert_eq!(run("2 3 =").stack(), &[false]);
        assert_eq!(run("true true =").stack(), &[true]);
        // A number never equals a bool — but it's not an error, just false.
        assert_eq!(run("1 true =").stack(), &[false]);
    }

    #[test]
    fn boolean_words_operate_on_bools() {
        assert_eq!(run("true not").stack(), &[false]);
        assert_eq!(run("true false and").stack(), &[false]);
        assert_eq!(run("true false or").stack(), &[true]);
        // Inequality is `=` then `not`.
        assert_eq!(run("2 3 = not").stack(), &[true]);
    }

    #[test]
    fn arithmetic_on_a_bool_is_a_type_error() {
        assert_eq!(
            run_err("true 1 +"),
            ErrorKind::TypeError {
                expected: "number",
                found: "bool"
            }
        );
    }

    #[test]
    fn boolean_words_reject_numbers() {
        // No truthiness rule: `and`/`or`/`not` are bool-only.
        assert_eq!(
            run_err("1 not"),
            ErrorKind::TypeError {
                expected: "bool",
                found: "number"
            }
        );
        assert_eq!(
            run_err("1 2 and"),
            ErrorKind::TypeError {
                expected: "bool",
                found: "number"
            }
        );
    }

    #[test]
    fn a_type_error_names_the_mismatch_and_the_command() {
        // Ops no longer preserve their operands on error — atomicity is the
        // caller's transaction (see `an_error_leaves_the_callers_engine_untouched`).
        // The error still names the mismatch and which command failed.
        let err = Engine::new().apply(&parse("true 1 +").unwrap()).unwrap_err();
        assert_eq!(
            err.kind,
            ErrorKind::TypeError {
                expected: "number",
                found: "bool"
            }
        );
        let trace = err.trace.unwrap();
        assert_eq!(trace.program[trace.index], Element::Word(Rc::from("+")));
    }

    #[test]
    fn values_display_without_type_noise() {
        assert_eq!(Value::Int(3).to_string(), "3");
        assert_eq!(Value::Num(3.5).to_string(), "3.5");
        assert_eq!(Value::Bool(true).to_string(), "true");
        assert_eq!(Value::Bool(false).to_string(), "false");
    }

    #[test]
    fn bare_numbers_are_ints_dotted_ones_are_floats() {
        assert_eq!(run("3").stack(), &[Value::Int(3)]);
        assert_eq!(run("-5").stack(), &[Value::Int(-5)]);
        // A `.` or exponent forces a float.
        assert_eq!(run("3.0").stack(), &[Value::Num(3.0)]);
        assert_eq!(run("2e3").stack(), &[Value::Num(2000.0)]);
    }

    #[test]
    fn integer_arithmetic_stays_integer() {
        assert_eq!(run("2 3 +").stack(), &[Value::Int(5)]);
        assert_eq!(run("2 3 -").stack(), &[Value::Int(-1)]);
        assert_eq!(run("4 5 *").stack(), &[Value::Int(20)]);
        assert_eq!(run("5 neg").stack(), &[Value::Int(-5)]);
    }

    #[test]
    fn division_always_yields_a_float() {
        // Even when it divides evenly: `4 2 /` is `Num(2.0)`, not `Int(2)`.
        assert_eq!(run("4 2 /").stack(), &[Value::Num(2.0)]);
        assert_eq!(run("1 2 /").stack(), &[Value::Num(0.5)]);
    }

    #[test]
    fn a_float_operand_promotes_the_whole_expression() {
        assert_eq!(run("2 3.0 +").stack(), &[Value::Num(5.0)]);
        assert_eq!(run("2.0 3 *").stack(), &[Value::Num(6.0)]);
    }

    #[test]
    fn integer_overflow_promotes_to_float() {
        // i64::MAX * 2 can't be an Int, so it becomes a float rather than wrap.
        assert_eq!(
            run("9223372036854775807 2 *").stack(),
            &[Value::Num(9223372036854775807.0 * 2.0)]
        );
    }

    #[test]
    fn equality_spans_the_int_float_split() {
        assert_eq!(run("2 2.0 =").stack(), &[true]);
        assert_eq!(run("2 3.0 =").stack(), &[false]);
    }

    #[test]
    fn string_literals_hold_their_spaces() {
        assert_eq!(run(r#""hello""#).stack(), &[Value::from("hello")]);
        // The tokenizer's lookahead keeps the interior spaces as one token.
        assert_eq!(
            run(r#""hello world""#).stack(),
            &[Value::from("hello world")]
        );
    }

    #[test]
    fn string_escapes_are_decoded() {
        assert_eq!(run(r#""a\nb\tc""#).stack(), &[Value::from("a\nb\tc")]);
        assert_eq!(run(r#""say \"hi\"""#).stack(), &[Value::from(r#"say "hi""#)]);
    }

    #[test]
    fn an_unterminated_string_is_a_parse_error() {
        assert_eq!(parse(r#""oops"#), Err(ErrorKind::UnterminatedString));
        assert_eq!(parse(r#""bad \"#), Err(ErrorKind::UnterminatedString));
    }

    #[test]
    fn plus_concatenates_two_strings() {
        assert_eq!(run(r#""foo" "bar" +"#).stack(), &[Value::from("foobar")]);
    }

    #[test]
    fn plus_does_not_mix_strings_and_numbers() {
        // No implicit `to_str`: the numeric path rejects the string.
        assert_eq!(
            run_err(r#""foo" 1 +"#),
            ErrorKind::TypeError {
                expected: "number",
                found: "string"
            }
        );
    }

    #[test]
    fn length_counts_characters() {
        assert_eq!(run(r#""hello" length"#).stack(), &[Value::Int(5)]);
        assert_eq!(run(r#""" length"#).stack(), &[Value::Int(0)]);
        assert_eq!(
            run_err("1 length"),
            ErrorKind::TypeError {
                expected: "string or list",
                found: "number"
            }
        );
    }

    #[test]
    fn to_str_renders_any_value_unquoted() {
        assert_eq!(run("3 to_str").stack(), &[Value::from("3")]);
        assert_eq!(run("true to_str").stack(), &[Value::from("true")]);
        // Idempotent on a string.
        assert_eq!(run(r#""hi" to_str"#).stack(), &[Value::from("hi")]);
        // The doc's computed-name shape: build "x1" from a string and a number.
        assert_eq!(run(r#""x" 1 to_str +"#).stack(), &[Value::from("x1")]);
    }

    #[test]
    fn strings_compare_by_content() {
        assert_eq!(run(r#""a" "a" ="#).stack(), &[true]);
        assert_eq!(run(r#""a" "b" ="#).stack(), &[false]);
        // A string never equals a number, even a look-alike.
        assert_eq!(run(r#"1 "1" ="#).stack(), &[false]);
    }

    #[test]
    fn strings_display_quoted_on_the_stack() {
        // Display quotes and escapes, so a string is visibly not a number;
        // `to_str` / `content_string` give the bare content.
        assert_eq!(Value::from("hi").to_string(), r#""hi""#);
        assert_eq!(Value::from("a\nb").to_string(), r#""a\nb""#);
    }

    #[test]
    fn neg_flips_top() {
        assert_eq!(run("5 neg").stack(), &[-5.0]);
        assert_eq!(run("5 neg neg").stack(), &[5.0]);
    }

    #[test]
    fn divide_by_zero_is_an_error() {
        assert_eq!(
            run("1 0").run_builtin(Builtin::Div).unwrap_err(),
            ErrorKind::DivideByZero
        );
    }

    #[test]
    fn underflow_is_an_error() {
        assert_eq!(
            run("1").run_builtin(Builtin::Add).unwrap_err(),
            ErrorKind::StackUnderflow
        );
    }

    #[test]
    fn errors_carry_the_trace_of_the_failing_command() {
        // No engine is attached (atomicity is the caller's); the error carries
        // the kind and a trace pointing at the command that failed.
        let err = Engine::new().apply(&parse("1 0 /").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::DivideByZero);
        let trace = err.trace.unwrap();
        assert_eq!(trace.index, 2);
        assert_eq!(trace.program[trace.index], Element::Word(Rc::from("/")));
    }

    #[test]
    fn an_error_leaves_the_callers_engine_untouched() {
        // Family-C atomicity: run against a *copy*; on error the original is
        // intact simply because it was never moved into `apply`.
        let original = run("1");
        assert_eq!(
            original.clone().apply(&parse("+").unwrap()).unwrap_err().kind,
            ErrorKind::StackUnderflow
        );
        assert_eq!(original.stack(), &[1.0]);
    }

    #[test]
    fn apply_error_traces_the_program_and_the_failing_command() {
        // `1 2 + /`: after `+` the stack is [3]; `/` underflows at index 3.
        let err = Engine::new().apply(&parse("1 2 + /").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::StackUnderflow);
        let trace = err.trace.clone().unwrap();
        assert_eq!(trace.index, 3);
        assert_eq!(
            trace.program,
            vec![
                Element::Literal(Value::Int(1)),
                Element::Literal(Value::Int(2)),
                Element::Word(Rc::from("+")),
                Element::Word(Rc::from("/")),
            ]
        );
        // The message shows the whole batch with the failing command bracketed.
        assert_eq!(err.to_string(), "too few arguments in `1 2 + [/]`");
    }

    #[test]
    fn an_unknown_word_is_a_runtime_unbound_error() {
        // Parsing no longer fails on an unknown word — it becomes a `Word`; the
        // failure surfaces at runtime when it can't be resolved.
        assert_eq!(
            parse("1 2 + oops"),
            Ok(vec![
                Element::Literal(Value::Int(1)),
                Element::Literal(Value::Int(2)),
                Element::Word(Rc::from("+")),
                Element::Word(Rc::from("oops")),
            ])
        );
        assert_eq!(run_err("oops"), ErrorKind::UnboundName("oops".to_string()));
    }

    #[test]
    fn parse_produces_a_program() {
        assert_eq!(
            parse("1 2 +"),
            Ok(vec![
                Element::Literal(Value::Int(1)),
                Element::Literal(Value::Int(2)),
                Element::Word(Rc::from("+")),
            ])
        );
    }

    #[test]
    fn dup_drop() {
        assert_eq!(run("3 dup").stack(), &[3.0, 3.0]);
        assert_eq!(run("3 4 drop").stack(), &[3.0]);
    }

    #[test]
    fn swap_over() {
        assert_eq!(run("1 2 swap").stack(), &[2.0, 1.0]);
        assert_eq!(run("1 2 over").stack(), &[1.0, 2.0, 1.0]);
    }

    #[test]
    fn rot_brings_third_to_top() {
        assert_eq!(run("1 2 3 rot").stack(), &[2.0, 3.0, 1.0]);
    }

    #[test]
    fn clear_empties_the_stack() {
        assert!(run("1 2 3 clear").stack().is_empty());
    }

    #[test]
    fn parse_maps_tokens_to_commands() {
        // Numbers and `'x` names become `Push`; every other token is a `Word`,
        // resolved at runtime (so `+`/`dup` and an unknown `nope` are alike).
        assert_eq!(Element::parse("3.5"), Element::Literal(Value::Num(3.5)));
        assert_eq!(Element::parse("+"), Element::Word(Rc::from("+")));
        assert_eq!(Element::parse("dup"), Element::Word(Rc::from("dup")));
        assert_eq!(Element::parse("nope"), Element::Word(Rc::from("nope")));
    }

    #[test]
    fn apply_runs_a_batch_of_commands() {
        // The TUI path: push literal elements, then run an operator directly on
        // the engine (as the operator keys do) rather than as a program word.
        let mut engine = Engine::new()
            .apply(&[
                Element::Literal(Value::Num(2.0)),
                Element::Literal(Value::Num(3.0)),
            ])
            .unwrap();
        engine.run_builtin(Builtin::Mul).unwrap();
        assert_eq!(engine.stack(), &[6.0]);
    }

    // --- M1: fixed shuffles and stack-consuming indexed ops ---

    #[test]
    fn fixed_shuffles() {
        assert_eq!(run("1 2 over").stack(), &[1.0, 2.0, 1.0]);
        assert_eq!(run("1 2 3 rot").stack(), &[2.0, 3.0, 1.0]);
        assert_eq!(run("1 2 3 unrot").stack(), &[3.0, 1.0, 2.0]);
        assert_eq!(run("1 2 nip").stack(), &[2.0]);
        assert_eq!(run("1 2 tuck").stack(), &[2.0, 1.0, 2.0]);
        assert_eq!(run("1 2 dupd").stack(), &[1.0, 1.0, 2.0]);
        assert_eq!(run("1 2 2dup").stack(), &[1.0, 2.0, 1.0, 2.0]);
        assert_eq!(run("1 2 3 2drop").stack(), &[1.0]);
    }

    #[test]
    fn unrot_is_rot_inverted() {
        assert_eq!(run("1 2 3 rot unrot").stack(), &[1.0, 2.0, 3.0]);
    }

    #[test]
    fn indexed_words_take_their_level_off_the_stack() {
        // `3 pickn` copies level 3 to the top (the level itself is consumed).
        assert_eq!(run("1 2 3 3 pickn").stack(), &[1.0, 2.0, 3.0, 1.0]);
        assert_eq!(run("1 2 3 3 rolln").stack(), &[2.0, 3.0, 1.0]);
        assert_eq!(run("1 2 3 3 rolldn").stack(), &[3.0, 1.0, 2.0]);
        assert_eq!(run("1 2 3 2 dropn").stack(), &[1.0, 3.0]);
        assert_eq!(run("1 2 3 2 swapn").stack(), &[2.0, 1.0, 3.0]);
    }

    #[test]
    fn a_swapn_at_the_bottom_has_nothing_below() {
        assert_eq!(run_err("1 2 3 3 swapn"), ErrorKind::StackUnderflow);
    }

    #[test]
    fn rolldn_inverts_rolln() {
        assert_eq!(
            run("1 2 3 4 3 rolln 3 rolldn").stack(),
            &[1.0, 2.0, 3.0, 4.0]
        );
    }

    #[test]
    fn a_non_integer_level_is_rejected_not_rounded() {
        assert_eq!(
            run_err("1 2 3 2.5 rolln"),
            ErrorKind::TypeError {
                expected: "integer",
                found: "float"
            }
        );
    }

    #[test]
    fn a_level_out_of_range_underflows() {
        assert_eq!(run_err("1 2 5 pickn"), ErrorKind::StackUnderflow);
        // A level of 0 is not a valid 1-based level either.
        assert_eq!(run_err("1 2 0 rolln"), ErrorKind::StackUnderflow);
    }

    #[test]
    fn indexed_words_render_n_suffixed() {
        assert_eq!(Builtin::PickN.to_string(), "pickn");
        assert_eq!(Builtin::RollN.to_string(), "rolln");
        assert_eq!(Builtin::RolldN.to_string(), "rolldn");
        assert_eq!(Builtin::DropN.to_string(), "dropn");
        assert_eq!(Builtin::SwapN.to_string(), "swapn");
    }

    // --- M2: lists and the mark discipline ---

    /// Shorthand for a list value from a slice of values.
    fn list(items: &[Value]) -> Value {
        Value::List(Rc::new(items.to_vec()))
    }

    #[test]
    fn brackets_collect_a_list() {
        assert_eq!(run("[ ]").stack(), &[list(&[])]);
        assert_eq!(
            run("[ 1 2 3 ]").stack(),
            &[list(&[Value::Int(1), Value::Int(2), Value::Int(3)])]
        );
    }

    #[test]
    fn lists_are_heterogeneous() {
        assert_eq!(
            run(r#"[ 1 true "x" ]"#).stack(),
            &[list(&[Value::Int(1), Value::Bool(true), Value::from("x")])]
        );
    }

    #[test]
    fn lists_nest() {
        assert_eq!(
            run("[ 1 [ 2 3 ] 4 ]").stack(),
            &[list(&[
                Value::Int(1),
                list(&[Value::Int(2), Value::Int(3)]),
                Value::Int(4),
            ])]
        );
    }

    #[test]
    fn words_run_while_collecting() {
        // The `+` fires inside the collection, so its result is an element.
        assert_eq!(
            run("[ 1 2 + 3 ]").stack(),
            &[list(&[Value::Int(3), Value::Int(3)])]
        );
        // Shuffles, too — they operate within the region.
        assert_eq!(
            run("[ 1 2 swap ]").stack(),
            &[list(&[Value::Int(2), Value::Int(1)])]
        );
    }

    #[test]
    fn a_mark_is_a_typed_literal_not_a_floor() {
        // A value word rejects the mark as an operand — `[ 1 +` is a type error.
        assert_eq!(
            run_err("[ 1 +"),
            ErrorKind::TypeError {
                expected: "number",
                found: "open list"
            }
        );
        // Reaching the mark from an outer value type-errors the same way.
        assert_eq!(
            run_err("1 [ 2 +"),
            ErrorKind::TypeError {
                expected: "number",
                found: "open list"
            }
        );
        // Shuffles, though, move and copy the mark like any other value, so a
        // collection is not a sealed scope.
        assert_eq!(
            run("[ dup").stack(),
            &[Value::Mark(MarkKind::List), Value::Mark(MarkKind::List)]
        );
        // An under-supplied shuffle therefore reshapes rather than erroring:
        // `rot` lifts the mark to the top, so `]` closes an empty list.
        assert_eq!(
            run("[ 1 2 rot ]").stack(),
            &[Value::Int(1), Value::Int(2), list(&[])]
        );
    }

    #[test]
    fn an_open_collection_persists_on_the_stack() {
        // Leaving `[` unclosed is legal — the mark stays, ready for a later `]`.
        assert_eq!(
            run("[ 1 2").stack(),
            &[Value::Mark(MarkKind::List), Value::Int(1), Value::Int(2)]
        );
        // A `]` in a later batch closes it.
        assert_eq!(
            run("[ 1 2").apply(&parse("]").unwrap()).unwrap().stack(),
            &[list(&[Value::Int(1), Value::Int(2)])]
        );
    }

    #[test]
    fn an_unmatched_close_is_an_error() {
        assert_eq!(run_err("]"), ErrorKind::UnmatchedClose);
        assert_eq!(run_err("1 2 ]"), ErrorKind::UnmatchedClose);
    }

    #[test]
    fn a_list_is_an_ordinary_value() {
        // It shuffles as one unit.
        assert_eq!(
            run("[ 1 2 ] dup").stack(),
            &[list(&[Value::Int(1), Value::Int(2)]), list(&[Value::Int(1), Value::Int(2)])]
        );
    }

    #[test]
    fn lists_compare_by_structure() {
        assert_eq!(run("[ 1 2 ] [ 1 2 ] =").stack(), &[true]);
        assert_eq!(run("[ 1 2 ] [ 1 3 ] =").stack(), &[false]);
    }

    #[test]
    fn length_counts_list_elements() {
        assert_eq!(run("[ 1 2 3 ] length").stack(), &[Value::Int(3)]);
        assert_eq!(run("[ ] length").stack(), &[Value::Int(0)]);
    }

    #[test]
    fn to_str_of_a_list_is_its_display() {
        assert_eq!(run("[ 1 2 ] to_str").stack(), &[Value::from("[ 1 2 ]")]);
    }

    #[test]
    fn lists_display_space_padded() {
        assert_eq!(list(&[]).to_string(), "[ ]");
        assert_eq!(list(&[Value::Int(1), Value::Int(2)]).to_string(), "[ 1 2 ]");
        assert_eq!(
            list(&[Value::Int(1), list(&[Value::Int(2)])]).to_string(),
            "[ 1 [ 2 ] ]"
        );
        // A string element keeps its quotes inside a list.
        assert_eq!(list(&[Value::from("a")]).to_string(), r#"[ "a" ]"#);
    }

    // --- List operations ---

    #[test]
    fn first_and_rest_split_the_head() {
        assert_eq!(run("[ 1 2 3 ] first").stack(), &[Value::Int(1)]);
        assert_eq!(
            run("[ 1 2 3 ] rest").stack(),
            &[list(&[Value::Int(2), Value::Int(3)])]
        );
        // rest of a singleton is the empty list.
        assert_eq!(run("[ 1 ] rest").stack(), &[list(&[])]);
    }

    #[test]
    fn first_and_rest_reject_an_empty_list() {
        assert_eq!(run_err("[ ] first"), ErrorKind::IndexOutOfRange);
        assert_eq!(run_err("[ ] rest"), ErrorKind::IndexOutOfRange);
    }

    #[test]
    fn cons_prepends_an_element() {
        assert_eq!(
            run("1 [ 2 3 ] cons").stack(),
            &[list(&[Value::Int(1), Value::Int(2), Value::Int(3)])]
        );
        // Any value conses, onto the empty list too.
        assert_eq!(
            run(r#""x" [ ] cons"#).stack(),
            &[list(&[Value::from("x")])]
        );
    }

    #[test]
    fn append_concatenates_two_lists() {
        assert_eq!(
            run("[ 1 2 ] [ 3 4 ] append").stack(),
            &[list(&[Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)])]
        );
        assert_eq!(run("[ ] [ ] append").stack(), &[list(&[])]);
    }

    #[test]
    fn nth_indexes_zero_based() {
        assert_eq!(run("[ 10 20 30 ] 0 nth").stack(), &[Value::Int(10)]);
        assert_eq!(run("[ 10 20 30 ] 2 nth").stack(), &[Value::Int(30)]);
        assert_eq!(run_err("[ 10 20 30 ] 3 nth"), ErrorKind::IndexOutOfRange);
        // A negative index is out of range, not wrapped.
        assert_eq!(run_err("[ 10 20 ] -1 nth"), ErrorKind::IndexOutOfRange);
    }

    #[test]
    fn list_ops_reject_non_lists() {
        assert_eq!(
            run_err("1 first"),
            ErrorKind::TypeError {
                expected: "list",
                found: "number"
            }
        );
        assert_eq!(
            run_err("1 2 append"),
            ErrorKind::TypeError {
                expected: "list",
                found: "number"
            }
        );
    }

    #[test]
    fn list_ops_compose() {
        // cons then first round-trips the head.
        assert_eq!(run("9 [ 1 2 ] cons first").stack(), &[Value::Int(9)]);
        // build up with append, read back with nth.
        assert_eq!(
            run("[ 1 ] [ 2 3 ] append 1 nth").stack(),
            &[Value::Int(2)]
        );
    }

    #[test]
    fn mutating_a_shared_value_copies_on_write() {
        // `dup` shares the underlying `Rc`; a mutating op (`make_mut`) must copy
        // so the other holder is untouched — the immutability guarantee.
        let one_two_three =
            list(&[Value::Int(1), Value::Int(2), Value::Int(3)]);

        // List `rest` on the shared top leaves the bottom copy intact.
        assert_eq!(
            run("[ 1 2 3 ] dup rest").stack(),
            &[
                one_two_three.clone(),
                list(&[Value::Int(2), Value::Int(3)])
            ]
        );
        // List `append` onto the shared top.
        assert_eq!(
            run("[ 1 2 3 ] dup [ 4 ] append").stack(),
            &[
                one_two_three,
                list(&[Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)])
            ]
        );
        // String concat onto the shared top.
        assert_eq!(
            run(r#""ab" dup "c" +"#).stack(),
            &[Value::from("ab"), Value::from("abc")]
        );
    }

    // --- M3b: the environment ---

    /// A name value from text.
    fn name(s: &str) -> Value {
        Value::Name(Rc::from(s))
    }

    #[test]
    fn quote_pushes_a_name() {
        assert_eq!(run("'x").stack(), &[name("x")]);
        // Names print with the quote so they're distinct from a look-alike.
        assert_eq!(name("1").to_string(), "'1");
        assert_ne!(name("1").to_string(), Value::Int(1).to_string());
    }

    #[test]
    fn set_then_get_round_trips() {
        assert_eq!(run("3 'x set 'x get").stack(), &[Value::Int(3)]);
        // Any value binds — a list too.
        assert_eq!(
            run("[ 1 2 ] 'xs set 'xs get").stack(),
            &[list(&[Value::Int(1), Value::Int(2)])]
        );
    }

    #[test]
    fn set_shadows_the_prior_binding() {
        assert_eq!(run("1 'x set 2 'x set 'x get").stack(), &[Value::Int(2)]);
    }

    #[test]
    fn get_on_an_unbound_name_fails() {
        assert_eq!(run_err("'y get"), ErrorKind::UnboundName("y".to_string()));
    }

    #[test]
    fn set_and_get_want_a_name() {
        // `set`'s name operand is on top; a number there is a type error.
        assert_eq!(
            run_err("3 4 set"),
            ErrorKind::TypeError {
                expected: "name",
                found: "number"
            }
        );
        assert_eq!(
            run_err("3 get"),
            ErrorKind::TypeError {
                expected: "name",
                found: "number"
            }
        );
    }

    #[test]
    fn names_compare_by_text() {
        assert_eq!(run("'x 'x =").stack(), &[true]);
        assert_eq!(run("'x 'y =").stack(), &[false]);
    }

    #[test]
    fn to_str_of_a_name_is_its_bare_text() {
        assert_eq!(run("'x to_str").stack(), &[Value::from("x")]);
    }

    #[test]
    fn a_bound_value_shares_but_get_plus_mutation_copies_on_write() {
        // `foo` holds a list; `get` shares it (Rc bump). Mutating the retrieved
        // copy must not corrupt the binding — the durable-alias case that made
        // us pick Rc + copy-on-write.
        assert_eq!(
            run("[ 1 2 3 ] 'foo set 'foo get rest 'foo get").stack(),
            &[
                list(&[Value::Int(2), Value::Int(3)]),
                list(&[Value::Int(1), Value::Int(2), Value::Int(3)]),
            ]
        );
    }

    // --- bare-word lookup ---

    #[test]
    fn a_bare_word_pushes_its_binding() {
        assert_eq!(run("3 'x set x").stack(), &[Value::Int(3)]);
        // A bare word and `get` retrieve the same value.
        assert_eq!(run("3 'x set x").stack(), run("3 'x set 'x get").stack());
    }

    #[test]
    fn a_user_binding_shadows_a_builtin() {
        // Rebinding `dup` makes the bare word push the binding, not duplicate —
        // user bindings sit "before" the builtin prelude in resolution.
        assert_eq!(run("5 'dup set 1 2 dup").stack(), &[1.0, 2.0, 5.0]);
    }

    #[test]
    fn builtins_are_reached_by_the_same_lookup() {
        // `+` is a word resolved to a prelude binding — no special parse case;
        // `get` reaches it through the same lookup as any user binding.
        assert_eq!(run("'+ get").stack(), &[Value::Builtin(Builtin::Add)]);
        assert_eq!(run_err("nope"), ErrorKind::UnboundName("nope".to_string()));
    }

    #[test]
    fn a_captured_builtin_runs_when_applied() {
        // `get` captures the op as a value; binding it to a name and applying
        // that name runs it — first-class words end to end.
        assert_eq!(run("3 4 '+ get 'plus set plus").stack(), &[7.0]);
    }

    #[test]
    fn every_builtin_is_in_the_prelude() {
        // `Builtin::ALL` isn't exhaustiveness-checked; this guards that the
        // prelude binds every op under its canonical word.
        let base = prelude();
        for &b in Builtin::ALL {
            let name = b.to_string();
            assert_eq!(
                base.get(name.as_str()),
                Some(&Value::Builtin(b)),
                "prelude missing `{name}`",
            );
        }
    }
}
