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

/// The value type held on the stack. Aliased so it can grow later (complex,
/// rationals, …) without touching every call site.
pub type Value = f64;

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
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErrorKind::StackUnderflow => write!(f, "too few arguments"),
            ErrorKind::DivideByZero => write!(f, "divide by zero"),
            ErrorKind::UnknownCommand(c) => write!(f, "unknown command: {c}"),
        }
    }
}

/// A single parsed instruction. Parsing turns text into these; evaluation
/// consumes them. `Push` carries its literal, so the whole program is a flat
/// stream of `Command`s — no AST, because RPN has no nesting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    Push(Value),
    Add,
    Sub,
    Mul,
    Div,
    Neg,
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
        if let Ok(n) = token.parse::<Value>() {
            return Ok(Command::Push(n));
        }
        Ok(match token {
            "+" => Command::Add,
            "-" => Command::Sub,
            "*" => Command::Mul,
            "/" => Command::Div,
            "neg" => Command::Neg,
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

/// Parse a whitespace-separated line into a program, failing on the first
/// unknown token (its text is carried in the [`ErrorKind`]). Parsing is a
/// frontend concern — the engine itself only runs programs, via
/// [`Engine::apply`].
pub fn parse(input: &str) -> Result<Vec<Command>, ErrorKind> {
    input.split_whitespace().map(Command::parse).collect()
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
            .try_fold(self, |engine, (index, &command)| {
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
    /// A total match, so a new command variant is a compile error until handled.
    fn apply_one(mut self, cmd: Command) -> Outcome {
        match cmd {
            Command::Push(n) => {
                self.stack.push(n);
                Ok(self)
            }
            Command::Add => self.binary(|a, b| Ok(a + b)),
            Command::Sub => self.binary(|a, b| Ok(a - b)),
            Command::Mul => self.binary(|a, b| Ok(a * b)),
            Command::Div => self.binary(|a, b| {
                if b == 0.0 {
                    Err(ErrorKind::DivideByZero)
                } else {
                    Ok(a / b)
                }
            }),
            Command::Neg => self.unary(|x| -x),
            Command::Dup(level) => self.dup(level),
            Command::Drop(level) => self.drop_at(level),
            Command::Swap(level) => self.swap(level),
            // `over` copies the second-from-top value to the top.
            Command::Over => self.dup(2),
            Command::Roll(level) => self.roll(level),
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

    /// Two-operand arithmetic. `a` is the deeper operand, `b` the top, so
    /// `a b <op>` reads left-to-right as `a <op> b`. The op may reject its
    /// inputs (e.g. divide-by-zero).
    fn binary(
        mut self,
        op: impl FnOnce(Value, Value) -> Result<Value, ErrorKind>,
    ) -> Outcome {
        let n = self.stack.len();
        let result = if n < 2 {
            Err(ErrorKind::StackUnderflow)
        } else {
            op(self.stack[n - 2], self.stack[n - 1])
        };
        match result {
            Ok(result) => {
                self.stack.truncate(n - 2);
                self.stack.push(result);
                Ok(self)
            }
            Err(kind) => self.fail(kind),
        }
    }

    /// One-operand op applied to the top of stack.
    fn unary(mut self, op: impl FnOnce(Value) -> Value) -> Outcome {
        let n = self.stack.len();
        if n < 1 {
            return self.fail(ErrorKind::StackUnderflow);
        }
        self.stack[n - 1] = op(self.stack[n - 1]);
        Ok(self)
    }

    /// Push a copy of the value at `level`.
    fn dup(mut self, level: usize) -> Outcome {
        let Some(i) = self.index_of_level(level) else {
            return self.fail(ErrorKind::StackUnderflow);
        };
        let v = self.stack[i];
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
                Command::Push(1.0),
                Command::Push(2.0),
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
            Ok(vec![Command::Push(1.0), Command::Push(2.0), Command::Add])
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
        assert_eq!(Command::parse("3.5"), Ok(Command::Push(3.5)));
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
            .apply(&[Command::Push(2.0), Command::Push(3.0), Command::Mul])
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
