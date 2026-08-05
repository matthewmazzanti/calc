//! The builtin **word vocabulary** — free functions over a `&mut Engine`, one
//! module per category. Each word is `fn(&mut Engine) -> Result<(), ErrorKind>`
//! built on the engine's stack-machine API (`pop`/`push`/`pop_num`/… and the
//! indexed shuffles); the words never touch the stack `Vec` directly, so the
//! machine and the vocabulary stay decoupled. Every module exposes a
//! `PRIMITIVES` table of its rows; [`primitives`] chains them for the prelude.
//!
//! The split by category (arith, compare, logic, stack, seq, env) is also where
//! this heads next: when functions land, the derived words leave these Rust
//! tables for an in-language prelude, and only the true primitives remain.

use super::Primitive;

mod arith;
mod compare;
mod env;
mod logic;
mod seq;
mod stack;

// The words the TUI dispatches directly (its operator keys and the empty-Enter
// `dup`), re-exported to the engine root so `crate::engine::ADD` resolves.
pub(crate) use arith::{ADD, DIV, MUL, SUB};
pub(crate) use stack::DUP;

/// Every primitive across the category tables, in order — the source the prelude
/// binds into the base frame.
pub(crate) fn primitives() -> impl Iterator<Item = &'static Primitive> {
    arith::PRIMITIVES
        .iter()
        .chain(compare::PRIMITIVES)
        .chain(logic::PRIMITIVES)
        .chain(stack::PRIMITIVES)
        .chain(seq::PRIMITIVES)
        .chain(env::PRIMITIVES)
}
