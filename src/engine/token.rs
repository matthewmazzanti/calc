//! The tokenizer — the first of the three phases (`language-v2.md` §3):
//!
//! ```text
//! characters → [tokenize] → tokens → [parse] → tree → [evaluate] → values
//! ```
//!
//! This phase is a split, a character set, and one mode. String and `#`-comment
//! **lookahead runs first**, so a delimiter or sigil inside `"…"` or after `#` is
//! text. Then ten characters are **self-delimiting** — each its own token
//! whatever it abuts — so `[1 2 3]` tokenizes exactly like `[ 1 2 3 ]` and `&f`
//! like `& f`:
//!
//! ```text
//! '  &  .  :  {  }  [  ]  (  )
//! ```
//!
//! Everything else is a whitespace-delimited run, emitted as a
//! [`TokenKind::Word`] with **no interpretation** — whether a word is a number, a
//! boolean, or a name to resolve is the parser's call. The one exception, and the
//! only place a token's *shape* decides the split, is a `.` **between digits**:
//! `3.5` stays one word, while `obj.x` splits on the dot.
//!
//! The tokenizer owns the input: there is no input-stream register, so no word
//! can read ahead (`language-v2.md` §10).

use std::rc::Rc;

use super::{ParseError, ParseErrorKind, Value};

/// A byte range in the source line — `start..end`, as a `str` index. Carried by
/// every token and by every [`ParseError`], so a diagnostic can point at the
/// offending text rather than merely naming it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub(crate) fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// The source text this span covers — for a diagnostic that quotes it.
    pub fn of<'a>(&self, source: &'a str) -> &'a str {
        &source[self.start..self.end]
    }
}

/// One of the three bracket pairs. All three are matched by the parser and must
/// nest; what they *mean* differs by phase — `{ }` resolves at parse time into a
/// template, while `[ ]` and `( )` are fixed elements whose effect is runtime
/// (`language-v2.md` §§3, 6, 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bracket {
    /// `{ }` — a function template.
    Brace,
    /// `[ ]` — a list region.
    Square,
    /// `( )` — a dict region.
    Paren,
}

impl Bracket {
    /// The opening character, for diagnostics.
    pub(crate) fn open(self) -> char {
        match self {
            Bracket::Brace => '{',
            Bracket::Square => '[',
            Bracket::Paren => '(',
        }
    }

    /// The closing character, for diagnostics.
    pub(crate) fn close(self) -> char {
        match self {
            Bracket::Brace => '}',
            Bracket::Square => ']',
            Bracket::Paren => ')',
        }
    }
}

/// What a token is: a **literal**, a **word**, or one of the fixed characters.
/// Both literal kinds arrive decoded — a `"…"` with its escapes resolved and its
/// quotes gone, a number already a [`Value`] — since the tokenizer owns the
/// grammar of each.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    /// A name: `+`, `dup`, `2dup`, `bi*`, `foo`, `true`. Whatever the number
    /// grammar didn't claim — see [`number`] for why the definition is negative.
    Word(Rc<str>),
    /// A number literal, already an `Int` or a `Num`.
    Number(Value),
    /// A string literal's *content*, escapes resolved, quotes gone.
    Str(Rc<String>),
    /// `'` — the parser takes the next token as a name.
    Quote,
    /// `&` — the parser takes the next token as a fetch.
    Amp,
    /// `.` — attribute access, which stages the receiver (§7).
    Dot,
    /// `:` — closes a template's parameter list (§5).
    Colon,
    /// `{`, `[`, or `(`.
    Open(Bracket),
    /// `}`, `]`, or `)`.
    Close(Bracket),
}

/// A token and where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// The fixed character a token is, if it is one of the ten self-delimiting
/// characters. `"` and `#` are fixed too, but they open a *region* of text
/// (a string, a comment) rather than standing alone, so they're handled by the
/// lookahead in [`tokenize`] instead.
fn delimiter(c: char) -> Option<TokenKind> {
    Some(match c {
        '\'' => TokenKind::Quote,
        '&' => TokenKind::Amp,
        '.' => TokenKind::Dot,
        ':' => TokenKind::Colon,
        '{' => TokenKind::Open(Bracket::Brace),
        '}' => TokenKind::Close(Bracket::Brace),
        '[' => TokenKind::Open(Bracket::Square),
        ']' => TokenKind::Close(Bracket::Square),
        '(' => TokenKind::Open(Bracket::Paren),
        ')' => TokenKind::Close(Bracket::Paren),
        _ => return None,
    })
}

/// Whether `c` ends a word run: whitespace, one of the ten self-delimiting
/// characters, or the `"`/`#` that open a string or a comment. Equivalently:
/// the characters no name may contain.
fn breaks_word(c: char) -> bool {
    c.is_whitespace() || delimiter(c).is_some() || c == '"' || c == '#'
}

/// The number grammar, and the only statement of it:
///
/// ```text
/// number   = "-"? digit+ fraction? exponent?
/// fraction = "." digit+
/// exponent = ("e" | "E") ("+" | "-")? digit+
/// ```
///
/// Deliberately ours and deliberately small. Names are defined *negatively* —
/// a word is a run this grammar doesn't claim — which is forced by a vocabulary
/// holding `2dup`, `bi*`, and `+` (no positive identifier grammar admits all
/// three; Forth, Factor, and Common Lisp are negative for the same reason). The
/// cost of a negative definition is that every literal shape deletes names, so
/// the grammar must be one we chose: inheriting Rust's `f64` parser silently
/// claimed `inf`, `nan`, `Inf`, and `infinity`, none of which are numbers here.
///
/// Returns the end index of the number reaching from `start`, or `None` if none
/// starts there. Callers ask two things of it — "is this whole run a number?"
/// ([`number`]) and "does a number continue through this `.`?" ([`read_word`]).
fn scan_number(input: &str, start: usize) -> Option<usize> {
    let text = &input[start..];
    let mut at = usize::from(text.starts_with('-'));
    match digits_end(text, at) {
        end if end > at => at = end,
        _ => return None, // a number leads with digits, so `.5` and `-x` aren't
    }
    // A fraction needs digits *after* the dot — which is exactly what leaves
    // `obj.x` to split and `3.` a number followed by a dot.
    if text[at..].starts_with('.') {
        match digits_end(text, at + 1) {
            end if end > at + 1 => at = end,
            _ => return Some(start + at),
        }
    }
    if text[at..].starts_with(['e', 'E']) {
        let signed = at + 1 + usize::from(text[at + 1..].starts_with(['+', '-']));
        if digits_end(text, signed) > signed {
            at = digits_end(text, signed);
        }
    }
    Some(start + at)
}

/// The index just past the digits starting at `from` — `from` itself if none.
fn digits_end(text: &str, from: usize) -> usize {
    from + text[from..].bytes().take_while(u8::is_ascii_digit).count()
}

/// The value of `text` if the whole of it is a number. Integer shape (no `.`,
/// no exponent) is an `Int`, falling back to a `Num` when it overflows `i64`.
fn number(text: &str) -> Option<Value> {
    if scan_number(text, 0)? != text.len() {
        return None;
    }
    if !text.contains(['.', 'e', 'E']) {
        if let Ok(int) = text.parse::<i64>() {
            return Some(Value::Int(int));
        }
    }
    // The grammar is a subset of Rust's, so this converts rather than decides.
    Some(Value::Num(
        text.parse().expect("matched the number grammar"),
    ))
}

/// Split `input` into tokens, or fail on an unterminated string — the one error
/// this phase can raise. Nesting, resolution, and meaning all belong to the
/// parser.
pub fn tokenize(input: &str) -> Result<Vec<Token>, ParseError> {
    let mut tokens = Vec::new();
    let mut i = 0;
    while let Some(c) = input[i..].chars().next() {
        let width = c.len_utf8();
        if c.is_whitespace() {
            i += width;
        } else if c == '#' {
            // A comment runs to end of line and vanishes here — it may contain
            // any character, delimiters included, since this lookahead wins.
            i = input[i..].find('\n').map_or(input.len(), |n| i + n);
        } else if c == '"' {
            let (text, end) = read_string(input, i)?;
            tokens.push(Token {
                kind: TokenKind::Str(Rc::new(text)),
                span: Span::new(i, end),
            });
            i = end;
        } else if let Some(kind) = delimiter(c) {
            tokens.push(Token {
                kind,
                span: Span::new(i, i + width),
            });
            i += width;
        } else {
            // Run first, classify second — never the other way round. Matching
            // a number greedily at the cursor would take `2dup` as a 2 and a
            // `dup`; the run is what a number has to account for *all* of.
            let end = read_word(input, i);
            let text = &input[i..end];
            tokens.push(Token {
                kind: match number(text) {
                    Some(value) => TokenKind::Number(value),
                    None => TokenKind::Word(Rc::from(text)),
                },
                span: Span::new(i, end),
            });
            i = end;
        }
    }
    Ok(tokens)
}

/// The end index of the run starting at `start`, whose first character is known
/// not to break a word. Stops at the first breaking character — *except* inside
/// a number, where a `.` is the fraction's rather than the attribute operator's.
/// This is the one place a token's shape decides the split, and it asks
/// [`scan_number`] rather than restating what a number looks like: a `.` is part
/// of the run exactly when the number reaching from `start` covers it. So `3.5`
/// is one run and `obj.x` is three tokens; `3.` splits, since a fraction needs
/// digits after the dot.
fn read_word(input: &str, start: usize) -> usize {
    let number = scan_number(input, start).unwrap_or(start);
    let mut end = start;
    for (offset, c) in input[start..].char_indices() {
        let at = start + offset;
        if breaks_word(c) && !(c == '.' && at < number) {
            break;
        }
        end = at + c.len_utf8();
    }
    end
}

/// Read the `"…"` literal opening at `start`, returning its content and the
/// index just past the closing quote. Supports the escapes `\"`, `\\`, `\n`,
/// `\t`; an unknown escape keeps both characters verbatim. Fails with
/// [`ParseErrorKind::UnterminatedString`] at end-of-input, pointing at the
/// opening quote — the character the reader has to fix.
fn read_string(input: &str, start: usize) -> Result<(String, usize), ParseError> {
    let unterminated = || {
        ParseError::new(
            ParseErrorKind::UnterminatedString,
            Span::new(start, input.len()),
        )
    };
    let mut text = String::new();
    let mut chars = input[start..].char_indices();
    chars.next(); // the opening quote
    while let Some((offset, c)) = chars.next() {
        match c {
            '"' => return Ok((text, start + offset + 1)),
            '\\' => match chars.next() {
                Some((_, '"')) => text.push('"'),
                Some((_, '\\')) => text.push('\\'),
                Some((_, 'n')) => text.push('\n'),
                Some((_, 't')) => text.push('\t'),
                Some((_, other)) => {
                    text.push('\\');
                    text.push(other);
                }
                None => return Err(unterminated()),
            },
            _ => text.push(c),
        }
    }
    Err(unterminated())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The token kinds of `input`, for a compact assertion.
    fn kinds(input: &str) -> Vec<TokenKind> {
        tokenize(input)
            .unwrap()
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    fn word(text: &str) -> TokenKind {
        TokenKind::Word(Rc::from(text))
    }

    fn int(value: i64) -> TokenKind {
        TokenKind::Number(Value::Int(value))
    }

    fn float(value: f64) -> TokenKind {
        TokenKind::Number(Value::Num(value))
    }

    #[test]
    fn splits_on_whitespace() {
        assert_eq!(kinds("1 2 +"), vec![int(1), int(2), word("+")]);
    }

    #[test]
    fn the_ten_characters_are_self_delimiting() {
        // The whole point: a spaced form and a tight form tokenize identically.
        assert_eq!(kinds("[1 2 3]"), kinds("[ 1 2 3 ]"));
        assert_eq!(kinds("{x *}"), kinds("{ x * }"));
        assert_eq!(kinds("&f"), kinds("& f"));
        assert_eq!(kinds("'f"), kinds("' f"));
        assert_eq!(kinds("obj.x"), kinds("obj . x"));
        assert_eq!(kinds("{w h: w}"), kinds("{ w h : w }"));
        assert_eq!(
            kinds("&f"),
            vec![TokenKind::Amp, word("f")],
            "the sigil is its own token, not a prefix on the word"
        );
    }

    #[test]
    fn a_dot_inside_a_number_is_the_fraction_not_the_operator() {
        assert_eq!(kinds("3.5"), vec![float(3.5)]);
        assert_eq!(kinds("3.5e-2"), vec![float(3.5e-2)]);
        // A fraction needs digits on both sides — `0.1`, never `.1` — so
        // everywhere else the dot is the attribute operator.
        assert_eq!(kinds("0.1"), vec![float(0.1)]);
        assert_eq!(kinds(".1"), vec![TokenKind::Dot, int(1)]);
        assert_eq!(kinds("3."), vec![int(3), TokenKind::Dot]);
        assert_eq!(kinds("obj.x"), vec![word("obj"), TokenKind::Dot, word("x")]);
        assert_eq!(
            kinds("3.5.x"),
            vec![float(3.5), TokenKind::Dot, word("x")],
            "a number is still dottable"
        );
    }

    #[test]
    fn the_run_is_taken_before_it_is_classified() {
        // Matching a number greedily at the cursor would split `2dup` into a 2
        // and a `dup`. The run comes first; a number must account for all of it.
        assert_eq!(kinds("2dup"), vec![word("2dup")]);
        assert_eq!(
            kinds("2drop bi* ->"),
            vec![word("2drop"), word("bi*"), word("->")]
        );
        assert_eq!(kinds("2 dup"), vec![int(2), word("dup")]);
    }

    #[test]
    fn the_number_grammar_is_ours_not_the_f64_parsers() {
        // Rust's `f64::from_str` accepts these; our grammar does not, so they
        // are ordinary names — which is the point of writing the grammar down.
        for name in ["inf", "-inf", "infinity", "nan", "NaN", "Inf"] {
            assert_eq!(kinds(name), vec![word(name)], "{name} should be a name");
        }
        // Nor do these shapes exist yet, so they are names too, and adding any
        // of them later is a deliberate change to what a name can be.
        for name in ["1/2", "0x1f", "1_000", "e", "1e", "3d5"] {
            assert_eq!(kinds(name), vec![word(name)], "{name} should be a name");
        }
    }

    #[test]
    fn numbers_arrive_decoded() {
        assert_eq!(kinds("3"), vec![int(3)]);
        assert_eq!(kinds("-5"), vec![int(-5)]);
        assert_eq!(kinds("2e3"), vec![float(2000.0)]);
        assert_eq!(kinds("1e-2"), vec![float(0.01)]);
        assert_eq!(kinds("3.0"), vec![float(3.0)]);
        // Integer shape that overflows `i64` falls back to a float.
        assert_eq!(kinds("99999999999999999999"), vec![float(1e20)]);
    }

    #[test]
    fn strings_are_read_by_lookahead() {
        // Spaces, delimiters, and `#` inside a string are text.
        assert_eq!(
            kinds(r#""a [b] # c""#),
            vec![TokenKind::Str(Rc::new("a [b] # c".to_string()))]
        );
        assert_eq!(
            kinds(r#""a\nb\"c""#),
            vec![TokenKind::Str(Rc::new("a\nb\"c".to_string()))]
        );
    }

    #[test]
    fn a_string_abutting_a_word_still_splits() {
        assert_eq!(
            kinds(r#"say"hi""#),
            vec![word("say"), TokenKind::Str(Rc::new("hi".to_string()))]
        );
    }

    #[test]
    fn comments_run_to_end_of_line() {
        assert_eq!(kinds("1 # 2 + [ } '"), vec![int(1)]);
        assert_eq!(kinds("1 # a\n2"), vec![int(1), int(2)]);
        assert_eq!(kinds("# nothing but a comment"), vec![]);
    }

    #[test]
    fn an_unterminated_string_is_the_one_lexical_error() {
        assert_eq!(
            tokenize(r#""oops"#).unwrap_err().kind,
            ParseErrorKind::UnterminatedString
        );
        assert_eq!(
            tokenize(r#""bad \"#).unwrap_err().kind,
            ParseErrorKind::UnterminatedString
        );
    }

    #[test]
    fn spans_locate_the_token_in_the_source() {
        let source = "1 dup";
        let tokens = tokenize(source).unwrap();
        assert_eq!(tokens[0].span, Span::new(0, 1));
        assert_eq!(tokens[1].span.of(source), "dup");
    }

    #[test]
    fn multibyte_text_spans_bytes_not_chars() {
        let source = "'héllo";
        let tokens = tokenize(source).unwrap();
        assert_eq!(tokens[1].span.of(source), "héllo");
    }
}
