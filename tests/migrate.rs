//! `migrate`: whole-style migration of legacy functions, tokens and filters,
//! checked against the reference implementation's output on the MapLibre demo
//! style (see `tests/fixtures/styles/ATTRIBUTION.md`).

use std::collections::BTreeMap;

use maplibre_expr::filter::parse_filter;
use maplibre_expr::migrate::{migrate, migrate_colors, migrate_property, property_spec};
use maplibre_expr::{
    evaluate, evaluate_with, is_expression, parse, parse_property, parse_property_with, typecheck,
    EvaluationContext, Feature, MigrateError, Options, Value,
};
use serde_json::{json, Value as Json};

fn fixture(name: &str) -> Json {
    let path = format!(
        "{}/tests/fixtures/styles/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn globe_style_matches_reference_migrate_output() {
    let migrated = migrate(&fixture("globe.json")).unwrap();
    assert_eq!(migrated, fixture("globe.migrated.json"));
}

#[test]
fn migrate_is_idempotent() {
    let once = migrate(&fixture("globe.json")).unwrap();
    assert_eq!(migrate(&once).unwrap(), once);
}

#[test]
fn migrated_globe_style_parses_and_typechecks_without_options() {
    let migrated = migrate(&fixture("globe.json")).unwrap();
    for layer in migrated["layers"].as_array().unwrap() {
        let id = layer["id"].as_str().unwrap();
        if let Some(filter) = layer.get("filter") {
            parse_filter(filter).unwrap_or_else(|e| panic!("{id} filter: {e}"));
        }
        for section in ["layout", "paint"] {
            let Some(props) = layer.get(section).and_then(Json::as_object) else {
                continue;
            };
            for (name, value) in props {
                assert!(
                    !value.is_object(),
                    "{id}.{section}.{name} still holds a function object"
                );
                if let Some(s) = value.as_str() {
                    assert!(
                        !s.contains('{'),
                        "{id}.{section}.{name} still holds a token string"
                    );
                }
                if is_expression(value) {
                    let expr =
                        parse(value).unwrap_or_else(|e| panic!("{id}.{section}.{name}: {e}"));
                    typecheck(&expr, None, false)
                        .unwrap_or_else(|e| panic!("{id}.{section}.{name}: {e}"));
                }
            }
        }
    }
}

#[test]
fn migrated_globe_label_evaluates_like_maplibre() {
    let migrated = migrate(&fixture("globe.json")).unwrap();
    let layer = migrated["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == "countries-label")
        .unwrap();

    let mut props = BTreeMap::new();
    props.insert("ABBREV".to_string(), Value::String("J.".into()));
    props.insert("NAME".to_string(), Value::String("Japan".into()));
    let feature = Feature {
        properties: props,
        ..Feature::default()
    };

    // text-field: {ABBREV} below zoom 4, {NAME} from zoom 4 — a token stop function.
    let text_field = parse(&layer["layout"]["text-field"]).unwrap();
    let at = |zoom: f64| {
        evaluate(
            &text_field,
            &EvaluationContext::new()
                .with_zoom(zoom)
                .with_feature(feature.clone()),
        )
        .unwrap()
    };
    assert_eq!(at(3.0), Value::String("J.".into()));
    assert_eq!(at(5.0), Value::String("Japan".into()));

    // text-size: numeric stops must interpolate, not step (spec says interpolated).
    let text_size = parse(&layer["layout"]["text-size"]).unwrap();
    let size = evaluate(&text_size, &EvaluationContext::new().with_zoom(3.0)).unwrap();
    assert_eq!(size, Value::Number(11.0)); // halfway between [2, 10] and [4, 12]
}

#[test]
fn non_v8_styles_are_rejected() {
    let err = migrate(&json!({"version": 7, "layers": []})).unwrap_err();
    assert_eq!(err, MigrateError::UnsupportedVersion(json!(7)));
    assert_eq!(err.to_string(), "Cannot migrate from 7");

    let err = migrate(&json!({"layers": []})).unwrap_err();
    assert_eq!(err.to_string(), "Cannot migrate from undefined");
}

#[test]
fn malformed_filter_names_the_layer() {
    let style = json!({
        "version": 8,
        "layers": [{"id": "bad", "type": "line", "filter": ["==", 1, 2]}],
    });
    match migrate(&style).unwrap_err() {
        MigrateError::Filter { layer, .. } => assert_eq!(layer, "bad"),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn per_property_migration_uses_the_property_spec() {
    // Interpolated number property → interpolate (not step).
    assert_eq!(
        migrate_property("line-width", &json!({"stops": [[0, 2], [6, 6]]})),
        json!(["interpolate", ["linear"], ["zoom"], 0, 2, 6, 6])
    );
    // Enum property → step.
    assert_eq!(
        migrate_property(
            "text-transform",
            &json!({"stops": [[0, "uppercase"], [2, "none"]]})
        ),
        json!(["step", ["zoom"], "uppercase", 2, "none"])
    );
    // Token string in a tokens property.
    assert_eq!(
        migrate_property("text-field", &json!("{name}")),
        json!(["to-string", ["get", "name"]])
    );
    assert_eq!(
        migrate_property("text-field", &json!("{name} ({ref})")),
        json!(["concat", ["get", "name"], " (", ["get", "ref"], ")"])
    );
    // Strings are only tokens where the spec says so.
    assert_eq!(
        migrate_property("text-font", &json!("{name}")),
        json!("{name}")
    );
    // Already an expression, unknown property, transition: untouched.
    let expr = json!(["get", "x"]);
    assert_eq!(migrate_property("line-width", &expr), expr);
    assert_eq!(
        migrate_property("no-such-prop", &json!({"stops": []})),
        json!({"stops": []})
    );
    let transition = json!({"duration": 300});
    assert_eq!(
        migrate_property("line-color-transition", &transition),
        transition
    );
}

#[test]
fn colors_are_normalised_in_color_properties_only() {
    assert_eq!(
        migrate_property("line-color", &json!("hsl(900, 0.15, 90%)")),
        json!("hsl(900,15%,90%)")
    );
    // Inside an expression too.
    assert_eq!(
        migrate_property(
            "line-color",
            &json!(["case", true, "hsla(1, .5, .5, 0.4)", "red"])
        ),
        json!(["case", true, "hsla(1,50%,50%,0.4)", "red"])
    );
    // Not for non-color properties.
    assert_eq!(
        migrate_property("text-field", &json!("hsl(900, 0.15, 90%)")),
        json!("hsl(900,15%,90%)".replace(",", ", ").replace("15%", "0.15"))
    );
    assert_eq!(migrate_colors(&json!("rgb(1,2,3)")), json!("rgb(1,2,3)"));
}

#[test]
fn reference_is_embedded() {
    assert_eq!(property_spec("line-width").unwrap()["type"], "number");
    assert_eq!(property_spec("text-field").unwrap()["tokens"], true);
    assert!(property_spec("nope").is_none());
}

fn feature_with(key: &str, value: Value) -> EvaluationContext {
    let mut props = BTreeMap::new();
    props.insert(key.to_string(), value);
    EvaluationContext::new().with_feature(Feature {
        properties: props,
        ..Feature::default()
    })
}

#[test]
fn parse_property_converts_with_the_property_spec() {
    // Same stops: interpolated for a number property, stepped for an enum one.
    let stops = json!({"stops": [[2, 10], [6, 16]]});
    let width = parse_property("line-width", &stops).unwrap();
    assert_eq!(
        evaluate(&width, &EvaluationContext::new().with_zoom(4.0)).unwrap(),
        Value::Number(13.0)
    );
    // `parse` itself, like `createExpression`, does not read function objects.
    assert!(parse(&stops).is_err());

    // Token strings resolve against the feature.
    let label = parse_property("text-field", &json!("{name}!")).unwrap();
    assert_eq!(
        evaluate(&label, &feature_with("name", Value::String("Tokyo".into()))).unwrap(),
        Value::String("Tokyo!".into())
    );
    // …but only for token-accepting properties.
    let font = parse_property("text-font", &json!("{name}")).unwrap();
    assert_eq!(
        evaluate(&font, &EvaluationContext::new()).unwrap(),
        Value::String("{name}".into())
    );
    // Expressions and literals pass straight through.
    let expr = parse_property("line-width", &json!(["*", ["get", "w"], 2])).unwrap();
    assert_eq!(
        evaluate(&expr, &feature_with("w", Value::Number(3.0))).unwrap(),
        Value::Number(6.0)
    );
    let lit = parse_property("line-width", &json!(4)).unwrap();
    assert_eq!(
        evaluate(&lit, &EvaluationContext::new()).unwrap(),
        Value::Number(4.0)
    );
}

#[test]
fn parse_property_with_accepts_options() {
    let mut opts = Options::new();
    opts.macro_def("double", vec!["x".into()], json!(["*", ["var", "x"], 2]));
    let expr = parse_property_with("line-width", &json!(["double", 3]), &opts).unwrap();
    assert_eq!(
        evaluate_with(&expr, &EvaluationContext::new(), &opts).unwrap(),
        Value::Number(6.0)
    );
}
