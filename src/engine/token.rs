//! The tokenizer — the first of the three phases (`language-v2.md` §3):
//!
//! ```text
//! characters → [tokenize] → tokens → [parse] → tree → [evaluate] → values
//! ```
//!
//! String and `#`-comment **lookahead runs first**, so anything inside `"…"` or
//! after a `#` is text. What remains is three kinds of character:
//!
//! ```text
//! {  }  [  ]  (  )  :     standalone — a token whatever they abut
//! '                       prefix sigil — binds to the run on its right
//! .                       postfix operator — binds to what is on its left
//! ```
//!
//! **Adjacency is this phase's, not the parser's.** `'x` and `.x` are single
//! tokens, so there is no bare sigil in the stream and no consume-next rule
//! downstream. Which in turn is why the two sigil kinds have mirror-image
//! rules, and why neither needs stating twice:
//!
//! - A **prefix** is a sigil when nothing precedes it in the same token. That
//!   falls out of [`breaks_word`] rather than being checked: a run swallows `'`
//!   (`x'`, `don't`), so the scanner can only *land* on one where a token
//!   begins.
//! - A **postfix** attaches to whatever is on its left, so `.` is the attribute
//!   operator unless nothing is there ([`attached`]) — `obj.1` reads as an
//!   attribute and fails, while `obj .1` is a number.
//!
//! This phase also owns **both literal grammars**, decoded: a `"…"` arrives with
//! its escapes resolved, a number as a [`Value`] (see [`number`]). Everything
//! else is a name, which is what makes the number grammar load-bearing — names
//! are defined as what it doesn't claim.
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

    /// Where this span starts, as a 1-based **character** column. Bytes are the
    /// right unit to index with and the wrong one to show a reader.
    pub fn column(&self, source: &str) -> usize {
        source[..self.start].chars().count() + 1
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
    /// `'x` — a name. The sigil binds to the run after it, so this is one
    /// lexical unit and there is no such thing as a bare `'` in the stream.
    Name(Rc<str>),
    /// `.x` — attribute access, which stages the receiver (§7).
    Attr(Rc<str>),
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

/// The token a **standalone** character is — one that is its own token whatever
/// it abuts, so `word[word` is three tokens.
///
/// This is the single statement of that set, and it answers two questions at
/// once: *what token is this* (for [`tokenize`]) and *is this character in the
/// set* (for [`breaks_word`]). Keeping them one function is what stops them
/// disagreeing. Were the mapping inlined at the call site instead, a character
/// present there but missing from `breaks_word` would lex correctly at a token
/// start and be swallowed by a run anywhere else — a difference nothing would
/// catch.
fn standalone(c: char) -> Option<TokenKind> {
    Some(match c {
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

/// Whether `c` ends a run — equivalently, the characters no name may contain
/// (`language-v2.md` §4). Four categories, and each is in the set for its own
/// reason:
///
/// - **whitespace**, which separates by definition;
/// - the [`standalone`] characters, each its own token;
/// - **`.`**, which is never a token by itself — it always binds to the name or
///   number after it — but must still end whatever run precedes it, or `obj.x`
///   would be one name;
/// - **`"` and `#`**, which open a region of text the lookahead owns.
///
/// **`'` is deliberately absent**, and that is what makes it position-sensitive
/// without a rule saying so. A run swallows it (`x'`, `don't`), so the scanner
/// only ever *lands* on one where a token genuinely begins — after whitespace,
/// after a standalone character, or at the start of input. There it is the
/// prefix sigil; nowhere else can it be reached.
///
/// **`&` is absent for a plainer reason: it is no longer syntax.** It was the
/// second prefix, and `{x}` says what `&x` said (§4), so it is an ordinary name
/// character in every position — `&`, `a&b`, and `&x` are all names.
fn breaks_word(c: char) -> bool {
    c.is_whitespace() || standalone(c).is_some() || matches!(c, '.' | '"' | '#')
}

/// Whether a `.` at `index` has anything on its left to attach to. `.` is a
/// *postfix* operator — the mirror of the prefix sigils — so it reads as the
/// attribute operator whenever it abuts what precedes it, and may begin a
/// number only when nothing does:
///
/// ```text
/// obj.1     attached   → attribute `1` → not a name, an error
/// [1 2].1   attached   → the same, for the same reason
/// obj .1    detached   → obj, then 0.1
/// ```
fn attached(input: &str, index: usize) -> bool {
    input[..index]
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_whitespace())
}

/// The number grammar, and the only statement of it:
///
/// ```text
/// number   = "-"? ( digit+ fraction? | fraction ) exponent?
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
    let sign = usize::from(text.starts_with('-'));
    let integer = digits_end(text, sign);
    // Digits, a fraction, or both — but at least one of them, which is the
    // whole of the leading rule. So `3`, `3.5`, and `.5` are numbers, while
    // `3.` is the number `3` with a dot after it, and `.x` and `-x` are names.
    let mut at = match (fraction_end(text, integer), integer > sign) {
        (Some(fraction), _) => fraction,
        (None, true) => integer,
        (None, false) => return None,
    };
    if let Some(exponent) = exponent_end(text, at) {
        at = exponent;
    }
    Some(start + at)
}

/// The index past the fraction at `at`, if one is there. A lone `.` is not one:
/// the digits after it are what make it a fraction rather than the attribute
/// operator, which is the rule that lets `3.` and `obj.x` split.
fn fraction_end(text: &str, at: usize) -> Option<usize> {
    text[at..]
        .starts_with('.')
        .then(|| digits_end(text, at + 1))
        .filter(|&end| end > at + 1)
}

/// The index past the exponent at `at`, if one is there — `e`/`E`, an optional
/// sign, then digits. Without the digits it is part of a name (`1e`).
fn exponent_end(text: &str, at: usize) -> Option<usize> {
    if !text[at..].starts_with(['e', 'E']) {
        return None;
    }
    let digits = at + 1 + usize::from(text[at + 1..].starts_with(['+', '-']));
    Some(digits_end(text, digits)).filter(|&end| end > digits)
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

/// Split `input` into tokens, or fail on a string with no closing `"` or a
/// sigil with no name. Nesting, resolution, and meaning belong to the parser.
///
/// Every arm yields one token and where it ends, so the loop pushes and advances
/// in one place; the two that produce nothing — whitespace and comments —
/// advance and `continue`.
pub fn tokenize(input: &str) -> Result<Vec<Token>, ParseError> {
    let mut tokens = Vec::new();
    let mut i = 0;
    while let Some(c) = input[i..].chars().next() {
        let (kind, end) = match c {
            // The two that produce nothing: a run of whitespace, and a comment
            // to end of line — which may hold any character, delimiters
            // included, since this lookahead wins. The newline itself is left
            // for the whitespace arm.
            _ if c.is_whitespace() => {
                i = skip_to(input, i, |c| !c.is_whitespace());
                continue;
            }
            '#' => {
                i = skip_to(input, i, |c| c == '\n');
                continue;
            }
            '"' => {
                let (text, end) = read_string(input, i)?;
                (TokenKind::Str(Rc::new(text)), end)
            }
            // The prefix sigil. Only reachable at a token start: a run swallows
            // the character, so a mid-name one is never seen here.
            '\'' => {
                let (name, end) = read_name(input, i, '\'')?;
                (TokenKind::Name(name), end)
            }
            '.' => read_dot(input, i)?,
            // A standalone character, or else a run to classify.
            _ => match standalone(c) {
                Some(kind) => (kind, i + c.len_utf8()),
                None => {
                    let end = read_word(input, i);
                    (classify(&input[i..end]), end)
                }
            },
        };
        tokens.push(Token {
            kind,
            span: Span::new(i, end),
        });
        i = end;
    }
    Ok(tokens)
}

/// The index at or after `start` of the first character `stop` accepts, or the
/// end of input — how the two token-less arms skip what they consume, in the
/// same shape every other arm reports where its token ends.
fn skip_to(input: &str, start: usize, stop: impl Fn(char) -> bool) -> usize {
    input[start..]
        .find(stop)
        .map_or(input.len(), |offset| start + offset)
}

/// A run is a number when the grammar accounts for **all** of it, and a name
/// when it doesn't — the negative definition, in one line.
fn classify(text: &str) -> TokenKind {
    match number(text) {
        Some(value) => TokenKind::Number(value),
        None => TokenKind::Word(Rc::from(text)),
    }
}

/// The token a `.` at `start` begins. Detached, it may open a number; attached,
/// it is the attribute operator. An attribute is the fallback either way, so
/// `.map` reads the same wherever it appears — only whether a *number* is on
/// offer depends on what precedes the dot.
///
/// An attribute is the fallback either way, so `.map` reads the same wherever it
/// appears — only whether a *number* is on offer depends on what precedes the
/// dot. With `&` no longer a sigil there is no second dotted form: `.&x` is the
/// attribute *named* `&x`, exactly as `&x` is a word named `&x`.
fn read_dot(input: &str, start: usize) -> Result<(TokenKind, usize), ParseError> {
    if !attached(input, start) {
        let end = read_word(input, start);
        if let Some(value) = number(&input[start..end]) {
            return Ok((TokenKind::Number(value), end));
        }
    }
    let (name, end) = read_name(input, start, '.')?;
    Ok((TokenKind::Attr(name), end))
}

/// Read the name a sigil at `start` binds to: the run just after it, which must
/// be a word. A literal is no name, and neither is an empty run — so `'3`, `'`,
/// and `. x` all fail here, at the sigil, rather than reaching the parser. This
/// is the whole of the consume-next rule, and it is lexical because adjacency is.
fn read_name(input: &str, start: usize, sigil: char) -> Result<(Rc<str>, usize), ParseError> {
    let expected = || {
        ParseError::new(
            ParseErrorKind::ExpectedName { after: sigil },
            Span::new(start, start + sigil.len_utf8()),
        )
    };
    let at = start + sigil.len_utf8();
    let end = match input[at..].chars().next() {
        Some(c) if !breaks_word(c) => read_word(input, at),
        _ => return Err(expected()),
    };
    let text = &input[at..end];
    match number(text) {
        Some(_) => Err(expected()),
        None => Ok((Rc::from(text), end)),
    }
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

    fn name(text: &str) -> TokenKind {
        TokenKind::Name(Rc::from(text))
    }

    fn attr(text: &str) -> TokenKind {
        TokenKind::Attr(Rc::from(text))
    }

    #[test]
    fn the_brackets_are_self_delimiting() {
        // A spaced form and a tight form give the same tokens, so a bracket may
        // bunch up against whatever it abuts.
        assert_eq!(kinds("[1 2 3]"), kinds("[ 1 2 3 ]"));
        assert_eq!(kinds("{x *}"), kinds("{ x * }"));
        assert_eq!(kinds("{w h: w}"), kinds("{ w h : w }"));
        assert_eq!(
            kinds("word[word"),
            vec![word("word"), TokenKind::Open(Bracket::Square), word("word")]
        );
    }

    #[test]
    fn the_sigils_are_prefixes_that_bind_to_a_run() {
        // One lexical unit, so there is no bare `'` in the stream — and the
        // spaced form is *not* the same thing.
        assert_eq!(kinds("'f g"), vec![name("f"), word("g")]);
        assert!(
            tokenize("' f").is_err(),
            "a sigil binds tightly or not at all"
        );
        assert!(tokenize("obj . x").is_err());
        // A sigil is a sigil only where a token begins — which needs no rule of
        // its own, since a run swallows the character everywhere else.
        assert_eq!(kinds("x'"), vec![word("x'")]);
        assert_eq!(kinds("don't"), vec![word("don't")]);
        assert_eq!(kinds("'x'"), vec![name("x'")]);
        // A token begins after a delimiter too, not only after whitespace.
        assert_eq!(
            kinds("['f]"),
            vec![
                TokenKind::Open(Bracket::Square),
                name("f"),
                TokenKind::Close(Bracket::Square)
            ]
        );
    }

    #[test]
    fn the_ampersand_is_an_ordinary_name_character() {
        // It was the second prefix sigil; `{x}` says what `&x` said, so the
        // character went back to the vocabulary and is unremarkable in every
        // position — leading, interior, and alone.
        assert_eq!(kinds("a&b"), vec![word("a&b")]);
        assert_eq!(kinds("&x"), vec![word("&x")]);
        assert_eq!(kinds("&"), vec![word("&")]);
        assert_eq!(kinds("1 2 &"), vec![int(1), int(2), word("&")]);
    }

    #[test]
    fn a_dot_is_a_postfix_so_adjacency_decides_it() {
        // Attached, `.` is the attribute operator — there is something on its
        // left to stage. Detached, it may open a number.
        assert_eq!(kinds("obj.x"), vec![word("obj"), attr("x")]);
        assert_eq!(kinds("obj .1"), vec![word("obj"), float(0.1)]);
        assert_eq!(kinds(".1"), vec![float(0.1)]);
        // So a numeric attribute is an error rather than a silent float, and it
        // reads the same after a `]` as after a word.
        assert!(tokenize("obj.1").is_err());
        assert!(tokenize("[1 2].1").is_err());
        // An attribute is always the fallback, so `.map` works anywhere.
        assert_eq!(
            kinds("{.map}"),
            vec![
                TokenKind::Open(Bracket::Brace),
                attr("map"),
                TokenKind::Close(Bracket::Brace)
            ]
        );
        assert_eq!(kinds(".map"), vec![attr("map")]);
        assert_eq!(kinds("obj.2dup"), vec![word("obj"), attr("2dup")]);
        // `&` is a name character after a dot as everywhere else, so `.&x` is
        // simply the attribute named `&x` — no second dotted form to tell apart.
        assert_eq!(kinds(".foo&bar"), vec![attr("foo&bar")]);
        assert_eq!(kinds("obj.&x"), vec![word("obj"), attr("&x")]);
    }

    #[test]
    fn a_dot_inside_a_number_is_the_fraction_not_the_operator() {
        assert_eq!(kinds(".1"), vec![float(0.1)]);
        assert_eq!(kinds("-.5"), vec![float(-0.5)]);
        assert_eq!(kinds("3.5"), vec![float(3.5)]);
        assert_eq!(kinds("3.5e-2"), vec![float(3.5e-2)]);
        // A fraction needs digits on both sides — `0.1`, never `.1` — so
        // everywhere else the dot is the attribute operator.
        assert_eq!(kinds("0.1"), vec![float(0.1)]);
        assert_eq!(kinds("obj.x"), vec![word("obj"), attr("x")]);
        // A trailing dot has no digits after it, so it is an attribute operator
        // with no name — an error, not a number.
        assert!(tokenize("3.").is_err());
        assert_eq!(
            kinds("3.5.x"),
            vec![float(3.5), attr("x")],
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
        // The span covers the sigil and its name together — one lexical unit.
        assert_eq!(tokens[0].kind, name("héllo"));
        assert_eq!(tokens[0].span.of(source), "'héllo");
    }
}
