//! Tests for legacy filter conversion (`maplibre_expr::filter`).
//!
//! Ported from maplibre-style-spec's `src/feature_filter/feature_filter.test.ts`:
//! the `isExpressionFilter` detection cases, the `convertFilter` output-shape
//! cases, and the behavioral table (each legacy filter is converted, parsed and
//! evaluated against a feature).

use std::collections::BTreeMap;

use maplibre_expr::filter::{convert_legacy_filter, is_expression_filter, FilterError};
use maplibre_expr::{evaluate, parse, EvaluationContext, Feature, Value};
use serde_json::{json, Value as Json};

// --- helpers ---------------------------------------------------------------

/// Convert a legacy filter, parse the result, and evaluate it against a
/// feature, returning the boolean outcome.
///
/// A filter expression whose evaluation errors (e.g. an ordered comparison on
/// mismatched types) is treated as `false` — this mirrors MapLibre's filter
/// evaluation, whose spec default is `false`. The exported `convertFilter`
/// relies on exactly this: outside an `any`, a runtime type error and a legacy
/// `false` are equivalent, so ordered comparisons are emitted bare and only
/// `any` disjuncts get preflight `typeof` guards.
fn run(filter: Json, feature: Feature) -> bool {
    let converted = convert_legacy_filter(&filter).expect("conversion should succeed");
    let expr = parse(&converted).unwrap_or_else(|e| panic!("parse of {converted} failed: {e}"));
    let ctx = EvaluationContext::new()
        .with_zoom(0.0)
        .with_feature(feature);
    match evaluate(&expr, &ctx) {
        Ok(Value::Bool(b)) => b,
        Ok(other) => panic!("filter {converted} did not evaluate to a boolean: {other:?}"),
        // A filter that errors excludes the feature (spec default is false).
        Err(_) => false,
    }
}

/// A feature with a single property `foo`.
fn foo(value: Value) -> Feature {
    let mut props = BTreeMap::new();
    props.insert("foo".to_string(), value);
    Feature {
        properties: props,
        ..Feature::default()
    }
}

/// A feature with no properties at all.
fn empty() -> Feature {
    Feature::default()
}

fn geom(ty: &str) -> Feature {
    Feature {
        geometry_type: Some(ty.to_string()),
        ..Feature::default()
    }
}

fn with_id(id: Value) -> Feature {
    Feature {
        id: Some(id),
        ..Feature::default()
    }
}

// --- isExpressionFilter detection ------------------------------------------

#[test]
fn detects_definitely_legacy_filters() {
    // More than two arguments.
    assert!(!is_expression_filter(&json!([
        "in", "color", "red", "blue"
    ])));
    // Second argument is not a string or array.
    assert!(!is_expression_filter(&json!(["in", "value", 42])));
    assert!(!is_expression_filter(&json!(["in", "value", true])));
    // Ambiguous single-value `in`: reported as legacy.
    assert!(!is_expression_filter(&json!(["in", "color", "red"])));
}

#[test]
fn detects_definitely_expressions() {
    assert!(is_expression_filter(&json!([
        "in",
        ["get", "color"],
        "reddish"
    ])));
    assert!(is_expression_filter(&json!([
        "in",
        ["get", "color"],
        ["red", "blue"]
    ])));
    assert!(is_expression_filter(&json!(["in", 42, 42])));
    assert!(is_expression_filter(&json!(["in", true, true])));
    assert!(is_expression_filter(&json!([
        "in",
        "red",
        ["get", "colors"]
    ])));
}

#[test]
fn booleans_are_expressions() {
    assert!(is_expression_filter(&json!(true)));
    assert!(is_expression_filter(&json!(false)));
}

#[test]
fn negated_and_special_forms_are_legacy() {
    assert!(!is_expression_filter(&json!(["!in", "a", "b"])));
    assert!(!is_expression_filter(&json!(["!has", "a"])));
    // `has $id` / `has $type` are legacy special keys, not expressions.
    assert!(!is_expression_filter(&json!(["has", "$id"])));
    assert!(!is_expression_filter(&json!(["has", "$type"])));
    // `has` with a normal property IS a valid modern expression.
    assert!(is_expression_filter(&json!(["has", "foo"])));
}

#[test]
fn any_all_none_detection() {
    // A legacy child makes the whole thing legacy.
    assert!(!is_expression_filter(&json!(["all", ["==", "a", 1]])));
    // An expression child makes the whole thing an expression.
    assert!(is_expression_filter(&json!([
        "all",
        ["==", ["get", "a"], 1]
    ])));
    // An expression child makes the whole thing an expression.
    assert!(is_expression_filter(&json!([
        "all",
        ["==", ["get", "a"], 1]
    ])));
    // Bare `all`/`any` with no children.
    assert!(is_expression_filter(&json!(["all"])));
    assert!(is_expression_filter(&json!(["any"])));
    // `none` with only booleans / no expression children is not an expression.
    assert!(!is_expression_filter(&json!(["none", ["==", "a", 1]])));
    assert!(is_expression_filter(&json!([
        "none",
        ["==", ["get", "a"], 1]
    ])));
}

#[test]
fn recognizes_type_mixed_with_expression_operators() {
    // Issue #1544: `["==", "$type", ...]` alone looks legacy, but a sibling
    // expression child must promote the whole `all` to an expression.
    let filter = json!([
        "all",
        ["==", "$type", "Point"],
        [
            "case",
            ["==", ["get", "id"], ["global-state", "activeTrackId"]],
            true,
            [
                "any",
                ["==", ["get", "role"], "start"],
                ["==", ["get", "role"], "end"]
            ]
        ]
    ]);
    assert!(is_expression_filter(&filter));
}

#[test]
fn converts_legacy_leaves_inside_mixed_combiner() {
    // A combiner (`all`/`any`/`none`) that `is_expression_filter` classifies as
    // an expression — because at least one child is a genuine expression (here
    // `["has", …]`) — but which still carries legacy-only leaves such as a
    // three-arg `["==", "prop", value]` or `["!has", …]`. Upstream MapLibre
    // *rejects* such a mixed filter, but as a renderer we convert the legacy
    // leaves in place so real-world styles (e.g. Protomaps basemap
    // `roads_bridges_*` layers) still render. Genuine expression children pass
    // through unchanged.
    let filter = json!([
        "all",
        ["has", "is_bridge"],
        ["==", "kind", "highway"],
        ["!has", "is_link"]
    ]);
    // Faithful to upstream: the whole thing classifies as an expression …
    assert!(is_expression_filter(&filter));
    // … but conversion still rewrites the legacy leaves, leaving no raw `!has`.
    let converted = convert_legacy_filter(&filter).unwrap();
    assert!(!converted.to_string().contains("!has"));
    assert_eq!(
        converted,
        json!([
            "all",
            ["has", "is_bridge"],
            ["==", ["get", "kind"], "highway"],
            ["!", ["has", "is_link"]]
        ])
    );
}

// --- convertFilter output shape --------------------------------------------

#[test]
fn passes_through_expression_filters_unchanged() {
    let expr = json!(["==", ["get", "x"], 1]);
    assert_eq!(convert_legacy_filter(&expr).unwrap(), expr);
    assert_eq!(convert_legacy_filter(&json!(true)).unwrap(), json!(true));
}

#[test]
fn null_filter_matches_everything() {
    assert_eq!(convert_legacy_filter(&Json::Null).unwrap(), json!(true));
}

#[test]
fn flattens_nested_single_child_all_expressions() {
    let filter = json!([
        "all",
        ["in", "$type", "Polygon", "LineString", "Point"],
        ["all", ["in", "type", "island"]]
    ]);
    let expected = json!([
        "all",
        [
            "match",
            ["geometry-type"],
            ["LineString", "Point", "Polygon"],
            true,
            false
        ],
        ["match", ["get", "type"], ["island"], true, false]
    ]);
    assert_eq!(convert_legacy_filter(&filter).unwrap(), expected);
}

#[test]
fn removes_duplicates_when_outputting_match_expressions() {
    let filter = json!(["in", "$id", 1, 2, 3, 2, 1]);
    let expected = json!(["match", ["id"], [1, 2, 3], true, false]);
    assert_eq!(convert_legacy_filter(&filter).unwrap(), expected);
}

#[test]
fn special_keys_map_to_geometry_type_and_id() {
    assert_eq!(
        convert_legacy_filter(&json!(["==", "$type", "Point"])).unwrap(),
        json!(["==", ["geometry-type"], "Point"])
    );
    assert_eq!(
        convert_legacy_filter(&json!(["==", "$id", 7])).unwrap(),
        json!(["==", ["id"], 7])
    );
    assert_eq!(
        convert_legacy_filter(&json!(["has", "$type"])).unwrap(),
        json!(true)
    );
    assert_eq!(
        convert_legacy_filter(&json!(["has", "$id"])).unwrap(),
        json!(["!=", ["id"], null])
    );
    assert_eq!(
        convert_legacy_filter(&json!(["!has", "$id"])).unwrap(),
        json!(["!", ["!=", ["id"], null]])
    );
}

#[test]
fn null_comparison_guards_on_presence() {
    assert_eq!(
        convert_legacy_filter(&json!(["==", "foo", null])).unwrap(),
        json!(["all", ["has", "foo"], ["==", ["get", "foo"], null]])
    );
    assert_eq!(
        convert_legacy_filter(&json!(["!=", "foo", null])).unwrap(),
        json!(["any", ["!", ["has", "foo"]], ["!=", ["get", "foo"], null]])
    );
}

#[test]
fn non_string_property_is_an_error() {
    assert_eq!(
        convert_legacy_filter(&json!(["==", 42, 5])),
        Err(FilterError::PropertyNotString { op: "==".into() })
    );
}

// --- behavioral table ------------------------------------------------------

#[test]
fn degenerate() {
    assert!(run(json!(["all"]), foo(Value::Number(1.0))));
    assert!(!run(json!(["any"]), foo(Value::Number(1.0))));
    assert_eq!(convert_legacy_filter(&Json::Null).unwrap(), json!(true));
}

#[test]
fn eq_string() {
    let f = || json!(["==", "foo", "bar"]);
    assert!(run(f(), foo(Value::String("bar".into()))));
    assert!(!run(f(), foo(Value::String("baz".into()))));
}

#[test]
fn eq_number() {
    let f = || json!(["==", "foo", 0]);
    assert!(run(f(), foo(Value::Number(0.0))));
    assert!(!run(f(), foo(Value::Number(1.0))));
    assert!(!run(f(), foo(Value::String("0".into()))));
    assert!(!run(f(), foo(Value::Bool(true))));
    assert!(!run(f(), foo(Value::Bool(false))));
    assert!(!run(f(), foo(Value::Null)));
    assert!(!run(f(), empty()));
}

#[test]
fn eq_null() {
    let f = || json!(["==", "foo", null]);
    assert!(!run(f(), foo(Value::Number(0.0))));
    assert!(!run(f(), foo(Value::String("0".into()))));
    assert!(!run(f(), foo(Value::Bool(true))));
    assert!(run(f(), foo(Value::Null)));
    assert!(!run(f(), empty()));
}

#[test]
fn eq_type() {
    let f = || json!(["==", "$type", "LineString"]);
    assert!(!run(f(), geom("Point")));
    assert!(run(f(), geom("LineString")));
}

#[test]
fn eq_id() {
    let f = || json!(["==", "$id", 1234]);
    assert!(run(f(), with_id(Value::Number(1234.0))));
    assert!(!run(f(), with_id(Value::String("1234".into()))));
    // A property named `id` is not `$id`.
    assert!(!run(f(), foo(Value::Number(1234.0))));
}

#[test]
fn ne_string() {
    let f = || json!(["!=", "foo", "bar"]);
    assert!(!run(f(), foo(Value::String("bar".into()))));
    assert!(run(f(), foo(Value::String("baz".into()))));
}

#[test]
fn ne_number() {
    let f = || json!(["!=", "foo", 0]);
    assert!(!run(f(), foo(Value::Number(0.0))));
    assert!(run(f(), foo(Value::Number(1.0))));
    assert!(run(f(), foo(Value::String("0".into()))));
    assert!(run(f(), foo(Value::Bool(true))));
    assert!(run(f(), foo(Value::Null)));
    assert!(run(f(), empty()));
}

#[test]
fn ne_null() {
    let f = || json!(["!=", "foo", null]);
    assert!(run(f(), foo(Value::Number(0.0))));
    assert!(run(f(), foo(Value::String("0".into()))));
    assert!(!run(f(), foo(Value::Null)));
    assert!(run(f(), empty()));
}

#[test]
fn ne_type() {
    let f = || json!(["!=", "$type", "LineString"]);
    assert!(run(f(), geom("Point")));
    assert!(!run(f(), geom("LineString")));
}

#[test]
fn lt_number() {
    let f = || json!(["<", "foo", 0]);
    assert!(!run(f(), foo(Value::Number(1.0))));
    assert!(!run(f(), foo(Value::Number(0.0))));
    assert!(run(f(), foo(Value::Number(-1.0))));
    assert!(!run(f(), foo(Value::String("-1".into()))));
    assert!(!run(f(), foo(Value::Bool(true))));
    assert!(!run(f(), foo(Value::Null)));
    assert!(!run(f(), empty()));
}

#[test]
fn lt_string() {
    let f = || json!(["<", "foo", "0"]);
    assert!(!run(f(), foo(Value::Number(-1.0))));
    assert!(!run(f(), foo(Value::String("1".into()))));
    assert!(!run(f(), foo(Value::String("0".into()))));
    assert!(run(f(), foo(Value::String("-1".into()))));
    assert!(!run(f(), foo(Value::Null)));
}

#[test]
fn lte_number() {
    let f = || json!(["<=", "foo", 0]);
    assert!(!run(f(), foo(Value::Number(1.0))));
    assert!(run(f(), foo(Value::Number(0.0))));
    assert!(run(f(), foo(Value::Number(-1.0))));
    assert!(!run(f(), foo(Value::String("0".into()))));
    assert!(!run(f(), foo(Value::Null)));
    assert!(!run(f(), empty()));
}

#[test]
fn lte_string() {
    let f = || json!(["<=", "foo", "0"]);
    assert!(!run(f(), foo(Value::Number(-1.0))));
    assert!(!run(f(), foo(Value::String("1".into()))));
    assert!(run(f(), foo(Value::String("0".into()))));
    assert!(run(f(), foo(Value::String("-1".into()))));
    assert!(!run(f(), foo(Value::Null)));
}

#[test]
fn gt_number() {
    let f = || json!([">", "foo", 0]);
    assert!(run(f(), foo(Value::Number(1.0))));
    assert!(!run(f(), foo(Value::Number(0.0))));
    assert!(!run(f(), foo(Value::Number(-1.0))));
    assert!(!run(f(), foo(Value::String("1".into()))));
    assert!(!run(f(), foo(Value::Bool(true))));
    assert!(!run(f(), foo(Value::Null)));
    assert!(!run(f(), empty()));
}

#[test]
fn gt_string() {
    let f = || json!([">", "foo", "0"]);
    assert!(!run(f(), foo(Value::Number(1.0))));
    assert!(run(f(), foo(Value::String("1".into()))));
    assert!(!run(f(), foo(Value::String("0".into()))));
    assert!(!run(f(), foo(Value::String("-1".into()))));
    assert!(!run(f(), foo(Value::Null)));
}

#[test]
fn gte_number() {
    let f = || json!([">=", "foo", 0]);
    assert!(run(f(), foo(Value::Number(1.0))));
    assert!(run(f(), foo(Value::Number(0.0))));
    assert!(!run(f(), foo(Value::Number(-1.0))));
    assert!(!run(f(), foo(Value::String("1".into()))));
    assert!(!run(f(), foo(Value::Null)));
    assert!(!run(f(), empty()));
}

#[test]
fn gte_string() {
    let f = || json!([">=", "foo", "0"]);
    assert!(!run(f(), foo(Value::Number(1.0))));
    assert!(run(f(), foo(Value::String("1".into()))));
    assert!(run(f(), foo(Value::String("0".into()))));
    assert!(!run(f(), foo(Value::String("-1".into()))));
    assert!(!run(f(), foo(Value::Null)));
}

#[test]
fn in_degenerate() {
    assert!(!run(json!(["in", "foo"]), foo(Value::Number(1.0))));
}

#[test]
fn in_string() {
    let f = || json!(["in", "foo", "0"]);
    assert!(!run(f(), foo(Value::Number(0.0))));
    assert!(run(f(), foo(Value::String("0".into()))));
    assert!(!run(f(), foo(Value::Bool(true))));
    assert!(!run(f(), foo(Value::Null)));
    assert!(!run(f(), empty()));
}

#[test]
fn in_number() {
    let f = || json!(["in", "foo", 0]);
    assert!(run(f(), foo(Value::Number(0.0))));
    assert!(!run(f(), foo(Value::String("0".into()))));
    assert!(!run(f(), foo(Value::Bool(true))));
    assert!(!run(f(), foo(Value::Null)));
}

#[test]
fn in_null() {
    let f = || json!(["in", "foo", null]);
    assert!(!run(f(), foo(Value::Number(0.0))));
    assert!(!run(f(), foo(Value::String("0".into()))));
    assert!(run(f(), foo(Value::Null)));
}

#[test]
fn in_multiple() {
    let f = || json!(["in", "foo", 0, 1]);
    assert!(run(f(), foo(Value::Number(0.0))));
    assert!(run(f(), foo(Value::Number(1.0))));
    assert!(!run(f(), foo(Value::Number(3.0))));
}

#[test]
fn in_type() {
    let f = || json!(["in", "$type", "LineString", "Polygon"]);
    assert!(!run(f(), geom("Point")));
    assert!(run(f(), geom("LineString")));
    assert!(run(f(), geom("Polygon")));

    let f1 = || json!(["in", "$type", "Polygon", "LineString", "Point"]);
    assert!(run(f1(), geom("Point")));
    assert!(run(f1(), geom("LineString")));
    assert!(run(f1(), geom("Polygon")));
}

#[test]
fn not_in_degenerate() {
    assert!(run(json!(["!in", "foo"]), foo(Value::Number(1.0))));
}

#[test]
fn not_in_string() {
    let f = || json!(["!in", "foo", "0"]);
    assert!(run(f(), foo(Value::Number(0.0))));
    assert!(!run(f(), foo(Value::String("0".into()))));
    assert!(run(f(), foo(Value::Null)));
    assert!(run(f(), empty()));
}

#[test]
fn not_in_number() {
    let f = || json!(["!in", "foo", 0]);
    assert!(!run(f(), foo(Value::Number(0.0))));
    assert!(run(f(), foo(Value::String("0".into()))));
    assert!(run(f(), foo(Value::Null)));
}

#[test]
fn not_in_null() {
    let f = || json!(["!in", "foo", null]);
    assert!(run(f(), foo(Value::Number(0.0))));
    assert!(run(f(), foo(Value::String("0".into()))));
    assert!(!run(f(), foo(Value::Null)));
}

#[test]
fn not_in_multiple() {
    let f = || json!(["!in", "foo", 0, 1]);
    assert!(!run(f(), foo(Value::Number(0.0))));
    assert!(!run(f(), foo(Value::Number(1.0))));
    assert!(run(f(), foo(Value::Number(3.0))));
}

#[test]
fn not_in_type() {
    let f = || json!(["!in", "$type", "LineString", "Polygon"]);
    assert!(run(f(), geom("Point")));
    assert!(!run(f(), geom("LineString")));
    assert!(!run(f(), geom("Polygon")));
}

#[test]
fn any_behavior() {
    assert!(!run(json!(["any"]), foo(Value::Number(1.0))));
    assert!(run(
        json!(["any", ["==", "foo", 1]]),
        foo(Value::Number(1.0))
    ));
    assert!(!run(
        json!(["any", ["==", "foo", 0]]),
        foo(Value::Number(1.0))
    ));
    assert!(run(
        json!(["any", ["==", "foo", 0], ["==", "foo", 1]]),
        foo(Value::Number(1.0))
    ));
}

#[test]
fn all_behavior() {
    assert!(run(json!(["all"]), foo(Value::Number(1.0))));
    assert!(run(
        json!(["all", ["==", "foo", 1]]),
        foo(Value::Number(1.0))
    ));
    assert!(!run(
        json!(["all", ["==", "foo", 0]]),
        foo(Value::Number(1.0))
    ));
    assert!(!run(
        json!(["all", ["==", "foo", 0], ["==", "foo", 1]]),
        foo(Value::Number(1.0))
    ));
}

#[test]
fn none_behavior() {
    assert!(run(json!(["none"]), foo(Value::Number(1.0))));
    assert!(!run(
        json!(["none", ["==", "foo", 1]]),
        foo(Value::Number(1.0))
    ));
    assert!(run(
        json!(["none", ["==", "foo", 0]]),
        foo(Value::Number(1.0))
    ));
    assert!(!run(
        json!(["none", ["==", "foo", 0], ["==", "foo", 1]]),
        foo(Value::Number(1.0))
    ));
}

#[test]
fn has_behavior() {
    let f = || json!(["has", "foo"]);
    assert!(run(f(), foo(Value::Number(0.0))));
    assert!(run(f(), foo(Value::String("0".into()))));
    assert!(run(f(), foo(Value::Bool(false))));
    // A present-but-null property still counts as present.
    assert!(run(f(), foo(Value::Null)));
    // Absent property.
    assert!(!run(f(), empty()));
}

#[test]
fn not_has_behavior() {
    let f = || json!(["!has", "foo"]);
    assert!(!run(f(), foo(Value::Number(0.0))));
    assert!(!run(f(), foo(Value::Bool(false))));
    assert!(!run(f(), foo(Value::Null)));
    assert!(run(f(), empty()));
}

#[test]
fn demotiles_geolines_filter() {
    // The `geolines` / `geolines-label` layers of MapLibre's demotiles style
    // carry this legacy filter, which currently warns in ezu translate.
    let filter = json!(["all", ["!=", "name", "International Date Line"]]);
    assert!(!is_expression_filter(&filter));

    let converted = convert_legacy_filter(&filter).unwrap();
    // A single-child `all` collapses to the child, and the bare property
    // becomes `["get", …]`.
    assert_eq!(
        converted,
        json!(["!=", ["get", "name"], "International Date Line"])
    );

    // And it behaves: keeps everything except the dateline.
    assert!(!run(
        filter.clone(),
        foo_named("name", Value::String("International Date Line".into()))
    ));
    assert!(run(
        filter.clone(),
        foo_named("name", Value::String("Equator".into()))
    ));
    // Missing `name` is not equal to the string, so it passes.
    assert!(run(filter, empty()));
}

fn foo_named(key: &str, value: Value) -> Feature {
    let mut props = BTreeMap::new();
    props.insert(key.to_string(), value);
    Feature {
        properties: props,
        ..Feature::default()
    }
}

#[test]
fn type_mismatch_semantics_in_any() {
    // ["any", ["all", [">","y",0], [">","y",0]], [">","x",0]] with preflight
    // type checks so a mistyped property yields false rather than erroring.
    let filter = json!(["any", ["all", [">", "y", 0], [">", "y", 0]], [">", "x", 0]]);
    let feat = |x: Value, y: Value| {
        let mut props = BTreeMap::new();
        props.insert("x".to_string(), x);
        props.insert("y".to_string(), y);
        Feature {
            properties: props,
            ..Feature::default()
        }
    };
    assert!(run(
        filter.clone(),
        feat(Value::Number(0.0), Value::Number(1.0))
    ));
    assert!(run(
        filter.clone(),
        feat(Value::Number(1.0), Value::Number(0.0))
    ));
    assert!(!run(
        filter.clone(),
        feat(Value::Number(0.0), Value::Number(0.0))
    ));
    assert!(run(filter.clone(), feat(Value::Null, Value::Number(1.0))));
    assert!(run(filter.clone(), feat(Value::Number(1.0), Value::Null)));
    assert!(!run(filter, feat(Value::Null, Value::Null)));
}
