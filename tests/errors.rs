//! Tests for the semantic error kinds and location keys.
//!
//! The `upstream_*` tests below are byte-for-byte message/key assertions taken
//! from `maplibre-style-spec` at the commit `src/reference/ATTRIBUTION.md`
//! pins (`ef522e45a28e0efafabbebb27197d3440c99fe34`). They exist because the
//! vendored expression fixtures do *not* reach these messages: a port that
//! grows to the shape of its test suite drifts precisely where the suite is
//! silent, so each case here is one of those blind spots, pinned by hand
//! against the upstream source rather than against a fixture.

use maplibre_expr::{is_expression, parse, typecheck, ParseErrorKind, Type};
use serde_json::json;

fn compile_err(expr: serde_json::Value, expected: Option<Type>) -> maplibre_expr::ParseError {
    let parsed = parse(&expr);
    match parsed {
        Err(e) => e,
        Ok(expr) => typecheck(&expr, expected.as_ref(), false).unwrap_err(),
    }
}

/// Assert the rendered message *and* the location key of a compile error. The
/// key matters as much as the text: it is what an editor highlights.
#[track_caller]
fn assert_err(expr: serde_json::Value, message: &str, key: &str) {
    let e = compile_err(expr.clone(), None);
    assert_eq!(e.to_string(), message, "message for {expr}");
    assert_eq!(e.key, key, "key for {expr}");
}

#[track_caller]
fn assert_ok(expr: serde_json::Value) {
    let parsed = parse(&expr).unwrap_or_else(|e| panic!("{expr} failed to parse: {e}"));
    typecheck(&parsed, None, false).unwrap_or_else(|e| panic!("{expr} failed to type-check: {e}"));
}

#[test]
fn unknown_expression_kind() {
    let e = compile_err(json!(["bogus", 1]), None);
    assert!(matches!(e.kind, ParseErrorKind::UnknownExpression(op) if op == "bogus"));
}

#[test]
fn wrong_arg_count_kind() {
    let e = compile_err(json!(["length", 1, 2]), None);
    assert!(matches!(e.kind, ParseErrorKind::WrongArgCount { op, .. } if op == "length"));
}

#[test]
fn nested_error_carries_location_key() {
    // The offending sub-expression is the 3rd element (index 2) of `get`, and
    // the unknown operator name sits at position 0 within it — as MapLibre keys.
    let e = compile_err(json!(["get", "x", ["bogus"]]), None);
    assert!(matches!(e.kind, ParseErrorKind::UnknownExpression(_)));
    assert_eq!(e.key, "[2][0]");
}

#[test]
fn comparison_kinds() {
    // "==" of two colors is not comparable.
    let e = compile_err(
        json!(["==", ["to-color", "red"], ["to-color", "blue"]]),
        None,
    );
    assert!(matches!(e.kind, ParseErrorKind::NotComparable { .. }));

    // string vs number: cannot compare.
    let e = compile_err(
        json!(["==", ["string", ["get", "x"]], ["number", ["get", "y"]]]),
        None,
    );
    assert!(matches!(e.kind, ParseErrorKind::CannotCompare { .. }));
}

#[test]
fn non_interpolatable_kind() {
    let e = compile_err(
        json!(["interpolate", ["linear"], ["zoom"], 0, false, 1, true]),
        None,
    );
    assert!(matches!(e.kind, ParseErrorKind::NotInterpolatable(_)));
}

#[test]
fn type_mismatch_kind() {
    // Property expects a number, but the expression yields a string.
    let e = compile_err(json!(["string", ["get", "x"]]), Some(Type::Number));
    assert!(matches!(e.kind, ParseErrorKind::TypeMismatch { .. }));
}

#[test]
fn structural_kinds_are_semantic() {
    // These used to be Other(String); they now have dedicated variants.
    assert!(matches!(
        compile_err(json!(["array", 0, ["literal", []]]), None).kind,
        ParseErrorKind::ArrayItemType
    ));
    assert!(matches!(
        compile_err(json!(["literal", 1, 2]), None).kind,
        ParseErrorKind::RequiresExactlyOneArg { .. }
    ));
    assert!(matches!(
        compile_err(json!({}), None).kind,
        ParseErrorKind::BareObject
    ));
    assert!(matches!(
        compile_err(json!(["match", ["get", "x"], true, "a", "d"]), None).kind,
        ParseErrorKind::BranchLabelsType
    ));
    // Formerly Other(String), now dedicated variants.
    assert!(matches!(
        compile_err(json!(["match", ["get", "x"], 1, "a", 2, "b"]), None).kind,
        ParseErrorKind::ExpectedEvenArgs
    ));
    assert!(matches!(
        compile_err(json!(["interpolate", "linear", ["zoom"], 0, 0]), None).kind,
        ParseErrorKind::InterpolationTypeArray
    ));
}

#[test]
fn eval_error_kinds_are_semantic() {
    use maplibre_expr::{evaluate, EvalErrorKind, EvaluationContext, Feature, Value};
    use std::collections::BTreeMap;
    // Feature-dependent so it isn't constant-folded away at compile time.
    let mut props = BTreeMap::new();
    props.insert("c".to_string(), Value::String("not-a-color".into()));
    props.insert("i".to_string(), Value::Number(-1.0));
    let ctx = EvaluationContext::new().with_feature(Feature {
        properties: props,
        ..Default::default()
    });
    let eval_err = |expr: serde_json::Value| {
        let e = typecheck(&parse(&expr).unwrap(), None, false).unwrap();
        evaluate(&e, &ctx).unwrap_err()
    };
    assert!(matches!(
        eval_err(json!(["to-color", ["get", "c"]])).kind,
        EvalErrorKind::CouldNotParse { ty: "color", .. }
    ));
    assert!(matches!(
        eval_err(json!(["at", ["get", "i"], ["literal", [1, 2]]])).kind,
        EvalErrorKind::ArrayIndexNegative { .. }
    ));
}

// ---------------------------------------------------------------------------
// `CompoundExpression` signature messages (`compound_expression.ts:92-96,
// 154-172`). A wrong argument count to any registered compound expression is
// reported against its typed overloads, never as a plain count — and that is
// true for all 68 of them, not just the five that happened to have fixtures.
// ---------------------------------------------------------------------------

#[test]
fn upstream_compound_arity_is_reported_as_a_signature() {
    // Zero-argument globals and feature lookups.
    for op in [
        "zoom",
        "id",
        "properties",
        "geometry-type",
        "accumulated",
        "heatmap-density",
        "line-progress",
        "elevation",
        "e",
        "pi",
        "ln2",
        "filter-has-id",
    ] {
        assert_err(
            json!([op, 1]),
            "Expected arguments of type (), but found (number) instead.",
            "",
        );
    }

    assert_err(
        json!(["get", "a", "b", "c"]),
        "Expected arguments of type (string) | (string, object), but found (string, string, string) instead.",
        "",
    );
    assert_err(
        json!(["has"]),
        "Expected arguments of type (string) | (string, object), but found () instead.",
        "",
    );
    assert_err(
        json!(["feature-state"]),
        "Expected arguments of type (string), but found () instead.",
        "",
    );
    assert_err(
        json!(["rgb", 1, 2]),
        "Expected arguments of type (number, number, number), but found (number, number) instead.",
        "",
    );
    assert_err(
        json!(["rgba", 1, 2, 3]),
        "Expected arguments of type (number, number, number, number), but found (number, number, number) instead.",
        "",
    );
    assert_err(
        json!(["sqrt", 1, 2]),
        "Expected arguments of type (number), but found (number, number) instead.",
        "",
    );
    assert_err(
        json!(["abs"]),
        "Expected arguments of type (number), but found () instead.",
        "",
    );
    assert_err(
        json!(["upcase"]),
        "Expected arguments of type (string), but found () instead.",
        "",
    );
    assert_err(
        json!(["error"]),
        "Expected arguments of type (string), but found () instead.",
        "",
    );
    assert_err(
        json!(["to-rgba"]),
        "Expected arguments of type (color), but found () instead.",
        "",
    );
    assert_err(
        json!(["join", "a"]),
        "Expected arguments of type (array<string>, string), but found (string) instead.",
        "",
    );
    assert_err(
        json!(["split", "a"]),
        "Expected arguments of type (string, string), but found (string) instead.",
        "",
    );
    assert_err(
        json!(["resolved-locale"]),
        "Expected arguments of type (collator), but found () instead.",
        "",
    );
    assert_err(
        json!(["!"]),
        "Expected arguments of type (boolean), but found () instead.",
        "",
    );
    for op in ["/", "%", "^"] {
        assert_err(
            json!([op, 1]),
            "Expected arguments of type (number, number), but found (number) instead.",
            "",
        );
    }
    // Two overloads, printed in registration order (`-` is the one the
    // fixtures already covered; it pins the separator and the ordering).
    assert_err(
        json!(["-"]),
        "Expected arguments of type (number, number) | (number), but found () instead.",
        "",
    );
}

#[test]
fn upstream_varargs_overloads_accept_any_count() {
    // A varargs signature is never filtered out by the argument-count pass
    // (`compound_expression.ts:92-96`), so these are not arity errors.
    for expr in [
        json!(["+"]),
        json!(["*"]),
        json!(["min"]),
        json!(["max"]),
        json!(["concat"]),
        json!(["all"]),
        json!(["any"]),
    ] {
        assert_ok(expr);
    }
}

#[test]
fn compound_signature_arg_types_are_a_documented_approximation() {
    // Upstream re-parses each argument and prints `typeToString(parsed.type)`;
    // this crate has no types yet at parse time (parse and typecheck are two
    // passes), so it falls back to the raw JSON's JavaScript `typeof`. Pinned
    // so the approximation stays a deliberate, visible one: upstream would say
    // `(value, number, number)` and `(array<number, 1>, number)` here.
    assert_err(
        json!(["-", ["get", "x"], 1, 2]),
        "Expected arguments of type (number, number) | (number), but found (object, number, number) instead.",
        "",
    );
    assert_err(
        json!(["typeof", ["literal", [1]], 2]),
        "Expected arguments of type (value), but found (object, number) instead.",
        "",
    );
}

// ---------------------------------------------------------------------------
// The operator registry (`definitions/index.ts` ∪ `CompoundExpression.register`)
// ---------------------------------------------------------------------------

#[test]
fn upstream_names_outside_the_registry_are_unknown() {
    // These four are not in MapLibre's expression registry at the pinned
    // commit (`config` appears in `reference/v8.json`, but not as an
    // expression), so they are unknown names, not unimplemented operators.
    for op in [
        "config",
        "measure-light",
        "raster-value",
        "sky-radial-progress",
    ] {
        assert!(
            !is_expression(&json!([op, 1])),
            "{op} must not be an operator"
        );
        assert_err(
            json!([op, 1]),
            &format!(
                "Unknown expression \"{op}\". If you wanted a literal array, use [\"literal\", [...]]."
            ),
            "[0]",
        );
    }
}

#[test]
fn upstream_internal_filter_operators_are_registered() {
    // MapLibre registers the `filter-*` family it compiles legacy filters
    // into; they parse here (and report as unimplemented at evaluation).
    for expr in [
        json!(["filter-has-id"]),
        json!(["filter-==", "k", 1]),
        json!(["filter-type-in", ["literal", ["Point"]]]),
    ] {
        assert!(is_expression(&expr), "{expr} must be an expression");
        assert!(parse(&expr).is_ok(), "{expr} must parse");
    }
}

// ---------------------------------------------------------------------------
// `let` (`let.ts:28-49`) and `var` (`var.ts:18-21`)
// ---------------------------------------------------------------------------

#[test]
fn upstream_let_requires_at_least_one_binding() {
    // A bindings-less `let` used to be accepted here.
    for expr in [
        json!(["let", "a"]),
        json!(["let", ["get", "x"]]),
        json!(["let", 5]),
    ] {
        assert_err(
            expr,
            "Expected at least 3 arguments, but found 1 instead.",
            "",
        );
    }
    assert_err(
        json!(["let", "a", 1]),
        "Expected at least 3 arguments, but found 2 instead.",
        "",
    );
}

#[test]
fn upstream_let_binding_name_must_be_a_string_at_its_own_key() {
    assert_err(
        json!(["let", 1, 2, ["var", "a"]]),
        "Expected string, but found number instead.",
        "[1]",
    );
    assert_err(
        json!(["let", "a", 1, ["get", "x"], 2, ["var", "a"]]),
        "Expected string, but found object instead.",
        "[3]",
    );
}

#[test]
fn upstream_let_does_not_check_parity() {
    // With an even argument count the last value doubles as the body; upstream
    // has no parity check, only the minimum above.
    assert_ok(json!(["let", "a", 1, "b", 2]));
}

#[test]
fn let_variable_name_and_scope_rules_are_unchanged() {
    // Guarded: the character check lives in the type-checker and must keep
    // reporting at the binding-name key.
    let e = compile_err(json!(["let", "$a", 1, ["var", "$a"]]), None);
    assert_eq!(
        e.to_string(),
        "Variable names must contain only alphanumeric characters or '_'."
    );
    assert_eq!(e.key, "[1]");
}

#[test]
fn upstream_var_has_a_single_guard() {
    for expr in [json!(["var"]), json!(["var", 1]), json!(["var", "a", "b"])] {
        assert_err(
            expr,
            "'var' expression requires exactly one string literal argument.",
            "",
        );
    }
}

#[test]
fn unbound_variable_message_is_unchanged() {
    assert_err(
        json!(["var", "nope"]),
        "Unknown variable \"nope\". Make sure \"nope\" has been bound in an enclosing \"let\" expression before using it.",
        "[1]",
    );
}

// ---------------------------------------------------------------------------
// Assertions and coercions (`assertion.ts:36`, `coercion.ts:41,46-47`)
// ---------------------------------------------------------------------------

#[test]
fn upstream_assertions_and_coercions_expect_at_least_one_argument() {
    for op in [
        "array",
        "number",
        "boolean",
        "string",
        "object",
        "to-number",
        "to-color",
    ] {
        assert_err(json!([op]), "Expected at least one argument.", "");
    }
}

#[test]
fn single_argument_coercion_carve_out_is_unchanged() {
    assert_err(json!(["to-boolean"]), "Expected one argument.", "");
    assert_err(json!(["to-string", "a", "b"]), "Expected one argument.", "");
}

// ---------------------------------------------------------------------------
// `image` (`image.ts:19-20`)
// ---------------------------------------------------------------------------

#[test]
fn upstream_image_takes_exactly_one_argument() {
    // `image` used to be folded into `format`'s variadic shape, which let a
    // two-argument `image` through parsing and type-checking entirely. The
    // message counts the operator, as MapLibre's does.
    assert_err(json!(["image", "a", "b"]), "Expected two arguments.", "");
    assert_err(json!(["image"]), "Expected two arguments.", "");
    assert_ok(json!(["image", "a"]));
}

// ---------------------------------------------------------------------------
// `step` (`step.ts:31-39,56-62`), `interpolate` (`interpolate.ts:106-190`),
// `match` (`match.ts:40-46`), `case` (`case.ts:23-28`)
// ---------------------------------------------------------------------------

#[test]
fn upstream_step_arity_is_two_distinct_messages() {
    assert_err(
        json!(["step", ["zoom"], 0]),
        "Expected at least 4 arguments, but found only 2.",
        "",
    );
    assert_err(
        json!(["step", ["zoom"], 0, 1, 2, 3]),
        "Expected an even number of arguments.",
        "",
    );
}

#[test]
fn upstream_stop_inputs_must_be_literal_numbers_keyed_at_the_stop() {
    assert_err(
        json!(["step", ["zoom"], 0, ["get", "x"], 2]),
        "Input/output pairs for \"step\" expressions must be defined using literal numeric values (not computed expressions) for the input values.",
        "[3]",
    );
    assert_err(
        json!(["step", ["zoom"], 0, 1, 2, ["get", "x"], 4]),
        "Input/output pairs for \"step\" expressions must be defined using literal numeric values (not computed expressions) for the input values.",
        "[5]",
    );
    assert_err(
        json!(["interpolate", ["linear"], ["zoom"], ["get", "x"], 1]),
        "Input/output pairs for \"interpolate\" expressions must be defined using literal numeric values (not computed expressions) for the input values.",
        "[3]",
    );
}

#[test]
fn ascending_stop_keys_are_unchanged() {
    // Guarded: `3 + 2 * (j + 1)` is correct for both layouts.
    assert_err(
        json!(["step", ["zoom"], 0, 10, 1, 5, 2]),
        "Input/output pairs for \"step\" expressions must be arranged with input values in strictly ascending order.",
        "[5]",
    );
    assert_err(
        json!(["interpolate", ["linear"], ["zoom"], 10, 1, 5, 2]),
        "Input/output pairs for \"interpolate\" expressions must be arranged with input values in strictly ascending order.",
        "[5]",
    );
}

#[test]
fn upstream_interpolation_type_is_validated_before_the_argument_count() {
    // `interpolate.ts:108-158` checks the type first; this crate used to check
    // the count first, swapping the two messages.
    assert_err(
        json!(["interpolate", "linear", 1]),
        "Expected an interpolation type expression.",
        "[1]",
    );
    assert_err(
        json!(["interpolate", [], ["zoom"], 0, 1]),
        "Expected an interpolation type expression.",
        "[1]",
    );
    assert_err(
        json!(["interpolate"]),
        "Expected an interpolation type expression.",
        "[1]",
    );
    assert_err(
        json!(["interpolate", ["linear"], ["zoom"], 0]),
        "Expected at least 4 arguments, but found only 3.",
        "",
    );
    assert_err(
        json!(["interpolate", ["linear"], ["zoom"], 0, 1, 2]),
        "Expected an even number of arguments.",
        "",
    );
}

#[test]
fn upstream_unknown_interpolation_type_is_unquoted_and_keyed_at_the_name() {
    // No quotes, no trailing period, and the key points at the name itself.
    assert_err(
        json!(["interpolate", ["wat"], ["zoom"], 0, 1]),
        "Unknown interpolation type wat",
        "[1][0]",
    );
    // A non-string head is not a separate error: it is stringified the way
    // JavaScript's `String()` would.
    assert_err(
        json!(["interpolate", [123], ["zoom"], 0, 1]),
        "Unknown interpolation type 123",
        "[1][0]",
    );
    assert_err(
        json!(["interpolate", [null], ["zoom"], 0, 1]),
        "Unknown interpolation type null",
        "[1][0]",
    );
}

#[test]
fn upstream_match_and_case_arity_messages() {
    assert_err(
        json!(["match", 1, 2]),
        "Expected at least 4 arguments, but found only 2.",
        "",
    );
    assert_err(
        json!(["match", ["get", "x"], 1, "a", 2, "b"]),
        "Expected an even number of arguments.",
        "",
    );
    assert_err(
        json!(["case", true]),
        "Expected at least 3 arguments, but found only 1.",
        "",
    );
    assert_err(
        json!(["case", true, 1, 2, 3]),
        "Expected an odd number of arguments.",
        "",
    );
}

// ---------------------------------------------------------------------------
// `format` (`format.ts:43-49`), `collator` (`collator.ts:27`),
// `number-format` (`number_format.ts:50,57`)
// ---------------------------------------------------------------------------

#[test]
fn upstream_format_collator_and_number_format_messages() {
    assert_err(json!(["format"]), "Expected at least one argument.", "");
    assert_err(
        json!(["format", {"font-scale": 1.2}]),
        "First argument must be an image or text section.",
        "",
    );
    assert_err(json!(["collator"]), "Expected one argument.", "");
    assert_err(json!(["collator", {}, {}]), "Expected one argument.", "");
    assert_err(json!(["number-format", 1]), "Expected two arguments.", "");
    assert_err(
        json!(["number-format", 1, 2, 3]),
        "Expected two arguments.",
        "",
    );
    assert_err(
        json!(["number-format", 1, "x"]),
        "NumberFormat options argument must be an object.",
        "",
    );
}

#[test]
fn format_section_option_keys_are_unchanged() {
    // Guarded: a section's options are keyed at the section's *content*
    // position, not at the options object's own position.
    assert_err(
        json!(["format", "a", {}, "b", {"text-color": ["bogus"]}]),
        "Unknown expression \"bogus\". If you wanted a literal array, use [\"literal\", [...]].",
        "[3][0]",
    );
}

// ---------------------------------------------------------------------------
// `array`'s length argument (`assertion.ts`)
// ---------------------------------------------------------------------------

#[test]
fn array_length_accepts_zero_and_null_despite_saying_positive() {
    // Upstream checks `N < 0 || N !== Math.floor(N)` yet words the failure as
    // "positive"; the port keeps the wording and the behaviour together.
    assert_ok(json!(["array", "number", 0, ["literal", []]]));
    assert_ok(json!(["array", "number", null, ["literal", []]]));
    assert_err(
        json!(["array", "number", -1, ["literal", []]]),
        "The length argument to \"array\" must be a positive integer literal",
        "[2]",
    );
    assert_err(
        json!(["array", "number", 1.5, ["literal", [1]]]),
        "The length argument to \"array\" must be a positive integer literal",
        "[2]",
    );
}

// ---------------------------------------------------------------------------
// Literal arrays must stay distinguishable from expressions
// ---------------------------------------------------------------------------

#[test]
fn font_stacks_are_still_not_expressions() {
    assert!(!is_expression(&json!(["Font A", "Font B"])));
    assert_err(
        json!(["Font A", "Font B"]),
        "Unknown expression \"Font A\". If you wanted a literal array, use [\"literal\", [...]].",
        "[0]",
    );
}

// ---------------------------------------------------------------------------
// `at` out-of-bounds reports the last valid index, which is -1 when empty
// ---------------------------------------------------------------------------

#[test]
fn at_out_of_bounds_reports_the_last_valid_index() {
    // Upstream `at.ts:44-47` interpolates `array.length - 1`, so an empty array
    // reports `> -1`. A `usize` here used to saturate that to `0`.
    assert_err(
        json!(["at", 0, ["literal", []]]),
        "Array index out of bounds: 0 > -1.",
        "",
    );
    assert_err(
        json!(["at", 5, ["literal", [1]]]),
        "Array index out of bounds: 5 > 0.",
        "",
    );
    assert_err(
        json!(["at", 9, ["literal", [1, 2, 3]]]),
        "Array index out of bounds: 9 > 2.",
        "",
    );
}
