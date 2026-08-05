//! Arithmetic words: `+ - * /` and `neg`. Integer-preserving where both operands
//! are `Int` (overflow promotes to `f64`); `/` always yields a float; `+` also
//! concatenates two strings.

use std::rc::Rc;

use crate::engine::{Engine, ErrorKind, Primitive, Value};

pub(crate) const ADD: Primitive = Primitive {
    name: "+",
    run: add,
};
pub(crate) const SUB: Primitive = Primitive {
    name: "-",
    run: sub,
};
pub(crate) const MUL: Primitive = Primitive {
    name: "*",
    run: mul,
};
pub(crate) const DIV: Primitive = Primitive {
    name: "/",
    run: div,
};

#[rustfmt::skip]
pub(super) static PRIMITIVES: &[Primitive] = &[
    ADD,
    SUB,
    MUL,
    DIV,
    Primitive { name: "neg", run: neg },
];

/// `+`: concatenate two strings, or add two numbers. A string and a number is a
/// type error from the numeric path (no implicit `to_str`).
fn add(e: &mut Engine) -> Result<(), ErrorKind> {
    let b = e.pop()?;
    let a = e.pop()?;
    let v = match (a, b) {
        (Value::Str(mut a), Value::Str(b)) => {
            Rc::make_mut(&mut a).push_str(&b);
            Value::Str(a)
        }
        (a, b) => arith_values(a, b, i64::checked_add, |a, b| a + b)?,
    };
    e.push(v);
    Ok(())
}

/// `-`: integer-preserving subtraction.
fn sub(e: &mut Engine) -> Result<(), ErrorKind> {
    arith(e, i64::checked_sub, |a, b| a - b)
}

/// `*`: integer-preserving multiplication.
fn mul(e: &mut Engine) -> Result<(), ErrorKind> {
    arith(e, i64::checked_mul, |a, b| a * b)
}

/// `/`: division, always yielding a float — `1 2 /` is `0.5`, not `0`.
fn div(e: &mut Engine) -> Result<(), ErrorKind> {
    num_binary(e, |a, b| {
        if b == 0.0 {
            Err(ErrorKind::DivideByZero)
        } else {
            Ok(a / b)
        }
    })
}

/// `neg`: negate the top, preserving `Int` (falling back to a float only on the
/// `i64::MIN` overflow).
fn neg(e: &mut Engine) -> Result<(), ErrorKind> {
    let v = match e.pop()? {
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
    e.push(v);
    Ok(())
}

/// Integer-preserving binary arithmetic (`- *`, and the numeric branch of `+`):
/// pop two, combine, push.
fn arith(
    e: &mut Engine,
    checked: impl FnOnce(i64, i64) -> Option<i64>,
    float: impl FnOnce(f64, f64) -> f64,
) -> Result<(), ErrorKind> {
    let b = e.pop()?;
    let a = e.pop()?;
    let v = arith_values(a, b, checked, float)?;
    e.push(v);
    Ok(())
}

/// Combine two owned values: two `Int`s stay an `Int` via `checked` (promoting
/// to `f64` on overflow), else both widen to `f64`. A bool operand is a
/// `TypeError` (via [`Value::as_num`](crate::engine::Value)).
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

/// Two-operand op whose result is always a float. `a` is the deeper operand, `b`
/// the top, so `a b <op>` reads left-to-right as `a <op> b`. Both widen via
/// [`Value::as_num`](crate::engine::Value); the op may still reject them
/// (divide-by-zero). Used by `/`.
fn num_binary(
    e: &mut Engine,
    op: impl FnOnce(f64, f64) -> Result<f64, ErrorKind>,
) -> Result<(), ErrorKind> {
    let b = e.pop_num()?;
    let a = e.pop_num()?;
    e.push(Value::Num(op(a, b)?));
    Ok(())
}
