//! Control words — the primitives that *apply* something rather than compute.
//!
//! `call` and `if`, which are expected to be the whole native set. Everything
//! else that takes a function — `dip`, `keep`, `bi`, `each`, `while` — is
//! definable over them in the language itself, which is where `direction-v2.md`
//! puts it (V6), and running flat is what makes that affordable: a combinator
//! defined by recursion doesn't grow the Rust stack.
//!
//! Both are **tail-position**: they schedule a call and return, letting the
//! evaluation loop descend. Neither runs a callable to completion itself, which
//! is what would reintroduce Rust recursion.

use crate::engine::{Engine, ErrorKind, Primitive, Value};

#[rustfmt::skip]
pub(super) static PRIMITIVES: &[Primitive] = &[
    Primitive { name: "call", run: call },     // fn -- ...
    Primitive { name: "if",   run: choose },   // bool then else -- ...
];

/// `call` ( fn -- … ): apply the value on top of the stack.
///
/// Reaches the same seam a bare word does, so a function enters its frame and a
/// builtin runs — and **anything else is pushed back**, since a value is a
/// nullary function that pushes itself (§1). So `3 call` is `3`, not an error.
///
/// In tail position this is a tail call: the activation it pushes replaces the
/// caller's rather than stacking on it, so `{… &f call}` recurses flat.
fn call(e: &mut Engine) -> Result<(), ErrorKind> {
    let value = e.pop()?;
    e.apply_value(value)
}

/// `if` ( bool then else -- … ): apply one of two functions.
///
/// **An ordinary word, not a special form** (§12.2). Both branches are already
/// on the stack as values when it runs; what makes that lazy is that a `{ }` is
/// a value in the first place, so neither branch has *run* — only been
/// instantiated. Applying the chosen one is the same seam `call` uses, so a
/// branch may equally be a builtin or a plain value.
///
/// The condition is a genuine boolean: no truthiness, so `1 {…} {…} if` is a
/// type error rather than a guess.
fn choose(e: &mut Engine) -> Result<(), ErrorKind> {
    let otherwise = e.pop()?;
    let then = e.pop()?;
    let taken = match e.pop()? {
        Value::Bool(true) => then,
        Value::Bool(false) => otherwise,
        other => {
            return Err(ErrorKind::TypeError {
                expected: "bool",
                found: other.type_name(),
            })
        }
    };
    e.apply_value(taken)
}
