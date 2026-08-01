//! The RPN calculator engine: an [`Engine`] wrapping a stack of floating-point
//! values (and, later, evaluation settings). No I/O, no history — its transforms
//! *consume* `self` and return the new engine, so a sequence of commands folds
//! through with no intermediate copies: the same value is moved from step to
//! step. [`Engine::eval`] is literally `commands.try_fold(self, Engine::apply)`.
//!
//! Because a transform takes `self` by value, an error consumes it — `eval`
//! returns `Err` and the partial engine is dropped. Evaluation is therefore
//! atomic from the caller's side: they keep their own copy (see the `history`
//! module) and commit the returned engine only on `Ok`.

/// The value type held on the stack. Aliased so it can grow later (complex,
/// rationals, …) without touching every call site.
pub type Value = f64;

/// The stack of values, bottom-to-top: the top of stack is the last element.
/// Internal — the public handle is [`Engine`].
type Stack = Vec<Value>;

/// Everything that can go wrong while evaluating a token.
#[derive(Debug, Clone, PartialEq)]
pub enum CalcError {
    /// An operation needed more operands than the stack held.
    StackUnderflow,
    /// Division with a zero divisor.
    DivideByZero,
    /// A token that was neither a number nor a known command.
    UnknownCommand(String),
}

impl std::fmt::Display for CalcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CalcError::StackUnderflow => write!(f, "too few arguments"),
            CalcError::DivideByZero => write!(f, "divide by zero"),
            CalcError::UnknownCommand(c) => write!(f, "unknown command: {c}"),
        }
    }
}

impl std::error::Error for CalcError {}

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
    pub fn parse(token: &str) -> Result<Command, CalcError> {
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
            other => return Err(CalcError::UnknownCommand(other.to_string())),
        })
    }
}

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

    /// Evaluate a whitespace-separated line, threading the engine through each
    /// command. The line is parsed first, so a malformed token fails before
    /// anything runs; the first runtime error short-circuits the fold and the
    /// partial engine is dropped with it.
    pub fn eval(self, input: &str) -> Result<Self, CalcError> {
        let program = input
            .split_whitespace()
            .map(Command::parse)
            .collect::<Result<Vec<_>, _>>()?;
        program.into_iter().try_fold(self, Engine::apply)
    }

    /// Apply a single command, consuming `self` and returning the new engine. A
    /// total match, so a new command variant is a compile error until handled.
    /// Every arm checks its preconditions before mutating, so on error the
    /// caller's copy is untouched (the partial engine here is just dropped).
    pub fn apply(mut self, cmd: Command) -> Result<Self, CalcError> {
        match cmd {
            Command::Push(n) => self.stack.push(n),
            Command::Add => binary(&mut self.stack, |a, b| Ok(a + b))?,
            Command::Sub => binary(&mut self.stack, |a, b| Ok(a - b))?,
            Command::Mul => binary(&mut self.stack, |a, b| Ok(a * b))?,
            Command::Div => binary(&mut self.stack, |a, b| {
                if b == 0.0 {
                    Err(CalcError::DivideByZero)
                } else {
                    Ok(a / b)
                }
            })?,
            Command::Neg => unary(&mut self.stack, |x| -x)?,
            Command::Dup(level) => {
                let i = level_index(self.stack.len(), level)?;
                let v = self.stack[i];
                self.stack.push(v);
            }
            Command::Drop(level) => {
                let i = level_index(self.stack.len(), level)?;
                self.stack.remove(i);
            }
            Command::Swap(level) => {
                let n = self.stack.len();
                let i = level_index(n, level)?;
                let j = level_index(n, level + 1)?;
                self.stack.swap(i, j);
            }
            Command::Over => stack_op(&mut self.stack, 2, |s| s.push(s[s.len() - 2]))?,
            // ( a b c ) -> ( b c a ): rolling the third value to the top.
            Command::Roll(level) => {
                let i = level_index(self.stack.len(), level)?;
                let v = self.stack.remove(i);
                self.stack.push(v);
            }
            Command::Clear => stack_op(&mut self.stack, 0, |s| s.clear())?,
        }
        Ok(self)
    }
}

/// Convert a 1-based level (level 1 == top of stack) into a `Stack` index.
/// Errors if the level doesn't exist on a stack of `len` values.
fn level_index(len: usize, level: usize) -> Result<usize, CalcError> {
    if level == 0 || level > len {
        return Err(CalcError::StackUnderflow);
    }
    Ok(len - level)
}

/// Apply a two-operand arithmetic op. `a` is the deeper operand, `b` the top,
/// so `a b <op>` reads left-to-right as `a <op> b`. The op returns a `Result`
/// so it can reject its inputs (e.g. divide-by-zero).
fn binary(
    stack: &mut Stack,
    op: impl FnOnce(Value, Value) -> Result<Value, CalcError>,
) -> Result<(), CalcError> {
    let len = stack.len();
    if len < 2 {
        return Err(CalcError::StackUnderflow);
    }
    let result = op(stack[len - 2], stack[len - 1])?;
    stack.truncate(len - 2);
    stack.push(result);
    Ok(())
}

/// Apply a one-operand op to the top of the stack in place.
fn unary(stack: &mut [Value], op: impl FnOnce(Value) -> Value) -> Result<(), CalcError> {
    let len = stack.len();
    if len < 1 {
        return Err(CalcError::StackUnderflow);
    }
    stack[len - 1] = op(stack[len - 1]);
    Ok(())
}

/// Run a pure stack manipulation after checking it has at least `need` values.
/// The closure may assume that precondition holds.
fn stack_op(
    stack: &mut Stack,
    need: usize,
    op: impl FnOnce(&mut Stack),
) -> Result<(), CalcError> {
    if stack.len() < need {
        return Err(CalcError::StackUnderflow);
    }
    op(stack);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Evaluate `input` from a fresh engine and return the result.
    fn run(input: &str) -> Engine {
        Engine::new().eval(input).unwrap()
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
        assert_eq!(run("1 0").apply(Command::Div), Err(CalcError::DivideByZero));
    }

    #[test]
    fn underflow_is_an_error() {
        assert_eq!(run("1").apply(Command::Add), Err(CalcError::StackUnderflow));
    }

    #[test]
    fn an_error_leaves_the_callers_engine_untouched() {
        // Family-C atomicity: evaluate a *copy*; on error the original is intact
        // simply because it was never moved into `eval`.
        let original = run("1");
        assert_eq!(original.clone().eval("+"), Err(CalcError::StackUnderflow));
        assert_eq!(original.stack(), &[1.0]);
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
    fn unknown_command_errors() {
        assert_eq!(
            Engine::new().eval("foo"),
            Err(CalcError::UnknownCommand("foo".to_string()))
        );
    }

    #[test]
    fn parse_maps_tokens_to_commands() {
        assert_eq!(Command::parse("3.5"), Ok(Command::Push(3.5)));
        assert_eq!(Command::parse("+"), Ok(Command::Add));
        assert_eq!(Command::parse("dup"), Ok(Command::Dup(1)));
        assert_eq!(
            Command::parse("nope"),
            Err(CalcError::UnknownCommand("nope".to_string()))
        );
    }

    #[test]
    fn a_parse_error_reports_the_bad_token() {
        // The whole line is parsed before any command runs, so a bad token
        // fails the line without applying anything.
        assert_eq!(
            Engine::new().eval("1 2 + oops"),
            Err(CalcError::UnknownCommand("oops".to_string()))
        );
    }

    #[test]
    fn commands_fold_through_apply() {
        // The TUI path: hand the engine Commands without going through text.
        // This is the same fold `eval` uses, spelled out.
        let engine = [Command::Push(2.0), Command::Push(3.0), Command::Mul]
            .into_iter()
            .try_fold(Engine::new(), Engine::apply)
            .unwrap();
        assert_eq!(engine.stack(), &[6.0]);
    }

    #[test]
    fn dup_copies_a_level_to_the_top() {
        // copy level 3 (=1) to the top
        assert_eq!(
            run("1 2 3").apply(Command::Dup(3)).unwrap().stack(),
            &[1.0, 2.0, 3.0, 1.0]
        );
    }

    #[test]
    fn drop_at_a_level_removes_it() {
        // remove the `2`
        assert_eq!(run("1 2 3").apply(Command::Drop(2)).unwrap().stack(), &[1.0, 3.0]);
    }

    #[test]
    fn swap_exchanges_with_the_level_below() {
        // top two: same as plain swap
        assert_eq!(run("1 2 3").apply(Command::Swap(1)).unwrap().stack(), &[1.0, 3.0, 2.0]);
        // level 2 (=2) with level 3 (=1)
        assert_eq!(run("1 2 3").apply(Command::Swap(2)).unwrap().stack(), &[2.0, 1.0, 3.0]);
    }

    #[test]
    fn swap_at_the_bottom_has_nothing_below() {
        assert_eq!(
            run("1 2 3").apply(Command::Swap(3)),
            Err(CalcError::StackUnderflow)
        );
    }

    #[test]
    fn roll_brings_a_level_to_the_top() {
        // bring level 3 (=2) to the top
        assert_eq!(
            run("1 2 3 4").apply(Command::Roll(3)).unwrap().stack(),
            &[1.0, 3.0, 4.0, 2.0]
        );
        // Roll(3) is exactly what the text `rot` parses to.
        assert_eq!(
            run("1 2 3 rot").stack(),
            run("1 2 3").apply(Command::Roll(3)).unwrap().stack()
        );
    }

    #[test]
    fn level_zero_and_out_of_range_error() {
        assert_eq!(run("1 2").apply(Command::Drop(0)), Err(CalcError::StackUnderflow));
        assert_eq!(run("1 2").apply(Command::Roll(5)), Err(CalcError::StackUnderflow));
    }
}
