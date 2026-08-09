//! List words: `first rest cons append`. The mutators copy-on-write through
//! `Rc::make_mut`, so a list shared by a `dup` or a binding is duplicated only
//! when one of the two writes to it.
//!
//! `length` and `nth` are *not* here. They read a list, but they are the generic
//! sequence protocol — the pair `each` is defined over, and the pair V5 turns
//! into per-type attributes — so they live with the [generic](super::generic)
//! words.
//!
//! `[` and `]` are not here either. They were prelude words under v1; in v2 they
//! are fixed parser elements, paired in the text and never looked up, so their
//! dispatch lives in `Engine::apply_one` (`language-v2.md` §§3–4). The mark
//! discipline they drive is unchanged.

use std::rc::Rc;

use crate::engine::{Engine, ErrorKind, Primitive, Value};

#[rustfmt::skip]
pub(super) static PRIMITIVES: &[Primitive] = &[
    Primitive { name: "first",  run: first },    // [a b c] -- a
    Primitive { name: "rest",   run: rest },     // [a b c] -- [b c]
    Primitive { name: "cons",   run: cons },     // x [b c] -- [x b c]
    Primitive { name: "append", run: append },   // [a b] [c d] -- [a b c d]
];

/// `first` ( [a b c] -- a ): the head of the top list; empty is out of range.
fn first(e: &mut Engine) -> Result<(), ErrorKind> {
    let head = e
        .pop_list()?
        .first()
        .cloned()
        .ok_or(ErrorKind::IndexOutOfRange)?;
    e.push(head);
    Ok(())
}

/// `rest` ( [a b c] -- [b c] ): the top list without its head; empty is out of
/// range.
fn rest(e: &mut Engine) -> Result<(), ErrorKind> {
    let mut items = e.pop_list()?;
    if items.is_empty() {
        return Err(ErrorKind::IndexOutOfRange);
    }
    Rc::make_mut(&mut items).remove(0);
    e.push(Value::List(items));
    Ok(())
}

/// `cons` ( x [b c] -- [x b c] ): prepend the element below to the top list.
fn cons(e: &mut Engine) -> Result<(), ErrorKind> {
    let mut items = e.pop_list()?;
    let x = e.pop()?;
    Rc::make_mut(&mut items).insert(0, x);
    e.push(Value::List(items));
    Ok(())
}

/// `append` ( [a b] [c d] -- [a b c d] ): concatenate two lists.
fn append(e: &mut Engine) -> Result<(), ErrorKind> {
    let b = e.pop_list()?;
    let mut a = e.pop_list()?;
    Rc::make_mut(&mut a).extend(b.iter().cloned());
    e.push(Value::List(a));
    Ok(())
}
