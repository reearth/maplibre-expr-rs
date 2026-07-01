//! Conformance harness: runs the vendored `maplibre-style-spec` expression
//! fixtures, one libtest case per fixture directory.
//!
//! Each `test.json` carries an `expression`, a list of `inputs`, and the
//! `expected` compile result plus per-input `outputs`. For every fixture we:
//!
//!   1. parse the expression (checking success vs. compile-error), then
//!   2. evaluate it against each input and compare to the expected output,
//!      matching `{ "error": ... }` outputs against evaluation errors.
//!
//! Compilation is modeled as parse + [`typecheck`] (the latter fed the expected
//! type derived from the fixture's `propertySpec`). Fixtures listed in
//! `tests/known_failures.txt` are reported as *ignored* rather than failing —
//! that file is the running to-do list of operators and behaviours not yet
//! implemented. Nothing is skipped silently: the count of ignored fixtures is
//! printed by the runner and the list is under version control.
//!
//! ## Scope note
//!
//! This harness verifies `compiled.result` (success vs. error), the per-input
//! `outputs`, and error-message/location-`key` parity against the fixtures'
//! `expected.compiled.errors` / `outputs[i].error`. It does **not** assert the
//! other static-analysis fields (`type`, `isFeatureConstant`, `isZoomConstant`).
//! Run with `PARITY=1` for an error-parity coverage report instead.

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use libtest_mimic::{Arguments, Failed, Trial};
use maplibre_expr::{evaluate, parse, typecheck, EvaluationContext, Feature, Type, Value};
use serde_json::Value as Json;

/// Map a fixture's `propertySpec` to the expected expression [`Type`], so the
/// type-checker sees the same expectation MapLibre derives from the spec.
fn property_spec_type(spec: &Json) -> Option<Type> {
    let scalar = |t: &str| match t {
        "color" => Some(Type::Color),
        "number" => Some(Type::Number),
        "string" | "enum" => Some(Type::String),
        "boolean" => Some(Type::Boolean),
        "formatted" => Some(Type::Formatted),
        "resolvedImage" => Some(Type::ResolvedImage),
        "padding" => Some(Type::Padding),
        "numberArray" => Some(Type::NumberArray),
        "colorArray" => Some(Type::ColorArray),
        "projectionDefinition" => Some(Type::ProjectionDefinition),
        "variableAnchorOffsetCollection" => Some(Type::VariableAnchorOffsetCollection),
        _ => None,
    };
    match spec.get("type").and_then(Json::as_str)? {
        "array" => {
            let item = spec
                .get("value")
                .and_then(Json::as_str)
                .and_then(scalar)
                .unwrap_or(Type::Value);
            let n = spec
                .get("length")
                .and_then(Json::as_u64)
                .map(|v| v as usize);
            Some(Type::array(item, n))
        }
        other => scalar(other),
    }
}

// ---- legacy function → expression conversion -------------------------------
//
// Ported from maplibre-style-spec's `src/function/convert.ts`. Legacy function
// objects (`{type, property, stops, ...}`) are converted to the equivalent
// modern expression before parsing.

fn convert_literal(v: &Json) -> Json {
    if v.is_object() || v.is_array() {
        serde_json::json!(["literal", v])
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
        curve.push(serde_json::json!(0));
        curve.push(out);
    }
}

fn convert_token_string(s: &str) -> Json {
    // Replace `{tokens}` with `["get", token]`, concatenating literal spans.
    let mut result: Vec<Json> = vec![serde_json::json!("concat")];
    let bytes = s.as_bytes();
    let mut pos = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(end) = s[i..].find('}') {
                let close = i + end;
                if i > pos {
                    result.push(serde_json::json!(&s[pos..i]));
                }
                result.push(serde_json::json!(["get", &s[i + 1..close]]));
                i = close + 1;
                pos = i;
                continue;
            }
        }
        i += 1;
    }
    if result.len() == 1 {
        return serde_json::json!(s);
    }
    if pos < s.len() {
        result.push(serde_json::json!(&s[pos..]));
    } else if result.len() == 2 {
        return serde_json::json!(["to-string", result[1]]);
    }
    Json::Array(result)
}

fn convert_identity(params: &Json, spec: &Json) -> Json {
    let get = serde_json::json!(["get", params.get("property")]);
    let spec_type = spec.get("type").and_then(Json::as_str).unwrap_or("");
    if params.get("default").is_none() {
        return if spec_type == "string" {
            serde_json::json!(["string", get])
        } else {
            get
        };
    }
    if spec_type == "enum" {
        let keys: Vec<Json> = spec
            .get("values")
            .and_then(Json::as_object)
            .map(|o| o.keys().map(|k| serde_json::json!(k)).collect())
            .unwrap_or_default();
        return serde_json::json!(["match", get, keys, get, params.get("default")]);
    }
    let op = if spec_type == "color" {
        "to-color"
    } else {
        spec_type
    };
    let mut expr = vec![serde_json::json!(op)];
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
    let get = serde_json::json!(["get", params.get("property")]);
    match ty.as_str() {
        "categorical" if stops.first().map(|s| s.0.is_boolean()).unwrap_or(false) => {
            let mut expr = vec![serde_json::json!("case")];
            for (input, output) in stops {
                expr.push(serde_json::json!(["==", get, input]));
                expr.push(output.clone());
            }
            expr.push(get_fallback(params, spec));
            Json::Array(expr)
        }
        "categorical" => {
            let mut expr = vec![serde_json::json!("match"), get];
            for (input, output) in stops {
                append_stop_pair(&mut expr, input.clone(), output.clone(), false);
            }
            expr.push(get_fallback(params, spec));
            Json::Array(expr)
        }
        "interval" => {
            let mut expr = vec![
                serde_json::json!("step"),
                serde_json::json!(["number", get]),
            ];
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
                serde_json::json!(["linear"])
            } else {
                serde_json::json!(["exponential", base])
            };
            let mut expr = vec![
                serde_json::json!(interpolate_operator(params)),
                interp,
                serde_json::json!(["number", get]),
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
        Some(default) => serde_json::json!([
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
        (vec![serde_json::json!("step"), input], true)
    } else {
        let base = params.get("base").and_then(Json::as_f64).unwrap_or(1.0);
        let interp = if base == 1.0 {
            serde_json::json!(["linear"])
        } else {
            serde_json::json!(["exponential", base])
        };
        (
            vec![
                serde_json::json!(interpolate_operator(params)),
                interp,
                input,
            ],
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
        m.insert("zoom".into(), serde_json::json!(zoom));
        for key in ["type", "property", "default"] {
            if let Some(v) = params.get(key) {
                m.insert(key.into(), v.clone());
            }
        }
        Json::Object(m)
    };
    let ty = function_type(&serde_json::json!({}), spec);
    if ty == "exponential" {
        let mut expr = vec![
            serde_json::json!(interpolate_operator(params)),
            serde_json::json!(["linear"]),
            serde_json::json!(["zoom"]),
        ];
        for (i, z) in zooms.iter().enumerate() {
            let output = convert_property(&feature_params(*z), spec, &grouped[i]);
            append_stop_pair(&mut expr, serde_json::json!(z), output, false);
        }
        Json::Array(expr)
    } else {
        let mut expr = vec![serde_json::json!("step"), serde_json::json!(["zoom"])];
        for (i, z) in zooms.iter().enumerate() {
            let output = convert_property(&feature_params(*z), spec, &grouped[i]);
            append_stop_pair(&mut expr, serde_json::json!(z), output, true);
        }
        fixup_degenerate_step(&mut expr);
        Json::Array(expr)
    }
}

/// Convert a legacy function object to the equivalent modern expression.
fn convert_function(params: &Json, spec: &Json) -> Json {
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
        convert_zoom(params, spec, &stops, serde_json::json!(["zoom"]))
    } else {
        convert_property(params, spec, &stops)
    }
}

fn main() {
    let args = Arguments::from_args();

    let root = fixtures_root();
    let known = load_known_failures();
    let mut fixtures = Vec::new();
    collect(&root, &root, &mut fixtures);
    fixtures.sort();

    if std::env::var_os("PARITY").is_some() {
        run_parity(&fixtures);
        return;
    }

    let trials = fixtures
        .into_iter()
        .map(|(name, path)| {
            let ignored = known.contains(&name);
            let mut trial = Trial::test(name, move || run_fixture(&path));
            if ignored {
                trial = trial.with_ignored_flag(true);
            }
            trial
        })
        .collect();

    libtest_mimic::run(&args, trials).exit();
}

/// Assessment mode (`PARITY=1 cargo test --test spec`): instead of pass/fail,
/// measure how closely our error *text* and location *key* match the fixtures'
/// `expected.compiled.errors` (Tier B/C), and the per-input eval-error text.
fn run_parity(fixtures: &[(String, PathBuf)]) {
    // Compile-error parity.
    let mut ce_total = 0usize; // fixtures expecting a compile error
    let mut ce_raised = 0usize; // ... where we also raised one
    let mut ce_msg_exact = 0usize; // ... with byte-identical message
    let mut ce_key_present = 0usize; // ... whose expected key is non-empty
    let mut ce_key_exact = 0usize; // ... where our key matches
    let mut msg_mismatches: Vec<(String, String, String)> = Vec::new();
    let mut key_mismatches: Vec<(String, String, String)> = Vec::new();

    // Eval-error parity.
    let mut ee_total = 0usize;
    let mut ee_raised = 0usize;
    let mut ee_msg_exact = 0usize;
    let mut ee_mismatches: Vec<(String, String, String)> = Vec::new();

    for (name, path) in fixtures {
        let Ok(raw) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(doc) = serde_json::from_str::<Json>(&raw) else {
            continue;
        };
        let Some(expression) = doc.get("expression") else {
            continue;
        };
        let expected = doc.get("expected").cloned().unwrap_or(Json::Null);
        let compiled_result = expected
            .get("compiled")
            .and_then(|c| c.get("result"))
            .and_then(Json::as_str)
            .unwrap_or("success");

        // Legacy stop-function objects are converted before parsing.
        let converted;
        let expression = if expression.is_object() {
            match doc.get("propertySpec") {
                Some(spec) => {
                    converted = convert_function(expression, spec);
                    &converted
                }
                None => expression,
            }
        } else {
            expression
        };

        let expected_type = doc.get("propertySpec").and_then(property_spec_type);
        let coerce_top_string = doc
            .get("propertySpec")
            .and_then(|s| s.get("type"))
            .and_then(Json::as_str)
            == Some("string");
        let compiled = parse(expression)
            .and_then(|e| typecheck(&e, expected_type.as_ref(), coerce_top_string));

        if compiled_result == "error" {
            ce_total += 1;
            let want = expected
                .get("compiled")
                .and_then(|c| c.get("errors"))
                .and_then(Json::as_array)
                .and_then(|a| a.first());
            let want_msg = want
                .and_then(|e| e.get("error"))
                .and_then(Json::as_str)
                .unwrap_or("");
            let want_key = want
                .and_then(|e| e.get("key"))
                .and_then(Json::as_str)
                .unwrap_or("");
            if let Err(e) = &compiled {
                ce_raised += 1;
                let got_msg = e.to_string();
                if got_msg == want_msg {
                    ce_msg_exact += 1;
                } else {
                    msg_mismatches.push((name.clone(), want_msg.to_string(), got_msg));
                }
                if !want_key.is_empty() {
                    ce_key_present += 1;
                    if e.key == want_key {
                        ce_key_exact += 1;
                    } else {
                        key_mismatches.push((name.clone(), want_key.to_string(), e.key.clone()));
                    }
                }
            }
            continue;
        }

        // Successful compile: measure eval-error text against `{ "error": ... }`.
        let Ok(expr) = compiled else { continue };
        let empty = Vec::new();
        let inputs = doc.get("inputs").and_then(Json::as_array).unwrap_or(&empty);
        let outputs = expected
            .get("outputs")
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default();
        let global_state: BTreeMap<String, Value> = doc
            .get("globalState")
            .and_then(Json::as_object)
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), Value::from_json(v)))
                    .collect()
            })
            .unwrap_or_default();
        for (i, input) in inputs.iter().enumerate() {
            let Some(want_msg) = outputs
                .get(i)
                .and_then(|o| o.get("error"))
                .and_then(Json::as_str)
            else {
                continue;
            };
            ee_total += 1;
            let Ok(ctx) = build_context(input) else {
                continue;
            };
            let ctx = ctx.with_global_state(global_state.clone());
            if let Err(e) = evaluate(&expr, &ctx) {
                ee_raised += 1;
                let got = e.to_string();
                if got == want_msg {
                    ee_msg_exact += 1;
                } else {
                    ee_mismatches.push((name.clone(), want_msg.to_string(), got));
                }
            }
        }
    }

    let pct = |n: usize, d: usize| {
        if d == 0 {
            100.0
        } else {
            100.0 * n as f64 / d as f64
        }
    };
    println!("\n=== Error-parity assessment ===\n");
    println!("Compile errors (Tier B = message, Tier C = key):");
    println!("  fixtures expecting a compile error : {ce_total}");
    println!(
        "  error raised by us                 : {ce_raised} ({:.1}%)",
        pct(ce_raised, ce_total)
    );
    println!(
        "  message byte-identical             : {ce_msg_exact} / {ce_total} ({:.1}%)",
        pct(ce_msg_exact, ce_total)
    );
    println!(
        "  location key matches               : {ce_key_exact} / {ce_key_present} non-empty keys ({:.1}%)",
        pct(ce_key_exact, ce_key_present)
    );
    println!("\nEval errors:");
    println!("  outputs expecting an error         : {ee_total}");
    println!(
        "  error raised by us                 : {ee_raised} ({:.1}%)",
        pct(ee_raised, ee_total)
    );
    println!(
        "  message byte-identical             : {ee_msg_exact} / {ee_total} ({:.1}%)",
        pct(ee_msg_exact, ee_total)
    );

    let show = |title: &str, v: &[(String, String, String)], limit: usize| {
        println!("\n--- {title} ({} total) ---", v.len());
        for (name, want, got) in v.iter().take(limit) {
            println!("  [{name}]\n    want: {want}\n    got : {got}");
        }
        if v.len() > limit {
            println!("  ... and {} more", v.len() - limit);
        }
    };
    show("compile message mismatches", &msg_mismatches, 60);
    show("location key mismatches", &key_mismatches, 40);
    show("eval message mismatches", &ee_mismatches, 40);
}

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("expression")
}

fn load_known_failures() -> HashSet<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("known_failures.txt");
    let Ok(contents) = fs::read_to_string(path) else {
        return HashSet::new();
    };
    contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect()
}

/// Recursively find every `test.json`, naming each by its path relative to the
/// fixtures root (e.g. `interpolate/linear`).
fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, out);
        } else if path.file_name().and_then(|n| n.to_str()) == Some("test.json") {
            let name = path
                .parent()
                .unwrap()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((name, path));
        }
    }
}

fn run_fixture(path: &Path) -> Result<(), Failed> {
    let raw = fs::read_to_string(path).map_err(|e| format!("cannot read fixture: {e}"))?;
    let doc: Json = serde_json::from_str(&raw).map_err(|e| format!("invalid fixture json: {e}"))?;

    let expression = doc
        .get("expression")
        .ok_or("fixture missing \"expression\"")?;
    let expected = doc.get("expected").ok_or("fixture missing \"expected\"")?;
    let compiled_result = expected
        .get("compiled")
        .and_then(|c| c.get("result"))
        .and_then(Json::as_str)
        .unwrap_or("success");

    // A legacy function object (`{type, property, stops, ...}`) is converted to
    // the equivalent modern expression before parsing.
    let converted;
    let expression = if expression.is_object() {
        match doc.get("propertySpec") {
            Some(spec) => {
                converted = convert_function(expression, spec);
                &converted
            }
            None => expression,
        }
    } else {
        expression
    };

    // A fixture "compiles" if it both parses and type-checks. The expected
    // type comes from the property spec, when present. Type checking returns the
    // annotated tree (with coercion/assertion nodes) that we then evaluate.
    let expected_type = doc.get("propertySpec").and_then(property_spec_type);
    let coerce_top_string = doc
        .get("propertySpec")
        .and_then(|s| s.get("type"))
        .and_then(Json::as_str)
        == Some("string");
    let compiled = parse(expression)
        .and_then(|expr| typecheck(&expr, expected_type.as_ref(), coerce_top_string));

    if compiled_result == "error" {
        return match compiled {
            // Also enforce message + location-key parity against the first
            // expected error (see the PARITY assessment mode).
            Err(e) => {
                let want = expected
                    .get("compiled")
                    .and_then(|c| c.get("errors"))
                    .and_then(Json::as_array)
                    .and_then(|a| a.first());
                if let Some(wmsg) = want.and_then(|w| w.get("error")).and_then(Json::as_str) {
                    if e.to_string() != wmsg {
                        return Err(format!(
                            "error message mismatch:\n  want: {wmsg}\n  got:  {e}"
                        )
                        .into());
                    }
                }
                if let Some(wkey) = want.and_then(|w| w.get("key")).and_then(Json::as_str) {
                    if e.key != wkey {
                        return Err(
                            format!("error key mismatch: want {wkey:?}, got {:?}", e.key).into(),
                        );
                    }
                }
                Ok(())
            }
            Ok(_) => {
                Err("expected a compile error, but the expression compiled successfully".into())
            }
        };
    }

    let expr = compiled.map_err(|e| format!("expected successful compile, but failed: {e}"))?;

    let empty = Vec::new();
    let inputs = doc.get("inputs").and_then(Json::as_array).unwrap_or(&empty);
    let outputs = expected
        .get("outputs")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();

    // `globalState` is a fixture-level map shared across all inputs.
    let global_state: BTreeMap<String, Value> = doc
        .get("globalState")
        .and_then(Json::as_object)
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), Value::from_json(v)))
                .collect()
        })
        .unwrap_or_default();

    for (i, input) in inputs.iter().enumerate() {
        let ctx = build_context(input)?.with_global_state(global_state.clone());
        let expected_output = outputs
            .get(i)
            .ok_or_else(|| format!("input #{i} has no expected output"))?;

        match evaluate(&expr, &ctx) {
            Ok(value) => {
                if let Some(err_obj) = expected_output.get("error") {
                    return Err(format!(
                        "input #{i}: expected evaluation error ({err_obj}), got value {:?}",
                        value
                    )
                    .into());
                }
                let actual = value_to_json(&value);
                if !json_close(&actual, expected_output) {
                    return Err(
                        format!("input #{i}: expected {expected_output}, got {actual}").into(),
                    );
                }
            }
            Err(e) => match expected_output.get("error").and_then(Json::as_str) {
                None => {
                    return Err(format!(
                        "input #{i}: expected {expected_output}, got evaluation error: {e}"
                    )
                    .into());
                }
                // Enforce evaluation-error message parity.
                Some(want) if e.to_string() != want => {
                    return Err(format!(
                        "input #{i}: error message mismatch:\n  want: {want}\n  got:  {e}"
                    )
                    .into());
                }
                Some(_) => {}
            },
        }
    }

    Ok(())
}

/// Build an [`EvaluationContext`] from a fixture input `[globals, feature]`.
fn build_context(input: &Json) -> Result<EvaluationContext, Failed> {
    let items = input
        .as_array()
        .ok_or("each input must be a [globals, feature] array")?;

    let mut ctx = EvaluationContext::new();
    if let Some(zoom) = items
        .first()
        .and_then(|g| g.get("zoom"))
        .and_then(Json::as_f64)
    {
        ctx.zoom = Some(zoom);
    }
    if let Some(images) = items
        .first()
        .and_then(|g| g.get("availableImages"))
        .and_then(Json::as_array)
    {
        ctx.available_images = images
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }
    if let Some(c) = items.first().and_then(|g| g.get("canonicalID")) {
        let n = |k| c.get(k).and_then(Json::as_u64).map(|v| v as u32);
        if let (Some(z), Some(x), Some(y)) = (n("z"), n("x"), n("y")) {
            ctx.canonical = Some((z, x, y));
        }
    }
    let global_num = |k| items.first().and_then(|g| g.get(k)).and_then(Json::as_f64);
    ctx.heatmap_density = global_num("heatmapDensity");
    ctx.elevation = global_num("elevation");
    ctx.line_progress = global_num("lineProgress");

    if let Some(feature_json) = items.get(1) {
        ctx.feature = build_feature(feature_json);
    }
    Ok(ctx)
}

fn build_feature(json: &Json) -> Feature {
    let mut feature = Feature::default();

    if let Some(props) = json.get("properties").and_then(Json::as_object) {
        feature.properties = props
            .iter()
            .map(|(k, v)| (k.clone(), Value::from_json(v)))
            .collect::<BTreeMap<_, _>>();
    }
    if let Some(id) = json.get("id") {
        if !id.is_null() {
            feature.id = Some(Value::from_json(id));
        }
    }
    if let Some(state) = json.get("featureState").and_then(Json::as_object) {
        feature.state = state
            .iter()
            .map(|(k, v)| (k.clone(), Value::from_json(v)))
            .collect::<BTreeMap<_, _>>();
    }
    feature.geometry_type = geometry_type(json);
    if let Some(geom) = json.get("geometry") {
        feature.geometry = extract_geometry(geom);
    }
    feature
}

fn geometry_type(json: &Json) -> Option<String> {
    // Normalize to the vector-tile geometry class (Point/LineString/Polygon).
    if let Some(t) = json
        .get("geometry")
        .and_then(|g| g.get("type"))
        .and_then(Json::as_str)
    {
        return Some(
            match t {
                "Point" | "MultiPoint" => "Point",
                "LineString" | "MultiLineString" => "LineString",
                "Polygon" | "MultiPolygon" => "Polygon",
                other => other,
            }
            .to_string(),
        );
    }
    match json.get("type") {
        Some(Json::String(s)) => Some(s.clone()),
        Some(Json::Number(n)) => match n.as_u64() {
            Some(1) => Some("Point".into()),
            Some(2) => Some("LineString".into()),
            Some(3) => Some("Polygon".into()),
            _ => None,
        },
        _ => None,
    }
}

/// Extract the feature geometry as raw `[lng, lat]` groups (rings / lines).
fn extract_geometry(geom: &Json) -> Vec<Vec<(f64, f64)>> {
    let pt = |c: &Json| -> Option<(f64, f64)> {
        let a = c.as_array()?;
        Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
    };
    let line = |c: &Json| -> Vec<(f64, f64)> {
        c.as_array()
            .map(|a| a.iter().filter_map(pt).collect())
            .unwrap_or_default()
    };
    let coords = geom.get("coordinates");
    match geom.get("type").and_then(Json::as_str) {
        Some("Point") => coords
            .and_then(pt)
            .map(|p| vec![vec![p]])
            .unwrap_or_default(),
        Some("MultiPoint") => coords
            .and_then(Json::as_array)
            .map(|a| a.iter().filter_map(pt).map(|p| vec![p]).collect())
            .unwrap_or_default(),
        Some("LineString") => coords.map(|c| vec![line(c)]).unwrap_or_default(),
        Some("MultiLineString") | Some("Polygon") => coords
            .and_then(Json::as_array)
            .map(|a| a.iter().map(line).collect())
            .unwrap_or_default(),
        Some("MultiPolygon") => coords
            .and_then(Json::as_array)
            .map(|polys| {
                polys
                    .iter()
                    .filter_map(Json::as_array)
                    .flatten()
                    .map(line)
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Serialize a [`Value`] to JSON using the same representation the spec
/// fixtures use for expected outputs.
fn value_to_json(value: &Value) -> Json {
    match value {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Number(n) => serde_json::json!(n),
        Value::String(s) => Json::String(s.clone()),
        // Colors are compared as normalized [r, g, b, a] arrays.
        Value::Color(c) => Json::Array(
            c.to_rgba_unit()
                .iter()
                .map(|n| serde_json::json!(n))
                .collect(),
        ),
        Value::Array(a) => Json::Array(a.iter().map(value_to_json).collect()),
        Value::Object(o) => Json::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect(),
        ),
        Value::Image { name, available } => {
            serde_json::json!({ "name": name, "available": available })
        }
        Value::NumberArray(v) => serde_json::json!({ "values": v }),
        Value::Padding(v) => serde_json::json!({ "values": v }),
        Value::ColorArray(v) => {
            let colors: Vec<Json> = v
                .iter()
                .map(|c| serde_json::json!({"r": c.r, "g": c.g, "b": c.b, "a": c.a}))
                .collect();
            serde_json::json!({ "values": colors })
        }
        Value::Projection(p) => match p {
            maplibre_expr::Projection::Named(s) => Json::String(s.clone()),
            maplibre_expr::Projection::Transition {
                from,
                to,
                transition,
            } => serde_json::json!({ "from": from, "to": to, "transition": transition }),
        },
        // A collator is never a direct output value.
        Value::Collator { .. } => Json::Null,
        Value::Formatted(sections) => {
            let secs: Vec<Json> = sections
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "text": s.text,
                        "image": s.image.as_ref().map(|(n, a)| serde_json::json!({"name": n, "available": a})),
                        "scale": s.scale,
                        "fontStack": s.font_stack,
                        "textColor": s.text_color.map(|c| serde_json::json!({"r": c.r, "g": c.g, "b": c.b, "a": c.a})),
                        "verticalAlign": s.vertical_align,
                    })
                })
                .collect();
            serde_json::json!({ "sections": secs })
        }
    }
}

/// Structural JSON equality that mirrors the upstream harness: numbers are
/// reduced to 6 significant decimal figures (see [`strip_precision`]) before
/// being compared, matching `deepEqual`/`stripPrecision` in the spec repo's
/// `test/lib/json-diff.ts`.
fn json_close(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Number(x), Json::Number(y)) => {
            let (x, y) = (
                x.as_f64().unwrap_or(f64::NAN),
                y.as_f64().unwrap_or(f64::NAN),
            );
            if x.is_nan() || y.is_nan() {
                return x.is_nan() && y.is_nan();
            }
            let (sx, sy) = (strip_precision(x, 6), strip_precision(y, 6));
            (sx - sy).abs() <= 1e-9 * sx.abs().max(1.0)
        }
        (Json::Array(x), Json::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| json_close(a, b))
        }
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| json_close(v, w)))
        }
        _ => a == b,
    }
}

/// Reduce `x` to `sig` significant decimal figures by truncation, matching
/// upstream `stripPrecision`. The double-floor guards against a value that is
/// already stripped drifting under floating-point rounding.
fn strip_precision(x: f64, sig: i32) -> f64 {
    if x == 0.0 {
        return 0.0;
    }
    let multiplier = 10f64.powf((sig as f64 - x.abs().log10().ceil()).max(0.0));
    let first = (x * multiplier).floor() / multiplier;
    (first * multiplier).floor() / multiplier
}
