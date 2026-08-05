//! The program representation and the tokenizer. A program is a flat
//! `Vec<`[`Element`]`>` — a literal to push or a word to resolve — with no AST,
//! since RPN has no nesting. [`parse`] turns a line of text into one; the
//! primitive ops it resolves against live in the parent module.

use std::rc::Rc;

use super::{ErrorKind, Value};

/// A program element: a literal to push, or a word to resolve at runtime
/// (language.md §12 — "a word reference or a literal"). `parse` produces a flat
/// `Vec<Element>` — no AST, since RPN has no nesting — and a function body will
/// be one too. This is the *only* thing a program contains; the primitive ops
/// ([`Primitive`](super::Primitive)) are reached only by resolving a `Word`.
#[derive(Debug, Clone, PartialEq)]
pub enum Element {
    /// A literal value: a number, string, name, or boolean.
    Literal(Value),
    /// A bare word, resolved against the environment at runtime: a user binding
    /// (which shadows), else a builtin from the prelude, else `UnboundName`.
    Word(Rc<str>),
}

impl Element {
    /// Parse one whitespace-delimited token into an `Element`. A number, a
    /// `'x` name, or `true`/`false` becomes a `Literal`; every other token is a
    /// [`Element::Word`], resolved against the environment at runtime. So parsing
    /// never fails on an unknown word — that's a runtime `UnboundName`.
    pub fn parse(token: &str) -> Element {
        // The `'` sigil: `'x` pushes the name `x` (§3). Owned here rather than
        // as a builtin word so it can't be shadowed.
        if let Some(name) = token.strip_prefix('\'') {
            return Element::Literal(Value::Name(Rc::from(name)));
        }
        // Boolean literals — like numbers, they're literals, not words.
        match token {
            "true" => return Element::Literal(Value::Bool(true)),
            "false" => return Element::Literal(Value::Bool(false)),
            _ => {}
        }
        // Integer first, then float: `3` is an `Int`, but `3.0`/`2e3`/`1e-2`
        // (anything with a `.`, exponent, or out of i64 range) is a `Num`.
        if let Ok(i) = token.parse::<i64>() {
            return Element::Literal(Value::Int(i));
        }
        if let Ok(n) = token.parse::<f64>() {
            return Element::Literal(Value::Num(n));
        }
        Element::Word(Rc::from(token))
    }
}

impl std::fmt::Display for Element {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Element::Literal(v) => write!(f, "{v}"),
            Element::Word(name) => write!(f, "{name}"),
        }
    }
}

/// Parse a line into a program (a `Vec<Element>`), or fail on an unterminated
/// string — the one lexical error. An unknown *word* is not a parse error; it
/// becomes an `Element::Word` and fails (if unbound) at runtime.
///
/// Mostly a whitespace split, but with the §4 lookahead: a `"` opens a string
/// literal that runs (across spaces) to its closing `"`, so strings are the one
/// thing [`Element::parse`] never sees — the tokenizer owns them. Every other
/// token is handed to [`Element::parse`] word-for-word.
pub fn parse(input: &str) -> Result<Vec<Element>, ErrorKind> {
    let mut program = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' {
            program.push(Element::Literal(Value::Str(Rc::new(read_string(&mut chars)?))));
        } else {
            // A plain word: everything up to the next whitespace.
            let mut word = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                word.push(c);
                chars.next();
            }
            program.push(Element::parse(&word));
        }
    }
    Ok(program)
}

/// Read a `"…"` literal, the opening quote still unconsumed. Supports the
/// escapes `\"`, `\\`, `\n`, `\t`; an unknown escape keeps both characters
/// verbatim. Fails with [`ErrorKind::UnterminatedString`] at end-of-input.
fn read_string(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<String, ErrorKind> {
    chars.next(); // opening quote
    let mut s = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Ok(s),
            '\\' => match chars.next() {
                Some('"') => s.push('"'),
                Some('\\') => s.push('\\'),
                Some('n') => s.push('\n'),
                Some('t') => s.push('\t'),
                Some(other) => {
                    s.push('\\');
                    s.push(other);
                }
                None => return Err(ErrorKind::UnterminatedString),
            },
            _ => s.push(c),
        }
    }
    Err(ErrorKind::UnterminatedString)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_maps_tokens_to_elements() {
        // Numbers and `'x` names become a `Literal`; every other token is a
        // `Word`, resolved at runtime (so `+`/`dup` and unknown `nope` alike).
        assert_eq!(Element::parse("3.5"), Element::Literal(Value::Num(3.5)));
        assert_eq!(Element::parse("+"), Element::Word(Rc::from("+")));
        assert_eq!(Element::parse("dup"), Element::Word(Rc::from("dup")));
        assert_eq!(Element::parse("nope"), Element::Word(Rc::from("nope")));
    }

    #[test]
    fn parse_produces_a_program() {
        assert_eq!(
            parse("1 2 +"),
            Ok(vec![
                Element::Literal(Value::Int(1)),
                Element::Literal(Value::Int(2)),
                Element::Word(Rc::from("+")),
            ])
        );
    }

    #[test]
    fn an_unterminated_string_is_a_parse_error() {
        assert_eq!(parse(r#""oops"#), Err(ErrorKind::UnterminatedString));
        assert_eq!(parse(r#""bad \"#), Err(ErrorKind::UnterminatedString));
    }
}
