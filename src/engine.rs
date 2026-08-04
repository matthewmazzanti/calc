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
    /// A token that was neither a number nor a known command.
    UnknownCommand(String),
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
            ErrorKind::UnknownCommand(c) => write!(f, "unknown command: {c}"),
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

/// A single parsed instruction. Parsing turns text into these; evaluation
/// consumes them. `Push` carries its literal, so the whole program is a flat
/// stream of `Command`s — no AST, because RPN has no nesting.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Push(Value),
    Add,
    Sub,
    Mul,
    Div,
    Neg,
    /// Equality — pops two values, pushes a `Bool`. Works across types (a
    /// number never equals a bool); inequality is `=` then `not`.
    Eq,
    /// Ordering comparisons — pop two numbers, push a `Bool`.
    Lt,
    Gt,
    Le,
    Ge,
    /// Boolean words — operate on `Bool`s only (no truthiness rule).
    Not,
    And,
    Or,
    /// The character count of a string, pushed as an `Int`.
    Length,
    /// Convert the top value to its string content (no quotes).
    ToStr,

    // Fixed shuffles — fixed arity, no level. The ergonomic core.
    Dup,    // a -- a a
    Drop,   // a --
    Swap,   // a b -- b a
    Over,   // a b -- a b a
    Rot,    // a b c -- b c a
    Unrot,  // a b c -- c a b   (Factor's -rot)
    Nip,    // a b -- b
    Tuck,   // a b -- b a b
    Dupd,   // a b -- a a b
    TwoDup,  // a b -- a b a b   (2dup)
    TwoDrop, // a b --           (2drop)

    // Indexed ops: the 1-based level (level 1 == top) is popped off the stack.
    // This is the in-language surface — `n rolln`, `n pickn`. Uniformly
    // N-suffixed to mark that they consume a level argument. The TUI cursor does
    // not go through these; it calls the engine's `*_at` methods directly (so a
    // stack edit is never intercepted by collection mode).
    /// Copy the value at the popped level to the top.
    PickN,
    /// Move the value at the popped level up to the top.
    RollN,
    /// Roll the top down to the popped level (Unrot's general form).
    RolldN,
    /// Drop the value at the popped level.
    DropN,
    /// Swap the value at the popped level with the one just below it.
    SwapN,

    /// `[` — push a list mark, opening a collection.
    OpenList,
    /// `]` — collect the values above the topmost mark into a `List`.
    CloseList,

    // List operations.
    First,  // [a b c] -- a
    Rest,   // [a b c] -- [b c]
    Cons,   // x [b c] -- [x b c]
    Append, // [a b] [c d] -- [a b c d]
    Nth,    // [a b c] n -- (the 0-based nth element)

    // Environment.
    Set, // value name -- (bind name to value)
    Get, // name -- value (look up name)

    Clear,
}

impl Command {
    /// Parse one whitespace-delimited token into a `Command`.
    ///
    /// A token that parses as a number becomes `Push`; otherwise it must be a
    /// known command word. This is the only place text becomes a `Command`.
    pub fn parse(token: &str) -> Result<Command, ErrorKind> {
        // The `'` sigil: `'x` pushes the name `x` (§3). Owned here rather than
        // as a builtin word so it can't be shadowed.
        if let Some(name) = token.strip_prefix('\'') {
            return Ok(Command::Push(Value::Name(Rc::from(name))));
        }
        // Integer first, then float: `3` is an `Int`, but `3.0`/`2e3`/`1e-2`
        // (anything with a `.`, exponent, or out of i64 range) is a `Num`.
        if let Ok(i) = token.parse::<i64>() {
            return Ok(Command::Push(Value::Int(i)));
        }
        if let Ok(n) = token.parse::<f64>() {
            return Ok(Command::Push(Value::Num(n)));
        }
        Ok(match token {
            "true" => Command::Push(Value::Bool(true)),
            "false" => Command::Push(Value::Bool(false)),
            "+" => Command::Add,
            "-" => Command::Sub,
            "*" => Command::Mul,
            "/" => Command::Div,
            "neg" => Command::Neg,
            "=" => Command::Eq,
            "<" => Command::Lt,
            ">" => Command::Gt,
            "<=" => Command::Le,
            ">=" => Command::Ge,
            "not" => Command::Not,
            "and" => Command::And,
            "or" => Command::Or,
            "length" => Command::Length,
            "to_str" => Command::ToStr,
            // Fixed shuffles.
            "dup" => Command::Dup,
            "drop" => Command::Drop,
            "swap" => Command::Swap,
            "over" => Command::Over,
            "rot" => Command::Rot,
            "unrot" => Command::Unrot,
            "nip" => Command::Nip,
            "tuck" => Command::Tuck,
            "dupd" => Command::Dupd,
            "2dup" => Command::TwoDup,
            "2drop" => Command::TwoDrop,
            // Indexed ops — the level comes off the stack (uniform N-suffix).
            "pickn" => Command::PickN,
            "rolln" => Command::RollN,
            "rolldn" => Command::RolldN,
            "dropn" => Command::DropN,
            "swapn" => Command::SwapN,
            // Lists — `[` and `]` are ordinary words (spaces required).
            "[" => Command::OpenList,
            "]" => Command::CloseList,
            "first" => Command::First,
            "rest" => Command::Rest,
            "cons" => Command::Cons,
            "append" => Command::Append,
            "nth" => Command::Nth,
            "set" => Command::Set,
            "get" => Command::Get,
            "clear" => Command::Clear,
            other => return Err(ErrorKind::UnknownCommand(other.to_string())),
        })
    }
}

impl std::fmt::Display for Command {
    /// The canonical token, so errors can name the command that failed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::Push(n) => write!(f, "{n}"),
            Command::Add => write!(f, "+"),
            Command::Sub => write!(f, "-"),
            Command::Mul => write!(f, "*"),
            Command::Div => write!(f, "/"),
            Command::Neg => write!(f, "neg"),
            Command::Eq => write!(f, "="),
            Command::Lt => write!(f, "<"),
            Command::Gt => write!(f, ">"),
            Command::Le => write!(f, "<="),
            Command::Ge => write!(f, ">="),
            Command::Not => write!(f, "not"),
            Command::And => write!(f, "and"),
            Command::Or => write!(f, "or"),
            Command::Length => write!(f, "length"),
            Command::ToStr => write!(f, "to_str"),
            Command::Dup => write!(f, "dup"),
            Command::Drop => write!(f, "drop"),
            Command::Swap => write!(f, "swap"),
            Command::Over => write!(f, "over"),
            Command::Rot => write!(f, "rot"),
            Command::Unrot => write!(f, "unrot"),
            Command::Nip => write!(f, "nip"),
            Command::Tuck => write!(f, "tuck"),
            Command::Dupd => write!(f, "dupd"),
            Command::TwoDup => write!(f, "2dup"),
            Command::TwoDrop => write!(f, "2drop"),
            Command::PickN => write!(f, "pickn"),
            Command::RollN => write!(f, "rolln"),
            Command::RolldN => write!(f, "rolldn"),
            Command::DropN => write!(f, "dropn"),
            Command::SwapN => write!(f, "swapn"),
            Command::OpenList => write!(f, "["),
            Command::CloseList => write!(f, "]"),
            Command::First => write!(f, "first"),
            Command::Rest => write!(f, "rest"),
            Command::Cons => write!(f, "cons"),
            Command::Append => write!(f, "append"),
            Command::Nth => write!(f, "nth"),
            Command::Set => write!(f, "set"),
            Command::Get => write!(f, "get"),
            Command::Clear => write!(f, "clear"),
        }
    }
}

/// Parse a line into a program, failing on the first unknown token (its text is
/// carried in the [`ErrorKind`]) or an unterminated string. Parsing is a
/// frontend concern — the engine itself only runs programs, via
/// [`Engine::apply`].
///
/// Mostly a whitespace split, but with the §4 lookahead: a `"` opens a string
/// literal that runs (across spaces) to its closing `"`, so strings are the one
/// thing [`Command::parse`] never sees — the tokenizer owns them. Every other
/// token is handed to [`Command::parse`] word-for-word.
pub fn parse(input: &str) -> Result<Vec<Command>, ErrorKind> {
    let mut commands = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' {
            commands.push(Command::Push(Value::Str(Rc::new(read_string(&mut chars)?))));
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
            commands.push(Command::parse(&word)?);
        }
    }
    Ok(commands)
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

/// The command sequence that was executing when an error struck, and the index
/// of the command that failed — "here's what was running."
#[derive(Debug, Clone, PartialEq)]
pub struct Trace {
    /// The whole batch of commands being applied.
    pub program: Vec<Command>,
    /// The 0-based index within `program` of the command that failed.
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
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Engine {
    stack: Stack,
    /// The environment: names to bound values. A single flat frame for now (the
    /// REPL/module scope); the frame chain arrives with functions.
    env: std::collections::HashMap<Rc<str>, Value>,
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
    pub fn apply(mut self, program: &[Command]) -> Outcome {
        for (index, command) in program.iter().enumerate() {
            if let Err(kind) = self.apply_one(command) {
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

    /// Apply a single command in place. A total match, so a new command variant
    /// is a compile error until handled.
    fn apply_one(&mut self, cmd: &Command) -> Result<(), ErrorKind> {
        match cmd {
            Command::Push(value) => {
                self.stack.push(value.clone());
                Ok(())
            }
            // `+` concatenates two strings, else adds numbers.
            Command::Add => self.add(),
            Command::Sub => self.arith(i64::checked_sub, |a, b| a - b),
            Command::Mul => self.arith(i64::checked_mul, |a, b| a * b),
            // Division always yields a float — `1 2 /` is `0.5`, not `0`.
            Command::Div => self.num_binary(|a, b| {
                if b == 0.0 {
                    Err(ErrorKind::DivideByZero)
                } else {
                    Ok(a / b)
                }
            }),
            Command::Neg => self.negate(),
            Command::Eq => self.equality(),
            Command::Lt => self.num_compare(|a, b| a < b),
            Command::Gt => self.num_compare(|a, b| a > b),
            Command::Le => self.num_compare(|a, b| a <= b),
            Command::Ge => self.num_compare(|a, b| a >= b),
            Command::Not => self.bool_unary(|a| !a),
            Command::And => self.bool_binary(|a, b| a && b),
            Command::Or => self.bool_binary(|a, b| a || b),
            Command::Length => self.length(),
            Command::ToStr => self.stringify(),
            // Fixed shuffles — several are just a fixed level of an indexed op.
            Command::Dup => self.pick_at(1),
            Command::Over => self.pick_at(2),
            Command::Rot => self.roll_at(3),
            Command::Unrot => self.rolld_at(3),
            Command::Drop => self.drop_at(1),
            Command::Nip => self.drop_at(2),
            Command::Swap => self.swap_at(1),
            Command::Tuck => self.tuck(),
            Command::Dupd => self.dupd(),
            Command::TwoDup => self.two_dup(),
            Command::TwoDrop => self.two_drop(),
            // Indexed: pop the level, then run the op.
            Command::PickN => self.indexed(Engine::pick_at),
            Command::RollN => self.indexed(Engine::roll_at),
            Command::RolldN => self.indexed(Engine::rolld_at),
            Command::DropN => self.indexed(Engine::drop_at),
            Command::SwapN => self.indexed(Engine::swap_at),
            Command::OpenList => {
                self.stack.push(Value::Mark(MarkKind::List));
                Ok(())
            }
            Command::CloseList => self.close_list(),
            Command::First => self.first(),
            Command::Rest => self.rest(),
            Command::Cons => self.cons(),
            Command::Append => self.append(),
            Command::Nth => self.nth(),
            Command::Set => self.set(),
            Command::Get => self.get(),
            Command::Clear => {
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

    /// `set` ( value name -- ): bind `name` to `value` in the environment,
    /// shadowing any prior binding. The name is on top (`3 'x set`).
    fn set(&mut self) -> Result<(), ErrorKind> {
        let name = self.pop_name()?;
        let value = self.pop()?;
        self.env.insert(name, value);
        Ok(())
    }

    /// `get` ( name -- value ): push the value bound to `name`, or fail with
    /// `UnboundName`. The value is cloned out (an `Rc` bump), leaving the
    /// binding in place; a later mutation copies-on-write.
    fn get(&mut self) -> Result<(), ErrorKind> {
        let name = self.pop_name()?;
        let value = self
            .env
            .get(&name)
            .cloned()
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
        assert_eq!(trace.program[trace.index], Command::Add);
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
            run("1 0").apply(&[Command::Div]).unwrap_err().kind,
            ErrorKind::DivideByZero
        );
    }

    #[test]
    fn underflow_is_an_error() {
        assert_eq!(
            run("1").apply(&[Command::Add]).unwrap_err().kind,
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
        assert_eq!(trace.program[trace.index], Command::Div);
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
                Command::Push(Value::Int(1)),
                Command::Push(Value::Int(2)),
                Command::Add,
                Command::Div
            ]
        );
        // The message shows the whole batch with the failing command bracketed.
        assert_eq!(err.to_string(), "too few arguments in `1 2 + [/]`");
    }

    #[test]
    fn parse_fails_on_the_bad_token() {
        // Parsing is separate from running: a bad token is an `ErrorKind`, with
        // no engine or trace (nothing ran).
        assert_eq!(
            parse("1 2 + oops"),
            Err(ErrorKind::UnknownCommand("oops".to_string()))
        );
    }

    #[test]
    fn parse_produces_a_program() {
        assert_eq!(
            parse("1 2 +"),
            Ok(vec![
                Command::Push(Value::Int(1)),
                Command::Push(Value::Int(2)),
                Command::Add
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
        assert_eq!(Command::parse("3.5"), Ok(Command::Push(Value::Num(3.5))));
        assert_eq!(Command::parse("+"), Ok(Command::Add));
        assert_eq!(Command::parse("dup"), Ok(Command::Dup));
        assert_eq!(
            Command::parse("nope"),
            Err(ErrorKind::UnknownCommand("nope".to_string()))
        );
    }

    #[test]
    fn apply_runs_a_batch_of_commands() {
        // The TUI path: hand the engine Commands without going through text.
        let engine = Engine::new()
            .apply(&[
                Command::Push(Value::Num(2.0)),
                Command::Push(Value::Num(3.0)),
                Command::Mul,
            ])
            .unwrap();
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
        assert_eq!(Command::PickN.to_string(), "pickn");
        assert_eq!(Command::RollN.to_string(), "rolln");
        assert_eq!(Command::RolldN.to_string(), "rolldn");
        assert_eq!(Command::DropN.to_string(), "dropn");
        assert_eq!(Command::SwapN.to_string(), "swapn");
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
}
