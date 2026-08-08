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

use super::{ParseError, ParseErrorKind};

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

/// What a token is. Deliberately shallow: the fixed characters are their own
/// variants, a `"…"` literal arrives with its escapes already resolved (the
/// tokenizer owns the lookahead, so the parser never sees a quote), and every
/// other run of characters is an uninterpreted [`TokenKind::Word`].
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    /// A run of ordinary characters: `+`, `dup`, `3.5`, `true`, `foo`. The
    /// parser decides which of those are literals and which are word references.
    Word(Rc<str>),
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
            let end = read_word(input, i);
            tokens.push(Token {
                kind: TokenKind::Word(Rc::from(&input[i..end])),
                span: Span::new(i, end),
            });
            i = end;
        }
    }
    Ok(tokens)
}

/// The end index of the word run starting at `start`, whose first character is
/// known not to break a word. Stops at the first breaking character — *except*
/// that a `.` flanked by digits is taken as part of the number, the one place a
/// token's shape decides the split. So `3.5` is one word and `obj.x` is three
/// tokens; `3.` and `.5` split, since neither has digits on both sides.
fn read_word(input: &str, start: usize) -> usize {
    let mut end = start;
    let mut prev_digit = false;
    for (offset, c) in input[start..].char_indices() {
        if c == '.' {
            let next = input[start + offset + 1..].chars().next();
            if !(prev_digit && next.is_some_and(|n| n.is_ascii_digit())) {
                break;
            }
        } else if breaks_word(c) {
            break;
        }
        prev_digit = c.is_ascii_digit();
        end = start + offset + c.len_utf8();
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

    #[test]
    fn splits_on_whitespace() {
        assert_eq!(kinds("1 2 +"), vec![word("1"), word("2"), word("+")]);
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
    fn a_dot_between_digits_belongs_to_the_number() {
        assert_eq!(kinds("3.5"), vec![word("3.5")]);
        assert_eq!(kinds("3.5e-2"), vec![word("3.5e-2")]);
        // Digits on both sides is the whole rule — otherwise the dot delimits.
        assert_eq!(kinds("obj.x"), vec![word("obj"), TokenKind::Dot, word("x")]);
        assert_eq!(kinds("3."), vec![word("3"), TokenKind::Dot]);
        assert_eq!(kinds(".5"), vec![TokenKind::Dot, word("5")]);
        assert_eq!(
            kinds("3.5.x"),
            vec![word("3.5"), TokenKind::Dot, word("x")],
            "a number is still dottable"
        );
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
        assert_eq!(kinds("1 # 2 + [ } '"), vec![word("1")]);
        assert_eq!(kinds("1 # a\n2"), vec![word("1"), word("2")]);
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
