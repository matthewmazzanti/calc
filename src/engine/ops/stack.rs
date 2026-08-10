//! Stack-shuffle words. The fixed shuffles are thin wrappers over the machine's
//! indexed surgery (`pick_at`/`drop_at`/`swap_at`/`roll_at`/`rolld_at`); the
//! `n`-suffixed variants pop their level first. `tuck`/`dupd`/`2dup`/`2drop`
//! are pure pop/push rearrangements. `clear` is the machine's stack reset.

use crate::engine::{Engine, ErrorKind, Primitive};

#[rustfmt::skip]
pub(super) static PRIMITIVES: &[Primitive] = &[
    Primitive { name: "dup",    run: |e| e.pick_at(1) },   // a -- a a
    Primitive { name: "drop",   run: |e| e.drop_at(1) },   // a --
    Primitive { name: "swap",   run: |e| e.swap_at(1) },   // a b -- b a
    Primitive { name: "over",   run: |e| e.pick_at(2) },   // a b -- a b a
    Primitive { name: "rot",    run: |e| e.roll_at(3) },   // a b c -- b c a
    Primitive { name: "unrot",  run: |e| e.rolld_at(3) },  // a b c -- c a b
    Primitive { name: "nip",    run: |e| e.drop_at(2) },   // a b -- b
    Primitive { name: "tuck",   run: tuck },               // a b -- b a b
    Primitive { name: "dupd",   run: dupd },               // a b -- a a b
    Primitive { name: "2dup",   run: two_dup },            // a b -- a b a b
    Primitive { name: "2drop",  run: two_drop },           // a b --
    // Indexed: the 1-based level is popped off the stack (`n rolln`).
    Primitive { name: "pickn",  run: |e| indexed(e, Engine::pick_at) },
    Primitive { name: "rolln",  run: |e| indexed(e, Engine::roll_at) },
    Primitive { name: "rolldn", run: |e| indexed(e, Engine::rolld_at) },
    Primitive { name: "dropn",  run: |e| indexed(e, Engine::drop_at) },
    Primitive { name: "swapn",  run: |e| indexed(e, Engine::swap_at) },
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
