//! Tests for transparent legacy function-object conversion.
//!
//! `parse` accepts legacy MapLibre function objects (`{type, property, stops,
//! ...}`) directly, converting them to the equivalent modern expression first.
//! The dedicated [`convert`](maplibre_expr::convert) module is also exercised
//! against the full upstream fixture set by the conformance harness.

use std::collections::BTreeMap;

use maplibre_expr::convert::{convert_function, is_function};
use maplibre_expr::{
    evaluate, parse, parse_with, EvaluationContext, Feature, Options, ParseErrorKind, Value,
};
use serde_json::json;

fn feature_with(key: &str, value: Value) -> EvaluationContext {
    let mut props = BTreeMap::new();
    props.insert(key.to_string(), value);
    EvaluationContext::new().with_feature(Feature {
        properties: props,
        ..Feature::default()
    })
}

#[test]
fn zoom_exponential_parses_transparently() {
    // A zoom function with a base becomes ["interpolate", ["exponential", b], ["zoom"], ...].
    let expr = parse(&json!({
        "type": "exponential",
        "base": 2,
        "stops": [[0, 0], [10, 100]],
    }))
    .unwrap();

    let at = |z: f64| evaluate(&expr, &EvaluationContext::new().with_zoom(z)).unwrap();
    assert_eq!(at(0.0), Value::Number(0.0));
    assert_eq!(at(10.0), Value::Number(100.0));
}

#[test]
fn interval_property_function_parses_transparently() {
    // An interval property function becomes a `step` over ["number", ["get", p]].
    let expr = parse(&json!({
        "type": "interval",
        "property": "x",
        "stops": [[0, "small"], [10, "big"]],
    }))
    .unwrap();

    let out = evaluate(&expr, &feature_with("x", Value::Number(5.0))).unwrap();
    assert_eq!(out, Value::String("small".into()));
    let out = evaluate(&expr, &feature_with("x", Value::Number(20.0))).unwrap();
    assert_eq!(out, Value::String("big".into()));
}

#[test]
fn categorical_property_function_parses_transparently() {
    let expr = parse(&json!({
        "type": "categorical",
        "property": "kind",
        "stops": [["a", 1], ["b", 2]],
        "default": 0,
    }))
    .unwrap();

    let out = evaluate(&expr, &feature_with("kind", Value::String("b".into()))).unwrap();
    assert_eq!(out, Value::Number(2.0));
    // Unmatched → default.
    let out = evaluate(&expr, &feature_with("kind", Value::String("z".into()))).unwrap();
    assert_eq!(out, Value::Number(0.0));
}

#[test]
fn modern_expressions_still_parse_unchanged() {
    // Conversion is transparent: modern expressions are untouched.
    let expr = parse(&json!(["+", ["get", "x"], 1])).unwrap();
    let out = evaluate(&expr, &feature_with("x", Value::Number(41.0))).unwrap();
    assert_eq!(out, Value::Number(42.0));
}

#[test]
fn disabling_conversion_rejects_function_objects() {
    let mut opts = Options::new();
    opts.convert_legacy(false);
    let err = parse_with(
        &json!({ "type": "exponential", "stops": [[0, 0], [10, 100]] }),
        &opts,
    )
    .unwrap_err();
    assert!(matches!(err.kind, ParseErrorKind::BareObject));
}

#[test]
fn non_function_objects_are_still_bare_object_errors() {
    // An object without `stops`/`property` is not a function, even with the
    // default (conversion enabled).
    assert!(!is_function(&json!({ "foo": "bar" })));
    let err = parse(&json!({ "foo": "bar" })).unwrap_err();
    assert!(matches!(err.kind, ParseErrorKind::BareObject));
}

#[test]
fn convert_function_with_spec_expands_tokens() {
    // With a property spec that enables tokens, `{name}` strings expand to
    // ["get", ...] — something the spec-less transparent path cannot know.
    let params = json!({ "stops": [[0, "{name}!"]] });
    let spec = json!({ "type": "string", "tokens": true });
    let converted = convert_function(&params, &spec);

    let expr = parse(&converted).unwrap();
    let out = evaluate(
        &expr,
        &feature_with("name", Value::String("hi".into())).with_zoom(0.0),
    )
    .unwrap();
    assert_eq!(out, Value::String("hi!".into()));
}
