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
    assert_eq!(run("{true} call").stack(), &[true]);
    assert_eq!(run("'true get").stack(), &[true]);
    // Suspended, it is the nullary function §1 says a bound value is.
    assert_eq!(run("{true}").stack()[0].type_name(), "function");
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
fn abs_and_inv() {
    assert_eq!(run("5 abs").stack(), &[Value::Int(5)]);
    assert_eq!(run("-5 abs").stack(), &[Value::Int(5)]);
    assert_eq!(run("-2.5 abs").stack(), &[Value::Num(2.5)]);
    assert_eq!(run("4 inv").stack(), &[Value::Num(0.25)]);
    // The reciprocal of zero is the division it is, not a generic `Undefined`.
    assert_eq!(run_err("0 inv"), ErrorKind::DivideByZero);
}

#[test]
fn power_preserves_integers_where_it_can() {
    assert_eq!(run("2 10 ^").stack(), &[Value::Int(1024)]);
    // A negative exponent has no integer answer, so it promotes rather than
    // truncating to zero.
    assert_eq!(run("2 -1 ^").stack(), &[Value::Num(0.5)]);
    assert_eq!(
        run("2 0.5 ^").stack(),
        &[Value::Num(std::f64::consts::SQRT_2)]
    );
    // Overflowing the `i64` promotes rather than wrapping.
    assert!(matches!(run("2 200 ^").stack(), [Value::Num(_)]));
}

#[test]
fn power_is_an_ordinary_word_named_with_an_operator() {
    // `^` is a name, not syntax — so it is fetchable and rebindable like any
    // other word, which is what keeps the vocabulary uniform.
    assert_eq!(run("{^} 'p set 3 4 p").stack(), &[Value::Int(81)]);
}

#[test]
fn rounding_yields_integers() {
    // Not `3.0` — a rounded value can feed `nth` or `dup-at` directly.
    assert_eq!(run("3.7 floor").stack(), &[Value::Int(3)]);
    assert_eq!(run("-3.2 floor").stack(), &[Value::Int(-4)]);
    assert_eq!(run("3.2 ceil").stack(), &[Value::Int(4)]);
    assert_eq!(run("2.5 round").stack(), &[Value::Int(3)]);
    assert_eq!(run("-2.7 trunc").stack(), &[Value::Int(-2)]);
    // An `Int` is already integral.
    assert_eq!(run("7 floor").stack(), &[Value::Int(7)]);
    assert_eq!(run("[ 1 2 3 ] 2.7 floor nth").stack(), &[Value::Int(3)]);
}

#[test]
fn rounding_a_float_too_large_for_an_integer_stays_a_float() {
    // `as` would saturate to `i64::MAX`, which is a wrong answer rather than a
    // lossy one.
    assert_eq!(run("1e30 floor").stack(), &[Value::Num(1e30)]);
}

#[test]
fn percent_is_floored_not_truncated() {
    // The result takes the sign of the divisor, so a value cycles into `0..b`
    // for a positive `b` whatever the sign of `a` — Rust's `%` would hand back
    // `-1` for the second case, a negative index needing correction by hand.
    assert_eq!(run("7 3 %").stack(), &[Value::Int(1)]);
    assert_eq!(run("-7 3 %").stack(), &[Value::Int(2)]);
    // A negative divisor is where floored parts company with `rem_euclid`,
    // which would give `1` here.
    assert_eq!(run("7 -3 %").stack(), &[Value::Int(-2)]);
    assert_eq!(run("-7 -3 %").stack(), &[Value::Int(-1)]);
    assert_eq!(run("7.5 2 %").stack(), &[Value::Num(1.5)]);
    assert_eq!(run("-7.5 2 %").stack(), &[Value::Num(0.5)]);
}

#[test]
fn percent_by_zero_is_the_division_it_is() {
    assert_eq!(run_err("7 0 %"), ErrorKind::DivideByZero);
    assert_eq!(run_err("7.0 0.0 %"), ErrorKind::DivideByZero);
}

#[test]
fn percent_survives_the_overflowing_divisor() {
    // `i64::MIN % -1` overflows the `%` operator; every value mod -1 is 0.
    assert_eq!(run("7 -1 %").stack(), &[Value::Int(0)]);
    assert_eq!(run("-9223372036854775808 -1 %").stack(), &[Value::Int(0)]);
}

#[test]
fn min_and_max_return_the_winning_operand() {
    assert_eq!(run("2 3 min").stack(), &[Value::Int(2)]);
    assert_eq!(run("2 3 max").stack(), &[Value::Int(3)]);
    assert_eq!(run("-1 -2 min").stack(), &[Value::Int(-2)]);
    // The operand comes back rather than a recomputed float, so comparing an
    // `Int` against a float leaves the `Int` an `Int`.
    assert_eq!(run("2 3.5 min").stack(), &[Value::Int(2)]);
    assert_eq!(run("3.5 2 max").stack(), &[Value::Num(3.5)]);
    // Equal as numbers but not as values: the deeper operand wins, by choice.
    assert_eq!(run("2 2.0 min").stack(), &[Value::Int(2)]);
}

#[test]
fn log_to_an_arbitrary_base() {
    assert_eq!(run("81 3 logb").stack(), &[Value::Num(4.0)]);
    assert_eq!(run("8 2 logb").stack(), &[Value::Num(3.0)]);
    assert_eq!(run("3.7 log2").stack(), &[Value::Num(3.7f64.log2())]);
}

#[test]
fn logb_dispatches_the_exact_bases() {
    // `ln x / ln b` rounds twice and drifts off exact answers — it computes
    // 2.9999999999999996 here. Visible at any display precision, so bases 10
    // and 2 go to the dedicated kernels instead.
    assert_eq!(run("1000 10 logb").stack(), &[Value::Num(3.0)]);
    assert_eq!(run("0.001 10 logb").stack(), &[Value::Num(-3.0)]);
    assert_eq!(run("1024 2 logb").stack(), &[Value::Num(10.0)]);
    assert_ne!(
        1000f64.ln() / 10f64.ln(),
        3.0,
        "the general form really does drift"
    );
}

#[test]
fn logb_rejects_bases_and_arguments_with_no_logarithm() {
    assert_eq!(run_err("0 10 logb"), ErrorKind::Undefined);
    assert_eq!(run_err("-1 10 logb"), ErrorKind::Undefined);
    // Base 1 divides by `ln 1 == 0`.
    assert_eq!(run_err("10 1 logb"), ErrorKind::Undefined);
    // Base 0 is the case a finiteness check alone would miss: `ln x / -inf` is
    // a finite `-0`, so it looks defined when the operation is not.
    assert_eq!(run_err("10 0 logb"), ErrorKind::Undefined);
    assert_eq!(run_err("10 -2 logb"), ErrorKind::Undefined);
}

#[test]
fn transcendentals_and_constants() {
    assert_eq!(run("16 sqrt").stack(), &[Value::Num(4.0)]);
    assert_eq!(run("100 log").stack(), &[Value::Num(2.0)]);
    assert_eq!(run("e ln").stack(), &[Value::Num(1.0)]);
    assert_eq!(run("0 sin").stack(), &[Value::Num(0.0)]);
    assert_eq!(run("pi cos").stack(), &[Value::Num(-1.0)]);
    assert_eq!(run("tau").stack(), &[Value::Num(std::f64::consts::TAU)]);
    assert_eq!(
        run("1 1 atan2").stack(),
        &[Value::Num(std::f64::consts::FRAC_PI_4)]
    );
}

#[test]
fn log_is_base_ten_and_ln_is_natural() {
    // The calculator convention, not C's `log`-means-natural — the two names
    // are only worth having if they differ.
    assert_eq!(run("1000 log").stack(), &[Value::Num(3.0)]);
    assert_eq!(run("1 ln").stack(), &[Value::Num(0.0)]);
}

#[test]
fn trig_is_radians_with_explicit_conversion() {
    // No angle mode: `to_rad`/`to_deg` are ordinary words, so what `sin` means
    // never depends on hidden state.
    assert_eq!(
        run("180 to_rad").stack(),
        &[Value::Num(std::f64::consts::PI)]
    );
    assert_eq!(run("pi to_deg").stack(), &[Value::Num(180.0)]);
    let engine = run("30 to_rad sin");
    let [Value::Num(x)] = engine.stack() else {
        panic!("expected one float")
    };
    assert!((x - 0.5).abs() < 1e-12, "{x} should be about 0.5");
}

#[test]
fn leaving_a_functions_domain_is_an_error_not_a_nan() {
    // A NaN is worse than a wrong answer because it is a silent one: it would
    // propagate through every later op, surfacing far from the word at fault.
    assert_eq!(run_err("-1 sqrt"), ErrorKind::Undefined);
    assert_eq!(run_err("0 ln"), ErrorKind::Undefined);
    assert_eq!(run_err("-1 log"), ErrorKind::Undefined);
    assert_eq!(run_err("2 asin"), ErrorKind::Undefined);
    assert_eq!(run_err("2 acos"), ErrorKind::Undefined);
    // Overflow out of the float range is refused on the same grounds.
    assert_eq!(run_err("1000 exp"), ErrorKind::Undefined);
    assert_eq!(run_err("-1 0.5 ^"), ErrorKind::Undefined);
}

#[test]
fn math_words_reject_non_numbers() {
    assert_eq!(
        run_err("true sqrt"),
        ErrorKind::TypeError {
            expected: "number",
            found: "bool",
        }
    );
    assert_eq!(
        run_err(r#""x" floor"#),
        ErrorKind::TypeError {
            expected: "number",
            found: "string",
        }
    );
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
    // state and the same last line": a line that ends where it started must
    // leave nothing behind for the next line to trip over, and a failed line
    // least of all.
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
    // and the slots they occupied. Reusing a slot after a rollback is safe
    // precisely because an id means something only inside the environment it
    // was minted in: a value and the `Env` naming it are always copied and
    // restored together, so the old occupant went with the state it lived in.
    let mut engine = Engine::new();
    let before = engine.clone();
    engine.apply(&parse("1 'x set").unwrap()).unwrap();
    let minted = engine.new_frame(None);
    engine = before.clone();
    assert_eq!(engine.lookup("x"), None);
    assert_eq!(engine, before);
    // Slot allocation rolled back too, so the very same id is handed out again
    // — *not* a hazard, and the clearest evidence the snapshot is total. The
    // frame that held it was discarded along with every value that named it,
    // because a value and the `Env` naming it always travel together.
    assert_eq!(engine.new_frame(None), minted);
}

#[test]
fn a_swept_id_does_not_alias_the_frame_that_replaces_it() {
    // What the generation *is* for: reuse within one version. A collection
    // frees a slot and the next frame takes it, so without a generation an id
    // held by a missed root would silently resolve to a stranger's frame
    // instead of failing loudly.
    let mut engine = Engine::new();
    let dead = engine.new_frame(None); // reachable from nothing
    engine.collect();
    assert!(
        engine.env.frame(dead).is_none(),
        "the collector kept an unreachable frame"
    );
    let fresh = engine.new_frame(None); // same slot, later generation
    assert_ne!(fresh, dead);
    assert!(engine.env.frame(dead).is_none(), "a stale id found a frame");
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
    assert_eq!(run("1 2 dup2").stack(), &[1.0, 2.0, 1.0, 2.0]);
    assert_eq!(run("1 2 3 drop2").stack(), &[1.0]);
}

#[test]
fn the_dup_family_agrees_at_level_one() {
    // `-at` names one item by depth, `-to` a run by width. At level 1 they are
    // the same item, which is what lets a bare `dup` belong to both families
    // without saying which.
    assert_eq!(run("1 2 1 dup-at").stack(), run("1 2 dup").stack());
    assert_eq!(run("1 2 1 dup-to").stack(), run("1 2 dup").stack());
}

#[test]
fn the_dup_family_diverges_above_level_one() {
    // The whole point of the split: at level 2 one copies the single item there,
    // the other copies everything down to it.
    assert_eq!(run("1 2 2 dup-at").stack(), run("1 2 over").stack());
    assert_eq!(run("1 2 2 dup-to").stack(), run("1 2 dup2").stack());
    assert_eq!(run("1 2 3 3 dup-at").stack(), run("1 2 3 pick").stack());
    assert_eq!(run("1 2 3 3 dup-to").stack(), run("1 2 3 dup3").stack());
    assert_eq!(
        run("1 2 3 3 dup-to").stack(),
        &[1.0, 2.0, 3.0, 1.0, 2.0, 3.0]
    );
}

#[test]
fn the_drop_family_splits_the_same_way() {
    // The pair that motivated the scheme: `dropn` and `2drop` were depth and
    // width under names that gave no hint which, and Forth spells the width one
    // `ndrop`. Level 1 agrees, and above it the word says which it means.
    assert_eq!(run("1 2 1 drop-at").stack(), run("1 2 drop").stack());
    assert_eq!(run("1 2 1 drop-to").stack(), run("1 2 drop").stack());
    assert_eq!(run("1 2 3 2 drop-at").stack(), run("1 2 3 nip").stack());
    assert_eq!(run("1 2 3 2 drop-to").stack(), run("1 2 3 drop2").stack());
    assert_eq!(run("1 2 3 3 drop-to").stack(), run("1 2 3 drop3").stack());
    assert_eq!(run("1 2 3 3 drop-to").stack(), &[] as &[Value]);
}

#[test]
fn pick_is_factors_fixed_level_three_not_forths_indexed_one() {
    // Worth pinning because the conventions disagree and ours fails *silently*.
    // Forth's `pick` is indexed and consumes its index: `1 2 3 2 pick` leaves
    // `1 2 3 1`. Ours is a fixed level 3, so the `2` stays on the stack and
    // shifts what level 3 means — level 3 of `1 2 3 2` is the `2` — leaving a
    // different value *and* a different depth.
    assert_eq!(run("1 2 3 pick").stack(), &[1.0, 2.0, 3.0, 1.0]);
    assert_eq!(run("1 2 3 2 pick").stack(), &[1.0, 2.0, 3.0, 2.0, 2.0]);
}

#[test]
fn unrot_is_rot_inverted() {
    assert_eq!(run("1 2 3 rot unrot").stack(), &[1.0, 2.0, 3.0]);
}

#[test]
fn indexed_words_take_their_level_off_the_stack() {
    // `3 dup-at` copies level 3 to the top (the level itself is consumed).
    assert_eq!(run("1 2 3 3 dup-at").stack(), &[1.0, 2.0, 3.0, 1.0]);
    // `dup-to` takes the whole run down to that level, not the one item at it.
    assert_eq!(run("1 2 3 2 dup-to").stack(), &[1.0, 2.0, 3.0, 2.0, 3.0]);
    assert_eq!(run("1 2 3 3 rot-to").stack(), &[2.0, 3.0, 1.0]);
    assert_eq!(run("1 2 3 3 unrot-to").stack(), &[3.0, 1.0, 2.0]);
    assert_eq!(run("1 2 3 2 drop-at").stack(), &[1.0, 3.0]);
    // `drop-to` clears the whole run down to that level, not the one item at it.
    assert_eq!(run("1 2 3 2 drop-to").stack(), &[1.0]);
    assert_eq!(run("1 2 3 2 swap-at").stack(), &[1.0, 3.0, 2.0]);
    assert_eq!(run("1 2 3 3 swap-to").stack(), &[3.0, 2.0, 1.0]);
}

#[test]
fn the_swap_family_reaches_for_the_top_not_the_neighbour() {
    // Both ends of the span, or the whole span reversed. They agree up to the
    // arity — level 1 is the identity for both, level 2 is the usual swap, and
    // level 3 has only one item between the ends — and part company at 4.
    assert_eq!(run("1 2 1 swap-at").stack(), run("1 2").stack());
    assert_eq!(run("1 2 1 swap-to").stack(), run("1 2").stack());
    assert_eq!(run("1 2 2 swap-at").stack(), run("1 2 swap").stack());
    assert_eq!(run("1 2 2 swap-to").stack(), run("1 2 swap").stack());
    assert_eq!(run("1 2 3 3 swap-at").stack(), run("1 2 3 swap3").stack());

    assert_eq!(run("1 2 3 4 4 swap-at").stack(), &[4.0, 2.0, 3.0, 1.0]);
    assert_eq!(run("1 2 3 4 4 swap-to").stack(), &[4.0, 3.0, 2.0, 1.0]);

    // Reaching for the top means there is no "nothing below" case: any level
    // the stack actually has works, where exchanging with the neighbour below
    // used to fail at the bottom.
    assert_eq!(run("1 2 3 3 swap-at").stack(), &[3.0, 2.0, 1.0]);
    assert_eq!(run_err("1 2 3 4 swap-at"), ErrorKind::StackUnderflow);
}

#[test]
fn unrot_to_inverts_rot_to() {
    assert_eq!(
        run("1 2 3 4 3 rot-to 3 unrot-to").stack(),
        &[1.0, 2.0, 3.0, 4.0]
    );
}

#[test]
fn a_non_integer_level_is_rejected_not_rounded() {
    assert_eq!(
        run_err("1 2 3 2.5 rot-to"),
        ErrorKind::TypeError {
            expected: "integer",
            found: "float"
        }
    );
}

#[test]
fn a_level_out_of_range_underflows() {
    assert_eq!(run_err("1 2 5 dup-at"), ErrorKind::StackUnderflow);
    assert_eq!(run_err("1 2 5 dup-to"), ErrorKind::StackUnderflow);
    // A level of 0 is not a valid 1-based level either.
    assert_eq!(run_err("1 2 0 rot-to"), ErrorKind::StackUnderflow);
}

#[test]
fn indexed_words_are_named_for_their_index() {
    // The naming decision: an indexed shuffle says so in its name. The dup pair
    // spells out which target it takes — `-at` positioned at a level, `-to`
    // spanning the top down to it. Rot is `-to` only, being a span operation
    // whose ends-only form is already called `swap-at`. They're bound in the prelude and
    // render as their word (a captured primitive Displays by name).
    let base = prelude();
    for name in [
        "dup-at", "dup-to", "drop-at", "drop-to", "swap-at", "swap-to", "rot-to", "unrot-to",
    ] {
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
    // `get` applies the binding, so a bound value lands back on the stack — the
    // name arriving as a value is the only difference from writing `x`.
    assert_eq!(run("3 'x set 'x get").stack(), &[Value::Int(3)]);
    // Any value binds — a list too.
    assert_eq!(
        run("[ 1 2 ] 'xs set 'xs get").stack(),
        &[list(&[Value::Int(1), Value::Int(2)])]
    );
    // And a function binding *runs*, since that is what applying one does.
    assert_eq!(run("'sq {dup *} =  4 'sq get").stack(), &[Value::Int(16)]);
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
fn a_one_word_template_is_the_suspension_idiom() {
    // What `&x` used to spell (§4). There is no sigil for it, because there is
    // nothing for one to add: `{x}` is an ordinary template whose body happens
    // to be a single word, so suspending a word and suspending anything else
    // are the same construct.
    //
    // Whatever `x` is bound to, the same thing comes back — a function — so
    // there is no lift for a data binding and no unwrapping for a callable one.
    assert_eq!(run("3 'x set {x}").stack()[0].to_string(), "{x}");
    assert_eq!(run("3 'x set {x}").stack()[0].type_name(), "function");
    // Applying it is applying the word, so a data binding lands its value.
    assert_eq!(run("3 'x set {x} call").stack(), &[Value::Int(3)]);
    assert_eq!(run("3 'x set {x} 'y set y").stack(), &[Value::Int(3)]);
    assert_eq!(run("'sq {dup *} =  4 {sq} call").stack(), &[Value::Int(16)]);
    assert_eq!(run("3 4 {+} call").stack(), &[Value::Int(7)]);
    // Including where a *branch* is wanted: `if` applies what it chose.
    assert_eq!(
        run("'a 1 =  'b 2 =  true {a} {b} if").stack(),
        &[Value::Int(1)]
    );
    // Deferring is not pinning: the name resolves when the function runs, so a
    // rebinding is visible through a suspension taken before it. Freezing the
    // binding instead would freeze exactly one level — the body's own mentions
    // stay live either way — which is what made a saved recursive word run its
    // old body against the new callee.
    assert_eq!(
        run("'x 1 =  {x} 'y set  'x 2 =  y").stack(),
        &[Value::Int(2)]
    );
    // Nothing is checked at suspension time either, which is what lets a
    // definition name something defined later.
    assert_eq!(run("'g {h} =  'h {7} =  g").stack(), &[Value::Int(7)]);
    assert_eq!(
        run_err("{nope} call"),
        ErrorKind::UnboundName("nope".to_string())
    );
}

#[test]
fn the_ampersand_is_an_ordinary_word_now() {
    // The payoff for dropping the sigil: `&` is a name character in every
    // position, so the vocabulary can have it back — `and`/`or`/`xor` no longer
    // hold the operator spellings open against a sigil that is gone.
    assert_eq!(run("'& {and} =  6 3 &").stack(), &[Value::Int(2)]);
    assert_eq!(run("'&x 7 =  &x").stack(), &[Value::Int(7)]);
    assert_eq!(run_err("&x"), ErrorKind::UnboundName("&x".to_string()));
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
    // `.&x` is not a second form — it is the attribute named `&x`.
    assert_eq!(
        run_err("3 .&x"),
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
    assert_eq!(run("3 4 {+} call").stack(), &[Value::Int(7)]);
}

#[test]
fn a_word_bound_to_a_function_runs_when_applied() {
    assert_eq!(run("'sq {dup *} =  4 sq").stack(), &[Value::Int(16)]);
    // `{sq}` suspends it instead, so `call` is what applies it.
    assert_eq!(run("'sq {dup *} =  4 {sq} call").stack(), &[Value::Int(16)]);
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
    let before = engine.env.len();
    engine.apply(&parse("1 inc inc inc").unwrap()).unwrap();
    assert_eq!(engine.stack(), &[Value::Int(4)]);
    assert_eq!(
        engine.env.len(),
        before,
        "a bindless call allocated a frame"
    );
}

#[test]
fn an_inner_template_is_a_capture_and_so_allocates_one() {
    // Instantiating a nested `{ }` captures, and a capture makes the call own
    // its frame — where the same body without one borrows what it inherited
    // (the test above). Since `{f}` is how a word is suspended now, that is the
    // standing cost of the idiom: a suspension in a hot body is a frame a call.
    let mut engine = Engine::new();
    engine.apply(&parse("'take {{+} drop} =").unwrap()).unwrap();
    let before = engine.env.len();
    engine.apply(&parse("take").unwrap()).unwrap();
    assert_eq!(engine.env.len(), before + 1);
}

#[test]
fn a_call_that_binds_allocates_one_frame() {
    let mut engine = Engine::new();
    engine.apply(&parse("'sq {n: n n *} =").unwrap()).unwrap();
    let before = engine.env.len();
    engine.apply(&parse("4 sq").unwrap()).unwrap();
    assert_eq!(engine.stack(), &[Value::Int(16)]);
    assert_eq!(engine.env.len(), before + 1);
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
fn marking_reaches_a_closure_through_a_deferring_word() {
    // What `&` keeps alive, now that it defers: not the value but the *frame*
    // `xs` resolves in — so `grab`'s frame survives its own return, and the
    // closure two aggregates down it survives with it. The wider retention is
    // the price of late binding; a snapshot would have held only the list.
    let mut engine = Engine::new();
    let source = "'make {n: {n}} =  'grab {'xs [ 7 make ] =  {xs}} =  grab";
    engine.apply(&parse(source).unwrap()).unwrap();
    engine.collect();
    engine.apply(&parse("call first call").unwrap()).unwrap();
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
fn each_is_defined_in_the_language_not_in_rust() {
    // The half of the prelude that is source rather than a fn pointer. Nothing
    // downstream can tell: `apply_value` is the one seam everything callable is
    // reached through, so a word can migrate between the halves unnoticed.
    assert!(matches!(
        Engine::new().lookup("each"),
        Some(Value::Function { .. })
    ));
    assert!(!ops::primitives().any(|p| p.name == "each"));
}

#[test]
fn each_applies_a_function_to_every_element() {
    assert_eq!(
        run("[ 1 2 3 ] {2 *} each").stack(),
        &[Value::Int(2), Value::Int(4), Value::Int(6)]
    );
    // Anything callable will do — a builtin as readily as a function.
    assert_eq!(
        run("[ 1 2 3 ] {neg} each").stack(),
        &[Value::Int(-1), Value::Int(-2), Value::Int(-3)]
    );
    // An empty list is not a special case.
    assert_eq!(run("[ ] {2 *} each").stack(), &[] as &[Value]);
}

#[test]
fn map_flatmap_and_reduce_are_calling_conventions_on_each() {
    // Not four words. A function may leave any number of values and `[ ]` is an
    // ordinary runtime mark, so the rest fall out of how `each` is called.
    assert_eq!(
        run("[ [ 1 2 3 ] {2 *} each ]").stack(),
        &[list(&[Value::Int(2), Value::Int(4), Value::Int(6)])]
    );
    // flatMap is the *same code* — there is no intermediate container to
    // flatten, so `f` leaving two values is indistinguishable from two
    // iterations leaving one.
    assert_eq!(
        run("[ [ 1 2 ] {dup} each ]").stack(),
        &[list(&[
            Value::Int(1),
            Value::Int(1),
            Value::Int(2),
            Value::Int(2)
        ])]
    );
    // reduce needs no accumulator parameter: the seed sits below the working
    // area and the stack does the accumulating.
    assert_eq!(run("0 [ 1 2 3 4 ] {+} each").stack(), &[Value::Int(10)]);
    assert_eq!(run("1 [ 1 2 3 4 5 ] {*} each").stack(), &[Value::Int(120)]);
    // filter, unfolded — the reason a `keep_if` adapter is wanted eventually.
    assert_eq!(
        run("[ [ 1 2 3 ] {dup 1 > { } {drop} if} each ]").stack(),
        &[list(&[Value::Int(2), Value::Int(3)])]
    );
}

#[test]
fn each_reaches_back_over_values_that_predate_the_region() {
    // The mark is an ordinary stack value, so a produced list can hold literals
    // beside the iteration's output. A `map` that opened its own region would
    // give this up — which is the argument against having one.
    assert_eq!(
        run("[ 0 [ 1 2 ] {10 *} each 99 ]").stack(),
        &[list(&[
            Value::Int(0),
            Value::Int(10),
            Value::Int(20),
            Value::Int(99)
        ])]
    );
    // No region at all is the other legitimate use: results just land.
    assert_eq!(run("[ 1 2 ] {10 *} each").stack().len(), 2);
}

#[test]
fn each_nests() {
    assert_eq!(
        run("[ [ 1 2 ] { 'a set [ [ 10 20 ] {a *} each ] } each ]").stack(),
        &[list(&[
            list(&[Value::Int(10), Value::Int(20)]),
            list(&[Value::Int(20), Value::Int(40)]),
        ])]
    );
}

#[test]
fn each_runs_flat_over_a_long_list() {
    // `step` recurses in tail position, so the loop replaces its activation
    // rather than nesting: iteration depth is bounded by memory, not by the
    // Rust stack. Without the tail call this overflows and aborts the process.
    let items = (1..=20_000).map(|i| i.to_string()).collect::<Vec<_>>();
    let source = format!("0 [ {} ] {{+}} each", items.join(" "));
    let engine = run(&source);
    assert_eq!(engine.stack(), &[Value::Int(20_000 * 20_001 / 2)]);
}

#[test]
fn a_prelude_word_is_shadowable_like_any_builtin() {
    // It binds in the global frame, not the session, so a user binding shadows
    // it exactly as it would shadow `+`.
    assert_eq!(run("42 'each set each").stack(), &[Value::Int(42)]);
}

#[test]
fn a_tail_call_replaces_an_exhausted_activation() {
    // The mechanism, directly: a call made when the caller has nothing left to
    // run takes its place rather than stacking on it. Iteration here is
    // recursion over combinators, so without this an unbounded loop grows the
    // activation stack without bound.
    let body: Template = Rc::new(vec![Element::Word(Rc::from("dup"))]);
    let mut engine = Engine::new();
    let env = engine.session;
    engine.push_call(Rc::clone(&body), env);
    engine.push_call(Rc::clone(&body), env); // caller mid-template: stacks
    let depth = engine.calls.len();
    assert_eq!(depth, 2);

    engine.calls.last_mut().unwrap().ip = body.len(); // now in tail position
    engine.push_call(body, env);
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
    // `foo` holds a list; applying it shares it (Rc bump). Mutating the retrieved
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
    // `get` is that same application with the name arriving as a value, so the
    // two agree by construction — both reach `apply_value`.
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
    // `+` is a word resolved to a prelude binding — no special parse case; the
    // same lookup any user binding gets, reached here through `get`.
    assert_eq!(run("3 4 '+ get").stack(), &[Value::Int(7)]);
    // A builtin is now only ever *in* the environment, never on the stack:
    // `&+` defers to the word rather than extracting it, so `Value::Builtin` is
    // the prelude's representation and `apply_value` its only consumer.
    assert_eq!(
        Engine::new().lookup("+"),
        Some(Value::Builtin(
            ops::primitives().find(|p| p.name == "+").unwrap()
        ))
    );
    assert_eq!(run("{+}").stack()[0].type_name(), "function");
    assert_eq!(run_err("nope"), ErrorKind::UnboundName("nope".to_string()));
}

#[test]
fn a_deferred_builtin_runs_when_applied() {
    // `&+` is a word that applies `+`; binding it to a name and applying that
    // name reaches the op — first-class words end to end, with no arm anywhere
    // for "it was a primitive".
    assert_eq!(run("{+} 'plus set 3 4 plus").stack(), &[7.0]);
    // `'+ get` is the other half: applying the op *by name*, without writing it.
    assert_eq!(run("3 4 '+ get").stack(), &[7.0]);
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
