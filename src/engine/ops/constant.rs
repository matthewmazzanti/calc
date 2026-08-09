//! Constant bindings — prelude entries that are *values* rather than
//! [`Primitive`](crate::engine::Primitive)s.
//!
//! `true` and `false` live here rather than in the parser, which is what makes
//! them ordinary names: they can be fetched (`&true`), shadowed, and
//! un-shadowed with `del` like any other builtin (`language-v2.md` §9), and
//! `'true` and `{true: …}` are legal because nothing about them is syntax.
//!
//! This costs a frame-chain walk per `true` — §1's "a value in the environment
//! is a function too, a nullary one that consumes nothing and pushes something,"
//! which §11 already accepts for every name. What it buys is that **the language
//! has no keywords**: every token is a literal shape, a fixed character, or a
//! name. `pi`, `e`, and `tau` will land here for the same reason.

use crate::engine::Value;

/// Every constant the prelude binds, as `(word, value)`. Not a `static`, since a
/// [`Value`] holds `Rc`s and so isn't `Sync`.
pub(super) fn constants() -> impl Iterator<Item = (&'static str, Value)> {
    [("true", Value::Bool(true)), ("false", Value::Bool(false))].into_iter()
}
