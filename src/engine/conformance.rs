//! The lexical surface as one table — every rule in `language-v2.md` §3 shown
//! working, as source → tokens → program.
//!
//! This is a **golden test**: the expected columns are the specification made
//! executable, and their *diff* is the review surface when a rule changes. A
//! grammar change should be visible here as a handful of rows, reviewed
//! deliberately, rather than discovered later. Adding a numeric literal shape
//! (rationals, hex, complex) will move rows out of [`words`] — which is exactly
//! the question "which names does this take?", answered by rerunning this.
//!
//! Both columns are typed values, not rendered text: a row states the tokens and
//! the elements themselves, so nothing here can pass or fail on formatting. The
//! [`tok`] and [`el`] constructors keep the two phases visually distinct in a
//! row, since their vocabularies deliberately mirror each other.
//!
//! Behavior lives in `tests.rs`; this file is about *shape*.

use std::rc::Rc;

use super::token::{tokenize, Bracket, TokenKind};
use super::{parse, Element, ParseError, ParseErrorKind, Region, Span, Value};

/// One row of the surface: a line of source, the tokens it lexes to, and the
/// program it parses to. Either phase may fail, and which one does is itself
/// part of the specification — a dangling sigil is the tokenizer's error, an
/// unpaired bracket the parser's.
struct Case {
    source: &'static str,
    tokens: Result<Vec<TokenKind>, ParseError>,
    program: Result<Vec<Element>, ParseError>,
}

/// The expected failure of either phase: a kind and the span it blames.
fn fails<T>(kind: ParseErrorKind, start: usize, end: usize) -> Result<T, ParseError> {
    Err(ParseError::new(kind, Span::new(start, end)))
}

/// Token constructors — the tokenizer's vocabulary.
mod tok {
    use super::*;

    pub fn word(text: &str) -> TokenKind {
        TokenKind::Word(Rc::from(text))
    }
    pub fn int(value: i64) -> TokenKind {
        TokenKind::Number(Value::Int(value))
    }
    pub fn float(value: f64) -> TokenKind {
        TokenKind::Number(Value::Num(value))
    }
    pub fn string(text: &str) -> TokenKind {
        TokenKind::Str(Rc::new(text.to_string()))
    }
    pub fn name(text: &str) -> TokenKind {
        TokenKind::Name(Rc::from(text))
    }
    pub fn attr(text: &str) -> TokenKind {
        TokenKind::Attr(Rc::from(text))
    }
    pub fn open(bracket: Bracket) -> TokenKind {
        TokenKind::Open(bracket)
    }
    pub fn close(bracket: Bracket) -> TokenKind {
        TokenKind::Close(bracket)
    }
}

/// Element constructors — the parser's vocabulary, deliberately parallel to
/// [`tok`] so a row reads as the mapping it is.
mod el {
    use super::*;

    pub fn word(text: &str) -> Element {
        Element::Word(Rc::from(text))
    }
    pub fn int(value: i64) -> Element {
        Element::Literal(Value::Int(value))
    }
    pub fn float(value: f64) -> Element {
        Element::Literal(Value::Num(value))
    }
    pub fn string(text: &str) -> Element {
        Element::Literal(Value::Str(Rc::new(text.to_string())))
    }
    pub fn name(text: &str) -> Element {
        Element::Literal(Value::Name(Rc::from(text)))
    }
    pub fn attr(text: &str) -> Element {
        Element::Attr(Rc::from(text))
    }
    pub fn bind(text: &str) -> Element {
        Element::Bind(Rc::from(text))
    }
    pub fn template(body: Vec<Element>) -> Element {
        Element::Template(body.into())
    }
    pub fn open(region: Region) -> Element {
        Element::Open(region)
    }
    pub fn close(region: Region) -> Element {
        Element::Close(region)
    }
}

/// Run every row, reporting only what moved — a table diff, not a wall of text.
/// Both columns are checked for every row, since which phase answers is part of
/// what the row specifies.
fn check(cases: Vec<Case>) {
    let mut moved = String::new();
    for case in cases {
        let tokens = tokenize(case.source).map(|tokens| {
            tokens
                .into_iter()
                .map(|token| token.kind)
                .collect::<Vec<_>>()
        });
        if tokens != case.tokens {
            moved.push_str(&report(case.source, "tokens", &case.tokens, &tokens));
        }
        let program = parse(case.source);
        if program != case.program {
            moved.push_str(&report(case.source, "program", &case.program, &program));
        }
    }
    assert!(
        moved.is_empty(),
        "the lexical surface moved — review each changed row:\n{moved}"
    );
}

fn report<T: std::fmt::Debug>(
    source: &str,
    column: &str,
    expected: &Result<T, ParseError>,
    actual: &Result<T, ParseError>,
) -> String {
    format!("\n  {source:?} {column}\n    expected: {expected:?}\n      actual: {actual:?}\n")
}

#[test]
fn numbers() {
    check(vec![
        Case {
            source: "3",
            tokens: Ok(vec![tok::int(3)]),
            program: Ok(vec![el::int(3)]),
        },
        Case {
            source: "-5",
            tokens: Ok(vec![tok::int(-5)]),
            program: Ok(vec![el::int(-5)]),
        },
        Case {
            source: "0.1",
            tokens: Ok(vec![tok::float(0.1)]),
            program: Ok(vec![el::float(0.1)]),
        },
        // A fraction needs digits after the dot, not before it.
        Case {
            source: ".1",
            tokens: Ok(vec![tok::float(0.1)]),
            program: Ok(vec![el::float(0.1)]),
        },
        Case {
            source: "-.5",
            tokens: Ok(vec![tok::float(-0.5)]),
            program: Ok(vec![el::float(-0.5)]),
        },
        Case {
            source: "2e3",
            tokens: Ok(vec![tok::float(2000.0)]),
            program: Ok(vec![el::float(2000.0)]),
        },
        Case {
            source: "-3.5e-2",
            tokens: Ok(vec![tok::float(-0.035)]),
            program: Ok(vec![el::float(-0.035)]),
        },
        // Integer shape that overflows `i64` falls back to a float.
        Case {
            source: "99999999999999999999",
            tokens: Ok(vec![tok::float(1e20)]),
            program: Ok(vec![el::float(1e20)]),
        },
    ]);
}

#[test]
fn words() {
    // Everything the number grammar left. Rows leaving this group are how a new
    // literal shape announces which names it takes.
    let names = [
        "dup", "+", "2dup", "bi*", "->", "true", "inf", "nan", "1/2", "0x1f", "1_000", "e", "1e",
        "x'", "a&b", "don't",
    ];
    check(
        names
            .iter()
            .map(|&source| Case {
                source,
                tokens: Ok(vec![tok::word(source)]),
                program: Ok(vec![el::word(source)]),
            })
            .collect(),
    );
}

#[test]
fn adjacency() {
    check(vec![
        Case {
            source: "'f",
            tokens: Ok(vec![tok::name("f")]),
            program: Ok(vec![el::name("f")]),
        },
        // A token begins after a delimiter, not only after whitespace.
        Case {
            source: "['f]",
            tokens: Ok(vec![
                tok::open(Bracket::Square),
                tok::name("f"),
                tok::close(Bracket::Square),
            ]),
            program: Ok(vec![
                el::open(Region::List),
                el::name("f"),
                el::close(Region::List),
            ]),
        },
        Case {
            source: "obj.x",
            tokens: Ok(vec![tok::word("obj"), tok::attr("x")]),
            program: Ok(vec![el::word("obj"), el::attr("x")]),
        },
        // `&` is an ordinary name character now, in every position — so a
        // dotted `.&x` is just the attribute named `&x`, and needs no rule.
        Case {
            source: "obj.&x",
            tokens: Ok(vec![tok::word("obj"), tok::attr("&x")]),
            program: Ok(vec![el::word("obj"), el::attr("&x")]),
        },
        Case {
            source: ".foo&bar",
            tokens: Ok(vec![tok::attr("foo&bar")]),
            program: Ok(vec![el::attr("foo&bar")]),
        },
        // An attribute name is a name, so it may lead with a digit.
        Case {
            source: "obj.2dup",
            tokens: Ok(vec![tok::word("obj"), tok::attr("2dup")]),
            program: Ok(vec![el::word("obj"), el::attr("2dup")]),
        },
        // Attribute is always the fallback, so `.map` reads anywhere.
        Case {
            source: "{.map}",
            tokens: Ok(vec![
                tok::open(Bracket::Brace),
                tok::attr("map"),
                tok::close(Bracket::Brace),
            ]),
            program: Ok(vec![el::template(vec![el::attr("map")])]),
        },
        // Detached, the same dot may open a number instead.
        Case {
            source: "obj .1",
            tokens: Ok(vec![tok::word("obj"), tok::float(0.1)]),
            program: Ok(vec![el::word("obj"), el::float(0.1)]),
        },
        Case {
            source: "3.5.x",
            tokens: Ok(vec![tok::float(3.5), tok::attr("x")]),
            program: Ok(vec![el::float(3.5), el::attr("x")]),
        },
        // The standalone characters bunch up against whatever they abut.
        Case {
            source: "word[word",
            tokens: Ok(vec![
                tok::word("word"),
                tok::open(Bracket::Square),
                tok::word("word"),
            ]),
            program: fails(ParseErrorKind::UnclosedOpen('['), 4, 5),
        },
        // Parameters: a run of words ended by `:`, compiled to binds, read
        // bottom to top so the rightmost takes the top of the stack.
        Case {
            source: "{w h: w h *}",
            tokens: Ok(vec![
                tok::open(Bracket::Brace),
                tok::word("w"),
                tok::word("h"),
                TokenKind::Colon,
                tok::word("w"),
                tok::word("h"),
                tok::word("*"),
                tok::close(Bracket::Brace),
            ]),
            program: Ok(vec![el::template(vec![
                el::bind("h"),
                el::bind("w"),
                el::word("w"),
                el::word("h"),
                el::word("*"),
            ])]),
        },
        Case {
            source: "1 # c",
            tokens: Ok(vec![tok::int(1)]),
            program: Ok(vec![el::int(1)]),
        },
        Case {
            source: r#""a b""#,
            tokens: Ok(vec![tok::string("a b")]),
            program: Ok(vec![el::string("a b")]),
        },
    ]);
}

#[test]
fn errors() {
    check(vec![
        // A sigil with no name is the *tokenizer's* error — adjacency is its
        // rule — so neither phase produces anything.
        Case {
            source: "'3",
            tokens: fails(ParseErrorKind::ExpectedName { after: '\'' }, 0, 1),
            program: fails(ParseErrorKind::ExpectedName { after: '\'' }, 0, 1),
        },
        // But `&` is no sigil, so `&2e3` is simply a name — the number grammar
        // never gets a chance to claim a run it doesn't account for.
        Case {
            source: "&2e3",
            tokens: Ok(vec![tok::word("&2e3")]),
            program: Ok(vec![el::word("&2e3")]),
        },
        // Attached, so the dot is the attribute operator and `1` is no name —
        // not a silent `obj 0.1`. A list on the left reads the same way.
        Case {
            source: "obj.1",
            tokens: fails(ParseErrorKind::ExpectedName { after: '.' }, 3, 4),
            program: fails(ParseErrorKind::ExpectedName { after: '.' }, 3, 4),
        },
        Case {
            source: "[1 2].1",
            tokens: fails(ParseErrorKind::ExpectedName { after: '.' }, 5, 6),
            program: fails(ParseErrorKind::ExpectedName { after: '.' }, 5, 6),
        },
        // A sigil binds tightly or not at all.
        Case {
            source: "' x",
            tokens: fails(ParseErrorKind::ExpectedName { after: '\'' }, 0, 1),
            program: fails(ParseErrorKind::ExpectedName { after: '\'' }, 0, 1),
        },
        // The rest are the parser's: the tokens are fine, the structure isn't.
        Case {
            source: "{x 3: x}",
            tokens: Ok(vec![
                tok::open(Bracket::Brace),
                tok::word("x"),
                tok::int(3),
                TokenKind::Colon,
                tok::word("x"),
                tok::close(Bracket::Brace),
            ]),
            program: fails(ParseErrorKind::InvalidParameter, 3, 4),
        },
        Case {
            source: "]",
            tokens: Ok(vec![tok::close(Bracket::Square)]),
            program: fails(ParseErrorKind::UnmatchedClose(']'), 0, 1),
        },
        Case {
            source: "[1",
            tokens: Ok(vec![tok::open(Bracket::Square), tok::int(1)]),
            program: fails(ParseErrorKind::UnclosedOpen('['), 0, 1),
        },
        Case {
            source: "{[}]",
            tokens: Ok(vec![
                tok::open(Bracket::Brace),
                tok::open(Bracket::Square),
                tok::close(Bracket::Brace),
                tok::close(Bracket::Square),
            ]),
            program: fails(
                ParseErrorKind::CrossingClose {
                    closer: '}',
                    crossed: '[',
                },
                2,
                3,
            ),
        },
        Case {
            source: "x:",
            tokens: Ok(vec![tok::word("x"), TokenKind::Colon]),
            program: fails(ParseErrorKind::MisplacedColon, 1, 2),
        },
        Case {
            source: r#""oops"#,
            tokens: fails(ParseErrorKind::UnterminatedString, 0, 5),
            program: fails(ParseErrorKind::UnterminatedString, 0, 5),
        },
    ]);
}
