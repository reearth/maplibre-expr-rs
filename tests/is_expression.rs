//! `is_expression` — telling a data-driven property expression apart from a
//! literal value that merely happens to be an array (e.g. a font stack).

use maplibre_expr::is_expression;
use serde_json::json;

#[test]
fn arrays_headed_by_an_operator_are_expressions() {
    assert!(is_expression(&json!(["get", "x"])));
    assert!(is_expression(&json!(["case", true, 1, 0])));
    assert!(is_expression(&json!(["match", ["get", "k"], "a", 1, 0])));
    assert!(is_expression(&json!(["step", ["zoom"], 0, 10, 1])));
    assert!(is_expression(&json!([
        "coalesce",
        ["get", "a"],
        ["get", "b"]
    ])));
    assert!(is_expression(&json!(["literal", ["A", "B"]])));
    // Special forms absent from the arity table are still operators.
    assert!(is_expression(&json!([
        "interpolate-hcl",
        ["linear"],
        ["zoom"],
        0,
        "red"
    ])));
    assert!(is_expression(&json!(["let", "n", 1, ["var", "n"]])));
}

#[test]
fn is_a_head_check_not_an_arity_check() {
    // Wrong arity, but the head is an operator → still an expression.
    assert!(is_expression(&json!(["get"])));
    assert!(is_expression(&json!(["rgb"])));
}

#[test]
fn literal_arrays_and_font_stacks_are_not_expressions() {
    // A bare font stack: neither name heads a built-in operator.
    assert!(!is_expression(&json!([
        "Open Sans Regular",
        "Arial Unicode MS Regular"
    ])));
    assert!(!is_expression(&json!(["Noto Sans Regular"])));
}

#[test]
fn non_arrays_and_empty_arrays_are_not_expressions() {
    assert!(!is_expression(&json!([])));
    assert!(!is_expression(&json!("get")));
    assert!(!is_expression(&json!(42)));
    assert!(!is_expression(&json!(true)));
    assert!(!is_expression(&json!(null)));
    // Legacy function object.
    assert!(!is_expression(
        &json!({ "stops": [[0, ["A"]]], "property": "p" })
    ));
    // First element not a string.
    assert!(!is_expression(&json!([["get", "x"], 1])));
}
