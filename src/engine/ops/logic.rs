//! Logic words: `not`, `and`, `or`, `xor` — **generic over booleans and
//! integers**, logical on the first and bitwise on the second. One name per
//! operation rather than Python's two (`and` beside `&`), which is what keeps
//! `&`, `|`, `^`, and `~` out of the vocabulary entirely — they stay ordinary
//! name characters, and `&` stays free to be the fetch sigil.
//!
//! There is no truthiness rule and no mixing: both operands must be the same
//! kind, so `true 1 and` is a `TypeError`, as is a float (bitwise on an
//! approximation is meaningless).
//!
//! **These are strict**, so they are Python's `& | ^ ~` rather than its
//! short-circuiting `and`/`or`: both operands are already on the stack when the
//! word runs. Short-circuiting needs unevaluated operands, which in this
//! language means functions — a `{…} {…}` combinator over `if` — and would be a
//! separate word, not this one behaving differently.
//!
//! On an integer, `not` is the bitwise complement: `!x`, which is `-x - 1` in
//! two's complement, matching Python's `~`.

use crate::engine::{Engine, ErrorKind, Primitive, Value};

#[rustfmt::skip]
pub(super) static PRIMITIVES: &[Primitive] = &[
    Primitive { name: "not", run: not },   // logical or bitwise complement
    Primitive { name: "and", run: and },
    Primitive { name: "or",  run: or },
    Primitive { name: "xor", run: xor },
];

fn not(e: &mut Engine) -> Result<(), ErrorKind> {
    let value = match e.pop()? {
        Value::Bool(a) => Value::Bool(!a),
        Value::Int(a) => Value::Int(!a),
        other => return Err(type_error(&other)),
    };
    e.push(value);
    Ok(())
}

fn and(e: &mut Engine) -> Result<(), ErrorKind> {
    binary(e, |a, b| a && b, |a, b| a & b)
}

fn or(e: &mut Engine) -> Result<(), ErrorKind> {
    binary(e, |a, b| a || b, |a, b| a | b)
}

fn xor(e: &mut Engine) -> Result<(), ErrorKind> {
    binary(e, |a, b| a != b, |a, b| a ^ b)
}

/// Apply whichever of the two readings the operands call for. The operands are
/// popped before either is checked, so the pair decides together: two bools are
/// logical, two ints are bitwise, and anything else — including one of each —
/// is a type error naming what the *second* operand should have been.
fn binary(
    e: &mut Engine,
    logical: impl FnOnce(bool, bool) -> bool,
    bitwise: impl FnOnce(i64, i64) -> i64,
) -> Result<(), ErrorKind> {
    let b = e.pop()?;
    let a = e.pop()?;
    let value = match (&a, &b) {
        (Value::Bool(a), Value::Bool(b)) => Value::Bool(logical(*a, *b)),
        (Value::Int(a), Value::Int(b)) => Value::Int(bitwise(*a, *b)),
        (Value::Bool(_) | Value::Int(_), other) => return Err(type_error(other)),
        (other, _) => return Err(type_error(other)),
    };
    e.push(value);
    Ok(())
}

/// The mismatch these words report: they take a bool or an int, both readings
/// of the same operation, and nothing else.
fn type_error(found: &Value) -> ErrorKind {
    ErrorKind::TypeError {
        expected: "bool or integer",
        found: found.type_name(),
    }
}
