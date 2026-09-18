//! Regression tests for the `distance` operator's degenerate-geometry paths.
//!
//! The reference implementation (maplibre-style-spec
//! `src/expression/definitions/distance.ts` + `src/util/cheap_ruler.ts`, at the
//! commit pinned by `src/reference/ATTRIBUTION.md`) never throws on an empty or
//! single-point geometry: `CheapRuler.pointOnLine` leaves its `minX`/`minY`
//! locals `undefined` when the line has no segments, so the result degrades to
//! `NaN`, and `pointsToPolygonDistance`/`pointSetToPointSetDistance` seed their
//! running minimum from `points[0]`/`polygon[0][0]`, so a degenerate one-point
//! "line" still yields a finite answer.

use maplibre_expr::{evaluate, parse, EvaluationContext, Feature, Value};
use serde_json::{json, Value as Json};

/// A canonical tile spanning lng `[0, 2.8125]` / lat `[0, 2.80]`, so every
/// feature coordinate used below survives the tile round trip with sub-metre
/// error. Its bottom-left corner is exactly `(0, 0)`.
const CANONICAL: (u32, u32, u32) = (7, 64, 63);

/// Evaluate `["distance", geom]` against a feature, returning the number.
///
/// Only the *feature* geometry is projected through tile coordinates; the
/// argument geometry is used as given, so it need not lie inside the tile.
fn distance(geom: Json, geometry_type: &str, geometry: Vec<Vec<(f64, f64)>>) -> f64 {
    let expr = parse(&json!(["distance", geom])).expect("distance should parse");
    let ctx = EvaluationContext {
        canonical: Some(CANONICAL),
        feature: Feature {
            geometry_type: Some(geometry_type.to_string()),
            geometry,
            ..Feature::default()
        },
        ..EvaluationContext::default()
    };
    match evaluate(&expr, &ctx).expect("distance should evaluate") {
        Value::Number(n) => n,
        other => panic!("distance did not evaluate to a number: {other:?}"),
    }
}

/// A square well clear of the origin, as a GeoJSON polygon. Its first vertex —
/// the one the upstream seed measures to — is `(2, 2)`.
fn far_square() -> Json {
    json!({
        "type": "Polygon",
        "coordinates": [[[2.0, 2.0], [3.0, 2.0], [3.0, 3.0], [2.0, 3.0], [2.0, 2.0]]]
    })
}

// --- A3: empty argument geometries must not panic ---------------------------

#[test]
fn empty_line_argument_yields_nan() {
    // `Ruler::point_on_line` used to seed from `line[0]` and panicked here.
    let d = distance(
        json!({"type": "LineString", "coordinates": []}),
        "Point",
        vec![vec![(0.5, 0.5)]],
    );
    assert!(d.is_nan(), "expected NaN for an empty LineString, got {d}");
}

#[test]
fn empty_polygon_ring_argument_yields_nan() {
    let d = distance(
        json!({"type": "Polygon", "coordinates": [[]]}),
        "Point",
        vec![vec![(0.5, 0.5)]],
    );
    assert!(
        d.is_nan(),
        "expected NaN for an empty Polygon ring, got {d}"
    );
}

#[test]
fn empty_argument_geometries_against_a_line_feature_yield_nan() {
    let feature = vec![vec![(0.5, 0.5), (1.0, 1.0)]];
    for geom in [
        json!({"type": "LineString", "coordinates": []}),
        json!({"type": "Polygon", "coordinates": [[]]}),
    ] {
        let d = distance(geom.clone(), "LineString", feature.clone());
        assert!(d.is_nan(), "expected NaN for {geom}, got {d}");
    }
}

// --- C19: the missing `currentMiniDist` seed --------------------------------

#[test]
fn single_point_line_feature_to_polygon_is_finite() {
    // A one-point LineString has no segments, so the brute-force loop finds
    // nothing and returns infinity; upstream still returns the seeded
    // `ruler.distance(points[0], polygon[0][0])`. Before the fix this was `inf`.
    let d = distance(far_square(), "LineString", vec![vec![(0.5, 0.5)]]);
    assert!(
        d.is_finite(),
        "expected a finite distance from a degenerate one-point line, got {d}"
    );

    // The seed is the distance to `polygon[0][0]`, i.e. to the vertex (2, 2).
    let to_first_vertex = distance(
        json!({"type": "Point", "coordinates": [2.0, 2.0]}),
        "Point",
        vec![vec![(0.5, 0.5)]],
    );
    assert!(
        (d - to_first_vertex).abs() < 1e-6,
        "seed should be the distance to polygon[0][0]: {d} vs {to_first_vertex}"
    );
}

#[test]
fn single_point_line_feature_to_point_yields_nan() {
    // Upstream takes the `isLine1 && !isLine2` branch, which calls
    // `pointToLineDistance` against a one-point sub-line; `pointOnLine` returns
    // `[undefined, undefined]`, so `Math.min(seed, NaN)` is NaN. The seed alone
    // would be finite, hence the explicit NaN propagation in `js_min`.
    let d = distance(
        json!({"type": "Point", "coordinates": [2.0, 2.0]}),
        "LineString",
        vec![vec![(0.5, 0.5)]],
    );
    assert!(
        d.is_nan(),
        "a one-point line against a point is NaN upstream, got {d}"
    );
}

// --- the non-degenerate path must be untouched ------------------------------

#[test]
fn one_degree_of_latitude_at_the_equator() {
    // The ruler is calibrated to the *feature*'s latitude, so a feature at
    // (0, 0) gives the equatorial `ky` exactly.
    let d = distance(
        json!({"type": "Point", "coordinates": [0.0, 1.0]}),
        "Point",
        vec![vec![(0.0, 0.0)]],
    );
    assert!(
        (d - 110574.276).abs() < 0.01,
        "one degree of latitude at the equator should be 110574.276 m, got {d}"
    );
}

#[test]
fn point_inside_polygon_is_zero() {
    let d = distance(far_square(), "Point", vec![vec![(2.5, 2.5)]]);
    assert_eq!(d, 0.0);
}

/// B1: `Feature::geometry` holds raw lng/lat degrees, not tile coordinates —
/// the doc used to claim the latter. Feeding tile coordinates gives a
/// nonsensical answer rather than an error, which is why the doc matters.
#[test]
fn feature_geometry_is_lng_lat_degrees() {
    let in_degrees = distance(
        json!({"type": "Point", "coordinates": [0.0, 0.0]}),
        "Point",
        vec![vec![(0.5, 0.5)]],
    );
    assert!(
        in_degrees.is_finite() && (10_000.0..200_000.0).contains(&in_degrees),
        "lng/lat degrees should give a sane sub-degree distance, got {in_degrees}"
    );

    // The same location expressed in the tile coordinates the old doc
    // described. It is silently wrong, not an error.
    let in_tile_coords = distance(
        json!({"type": "Point", "coordinates": [0.0, 0.0]}),
        "Point",
        vec![vec![(4107.4, 4084.6)]],
    );
    assert!(
        !in_tile_coords.is_finite(),
        "tile coordinates should not be accepted as degrees, got {in_tile_coords}"
    );
}
