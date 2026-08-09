//! Integration-level engine tests: behavior exercised end-to-end through
//! `parse` + `Engine::apply`. Type-local unit tests live next to their type
//! (`value.rs`, `program.rs`); this module is the behavioral suite.

use super::*;

/// Parse and run `input` on a fresh engine, and return it.
fn run(input: &str) -> Engine {
    let mut engine = Engine::new();
    engine.apply(&parse(input).unwrap()).unwrap();
    engine
}

#[test]
fn pushes_numbers() {
    assert_eq!(run("1 2 3").stack(), &[1.0, 2.0, 3.0]);
}

#[test]
fn parses_negatives_and_decimals() {
    assert_eq!(run("-1.5 2e3").stack(), &[-1.5, 2000.0]);
}

#[test]
fn arithmetic() {
    assert_eq!(run("1 2 +").stack(), &[3.0]);
    assert_eq!(run("10 3 -").stack(), &[7.0]);
    assert_eq!(run("4 5 *").stack(), &[20.0]);
    assert_eq!(run("20 4 /").stack(), &[5.0]);
}

#[test]
fn operand_order_is_left_to_right() {
    // `7 3 -` is 7 - 3, not 3 - 7.
    assert_eq!(run("7 3 -").stack(), &[4.0]);
    assert_eq!(run("8 2 /").stack(), &[4.0]);
}

/// The `ErrorKind` from running `input` against a fresh engine.
fn run_err(input: &str) -> ErrorKind {
    Engine::new()
        .apply(&parse(input).unwrap())
        .unwrap_err()
        .kind
}

#[test]
fn booleans_are_prelude_bindings_not_keywords() {
    // Applying the binding pushes its value — §1's "a value in the environment
    // is a nullary function" — so this reads exactly like a literal.
    assert_eq!(run("true false").stack(), &[true, false]);
    // But it *is* a binding: fetchable, nameable, and shadowable like any
    // builtin, with `del` as the recovery path (§9). The language has no
    // keywords, so there is nothing here the parser reserves.
    assert_eq!(run("&true").stack(), &[true]);
    assert_eq!(run("'true get").stack(), &[true]);
    assert_eq!(run("1 'true set true").stack(), &[Value::Int(1)]);
}

#[test]
fn comparisons_push_a_bool() {
    assert_eq!(run("1 2 <").stack(), &[true]);
    assert_eq!(run("1 2 >").stack(), &[false]);
    assert_eq!(run("2 2 <=").stack(), &[true]);
    assert_eq!(run("2 2 >=").stack(), &[true]);
    assert_eq!(run("3 2 >=").stack(), &[true]);
}

#[test]
fn equality_works_across_types() {
    assert_eq!(run("2 2 ==").stack(), &[true]);
    assert_eq!(run("2 3 ==").stack(), &[false]);
    assert_eq!(run("true true ==").stack(), &[true]);
    // A number never equals a bool — but it's not an error, just false.
    assert_eq!(run("1 true ==").stack(), &[false]);
}

#[test]
fn boolean_words_operate_on_bools() {
    assert_eq!(run("true not").stack(), &[false]);
    assert_eq!(run("true false and").stack(), &[false]);
    assert_eq!(run("true false or").stack(), &[true]);
    // Inequality is `=` then `not`.
    assert_eq!(run("2 3 == not").stack(), &[true]);
}

#[test]
fn arithmetic_on_a_bool_is_a_type_error() {
    assert_eq!(
        run_err("true 1 +"),
        ErrorKind::TypeError {
            expected: "number",
            found: "bool"
        }
    );
}

#[test]
fn logic_words_are_generic_over_bools_and_integers() {
    // One name per operation, logical on bools and bitwise on ints — which is
    // what keeps `& | ^ ~` out of the vocabulary, leaving `&` free to be the
    // fetch sigil.
    assert_eq!(run("6 3 and").stack(), &[Value::Int(2)]);
    assert_eq!(run("6 3 or").stack(), &[Value::Int(7)]);
    assert_eq!(run("6 3 xor").stack(), &[Value::Int(5)]);
    assert_eq!(run("true false xor").stack(), &[true]);
    assert_eq!(run("true true xor").stack(), &[false]);
    // `not` on an integer is the bitwise complement, as Python's `~` is.
    assert_eq!(run("5 not").stack(), &[Value::Int(-6)]);
    assert_eq!(run("true not").stack(), &[false]);
}

#[test]
fn logic_words_reject_floats_and_mixed_operands() {
    // No truthiness rule, and no mixing: the pair decides which reading applies,
    // so one of each is as much an error as a float (bitwise on an
    // approximation would be meaningless).
    assert_eq!(
        run_err("1.5 not"),
        ErrorKind::TypeError {
            expected: "bool or integer",
            found: "number"
        }
    );
    assert_eq!(
        run_err("true 1 and"),
        ErrorKind::TypeError {
            expected: "bool or integer",
            found: "number"
        }
    );
    assert_eq!(
        run_err(r#""s" 1 or"#),
        ErrorKind::TypeError {
            expected: "bool or integer",
            found: "string"
        }
    );
}

#[test]
fn a_type_error_names_the_mismatch_and_the_command() {
    // Ops no longer preserve their operands on error — atomicity is the
    // caller's transaction (see `an_error_leaves_the_callers_engine_untouched`).
    // The error still names the mismatch and which command failed.
    let err = Engine::new()
        .apply(&parse("true 1 +").unwrap())
        .unwrap_err();
    assert_eq!(
        err.kind,
        ErrorKind::TypeError {
            expected: "number",
            found: "bool"
        }
    );
    let call = &err.trace.unwrap().calls[0];
    assert_eq!(call.template[call.index], Element::Word(Rc::from("+")));
}

#[test]
fn bare_numbers_are_ints_dotted_ones_are_floats() {
    assert_eq!(run("3").stack(), &[Value::Int(3)]);
    assert_eq!(run("-5").stack(), &[Value::Int(-5)]);
    // A `.` or exponent forces a float.
    assert_eq!(run("3.0").stack(), &[Value::Num(3.0)]);
    assert_eq!(run("2e3").stack(), &[Value::Num(2000.0)]);
}

#[test]
fn integer_arithmetic_stays_integer() {
    assert_eq!(run("2 3 +").stack(), &[Value::Int(5)]);
    assert_eq!(run("2 3 -").stack(), &[Value::Int(-1)]);
    assert_eq!(run("4 5 *").stack(), &[Value::Int(20)]);
    assert_eq!(run("5 neg").stack(), &[Value::Int(-5)]);
}

#[test]
fn division_always_yields_a_float() {
    // Even when it divides evenly: `4 2 /` is `Num(2.0)`, not `Int(2)`.
    assert_eq!(run("4 2 /").stack(), &[Value::Num(2.0)]);
    assert_eq!(run("1 2 /").stack(), &[Value::Num(0.5)]);
}

#[test]
fn a_float_operand_promotes_the_whole_expression() {
    assert_eq!(run("2 3.0 +").stack(), &[Value::Num(5.0)]);
    assert_eq!(run("2.0 3 *").stack(), &[Value::Num(6.0)]);
}

#[test]
fn integer_overflow_promotes_to_float() {
    // i64::MAX * 2 can't be an Int, so it becomes a float rather than wrap.
    assert_eq!(
        run("9223372036854775807 2 *").stack(),
        &[Value::Num(9223372036854775807.0 * 2.0)]
    );
}

#[test]
fn equality_spans_the_int_float_split() {
    assert_eq!(run("2 2.0 ==").stack(), &[true]);
    assert_eq!(run("2 3.0 ==").stack(), &[false]);
}

#[test]
fn string_literals_hold_their_spaces() {
    assert_eq!(run(r#""hello""#).stack(), &[Value::from("hello")]);
    // The tokenizer's lookahead keeps the interior spaces as one token.
    assert_eq!(
        run(r#""hello world""#).stack(),
        &[Value::from("hello world")]
    );
}

#[test]
fn string_escapes_are_decoded() {
    assert_eq!(run(r#""a\nb\tc""#).stack(), &[Value::from("a\nb\tc")]);
    assert_eq!(
        run(r#""say \"hi\"""#).stack(),
        &[Value::from(r#"say "hi""#)]
    );
}

#[test]
fn plus_concatenates_two_strings() {
    assert_eq!(run(r#""foo" "bar" +"#).stack(), &[Value::from("foobar")]);
}

#[test]
fn plus_does_not_mix_strings_and_numbers() {
    // No implicit `to_str`: the numeric path rejects the string.
    assert_eq!(
        run_err(r#""foo" 1 +"#),
        ErrorKind::TypeError {
            expected: "number",
            found: "string"
        }
    );
}

#[test]
fn length_counts_characters() {
    assert_eq!(run(r#""hello" length"#).stack(), &[Value::Int(5)]);
    assert_eq!(run(r#""" length"#).stack(), &[Value::Int(0)]);
    assert_eq!(
        run_err("1 length"),
        ErrorKind::TypeError {
            expected: "string or list",
            found: "number"
        }
    );
}

#[test]
fn to_str_renders_any_value_unquoted() {
    assert_eq!(run("3 to_str").stack(), &[Value::from("3")]);
    assert_eq!(run("true to_str").stack(), &[Value::from("true")]);
    // Idempotent on a string.
    assert_eq!(run(r#""hi" to_str"#).stack(), &[Value::from("hi")]);
    // The doc's computed-name shape: build "x1" from a string and a number.
    assert_eq!(run(r#""x" 1 to_str +"#).stack(), &[Value::from("x1")]);
}

#[test]
fn strings_compare_by_content() {
    assert_eq!(run(r#""a" "a" =="#).stack(), &[true]);
    assert_eq!(run(r#""a" "b" =="#).stack(), &[false]);
    // A string never equals a number, even a look-alike.
    assert_eq!(run(r#"1 "1" =="#).stack(), &[false]);
}

#[test]
fn neg_flips_top() {
    assert_eq!(run("5 neg").stack(), &[-5.0]);
    assert_eq!(run("5 neg neg").stack(), &[5.0]);
}

#[test]
fn divide_by_zero_is_an_error() {
    assert_eq!(run_err("1 0 /"), ErrorKind::DivideByZero);
}

#[test]
fn underflow_is_an_error() {
    assert_eq!(run_err("1 +"), ErrorKind::StackUnderflow);
}

#[test]
fn errors_carry_the_trace_of_the_failing_command() {
    // No engine is attached (atomicity is the caller's); the error carries
    // the kind and a trace pointing at the command that failed.
    let err = Engine::new().apply(&parse("1 0 /").unwrap()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::DivideByZero);
    let trace = err.trace.unwrap();
    assert_eq!(trace.calls.len(), 1, "nothing was called, so one level");
    assert_eq!(trace.calls[0].index, 2);
    assert_eq!(trace.calls[0].template[2], Element::Word(Rc::from("/")));
}

#[test]
fn a_failed_batch_is_rolled_back_by_the_copy_taken_first() {
    // Atomicity is the caller's: a failed batch leaves the engine part-way
    // through, and the copy taken beforehand puts it back. A copy really is
    // independent — frames live in a map keyed by id and are copied on write —
    // so this is an assignment with nothing left to reconcile.
    let mut engine = run("1 'x set");
    let before = engine.clone();
    assert_eq!(
        engine
            .apply(&parse("2 'y set +").unwrap())
            .unwrap_err()
            .kind,
        ErrorKind::StackUnderflow
    );
    // Mid-failure the damage is real — `y` was bound before `+` failed.
    assert_eq!(engine.lookup("y"), Some(Value::Int(2)));
    engine = before;
    assert_eq!(engine.stack(), &[] as &[Value]);
    assert_eq!(engine.lookup("x"), Some(Value::Int(1)));
    assert_eq!(engine.lookup("y"), None);
}

#[test]
fn a_line_leaves_no_residue_of_its_execution() {
    // An activation is what is *currently executing*, so between lines there
    // are none — it pops when exhausted, and a failure clears what is left.
    // That is what lets equality mean "the same state" rather than "the same
    // state and the same last line", which the no-op check depends on: without
    // it every command would record an undo point.
    let mut engine = Engine::new();
    let rest = engine.clone();
    engine.apply(&parse("1 drop").unwrap()).unwrap();
    assert_eq!(engine, rest, "a no-op line left the engine changed");
    assert!(engine.apply(&parse("+").unwrap()).is_err());
    assert_eq!(engine, rest, "a failed line left its activation behind");
}

#[test]
fn the_session_frame_persists_across_evaluations() {
    // The *frame* is the continuous thing, not the activation: each line runs
    // in a new activation over the same session scope, so bindings accumulate.
    let mut engine = Engine::new();
    engine.apply(&parse("1 'x set").unwrap()).unwrap();
    engine.apply(&parse("2 'y set").unwrap()).unwrap();
    engine.apply(&parse("x y +").unwrap()).unwrap();
    assert_eq!(engine.stack(), &[Value::Int(3)]);
}

#[test]
fn a_snapshot_is_the_whole_engine_so_nothing_can_be_left_out() {
    // Everything a line touches rides along, including the frames it allocated
    // and the id counter. Rewinding the counter is safe precisely because an id
    // means something only inside the environment it was minted in: a value and
    // the `Env` naming it are always copied and restored together.
    let mut engine = Engine::new();
    let before = engine.clone();
    engine.apply(&parse("1 'x set").unwrap()).unwrap();
    let minted = engine.new_frame(Some(0));
    engine = before.clone();
    assert_eq!(engine.lookup("x"), None);
    assert_eq!(engine, before);
    // The id is free again, and the frame it named went with the state it was
    // minted into.
    assert_eq!(engine.new_frame(Some(0)), minted);
}

#[test]
fn apply_error_traces_the_program_and_the_failing_command() {
    // `1 2 + /`: after `+` the stack is [3]; `/` underflows at index 3.
    let err = Engine::new().apply(&parse("1 2 + /").unwrap()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::StackUnderflow);
    let trace = err.trace.clone().unwrap();
    assert_eq!(trace.calls[0].index, 3);
    assert_eq!(
        *trace.calls[0].template,
        [
            Element::Literal(Value::Int(1)),
            Element::Literal(Value::Int(2)),
            Element::Word(Rc::from("+")),
            Element::Word(Rc::from("/")),
        ]
    );
    // The message shows the whole batch with the failing command bracketed.
    assert_eq!(err.to_string(), "too few arguments in `1 2 + [/]`");
}

#[test]
fn a_trace_names_every_level_that_was_running() {
    // A flat program-plus-index cannot describe this: the failing element is
    // inside `sq`, whose template the line never mentions. Innermost first,
    // then outward to the line that reached it.
    let err = Engine::new()
        .apply(&parse("'sq {dup *} =  \"x\" sq").unwrap())
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "expected number, found string in `dup [*]`, called from `'sq {dup *} = \"x\" [sq]`"
    );
    let trace = err.trace.unwrap();
    assert_eq!(trace.calls.len(), 2);
    // `calls[0]` is always the line; the last is where it actually failed.
    assert_eq!(
        trace.calls[0].template[trace.calls[0].index].to_string(),
        "sq"
    );
    assert_eq!(
        trace.calls[1].template[trace.calls[1].index].to_string(),
        "*"
    );
}

#[test]
fn a_tail_call_leaves_no_level_in_the_trace() {
    // The bargain every language with proper tail calls makes: the frame that
    // was replaced cannot appear, because it no longer exists.
    let err = Engine::new()
        .apply(&parse("'boom {+} =  'go {boom} =  go").unwrap())
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::StackUnderflow);
    let trace = err.trace.unwrap();
    assert_eq!(
        trace.calls.len(),
        2,
        "`go`'s activation was replaced by the tail call to `boom`"
    );
}

#[test]
fn an_unknown_word_is_a_runtime_unbound_error() {
    // Parsing no longer fails on an unknown word — it becomes a `Word`; the
    // failure surfaces at runtime when it can't be resolved.
    assert_eq!(
        parse("1 2 + oops"),
        Ok(vec![
            Element::Literal(Value::Int(1)),
            Element::Literal(Value::Int(2)),
            Element::Word(Rc::from("+")),
            Element::Word(Rc::from("oops")),
        ])
    );
    assert_eq!(run_err("oops"), ErrorKind::UnboundName("oops".to_string()));
}

#[test]
fn dup_drop() {
    assert_eq!(run("3 dup").stack(), &[3.0, 3.0]);
    assert_eq!(run("3 4 drop").stack(), &[3.0]);
}

#[test]
fn swap_over() {
    assert_eq!(run("1 2 swap").stack(), &[2.0, 1.0]);
    assert_eq!(run("1 2 over").stack(), &[1.0, 2.0, 1.0]);
}

#[test]
fn rot_brings_third_to_top() {
    assert_eq!(run("1 2 3 rot").stack(), &[2.0, 3.0, 1.0]);
}

#[test]
fn clear_empties_the_stack() {
    assert!(run("1 2 3 clear").stack().is_empty());
}

#[test]
fn apply_runs_a_batch_of_elements() {
    // Elements built directly rather than parsed — the engine's own interface,
    // independent of the front end.
    let mut engine = Engine::new();
    engine
        .apply(&[
            Element::Literal(Value::Num(2.0)),
            Element::Literal(Value::Num(3.0)),
            Element::Word(Rc::from("*")),
        ])
        .unwrap();
    assert_eq!(engine.stack(), &[6.0]);
}

// --- M1: fixed shuffles and stack-consuming indexed ops ---

#[test]
fn fixed_shuffles() {
    assert_eq!(run("1 2 over").stack(), &[1.0, 2.0, 1.0]);
    assert_eq!(run("1 2 3 rot").stack(), &[2.0, 3.0, 1.0]);
    assert_eq!(run("1 2 3 unrot").stack(), &[3.0, 1.0, 2.0]);
    assert_eq!(run("1 2 nip").stack(), &[2.0]);
    assert_eq!(run("1 2 tuck").stack(), &[2.0, 1.0, 2.0]);
    assert_eq!(run("1 2 dupd").stack(), &[1.0, 1.0, 2.0]);
    assert_eq!(run("1 2 2dup").stack(), &[1.0, 2.0, 1.0, 2.0]);
    assert_eq!(run("1 2 3 2drop").stack(), &[1.0]);
}

#[test]
fn unrot_is_rot_inverted() {
    assert_eq!(run("1 2 3 rot unrot").stack(), &[1.0, 2.0, 3.0]);
}

#[test]
fn indexed_words_take_their_level_off_the_stack() {
    // `3 pickn` copies level 3 to the top (the level itself is consumed).
    assert_eq!(run("1 2 3 3 pickn").stack(), &[1.0, 2.0, 3.0, 1.0]);
    assert_eq!(run("1 2 3 3 rolln").stack(), &[2.0, 3.0, 1.0]);
    assert_eq!(run("1 2 3 3 rolldn").stack(), &[3.0, 1.0, 2.0]);
    assert_eq!(run("1 2 3 2 dropn").stack(), &[1.0, 3.0]);
    assert_eq!(run("1 2 3 2 swapn").stack(), &[2.0, 1.0, 3.0]);
}

#[test]
fn a_swapn_at_the_bottom_has_nothing_below() {
    assert_eq!(run_err("1 2 3 3 swapn"), ErrorKind::StackUnderflow);
}

#[test]
fn rolldn_inverts_rolln() {
    assert_eq!(
        run("1 2 3 4 3 rolln 3 rolldn").stack(),
        &[1.0, 2.0, 3.0, 4.0]
    );
}

#[test]
fn a_non_integer_level_is_rejected_not_rounded() {
    assert_eq!(
        run_err("1 2 3 2.5 rolln"),
        ErrorKind::TypeError {
            expected: "integer",
            found: "float"
        }
    );
}

#[test]
fn a_level_out_of_range_underflows() {
    assert_eq!(run_err("1 2 5 pickn"), ErrorKind::StackUnderflow);
    // A level of 0 is not a valid 1-based level either.
    assert_eq!(run_err("1 2 0 rolln"), ErrorKind::StackUnderflow);
}

#[test]
fn indexed_words_are_n_suffixed() {
    // The naming decision: the indexed shuffles carry an `n` suffix. They're
    // bound in the prelude and render as their word (a captured primitive
    // Displays by name).
    let base = prelude();
    for name in ["pickn", "rolln", "rolldn", "dropn", "swapn"] {
        let bound = base.get(name).expect("indexed word bound in the prelude");
        assert_eq!(bound.to_string(), name);
    }
}

// --- M2: lists and the mark discipline ---

/// Shorthand for a list value from a slice of values.
fn list(items: &[Value]) -> Value {
    Value::List(Rc::new(items.to_vec()))
}

#[test]
fn brackets_collect_a_list() {
    assert_eq!(run("[ ]").stack(), &[list(&[])]);
    assert_eq!(
        run("[ 1 2 3 ]").stack(),
        &[list(&[Value::Int(1), Value::Int(2), Value::Int(3)])]
    );
}

#[test]
fn lists_are_heterogeneous() {
    assert_eq!(
        run(r#"[ 1 true "x" ]"#).stack(),
        &[list(&[Value::Int(1), Value::Bool(true), Value::from("x")])]
    );
}

#[test]
fn lists_nest() {
    assert_eq!(
        run("[ 1 [ 2 3 ] 4 ]").stack(),
        &[list(&[
            Value::Int(1),
            list(&[Value::Int(2), Value::Int(3)]),
            Value::Int(4),
        ])]
    );
}

#[test]
fn words_run_while_collecting() {
    // The `+` fires inside the collection, so its result is an element.
    assert_eq!(
        run("[ 1 2 + 3 ]").stack(),
        &[list(&[Value::Int(3), Value::Int(3)])]
    );
    // Shuffles, too — they operate within the region.
    assert_eq!(
        run("[ 1 2 swap ]").stack(),
        &[list(&[Value::Int(2), Value::Int(1)])]
    );
}

#[test]
fn a_mark_is_a_typed_literal_not_a_floor() {
    // A value word rejects the mark as an operand — `[ 1 + ]` is a type error,
    // raised at the `+` before the region ever closes.
    assert_eq!(
        run_err("[ 1 + ]"),
        ErrorKind::TypeError {
            expected: "number",
            found: "open list"
        }
    );
    // Reaching the mark from an outer value type-errors the same way.
    assert_eq!(
        run_err("1 [ 2 + ]"),
        ErrorKind::TypeError {
            expected: "number",
            found: "open list"
        }
    );
    // Shuffles, though, move and copy the mark like any other value, so a
    // collection is not a sealed scope: `dup` leaves two marks, and the `]`
    // takes the nearer one — closing an empty list and leaving the other open.
    assert_eq!(
        run("[ dup ]").stack(),
        &[Value::Mark(MarkKind::List), list(&[])]
    );
    // An under-supplied shuffle therefore reshapes rather than erroring:
    // `rot` lifts the mark to the top, so `]` closes an empty list.
    assert_eq!(
        run("[ 1 2 rot ]").stack(),
        &[Value::Int(1), Value::Int(2), list(&[])]
    );
}

#[test]
fn a_region_reaches_backwards_over_the_stack() {
    // The mark is an ordinary stack value, so permutation can move it beneath
    // values that predate the region — which is what buys runtime sizing and
    // splicing (`language-v2.md` §6). `unrot` sends the mark below the 1 and 2,
    // so the `]` collects all three.
    assert_eq!(
        run("1 2 [unrot 3]").stack(),
        &[list(&[Value::Int(1), Value::Int(2), Value::Int(3)])]
    );
}

#[test]
fn a_region_must_pair_in_the_text() {
    // v2 pairs every bracket at parse time (§3), so an open collection can no
    // longer be left dangling for a later line to close — the v1 trick is gone.
    // What stays dynamic is *which* mark a closer consumes, not whether the
    // text balances (§6).
    assert_eq!(
        parse("[ 1 2").unwrap_err().kind,
        ParseErrorKind::UnclosedOpen('[')
    );
    assert_eq!(
        parse("1 2 ]").unwrap_err().kind,
        ParseErrorKind::UnmatchedClose(']')
    );
}

#[test]
fn a_closer_with_no_mark_left_is_a_runtime_error() {
    // Paired in the text, but `drop` ate the mark. Marks are meant to be linear
    // — one consumer, no `drop` (§6) — and until that discipline is enforced in
    // the primitives, the closer's own check is what catches it.
    assert_eq!(run_err("[ drop ]"), ErrorKind::UnmatchedClose);
}

#[test]
fn a_list_is_an_ordinary_value() {
    // It shuffles as one unit.
    assert_eq!(
        run("[ 1 2 ] dup").stack(),
        &[
            list(&[Value::Int(1), Value::Int(2)]),
            list(&[Value::Int(1), Value::Int(2)])
        ]
    );
}

#[test]
fn lists_compare_by_structure() {
    assert_eq!(run("[ 1 2 ] [ 1 2 ] ==").stack(), &[true]);
    assert_eq!(run("[ 1 2 ] [ 1 3 ] ==").stack(), &[false]);
}

#[test]
fn length_counts_list_elements() {
    assert_eq!(run("[ 1 2 3 ] length").stack(), &[Value::Int(3)]);
    assert_eq!(run("[ ] length").stack(), &[Value::Int(0)]);
}

#[test]
fn to_str_of_a_list_is_its_display() {
    assert_eq!(run("[ 1 2 ] to_str").stack(), &[Value::from("[ 1 2 ]")]);
}

#[test]
fn lists_display_space_padded() {
    assert_eq!(list(&[]).to_string(), "[ ]");
    assert_eq!(list(&[Value::Int(1), Value::Int(2)]).to_string(), "[ 1 2 ]");
    assert_eq!(
        list(&[Value::Int(1), list(&[Value::Int(2)])]).to_string(),
        "[ 1 [ 2 ] ]"
    );
    // A string element keeps its quotes inside a list.
    assert_eq!(list(&[Value::from("a")]).to_string(), r#"[ "a" ]"#);
}

// --- List operations ---

#[test]
fn first_and_rest_split_the_head() {
    assert_eq!(run("[ 1 2 3 ] first").stack(), &[Value::Int(1)]);
    assert_eq!(
        run("[ 1 2 3 ] rest").stack(),
        &[list(&[Value::Int(2), Value::Int(3)])]
    );
    // rest of a singleton is the empty list.
    assert_eq!(run("[ 1 ] rest").stack(), &[list(&[])]);
}

#[test]
fn first_and_rest_reject_an_empty_list() {
    assert_eq!(run_err("[ ] first"), ErrorKind::IndexOutOfRange);
    assert_eq!(run_err("[ ] rest"), ErrorKind::IndexOutOfRange);
}

#[test]
fn cons_prepends_an_element() {
    assert_eq!(
        run("1 [ 2 3 ] cons").stack(),
        &[list(&[Value::Int(1), Value::Int(2), Value::Int(3)])]
    );
    // Any value conses, onto the empty list too.
    assert_eq!(run(r#""x" [ ] cons"#).stack(), &[list(&[Value::from("x")])]);
}

#[test]
fn append_concatenates_two_lists() {
    assert_eq!(
        run("[ 1 2 ] [ 3 4 ] append").stack(),
        &[list(&[
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4)
        ])]
    );
    assert_eq!(run("[ ] [ ] append").stack(), &[list(&[])]);
}

#[test]
fn nth_indexes_zero_based() {
    assert_eq!(run("[ 10 20 30 ] 0 nth").stack(), &[Value::Int(10)]);
    assert_eq!(run("[ 10 20 30 ] 2 nth").stack(), &[Value::Int(30)]);
    assert_eq!(run_err("[ 10 20 30 ] 3 nth"), ErrorKind::IndexOutOfRange);
    // A negative index is out of range, not wrapped.
    assert_eq!(run_err("[ 10 20 ] -1 nth"), ErrorKind::IndexOutOfRange);
}

#[test]
fn list_ops_reject_non_lists() {
    assert_eq!(
        run_err("1 first"),
        ErrorKind::TypeError {
            expected: "list",
            found: "number"
        }
    );
    assert_eq!(
        run_err("1 2 append"),
        ErrorKind::TypeError {
            expected: "list",
            found: "number"
        }
    );
}

#[test]
fn list_ops_compose() {
    // cons then first round-trips the head.
    assert_eq!(run("9 [ 1 2 ] cons first").stack(), &[Value::Int(9)]);
    // build up with append, read back with nth.
    assert_eq!(run("[ 1 ] [ 2 3 ] append 1 nth").stack(), &[Value::Int(2)]);
}

#[test]
fn mutating_a_shared_value_copies_on_write() {
    // `dup` shares the underlying `Rc`; a mutating op (`make_mut`) must copy
    // so the other holder is untouched — the immutability guarantee.
    let one_two_three = list(&[Value::Int(1), Value::Int(2), Value::Int(3)]);

    // List `rest` on the shared top leaves the bottom copy intact.
    assert_eq!(
        run("[ 1 2 3 ] dup rest").stack(),
        &[one_two_three.clone(), list(&[Value::Int(2), Value::Int(3)])]
    );
    // List `append` onto the shared top.
    assert_eq!(
        run("[ 1 2 3 ] dup [ 4 ] append").stack(),
        &[
            one_two_three,
            list(&[Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)])
        ]
    );
    // String concat onto the shared top.
    assert_eq!(
        run(r#""ab" dup "c" +"#).stack(),
        &[Value::from("ab"), Value::from("abc")]
    );
}

// --- M3b: the environment ---

/// A name value from text.
fn name(s: &str) -> Value {
    Value::Name(Rc::from(s))
}

#[test]
fn quote_pushes_a_name() {
    assert_eq!(run("'x").stack(), &[name("x")]);
    // Names print with the quote so they're distinct from a look-alike.
    assert_eq!(name("1").to_string(), "'1");
    assert_ne!(name("1").to_string(), Value::Int(1).to_string());
}

#[test]
fn set_then_get_round_trips() {
    assert_eq!(run("3 'x set 'x get").stack(), &[Value::Int(3)]);
    // Any value binds — a list too.
    assert_eq!(
        run("[ 1 2 ] 'xs set 'xs get").stack(),
        &[list(&[Value::Int(1), Value::Int(2)])]
    );
}

#[test]
fn set_shadows_the_prior_binding() {
    assert_eq!(run("1 'x set 2 'x set 'x get").stack(), &[Value::Int(2)]);
}

#[test]
fn get_on_an_unbound_name_fails() {
    assert_eq!(run_err("'y get"), ErrorKind::UnboundName("y".to_string()));
}

#[test]
fn the_fetch_sigil_pushes_a_binding_unapplied() {
    // `&x` is what `'x get` spells the long way — application's reflective
    // inverse (§4). It's parser-owned, so it can't be shadowed the way `get`
    // can, and unlike `'x` it demands that the name be bound.
    assert_eq!(run("3 'x set &x").stack(), &[Value::Int(3)]);
    assert_eq!(run("&+ to_str").stack(), &[Value::from("+")]);
    assert_eq!(run_err("&nope"), ErrorKind::UnboundName("nope".to_string()));
}

#[test]
fn a_bind_element_binds_like_set() {
    // What `{x: …}` emits, applied on its own — the parameter tests cover it
    // in place, this one pins the element's own behavior.
    let program = [
        Element::Literal(Value::Int(3)),
        Element::Bind(Rc::from("x")),
        Element::Word(Rc::from("x")),
    ];
    let mut engine = Engine::new();
    engine.apply(&program).unwrap();
    assert_eq!(engine.stack(), &[3.0]);
    // It takes the top of the stack, so an empty one underflows — the same
    // failure `set` gives, since it is the same binding.
    assert_eq!(
        Engine::new().apply(&program[1..]).unwrap_err().kind,
        ErrorKind::StackUnderflow
    );
}

#[test]
fn the_parsed_but_unevaluated_surface_names_its_milestone() {
    // The parser accepts all of v2; the evaluator catches up at V5 for dicts
    // and attributes. Until then these are honest about which.
    assert_eq!(run_err("('x 1)"), ErrorKind::Unimplemented("dicts"));
    assert_eq!(
        run_err("3 .x"),
        ErrorKind::Unimplemented("attribute access")
    );
}

// --- V3: functions ---

#[test]
fn a_template_instantiates_into_a_function() {
    // `{ }` is a *value*, not a call: evaluating one pairs the template with
    // the frame the code is running in and leaves it on the stack.
    assert_eq!(run("{dup *}").stack().len(), 1);
    assert_eq!(run("{dup *}").stack()[0].to_string(), "{dup *}");
    // Two instantiations of the same template in the same scope are the same
    // value — the template is shared, not re-parsed (§5).
    assert_eq!(run("{dup *} {dup *} ==").stack(), &[true]);
}

#[test]
fn call_applies_the_function_on_top() {
    assert_eq!(run("3 {dup *} call").stack(), &[Value::Int(9)]);
    // A value is a nullary function that pushes itself, so calling one is a
    // push rather than an error (§1).
    assert_eq!(run("3 call").stack(), &[Value::Int(3)]);
    // Builtins reach the same seam, so primitive-vs-function is invisible.
    assert_eq!(run("3 4 &+ call").stack(), &[Value::Int(7)]);
}

#[test]
fn a_word_bound_to_a_function_runs_when_applied() {
    assert_eq!(run("'sq {dup *} =  4 sq").stack(), &[Value::Int(16)]);
    // `&sq` fetches it instead, unapplied — application's reflective inverse.
    assert_eq!(run("'sq {dup *} =  4 &sq call").stack(), &[Value::Int(16)]);
}

#[test]
fn both_binders_reach_the_same_frame_differing_only_in_order() {
    // §5: `=` takes the name first, `set` the value first, and both are
    // primitive. `=` suits a definition, whose value is a literal and so pushes
    // everything it consumes; `set` suits a computed value, where a name pushed
    // first would be in the expression's way.
    assert_eq!(run("'x 3 =  x").stack(), &[Value::Int(3)]);
    assert_eq!(run("3 'x set  x").stack(), &[Value::Int(3)]);
    // Each wants on top what the other wants underneath, so a swapped pair is a
    // type error rather than a silent misbinding.
    assert_eq!(
        run_err("3 'x ="),
        ErrorKind::TypeError {
            expected: "name",
            found: "number"
        }
    );
    assert_eq!(
        run_err("'x 3 set"),
        ErrorKind::TypeError {
            expected: "name",
            found: "number"
        }
    );
}

#[test]
fn a_definition_binds_into_the_frame_it_captured() {
    // The shape that would be a reference cycle if `env` were a pointer: the
    // session frame ends up holding a function whose captured environment is
    // that same frame (`memory-model.md` §0.2). It stores an id, so there is
    // nothing to collect — and the function still sees definitions made after
    // it, which is the same fact from the other side.
    assert_eq!(run("'sq {n *} =  'n 5 =  5 sq").stack(), &[Value::Int(25)]);
}

#[test]
fn parameters_bind_into_the_calls_own_frame() {
    assert_eq!(run("3 4 {w h: w h *} call").stack(), &[Value::Int(12)]);
    // The names read bottom to top, so the rightmost took the top of stack.
    assert_eq!(run("10 3 {a b: a b -} call").stack(), &[Value::Int(7)]);
    // And they are *locals*: the call's frame is gone once it returns.
    assert_eq!(
        run_err("3 {n: n} call n"),
        ErrorKind::UnboundName("n".into())
    );
}

#[test]
fn a_callee_sees_its_definitions_scope_not_its_callers() {
    // §8's example, exactly: `f` resolves `y` through *its* chain. No uplevel,
    // no dynamic override — the call frame's parent is the captured env.
    let source = "{y 1 +} 'f set  {3 'y set f} 'a set  7 'y set  a";
    assert_eq!(run(source).stack(), &[Value::Int(8)]);
}

#[test]
fn if_applies_one_of_two_functions() {
    assert_eq!(run("true {1} {2} if").stack(), &[Value::Int(1)]);
    assert_eq!(run("false {1} {2} if").stack(), &[Value::Int(2)]);
    // A branch is an ordinary value, so it need not be a function.
    assert_eq!(run("true 1 2 if").stack(), &[Value::Int(1)]);
    // No truthiness — the condition is a genuine boolean.
    assert_eq!(
        run_err("1 {1} {2} if"),
        ErrorKind::TypeError {
            expected: "bool",
            found: "number"
        }
    );
}

#[test]
fn binding_is_late_so_recursion_needs_no_declaration() {
    // The closure captured the session frame before `fac` was bound into it,
    // and resolves the name when it *runs* — which is the whole reason the
    // captured environment is an id rather than a copy of the bindings.
    let fac = "{n: n 1 <= {1} {n 1 - fac n *} if} 'fac set";
    assert_eq!(run(&format!("{fac}  5 fac")).stack(), &[Value::Int(120)]);
}

#[test]
fn a_call_that_neither_binds_nor_captures_allocates_no_frame() {
    // Most calls in a concatenative language touch the environment only to
    // *read* it, and a frame that stays empty adds nothing to a lookup chain —
    // so the call runs against the one it inherited (`memory-model.md` §7.2).
    let mut engine = Engine::new();
    engine.apply(&parse("'inc {1 +} =").unwrap()).unwrap();
    let before = engine.next_frame;
    engine.apply(&parse("1 inc inc inc").unwrap()).unwrap();
    assert_eq!(engine.stack(), &[Value::Int(4)]);
    assert_eq!(
        engine.next_frame, before,
        "a bindless call allocated a frame"
    );
}

#[test]
fn a_call_that_binds_allocates_one_frame() {
    let mut engine = Engine::new();
    engine.apply(&parse("'sq {n: n n *} =").unwrap()).unwrap();
    let before = engine.next_frame;
    engine.apply(&parse("4 sq").unwrap()).unwrap();
    assert_eq!(engine.stack(), &[Value::Int(16)]);
    assert_eq!(engine.next_frame, before + 1);
}

#[test]
fn capture_allocates_the_frame_a_later_bind_lands_in() {
    // The case that makes "allocate on first bind" wrong on its own. The inner
    // `{x}` must capture the frame the *later* `set` writes to — if capture
    // borrowed the inherited frame and the `set` then allocated a fresh one,
    // the closure would resolve `x` against the wrong environment and fail.
    let source = "'f {  {x} 'peek set   3 'x set   peek call  } =  f";
    assert_eq!(run(source).stack(), &[Value::Int(3)]);
}

#[test]
fn dead_frames_are_collected_mid_line() {
    // The whole point of collecting mid-line rather than at a boundary: a
    // loop's frames are born and die *within* one evaluation, so waiting for
    // the boundary would leave the peak untouched.
    let mut engine = Engine::new();
    let source = "'go {n: n 0 <= {0} {n 1 - go} if} =  20000 go";
    engine.apply(&parse(source).unwrap()).unwrap();
    assert_eq!(engine.stack(), &[Value::Int(0)]);
    assert!(
        engine.env.len() <= MIN_FRAMES,
        "20000 calls left {} frames behind",
        engine.env.len()
    );
}

#[test]
fn a_closure_on_the_stack_keeps_its_frame() {
    // The stack is a root, and it has to be: this closure is the only thing
    // naming the frame that holds `n`.
    let mut engine = Engine::new();
    engine
        .apply(&parse("'make {n: {n}} =  5 make").unwrap())
        .unwrap();
    engine.collect();
    engine.apply(&parse("call").unwrap()).unwrap();
    assert_eq!(engine.stack(), &[Value::Int(5)]);
}

#[test]
fn marking_reaches_closures_inside_aggregates() {
    // `memory-model.md` §4.1's invariant 2 — the bridge from the refcounted
    // world into the traced one. A list is an `Rc` the collector doesn't own,
    // and a closure inside one is reachable *only* through it.
    let mut engine = Engine::new();
    engine
        .apply(&parse("'make {n: {n}} =  [ 7 make ]").unwrap())
        .unwrap();
    engine.collect();
    engine.apply(&parse("first call").unwrap()).unwrap();
    assert_eq!(engine.stack(), &[Value::Int(7)]);
}

#[test]
fn a_live_call_chains_frames_are_roots() {
    // Deep recursion holds every frame at once — nothing here is garbage, and
    // a collection mid-way must not decide otherwise.
    let mut engine = Engine::new();
    let source = "'deep {n: n 0 <= {0} {n 1 - deep 0 +} if} =  3000 deep";
    engine.apply(&parse(source).unwrap()).unwrap();
    assert_eq!(engine.stack(), &[Value::Int(0)]);
}

#[test]
fn a_tail_call_replaces_an_exhausted_activation() {
    // The mechanism, directly: a call made when the caller has nothing left to
    // run takes its place rather than stacking on it. Iteration here is
    // recursion over combinators, so without this an unbounded loop grows the
    // activation stack without bound.
    let body: Template = Rc::new(vec![Element::Word(Rc::from("dup"))]);
    let mut engine = Engine::new();
    engine.push_call(Rc::clone(&body), 1);
    engine.push_call(Rc::clone(&body), 1); // caller mid-template: stacks
    let depth = engine.calls.len();
    assert_eq!(depth, 2);

    engine.calls.last_mut().unwrap().ip = body.len(); // now in tail position
    engine.push_call(body, 1);
    assert_eq!(engine.calls.len(), depth, "a tail call grew the stack");
}

#[test]
fn deep_recursion_completes() {
    // End to end. The loop is explicit, so depth costs heap rather than Rust
    // stack either way — what this pins is that the whole path (parameters,
    // `if`, late-bound self-reference, returning) survives being run 20000
    // times over.
    let countdown = "{n: n 0 <= {0} {n 1 - countdown} if} 'countdown set";
    assert_eq!(
        run(&format!("{countdown}  20000 countdown")).stack(),
        &[Value::Int(0)]
    );
}

#[test]
fn set_and_get_want_a_name() {
    // `set`'s name operand is on top; a number there is a type error.
    assert_eq!(
        run_err("3 4 set"),
        ErrorKind::TypeError {
            expected: "name",
            found: "number"
        }
    );
    assert_eq!(
        run_err("3 get"),
        ErrorKind::TypeError {
            expected: "name",
            found: "number"
        }
    );
}

#[test]
fn names_compare_by_text() {
    assert_eq!(run("'x 'x ==").stack(), &[true]);
    assert_eq!(run("'x 'y ==").stack(), &[false]);
}

#[test]
fn to_str_of_a_name_is_its_bare_text() {
    assert_eq!(run("'x to_str").stack(), &[Value::from("x")]);
}

#[test]
fn a_bound_value_shares_but_get_plus_mutation_copies_on_write() {
    // `foo` holds a list; `get` shares it (Rc bump). Mutating the retrieved
    // copy must not corrupt the binding — the durable-alias case that made
    // us pick Rc + copy-on-write.
    assert_eq!(
        run("[ 1 2 3 ] 'foo set 'foo get rest 'foo get").stack(),
        &[
            list(&[Value::Int(2), Value::Int(3)]),
            list(&[Value::Int(1), Value::Int(2), Value::Int(3)]),
        ]
    );
}

// --- bare-word lookup ---

#[test]
fn a_bare_word_pushes_its_binding() {
    assert_eq!(run("3 'x set x").stack(), &[Value::Int(3)]);
    // A bare word and `get` retrieve the same value.
    assert_eq!(run("3 'x set x").stack(), run("3 'x set 'x get").stack());
}

#[test]
fn a_user_binding_shadows_a_builtin() {
    // Rebinding `dup` makes the bare word push the binding, not duplicate —
    // user bindings sit "before" the builtin prelude in resolution.
    assert_eq!(run("5 'dup set 1 2 dup").stack(), &[1.0, 2.0, 5.0]);
}

#[test]
fn builtins_are_reached_by_the_same_lookup() {
    // `+` is a word resolved to a prelude binding — no special parse case;
    // `get` reaches it through the same lookup as any user binding.
    assert_eq!(run("'+ get").stack()[0].to_string(), "+");
    assert_eq!(run("'+ get").stack()[0].type_name(), "builtin");
    assert_eq!(run_err("nope"), ErrorKind::UnboundName("nope".to_string()));
}

#[test]
fn a_captured_builtin_runs_when_applied() {
    // `get` captures the op as a value; binding it to a name and applying
    // that name runs it — first-class words end to end.
    assert_eq!(run("3 4 '+ get 'plus set plus").stack(), &[7.0]);
}

#[test]
fn every_primitive_is_in_the_prelude() {
    // The `ops` category tables are the source of the vocabulary; this guards
    // that the prelude binds each one under its canonical word.
    let base = prelude();
    for p in ops::primitives() {
        assert_eq!(
            base.get(p.name),
            Some(&Value::Builtin(p)),
            "prelude missing `{}`",
            p.name,
        );
    }
}
