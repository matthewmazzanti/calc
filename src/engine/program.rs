//! The parser — the second phase (`language-v2.md` §3), and the program
//! representation it produces.
//!
//! ```text
//! characters → [tokenize] → tokens → [parse] → tree → [evaluate] → values
//! ```
//!
//! The parser consumes the flat token stream and **resolves everything
//! positional**: `{` recurses into a nested [`Element::Template`], the sigils
//! consume the token after them, `[ ] ( )` become fixed elements that skip the
//! lookup every other token gets, and a leading `names :` run becomes the binds
//! it abbreviates. What survives into the tree is only what evaluation still has
//! to decide.
//!
//! **A tree, not a flat program** — this is the reversal from v1, where `{ }`
//! was a runtime mark and code was data. A template is parsed once, holds no
//! environment, and is immutable and shared; evaluation pairs it with the
//! current frame to make a function (`direction-v2.md`).
//!
//! Four errors belong to this phase — a closer with nothing open, an opener
//! never closed, a closer crossing a region opened inside another, and a sigil
//! with nothing following it — plus the misplaced `:` the parameter rule
//! implies, and one implementation bound ([`MAX_NESTING`]). All are free: they
//! occur before evaluation, so there is no state to restore.

use std::rc::Rc;

use super::token::{tokenize, Bracket, Token, TokenKind};
use super::{ParseError, ParseErrorKind, Span, Value};

/// A runtime region: the pair that opens a mark on the data stack and collects
/// at its closer. Unlike `{ }`, these hold *values that come into existence
/// during evaluation*, so the mark, the contents, and the collection are all
/// deferred — only the pairing is settled here (§6).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Region {
    /// `[ ]` — a list.
    List,
    /// `( )` — a dict.
    Dict,
}

/// A program element — a member of a template's sequence (§14). The parser
/// produces a `Vec<Element>` per parse unit and an `Rc<[Element]>` per nested
/// template; evaluation walks it.
#[derive(Debug, Clone, PartialEq)]
pub enum Element {
    /// A literal value: a number, string, name, or boolean. `'x` lands here as a
    /// `Value::Name` — the sigil is resolved at parse time, not looked up.
    Literal(Value),
    /// A bare word, resolved against the environment at *application* time — the
    /// late binding that gives recursion for free (§8).
    Word(Rc<str>),
    /// `&f` — push the value bound to `f` unapplied. Requires `f` to be bound;
    /// contrast `'f`, which denotes and works on unbound names (§4).
    Fetch(Rc<str>),
    /// `.x` — attribute access, staging the receiver: `obj.x` ≡ `obj.&x call`.
    Attr(Rc<str>),
    /// `.&x` — the same lookup without applying, leaving the receiver beneath
    /// the function it found (§7).
    AttrFetch(Rc<str>),
    /// Bind the top of the stack to a name in the current frame — what a
    /// template's `names :` list emits, one per parameter.
    ///
    /// The same binding `set` performs, but as a *fixed* element rather than a
    /// word reference. `:` is fixed syntax, so it must not be breakable by
    /// rebinding `set`, exactly as `[`/`]` stopped being words in v2. It also
    /// makes the parameter list recoverable from the tree — a leading run of
    /// `Bind`s is unambiguously one, since a hand-written `'x set` parses to a
    /// `Literal` and a `Word` — which is where arity and signatures come from.
    Bind(Rc<str>),
    /// A `{ … }` template: an element sequence with no environment, immutable
    /// and shared. Evaluating one instantiates a function by pairing it with the
    /// current frame (§5).
    Template(Rc<[Element]>),
    /// `[` or `(` — push a mark, opening a region. A *fixed* element: the parser
    /// pairs it, and it is never looked up, so it can't be rebound or shadowed.
    Open(Region),
    /// `]` or `)` — collect the values above the nearest mark.
    Close(Region),
}

impl std::fmt::Display for Element {
    /// The canonical text of an element — re-parseable, and written in the §13
    /// style (brackets against their contents).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Element::Literal(v) => write!(f, "{v}"),
            Element::Word(name) => write!(f, "{name}"),
            Element::Fetch(name) => write!(f, "&{name}"),
            Element::Attr(name) => write!(f, ".{name}"),
            Element::AttrFetch(name) => write!(f, ".&{name}"),
            // Only reachable outside a template's parameter list, which the
            // parser never produces — printed as the `set` it is equivalent to,
            // so the form stays re-parseable whatever built it.
            Element::Bind(name) => write!(f, "'{name} set"),
            Element::Template(body) => {
                // A leading run of `Bind`s is the parameter list, so print it
                // back as one: `{w h: …}`, not the `set`s it compiles to. The
                // run reads reversed, since `:` emits the names top-of-stack
                // first.
                let count = body
                    .iter()
                    .take_while(|e| matches!(e, Element::Bind(_)))
                    .count();
                write!(f, "{{")?;
                for (i, element) in body[..count].iter().rev().enumerate() {
                    let Element::Bind(name) = element else {
                        unreachable!("the run holds only binds")
                    };
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{name}")?;
                }
                if count > 0 {
                    write!(f, ":")?;
                }
                for (i, element) in body[count..].iter().enumerate() {
                    if i > 0 || count > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{element}")?;
                }
                write!(f, "}}")
            }
            Element::Open(Region::List) => write!(f, "["),
            Element::Close(Region::List) => write!(f, "]"),
            Element::Open(Region::Dict) => write!(f, "("),
            Element::Close(Region::Dict) => write!(f, ")"),
        }
    }
}

/// How deep regions may nest. `{` is the parser's own recursion, so unbounded
/// nesting would be an unbounded Rust stack — and a REPL that aborts on pasted
/// input loses the session. Far past anything hand-written; the depth is the
/// *open region* count, which `[` and `(` share since they hold the recursion's
/// frame open too.
const MAX_NESTING: usize = 256;

/// Parse a line into a program, or fail with the offending [`Span`].
pub fn parse(input: &str) -> Result<Vec<Element>, ParseError> {
    let tokens = tokenize(input)?;
    Parser {
        tokens: &tokens,
        next: 0,
        opens: Vec::new(),
    }
    .body(0)
}

/// The parse state: a position in the token stream, and the regions currently
/// open.
///
/// `opens` is **one stack for all three bracket pairs**, shared across the
/// recursion rather than one per body. Nesting `{ }` is the parser's own
/// recursion (§3), so the stack isn't a depth counter — it is what lets a closer
/// say *which* opener it crossed, since `[ ]` and `( )` are emitted flat and so
/// have no recursion of their own to check them.
struct Parser<'a> {
    tokens: &'a [Token],
    next: usize,
    opens: Vec<(Bracket, Span)>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a TokenKind> {
        self.peek_at(self.next)
    }

    fn peek_at(&self, index: usize) -> Option<&'a TokenKind> {
        self.tokens.get(index).map(|t| &t.kind)
    }

    fn advance(&mut self) -> Option<&'a Token> {
        let token = self.tokens.get(self.next)?;
        self.next += 1;
        Some(token)
    }

    /// Parse elements until this body's closer, recursing on `{`. `base` is
    /// `opens.len()` on entry — everything above it was opened *inside* this
    /// body and must therefore close inside it.
    fn body(&mut self, base: usize) -> Result<Vec<Element>, ParseError> {
        // Parameters are recognized only here, at the head of a template body.
        let mut elements = if base > 0 { self.params()? } else { Vec::new() };
        loop {
            let Some(token) = self.advance() else {
                // End of input. Anything still open never closed — report the
                // innermost, which is the one the reader has to close first.
                return match self.opens.last() {
                    Some(&(bracket, span)) => Err(ParseError::new(
                        ParseErrorKind::UnclosedOpen(bracket.open()),
                        span,
                    )),
                    None => Ok(elements),
                };
            };
            match &token.kind {
                // Both literal kinds arrive decoded; a word is looked up at
                // application time. The tokenizer settled which is which.
                TokenKind::Word(name) => elements.push(Element::Word(name.clone())),
                TokenKind::Number(value) => elements.push(Element::Literal(value.clone())),
                TokenKind::Str(text) => elements.push(Element::Literal(Value::Str(text.clone()))),
                // `'` denotes and `&` fetches — both consume the next token (§4).
                TokenKind::Quote => {
                    let name = self.name_after(token)?;
                    elements.push(Element::Literal(Value::Name(name)));
                }
                TokenKind::Amp => {
                    let name = self.name_after(token)?;
                    elements.push(Element::Fetch(name));
                }
                // `.x` applies what it finds, `.&x` leaves it unapplied (§7).
                TokenKind::Dot => {
                    let fetch = matches!(self.peek(), Some(TokenKind::Amp));
                    if fetch {
                        self.advance();
                    }
                    let name = self.name_after(token)?;
                    elements.push(if fetch {
                        Element::AttrFetch(name)
                    } else {
                        Element::Attr(name)
                    });
                }
                // A `:` the parameter scan didn't consume is out of position.
                TokenKind::Colon => {
                    return Err(ParseError::new(ParseErrorKind::MisplacedColon, token.span))
                }
                TokenKind::Open(Bracket::Brace) => {
                    if self.opens.len() >= MAX_NESTING {
                        return Err(ParseError::new(ParseErrorKind::TooDeeplyNested, token.span));
                    }
                    self.opens.push((Bracket::Brace, token.span));
                    let inner = self.body(self.opens.len())?;
                    elements.push(Element::Template(inner.into()));
                }
                TokenKind::Open(bracket) => {
                    self.opens.push((*bracket, token.span));
                    elements.push(Element::Open(region(*bracket)));
                }
                TokenKind::Close(bracket) => {
                    if self.close(*bracket, token, base)? {
                        return Ok(elements); // our own `}` — the body ends here
                    }
                    elements.push(Element::Close(region(*bracket)));
                }
            }
        }
    }

    /// Match a closer against the open regions. Returns whether it closed the
    /// *current body* — only a `}` can — having popped what it matched.
    ///
    /// A region opened inside this body (`opens.len() > base`) must be the
    /// innermost thing open, so anything else is a crossing. With nothing open
    /// inside the body, a `}` is this template's own closer; any other closer is
    /// reaching for an opener it can't have, and which error that is depends on
    /// whether such an opener exists at all:
    ///
    /// ```text
    /// ( 1 ]        `]` crosses an open `(`      — a region is open, and it isn't ours
    /// { [ } ]      `}` crosses an open `[`      — the same, one level up
    /// [ { ] }      `]` crosses an open `{`      — the `[` is real, but outside the template
    /// { ] }        unmatched `]`                — no `[` anywhere to close
    /// ```
    fn close(&mut self, bracket: Bracket, token: &Token, base: usize) -> Result<bool, ParseError> {
        let crossing = |crossed: Bracket| {
            ParseError::new(
                ParseErrorKind::CrossingClose {
                    closer: bracket.close(),
                    crossed: crossed.open(),
                },
                token.span,
            )
        };
        if self.opens.len() > base {
            let &(innermost, _) = self.opens.last().expect("opens.len() > base ≥ 0");
            if innermost != bracket {
                return Err(crossing(innermost));
            }
            self.opens.pop();
            return Ok(false); // an inner region closed; this body continues
        }
        if base > 0 && bracket == Bracket::Brace {
            self.opens.pop(); // our own `{`
            return Ok(true);
        }
        match self.opens.last() {
            Some(&(innermost, _)) if self.opens.iter().any(|&(open, _)| open == bracket) => {
                Err(crossing(innermost))
            }
            _ => Err(ParseError::new(
                ParseErrorKind::UnmatchedClose(bracket.close()),
                token.span,
            )),
        }
    }

    /// The name a sigil takes: the next token, which must be a word — no name
    /// contains one of the fixed characters, and a string or end-of-input is no
    /// name either. Blamed on the sigil, since that's what's left dangling.
    fn name_after(&mut self, sigil: &Token) -> Result<Rc<str>, ParseError> {
        match self.peek() {
            Some(TokenKind::Word(name)) => {
                let name = name.clone();
                self.advance();
                Ok(name)
            }
            _ => Err(ParseError::new(
                ParseErrorKind::ExpectedName {
                    after: match sigil.kind {
                        TokenKind::Quote => '\'',
                        TokenKind::Amp => '&',
                        _ => '.', // only the three sigils reach here
                    },
                },
                sigil.span,
            )),
        }
    }

    /// A template's parameter list: a leading run of names ended by `:`, which
    /// binds them from the stack. `{w h: …}` binds as `{'h set 'w set …}` would
    /// — the names read bottom to top, so the rightmost takes the top of the
    /// stack and the list reads in the order a caller supplies it (§5).
    ///
    /// Two things the `set` spelling doesn't get. The binder is an
    /// [`Element::Bind`], not a lookup of the word `set`, so fixed syntax can't
    /// be broken by rebinding a word. And because the list is *syntax* rather
    /// than a name datum, its names are checked here: `'3 set` is a legal way to
    /// bind an odd name, but `{x 3: …}` is a typo, and a parse error costs
    /// nothing.
    ///
    /// Finding the list is pure lookahead — a run of words *not* followed by `:`
    /// is ordinary code, so `{1 2 3}` parses as it reads and nothing is
    /// consumed. Only once the `:` confirms a parameter list do the names have
    /// to be names.
    fn params(&mut self) -> Result<Vec<Element>, ParseError> {
        // The run is of *atoms* — words and literals alike — so that `{x 3: …}`
        // is a parameter list with a bad name rather than a stray `:`. Structure
        // (a sigil, a bracket) still ends the scan, and a `:` after one of those
        // is genuinely misplaced.
        let atom = |kind: Option<&TokenKind>| {
            matches!(
                kind,
                Some(TokenKind::Word(_) | TokenKind::Number(_) | TokenKind::Str(_))
            )
        };
        let mut scan = self.next;
        while atom(self.peek_at(scan)) {
            scan += 1;
        }
        if scan == self.next || !matches!(self.peek_at(scan), Some(TokenKind::Colon)) {
            return Ok(Vec::new());
        }
        let mut elements = Vec::with_capacity(scan - self.next);
        for token in &self.tokens[self.next..scan] {
            // A parameter must be a name, and a name is what the number grammar
            // didn't claim. Read in written order, so the leftmost offender is
            // the one reported.
            let TokenKind::Word(name) = &token.kind else {
                return Err(ParseError::new(
                    ParseErrorKind::InvalidParameter,
                    token.span,
                ));
            };
            elements.push(Element::Bind(name.clone()));
        }
        elements.reverse(); // the last name written takes the top of the stack
        self.next = scan + 1; // past the names and the `:`
        Ok(elements)
    }
}

/// The region a bracket opens. `{ }` never gets here — it resolves at parse time
/// into a template instead.
fn region(bracket: Bracket) -> Region {
    match bracket {
        Bracket::Square => Region::List,
        Bracket::Paren => Region::Dict,
        Bracket::Brace => unreachable!("a brace becomes a template, not a region"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(input: &str) -> Vec<Element> {
        parse(input).expect("should parse")
    }

    fn err(input: &str) -> ParseErrorKind {
        parse(input).expect_err("should not parse").kind
    }

    fn literal(value: impl Into<Value>) -> Element {
        Element::Literal(value.into())
    }

    fn word_ref(name: &str) -> Element {
        Element::Word(Rc::from(name))
    }

    fn name(text: &str) -> Element {
        Element::Literal(Value::Name(Rc::from(text)))
    }

    fn template(body: Vec<Element>) -> Element {
        Element::Template(body.into())
    }

    #[test]
    fn literals_come_decoded_and_every_other_token_is_a_reference() {
        // The tokenizer settled literal-vs-word; the parser only places them.
        assert_eq!(
            parsed(r#"1 2.5 "s" + true"#),
            vec![
                literal(1_i64),
                literal(2.5),
                literal("s"),
                word_ref("+"),
                // `true` is a prelude *binding*, so it resolves like any name —
                // the language has no keywords.
                word_ref("true"),
            ]
        );
        // Parsing never fails on an unknown word — that's a runtime unbound.
        assert_eq!(parsed("nope"), vec![word_ref("nope")]);
    }

    #[test]
    fn the_sigils_consume_the_token_after_them() {
        assert_eq!(parsed("'x"), vec![name("x")]);
        assert_eq!(parsed("&x"), vec![Element::Fetch(Rc::from("x"))]);
        // Self-delimiting, so the spaced form is the same program.
        assert_eq!(parsed("' x"), parsed("'x"));
        assert_eq!(parsed("& x"), parsed("&x"));
        // `'` denotes and so works on an unbound name; `&` fetches.
        assert_eq!(
            parsed("'sq &sq"),
            vec![name("sq"), Element::Fetch(Rc::from("sq"))]
        );
    }

    #[test]
    fn a_dot_stages_the_receiver() {
        assert_eq!(
            parsed("obj.x"),
            vec![word_ref("obj"), Element::Attr(Rc::from("x"))]
        );
        assert_eq!(
            parsed("obj.&x"),
            vec![word_ref("obj"), Element::AttrFetch(Rc::from("x"))]
        );
    }

    #[test]
    fn braces_recurse_into_a_template() {
        assert_eq!(
            parsed("'sq {dup *} ="),
            vec![
                name("sq"),
                template(vec![word_ref("dup"), word_ref("*")]),
                word_ref("="),
            ]
        );
        // Nesting is the parser's own recursion — depth needs no counter.
        assert_eq!(
            parsed("{{*}}"),
            vec![template(vec![template(vec![word_ref("*")])])]
        );
        assert_eq!(parsed("{}"), vec![template(vec![])]);
    }

    #[test]
    fn regions_are_fixed_elements_not_words() {
        // `[` and `]` skip the lookup every other token gets, so they parse to
        // fixed elements rather than word references.
        assert_eq!(
            parsed("[1 2]"),
            vec![
                Element::Open(Region::List),
                literal(1_i64),
                literal(2_i64),
                Element::Close(Region::List),
            ]
        );
        assert_eq!(parsed("[1 2]"), parsed("[ 1 2 ]"));
        assert_eq!(
            parsed("()"),
            vec![Element::Open(Region::Dict), Element::Close(Region::Dict)]
        );
    }

    #[test]
    fn parameters_bind_bottom_to_top() {
        // `{w h: …}` binds as `{'h set 'w set …}` would — the names read bottom
        // to top, so the rightmost takes the top of the stack (§5).
        assert_eq!(
            parsed("{w h: w h *}"),
            vec![template(vec![
                Element::Bind(Rc::from("h")),
                Element::Bind(Rc::from("w")),
                word_ref("w"),
                word_ref("h"),
                word_ref("*"),
            ])]
        );
        assert_eq!(
            parsed("{n: {n *}}"),
            vec![template(vec![
                Element::Bind(Rc::from("n")),
                template(vec![word_ref("n"), word_ref("*")]),
            ])]
        );
    }

    #[test]
    fn parameters_bind_without_looking_up_a_word() {
        // `:` is fixed syntax, so — like `[` and `]` — it must not be breakable
        // by rebinding a word. The binder is an element, not a `set` reference.
        let Element::Template(body) = &parsed("{w h: w h *}")[0] else {
            panic!("expected a template");
        };
        assert!(
            !body.contains(&word_ref("set")),
            "the parameter list resolved a word: {body:?}"
        );
        // A hand-written `set` still parses to the word, so the two forms stay
        // distinguishable — which is what makes a leading `Bind` run a reliable
        // parameter list to read arity back off.
        assert_eq!(
            parsed("{'h set}"),
            vec![template(vec![name("h"), word_ref("set")])]
        );
    }

    #[test]
    fn a_parameter_must_be_a_name() {
        // A parameter list is syntax, so it can be strict where a name *datum*
        // can't: `'3 set` binds an odd name on purpose, `{x 3: …}` is a typo.
        assert_eq!(err("{x 3: x}"), ParseErrorKind::InvalidParameter);
        assert_eq!(err("{2e3: x}"), ParseErrorKind::InvalidParameter);
        assert_eq!(err(r#"{"s": x}"#), ParseErrorKind::InvalidParameter);
        // The leftmost offender is the one reported.
        let source = "{a 1 2: x}";
        assert_eq!(parse(source).unwrap_err().span.of(source), "1");
        // Anything the number grammar didn't claim is a name — digits, symbols,
        // and `true`, which is a binding rather than a keyword.
        assert!(parse("{2dup +: x}").is_ok());
        assert!(parse("{+ -> x2: x}").is_ok());
        assert!(parse("{true inf: x}").is_ok());
    }

    #[test]
    fn a_leading_word_run_is_only_parameters_when_a_colon_follows() {
        // Pure lookahead — `{1 2 3}` is a body, not a parameter list.
        assert_eq!(
            parsed("{1 2 3}"),
            vec![template(vec![
                literal(1_i64),
                literal(2_i64),
                literal(3_i64)
            ])]
        );
        assert_eq!(
            parsed("{dup *}"),
            vec![template(vec![word_ref("dup"), word_ref("*")])]
        );
    }

    #[test]
    fn a_colon_out_of_position_is_an_error() {
        // A run of *words* ended by `:` is the parameter list, wherever those
        // words come from — so a misplaced colon is one the scan can't reach:
        // preceded by a non-word, second in a body, or outside a template.
        assert_eq!(err("{'x :}"), ParseErrorKind::MisplacedColon);
        assert_eq!(err("{[1] :}"), ParseErrorKind::MisplacedColon);
        assert_eq!(err("{x: y:}"), ParseErrorKind::MisplacedColon);
        assert_eq!(err("{: x}"), ParseErrorKind::MisplacedColon);
        assert_eq!(err("x:"), ParseErrorKind::MisplacedColon);
    }

    #[test]
    fn a_closer_with_nothing_open_is_an_error() {
        assert_eq!(err("]"), ParseErrorKind::UnmatchedClose(']'));
        assert_eq!(err("1 2 }"), ParseErrorKind::UnmatchedClose('}'));
        // Nothing of *that kind* is open — the `{` doesn't make a `]` matchable.
        assert_eq!(err("{ ] }"), ParseErrorKind::UnmatchedClose(']'));
    }

    #[test]
    fn an_opener_never_closed_is_an_error() {
        assert_eq!(err("[ 1 2"), ParseErrorKind::UnclosedOpen('['));
        assert_eq!(err("{dup *"), ParseErrorKind::UnclosedOpen('{'));
        // The innermost is reported — it has to close first.
        assert_eq!(err("{ [ 1"), ParseErrorKind::UnclosedOpen('['));
    }

    #[test]
    fn a_closer_crossing_a_region_is_an_error() {
        // The canonical case: the `}` would close across a `[` opened inside it.
        assert_eq!(
            err("{ [ } ]"),
            ParseErrorKind::CrossingClose {
                closer: '}',
                crossed: '['
            }
        );
        // And the mirror: a region opened outside can't be closed inside a `{`.
        assert_eq!(
            err("[ { ] }"),
            ParseErrorKind::CrossingClose {
                closer: ']',
                crossed: '{'
            }
        );
        // Mismatched pairs are the same error — the closer crosses its sibling.
        assert_eq!(
            err("( 1 ]"),
            ParseErrorKind::CrossingClose {
                closer: ']',
                crossed: '('
            }
        );
    }

    #[test]
    fn a_sigil_with_nothing_usable_after_it_is_an_error() {
        assert_eq!(err("'"), ParseErrorKind::ExpectedName { after: '\'' });
        assert_eq!(err("&"), ParseErrorKind::ExpectedName { after: '&' });
        assert_eq!(err("x."), ParseErrorKind::ExpectedName { after: '.' });
        // No name may contain a fixed character, so a delimiter is no name.
        assert_eq!(err("'{}"), ParseErrorKind::ExpectedName { after: '\'' });
        assert_eq!(err(r#"&"s""#), ParseErrorKind::ExpectedName { after: '&' });
        // Nor is a literal. One rule now covers the sigils and the parameter
        // list: a name is what the number grammar didn't claim.
        assert_eq!(err("'3"), ParseErrorKind::ExpectedName { after: '\'' });
        assert_eq!(err("&2e3"), ParseErrorKind::ExpectedName { after: '&' });
        assert_eq!(err("obj.3"), ParseErrorKind::ExpectedName { after: '.' });
        // `true` is a binding, so it names like anything else.
        assert!(parse("'true &true obj.true").is_ok());
    }

    #[test]
    fn nesting_is_bounded_so_pathological_input_is_a_diagnostic() {
        // An implementation limit, not a language rule: `{` recurses, and a
        // REPL must answer bad input with a message rather than a stack
        // overflow. Anything hand-written is orders of magnitude under it.
        let over = MAX_NESTING + 1;
        assert_eq!(
            err(&("{".repeat(over) + &"}".repeat(over))),
            ParseErrorKind::TooDeeplyNested
        );
        let at_limit = "{".repeat(MAX_NESTING) + &"}".repeat(MAX_NESTING);
        assert!(parse(&at_limit).is_ok());
    }

    #[test]
    fn errors_point_at_the_offending_text() {
        // Three of the four are locatable at the offending token; only an
        // unclosed opener has to look back, and its span does.
        let source = "1 2 [ 3";
        let error = parse(source).unwrap_err();
        assert_eq!(error.span.of(source), "[");

        let source = "{ 1 } ]";
        let error = parse(source).unwrap_err();
        assert_eq!(error.span.of(source), "]");
    }

    #[test]
    fn elements_display_re_parseably() {
        for source in [
            "1 2 +",
            "'sq {dup *} =",
            "{{*}}",
            "[ 1 2 ]",
            "&f",
            "obj.x",
            "obj.&x",
            // A parameter list prints back as one, not as the binds it holds.
            "{w h: w h *}",
            "{n: {n *}}",
            "{x:}",
        ] {
            let program = parsed(source);
            let text = program
                .iter()
                .map(Element::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(parsed(&text), program, "{source} did not round-trip");
        }
    }
}
