//! Stack-shuffle words, and the level-indexed surgery they are all built on.
//!
//! This module *owns* `pick_at`/`drop_at`/`swap_at`/`roll_at`/`rolld_at` —
//! levels are 1-based, level 1 is the top of stack. [`Engine`] re-exposes the
//! five as methods so a caller outside the vocabulary (the TUI's cursor edits)
//! can ask for one directly; the definitions are here, with the words.
//!
//! The fixed shuffles are the ones at a constant level — `drop` is `dropn 1`,
//! `rot` is `rolln 3` — and the `n`-suffixed variants pop their level off the
//! stack first. `tuck`/`dupd`/`2dup`/`2drop` are pure pop/push rearrangements.
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

/// Copy the value at `level` to the top (`dup` = 1, `over` = 2, `pickn`).
pub(in crate::engine) fn pick_at(e: &mut Engine, level: usize) -> Result<(), ErrorKind> {
    let i = index_of_level(e, level).ok_or(ErrorKind::StackUnderflow)?;
    let v = e.stack[i].clone();
    e.stack.push(v);
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
    Primitive { name: "dup",    run: |e| pick_at(e, 1) },  // a -- a a
    Primitive { name: "drop",   run: |e| drop_at(e, 1) },  // a --
    Primitive { name: "swap",   run: |e| swap_at(e, 1) },  // a b -- b a
    Primitive { name: "over",   run: |e| pick_at(e, 2) },  // a b -- a b a
    Primitive { name: "rot",    run: |e| roll_at(e, 3) },  // a b c -- b c a
    Primitive { name: "unrot",  run: |e| rolld_at(e, 3) }, // a b c -- c a b
    Primitive { name: "nip",    run: |e| drop_at(e, 2) },  // a b -- b
    Primitive { name: "tuck",   run: tuck },               // a b -- b a b
    Primitive { name: "dupd",   run: dupd },               // a b -- a a b
    Primitive { name: "2dup",   run: two_dup },            // a b -- a b a b
    Primitive { name: "2drop",  run: two_drop },           // a b --
    // Indexed: the 1-based level is popped off the stack (`n rolln`).
    Primitive { name: "pickn",  run: |e| indexed(e, pick_at) },
    Primitive { name: "rolln",  run: |e| indexed(e, roll_at) },
    Primitive { name: "rolldn", run: |e| indexed(e, rolld_at) },
    Primitive { name: "dropn",  run: |e| indexed(e, drop_at) },
    Primitive { name: "swapn",  run: |e| indexed(e, swap_at) },
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

/// `2dup` ( a b -- a b a b ): copy the top two, order preserved.
fn two_dup(e: &mut Engine) -> Result<(), ErrorKind> {
    let b = e.pop()?;
    let a = e.pop()?;
    e.push(a.clone());
    e.push(b.clone());
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
