//! Stack-shuffle words, and the level-indexed surgery they are all built on.
//!
//! This module *owns* the surgery — `dup_at`/`dup_to`/`drop_at`/`swap_at`/
//! `roll_at`/`rolld_at`, levels 1-based with level 1 the top of stack. [`Engine`]
//! re-exposes what the TUI's cursor edits drive, so a caller outside the
//! vocabulary can ask for one directly; the definitions are here, with the words.
//!
//! **The indexed word is the general form; every fixed shuffle is one of them
//! with its level written in** — `drop` is level 1 of `dropn`, `rot` is level 3
//! of `rolln`. The table is grouped by family with the indexed forms at the head
//! of each, so a group reads as one operation and the names it answers to.
//!
//! Two ways to name a target, and `dup` has both: `-at` takes the single item
//! *at* a level (`over` is `2 dup-at`), `-to` takes the whole run from the top
//! down *to* it (`dup2` is `2 dup-to`). They coincide at level 1, which is why a
//! bare `dup` needs no qualifier.
//!
//! `tuck`/`dupd`/`2drop` are pop/push rearrangements no index names, and
//! `clear` is the machine's stack reset.
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
    let len = e.stack.len();
    (1..=len).contains(&level).then(|| len - level)
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

/// Remove the value at `level` (`drop` = 1, `nip` = 2, `dropn`).
pub(in crate::engine) fn drop_at(e: &mut Engine, level: usize) -> Result<(), ErrorKind> {
    let i = index_of_level(e, level).ok_or(ErrorKind::StackUnderflow)?;
    e.stack.remove(i);
    Ok(())
}

/// Exchange the value at `level` with the one just below it. `swap` = 1.
pub(in crate::engine) fn swap_at(e: &mut Engine, level: usize) -> Result<(), ErrorKind> {
    let i = index_of_level(e, level).ok_or(ErrorKind::StackUnderflow)?;
    let j = index_of_level(e, level + 1).ok_or(ErrorKind::StackUnderflow)?;
    e.stack.swap(i, j);
    Ok(())
}

/// Move the value at `level` up to the top. `rot` = 3, `rolln`.
pub(in crate::engine) fn roll_at(e: &mut Engine, level: usize) -> Result<(), ErrorKind> {
    let i = index_of_level(e, level).ok_or(ErrorKind::StackUnderflow)?;
    let v = e.stack.remove(i);
    e.stack.push(v);
    Ok(())
}

/// Move the top value down to `level` — the inverse of [`roll_at`].
/// `unrot` = 3, `rolldn`.
pub(in crate::engine) fn rolld_at(e: &mut Engine, level: usize) -> Result<(), ErrorKind> {
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

    // Remove. `2drop` is the width form the dup family spells `dup-to`, still
    // hardcoded because there is no `drop-to` yet.
    Primitive { name: "dropn",  run: |e| indexed(e, drop_at) },
    Primitive { name: "drop",   run: |e| drop_at(e, 1) },  // a --
    Primitive { name: "nip",    run: |e| drop_at(e, 2) },  // a b -- b
    Primitive { name: "2drop",  run: two_drop },           // a b --

    // Exchange. Self-inverse, so there is no `un` half.
    Primitive { name: "swapn",  run: |e| indexed(e, swap_at) },
    Primitive { name: "swap",   run: |e| swap_at(e, 1) },  // a b -- b a

    // Move, and its inverse. No bare form: both are identity at level 1, which
    // is why the shorthands sit at 3.
    Primitive { name: "rolln",  run: |e| indexed(e, roll_at) },
    Primitive { name: "rolldn", run: |e| indexed(e, rolld_at) },
    Primitive { name: "rot",    run: |e| roll_at(e, 3) },  // a b c -- b c a
    Primitive { name: "unrot",  run: |e| rolld_at(e, 3) }, // a b c -- c a b

    // Rearrangements no index names, and the machine's stack reset.
    Primitive { name: "tuck",   run: tuck },               // a b -- b a b
    Primitive { name: "dupd",   run: dupd },               // a b -- a a b
    Primitive { name: "clear",  run: Engine::clear },
];

/// `tuck` ( a b -- b a b ): tuck a copy of the top below the second.
fn tuck(e: &mut Engine) -> Result<(), ErrorKind> {
    let b = e.pop()?;
    let a = e.pop()?;
    e.push(b.clone());
    e.push(a);
    e.push(b);
    Ok(())
}

/// `dupd` ( a b -- a a b ): duplicate the second element.
fn dupd(e: &mut Engine) -> Result<(), ErrorKind> {
    let b = e.pop()?;
    let a = e.pop()?;
    e.push(a.clone());
    e.push(a);
    e.push(b);
    Ok(())
}

/// `2drop` ( a b -- ): drop the top two.
fn two_drop(e: &mut Engine) -> Result<(), ErrorKind> {
    e.pop()?;
    e.pop()?;
    Ok(())
}

/// Run an indexed shuffle with its 1-based level popped off the stack.
fn indexed(
    e: &mut Engine,
    op: impl FnOnce(&mut Engine, usize) -> Result<(), ErrorKind>,
) -> Result<(), ErrorKind> {
    let level = e.pop()?.as_index()?;
    op(e, level)
}
