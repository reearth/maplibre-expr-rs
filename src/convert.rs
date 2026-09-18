//! Converting legacy MapLibre *function objects* into modern expressions.
//!
//! Before expressions existed, data- and zoom-driven styling was expressed with
//! *function objects* — `{ "type": "exponential", "property": "x", "stops":
//! [...] }` and friends. MapLibre still accepts them, converting each to the
//! equivalent modern expression (`interpolate` / `step` / `match` / `case` / …)
//! before parsing. This module is a port of maplibre-style-spec's
//! `src/function/convert.ts`, so the produced expressions match the reference
//! implementation — with one deliberate exception, a degenerate (constant)
//! step curve, where upstream has an off-by-one bug that changes what the
//! curve evaluates to; see `fixup_degenerate_step`.
//!
//! [`convert_function`] does the conversion given the function object and its
//! *property spec* (the style-spec entry for the property being styled). The
//! spec supplies information the object alone lacks — whether the property is
//! interpolatable (which picks `exponential` vs `interval` when `type` is
//! omitted), whether `{token}` strings expand to `["get", …]`, and the item
//! type for identity `array`/`enum`/`color` properties. MapLibre always has
//! that spec when it reads a style value, and so should you: use
//! [`parse_property`](crate::parse_property) or [`migrate`](crate::migrate::migrate)
//! to look it up from the embedded reference. Passing `&Value::Null` makes
//! the conversion rely on the object's own `type`/`base`/`default`/`stops`/
//! `property` fields alone, which can differ from MapLibre's reading (an
//! untyped numeric stop function comes out as `step`, tokens never expand).

use std::fmt;

use serde_json::{json, Value as Json};

/// A legacy function object that cannot be converted.
///
/// The reference implementation `throw`s in exactly two places — an unknown
/// property function `type` (`convert.ts:194-196`) and an unknown zoom function
/// `type` (`convert.ts:213-215`) — and this carries those messages verbatim.
/// Returned by [`try_convert_function`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertError {
    message: String,
}

impl ConvertError {
    fn new(message: String) -> Self {
        Self { message }
    }

    /// The reference implementation's error message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConvertError {}

/// Whether `value` carries the marks of a legacy function object — a JSON
/// object with `stops` (interval/exponential/categorical) or `property` (an
/// identity function).
///
/// This is a convenience predicate for callers who want to tell a function
/// object from some other bare object before handing it to [`parse`](crate::parse),
/// which rejects every object. It is *not* the test [`migrate`](crate::migrate::migrate)
/// applies: like upstream `migrate/expressions.ts:26` (`typeof value ===
/// 'object' && !Array.isArray(value)`), migration runs [`convert_function`] on
/// *every* non-array object it finds in a layout/paint property, `stops` and
/// `property` or not. An object with neither is treated as an identity function
/// and converts to `["get", null]`.
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
///
/// Where the reference implementation `throw`s — an unrecognised function
/// `type`, such as a `categorical` *zoom* function, which upstream rejects by
/// name — this returns `["error", "<the reference message>"]`, an expression
/// that parses but fails loudly, so a style typo cannot quietly change what is
/// drawn. Use [`try_convert_function`] to handle the failure yourself.
pub fn convert_function(params: &Json, spec: &Json) -> Json {
    match try_convert_function(params, spec) {
        Ok(expr) => expr,
        Err(e) => json!(["error", e.message()]),
    }
}

/// [`convert_function`], reporting the two failures the reference
/// implementation `throw`s on instead of encoding them as `["error", …]`.
///
/// Upstream raises `Unknown property function type ${type}`
/// (`convert.ts:194-196`) and `Unknown zoom function type "${type}"`
/// (`convert.ts:213-215`) for a `type` that is neither `exponential`,
/// `interval`, nor (for property functions) `categorical` — a `categorical`
/// zoom function included.
pub fn try_convert_function(params: &Json, spec: &Json) -> Result<Json, ConvertError> {
    let Some(raw_stops) = params.get("stops").and_then(Json::as_array) else {
        return Ok(convert_identity(params, spec));
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

fn unknown_property_type(ty: &str) -> ConvertError {
    ConvertError::new(format!("Unknown property function type {ty}"))
}

fn unknown_zoom_type(ty: &str) -> ConvertError {
    ConvertError::new(format!("Unknown zoom function type \"{ty}\""))
}

fn convert_literal(v: &Json) -> Json {
    // Upstream (`convert.ts:3-5`) is `typeof value === 'object' ? ['literal',
    // value] : value`. In JavaScript `typeof null` is also `"object"`, so a
    // `null` stop output is wrapped too — `convertLiteral(null)` returns
    // `["literal", null]`. Mirror that; see `crate::filter`'s `js_typeof` for
    // the same JS quirk.
    if v.is_object() || v.is_array() || v.is_null() {
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
        // Upstream `convert.ts:128-141`: "Some fields with type: resolvedImage
        // have an undefined default. Because undefined is an invalid value for
        // resolvedImage, set fallback to an empty string instead of undefined
        // to ensure output passes validation." `icon-image` is exactly such a
        // field, and upstream's own `migrate.test.ts:92-124` asserts the `""`.
        None if spec.get("type").and_then(Json::as_str) == Some("resolvedImage") => json!(""),
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

/// Add a noop stop to a degenerate (constant) step curve, so that
/// `["step", input, out]` becomes a well-formed `["step", input, out, 0, out]`.
///
/// **Deliberate divergence from the reference implementation.** Upstream
/// (`convert.ts:226-232`) is:
///
/// ```js
/// if (expression[0] === 'step' && expression.length === 3) {
///     expression.push(0);
///     expression.push(expression[3]);
/// }
/// ```
///
/// The first `push` writes `0` *into index 3* (the array had length 3), so the
/// second `push` reads back that freshly written `0` rather than the curve's
/// output at index 2. Upstream therefore produces `["step", input, out, 0, 0]`
/// — verified against the pinned commit: `fixupDegenerateStepCurve(["step",
/// ["zoom"], 3])` yields `["step", ["zoom"], 3, 0, 0]`, a curve that evaluates
/// to `0` for every input `>= 0` instead of to the constant it came from.
///
/// That is an off-by-one bug, not a semantic the port should reproduce: this
/// crate captures `curve[2]` *before* pushing and emits `["step", input, out,
/// 0, out]`, which preserves the constant function the legacy object meant.
/// Do not "fix" this to match upstream — the difference is intentional and is
/// pinned by a regression test in `tests/legacy.rs`.
fn fixup_degenerate_step(curve: &mut Vec<Json>) {
    if curve.first().and_then(Json::as_str) == Some("step") && curve.len() == 3 {
        let out = curve[2].clone();
        curve.push(json!(0));
        curve.push(out);
    }
}

/// Convert a legacy `{token}` string — `"{name} ({ref})"` — to the equivalent
/// expression: `["get", token]` for each token, concatenated with the literal
/// spans between them (`["to-string", ["get", …]]` for a lone token, the input
/// unchanged when it has no tokens). Only properties whose spec has `tokens:
/// true` (`text-field`, `icon-image`) interpret strings this way; see
/// [`migrate`](crate::migrate::migrate).
pub fn convert_token_string(s: &str) -> Json {
    // Replace `{tokens}` with `["get", token]`, concatenating literal spans.
    //
    // The token grammar is upstream's `/{([^{}]+)}/g` (`convert.ts:258`): the
    // token body may contain neither brace and must be non-empty, so `"{}"` has
    // no token at all and `"{a{b}"` matches only the inner `{b}` (the leading
    // `{a` stays literal). Scanning left to right and skipping a `{` that fails
    // to start a token reproduces the regex exactly.
    //
    // Byte indexing is safe: `{` and `}` are ASCII and, in UTF-8, never occur
    // inside a multi-byte sequence, so every index we slice at is a char
    // boundary.
    let mut result: Vec<Json> = vec![json!("concat")];
    let bytes = s.as_bytes();
    let mut pos = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            let mut close = i + 1;
            while close < bytes.len() && bytes[close] != b'{' && bytes[close] != b'}' {
                close += 1;
            }
            // A token needs a closing brace and a non-empty body.
            if close < bytes.len() && bytes[close] == b'}' && close > i + 1 {
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
        // The full reference lists enum `values` as an object; the pruned
        // reference embedded for `migrate` stores the keys as an array (in
        // source order, which `serde_json`'s sorted maps would otherwise lose).
        let keys: Vec<Json> = match spec.get("values") {
            Some(Json::Object(o)) => o.keys().map(|k| json!(k)).collect(),
            Some(Json::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
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

fn convert_property(
    params: &Json,
    spec: &Json,
    stops: &[(Json, Json)],
) -> Result<Json, ConvertError> {
    let ty = function_type(params, spec);
    let get = json!(["get", params.get("property")]);
    Ok(match ty.as_str() {
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
        "exponential" => {
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
        // Upstream `convert.ts:194-196` throws here rather than falling back to
        // interpolation, so an unrecognised `type` cannot silently restyle.
        other => return Err(unknown_property_type(other)),
    })
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

fn convert_zoom(
    params: &Json,
    spec: &Json,
    stops: &[(Json, Json)],
    input: Json,
) -> Result<Json, ConvertError> {
    let ty = function_type(params, spec);
    let (mut expr, is_step) = if ty == "interval" {
        (vec![json!("step"), input], true)
    } else if ty == "exponential" {
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
    } else {
        // Upstream `convert.ts:213-215` throws. Note this rejects `categorical`
        // by name: a categorical *zoom* function has no expression equivalent.
        return Err(unknown_zoom_type(&ty));
    };
    for (i, o) in stops {
        append_stop_pair(&mut expr, i.clone(), o.clone(), is_step);
    }
    fixup_degenerate_step(&mut expr);
    Ok(Json::Array(expr))
}

fn convert_zoom_and_property(
    params: &Json,
    spec: &Json,
    stops: &[(Json, Json)],
) -> Result<Json, ConvertError> {
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
            let output = convert_property(&feature_params(*z), spec, &grouped[i])?;
            append_stop_pair(&mut expr, json!(z), output, false);
        }
        Ok(Json::Array(expr))
    } else {
        let mut expr = vec![json!("step"), json!(["zoom"])];
        for (i, z) in zooms.iter().enumerate() {
            let output = convert_property(&feature_params(*z), spec, &grouped[i])?;
            append_stop_pair(&mut expr, json!(z), output, true);
        }
        fixup_degenerate_step(&mut expr);
        Ok(Json::Array(expr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `line-width` as the embedded reference has it: an interpolatable number.
    fn line_width() -> Json {
        json!({ "type": "number", "default": 1, "expression": { "interpolated": true } })
    }

    // --- B5: an unknown function `type` must not silently interpolate -------

    #[test]
    fn unknown_zoom_function_type_is_an_error_not_an_interpolation() {
        // Upstream `convert.ts:213-215` throws `Unknown zoom function type
        // "<type>"`. Verified against maplibre-style-spec@ef522e45:
        //   convertFunction({type:'bogus', stops:[[0,1],[1,2]]}, line-width)
        //     -> throws 'Unknown zoom function type "bogus"'
        let params = json!({ "type": "bogus", "stops": [[0, 1], [1, 2]] });
        let err = try_convert_function(&params, &line_width()).unwrap_err();
        assert_eq!(err.message(), "Unknown zoom function type \"bogus\"");
        assert_eq!(
            convert_function(&params, &line_width()),
            json!(["error", "Unknown zoom function type \"bogus\""])
        );
    }

    #[test]
    fn categorical_zoom_function_is_rejected_by_name() {
        // A categorical *zoom* function has no expression equivalent, and
        // upstream rejects it through the same throw. This crate used to turn
        // it into ["interpolate", ["linear"], ["zoom"], 0, 1, 1, 2].
        let params = json!({ "type": "categorical", "stops": [[0, 1], [1, 2]] });
        let err = try_convert_function(&params, &line_width()).unwrap_err();
        assert_eq!(err.message(), "Unknown zoom function type \"categorical\"");
    }

    #[test]
    fn unknown_property_function_type_is_an_error() {
        // Upstream `convert.ts:194-196`: `Unknown property function type bogus`
        // — no quotes around the type, unlike the zoom message.
        let params = json!({ "type": "bogus", "property": "x", "stops": [[0, 1], [1, 2]] });
        let err = try_convert_function(&params, &line_width()).unwrap_err();
        assert_eq!(err.message(), "Unknown property function type bogus");
    }

    #[test]
    fn known_function_types_still_convert() {
        // The error arm must not shadow the three real types.
        assert_eq!(
            convert_function(
                &json!({ "type": "exponential", "stops": [[0, 1], [1, 2]] }),
                &line_width()
            ),
            json!(["interpolate", ["linear"], ["zoom"], 0, 1, 1, 2])
        );
        assert_eq!(
            convert_function(
                &json!({ "type": "interval", "stops": [[0, 1], [1, 2]] }),
                &line_width()
            ),
            json!(["step", ["zoom"], 1, 1, 2])
        );
        assert_eq!(
            convert_function(
                &json!({ "type": "categorical", "property": "x", "stops": [["a", 1]] }),
                &line_width()
            ),
            json!(["match", ["get", "x"], "a", 1, 1])
        );
    }

    // --- B7: resolvedImage gets a "" fallback, not null --------------------

    #[test]
    fn resolved_image_categorical_function_falls_back_to_empty_string() {
        // Ported verbatim from upstream `src/migrate.test.ts:92-124`
        // ("converts categorical function on resolvedImage type to valid
        // expression"), whose assertion is the trailing `''`.
        let style = json!({
            "version": 8,
            "sources": {
                "maplibre": {
                    "url": "https://demotiles.maplibre.org/tiles/tiles.json",
                    "type": "vector"
                }
            },
            "layers": [{
                "id": "1",
                "source": "maplibre",
                "source-layer": "labels",
                "type": "symbol",
                "layout": {
                    "icon-image": {
                        "base": 1,
                        "type": "categorical",
                        "property": "type",
                        "stops": [["park", "some-icon"]]
                    }
                }
            }]
        });
        let migrated = crate::migrate::migrate(&style).unwrap();
        assert_eq!(
            migrated["layers"][0]["layout"]["icon-image"],
            json!(["match", ["get", "type"], "park", "some-icon", ""])
        );
    }

    #[test]
    fn non_resolved_image_fallback_stays_null() {
        // Only `resolvedImage` gets the `""`; everything else keeps upstream's
        // `undefined`, which serialises as JSON null.
        let spec = json!({ "type": "string", "expression": { "interpolated": false } });
        assert_eq!(
            convert_function(
                &json!({ "type": "categorical", "property": "x", "stops": [["a", "b"]] }),
                &spec
            ),
            json!(["match", ["get", "x"], "a", "b", null])
        );
    }

    // --- C8: deliberate divergence on degenerate step curves ---------------

    #[test]
    fn degenerate_step_curve_keeps_its_constant() {
        // DELIBERATE DIVERGENCE — do not "fix" this to match upstream.
        // Upstream (`convert.ts:226-232`) emits ["step", ["zoom"], 3, 0, 0],
        // because its first push overwrites index 3 before the second push
        // reads it; that curve evaluates to 0 for every input >= 0. See the
        // doc comment on `fixup_degenerate_step`.
        assert_eq!(
            convert_function(
                &json!({ "type": "interval", "stops": [[0, 3]] }),
                &line_width()
            ),
            json!(["step", ["zoom"], 3, 0, 3])
        );
    }

    // --- C9: JS `typeof null === "object"` ---------------------------------

    #[test]
    fn null_stop_output_is_wrapped_in_literal() {
        // Verified against upstream: convertFunction({stops:[[0,null],[1,2]]},
        // line-width) -> ["interpolate",["linear"],["zoom"],0,["literal",null],1,2]
        assert_eq!(
            convert_function(&json!({ "stops": [[0, null], [1, 2]] }), &line_width()),
            json!([
                "interpolate",
                ["linear"],
                ["zoom"],
                0,
                ["literal", null],
                1,
                2
            ])
        );
    }

    #[test]
    fn null_identity_default_is_wrapped_in_literal() {
        let spec = json!({ "type": "number" });
        assert_eq!(
            convert_function(&json!({ "property": "x", "default": null }), &spec),
            json!(["number", ["get", "x"], ["literal", null]])
        );
    }

    // --- C10: token grammar is /{([^{}]+)}/g -------------------------------

    #[test]
    fn token_grammar_matches_the_reference_regex() {
        // Every expectation below was printed by a verbatim node copy of
        // upstream's `convertTokenString` at maplibre-style-spec@ef522e45.
        let cases: &[(&str, Json)] = &[
            // Braces may not appear in a token body: only `{b}` matches.
            ("{a{b}", json!(["concat", "{a", ["get", "b"]])),
            // An empty token body is not a token at all.
            ("{}", json!("{}")),
            ("{name}", json!(["to-string", ["get", "name"]])),
            (
                "{name} ({ref})",
                json!(["concat", ["get", "name"], " (", ["get", "ref"], ")"]),
            ),
            ("{ a }", json!(["to-string", ["get", " a "]])),
            ("{a}{b}", json!(["concat", ["get", "a"], ["get", "b"]])),
            ("pre{a}post", json!(["concat", "pre", ["get", "a"], "post"])),
            ("no tokens", json!("no tokens")),
            ("{", json!("{")),
            ("}", json!("}")),
            ("a{b", json!("a{b")),
            ("あ{a}", json!(["concat", "あ", ["get", "a"]])),
            ("{a}{", json!(["concat", ["get", "a"], "{"])),
            ("{{a}}", json!(["concat", "{", ["get", "a"], "}"])),
        ];
        for (input, expected) in cases {
            assert_eq!(&convert_token_string(input), expected, "input {input:?}");
        }
    }

    // --- D7: `is_function` is a caller-facing predicate, not migrate's test -

    #[test]
    fn is_function_is_narrower_than_what_migrate_converts() {
        assert!(is_function(&json!({ "stops": [[0, 1]] })));
        assert!(is_function(&json!({ "property": "x" })));
        assert!(!is_function(&json!({ "foo": "bar" })));
        // ...yet migrate, like upstream `migrate/expressions.ts:26`, converts
        // every non-array object, so an object `is_function` rejects still goes
        // through `convert_function` — as an identity function.
        assert_eq!(
            convert_function(&json!({ "foo": "bar" }), &json!({ "type": "number" })),
            json!(["get", null])
        );
    }
}
