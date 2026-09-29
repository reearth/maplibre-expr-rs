//! Tests for reading legacy function objects with their property spec.
//!
//! As in MapLibre, `parse` accepts only expressions; a legacy function object
//! (`{type, property, stops, ...}`) is read through `parse_property`, which
//! converts it with the property's spec before parsing. The dedicated
//! [`convert`](maplibre_expr::convert) module is also exercised against the
//! full upstream fixture set by the conformance harness.

use std::collections::BTreeMap;

use maplibre_expr::convert::{convert_function, is_function};
use maplibre_expr::{
    evaluate, parse, parse_property, EvaluationContext, Feature, ParseErrorKind, Value,
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
fn zoom_exponential_function_reads_as_interpolate() {
    // A zoom function with a base becomes ["interpolate", ["exponential", b], ["zoom"], ...].
    let expr = parse_property(
        "line-width",
        &json!({
            "type": "exponential",
            "base": 2,
            "stops": [[0, 0], [10, 100]],
        }),
    )
    .unwrap();

    let at = |z: f64| evaluate(&expr, &EvaluationContext::new().with_zoom(z)).unwrap();
    assert_eq!(at(0.0), Value::Number(0.0));
    assert_eq!(at(10.0), Value::Number(100.0));
    // base 2: the midpoint lies below the linear 50.
    assert_eq!(
        at(5.0),
        Value::Number(100.0 * (2f64.powi(5) - 1.0) / (2f64.powi(10) - 1.0))
    );
}

#[test]
fn interval_property_function_reads_as_step() {
    // An interval property function becomes a `step` over ["number", ["get", p]].
    let expr = parse_property(
        "text-transform",
        &json!({
            "type": "interval",
            "property": "x",
            "stops": [[0, "small"], [10, "big"]],
        }),
    )
    .unwrap();

    let out = evaluate(&expr, &feature_with("x", Value::Number(5.0))).unwrap();
    assert_eq!(out, Value::String("small".into()));
    let out = evaluate(&expr, &feature_with("x", Value::Number(20.0))).unwrap();
    assert_eq!(out, Value::String("big".into()));
}

#[test]
fn categorical_property_function_reads_as_match() {
    let expr = parse_property(
        "line-width",
        &json!({
            "type": "categorical",
            "property": "kind",
            "stops": [["a", 1], ["b", 2]],
            "default": 0,
        }),
    )
    .unwrap();

    let out = evaluate(&expr, &feature_with("kind", Value::String("b".into()))).unwrap();
    assert_eq!(out, Value::Number(2.0));
    // Unmatched → default.
    let out = evaluate(&expr, &feature_with("kind", Value::String("z".into()))).unwrap();
    assert_eq!(out, Value::Number(0.0));
}

#[test]
fn modern_expressions_pass_through_parse_property() {
    let expr = parse_property("line-width", &json!(["+", ["get", "x"], 1])).unwrap();
    let out = evaluate(&expr, &feature_with("x", Value::Number(41.0))).unwrap();
    assert_eq!(out, Value::Number(42.0));
}

#[test]
fn parse_rejects_function_objects_like_create_expression() {
    // MapLibre's `createExpression` never reads a function object; neither
    // does `parse`. The message is the reference implementation's.
    let function = json!({ "type": "exponential", "stops": [[0, 0], [10, 100]] });
    assert!(is_function(&function));
    let err = parse(&function).unwrap_err();
    assert!(matches!(err.kind, ParseErrorKind::BareObject));
    assert_eq!(
        err.to_string(),
        "Bare objects invalid. Use [\"literal\", {...}] instead."
    );

    assert!(!is_function(&json!({ "foo": "bar" })));
    let err = parse(&json!({ "foo": "bar" })).unwrap_err();
    assert!(matches!(err.kind, ParseErrorKind::BareObject));
}

#[test]
fn convert_function_with_spec_expands_tokens() {
    // With a property spec that enables tokens, `{name}` strings expand to
    // ["get", ...] — something the object alone cannot tell.
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
