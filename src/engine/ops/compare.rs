//! Comparison words: `=` (any two values) and the numeric orderings
//! `< > <= >=`. All push a `Bool`.

use crate::engine::{Engine, ErrorKind, Primitive, Value};

#[rustfmt::skip]
pub(super) static PRIMITIVES: &[Primitive] = &[
    Primitive { name: "=",  run: eq },
    Primitive { name: "<",  run: lt },
    Primitive { name: ">",  run: gt },
    Primitive { name: "<=", run: le },
    Primitive { name: ">=", run: ge },
];

/// `=`: equality of the top two values. Numbers compare by value across the
/// int/float split, so `2 2.0 =` is true; anything else is structural equality.
fn eq(e: &mut Engine) -> Result<(), ErrorKind> {
    let b = e.pop()?;
    let a = e.pop()?;
    let equal = match (a.as_num(), b.as_num()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    e.push(Value::Bool(equal));
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

/// Two-operand numeric comparison, pushing a `Bool`.
fn num_compare(e: &mut Engine, op: impl FnOnce(f64, f64) -> bool) -> Result<(), ErrorKind> {
    let b = e.pop_num()?;
    let a = e.pop_num()?;
    e.push(Value::Bool(op(a, b)));
    Ok(())
}
