//! The **generic** words: those that dispatch on the type of their operand
//! rather than belonging to one type. `==` compares any two values, `to_str` is
//! total over all of them, and `length`/`nth` are the sequence protocol — today
//! lists, plus strings for `length`; later strings for both, and dicts and
//! ranges besides.
//!
//! This is exactly the set V5 promotes to **per-type attribute tables**, at
//! which point a free word here becomes a dot on its receiver (`lst.length`) and
//! the dispatch moves into the table, with `'length {.length} =` falling out
//! (`direction-v2.md` V5, `language-v2.md` §7). Keeping them together now is
//! what makes that a move rather than a re-cut.
//!
//! `length` and `nth` stay paired for the same reason `each` is defined over
//! them and not over `first`/`rest`: `rest` clones the list each step, which
//! makes the cons-style definition quadratic (`direction-v2.md` V6). They are
//! one protocol, not two list words.

use std::rc::Rc;

use crate::engine::{Engine, ErrorKind, Primitive, Value};

#[rustfmt::skip]
pub(super) static PRIMITIVES: &[Primitive] = &[
    Primitive { name: "==",     run: eq },       // a b -- bool
    Primitive { name: "length", run: length },   // string/list -- count
    Primitive { name: "nth",    run: nth },      // [a b c] i -- x, 0-based
    Primitive { name: "to_str", run: to_str },   // a -- string
];

/// `==`: equality of the top two values. Numbers compare by value across the
/// int/float split, so `2 2.0 ==` is true; anything else is structural equality.
fn eq(e: &mut Engine) -> Result<(), ErrorKind> {
    let b = e.pop()?;
    let a = e.pop()?;
    let equal = match (a.as_num(), b.as_num()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    e.push(Value::Bool(equal));
    Ok(())
}

/// `length`: the element count of the top string (characters) or list.
fn length(e: &mut Engine) -> Result<(), ErrorKind> {
    let len = match e.pop()? {
        Value::Str(s) => s.chars().count() as i64,
        Value::List(items) => items.len() as i64,
        other => {
            return Err(ErrorKind::TypeError {
                expected: "string or list",
                found: other.type_name(),
            })
        }
    };
    e.push(Value::Int(len));
    Ok(())
}

/// `nth` ( [a b c] i -- x ): the 0-based `i`th element. List indexing is 0-based
/// (other-language convention), unlike the 1-based `dup-at`/`rot-to` — so the
/// index is type-checked by [`Value::as_int`](crate::engine::Value) but *not*
/// clamped the way a stack level is: a negative is out of range, not level 0.
fn nth(e: &mut Engine) -> Result<(), ErrorKind> {
    let idx = usize::try_from(e.pop()?.as_int()?).map_err(|_| ErrorKind::IndexOutOfRange)?;
    let item = e
        .pop_list()?
        .get(idx)
        .cloned()
        .ok_or(ErrorKind::IndexOutOfRange)?;
    e.push(item);
    Ok(())
}

/// `to_str`: replace the top value with its string content (no quotes). Total —
/// every value has a string form.
fn to_str(e: &mut Engine) -> Result<(), ErrorKind> {
    let s = e.pop()?.content_string();
    e.push(Value::Str(Rc::new(s)));
    Ok(())
}
