//! Sequence words over the two collection types — lists (`[ ] first rest cons
//! append nth`) and, where they overlap, strings (`length`, `to_str`). `[`
//! opens a collection via the mark discipline; `]` is the machine's mark scan.
//! The list mutators copy-on-write through `Rc::make_mut`.

use std::rc::Rc;

use crate::engine::{Engine, ErrorKind, MarkKind, Primitive, Value};

#[rustfmt::skip]
pub(super) static PRIMITIVES: &[Primitive] = &[
    Primitive { name: "[",      run: open_list },          // push a list mark
    Primitive { name: "]",      run: Engine::close_list },  // collect to the mark
    Primitive { name: "first",  run: first },               // [a b c] -- a
    Primitive { name: "rest",   run: rest },                // [a b c] -- [b c]
    Primitive { name: "cons",   run: cons },                // x [b c] -- [x b c]
    Primitive { name: "append", run: append },              // [a b] [c d] -- [a b c d]
    Primitive { name: "nth",    run: nth },                 // [a b c] i -- (0-based)
    Primitive { name: "length", run: length },              // string/list count
    Primitive { name: "to_str", run: to_str },
];

/// `[`: push a list mark, opening a collection (§13 mark discipline).
fn open_list(e: &mut Engine) -> Result<(), ErrorKind> {
    e.push(Value::Mark(MarkKind::List));
    Ok(())
}

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

/// `nth` ( [a b c] i -- x ): the 0-based `i`th element. List indexing is 0-based
/// (other-language convention), unlike the 1-based `pickn`/`rolln`.
fn nth(e: &mut Engine) -> Result<(), ErrorKind> {
    let idx = match e.pop()? {
        Value::Int(i) if i >= 0 => i as usize,
        Value::Int(_) => return Err(ErrorKind::IndexOutOfRange),
        Value::Num(_) => {
            return Err(ErrorKind::TypeError {
                expected: "integer",
                found: "float",
            })
        }
        other => {
            return Err(ErrorKind::TypeError {
                expected: "integer",
                found: other.type_name(),
            })
        }
    };
    let item = e
        .pop_list()?
        .get(idx)
        .cloned()
        .ok_or(ErrorKind::IndexOutOfRange)?;
    e.push(item);
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

/// `to_str`: replace the top value with its string content (no quotes). Total —
/// every value has a string form.
fn to_str(e: &mut Engine) -> Result<(), ErrorKind> {
    let s = e.pop()?.content_string();
    e.push(Value::Str(Rc::new(s)));
    Ok(())
}
