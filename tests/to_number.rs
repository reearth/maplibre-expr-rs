//! `to-number`, pinned against `Coercion.evaluate`'s `'number'` case in
//! `maplibre-style-spec` at `ef522e45a28e0efafabbebb27197d3440c99fe34`
//! (`src/expression/definitions/coercion.ts:153-163`):
//!
//! ```js
//! let value = null;
//! for (const arg of this.args) {
//!     value = arg.evaluate(ctx);
//!     if (value === null) return 0;
//!     const num = Number(value);
//!     if (isNaN(num)) continue;
//!     return num;
//! }
//! throw new RuntimeError(`Could not convert ${JSON.stringify(value)} to number.`);
//! ```
//!
//! The vendored fixtures never produce a NaN here, so the `isNaN(num) →
//! continue` branch is invisible to them; these cases cover it by hand.

use maplibre_expr::{evaluate, parse, EvaluationContext, Value};
use serde_json::{json, Value as Json};

fn eval(expr: Json) -> Result<Value, String> {
    let parsed = parse(&expr).unwrap_or_else(|e| panic!("{expr} failed to parse: {e}"));
    evaluate(&parsed, &EvaluationContext::new()).map_err(|e| e.to_string())
}

#[track_caller]
fn number(expr: Json) -> f64 {
    match eval(expr.clone()) {
        Ok(Value::Number(n)) => n,
        other => panic!("{expr} evaluated to {other:?}"),
    }
}

#[track_caller]
fn error(expr: Json) -> String {
    match eval(expr.clone()) {
        Err(e) => e,
        Ok(v) => panic!("{expr} unexpectedly evaluated to {v:?}"),
    }
}

/// `Infinity - Infinity` is a NaN that arrives already typed as a number.
/// Upstream `continue`s past it and, with no argument left, throws —
/// `JSON.stringify(NaN)` is `"null"`, so the message names `null`.
#[test]
fn nan_number_argument_is_not_returned() {
    let nan = json!(["-", ["/", 1, 0], ["/", 1, 0]]);
    assert_eq!(
        error(json!(["to-number", nan])),
        "Could not convert null to number."
    );
}

/// Rust's `"NaN".parse::<f64>()` succeeds; JavaScript's `Number("NaN")` is NaN,
/// so this argument is skipped like any other unparsable string. Same for the
/// other Rust-only float spellings.
#[test]
fn rust_only_float_spellings_are_not_javascript_numbers() {
    for s in ["NaN", "nan", "inf", "-inf", "infinity", "INFINITY"] {
        assert_eq!(
            error(json!(["to-number", s])),
            format!("Could not convert \"{s}\" to number."),
            "{s} should not convert"
        );
    }
}

/// A skipped argument falls through to the next one, whether it was a NaN
/// number or an unparsable string.
#[test]
fn nan_arguments_fall_through_to_the_next() {
    let nan = json!(["-", ["/", 1, 0], ["/", 1, 0]]);
    assert_eq!(number(json!(["to-number", nan, "12"])), 12.0);
    assert_eq!(number(json!(["to-number", "NaN", "oops", 7])), 7.0);
}

/// `null` is the one argument that does *not* fall through: it short-circuits
/// to `0` and the remaining arguments are never evaluated.
#[test]
fn null_short_circuits_to_zero() {
    assert_eq!(number(json!(["to-number", ["get", "absent"], 5])), 0.0);
}

/// The exhausted-arguments message stringifies the *last* value tried, not the
/// first.
#[test]
fn error_names_the_last_argument() {
    assert_eq!(
        error(json!(["to-number", "a", ["literal", {"x": 1}]])),
        "Could not convert {\"x\":1} to number."
    );
}

/// The rest of `Number(value)`: booleans, blank strings, the literal
/// `"Infinity"`, and non-decimal integer literals.
#[test]
fn javascript_number_conversion() {
    assert_eq!(number(json!(["to-number", true])), 1.0);
    assert_eq!(number(json!(["to-number", false])), 0.0);
    assert_eq!(number(json!(["to-number", "   "])), 0.0);
    assert_eq!(number(json!(["to-number", ""])), 0.0);
    assert_eq!(number(json!(["to-number", "  12.5  "])), 12.5);
    assert_eq!(number(json!(["to-number", "1e3"])), 1000.0);
    assert!(number(json!(["to-number", "Infinity"])).is_infinite());
    assert_eq!(number(json!(["to-number", "-Infinity"])), f64::NEG_INFINITY);
    assert_eq!(number(json!(["to-number", "0x1f"])), 31.0);
    assert_eq!(number(json!(["to-number", "0b101"])), 5.0);
    assert_eq!(number(json!(["to-number", "0o17"])), 15.0);
}

/// Arrays coerce through `Array.prototype.toString` before `ToNumber`.
#[test]
fn arrays_coerce_through_their_string_form() {
    assert_eq!(number(json!(["to-number", ["literal", []]])), 0.0);
    assert_eq!(number(json!(["to-number", ["literal", [7]]])), 7.0);
    assert_eq!(number(json!(["to-number", ["literal", ["8"]]])), 8.0);
    assert_eq!(
        error(json!(["to-number", ["literal", [1, 2]]])),
        "Could not convert [1,2] to number."
    );
}
