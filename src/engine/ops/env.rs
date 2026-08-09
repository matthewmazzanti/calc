//! Environment words: `=` and `set` bind a name in the current frame, `get`
//! applies one (reaching the prelude too, so `3 4 '+ get` adds) — the dynamic
//! counterpart of writing the word out. `&f` is the other direction: the
//! binding, unapplied.
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

/// `get` ( name -- … ): **apply** the binding named on top of the stack — a user
/// binding or a prelude builtin — or fail with `UnboundName`.
///
/// `'x get` is what writing `x` does, with the name arriving as a value instead
/// of being written out, so a computed name can be applied. It reaches
/// [`Engine::apply_value`], the same seam a bare word does: a function enters
/// its frame, a builtin runs, and a data binding pushes — which is why
/// `'x 1 = 'x get` leaves `1`.
///
/// **`&x` is the other direction, and the two are no longer the same word**: the
/// sigil defers instead of applying — `&x` is `{x}` — so `'x get` is `&x call`.
fn get(e: &mut Engine) -> Result<(), ErrorKind> {
    let name = e.pop_name()?;
    let value = e
        .lookup(&name)
        .ok_or_else(|| ErrorKind::UnboundName(name.to_string()))?;
    e.apply_value(value)
}
