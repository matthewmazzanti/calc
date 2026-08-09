//! Environment words: `=` and `set` bind a name in the current frame, `get`
//! reads one back (reaching the prelude too, so `'+ get` captures the op as a
//! value).
//!
//! **Two binders, differing only in argument order** (§5), and both primitive:
//!
//! ```text
//! 'square {dup *} =            name first
//! {dup *} 'square set          value first
//! ```
//!
//! `=` suits a definition, whose value is a literal and so pushes everything it
//! consumes — the name leads, so a file scans down its left edge and a
//! multi-line definition says what it defines before the reader wades in. `set`
//! suits a computed value, where the expression takes from the stack and a name
//! pushed first would be in its way. `:` is better than either for parameters.

use crate::engine::{Engine, ErrorKind, Primitive};

#[rustfmt::skip]
pub(super) static PRIMITIVES: &[Primitive] = &[
    Primitive { name: "=",   run: bind_name_first },   // name value --
    Primitive { name: "set", run: set },               // value name --
    Primitive { name: "get", run: get },               // name -- value
];

/// `=` ( name value -- ): bind `name` to `value` in the current frame. The name
/// is underneath (`'x 3 =`), which is what lets a definition read left to right.
fn bind_name_first(e: &mut Engine) -> Result<(), ErrorKind> {
    let value = e.pop()?;
    let name = e.pop_name()?;
    e.bind(name, value);
    Ok(())
}

/// `set` ( value name -- ): bind `name` to `value` in the current frame,
/// shadowing any binding further out (including a prelude builtin). The name is
/// on top (`3 'x set`).
fn set(e: &mut Engine) -> Result<(), ErrorKind> {
    let name = e.pop_name()?;
    let value = e.pop()?;
    e.bind(name, value);
    Ok(())
}

/// `get` ( name -- value ): push the value bound to `name` — a user binding or a
/// prelude builtin — or fail with `UnboundName`. The value is *pushed*, not run:
/// the reflective inverse of bare-word application.
fn get(e: &mut Engine) -> Result<(), ErrorKind> {
    let name = e.pop_name()?;
    let value = e
        .lookup(&name)
        .ok_or_else(|| ErrorKind::UnboundName(name.to_string()))?;
    e.push(value);
    Ok(())
}
