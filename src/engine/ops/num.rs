//! Number words: arithmetic (`+ - * /`, `neg`) and the orderings (`< > <= >=`).
//! Integer-preserving where both operands are `Int` (overflow promotes to
//! `f64`); `/` always yields a float; the orderings widen both operands through
//! [`Value::as_num`](crate::engine::Value) and push a `Bool`.
//!
//! `+` also concatenates two strings. That is arithmetic with a string reading,
//! not a sequence word, so it lives here; a string *and* a number is a type
//! error from the numeric path (no implicit `to_str`).
//!
//! **Equality is not here.** `==` compares any two values, so it sits with the
//! [generic](super::generic) words; only the orderings are numeric. That it is
//! `==` rather than `=` is because **`=` is the binder** (§5): `'sq {dup *} =`
//! reads name-first, which is what makes a definition scan down its left edge.
//! Doubling the character for equality is the cost of spending the single one on
//! the thing a calculator session does more often.
//!
//! The math functions (`sqrt exp ln`, the trig family, `floor ceil round`) and
//! the constants (`pi e tau`) land here next — trig once the engine carries an
//! angle mode.

use std::rc::Rc;

use crate::engine::{Engine, ErrorKind, Primitive, Value};

#[rustfmt::skip]
pub(super) static PRIMITIVES: &[Primitive] = &[
    Primitive { name: "+",   run: add },   // a b -- a+b, or two strings joined
    Primitive { name: "-",   run: sub },   // a b -- a-b
    Primitive { name: "*",   run: mul },   // a b -- a*b
    Primitive { name: "/",   run: div },   // a b -- a/b, always a float
    Primitive { name: "neg", run: neg },   // a -- -a
    Primitive { name: "<",   run: lt },    // a b -- bool
    Primitive { name: ">",   run: gt },    // a b -- bool
    Primitive { name: "<=",  run: le },    // a b -- bool
    Primitive { name: ">=",  run: ge },    // a b -- bool
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

fn lt(e: &mut Engine) -> Result<(), ErrorKind> {
    num_compare(e, |a, b| a < b)
}

fn gt(e: &mut Engine) -> Result<(), ErrorKind> {
    num_compare(e, |a, b| a > b)
}

fn le(e: &mut Engine) -> Result<(), ErrorKind> {
    num_compare(e, |a, b| a <= b)
}

fn ge(e: &mut Engine) -> Result<(), ErrorKind> {
    num_compare(e, |a, b| a >= b)
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

/// Two-operand numeric comparison, pushing a `Bool`.
fn num_compare(e: &mut Engine, op: impl FnOnce(f64, f64) -> bool) -> Result<(), ErrorKind> {
    let b = e.pop_num()?;
    let a = e.pop_num()?;
    e.push(Value::Bool(op(a, b)));
    Ok(())
}
