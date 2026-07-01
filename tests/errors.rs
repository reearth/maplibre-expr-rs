//! Tests for the semantic error kinds and location keys.

use maplibre_expr::{parse, typecheck, ParseErrorKind, Type};
use serde_json::json;

fn compile_err(expr: serde_json::Value, expected: Option<Type>) -> maplibre_expr::ParseError {
    let parsed = parse(&expr);
    match parsed {
        Err(e) => e,
        Ok(expr) => typecheck(&expr, expected.as_ref(), false).unwrap_err(),
    }
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
