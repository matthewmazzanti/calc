//! Non-interactive evaluation: `calc -c "1 2 +"`.
//!
//! **The whole stack is the result**, one value per line, bottom to top. That
//! degenerates to a single line whenever a program leaves one value — so
//! `$(calc -c "2 3 +")` is `5` — while still telling the truth when it leaves
//! several. Printing only the top would make `1 2` and `2` indistinguishable,
//! and in a language where the stack persists between words that is a lie worth
//! avoiding.
//!
//! Values render exactly as the TUI shows them, so a string keeps its quotes and
//! a list its brackets. Where a raw form is wanted, the language already spells
//! it: `to_str`.

use crate::engine::{parse, Engine};

/// Evaluate `source` and render the resulting stack, or the error that stopped
/// it. Errors are already caller-facing text — this is the whole of what `-c`
/// needs, which is what keeps `main` free of logic.
pub fn evaluate(source: &str) -> Result<String, String> {
    let program = parse(source).map_err(|error| {
        format!(
            "{error} at column {} (`{}`)",
            error.span.column(source),
            error.span.of(source)
        )
    })?;
    let mut engine = Engine::new();
    engine.apply(&program).map_err(|error| error.to_string())?;
    Ok(engine
        .stack()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_result_is_the_whole_stack_bottom_to_top() {
        assert_eq!(evaluate("2 3 +").unwrap(), "5");
        assert_eq!(evaluate("1 2 3").unwrap(), "1\n2\n3");
        // Nothing left is not an error — it is an empty result.
        assert_eq!(evaluate("").unwrap(), "");
        assert_eq!(evaluate("1 drop").unwrap(), "");
    }

    #[test]
    fn values_render_as_the_stack_shows_them() {
        assert_eq!(evaluate(r#""hi""#).unwrap(), r#""hi""#);
        assert_eq!(evaluate("[1 2]").unwrap(), "[ 1 2 ]");
        assert_eq!(evaluate("'sq {dup *} =  &sq").unwrap(), "{dup *}");
        assert_eq!(evaluate(r#""hi" to_str"#).unwrap(), r#""hi""#);
    }

    #[test]
    fn a_definition_and_its_use_run_in_one_go() {
        // The session frame lasts for the whole `-c`, so a line may define and
        // then use — which is the shape a shell one-liner wants.
        assert_eq!(evaluate("'sq {dup *} =  7 sq").unwrap(), "49");
    }

    #[test]
    fn errors_come_back_as_text_naming_what_failed() {
        assert_eq!(
            evaluate("1 0 /").unwrap_err(),
            "divide by zero in `1 0 [/]`"
        );
        assert!(evaluate("nope").unwrap_err().contains("unbound name: nope"));
        // A syntax error locates itself, as it does in the TUI.
        assert_eq!(
            evaluate("1 2 ]").unwrap_err(),
            "unmatched `]` at column 5 (`]`)"
        );
    }
}
