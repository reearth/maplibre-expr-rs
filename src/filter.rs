//! Converting legacy MapLibre *filters* into modern expressions.
//!
//! Before expressions existed, layer filters were written as nested arrays with
//! a bare property name in the operand slot — `["==", "class", "primary"]`,
//! `["in", "type", "a", "b"]`, `["all", …]`. MapLibre still accepts them,
//! converting each to the equivalent boolean expression
//! (`["==", ["get", "class"], "primary"]`, …) before compiling. This module is
//! a port of maplibre-style-spec's `src/feature_filter/convert.ts` (the
//! conversion body) and the `isExpressionFilter` discriminator from
//! `src/feature_filter/index.ts`, so the produced expressions match the
//! reference implementation.
//!
//! Two entry points:
//! - [`is_expression_filter`] — is this filter already a modern expression (so
//!   it needs no conversion), or a legacy filter?
//! - [`convert_legacy_filter`] — convert a legacy filter to the equivalent
//!   modern expression, returned as raw JSON (ready for [`parse`](crate::parse)
//!   or to be embedded verbatim in a style). An input that already *is* an
//!   expression is returned unchanged.
//!
//! # Legacy comparison semantics
//!
//! Legacy comparisons are strictly typed with no implicit conversion: when a
//! property's runtime type differs from the compared value's type, the filter
//! simply yields `false`. The modern `==`/`<`/… operators instead type-check
//! (and may error or coerce), so a naive `["==", ["get", k], v]` would not
//! reproduce legacy behavior inside an `any`. Following `convert.ts`, each
//! `any` term is guarded with a preflight `typeof` check (see
//! [`convert_legacy_filter`] for the worked example) so a type mismatch
//! short-circuits to `false` instead of erroring out the whole filter.

use serde_json::{json, Value as Json};

/// A legacy filter that could not be converted to an expression.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterError {
    /// A property operand of a legacy comparison/`in`/`has` was not a string
    /// (legacy filters name the property with a bare string). `op` is the
    /// offending operator.
    PropertyNotString { op: String },
}

impl std::fmt::Display for FilterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FilterError::PropertyNotString { op } => write!(
                f,
                "legacy filter operator {op:?} expects a string property name"
            ),
        }
    }
}

impl std::error::Error for FilterError {}

/// Whether `filter` is already a modern expression filter (as opposed to a
/// legacy filter needing conversion). A direct port of `isExpressionFilter`.
///
/// The rule errs toward reporting *legacy* for the ambiguous shapes an old
/// style might carry (e.g. `["in", "color", "red"]`) so they are converted;
/// authors can force expression interpretation with a `["literal", …]` operand.
pub fn is_expression_filter(filter: &Json) -> bool {
    if filter.is_boolean() {
        return true;
    }
    let Some(arr) = filter.as_array() else {
        return false;
    };
    if arr.is_empty() {
        return false;
    }

    // A non-string operator slot (nested array, number, …) is not a legacy
    // operator, so the whole thing is treated as an expression (JS `default`).
    let op = arr[0].as_str();
    let is_string = |v: Option<&Json>| v.is_some_and(Json::is_string);
    let is_array = |v: Option<&Json>| v.is_some_and(Json::is_array);

    match op {
        Some("has") => arr.len() >= 2 && !matches!(arr[1].as_str(), Some("$id") | Some("$type")),

        Some("in") => arr.len() >= 3 && (!is_string(arr.get(1)) || is_array(arr.get(2))),

        Some("!in") | Some("!has") => false,

        Some("==") | Some("!=") | Some(">") | Some(">=") | Some("<") | Some("<=") => {
            arr.len() != 3 || is_array(arr.get(1)) || is_array(arr.get(2))
        }

        Some("none") => {
            // An expression only if some child is definitely an expression.
            for f in &arr[1..] {
                if f.is_boolean() {
                    continue;
                }
                if is_expression_filter(f) {
                    return true;
                }
            }
            false
        }

        Some("any") | Some("all") => {
            // An expression unless a child is definitely legacy.
            let mut has_legacy = false;
            for f in &arr[1..] {
                if f.is_boolean() {
                    continue;
                }
                if is_expression_filter(f) {
                    return true;
                }
                has_legacy = true;
            }
            !has_legacy
        }

        _ => true,
    }
}

/// Whether an expression-classified `all`/`any`/`none` combiner still hides a
/// legacy-only leaf that [`convert`] should rewrite.
///
/// `is_expression_filter` promotes a combiner to an expression as soon as *one*
/// child is a genuine expression, even when siblings are legacy-only (e.g. a
/// three-arg `["==", "prop", value]` or `["!has", …]`). Upstream MapLibre
/// rejects that mix; we instead convert the legacy leaves. Only combiners are
/// descended — a legacy shape nested inside some other expression operator is
/// not something MapLibre (or we) auto-convert.
fn has_convertible_legacy_leaf(filter: &Json) -> bool {
    let Some(arr) = filter.as_array() else {
        return false;
    };
    if !matches!(
        arr.first().and_then(Json::as_str),
        Some("all") | Some("any") | Some("none")
    ) {
        return false;
    }
    arr[1..].iter().any(|child| {
        child.is_array() && (!is_expression_filter(child) || has_convertible_legacy_leaf(child))
    })
}

/// Convert a legacy MapLibre filter to the equivalent modern expression.
///
/// Supported legacy operators: `==` `!=` `<` `<=` `>` `>=` `in` `!in` `has`
/// `!has` `all` `any` `none`. The special keys `"$type"` and `"$id"` map to
/// `["geometry-type"]` and `["id"]`. A filter that already
/// [`is_expression_filter`] is returned unchanged.
///
/// The result is raw JSON — a boolean expression ready for
/// [`parse`](crate::parse) or to embed directly in a style. Returns
/// [`FilterError`] only for structurally malformed legacy filters (a
/// non-string property name).
///
/// # Type-mismatch semantics
///
/// Legacy filters treat a property whose runtime type differs from the compared
/// value as simply not matching, rather than as an error. Inside `any`, that
/// matters: the reference converts
///
/// ```text
/// ["any", ["all", [">", "y", 0], [">", "z", 0]], [">", "x", 0]]
/// ```
///
/// by prefixing each disjunct with a `typeof` preflight, so a mistyped `y`/`z`
/// does not abort evaluation of the whole `any`:
///
/// ```text
/// ["any",
///   ["case",
///     ["all", ["==", ["typeof", ["get", "y"]], "number"],
///             ["==", ["typeof", ["get", "z"]], "number"]],
///     ["all", [">", ["get", "y"], 0], [">", ["get", "z"], 0]],
///     false],
///   ["case",
///     ["==", ["typeof", ["get", "x"]], "number"],
///     [">", ["get", "x"], 0],
///     false]]
/// ```
pub fn convert_legacy_filter(filter: &Json) -> Result<Json, FilterError> {
    let mut expected = ExpectedTypes::new();
    convert(filter, &mut expected)
}

/// Insertion-ordered map of property name -> expected `typeof` string, used to
/// build the preflight type checks for `any`. Mirrors JS object semantics:
/// assigning an existing key overwrites its value but keeps its position.
struct ExpectedTypes {
    entries: Vec<(String, &'static str)>,
}

impl ExpectedTypes {
    fn new() -> ExpectedTypes {
        ExpectedTypes {
            entries: Vec::new(),
        }
    }

    fn set(&mut self, property: &str, ty: &'static str) {
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| k == property) {
            slot.1 = ty;
        } else {
            self.entries.push((property.to_string(), ty));
        }
    }
}

fn convert(filter: &Json, expected: &mut ExpectedTypes) -> Result<Json, FilterError> {
    // A genuine expression passes through unchanged — *unless* it is an
    // `all`/`any`/`none` combiner that (per `is_expression_filter`) classifies
    // as an expression only because some child is one, yet still carries a
    // legacy-only leaf. Upstream MapLibre rejects such mixed filters; we instead
    // descend and convert the legacy leaves in place (the combiner arms below
    // recurse per child, so genuine expression children still pass through),
    // which lets real-world styles like Protomaps basemap render.
    if is_expression_filter(filter) && !has_convertible_legacy_leaf(filter) {
        return Ok(filter.clone());
    }
    // Falsy filters (`null`) mean "match everything".
    if filter.is_null() {
        return Ok(json!(true));
    }
    let Some(arr) = filter.as_array() else {
        // Not an array and not falsy: the reference falls through to `true`.
        return Ok(json!(true));
    };

    let op = arr[0].as_str();
    if arr.len() <= 1 {
        // `["all"]`/`["foo"]` -> true, `["any"]` -> false.
        return Ok(json!(op != Some("any")));
    }

    match op {
        Some(cmp @ ("==" | "!=" | "<" | ">" | "<=" | ">=")) => {
            convert_comparison_op(&arr[1], &arr[2], cmp, expected)
        }
        Some("any") => {
            let mut children = vec![json!("any")];
            for f in &arr[1..] {
                let mut types = ExpectedTypes::new();
                let child = convert(f, &mut types)?;
                let checks = runtime_type_checks(&types);
                if checks == json!(true) {
                    children.push(child);
                } else {
                    children.push(json!(["case", checks, child, false]));
                }
            }
            Ok(Json::Array(children))
        }
        Some("all") => {
            let mut children = Vec::with_capacity(arr.len() - 1);
            for f in &arr[1..] {
                children.push(convert(f, expected)?);
            }
            if children.len() > 1 {
                let mut out = vec![json!("all")];
                out.extend(children);
                Ok(Json::Array(out))
            } else {
                // `all` with a single child collapses to that child.
                Ok(children.into_iter().next().unwrap())
            }
        }
        Some("none") => {
            // none(…) == !any(…), evaluated with its own (discarded) type map.
            let mut any = vec![json!("any")];
            any.extend(arr[1..].iter().cloned());
            let mut types = ExpectedTypes::new();
            let inner = convert(&Json::Array(any), &mut types)?;
            Ok(json!(["!", inner]))
        }
        Some("in") => convert_in_op(&arr[1], &arr[2..], false),
        Some("!in") => convert_in_op(&arr[1], &arr[2..], true),
        Some("has") => convert_has_op(&arr[1]),
        Some("!has") => Ok(json!(["!", convert_has_op(&arr[1])?])),
        _ => Ok(json!(true)),
    }
}

fn runtime_type_checks(expected: &ExpectedTypes) -> Json {
    let mut conditions: Vec<Json> = Vec::new();
    for (property, ty) in &expected.entries {
        let get = if property == "$id" {
            json!(["id"])
        } else {
            json!(["get", property])
        };
        conditions.push(json!(["==", ["typeof", get], ty]));
    }
    match conditions.len() {
        0 => json!(true),
        1 => conditions.into_iter().next().unwrap(),
        _ => {
            let mut out = vec![json!("all")];
            out.extend(conditions);
            Json::Array(out)
        }
    }
}

fn convert_comparison_op(
    property: &Json,
    value: &Json,
    op: &str,
    expected: &mut ExpectedTypes,
) -> Result<Json, FilterError> {
    // `$type` compares the geometry type and takes no null special-casing.
    if property.as_str() == Some("$type") {
        return Ok(json!([op, ["geometry-type"], value]));
    }

    let is_id = property.as_str() == Some("$id");
    let get = if is_id {
        json!(["id"])
    } else {
        json!(["get", property_str(property, op)?])
    };

    // Record the expected type so an enclosing `any` can guard on it. `$id`
    // records under its own key (looked up as `["id"]` above).
    if !value.is_null() {
        let key = if is_id {
            "$id"
        } else {
            property_str(property, op)?
        };
        expected.set(key, js_typeof(value));
    }

    // A missing property is not `null` for legacy filters, so `== null` also
    // requires the property to be present (and `!= null` accepts its absence).
    if op == "==" && !is_id && value.is_null() {
        let p = property_str(property, op)?;
        return Ok(json!(["all", ["has", p], ["==", get, Json::Null]]));
    }
    if op == "!=" && !is_id && value.is_null() {
        let p = property_str(property, op)?;
        return Ok(json!(["any", ["!", ["has", p]], ["!=", get, Json::Null]]));
    }

    Ok(json!([op, get, value]))
}

fn convert_in_op(property: &Json, values: &[Json], negate: bool) -> Result<Json, FilterError> {
    if values.is_empty() {
        return Ok(json!(negate));
    }

    let op = if negate { "!in" } else { "in" };
    let get = match property.as_str() {
        Some("$type") => json!(["geometry-type"]),
        Some("$id") => json!(["id"]),
        _ => json!(["get", property_str(property, op)?]),
    };

    // A homogeneous list of strings or numbers can use one `match` rather than
    // a chain of `==`/`!=`.
    let type0 = js_typeof(&values[0]);
    let uniform = values.iter().all(|v| js_typeof(v) == type0);
    if uniform && (type0 == "string" || type0 == "number") {
        let unique = sort_and_dedupe(values);
        return Ok(json!(["match", get, unique, !negate, negate]));
    }

    let (combiner, cmp) = if negate { ("all", "!=") } else { ("any", "==") };
    let mut out = vec![json!(combiner)];
    for v in values {
        out.push(json!([cmp, get, v]));
    }
    Ok(Json::Array(out))
}

fn convert_has_op(property: &Json) -> Result<Json, FilterError> {
    match property.as_str() {
        Some("$type") => Ok(json!(true)),
        Some("$id") => Ok(json!(["!=", ["id"], Json::Null])),
        _ => Ok(json!(["has", property_str(property, "has")?])),
    }
}

/// The property operand as a string, or a [`FilterError`] — legacy filters
/// always name properties with a bare string.
fn property_str<'a>(property: &'a Json, op: &str) -> Result<&'a str, FilterError> {
    property
        .as_str()
        .ok_or_else(|| FilterError::PropertyNotString { op: op.to_string() })
}

/// The JS `typeof` of a JSON value, matching the strings the `typeof`
/// expression produces.
fn js_typeof(v: &Json) -> &'static str {
    match v {
        Json::Bool(_) => "boolean",
        Json::Number(_) => "number",
        Json::String(_) => "string",
        // JS: `typeof null`, `typeof []`, `typeof {}` are all "object".
        _ => "object",
    }
}

/// Sort like JS `Array.prototype.sort()` (lexicographic by string form) and
/// drop adjacent duplicates, so a `match` gets unique branch labels.
fn sort_and_dedupe(values: &[Json]) -> Vec<Json> {
    let mut sorted = values.to_vec();
    sorted.sort_by_cached_key(js_string);
    let mut unique: Vec<Json> = Vec::with_capacity(sorted.len());
    for v in sorted {
        if unique.last() != Some(&v) {
            unique.push(v);
        }
    }
    unique
}

/// The JS `String(v)` form used as the default sort key.
fn js_string(v: &Json) -> String {
    match v {
        Json::String(s) => s.clone(),
        Json::Number(n) => n.to_string(),
        Json::Bool(b) => b.to_string(),
        Json::Null => "null".to_string(),
        other => other.to_string(),
    }
}
