//! Converting legacy MapLibre *function objects* into modern expressions.
//!
//! Before expressions existed, data- and zoom-driven styling was expressed with
//! *function objects* — `{ "type": "exponential", "property": "x", "stops":
//! [...] }` and friends. MapLibre still accepts them, converting each to the
//! equivalent modern expression (`interpolate` / `step` / `match` / `case` / …)
//! before parsing. This module is a port of maplibre-style-spec's
//! `src/function/convert.ts`, so the produced expressions match the reference
//! implementation.
//!
//! [`convert_function`] does the conversion given the function object and its
//! *property spec* (the style-spec entry for the property being styled). The
//! spec supplies information the object alone lacks — whether the property is
//! interpolatable (which picks `exponential` vs `interval` when `type` is
//! omitted), whether `{token}` strings expand to `["get", …]`, and the item
//! type for identity `array`/`enum`/`color` properties. Pass `&Value::Null`
//! (or an empty object) when no spec is available: conversion then relies only
//! on the object's own `type`/`base`/`default`/`stops`/`property` fields, which
//! covers the common cases.
//!
//! By default [`parse`](crate::parse) applies this transparently: hand it either
//! a modern expression or a legacy function object and it does the right thing.
//! Disable that with [`Options::convert_legacy`](crate::Options::convert_legacy)
//! when you want bare objects to be rejected instead.

use serde_json::{json, Value as Json};

/// Whether `value` looks like a legacy function object — i.e. a JSON object
/// carrying `stops` (interval/exponential/categorical) or `property` (an
/// identity function). Other objects are not functions and remain parse errors.
pub fn is_function(value: &Json) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.contains_key("stops") || o.contains_key("property"))
}

/// Convert a legacy function object to the equivalent modern expression.
///
/// `params` is the function object; `spec` is the property's style-spec entry
/// (pass `&Value::Null` when unavailable). The result is a modern expression as
/// raw JSON, ready for [`parse`](crate::parse).
pub fn convert_function(params: &Json, spec: &Json) -> Json {
    let Some(raw_stops) = params.get("stops").and_then(Json::as_array) else {
        return convert_identity(params, spec);
    };
    let zoom_and_feature = raw_stops
        .first()
        .and_then(Json::as_array)
        .and_then(|s| s.first())
        .map(Json::is_object)
        .unwrap_or(false);
    let feature_dependent = zoom_and_feature || params.get("property").is_some();
    let zoom_dependent = zoom_and_feature || !feature_dependent;
    let tokens = spec.get("tokens").and_then(Json::as_bool).unwrap_or(false);

    let stops: Vec<(Json, Json)> = raw_stops
        .iter()
        .filter_map(Json::as_array)
        .filter(|s| s.len() >= 2)
        .map(|s| {
            let output = if !feature_dependent && tokens && s[1].is_string() {
                convert_token_string(s[1].as_str().unwrap())
            } else {
                convert_literal(&s[1])
            };
            (s[0].clone(), output)
        })
        .collect();

    if zoom_and_feature {
        convert_zoom_and_property(params, spec, &stops)
    } else if zoom_dependent {
        convert_zoom(params, spec, &stops, json!(["zoom"]))
    } else {
        convert_property(params, spec, &stops)
    }
}

fn convert_literal(v: &Json) -> Json {
    if v.is_object() || v.is_array() {
        json!(["literal", v])
    } else {
        v.clone()
    }
}

fn function_type(params: &Json, spec: &Json) -> String {
    if let Some(t) = params.get("type").and_then(Json::as_str) {
        return t.to_string();
    }
    let interpolated = spec
        .get("expression")
        .and_then(|e| e.get("interpolated"))
        .and_then(Json::as_bool)
        .unwrap_or(false);
    if interpolated {
        "exponential"
    } else {
        "interval"
    }
    .to_string()
}

fn interpolate_operator(params: &Json) -> &'static str {
    match params.get("colorSpace").and_then(Json::as_str) {
        Some("hcl") => "interpolate-hcl",
        Some("lab") => "interpolate-lab",
        _ => "interpolate",
    }
}

fn get_fallback(params: &Json, spec: &Json) -> Json {
    let d = params
        .get("default")
        .or_else(|| spec.get("default"))
        .cloned();
    match d {
        Some(v) => convert_literal(&v),
        None => Json::Null,
    }
}

fn append_stop_pair(curve: &mut Vec<Json>, input: Json, output: Json, is_step: bool) {
    if curve.len() > 3 && curve.get(curve.len() - 2) == Some(&input) {
        return;
    }
    if !(is_step && curve.len() == 2) {
        curve.push(input);
    }
    curve.push(output);
}

fn fixup_degenerate_step(curve: &mut Vec<Json>) {
    if curve.first().and_then(Json::as_str) == Some("step") && curve.len() == 3 {
        let out = curve[2].clone();
        curve.push(json!(0));
        curve.push(out);
    }
}

fn convert_token_string(s: &str) -> Json {
    // Replace `{tokens}` with `["get", token]`, concatenating literal spans.
    let mut result: Vec<Json> = vec![json!("concat")];
    let bytes = s.as_bytes();
    let mut pos = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(end) = s[i..].find('}') {
                let close = i + end;
                if i > pos {
                    result.push(json!(&s[pos..i]));
                }
                result.push(json!(["get", &s[i + 1..close]]));
                i = close + 1;
                pos = i;
                continue;
            }
        }
        i += 1;
    }
    if result.len() == 1 {
        return json!(s);
    }
    if pos < s.len() {
        result.push(json!(&s[pos..]));
    } else if result.len() == 2 {
        return json!(["to-string", result[1]]);
    }
    Json::Array(result)
}

fn convert_identity(params: &Json, spec: &Json) -> Json {
    let get = json!(["get", params.get("property")]);
    let spec_type = spec.get("type").and_then(Json::as_str).unwrap_or("");
    if params.get("default").is_none() {
        return if spec_type == "string" {
            json!(["string", get])
        } else {
            get
        };
    }
    if spec_type == "enum" {
        let keys: Vec<Json> = spec
            .get("values")
            .and_then(Json::as_object)
            .map(|o| o.keys().map(|k| json!(k)).collect())
            .unwrap_or_default();
        return json!(["match", get, keys, get, params.get("default")]);
    }
    let op = if spec_type == "color" {
        "to-color"
    } else {
        spec_type
    };
    let mut expr = vec![json!(op)];
    if spec_type == "array" {
        expr.push(spec.get("value").cloned().unwrap_or(Json::Null));
        expr.push(spec.get("length").cloned().unwrap_or(Json::Null));
    }
    expr.push(get);
    expr.push(convert_literal(params.get("default").unwrap()));
    Json::Array(expr)
}

fn convert_property(params: &Json, spec: &Json, stops: &[(Json, Json)]) -> Json {
    let ty = function_type(params, spec);
    let get = json!(["get", params.get("property")]);
    match ty.as_str() {
        "categorical" if stops.first().map(|s| s.0.is_boolean()).unwrap_or(false) => {
            let mut expr = vec![json!("case")];
            for (input, output) in stops {
                expr.push(json!(["==", get, input]));
                expr.push(output.clone());
            }
            expr.push(get_fallback(params, spec));
            Json::Array(expr)
        }
        "categorical" => {
            let mut expr = vec![json!("match"), get];
            for (input, output) in stops {
                append_stop_pair(&mut expr, input.clone(), output.clone(), false);
            }
            expr.push(get_fallback(params, spec));
            Json::Array(expr)
        }
        "interval" => {
            let mut expr = vec![json!("step"), json!(["number", get])];
            for (input, output) in stops {
                append_stop_pair(&mut expr, input.clone(), output.clone(), true);
            }
            fixup_degenerate_step(&mut expr);
            wrap_default(params, Json::Array(expr), &get)
        }
        _ => {
            // exponential
            let base = params.get("base").and_then(Json::as_f64).unwrap_or(1.0);
            let interp = if base == 1.0 {
                json!(["linear"])
            } else {
                json!(["exponential", base])
            };
            let mut expr = vec![
                json!(interpolate_operator(params)),
                interp,
                json!(["number", get]),
            ];
            for (input, output) in stops {
                append_stop_pair(&mut expr, input.clone(), output.clone(), false);
            }
            wrap_default(params, Json::Array(expr), &get)
        }
    }
}

/// Wrap an interval/exponential property function in a `case` that falls back
/// to the default when the property is not a number.
fn wrap_default(params: &Json, expr: Json, get: &Json) -> Json {
    match params.get("default") {
        None => expr,
        Some(default) => json!([
            "case",
            ["==", ["typeof", get], "number"],
            expr,
            convert_literal(default)
        ]),
    }
}

fn convert_zoom(params: &Json, spec: &Json, stops: &[(Json, Json)], input: Json) -> Json {
    let ty = function_type(params, spec);
    let (mut expr, is_step) = if ty == "interval" {
        (vec![json!("step"), input], true)
    } else {
        let base = params.get("base").and_then(Json::as_f64).unwrap_or(1.0);
        let interp = if base == 1.0 {
            json!(["linear"])
        } else {
            json!(["exponential", base])
        };
        (
            vec![json!(interpolate_operator(params)), interp, input],
            false,
        )
    };
    for (i, o) in stops {
        append_stop_pair(&mut expr, i.clone(), o.clone(), is_step);
    }
    fixup_degenerate_step(&mut expr);
    Json::Array(expr)
}

fn convert_zoom_and_property(params: &Json, spec: &Json, stops: &[(Json, Json)]) -> Json {
    // Group stops by zoom level, preserving encounter order.
    let mut zooms: Vec<f64> = Vec::new();
    let mut grouped: Vec<Vec<(Json, Json)>> = Vec::new();
    for (key, output) in stops {
        let zoom = key.get("zoom").and_then(Json::as_f64).unwrap_or(0.0);
        let value = key.get("value").cloned().unwrap_or(Json::Null);
        match zooms.iter().position(|z| *z == zoom) {
            Some(idx) => grouped[idx].push((value, output.clone())),
            None => {
                zooms.push(zoom);
                grouped.push(vec![(value, output.clone())]);
            }
        }
    }
    let feature_params = |zoom: f64| {
        let mut m = serde_json::Map::new();
        m.insert("zoom".into(), json!(zoom));
        for key in ["type", "property", "default"] {
            if let Some(v) = params.get(key) {
                m.insert(key.into(), v.clone());
            }
        }
        Json::Object(m)
    };
    let ty = function_type(&json!({}), spec);
    if ty == "exponential" {
        let mut expr = vec![
            json!(interpolate_operator(params)),
            json!(["linear"]),
            json!(["zoom"]),
        ];
        for (i, z) in zooms.iter().enumerate() {
            let output = convert_property(&feature_params(*z), spec, &grouped[i]);
            append_stop_pair(&mut expr, json!(z), output, false);
        }
        Json::Array(expr)
    } else {
        let mut expr = vec![json!("step"), json!(["zoom"])];
        for (i, z) in zooms.iter().enumerate() {
            let output = convert_property(&feature_params(*z), spec, &grouped[i]);
            append_stop_pair(&mut expr, json!(z), output, true);
        }
        fixup_degenerate_step(&mut expr);
        Json::Array(expr)
    }
}
