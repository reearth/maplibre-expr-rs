//! Turning raw JSON (`serde_json::Value`) into an [`Expr`] tree.

use serde_json::Value as Json;

use crate::ast::{Expr, FormatArg, InterpKind, InterpSpace};
use crate::distance::SimpleGeom;
use crate::error::ParseError;
use crate::value::Value;

/// Valid `vertical-align` option values for the `format` operator.
const VERTICAL_ALIGN: [&str; 3] = ["bottom", "center", "top"];

type Result<T> = std::result::Result<T, ParseError>;

/// Parse a MapLibre expression from JSON.
pub fn parse(json: &Json) -> Result<Expr> {
    match json {
        Json::Array(items) => parse_array(items),
        Json::Object(_) => Err(ParseError::new(
            "Expected an array, but found an object instead.",
        )),
        _ => Ok(Expr::Literal(Value::from_json(json))),
    }
}

fn parse_array(items: &[Json]) -> Result<Expr> {
    let first = items
        .first()
        .ok_or_else(|| ParseError::new("Expected an array with at least one element."))?;
    let op = first.as_str().ok_or_else(|| {
        ParseError::new("Expression name must be a string, but found a non-string instead.")
    })?;
    let args = &items[1..];

    match op {
        "literal" => {
            expect_arity(op, args, 1)?;
            Ok(Expr::Literal(Value::from_json(&args[0])))
        }
        "let" => parse_let(args),
        "var" => {
            expect_arity(op, args, 1)?;
            let name = args[0]
                .as_str()
                .ok_or_else(|| ParseError::new("'var' requires a string binding name."))?;
            Ok(Expr::Var(name.to_string()))
        }
        "match" => parse_match(args),
        "step" => parse_step(args),
        "interpolate" => parse_interpolate(InterpSpace::Rgb, args),
        "interpolate-hcl" => parse_interpolate(InterpSpace::Hcl, args),
        "interpolate-lab" => parse_interpolate(InterpSpace::Lab, args),
        "format" => parse_format(args),
        "number-format" => parse_number_format(args),
        "within" => parse_within(args),
        "distance" => parse_distance(args),
        "array" => {
            check_generic_arity(op, args.len())?;
            validate_array_type_args(args)?;
            let parsed = args.iter().map(parse).collect::<Result<Vec<_>>>()?;
            Ok(Expr::Call {
                op: op.to_string(),
                args: parsed,
            })
        }
        _ => {
            check_generic_arity(op, args.len())?;
            let args = args.iter().map(parse).collect::<Result<Vec<_>>>()?;
            Ok(Expr::Call {
                op: op.to_string(),
                args,
            })
        }
    }
}

/// Reject unknown operators and calls with the wrong number of arguments at
/// parse time — these are `"result": "error"` cases in the spec fixtures.
///
/// Operators that MapLibre defines but this crate does not yet evaluate are
/// still accepted here (so their arguments parse); evaluation reports them as
/// unimplemented. Only genuinely unknown names are rejected.
fn check_generic_arity(op: &str, argc: usize) -> Result<()> {
    // `case` has an irregular (odd, >= 3) shape.
    if op == "case" {
        if argc < 3 || argc.is_multiple_of(2) {
            return Err(ParseError::new(
                "Expected an odd number of arguments (>= 3) to 'case'.",
            ));
        }
        return Ok(());
    }

    let range =
        arity(op).ok_or_else(|| ParseError::new(format!("Unknown expression name \"{op}\".")))?;
    let (min, max) = range;
    if argc < min || max.is_some_and(|m| argc > m) {
        return Err(ParseError::new(format!(
            "Wrong number of arguments to '{op}': expected {}, found {argc}.",
            match max {
                Some(m) if m == min => format!("{min}"),
                Some(m) => format!("{min}..={m}"),
                None => format!("at least {min}"),
            }
        )));
    }
    Ok(())
}

/// `(min, max)` argument counts for each known operator; `None` max means
/// variadic. Operators absent from this table are unknown names.
fn arity(op: &str) -> Option<(usize, Option<usize>)> {
    Some(match op {
        // lookups
        "get" => (1, Some(2)),
        "has" => (1, Some(2)),
        "properties"
        | "id"
        | "geometry-type"
        | "zoom"
        | "heatmap-density"
        | "line-progress"
        | "accumulated"
        | "e"
        | "pi"
        | "ln2"
        | "resolved-locale"
        | "raster-value"
        | "sky-radial-progress"
        | "measure-light"
        | "elevation" => (0, Some(0)),
        "at" => (2, Some(2)),
        "in" => (2, Some(2)),
        "index-of" => (2, Some(3)),
        "slice" => (2, Some(3)),
        "length" => (1, Some(1)),
        "feature-state" | "config" => (1, Some(2)),
        "global-state" => (1, Some(1)),

        // decision / boolean
        "!" => (1, Some(1)),
        "all" | "any" | "coalesce" => (0, None),
        "error" => (1, Some(1)),
        "==" | "!=" | "<" | ">" | "<=" | ">=" => (2, Some(3)),

        // arithmetic — +/*/min/max accept zero args (identity element)
        "+" | "*" | "min" | "max" => (0, None),
        "-" => (1, Some(2)),
        "/" | "%" | "^" => (2, Some(2)),
        "abs" | "acos" | "asin" | "atan" | "ceil" | "cos" | "floor" | "ln" | "log10" | "log2"
        | "round" | "sin" | "sqrt" | "tan" => (1, Some(1)),
        "distance" => (1, Some(1)),

        // strings
        "concat" => (0, None),
        "upcase" | "downcase" => (1, Some(1)),
        "join" => (2, Some(2)),
        "split" => (2, Some(2)),
        "is-supported-script" => (1, Some(1)),
        "collator" => (1, Some(1)),
        "number-format" => (2, Some(2)),
        "format" | "image" => (1, None),

        // type assertions & conversions ("array" takes an optional item type
        // and length prefix, then one or more fallback value candidates)
        "array" => (1, None),
        "boolean" | "number" | "string" | "object" | "to-number" | "to-color" => (1, None),
        "to-boolean" | "to-string" | "to-rgba" | "typeof" => (1, Some(1)),

        // color constructors
        "rgb" => (3, Some(3)),
        "rgba" => (4, Some(4)),

        // geometry predicates
        "within" => (1, Some(1)),

        _ => return None,
    })
}

fn parse_let(args: &[Json]) -> Result<Expr> {
    if args.is_empty() || args.len().is_multiple_of(2) {
        return Err(ParseError::new(
            "Expected an odd number of arguments to 'let'.",
        ));
    }
    let mut bindings = Vec::new();
    let mut i = 0;
    while i + 1 < args.len() {
        let name = args[i]
            .as_str()
            .ok_or_else(|| ParseError::new("'let' binding names must be strings."))?;
        bindings.push((name.to_string(), parse(&args[i + 1])?));
        i += 2;
    }
    let body = parse(&args[args.len() - 1])?;
    Ok(Expr::Let {
        bindings,
        body: Box::new(body),
    })
}

fn parse_match(args: &[Json]) -> Result<Expr> {
    // args = input, (label, output)+, default  =>  even count, >= 4.
    if args.len() < 4 || !args.len().is_multiple_of(2) {
        return Err(ParseError::new(
            "Expected an even number of arguments (>= 4) to 'match'.",
        ));
    }
    let input = parse(&args[0])?;
    let mut arms = Vec::new();
    let mut i = 1;
    while i + 1 < args.len() {
        let labels = parse_match_labels(&args[i])?;
        let output = parse(&args[i + 1])?;
        arms.push((labels, output));
        i += 2;
    }
    let default = parse(&args[args.len() - 1])?;
    Ok(Expr::Match {
        input: Box::new(input),
        arms,
        default: Box::new(default),
    })
}

/// `match` labels are unquoted literals: a single value, or an array of values.
fn parse_match_labels(json: &Json) -> Result<Vec<Value>> {
    match json {
        Json::Array(items) => Ok(items.iter().map(Value::from_json).collect()),
        Json::Number(_) | Json::String(_) => Ok(vec![Value::from_json(json)]),
        _ => Err(ParseError::new(
            "Match labels must be numbers, strings, or arrays thereof.",
        )),
    }
}

fn parse_step(args: &[Json]) -> Result<Expr> {
    if args.len() < 3 || args.len() % 2 == 1 {
        return Err(ParseError::new(
            "Expected an even number of arguments (>= 4) to 'step'.",
        ));
    }
    let input = parse(&args[0])?;
    let output0 = parse(&args[1])?;
    let mut stops = Vec::new();
    let mut i = 2;
    while i + 1 < args.len() {
        let stop = args[i]
            .as_f64()
            .ok_or_else(|| ParseError::new("Step stop inputs must be numbers."))?;
        stops.push((stop, parse(&args[i + 1])?));
        i += 2;
    }
    check_ascending(&stops)?;
    Ok(Expr::Step {
        input: Box::new(input),
        output0: Box::new(output0),
        stops,
    })
}

fn parse_interpolate(space: InterpSpace, args: &[Json]) -> Result<Expr> {
    if args.len() < 4 || args.len() % 2 == 1 {
        return Err(ParseError::new(
            "Expected an even number of arguments (>= 4) to 'interpolate'.",
        ));
    }
    let kind = parse_interp_kind(&args[0])?;
    let input = parse(&args[1])?;
    let mut stops = Vec::new();
    let mut i = 2;
    while i + 1 < args.len() {
        let stop = args[i]
            .as_f64()
            .ok_or_else(|| ParseError::new("Interpolation stop inputs must be numbers."))?;
        stops.push((stop, parse(&args[i + 1])?));
        i += 2;
    }
    check_ascending(&stops)?;
    Ok(Expr::Interpolate {
        kind,
        space,
        input: Box::new(input),
        stops,
        projection: false,
    })
}

/// Parse `["within", geojson]`, extracting polygon rings (as `[lng, lat]`)
/// from a Polygon, MultiPolygon, Feature, or FeatureCollection.
fn parse_number_format(args: &[Json]) -> Result<Expr> {
    if args.len() != 2 {
        return Err(ParseError::new(
            "Expected two arguments to 'number-format'.",
        ));
    }
    let value = Box::new(parse(&args[0])?);
    let opts = args[1]
        .as_object()
        .ok_or_else(|| ParseError::new("'number-format' options must be an object."))?;
    if opts.contains_key("currency") && opts.contains_key("unit") {
        return Err(ParseError::new(
            "Cannot use both 'currency' and 'unit' in 'number-format'.",
        ));
    }
    let opt = |key: &str| -> Result<Option<Box<Expr>>> {
        match opts.get(key) {
            Some(v) => Ok(Some(Box::new(parse(v)?))),
            None => Ok(None),
        }
    };
    Ok(Expr::NumberFormat {
        value,
        locale: opt("locale")?,
        currency: opt("currency")?,
        min_fraction_digits: opt("min-fraction-digits")?,
        max_fraction_digits: opt("max-fraction-digits")?,
        unit: opt("unit")?,
    })
}

fn parse_within(args: &[Json]) -> Result<Expr> {
    let err = || {
        ParseError::new(
            "'within' expression requires valid geojson object that contains polygon geometry type.",
        )
    };
    if args.len() != 1 {
        return Err(ParseError::new(
            "'within' expression requires exactly one argument.",
        ));
    }
    let geojson = &args[0];
    let mut polygons: Vec<Vec<Vec<(f64, f64)>>> = Vec::new();
    let mut add_geometry =
        |ty: Option<&str>, coords: Option<&Json>| match (ty, coords.and_then(Json::as_array)) {
            (Some("Polygon"), Some(c)) => {
                if let Some(p) = parse_polygon(c) {
                    polygons.push(p);
                }
            }
            (Some("MultiPolygon"), Some(c)) => {
                for poly in c.iter().filter_map(Json::as_array) {
                    if let Some(p) = parse_polygon(poly) {
                        polygons.push(p);
                    }
                }
            }
            _ => {}
        };
    match geojson.get("type").and_then(Json::as_str) {
        Some("FeatureCollection") => {
            for feat in geojson
                .get("features")
                .and_then(Json::as_array)
                .into_iter()
                .flatten()
            {
                let g = feat.get("geometry");
                add_geometry(
                    g.and_then(|g| g.get("type")).and_then(Json::as_str),
                    g.and_then(|g| g.get("coordinates")),
                );
            }
        }
        Some("Feature") => {
            let g = geojson.get("geometry");
            add_geometry(
                g.and_then(|g| g.get("type")).and_then(Json::as_str),
                g.and_then(|g| g.get("coordinates")),
            );
        }
        Some(t @ ("Polygon" | "MultiPolygon")) => {
            add_geometry(Some(t), geojson.get("coordinates"));
        }
        _ => {}
    }
    if polygons.is_empty() {
        return Err(err());
    }
    Ok(Expr::Within(polygons))
}

/// Parse `["distance", geojson]`, extracting the argument geometries (splitting
/// any `Multi*` into simple Point/LineString/Polygon geometries).
fn parse_distance(args: &[Json]) -> Result<Expr> {
    let err = || {
        ParseError::new(
            "'distance' expression requires valid geojson object that contains geometry.",
        )
    };
    if args.len() != 1 {
        return Err(ParseError::new(
            "'distance' expression requires exactly one argument.",
        ));
    }
    let mut geoms: Vec<SimpleGeom> = Vec::new();
    match args[0].get("type").and_then(Json::as_str) {
        Some("FeatureCollection") => {
            for feat in args[0]
                .get("features")
                .and_then(Json::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(g) = feat.get("geometry") {
                    add_simple_geometry(g, &mut geoms);
                }
            }
        }
        Some("Feature") => {
            if let Some(g) = args[0].get("geometry") {
                add_simple_geometry(g, &mut geoms);
            }
        }
        Some(_) => add_simple_geometry(&args[0], &mut geoms),
        None => {}
    }
    if geoms.is_empty() {
        return Err(err());
    }
    Ok(Expr::Distance(geoms))
}

fn parse_point(c: &Json) -> Option<(f64, f64)> {
    let a = c.as_array()?;
    Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
}

fn parse_line(c: &Json) -> Vec<(f64, f64)> {
    c.as_array()
        .map(|a| a.iter().filter_map(parse_point).collect())
        .unwrap_or_default()
}

/// Append the simple geometries of a GeoJSON geometry (splitting `Multi*`).
fn add_simple_geometry(geom: &Json, out: &mut Vec<SimpleGeom>) {
    let coords = geom.get("coordinates");
    match geom.get("type").and_then(Json::as_str) {
        Some("Point") => {
            if let Some(p) = coords.and_then(parse_point) {
                out.push(SimpleGeom::Point(p));
            }
        }
        Some("MultiPoint") => {
            for p in coords.and_then(Json::as_array).into_iter().flatten() {
                if let Some(p) = parse_point(p) {
                    out.push(SimpleGeom::Point(p));
                }
            }
        }
        Some("LineString") => {
            if let Some(c) = coords {
                out.push(SimpleGeom::Line(parse_line(c)));
            }
        }
        Some("MultiLineString") => {
            for l in coords.and_then(Json::as_array).into_iter().flatten() {
                out.push(SimpleGeom::Line(parse_line(l)));
            }
        }
        Some("Polygon") => {
            if let Some(c) = coords.and_then(Json::as_array) {
                if let Some(p) = parse_polygon(c) {
                    out.push(SimpleGeom::Polygon(p));
                }
            }
        }
        Some("MultiPolygon") => {
            for poly in coords.and_then(Json::as_array).into_iter().flatten() {
                if let Some(p) = poly.as_array().and_then(|r| parse_polygon(r)) {
                    out.push(SimpleGeom::Polygon(p));
                }
            }
        }
        _ => {}
    }
}

/// Parse a GeoJSON polygon (array of rings of `[lng, lat]`).
fn parse_polygon(rings: &[Json]) -> Option<Vec<Vec<(f64, f64)>>> {
    let mut out = Vec::new();
    for ring in rings.iter().filter_map(Json::as_array) {
        let mut r = Vec::new();
        for pt in ring.iter().filter_map(Json::as_array) {
            let lng = pt.first().and_then(Json::as_f64)?;
            let lat = pt.get(1).and_then(Json::as_f64)?;
            r.push((lng, lat));
        }
        out.push(r);
    }
    Some(out)
}

fn parse_format(args: &[Json]) -> Result<Expr> {
    if args.is_empty() {
        return Err(ParseError::new(
            "Expected at least one argument to 'format'.",
        ));
    }
    if args[0].is_object() {
        return Err(ParseError::new(
            "First argument to 'format' must be an image or text section.",
        ));
    }
    let mut sections: Vec<FormatArg> = Vec::new();
    let mut next_may_be_object = false;
    for arg in args {
        if next_may_be_object && arg.is_object() {
            next_may_be_object = false;
            let obj = arg.as_object().unwrap();
            let section = sections.last_mut().unwrap();
            if let Some(v) = obj.get("font-scale") {
                section.scale = Some(parse(v)?);
            }
            if let Some(v) = obj.get("text-font") {
                section.font = Some(parse(v)?);
            }
            if let Some(v) = obj.get("text-color") {
                section.text_color = Some(parse(v)?);
            }
            if let Some(v) = obj.get("vertical-align") {
                if let Some(s) = v.as_str() {
                    if !VERTICAL_ALIGN.contains(&s) {
                        return Err(ParseError::new(format!(
                            "'vertical-align' must be one of: 'bottom', 'center', 'top' but found '{s}' instead."
                        )));
                    }
                }
                section.vertical_align = Some(parse(v)?);
            }
        } else {
            sections.push(FormatArg {
                content: parse(arg)?,
                scale: None,
                font: None,
                text_color: None,
                vertical_align: None,
            });
            next_may_be_object = true;
        }
    }
    Ok(Expr::Format(sections))
}

fn parse_interp_kind(json: &Json) -> Result<InterpKind> {
    let items = json.as_array().ok_or_else(|| {
        ParseError::new("Interpolation type must be an array, e.g. [\"linear\"].")
    })?;
    let name = items
        .first()
        .and_then(Json::as_str)
        .ok_or_else(|| ParseError::new("Interpolation type name must be a string."))?;
    match name {
        "linear" => Ok(InterpKind::Linear),
        "exponential" => {
            let base = items
                .get(1)
                .and_then(Json::as_f64)
                .ok_or_else(|| ParseError::new("'exponential' interpolation requires a base."))?;
            Ok(InterpKind::Exponential(base))
        }
        "cubic-bezier" => {
            let cubic_err = || {
                ParseError::new(
                    "Cubic bezier interpolation requires four numeric arguments with values between 0 and 1.",
                )
            };
            // Exactly four control points, each in 0..=1.
            if items.len() != 5 {
                return Err(cubic_err());
            }
            let n = |i: usize| items.get(i).and_then(Json::as_f64);
            match (n(1), n(2), n(3), n(4)) {
                (Some(a), Some(b), Some(c), Some(d))
                    if [a, b, c, d].iter().all(|v| (0.0..=1.0).contains(v)) =>
                {
                    Ok(InterpKind::CubicBezier(a, b, c, d))
                }
                _ => Err(cubic_err()),
            }
        }
        other => Err(ParseError::new(format!(
            "Unknown interpolation type \"{other}\"."
        ))),
    }
}

/// Validate the (optional) item-type and length arguments of `array` against
/// the raw JSON: they must be a bare type name and a bare non-negative integer,
/// not `["literal", ...]` sub-expressions.
fn validate_array_type_args(args: &[Json]) -> Result<()> {
    if args.len() < 2 {
        return Ok(());
    }
    match args[0].as_str() {
        Some("string" | "number" | "boolean") => {}
        _ => {
            return Err(ParseError::new(
                "The item type argument of \"array\" must be one of string, number, boolean.",
            ))
        }
    }
    if args.len() >= 3 {
        // The length may be null (unspecified) or a non-negative integer.
        if !args[1].is_null() {
            match args[1].as_f64() {
                Some(n) if n >= 0.0 && n.fract() == 0.0 => {}
                _ => {
                    return Err(ParseError::new(
                        "The length argument to \"array\" must be a positive integer literal.",
                    ))
                }
            }
        }
    }
    Ok(())
}

fn check_ascending(stops: &[(f64, Expr)]) -> Result<()> {
    for pair in stops.windows(2) {
        if pair[1].0 <= pair[0].0 {
            return Err(ParseError::new(
                "Stop inputs must be arranged in strictly ascending order.",
            ));
        }
    }
    Ok(())
}

fn expect_arity(op: &str, args: &[Json], n: usize) -> Result<()> {
    if args.len() == n {
        Ok(())
    } else {
        Err(ParseError::new(format!(
            "Expected {n} argument(s) to '{op}', but found {}.",
            args.len()
        )))
    }
}
