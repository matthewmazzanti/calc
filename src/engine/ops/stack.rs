//! Stack-shuffle words, and the level-indexed surgery they are all built on.
//!
//! This module *owns* the surgery — `dup_at`/`dup_to`/`drop_at`/`drop_to`/
//! `swap_at`/`swap_to`/`rot_to`/`unrot_to`, levels 1-based with level 1 the top
//! of stack. [`Engine`]
//! re-exposes what the TUI's cursor edits drive, so a caller outside the
//! vocabulary can ask for one directly; the definitions are here, with the words.
//!
//! **The indexed word is the general form; every fixed shuffle is one of them
//! with its level written in** — `drop` is level 1 of `drop-at`, `rot` is level 3
//! of `rot-to`. The table is grouped by family with the indexed forms at the head
//! of each, so a group reads as one operation and the names it answers to.
//!
//! Two ways to name a target: `-at` is the operation *positioned at* a level
//! (`over` is `2 dup-at`, `nip` is `2 drop-at`), `-to` is the one *spanning* the
//! top down to it (`dup2` is `2 dup-to`, `drop2` is `2 drop-to`). Where both
//! exist they coincide at the family's arity, and that is where its bare word
//! sits: `dup`/`drop` at 1, `swap` at 2, `rot` at 3 — though rot has no `-at`
//! at all, being a span operation already.
//!
//! Every word here is one of those, at a level or at a constant — with no
//! exceptions left. A "copy X and place it at Y" takes two indices rather than
//! one and so belongs to no family: `tuck` and `dupd` were exactly that, and are
//! gone, spelling out as `swap over` and `over swap`. `clear` left too, being a
//! decision a person makes rather than a step a program takes.
//!
//! This is the one word module that reaches into the stack `Vec`; the rest are
//! built on the machine's `pop`/`push` API, and stay that way. Holding the
//! surgery here is the point of the exception — it is the thing they are all
//! defined in terms of.

use crate::engine::{Engine, ErrorKind, Primitive};

/// The `Vec` index for a 1-based level, or `None` if the level is out of range.
/// Callers turn `None` into a `StackUnderflow`. A mark counts as an ordinary
/// level — the shuffles move and copy marks like any other value, so a
/// collection is not a sealed scope.
fn index_of_level(e: &Engine, level: usize) -> Option<usize> {
    if level == 0 {
        return None; // 0 is not a level: they are 1-based
    }
    e.stack.len().checked_sub(level) // and deeper than the stack is out of range
}

/// Copy the value **at** `level` to the top (`dup` = 1, `over` = 2, `dup-at`).
pub(in crate::engine) fn dup_at(e: &mut Engine, level: usize) -> Result<(), ErrorKind> {
    let i = index_of_level(e, level).ok_or(ErrorKind::StackUnderflow)?;
    let v = e.stack[i].clone();
    e.stack.push(v);
    Ok(())
}

/// Copy every value from the top down **to** `level` — levels `1..=level`, order
/// preserved (`dup` = 1, `dup2` = 2, `dup-to`).
///
/// The other half of the dup family: `dup-at` names one item by depth, `dup-to`
/// names a run by width. They agree at level 1, which is what lets a bare `dup`
/// belong to both. Derivable as `dup-at level` applied `level` times, but that
/// is `level` calls against one slice copy.
pub(in crate::engine) fn dup_to(e: &mut Engine, level: usize) -> Result<(), ErrorKind> {
    let start = index_of_level(e, level).ok_or(ErrorKind::StackUnderflow)?;
    e.stack.extend_from_within(start..);
    Ok(())
}

/// Remove the value **at** `level` (`drop` = 1, `nip` = 2, `drop-at`).
pub(in crate::engine) fn drop_at(e: &mut Engine, level: usize) -> Result<(), ErrorKind> {
    let i = index_of_level(e, level).ok_or(ErrorKind::StackUnderflow)?;
    e.stack.remove(i);
    Ok(())
}

/// Remove every value from the top down **to** `level` — levels `1..=level`
/// (`drop` = 1, `drop2` = 2, `drop-to`).
///
/// The same two ways of naming a target as the dup family, and the reason this
/// pair had to exist: `dropn` and `2drop` were *depth* and *width* under names
/// that gave no hint which, and Forth spells the width one `ndrop`. Now the word
/// says it — `2 drop-at` is `nip`, `2 drop-to` is `drop2`.
pub(in crate::engine) fn drop_to(e: &mut Engine, level: usize) -> Result<(), ErrorKind> {
    let start = index_of_level(e, level).ok_or(ErrorKind::StackUnderflow)?;
    e.stack.truncate(start);
    Ok(())
}

/// Exchange the value **at** `level` with the top, leaving everything between
/// them where it is — `a … b -- b … a`. `swap` = 2; level 1 is the identity.
///
/// Reaching for the top rather than for the neighbour below is what makes level
/// 1 the degenerate case instead of level `depth`: there is always a top, so
/// this cannot fail for any level the stack has.
pub(in crate::engine) fn swap_at(e: &mut Engine, level: usize) -> Result<(), ErrorKind> {
    let i = index_of_level(e, level).ok_or(ErrorKind::StackUnderflow)?;
    let top = e.stack.len() - 1; // a valid level implies a non-empty stack
    e.stack.swap(i, top);
    Ok(())
}

/// Reverse the values from the top down **to** `level` — `a b c -- c b a` at 3.
/// `swap` = 2; level 1 is the identity.
///
/// The width form, and it needs only `level` values where exchanging two blocks
/// of `level` would need twice that — so it is defined for every n, not just the
/// ones with room for a partner.
pub(in crate::engine) fn swap_to(e: &mut Engine, level: usize) -> Result<(), ErrorKind> {
    let start = index_of_level(e, level).ok_or(ErrorKind::StackUnderflow)?;
    e.stack[start..].reverse();
    Ok(())
}

/// Rotate the span from the top down **to** `level` upward, bringing level
/// `level` to the top (`rot` = 3, `rot-to`).
///
/// A span operation, which is why there is no `-at` half: moving a level to the
/// top while leaving the middle untouched is impossible — the displaced top has
/// to go somewhere, and the only place that disturbs nothing else is where the
/// level came from. That op exists and is called [`swap_at`].
pub(in crate::engine) fn rot_to(e: &mut Engine, level: usize) -> Result<(), ErrorKind> {
    let i = index_of_level(e, level).ok_or(ErrorKind::StackUnderflow)?;
    let v = e.stack.remove(i);
    e.stack.push(v);
    Ok(())
}

/// Rotate the same span downward, sending the top to `level` — the inverse of
/// [`rot_to`] (`unrot` = 3, `unrot-to`).
pub(in crate::engine) fn unrot_to(e: &mut Engine, level: usize) -> Result<(), ErrorKind> {
    let dest = index_of_level(e, level).ok_or(ErrorKind::StackUnderflow)?;
    // `dest` is where the top must land. Popping first leaves every index
    // ≤ dest unchanged (dest ≤ len - 1), so we can insert straight in.
    let v = e.stack.pop().expect("level ≥ 1 implies a non-empty stack");
    e.stack.insert(dest, v);
    Ok(())
}

#[rustfmt::skip]
pub(super) static PRIMITIVES: &[Primitive] = &[
    // Copy. `-at` takes the one item at a level, `-to` the whole run down to it,
    // and the shorthands say which by shape: traditional names for `-at`, a
    // numeric suffix for `-to`. NB `pick` is Factor's fixed level 3, *not*
    // Forth's indexed one — `2 pick` here pushes a 2 and then copies.
    Primitive { name: "dup-at", run: |e| indexed(e, dup_at) },
    Primitive { name: "dup-to", run: |e| indexed(e, dup_to) },
    Primitive { name: "dup",    run: |e| dup_at(e, 1) },   // a -- a a
    Primitive { name: "over",   run: |e| dup_at(e, 2) },   // a b -- a b a
    Primitive { name: "pick",   run: |e| dup_at(e, 3) },   // a b c -- a b c a
    Primitive { name: "dup2",   run: |e| dup_to(e, 2) },   // a b -- a b a b
    Primitive { name: "dup3",   run: |e| dup_to(e, 3) },   // a b c -- a b c a b c

    // Remove. No traditional name for `-at` at level 3, so none is invented.
    Primitive { name: "drop-at", run: |e| indexed(e, drop_at) },
    Primitive { name: "drop-to", run: |e| indexed(e, drop_to) },
    Primitive { name: "drop",   run: |e| drop_at(e, 1) },  // a --
    Primitive { name: "nip",    run: |e| drop_at(e, 2) },  // a b -- b
    Primitive { name: "drop2",  run: |e| drop_to(e, 2) },  // a b --
    Primitive { name: "drop3",  run: |e| drop_to(e, 3) },  // a b c --

    // Exchange, both self-inverse so there is no `un` half. The bare word sits
    // at 2, not 1 — a family's shorthand lands on its arity, and level 1 here is
    // the identity. `-at` moves just the two ends, `-to` reverses the span.
    Primitive { name: "swap-at", run: |e| indexed(e, swap_at) },
    Primitive { name: "swap-to", run: |e| indexed(e, swap_to) },
    Primitive { name: "swap",   run: |e| swap_at(e, 2) },  // a b -- b a
    Primitive { name: "swap3",  run: |e| swap_to(e, 3) },  // a b c -- c b a

    // Rotate the span, either way. `-to` only: the ends-only form of "bring a
    // level to the top" is `swap-at`, so there is no `rot-at` to write.
    Primitive { name: "rot-to", run: |e| indexed(e, rot_to) },
    Primitive { name: "unrot-to", run: |e| indexed(e, unrot_to) },
    Primitive { name: "rot",    run: |e| rot_to(e, 3) },   // a b c -- b c a
    Primitive { name: "unrot",  run: |e| unrot_to(e, 3) }, // a b c -- c a b
];

/// Run an indexed shuffle with its 1-based level popped off the stack.
fn indexed(
    e: &mut Engine,
    op: impl FnOnce(&mut Engine, usize) -> Result<(), ErrorKind>,
) -> Result<(), ErrorKind> {
    let level = e.pop()?.as_index()?;
    op(e, level)
}
