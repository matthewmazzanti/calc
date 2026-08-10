//! The builtin **word vocabulary** — free functions over a `&mut Engine`, one
//! module per group. Each word is `fn(&mut Engine) -> Result<(), ErrorKind>`
//! built on the engine's stack-machine API (`pop`/`push`/`pop_num`/… and the
//! indexed shuffles); the words never touch the stack `Vec` directly, so the
//! machine and the vocabulary stay decoupled. Every module exposes a
//! `PRIMITIVES` table of its rows, and those that bind values a `constants`;
//! [`primitives`] and [`constants`] chain them for the prelude.
//!
//! **The grouping is by type — the type a word is *about***, not by the kind of
//! operation it performs ([`number`], [`mod@bool`], [`list`]). Three modules are about
//! the machine rather than any value type and stay grouped by role: [`stack`],
//! [`mod@env`], [`control`]. A word that dispatches on its operand's type belongs to
//! neither and lives in [`generic`].
//!
//! That axis is chosen to match where this is headed. V5 gives each type an
//! **attribute table** (`lst.length`), so a type's module becomes the home of
//! both its free words and its attributes, and [`generic`] is precisely the set
//! that migrates into those tables. V6 then moves the derived words — the
//! shuffles beyond the primitive core, the combinators — out of Rust entirely
//! into an in-language prelude, leaving only the true primitives here.

use super::{Primitive, Value};

mod bool;
mod control;
mod env;
mod generic;
mod list;
mod number;
mod stack;

/// Every primitive across the group tables, in order — the source the prelude
/// binds into the global frame. Adding a module is one row.
pub(crate) fn primitives() -> impl Iterator<Item = &'static Primitive> {
    [
        number::PRIMITIVES,
        bool::PRIMITIVES,
        control::PRIMITIVES,
        stack::PRIMITIVES,
        list::PRIMITIVES,
        generic::PRIMITIVES,
        env::PRIMITIVES,
    ]
    .into_iter()
    .flatten()
}

/// The prelude's non-primitive bindings — values rather than operations:
/// [`mod@bool`]'s `true`/`false` and [`number`]'s `pi`/`e`/`tau`.
pub(crate) fn constants() -> impl Iterator<Item = (&'static str, Value)> {
    bool::constants().chain(number::constants())
}
