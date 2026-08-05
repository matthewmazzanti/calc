//! Boolean words: `not`, `and`, `or`. Operands must be `Bool` — there is no
//! truthiness rule, so a number is a `TypeError`.

use crate::engine::{Engine, ErrorKind, Primitive, Value};

#[rustfmt::skip]
pub(super) static PRIMITIVES: &[Primitive] = &[
    Primitive { name: "not", run: not },
    Primitive { name: "and", run: and },
    Primitive { name: "or",  run: or },
];

fn not(e: &mut Engine) -> Result<(), ErrorKind> {
    let a = e.pop_bool()?;
    e.push(Value::Bool(!a));
    Ok(())
}

fn and(e: &mut Engine) -> Result<(), ErrorKind> {
    bool_binary(e, |a, b| a && b)
}

fn or(e: &mut Engine) -> Result<(), ErrorKind> {
    bool_binary(e, |a, b| a || b)
}

/// Two-operand boolean op (`and`/`or`).
fn bool_binary(e: &mut Engine, op: impl FnOnce(bool, bool) -> bool) -> Result<(), ErrorKind> {
    let b = e.pop_bool()?;
    let a = e.pop_bool()?;
    e.push(Value::Bool(op(a, b)));
    Ok(())
}
