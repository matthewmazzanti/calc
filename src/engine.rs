//! The RPN calculator engine: an [`Engine`] wrapping a stack of floating-point
//! values (and, later, evaluation settings). No I/O and no history — its
//! transforms *consume* `self` and return the new engine, so a batch of
//! commands folds through with no intermediate copies: the same value is moved
//! from step to step. [`Engine::apply`] runs a whole slice of commands; turning
//! a line of text into one is a frontend concern, handled by [`parse`].
//!
//! On failure a transform moves `self` *into* the error rather than dropping it:
//! [`CalcError`] carries the engine as it stood when the command failed — its
//! stack is exactly the state the command saw, since ops check before mutating —
//! so callers can inspect it. The caller keeps its own copy for undo (see the
//! `history` module) and commits the returned engine only on `Ok`.

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
    /// A string. Owns its bytes, so `Value` is no longer `Copy` — the stack ops
    /// clone or move rather than copy. Built by the tokenizer's `"…"` literals
    /// and by `to_str`; concatenated with `+`.
    Str(String),
    /// A list — a growable, heterogeneous, ordinary sequence. Built by the
    /// `[ … ]` words via the mark discipline, never a `Push` literal.
    List(Vec<Value>),
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
            Value::Str(s) => s.clone(),
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
                for item in items {
                    write!(f, " {item}")?;
                }
                write!(f, " ]")
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
        Value::Str(s)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.to_string())
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

    // Indexed ops, stack-consuming form: the 1-based level (level 1 == top) is
    // popped off the stack. This is the text/RPN surface — `n roll`, `n pick`.
    /// Copy the value at the popped level to the top.
    Pick,
    /// Move the value at the popped level up to the top.
    Roll,
    /// Roll the top down to the popped level (Rot's inverse at level 3).
    Rolld,
    /// Drop the value at the popped level.
    DropN,
    /// Swap the value at the popped level with the one just below it.
    SwapN,

    // Indexed ops, parameterized form: the level is baked into the instruction.
    // Not produced by the tokenizer (a flat word can't carry an argument) — the
    // TUI cursor emits these directly. They render as `<word> <level>`. Only the
    // four the cursor actually emits exist; `rolld` has no cursor key, so there
    // is no `RolldAt` (add it with its emitter).
    PickAt(usize),
    RollAt(usize),
    DropAt(usize),
    SwapAt(usize),

    /// `[` — push a list mark, opening a collection.
    OpenList,
    /// `]` — collect the values above the topmost mark into a `List`.
    CloseList,

    Clear,
}

impl Command {
    /// Parse one whitespace-delimited token into a `Command`.
    ///
    /// A token that parses as a number becomes `Push`; otherwise it must be a
    /// known command word. This is the only place text becomes a `Command`.
    pub fn parse(token: &str) -> Result<Command, ErrorKind> {
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
            // Indexed ops — the level comes off the stack.
            "pick" => Command::Pick,
            "roll" => Command::Roll,
            "rolld" => Command::Rolld,
            "dropn" => Command::DropN,
            "swapn" => Command::SwapN,
            // Lists — `[` and `]` are ordinary words (spaces required).
            "[" => Command::OpenList,
            "]" => Command::CloseList,
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
            Command::Pick => write!(f, "pick"),
            Command::Roll => write!(f, "roll"),
            Command::Rolld => write!(f, "rolld"),
            Command::DropN => write!(f, "dropn"),
            Command::SwapN => write!(f, "swapn"),
            // Parameterized (TUI-emitted): the word plus the baked level, but
            // render the level that names a fixed shuffle as that word — so a
            // cursor op on the top of stack reads `drop`, not `dropn 1`.
            Command::PickAt(1) => write!(f, "dup"),
            Command::PickAt(l) => write!(f, "pick {l}"),
            Command::RollAt(3) => write!(f, "rot"),
            Command::RollAt(l) => write!(f, "roll {l}"),
            Command::DropAt(1) => write!(f, "drop"),
            Command::DropAt(l) => write!(f, "dropn {l}"),
            Command::SwapAt(1) => write!(f, "swap"),
            Command::SwapAt(l) => write!(f, "swapn {l}"),
            Command::OpenList => write!(f, "["),
            Command::CloseList => write!(f, "]"),
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
            commands.push(Command::Push(Value::Str(read_string(&mut chars)?)));
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

/// A semantic error plus the context to inspect it: the engine as it stood when
/// evaluation failed, and the command sequence it was running.
#[derive(Debug, Clone, PartialEq)]
pub struct CalcError {
    /// What went wrong.
    pub kind: ErrorKind,
    /// The engine at the moment of failure. Since every op checks its
    /// preconditions before mutating, its stack is exactly the state the failing
    /// command saw — nothing partially applied.
    pub engine: Engine,
    /// The command sequence being run and which command failed, for a runtime
    /// error. `None` for parse errors, whose offending token is already named
    /// in `kind`.
    pub trace: Option<Trace>,
}

impl CalcError {
    fn new(engine: Engine, kind: ErrorKind) -> Self {
        Self {
            kind,
            engine,
            trace: None,
        }
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

/// The result of a transform: the new engine, or a [`CalcError`] carrying the
/// engine at the point of failure.
pub type Outcome = Result<Engine, CalcError>;

/// The calculator engine: the RPN stack, plus (later) evaluation settings such
/// as angle mode, display precision, or named registers.
///
/// Transforms consume `self` and return the new engine, so a sequence of
/// commands folds through with no intermediate copies (`self` is moved from
/// step to step). Callers keep their own copy for undo and commit the result
/// only on `Ok` — see the `history` module.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Engine {
    stack: Stack,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// The current stack, bottom-to-top (top of stack is the last element).
    pub fn stack(&self) -> &[Value] {
        &self.stack
    }

    /// Apply a batch of commands in order, threading the engine through each.
    /// The first runtime error short-circuits the fold and carries a [`Trace`]
    /// of the whole batch plus the index that failed — "here's what was running."
    pub fn apply(self, program: &[Command]) -> Outcome {
        program
            .iter()
            .enumerate()
            .try_fold(self, |engine, (index, command)| {
                engine.apply_one(command).map_err(|mut e| {
                    e.trace = Some(Trace {
                        program: program.to_vec(),
                        index,
                    });
                    e
                })
            })
    }

    /// Apply a single command, consuming `self` and returning the new engine.
    /// Borrows the command (it is no longer `Copy`, and the caller still needs
    /// the program for the trace). A total match, so a new command variant is a
    /// compile error until handled.
    fn apply_one(mut self, cmd: &Command) -> Outcome {
        match cmd {
            Command::Push(value) => {
                self.stack.push(value.clone());
                Ok(self)
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
            // Indexed, stack-consuming: pop the level, then run the op.
            Command::Pick => self.indexed(Engine::pick_at),
            Command::Roll => self.indexed(Engine::roll_at),
            Command::Rolld => self.indexed(Engine::rolld_at),
            Command::DropN => self.indexed(Engine::drop_at),
            Command::SwapN => self.indexed(Engine::swap_at),
            // Indexed, parameterized: the level is baked in.
            Command::PickAt(level) => self.pick_at(*level),
            Command::RollAt(level) => self.roll_at(*level),
            Command::DropAt(level) => self.drop_at(*level),
            Command::SwapAt(level) => self.swap_at(*level),
            Command::OpenList => {
                self.stack.push(Value::Mark(MarkKind::List));
                Ok(self)
            }
            Command::CloseList => self.close_list(),
            Command::Clear => {
                self.stack.clear();
                Ok(self)
            }
        }
    }

    // Stack transforms. Each consumes `self`; on failure it moves `self` into
    // the error (via `fail`) instead of dropping it, so the engine is available
    // for inspection. The pure checks return an `ErrorKind`; the one `match`
    // per op is where `self` gets attached.

    /// The failure path of every transform: move `self` into the error and
    /// return it as an `Err`, ready to hand straight back.
    fn fail<T>(self, kind: ErrorKind) -> Result<T, CalcError> {
        Err(CalcError::new(self, kind))
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

    /// Two-operand op whose result is always a float. `a` is the deeper
    /// operand, `b` the top, so `a b <op>` reads left-to-right as `a <op> b`.
    /// Both operands are widened via [`Value::as_num`] (so `Int`s are accepted);
    /// the op may still reject them (e.g. divide-by-zero). This is the path for
    /// `/`; the integer-preserving ops use [`Engine::arith`]. The type/arity
    /// check runs before any mutation, so a failure leaves the stack untouched.
    fn num_binary(
        mut self,
        op: impl FnOnce(f64, f64) -> Result<f64, ErrorKind>,
    ) -> Outcome {
        let n = self.stack.len();
        let result = if n < 2 {
            Err(ErrorKind::StackUnderflow)
        } else {
            self.stack[n - 2]
                .as_num()
                .and_then(|a| self.stack[n - 1].as_num().map(|b| (a, b)))
                .and_then(|(a, b)| op(a, b))
        };
        match result {
            Ok(value) => {
                self.stack.truncate(n - 2);
                self.stack.push(Value::Num(value));
                Ok(self)
            }
            Err(kind) => self.fail(kind),
        }
    }

    /// Integer-preserving binary arithmetic (`+ - *`). Two `Int`s stay an `Int`
    /// via `checked`; if that overflows, or either operand is a float, the op
    /// promotes to `f64` and uses `float`. A bool operand is a `TypeError`.
    fn arith(
        mut self,
        checked: impl FnOnce(i64, i64) -> Option<i64>,
        float: impl FnOnce(f64, f64) -> f64,
    ) -> Outcome {
        let n = self.stack.len();
        let result = if n < 2 {
            Err(ErrorKind::StackUnderflow)
        } else {
            match (&self.stack[n - 2], &self.stack[n - 1]) {
                (Value::Int(a), Value::Int(b)) => Ok(checked(*a, *b)
                    .map(Value::Int)
                    .unwrap_or_else(|| Value::Num(float(*a as f64, *b as f64)))),
                (a, b) => a
                    .as_num()
                    .and_then(|a| b.as_num().map(|b| Value::Num(float(a, b)))),
            }
        };
        match result {
            Ok(value) => {
                self.stack.truncate(n - 2);
                self.stack.push(value);
                Ok(self)
            }
            Err(kind) => self.fail(kind),
        }
    }

    /// Negate the top of stack, preserving `Int` (falling back to a float only
    /// on the `i64::MIN` overflow).
    fn negate(mut self) -> Outcome {
        let n = self.stack.len();
        let result = if n < 1 {
            Err(ErrorKind::StackUnderflow)
        } else {
            match &self.stack[n - 1] {
                Value::Int(i) => Ok(i
                    .checked_neg()
                    .map(Value::Int)
                    .unwrap_or_else(|| Value::Num(-(*i as f64)))),
                Value::Num(x) => Ok(Value::Num(-*x)),
                other => Err(ErrorKind::TypeError {
                    expected: "number",
                    found: other.type_name(),
                }),
            }
        };
        match result {
            Ok(value) => {
                self.stack[n - 1] = value;
                Ok(self)
            }
            Err(kind) => self.fail(kind),
        }
    }

    /// Two-operand numeric comparison, pushing a `Bool` (`< > <= >=`).
    fn num_compare(mut self, op: impl FnOnce(f64, f64) -> bool) -> Outcome {
        let n = self.stack.len();
        let result = if n < 2 {
            Err(ErrorKind::StackUnderflow)
        } else {
            self.stack[n - 2]
                .as_num()
                .and_then(|a| self.stack[n - 1].as_num().map(|b| op(a, b)))
        };
        match result {
            Ok(b) => {
                self.stack.truncate(n - 2);
                self.stack.push(Value::Bool(b));
                Ok(self)
            }
            Err(kind) => self.fail(kind),
        }
    }

    /// Equality of the top two values, pushing a `Bool`. Takes any two values —
    /// no type check. Numbers compare by value across the int/float split, so
    /// `2 2.0 =` is true; a number and a bool simply compare unequal.
    fn equality(mut self) -> Outcome {
        let n = self.stack.len();
        if n < 2 {
            return self.fail(ErrorKind::StackUnderflow);
        }
        let (a, b) = (&self.stack[n - 2], &self.stack[n - 1]);
        // Widen numerics so `Int(2)` equals `Num(2.0)`; anything else (bools,
        // strings, cross-type) falls back to structural equality.
        let eq = match (a.as_num(), b.as_num()) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        };
        self.stack.truncate(n - 2);
        self.stack.push(Value::Bool(eq));
        Ok(self)
    }

    /// Two-operand boolean op (`and`/`or`). Both operands must be `Bool` —
    /// there is no truthiness rule, so a number is a `TypeError`.
    fn bool_binary(mut self, op: impl FnOnce(bool, bool) -> bool) -> Outcome {
        let n = self.stack.len();
        let result = if n < 2 {
            Err(ErrorKind::StackUnderflow)
        } else {
            self.stack[n - 2]
                .as_bool()
                .and_then(|a| self.stack[n - 1].as_bool().map(|b| op(a, b)))
        };
        match result {
            Ok(b) => {
                self.stack.truncate(n - 2);
                self.stack.push(Value::Bool(b));
                Ok(self)
            }
            Err(kind) => self.fail(kind),
        }
    }

    /// One-operand boolean op (`not`), applied to the top of stack.
    fn bool_unary(mut self, op: impl FnOnce(bool) -> bool) -> Outcome {
        let n = self.stack.len();
        let result = if n < 1 {
            Err(ErrorKind::StackUnderflow)
        } else {
            self.stack[n - 1].as_bool().map(op)
        };
        match result {
            Ok(b) => {
                self.stack[n - 1] = Value::Bool(b);
                Ok(self)
            }
            Err(kind) => self.fail(kind),
        }
    }

    /// `+`: concatenate two strings, or add two numbers. Strings only
    /// concatenate with strings — a string and a number is a `TypeError` from
    /// the numeric path, not an implicit `to_str`.
    fn add(mut self) -> Outcome {
        let n = self.stack.len();
        let both_str = n >= 2
            && matches!(self.stack[n - 2], Value::Str(_))
            && matches!(self.stack[n - 1], Value::Str(_));
        if both_str {
            // Pop top first, then reuse the deeper string's allocation.
            let Value::Str(b) = self.stack.pop().unwrap() else {
                unreachable!()
            };
            let Value::Str(mut a) = self.stack.pop().unwrap() else {
                unreachable!()
            };
            a.push_str(&b);
            self.stack.push(Value::Str(a));
            Ok(self)
        } else {
            self.arith(i64::checked_add, |a, b| a + b)
        }
    }

    /// `length`: the element count of the top string (characters) or list,
    /// pushed as an `Int`.
    fn length(mut self) -> Outcome {
        let n = self.stack.len();
        let result = if n < 1 {
            Err(ErrorKind::StackUnderflow)
        } else {
            match &self.stack[n - 1] {
                Value::Str(s) => Ok(s.chars().count() as i64),
                Value::List(items) => Ok(items.len() as i64),
                other => Err(ErrorKind::TypeError {
                    expected: "string or list",
                    found: other.type_name(),
                }),
            }
        };
        match result {
            Ok(len) => {
                self.stack[n - 1] = Value::Int(len);
                Ok(self)
            }
            Err(kind) => self.fail(kind),
        }
    }

    /// `to_str`: replace the top value with its string content (no quotes).
    /// Total — every value has a string form. (Named `stringify`, not `to_str`,
    /// since it consumes `self` as an engine transform rather than borrowing.)
    fn stringify(mut self) -> Outcome {
        let n = self.stack.len();
        if n < 1 {
            return self.fail(ErrorKind::StackUnderflow);
        }
        let s = self.stack[n - 1].content_string();
        self.stack[n - 1] = Value::Str(s);
        Ok(self)
    }

    /// Run an indexed op with its level popped off the stack: check there is a
    /// level, read it as an index, remove it, then delegate. This is the
    /// stack-consuming (`n roll`) surface; the parameterized commands call the
    /// `*_at` helpers directly.
    fn indexed(mut self, op: impl FnOnce(Engine, usize) -> Outcome) -> Outcome {
        let n = self.stack.len();
        if n < 1 {
            return self.fail(ErrorKind::StackUnderflow);
        }
        let level = match self.stack[n - 1].as_index() {
            Ok(level) => level,
            Err(kind) => return self.fail(kind),
        };
        self.stack.pop();
        op(self, level)
    }

    /// Copy the value at `level` to the top (`dup` = 1, `over` = 2, `pick`).
    fn pick_at(mut self, level: usize) -> Outcome {
        let Some(i) = self.index_of_level(level) else {
            return self.fail(ErrorKind::StackUnderflow);
        };
        let v = self.stack[i].clone();
        self.stack.push(v);
        Ok(self)
    }

    /// Remove the value at `level` (`drop` = 1, `nip` = 2, `dropn`).
    fn drop_at(mut self, level: usize) -> Outcome {
        let Some(i) = self.index_of_level(level) else {
            return self.fail(ErrorKind::StackUnderflow);
        };
        self.stack.remove(i);
        Ok(self)
    }

    /// Exchange the value at `level` with the one just below it (`level + 1`).
    /// `swap` = 1, `swapn`.
    fn swap_at(mut self, level: usize) -> Outcome {
        let (Some(i), Some(j)) = (
            self.index_of_level(level),
            self.index_of_level(level + 1),
        )
        else {
            return self.fail(ErrorKind::StackUnderflow);
        };
        self.stack.swap(i, j);
        Ok(self)
    }

    /// Move the value at `level` up to the top, shifting shallower values down.
    /// `rot` = 3, `roll`.
    fn roll_at(mut self, level: usize) -> Outcome {
        let Some(i) = self.index_of_level(level) else {
            return self.fail(ErrorKind::StackUnderflow);
        };
        let v = self.stack.remove(i);
        self.stack.push(v);
        Ok(self)
    }

    /// Move the top value down to `level`, shifting the intervening values up —
    /// the inverse of `roll_at`. `unrot` = 3, `rolld`.
    fn rolld_at(mut self, level: usize) -> Outcome {
        let Some(dest) = self.index_of_level(level) else {
            return self.fail(ErrorKind::StackUnderflow);
        };
        // `dest` is where the top must land. Popping first leaves every index
        // ≤ dest unchanged (dest ≤ len - 1), so we can insert straight in.
        let v = self.stack.pop().expect("level ≥ 1 implies a non-empty stack");
        self.stack.insert(dest, v);
        Ok(self)
    }

    /// `tuck` ( a b -- b a b ): insert a copy of the top below the second.
    fn tuck(mut self) -> Outcome {
        let n = self.stack.len();
        if n < 2 {
            return self.fail(ErrorKind::StackUnderflow);
        }
        let top = self.stack[n - 1].clone();
        self.stack.insert(n - 2, top);
        Ok(self)
    }

    /// `dupd` ( a b -- a a b ): duplicate the second element in place.
    fn dupd(mut self) -> Outcome {
        let n = self.stack.len();
        if n < 2 {
            return self.fail(ErrorKind::StackUnderflow);
        }
        let second = self.stack[n - 2].clone();
        self.stack.insert(n - 1, second);
        Ok(self)
    }

    /// `2dup` ( a b -- a b a b ): copy the top two, order preserved.
    fn two_dup(mut self) -> Outcome {
        let n = self.stack.len();
        if n < 2 {
            return self.fail(ErrorKind::StackUnderflow);
        }
        let a = self.stack[n - 2].clone();
        let b = self.stack[n - 1].clone();
        self.stack.push(a);
        self.stack.push(b);
        Ok(self)
    }

    /// `2drop` ( a b -- ): drop the top two.
    fn two_drop(mut self) -> Outcome {
        let n = self.stack.len();
        if n < 2 {
            return self.fail(ErrorKind::StackUnderflow);
        }
        self.stack.truncate(n - 2);
        Ok(self)
    }

    /// `]`: collect the values above the topmost mark into a `List`, consuming
    /// the mark. Fails with `UnmatchedClose` when no collection is open. The
    /// collected values are, by the region discipline, all non-marks — so the
    /// list never contains a mark. (When `{` arrives, this will also reject a
    /// mark of the wrong kind.)
    fn close_list(mut self) -> Outcome {
        let Some(mark) = self
            .stack
            .iter()
            .rposition(|v| matches!(v, Value::Mark(_)))
        else {
            return self.fail(ErrorKind::UnmatchedClose);
        };
        let items: Vec<Value> = self.stack.drain(mark + 1..).collect();
        self.stack.pop(); // the mark, now on top
        self.stack.push(Value::List(items));
        Ok(self)
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
    fn a_type_error_leaves_the_stack_untouched() {
        // The check runs before any mutation, so the operands are still there.
        let err = Engine::new().apply(&parse("true 1 +").unwrap()).unwrap_err();
        assert_eq!(err.engine.stack(), &[Value::Bool(true), Value::Int(1)]);
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
    fn errors_carry_the_engine_at_the_point_of_failure() {
        // The whole engine is attached for inspection; its stack is the state
        // the failing command saw (operands still present, nothing partial).
        let err = Engine::new().apply(&parse("1 0 /").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::DivideByZero);
        assert_eq!(err.engine.stack(), &[1.0, 0.0]);
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

    #[test]
    fn pick_at_copies_a_level_to_the_top() {
        // copy level 3 to the top
        assert_eq!(
            run("1 2 3").apply(&[Command::PickAt(3)]).unwrap().stack(),
            &[1.0, 2.0, 3.0, 1.0]
        );
    }

    #[test]
    fn drop_at_a_level_removes_it() {
        // remove the `2`
        assert_eq!(
            run("1 2 3").apply(&[Command::DropAt(2)]).unwrap().stack(),
            &[1.0, 3.0]
        );
    }

    #[test]
    fn swap_at_exchanges_with_the_level_below() {
        // level 1 with level 2: same as plain swap
        assert_eq!(
            run("1 2 3").apply(&[Command::SwapAt(1)]).unwrap().stack(),
            &[1.0, 3.0, 2.0]
        );
        // level 2 with level 3
        assert_eq!(
            run("1 2 3").apply(&[Command::SwapAt(2)]).unwrap().stack(),
            &[2.0, 1.0, 3.0]
        );
    }

    #[test]
    fn swap_at_the_bottom_has_nothing_below() {
        assert_eq!(
            run("1 2 3").apply(&[Command::SwapAt(3)]).unwrap_err().kind,
            ErrorKind::StackUnderflow
        );
    }

    #[test]
    fn roll_at_brings_a_level_to_the_top() {
        // bring level 3 to the top
        assert_eq!(
            run("1 2 3 4").apply(&[Command::RollAt(3)]).unwrap().stack(),
            &[1.0, 3.0, 4.0, 2.0]
        );
        // `RollAt(3)` has the same effect as the text `rot`.
        assert_eq!(
            run("1 2 3 rot").stack(),
            run("1 2 3").apply(&[Command::RollAt(3)]).unwrap().stack()
        );
    }

    #[test]
    fn level_zero_and_out_of_range_error() {
        assert_eq!(
            run("1 2").apply(&[Command::DropAt(0)]).unwrap_err().kind,
            ErrorKind::StackUnderflow
        );
        assert_eq!(
            run("1 2").apply(&[Command::RollAt(5)]).unwrap_err().kind,
            ErrorKind::StackUnderflow
        );
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
        // `3 pick` copies level 3 to the top (the level itself is consumed).
        assert_eq!(run("1 2 3 3 pick").stack(), &[1.0, 2.0, 3.0, 1.0]);
        assert_eq!(run("1 2 3 3 roll").stack(), &[2.0, 3.0, 1.0]);
        assert_eq!(run("1 2 3 3 rolld").stack(), &[3.0, 1.0, 2.0]);
        assert_eq!(run("1 2 3 2 dropn").stack(), &[1.0, 3.0]);
        assert_eq!(run("1 2 3 2 swapn").stack(), &[2.0, 1.0, 3.0]);
    }

    #[test]
    fn rolld_inverts_roll() {
        assert_eq!(run("1 2 3 4 3 roll 3 rolld").stack(), &[1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn a_non_integer_level_is_rejected_not_rounded() {
        assert_eq!(
            run_err("1 2 3 2.5 roll"),
            ErrorKind::TypeError {
                expected: "integer",
                found: "float"
            }
        );
    }

    #[test]
    fn a_level_out_of_range_underflows() {
        assert_eq!(run_err("1 2 5 pick"), ErrorKind::StackUnderflow);
        // A level of 0 is not a valid 1-based level either.
        assert_eq!(run_err("1 2 0 roll"), ErrorKind::StackUnderflow);
    }

    #[test]
    fn parameterized_indexed_commands_render_with_their_level() {
        assert_eq!(Command::PickAt(3).to_string(), "pick 3");
        assert_eq!(Command::RollAt(2).to_string(), "roll 2");
        assert_eq!(Command::DropAt(2).to_string(), "dropn 2");
        assert_eq!(Command::SwapAt(2).to_string(), "swapn 2");
    }

    #[test]
    fn a_parameterized_op_at_a_named_level_renders_as_that_word() {
        // A cursor op on the top of stack reads as the fixed shuffle.
        assert_eq!(Command::PickAt(1).to_string(), "dup");
        assert_eq!(Command::DropAt(1).to_string(), "drop");
        assert_eq!(Command::SwapAt(1).to_string(), "swap");
        assert_eq!(Command::RollAt(3).to_string(), "rot");
    }

    // --- M2: lists and the mark discipline ---

    /// Shorthand for a list value from a slice of values.
    fn list(items: &[Value]) -> Value {
        Value::List(items.to_vec())
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
}
