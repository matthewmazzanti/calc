//! The lexical surface as one table — every rule in `language-v2.md` §3 shown
//! working, on one page, as source → tokens → program.
//!
//! This is a **golden test**: the expected column is the specification made
//! executable, and its *diff* is the review surface when a rule changes. A
//! grammar change should be visible here as a handful of rows, reviewed
//! deliberately, rather than discovered later. Adding a numeric literal shape
//! (rationals, hex, complex) will move rows out of `WORDS` — which is exactly
//! the question "which names does this take?", answered by rerunning this.
//!
//! Behavior lives in `tests.rs`; this file is about *shape*.

use super::token::{tokenize, TokenKind};
use super::{parse, Element};

/// One token, rendered compactly enough to read a whole row.
fn show(kind: &TokenKind) -> String {
    match kind {
        TokenKind::Word(w) => format!("Word({w})"),
        TokenKind::Number(v) => format!("Number({v})"),
        TokenKind::Str(s) => format!("Str({s:?})"),
        TokenKind::Name(n) => format!("Name({n})"),
        TokenKind::Fetch(n) => format!("Fetch({n})"),
        TokenKind::Attr(n) => format!("Attr({n})"),
        TokenKind::AttrFetch(n) => format!("AttrFetch({n})"),
        TokenKind::Colon => ":".to_string(),
        TokenKind::Open(b) => b.open().to_string(),
        TokenKind::Close(b) => b.close().to_string(),
    }
}

/// `source | tokens | program`, or the error that stopped it. The program column
/// is `Element::Display`, so it is also what a trace or the info bar shows.
fn row(source: &str) -> String {
    let tokens = match tokenize(source) {
        Ok(tokens) => tokens
            .iter()
            .map(|t| show(&t.kind))
            .collect::<Vec<_>>()
            .join(" "),
        Err(error) => format!("error: {error}"),
    };
    let program = match parse(source) {
        Ok(program) => program
            .iter()
            .map(Element::to_string)
            .collect::<Vec<_>>()
            .join(" "),
        Err(error) => format!("error: {error} @{}", error.span.start),
    };
    format!("{source:<14} | {tokens:<44} | {program}")
}

/// Compare a group against its expected rows, reporting only the rows that
/// moved — a table diff, not a wall of escaped text.
fn check(cases: &[&str], expected: &str) {
    let actual: Vec<String> = cases.iter().map(|s| row(s)).collect();
    let expected: Vec<&str> = expected.trim().lines().map(str::trim_end).collect();
    let mut moved = String::new();
    for row in 0..actual.len().max(expected.len()) {
        let (was, now) = (
            expected.get(row).copied().unwrap_or(""),
            actual.get(row).map(String::as_str).unwrap_or(""),
        );
        if was != now {
            moved.push_str(&format!("\n  expected: {was}\n    actual: {now}\n"));
        }
    }
    assert!(
        moved.is_empty(),
        "the lexical surface moved — review each changed row:\n{moved}"
    );
}

#[test]
fn numbers() {
    check(
        &[
            "3", "-5", "0.1", ".1", "-.5", "3.5", "2e3", "1e-2", "-3.5e-2",
        ],
        r#"
3              | Number(3)                                    | 3
-5             | Number(-5)                                   | -5
0.1            | Number(0.1)                                  | 0.1
.1             | Number(0.1)                                  | 0.1
-.5            | Number(-0.5)                                 | -0.5
3.5            | Number(3.5)                                  | 3.5
2e3            | Number(2000)                                 | 2000
1e-2           | Number(0.01)                                 | 0.01
-3.5e-2        | Number(-0.035)                               | -0.035
"#,
    );
}

#[test]
fn words_are_whatever_the_number_grammar_left() {
    check(
        &[
            "dup", "+", "2dup", "bi*", "->", "true", "inf", "nan", "1/2", "0x1f", "1_000", "e",
            "1e", "x'", "a&b",
        ],
        r#"
dup            | Word(dup)                                    | dup
+              | Word(+)                                      | +
2dup           | Word(2dup)                                   | 2dup
bi*            | Word(bi*)                                    | bi*
->             | Word(->)                                     | ->
true           | Word(true)                                   | true
inf            | Word(inf)                                    | inf
nan            | Word(nan)                                    | nan
1/2            | Word(1/2)                                    | 1/2
0x1f           | Word(0x1f)                                   | 0x1f
1_000          | Word(1_000)                                  | 1_000
e              | Word(e)                                      | e
1e             | Word(1e)                                     | 1e
x'             | Word(x')                                     | x'
a&b            | Word(a&b)                                    | a&b
"#,
    );
}

#[test]
fn adjacency() {
    check(
        &[
            "'f",
            "&f",
            "[&f]",
            "obj.x",
            "obj.&x",
            "obj.2dup",
            ".foo&bar",
            ".map",
            "obj .1",
            "3.5.x",
            "word[word",
            "{w h: w h *}",
            "1 # c",
            r#""a b""#,
        ],
        r#"
'f             | Name(f)                                      | 'f
&f             | Fetch(f)                                     | &f
[&f]           | [ Fetch(f) ]                                 | [ &f ]
obj.x          | Word(obj) Attr(x)                            | obj .x
obj.&x         | Word(obj) AttrFetch(x)                       | obj .&x
obj.2dup       | Word(obj) Attr(2dup)                         | obj .2dup
.foo&bar       | Attr(foo&bar)                                | .foo&bar
.map           | Attr(map)                                    | .map
obj .1         | Word(obj) Number(0.1)                        | obj 0.1
3.5.x          | Number(3.5) Attr(x)                          | 3.5 .x
word[word      | Word(word) [ Word(word)                      | error: unclosed `[` @4
{w h: w h *}   | { Word(w) Word(h) : Word(w) Word(h) Word(*) } | {w h: w h *}
1 # c          | Number(1)                                    | 1
"a b"          | Str("a b")                                   | "a b"
"#,
    );
}

#[test]
fn errors() {
    check(
        &[
            "'3", "&2e3", "obj.1", "[1 2].1", "obj.", "' x", "'", "{x 3: x}", "]", "[1", "{[}]",
            "x:", r#""oops"#,
        ],
        r#"
'3             | error: expected a name after `'`             | error: expected a name after `'` @0
&2e3           | error: expected a name after `&`             | error: expected a name after `&` @0
obj.1          | error: expected a name after `.`             | error: expected a name after `.` @3
[1 2].1        | error: expected a name after `.`             | error: expected a name after `.` @5
obj.           | error: expected a name after `.`             | error: expected a name after `.` @3
' x            | error: expected a name after `'`             | error: expected a name after `'` @0
'              | error: expected a name after `'`             | error: expected a name after `'` @0
{x 3: x}       | { Word(x) Number(3) : Word(x) }              | error: not a name, so not a parameter @3
]              | ]                                            | error: unmatched `]` @0
[1             | [ Number(1)                                  | error: unclosed `[` @0
{[}]           | { [ } ]                                      | error: `}` crosses an open `[` @2
x:             | Word(x) :                                    | error: `:` is only valid after a template's parameter names @1
"oops          | error: unterminated string                   | error: unterminated string @0
"#,
    );
}
