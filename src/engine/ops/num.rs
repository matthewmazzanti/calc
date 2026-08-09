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
//! **`log` is base 10 and `ln` is natural**, the calculator convention (HP48,
//! TI) rather than C's and Python's `log`-means-natural. A session types `log`
//! meaning the base-10 one far more often than it types anything needing the
//! other spelling, and having both names present makes the split unambiguous at
//! a glance — which the single-`log` convention cannot manage.
//!
//! **Trig is in radians, with no angle mode.** `to_rad`/`to_deg` convert
//! explicitly (`30 to_rad sin`), which is a deliberate first answer rather than
//! a placeholder: a mode is hidden state that silently changes what `sin` means,
//! and it is the classic source of a confidently wrong calculator answer. If a
//! mode arrives later it rides in the [`Engine`] and is snapshotted with
//! everything else, so nothing here has to change to allow it.
//!
//! **Integer-preserving where it can be**: `+ - * neg abs` on two `Int`s, `^`
//! with a non-negative `Int` exponent, and `floor ceil round trunc`, which
//! *produce* an `Int` so a rounded value can go straight into `nth` or `pickn`.
//! Every transcendental yields a float; `/` and `inv` always do.

use std::rc::Rc;

use crate::engine::{Engine, ErrorKind, Primitive, Value};

#[rustfmt::skip]
pub(super) static PRIMITIVES: &[Primitive] = &[
    Primitive { name: "+",      run: add },                          // a b -- a+b, or two strings joined
    Primitive { name: "-",      run: sub },                          // a b -- a-b
    Primitive { name: "*",      run: mul },                          // a b -- a*b
    Primitive { name: "/",      run: div },                          // a b -- a/b, always a float
    Primitive { name: "neg",    run: neg },                          // a -- -a
    Primitive { name: "abs",    run: abs },                          // a -- |a|
    Primitive { name: "inv",    run: inv },                          // a -- 1/a
    Primitive { name: "^",      run: pow },                          // a b -- a^b
    Primitive { name: "<",      run: lt },                           // a b -- bool
    Primitive { name: ">",      run: gt },                           // a b -- bool
    Primitive { name: "<=",     run: le },                           // a b -- bool
    Primitive { name: ">=",     run: ge },                           // a b -- bool
    // Rounding, each yielding an `Int` where the result fits in one.
    Primitive { name: "floor",  run: |e| round_with(e, f64::floor) },   // toward -inf
    Primitive { name: "ceil",   run: |e| round_with(e, f64::ceil) },    // toward +inf
    Primitive { name: "round",  run: |e| round_with(e, f64::round) },   // half away from zero
    Primitive { name: "trunc",  run: |e| round_with(e, f64::trunc) },   // toward zero
    // Transcendental. `sqrt`, `ln`, `log`, `asin`, and `acos` have domains, and
    // leaving one is `Undefined` rather than a NaN — see [`float_unary`].
    Primitive { name: "sqrt",   run: |e| float_unary(e, f64::sqrt) },
    Primitive { name: "exp",    run: |e| float_unary(e, f64::exp) },
    Primitive { name: "ln",     run: |e| float_unary(e, f64::ln) },     // natural
    Primitive { name: "log",    run: |e| float_unary(e, f64::log10) },  // base 10
    Primitive { name: "log2",   run: |e| float_unary(e, f64::log2) },
    Primitive { name: "logb",   run: logb },                         // x b -- log_b x
    Primitive { name: "sin",    run: |e| float_unary(e, f64::sin) },    // radians
    Primitive { name: "cos",    run: |e| float_unary(e, f64::cos) },
    Primitive { name: "tan",    run: |e| float_unary(e, f64::tan) },
    Primitive { name: "asin",   run: |e| float_unary(e, f64::asin) },
    Primitive { name: "acos",   run: |e| float_unary(e, f64::acos) },
    Primitive { name: "atan",   run: |e| float_unary(e, f64::atan) },
    Primitive { name: "atan2",  run: atan2 },                        // y x -- angle
    Primitive { name: "to_rad", run: |e| float_unary(e, f64::to_radians) },
    Primitive { name: "to_deg", run: |e| float_unary(e, f64::to_degrees) },
];

/// The constants the prelude binds, as `(word, value)`. Bindings rather than
/// literals for the same reason `true`/`false` are (see [`bool`](super::bool)):
/// the language has no keywords, so `pi` is fetchable, shadowable, and
/// `del`-recoverable like any other name.
pub(super) fn constants() -> impl Iterator<Item = (&'static str, Value)> {
    [
        ("pi", Value::Num(std::f64::consts::PI)),
        ("e", Value::Num(std::f64::consts::E)),
        ("tau", Value::Num(std::f64::consts::TAU)),
    ]
    .into_iter()
}

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
        other => return Err(not_a_number(&other)),
    };
    e.push(v);
    Ok(())
}

/// `abs`: magnitude, preserving `Int` (falling back to a float only on the
/// `i64::MIN` overflow, like [`neg`]).
fn abs(e: &mut Engine) -> Result<(), ErrorKind> {
    let v = match e.pop()? {
        Value::Int(i) => i
            .checked_abs()
            .map(Value::Int)
            .unwrap_or_else(|| Value::Num((i as f64).abs())),
        Value::Num(x) => Value::Num(x.abs()),
        other => return Err(not_a_number(&other)),
    };
    e.push(v);
    Ok(())
}

/// `inv` ( a -- 1/a ): the reciprocal, always a float. Zero is
/// [`ErrorKind::DivideByZero`] rather than [`ErrorKind::Undefined`] — it is the
/// same division `1 0 /` performs, so it gets the same, more specific answer.
fn inv(e: &mut Engine) -> Result<(), ErrorKind> {
    let x = e.pop_num()?;
    if x == 0.0 {
        return Err(ErrorKind::DivideByZero);
    }
    e.push(Value::Num(1.0 / x));
    Ok(())
}

/// `^` ( a b -- a^b ): exponentiation, integer-preserving when the base is an
/// `Int` and the exponent a non-negative `Int` whose power doesn't overflow —
/// `2 10 ^` is `1024`, not `1024.0`. A negative exponent yields a float
/// (`2 -1 ^` is `0.5`), since the integer answer would truncate to nothing.
///
/// Spelled `^` rather than `pow`: it is an *operator* the way `+` and `*` are,
/// and `^` is free — the logic words are named `and`/`or`/`xor`, which is
/// precisely what keeps `&`, `|`, `^`, and `~` out of the vocabulary and
/// available (see [`bool`](super::bool)).
fn pow(e: &mut Engine) -> Result<(), ErrorKind> {
    let b = e.pop()?;
    let a = e.pop()?;
    let v = match (&a, &b) {
        (Value::Int(base), Value::Int(exp)) if *exp >= 0 => u32::try_from(*exp)
            .ok()
            .and_then(|exp| base.checked_pow(exp))
            .map(Value::Int)
            .unwrap_or_else(|| Value::Num((*base as f64).powf(*exp as f64))),
        _ => {
            let (a, b) = (a.as_num()?, b.as_num()?);
            Value::Num(defined(a.powf(b), &[a, b])?)
        }
    };
    e.push(v);
    Ok(())
}

/// `logb` ( x b -- log_b x ): the logarithm of `x` to base `b`, with the base on
/// top so it reads as a parameter to the word below it, like `nth`'s index.
///
/// **Bases 10 and 2 dispatch to the dedicated kernels.** The general form is
/// `ln x / ln b`, which rounds twice and drifts off exact answers: `1000 10 logb`
/// computes 2.9999999999999996 rather than 3. Unlike [`exp`] versus `^` — a
/// 15th-digit difference no display would show — this one is visible at any
/// precision a calculator would use, so the special case earns its place here
/// where it did not there.
///
/// Not C's `logb`, which extracts a float's exponent. That function has no
/// meaning in a calculator vocabulary, and this name is the one a user reaches
/// for; the collision is only visible from C.
fn logb(e: &mut Engine) -> Result<(), ErrorKind> {
    let base = e.pop_num()?;
    let x = e.pop_num()?;
    // A base must be positive and not 1. Checked up front rather than left to
    // [`defined`], because base 0 would otherwise slip through: `ln x / -inf` is
    // a *finite* `-0`, so the result looks defined when the operation is not.
    if base <= 0.0 || base == 1.0 {
        return Err(ErrorKind::Undefined);
    }
    let y = match base {
        10.0 => x.log10(),
        2.0 => x.log2(),
        _ => x.log(base),
    };
    e.push(Value::Num(defined(y, &[x, base])?));
    Ok(())
}

/// `atan2` ( y x -- angle ): the angle to the point `(x, y)` in radians, using
/// both signs to place it in the right quadrant — which `atan` on a ratio
/// cannot do. Operand order matches the `atan2(y, x)` convention, so the `y`
/// that would have been the numerator stays underneath.
fn atan2(e: &mut Engine) -> Result<(), ErrorKind> {
    let x = e.pop_num()?;
    let y = e.pop_num()?;
    e.push(Value::Num(y.atan2(x)));
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

/// One-operand float math: pop, apply, push — the shape every transcendental
/// shares, so each is one table row. The result is always a [`Value::Num`], even
/// where it happens to be integral (`1 ln` is `0`, displayed as `0`).
fn float_unary(e: &mut Engine, f: impl FnOnce(f64) -> f64) -> Result<(), ErrorKind> {
    let x = e.pop_num()?;
    e.push(Value::Num(defined(f(x), &[x])?));
    Ok(())
}

/// Guard a computed float: a non-finite result from finite operands means the
/// word was applied outside its domain (`-1 sqrt`, `0 ln`) or overflowed
/// (`1000 exp`), and is [`ErrorKind::Undefined`] rather than a NaN on the stack.
///
/// One rule covers every case, so no word carries its own domain check and none
/// can be forgotten. Operands are examined so a non-finite input *passes
/// through* — the word didn't produce it, and rejecting it here would report the
/// wrong culprit.
fn defined(y: f64, operands: &[f64]) -> Result<f64, ErrorKind> {
    if y.is_finite() || operands.iter().any(|x| !x.is_finite()) {
        Ok(y)
    } else {
        Err(ErrorKind::Undefined)
    }
}

/// Round by `f`, yielding an `Int` where the result fits in one — `3.7 floor` is
/// `3`, not `3.0`, so a rounded value can feed `nth` or `pickn` directly. An
/// `Int` is already integral and passes through untouched. A float too large for
/// an `i64` keeps its `Num` form rather than saturating to a wrong integer.
fn round_with(e: &mut Engine, f: impl FnOnce(f64) -> f64) -> Result<(), ErrorKind> {
    let v = match e.pop()? {
        Value::Int(i) => Value::Int(i),
        Value::Num(x) => {
            let r = f(x);
            // `as` saturates, so the range is checked first; the bounds are
            // exactly representable powers of two, so the comparison is exact.
            if (i64::MIN as f64..=i64::MAX as f64).contains(&r) {
                Value::Int(r as i64)
            } else {
                Value::Num(r)
            }
        }
        other => return Err(not_a_number(&other)),
    };
    e.push(v);
    Ok(())
}

/// The mismatch the type-dispatching number words report.
fn not_a_number(found: &Value) -> ErrorKind {
    ErrorKind::TypeError {
        expected: "number",
        found: found.type_name(),
    }
}
