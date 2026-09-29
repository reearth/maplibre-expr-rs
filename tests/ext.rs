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
fn recursive_expr_fn_runs() {
    let mut opts = Options::new();
    // countdown(n) = n <= 0 ? 0 : countdown(n - 1)
    opts.expr_fn(
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
fn expr_fn_can_recurse_and_compute() {
    let mut opts = Options::new();
    // sum(n) = n <= 0 ? 0 : n + sum(n - 1)   (triangular number)
    opts.expr_fn(
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
    opts.expr_fn(
        "forever",
        vec!["n".into()],
        json!(["forever", ["+", ["var", "n"], 1]]),
    );
    let expr = parse_with(&json!(["forever", 0]), &opts).unwrap();
    assert!(evaluate_with(&expr, &EvaluationContext::new(), &opts).is_err());
}

#[test]
fn expr_fn_arity_is_checked_at_parse() {
    let mut opts = Options::new();
    opts.expr_fn("id", vec!["x".into()], json!(["var", "x"]));
    assert!(parse_with(&json!(["id", 1, 2]), &opts).is_err());
}

#[test]
fn external_function_changes_result_by_argument() {
    let mut opts = Options::new();
    // An external closure that dynamically maps codes to labels.
    opts.external("label", 1, |args, _ctx| {
        let code = args[0].as_str().unwrap_or("");
        Ok(Value::String(
            match code {
                "hi" => "Hospital",
                "sc" => "School",
                _ => "Unknown",
            }
            .to_string(),
        ))
    });
    let expr = parse_with(&json!(["label", ["get", "code"]]), &opts).unwrap();
    let ctx = feature_with("code", Value::String("sc".into()));
    assert_eq!(
        evaluate_with(&expr, &ctx, &opts).unwrap(),
        Value::String("School".into())
    );
}

#[test]
fn external_function_can_read_context() {
    let mut opts = Options::new();
    // zoom + argument, reading the context directly.
    opts.external("plus_zoom", 1, |args, ctx| {
        let n = args[0].as_number().unwrap_or(0.0);
        Ok(Value::Number(n + ctx.zoom.unwrap_or(0.0)))
    });
    let expr = parse_with(&json!(["plus_zoom", 10]), &opts).unwrap();
    let ctx = EvaluationContext::new().with_zoom(5.0);
    assert_eq!(
        evaluate_with(&expr, &ctx, &opts).unwrap(),
        Value::Number(15.0)
    );
}

#[test]
fn options_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Options>();
}

#[test]
fn expr_fn_registered_after_first_evaluation_is_picked_up() {
    // Bodies are compiled lazily and cached; a later registration must
    // invalidate that cache rather than keep evaluating the stale set.
    let mut opts = Options::new();
    opts.expr_fn("one", vec![], json!(1));
    let expr = parse_with(&json!(["one"]), &opts).unwrap();
    assert_eq!(
        evaluate_with(&expr, &EvaluationContext::new(), &opts).unwrap(),
        Value::Number(1.0)
    );

    opts.expr_fn("two", vec![], json!(["+", ["one"], 1]));
    let expr = parse_with(&json!(["two"]), &opts).unwrap();
    assert_eq!(
        evaluate_with(&expr, &EvaluationContext::new(), &opts).unwrap(),
        Value::Number(2.0)
    );
}

#[test]
fn expr_fn_body_that_fails_to_parse_errors_at_evaluation() {
    let mut opts = Options::new();
    opts.expr_fn("bad", vec![], json!(["no-such-op"]));
    let expr = parse_with(&json!(["bad"]), &opts).unwrap();
    assert!(evaluate_with(&expr, &EvaluationContext::new(), &opts).is_err());
    // Still an error on the second call (the cached failure is not lost).
    assert!(evaluate_with(&expr, &EvaluationContext::new(), &opts).is_err());
}
