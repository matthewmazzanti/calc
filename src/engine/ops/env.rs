//! Environment words: `set` binds a name in the user frame, `get` reads one
//! back (reaching the prelude too, so `'+ get` captures the op as a value).

use crate::engine::{Engine, ErrorKind, Primitive};

#[rustfmt::skip]
pub(super) static PRIMITIVES: &[Primitive] = &[
    Primitive { name: "set", run: set },   // value name --
    Primitive { name: "get", run: get },   // name -- value
];

/// `set` ( value name -- ): bind `name` to `value` in the user frame, shadowing
/// any prior binding (including a prelude builtin). The name is on top
/// (`3 'x set`).
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
