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

/// A value on the stack. Started as a bare `f64`; now a small sum type so the
/// stack can hold more than numbers. Grows further later (strings, lists,
/// functions). Kept `Copy` while every variant is — strings will end that.
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
        }
    }

    /// Widen to `f64`, or a [`ErrorKind::TypeError`] naming what was found.
    /// Comparisons, division, and mixed arithmetic funnel operands through this,
    /// so an `Int` is accepted wherever a number is wanted.
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

    /// Borrow the string content, or a [`ErrorKind::TypeError`]. `length` funnels
    /// its operand through this.
    fn as_str(&self) -> Result<&str, ErrorKind> {
        match self {
            Value::Str(s) => Ok(s),
            other => Err(ErrorKind::TypeError {
                expected: "string",
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
            Value::Bool(_) | Value::Str(_) => false,
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
    /// Push a copy of the value at the given 1-based level (level 1 == top of
    /// stack) onto the top.
    Dup(usize),
    /// Remove the value at the given 1-based level (level 1 == top of stack).
    Drop(usize),
    /// Exchange the value at `level` with the one just below it (`level + 1`).
    Swap(usize),
    Over,
    /// Roll the value at `level` up to the top, shifting the shallower values
    /// down one.
    Roll(usize),
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
            "dup" => Command::Dup(1),
            // The text commands are the fixed-level cases of the parameterized
            // stack ops: drop the top, swap the top two, roll the top three.
            "drop" => Command::Drop(1),
            "swap" => Command::Swap(1),
            "over" => Command::Over,
            "rot" => Command::Roll(3),
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
            Command::Dup(1) => write!(f, "dup"),
            Command::Dup(l) => write!(f, "dup {l}"),
            Command::Drop(1) => write!(f, "drop"),
            Command::Drop(l) => write!(f, "drop {l}"),
            Command::Swap(1) => write!(f, "swap"),
            Command::Swap(l) => write!(f, "swap {l}"),
            Command::Over => write!(f, "over"),
            Command::Roll(3) => write!(f, "rot"),
            Command::Roll(l) => write!(f, "roll {l}"),
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
            Command::Dup(level) => self.dup(*level),
            Command::Drop(level) => self.drop_at(*level),
            Command::Swap(level) => self.swap(*level),
            // `over` copies the second-from-top value to the top.
            Command::Over => self.dup(2),
            Command::Roll(level) => self.roll(*level),
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
    /// with a `let-else` early return.
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

    /// `length`: the character count of the top string, pushed as an `Int`.
    fn length(mut self) -> Outcome {
        let n = self.stack.len();
        let result = if n < 1 {
            Err(ErrorKind::StackUnderflow)
        } else {
            self.stack[n - 1].as_str().map(|s| s.chars().count() as i64)
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

    /// Push a copy of the value at `level`.
    fn dup(mut self, level: usize) -> Outcome {
        let Some(i) = self.index_of_level(level) else {
            return self.fail(ErrorKind::StackUnderflow);
        };
        let v = self.stack[i].clone();
        self.stack.push(v);
        Ok(self)
    }

    /// Remove the value at `level`.
    fn drop_at(mut self, level: usize) -> Outcome {
        let Some(i) = self.index_of_level(level) else {
            return self.fail(ErrorKind::StackUnderflow);
        };
        self.stack.remove(i);
        Ok(self)
    }

    /// Exchange the value at `level` with the one just below it (`level + 1`).
    fn swap(mut self, level: usize) -> Outcome {
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

    /// Roll the value at `level` up to the top, shifting shallower values down.
    fn roll(mut self, level: usize) -> Outcome {
        let Some(i) = self.index_of_level(level) else {
            return self.fail(ErrorKind::StackUnderflow);
        };
        let v = self.stack.remove(i);
        self.stack.push(v);
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
                expected: "string",
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
        assert_eq!(Command::parse("dup"), Ok(Command::Dup(1)));
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
    fn dup_copies_a_level_to_the_top() {
        // copy level 3 (=1) to the top
        assert_eq!(
            run("1 2 3").apply(&[Command::Dup(3)]).unwrap().stack(),
            &[1.0, 2.0, 3.0, 1.0]
        );
    }

    #[test]
    fn drop_at_a_level_removes_it() {
        // remove the `2`
        assert_eq!(run("1 2 3").apply(&[Command::Drop(2)]).unwrap().stack(), &[1.0, 3.0]);
    }

    #[test]
    fn swap_exchanges_with_the_level_below() {
        // top two: same as plain swap
        assert_eq!(run("1 2 3").apply(&[Command::Swap(1)]).unwrap().stack(), &[1.0, 3.0, 2.0]);
        // level 2 (=2) with level 3 (=1)
        assert_eq!(run("1 2 3").apply(&[Command::Swap(2)]).unwrap().stack(), &[2.0, 1.0, 3.0]);
    }

    #[test]
    fn swap_at_the_bottom_has_nothing_below() {
        assert_eq!(
            run("1 2 3").apply(&[Command::Swap(3)]).unwrap_err().kind,
            ErrorKind::StackUnderflow
        );
    }

    #[test]
    fn roll_brings_a_level_to_the_top() {
        // bring level 3 (=2) to the top
        assert_eq!(
            run("1 2 3 4").apply(&[Command::Roll(3)]).unwrap().stack(),
            &[1.0, 3.0, 4.0, 2.0]
        );
        // Roll(3) is exactly what the text `rot` parses to.
        assert_eq!(
            run("1 2 3 rot").stack(),
            run("1 2 3").apply(&[Command::Roll(3)]).unwrap().stack()
        );
    }

    #[test]
    fn level_zero_and_out_of_range_error() {
        assert_eq!(
            run("1 2").apply(&[Command::Drop(0)]).unwrap_err().kind,
            ErrorKind::StackUnderflow
        );
        assert_eq!(
            run("1 2").apply(&[Command::Roll(5)]).unwrap_err().kind,
            ErrorKind::StackUnderflow
        );
    }
}
