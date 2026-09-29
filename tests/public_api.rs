//! The public surface, exercised from outside the crate.
//!
//! `Expr` and `Value` are public enums, so every type inside a public variant
//! has to be nameable — otherwise a downstream crate can pattern-match a
//! `Value::Formatted` but cannot construct one. This file is the compile-time
//! proof that it can: if any of these names stops being re-exported, the test
//! suite fails to build.

use maplibre_expr::{
    evaluate, format_number, is_subtype, parse, Color, EvaluationContext, Expr, Feature, FormatArg,
    FormatSection, SimpleGeom, Type, Value,
};
use serde_json::json;

/// `Value::Formatted` carries `FormatSection`s, so the type must be nameable
/// and its fields constructible.
#[test]
fn format_section_is_nameable_and_constructible() {
    let section = FormatSection {
        text: "hi".to_string(),
        image: None,
        scale: Some(1.5),
        font_stack: Some("Arial".to_string()),
        text_color: Some(Color::new(1.0, 0.0, 0.0, 1.0)),
        vertical_align: None,
    };
    let value = Value::Formatted(vec![section.clone()]);
    assert_eq!(value.type_name(), "formatted");
    match value {
        Value::Formatted(sections) => assert_eq!(sections, vec![section]),
        other => panic!("{other:?}"),
    }
}

/// `Expr::Format` carries `FormatArg`s. Building one by hand and evaluating it
/// is the shape a downstream code generator needs.
#[test]
fn format_arg_is_nameable_and_evaluable() {
    let expr = Expr::Format(vec![FormatArg {
        content: Expr::Literal(Value::String("hello".to_string())),
        scale: Some(Expr::Literal(Value::Number(2.0))),
        font: None,
        text_color: None,
        vertical_align: None,
    }]);
    match evaluate(&expr, &EvaluationContext::new()).unwrap() {
        Value::Formatted(sections) => {
            assert_eq!(sections.len(), 1);
            assert_eq!(sections[0].text, "hello");
            assert_eq!(sections[0].scale, Some(2.0));
        }
        other => panic!("{other:?}"),
    }
}

/// `Expr::Distance` carries `SimpleGeom`s.
#[test]
fn simple_geom_is_nameable_and_evaluable() {
    let expr = Expr::Distance(vec![SimpleGeom::Point((0.0, 0.0))]);
    let mut ctx = EvaluationContext::new().with_feature(Feature {
        geometry_type: Some("Point".to_string()),
        geometry: vec![vec![(0.0, 0.0)]],
        ..Feature::default()
    });
    ctx.canonical = Some((0, 0, 0));
    match evaluate(&expr, &ctx).unwrap() {
        Value::Number(n) => assert!(n.abs() < 1.0, "distance to itself was {n}"),
        other => panic!("{other:?}"),
    }
    // The other two shapes are nameable too.
    let _ = Expr::Distance(vec![
        SimpleGeom::Line(vec![(0.0, 0.0), (1.0, 1.0)]),
        SimpleGeom::Polygon(vec![vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 0.0)]]),
    ]);
}

/// `Type` is public, so its canonical subtype predicate travels with it.
#[test]
fn is_subtype_is_callable() {
    assert!(is_subtype(&Type::Value, &Type::Number));
    assert!(!is_subtype(&Type::Number, &Type::Value));
    assert!(is_subtype(
        &Type::Array(Box::new(Type::Value), None),
        &Type::Array(Box::new(Type::Number), Some(2)),
    ));
    assert!(!is_subtype(
        &Type::Array(Box::new(Type::Number), Some(3)),
        &Type::Array(Box::new(Type::Number), Some(2)),
    ));
}

/// `format_number` is how this crate renders a number as a string everywhere
/// (`to-string`, `concat`, error messages); it is a standalone port of
/// ECMA-262 `Number::toString`, so it is worth having on the public surface.
/// These are the cases that differ from Rust's own `f64` formatting.
#[test]
fn format_number_is_callable() {
    assert_eq!(format_number(1.0), "1");
    assert_eq!(format_number(-0.0), "0");
    assert_eq!(format_number(1e21), "1e+21");
    assert_eq!(format_number(1e-7), "1e-7");
    assert_eq!(format_number(f64::NAN), "NaN");
    assert_eq!(format_number(f64::INFINITY), "Infinity");
    // The same rendering the evaluator reaches through `to-string`.
    let expr = parse(&json!(["to-string", 1e21])).unwrap();
    assert_eq!(
        evaluate(&expr, &EvaluationContext::new()).unwrap(),
        Value::String(format_number(1e21))
    );
}

/// The `as_*` accessors are the documented way to get out of a `Value`; they
/// only make sense as a set.
#[test]
fn value_accessors() {
    assert_eq!(Value::Bool(true).as_bool(), Some(true));
    assert_eq!(Value::Number(1.0).as_bool(), None);
    assert_eq!(Value::Number(2.5).as_number(), Some(2.5));
    assert_eq!(Value::String("s".into()).as_str(), Some("s"));
}
