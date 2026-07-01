//! Tests for the optional macro / function extensions.

use std::collections::BTreeMap;

use maplibre_expr::{
    evaluate, evaluate_with, parse_with, EvaluationContext, Feature, Options, Value,
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
fn macro_expands_at_parse_time() {
    let mut opts = Options::new();
    // double(x) = x * 2
    opts.macro_def("double", vec!["x".into()], json!(["*", ["var", "x"], 2]));

    let expr = parse_with(&json!(["double", ["get", "n"]]), &opts).unwrap();
    // The expansion is a plain expression: no runtime options needed.
    let ctx = feature_with("n", Value::Number(21.0));
    assert_eq!(evaluate(&expr, &ctx).unwrap(), Value::Number(42.0));
}

#[test]
fn macros_can_nest() {
    let mut opts = Options::new();
    opts.macro_def("double", vec!["x".into()], json!(["*", ["var", "x"], 2]));
    opts.macro_def(
        "quad",
        vec!["x".into()],
        json!(["double", ["double", ["var", "x"]]]),
    );
    let expr = parse_with(&json!(["quad", 3]), &opts).unwrap();
    assert_eq!(
        evaluate(&expr, &EvaluationContext::new()).unwrap(),
        Value::Number(12.0)
    );
}

#[test]
fn recursive_macro_is_rejected() {
    let mut opts = Options::new();
    // loop() = loop()  — must not hang; expansion depth is bounded.
    opts.macro_def("loop", vec![], json!(["loop"]));
    assert!(parse_with(&json!(["loop"]), &opts).is_err());
}

#[test]
fn macro_arity_is_checked() {
    let mut opts = Options::new();
    opts.macro_def("double", vec!["x".into()], json!(["*", ["var", "x"], 2]));
    assert!(parse_with(&json!(["double", 1, 2]), &opts).is_err());
}

#[test]
fn recursive_function_runs() {
    let mut opts = Options::new();
    // countdown(n) = n <= 0 ? 0 : countdown(n - 1)
    opts.function(
        "countdown",
        vec!["n".into()],
        json!([
            "case",
            ["<=", ["var", "n"], 0],
            0,
            ["countdown", ["-", ["var", "n"], 1]]
        ]),
    );
    let expr = parse_with(&json!(["countdown", 5]), &opts).unwrap();
    assert_eq!(
        evaluate_with(&expr, &EvaluationContext::new(), &opts).unwrap(),
        Value::Number(0.0)
    );
}

#[test]
fn function_can_recurse_and_compute() {
    let mut opts = Options::new();
    // sum(n) = n <= 0 ? 0 : n + sum(n - 1)   (triangular number)
    opts.function(
        "sum",
        vec!["n".into()],
        json!([
            "case",
            ["<=", ["var", "n"], 0],
            0,
            ["+", ["var", "n"], ["sum", ["-", ["var", "n"], 1]]]
        ]),
    );
    let expr = parse_with(&json!(["sum", 5]), &opts).unwrap();
    assert_eq!(
        evaluate_with(&expr, &EvaluationContext::new(), &opts).unwrap(),
        Value::Number(15.0)
    );
}

#[test]
fn unbounded_recursion_errors_rather_than_hangs() {
    let mut opts = Options::new();
    // forever(n) = forever(n + 1)  — no base case; must error via depth limit.
    opts.function(
        "forever",
        vec!["n".into()],
        json!(["forever", ["+", ["var", "n"], 1]]),
    );
    let expr = parse_with(&json!(["forever", 0]), &opts).unwrap();
    assert!(evaluate_with(&expr, &EvaluationContext::new(), &opts).is_err());
}

#[test]
fn function_arity_is_checked_at_parse() {
    let mut opts = Options::new();
    opts.function("id", vec!["x".into()], json!(["var", "x"]));
    assert!(parse_with(&json!(["id", 1, 2]), &opts).is_err());
}
