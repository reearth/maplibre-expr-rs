//! The `distance` operator: geodesic distance from the feature geometry to an
//! argument GeoJSON geometry, ported from maplibre-style-spec's `distance.ts`
//! and `cheap_ruler.ts`.
//!
//! MapLibre uses a bounding-volume hierarchy to prune the pairwise search; the
//! minimum distance is independent of traversal order, so this port computes it
//! by brute force over all element pairs — the numeric result is identical.

use crate::geometry::{point_within_polygon, segment_intersect, tile_round_trip};

type P = (f64, f64);

/// One argument geometry (a GeoJSON `Multi*` is split into several of these).
#[derive(Debug, Clone)]
pub enum SimpleGeom {
    Point(P),
    Line(Vec<P>),
    Polygon(Vec<Vec<P>>),
}

/// A cheap-ruler: an equirectangular distance approximation calibrated to a
/// latitude, matching MapLibre's `CheapRuler`.
struct Ruler {
    kx: f64,
    ky: f64,
}

impl Ruler {
    fn new(lat: f64) -> Ruler {
        const RE: f64 = 6378.137;
        const FE: f64 = 1.0 / 298.257223563;
        let e2 = FE * (2.0 - FE);
        let rad = std::f64::consts::PI / 180.0;
        let m = rad * RE * 1000.0;
        let coslat = (lat * rad).cos();
        let w2 = 1.0 / (1.0 - e2 * (1.0 - coslat * coslat));
        let w = w2.sqrt();
        Ruler {
            kx: m * w * coslat,
            ky: m * w * w2 * (1.0 - e2),
        }
    }

    fn wrap(mut deg: f64) -> f64 {
        while deg < -180.0 {
            deg += 360.0;
        }
        while deg > 180.0 {
            deg -= 360.0;
        }
        deg
    }

    fn distance(&self, a: P, b: P) -> f64 {
        let dx = Self::wrap(a.0 - b.0) * self.kx;
        let dy = (a.1 - b.1) * self.ky;
        (dx * dx + dy * dy).sqrt()
    }

    /// The nearest point on `line` to `p`.
    fn point_on_line(&self, line: &[P], p: P) -> P {
        let mut min_dist = f64::INFINITY;
        let mut best = line[0];
        for seg in line.windows(2) {
            let (mut x, mut y) = seg[0];
            let mut dx = Self::wrap(seg[1].0 - x) * self.kx;
            let mut dy = (seg[1].1 - y) * self.ky;
            if dx != 0.0 || dy != 0.0 {
                let t = (Self::wrap(p.0 - x) * self.kx * dx + (p.1 - y) * self.ky * dy)
                    / (dx * dx + dy * dy);
                if t > 1.0 {
                    x = seg[1].0;
                    y = seg[1].1;
                } else if t > 0.0 {
                    x += (dx / self.kx) * t;
                    y += (dy / self.ky) * t;
                }
            }
            dx = Self::wrap(p.0 - x) * self.kx;
            dy = (p.1 - y) * self.ky;
            let sq = dx * dx + dy * dy;
            if sq < min_dist {
                min_dist = sq;
                best = (x, y);
            }
        }
        best
    }
}

fn point_to_line(point: P, line: &[P], ruler: &Ruler) -> f64 {
    ruler.distance(point, ruler.point_on_line(line, point))
}

fn segment_to_segment(p1: P, p2: P, q1: P, q2: P, ruler: &Ruler) -> f64 {
    let d1 = point_to_line(p1, &[q1, q2], ruler).min(point_to_line(p2, &[q1, q2], ruler));
    let d2 = point_to_line(q1, &[p1, p2], ruler).min(point_to_line(q2, &[p1, p2], ruler));
    d1.min(d2)
}

/// Distance between two point sets, each optionally interpreted as a polyline.
fn set_to_set(a: &[P], a_line: bool, b: &[P], b_line: bool, ruler: &Ruler) -> f64 {
    match (a_line, b_line) {
        (true, true) => {
            let mut dist = f64::INFINITY;
            for pa in a.windows(2) {
                for pb in b.windows(2) {
                    if segment_intersect(pa[0], pa[1], pb[0], pb[1]) {
                        return 0.0;
                    }
                    dist = dist.min(segment_to_segment(pa[0], pa[1], pb[0], pb[1], ruler));
                }
            }
            dist
        }
        (true, false) => b
            .iter()
            .map(|&p| point_to_line(p, a, ruler))
            .fold(f64::INFINITY, f64::min),
        (false, true) => a
            .iter()
            .map(|&p| point_to_line(p, b, ruler))
            .fold(f64::INFINITY, f64::min),
        (false, false) => {
            let mut dist = f64::INFINITY;
            for &pa in a {
                for &pb in b {
                    dist = dist.min(ruler.distance(pa, pb));
                    if dist == 0.0 {
                        return 0.0;
                    }
                }
            }
            dist
        }
    }
}

fn point_to_polygon(point: P, polygon: &[Vec<P>], ruler: &Ruler) -> f64 {
    if point_within_polygon(point, polygon, true) {
        return 0.0;
    }
    let mut dist = f64::INFINITY;
    for ring in polygon {
        if ring.len() >= 2 {
            let (front, back) = (ring[0], ring[ring.len() - 1]);
            if front != back {
                dist = dist.min(point_to_line(point, &[back, front], ruler));
            }
        }
        dist = dist.min(ruler.distance(point, ruler.point_on_line(ring, point)));
        if dist == 0.0 {
            return 0.0;
        }
    }
    dist
}

/// Iterate a ring's edges with wrap-around (closing edge included).
fn ring_edges(ring: &[P]) -> impl Iterator<Item = (P, P)> + '_ {
    let n = ring.len();
    (0..n).map(move |j| {
        let k = if j == 0 { n - 1 } else { j - 1 };
        (ring[k], ring[j])
    })
}

fn line_to_polygon(line: &[P], polygon: &[Vec<P>], ruler: &Ruler) -> f64 {
    if line.iter().any(|&p| point_within_polygon(p, polygon, true)) {
        return 0.0;
    }
    let mut dist = f64::INFINITY;
    for seg in line.windows(2) {
        for ring in polygon {
            for (q1, q2) in ring_edges(ring) {
                if segment_intersect(seg[0], seg[1], q1, q2) {
                    return 0.0;
                }
                dist = dist.min(segment_to_segment(seg[0], seg[1], q1, q2, ruler));
            }
        }
    }
    dist
}

fn polygon_intersect(a: &[Vec<P>], b: &[Vec<P>]) -> bool {
    a.iter()
        .any(|ring| ring.iter().any(|&p| point_within_polygon(p, b, true)))
}

fn polygon_to_polygon(a: &[Vec<P>], b: &[Vec<P>], ruler: &Ruler) -> f64 {
    if polygon_intersect(a, b) || polygon_intersect(b, a) {
        return 0.0;
    }
    let mut dist = f64::INFINITY;
    for r1 in a {
        for (p1, p2) in ring_edges(r1) {
            for r2 in b {
                for (q1, q2) in ring_edges(r2) {
                    if segment_intersect(p1, p2, q1, q2) {
                        return 0.0;
                    }
                    dist = dist.min(segment_to_segment(p1, p2, q1, q2, ruler));
                }
            }
        }
    }
    dist
}

/// Distance from a point set (or polyline) to a polygon.
fn points_to_polygon(points: &[P], is_line: bool, polygon: &[Vec<P>], ruler: &Ruler) -> f64 {
    if is_line {
        line_to_polygon(points, polygon, ruler)
    } else {
        points
            .iter()
            .map(|&p| point_to_polygon(p, polygon, ruler))
            .fold(f64::INFINITY, f64::min)
    }
}

fn signed_area(ring: &[P]) -> f64 {
    let n = ring.len();
    let mut sum = 0.0;
    for i in 0..n {
        let j = if i == 0 { n - 1 } else { i - 1 };
        let (p1, p2) = (ring[i], ring[j]);
        sum += (p2.0 - p1.0) * (p1.1 + p2.1);
    }
    sum
}

/// Group polygon rings into polygons (an exterior ring plus its holes) by
/// winding order, matching MapLibre's `classifyRings`.
fn classify_rings(rings: &[Vec<P>]) -> Vec<Vec<Vec<P>>> {
    if rings.len() <= 1 {
        return vec![rings.to_vec()];
    }
    let mut polygons: Vec<Vec<Vec<P>>> = Vec::new();
    let mut polygon: Vec<Vec<P>> = Vec::new();
    let mut ccw: Option<bool> = None;
    for ring in rings {
        let area = signed_area(ring);
        if area == 0.0 {
            continue;
        }
        let is_ccw = area < 0.0;
        if ccw.is_none() {
            ccw = Some(is_ccw);
        }
        if ccw == Some(is_ccw) {
            if !polygon.is_empty() {
                polygons.push(std::mem::take(&mut polygon));
            }
            polygon.push(ring.clone());
        } else {
            polygon.push(ring.clone());
        }
    }
    if !polygon.is_empty() {
        polygons.push(polygon);
    }
    polygons
}

/// Compute `distance(feature, args)`; returns `NaN` when the geometry is
/// unavailable (matching the reference).
pub fn distance(feature_geom: &[Vec<P>], geom_type: &str, z: u32, args: &[SimpleGeom]) -> f64 {
    if feature_geom.is_empty() {
        return f64::NAN;
    }
    match geom_type {
        "Point" | "LineString" => {
            let pts: Vec<P> = feature_geom
                .iter()
                .flatten()
                .map(|&(lng, lat)| tile_round_trip(lng, lat, z))
                .collect();
            if pts.is_empty() {
                return f64::NAN;
            }
            let is_line = geom_type == "LineString";
            let ruler = Ruler::new(pts[0].1);
            let mut dist = f64::INFINITY;
            for g in args {
                dist = dist.min(match g {
                    SimpleGeom::Point(p) => set_to_set(&pts, is_line, &[*p], false, &ruler),
                    SimpleGeom::Line(l) => set_to_set(&pts, is_line, l, true, &ruler),
                    SimpleGeom::Polygon(poly) => points_to_polygon(&pts, is_line, poly, &ruler),
                });
                if dist == 0.0 {
                    return 0.0;
                }
            }
            dist
        }
        "Polygon" => {
            let rings: Vec<Vec<P>> = feature_geom
                .iter()
                .map(|ring| {
                    ring.iter()
                        .map(|&(lng, lat)| tile_round_trip(lng, lat, z))
                        .collect()
                })
                .collect();
            let polygons = classify_rings(&rings);
            if polygons.is_empty() || polygons[0].is_empty() || polygons[0][0].is_empty() {
                return f64::NAN;
            }
            let ruler = Ruler::new(polygons[0][0][0].1);
            let mut dist = f64::INFINITY;
            for g in args {
                for poly in &polygons {
                    dist = dist.min(match g {
                        SimpleGeom::Point(p) => points_to_polygon(&[*p], false, poly, &ruler),
                        SimpleGeom::Line(l) => points_to_polygon(l, true, poly, &ruler),
                        SimpleGeom::Polygon(other) => polygon_to_polygon(poly, other, &ruler),
                    });
                    if dist == 0.0 {
                        return 0.0;
                    }
                }
            }
            dist
        }
        _ => f64::NAN,
    }
}
