//! Geometry helpers for the `within` operator: Web Mercator tile projection
//! and point/line-in-polygon tests, ported from maplibre-style-spec's
//! `util/geometry_util.ts` and `definitions/within.ts`.

/// Tile extent (coordinate units per tile edge).
pub const EXTENT: f64 = 8192.0;

type P = (f64, f64);
/// An axis-aligned bounding box `[minx, miny, maxx, maxy]`.
type BBox = [f64; 4];

/// Project a `[lng, lat]` coordinate to global tile coordinates at zoom `z`.
pub fn tile_coord(lng: f64, lat: f64, z: u32) -> P {
    let mx = (180.0 + lng) / 360.0;
    let my = (180.0
        - (180.0 / std::f64::consts::PI)
            * (std::f64::consts::PI / 4.0 + (lat * std::f64::consts::PI) / 360.0)
                .tan()
                .ln())
        / 360.0;
    let tiles = 2f64.powi(z as i32);
    ((mx * tiles * EXTENT).round(), (my * tiles * EXTENT).round())
}

fn update_bbox(b: &mut BBox, p: P) {
    b[0] = b[0].min(p.0);
    b[1] = b[1].min(p.1);
    b[2] = b[2].max(p.0);
    b[3] = b[3].max(p.1);
}

fn reset_bbox(b: &mut BBox) {
    b[0] = f64::INFINITY;
    b[1] = f64::INFINITY;
    b[2] = f64::NEG_INFINITY;
    b[3] = f64::NEG_INFINITY;
}

fn box_within_box(a: &BBox, b: &BBox) -> bool {
    a[0] > b[0] && a[2] < b[2] && a[1] > b[1] && a[3] < b[3]
}

fn ray_intersect(p: P, p1: P, p2: P) -> bool {
    (p1.1 > p.1) != (p2.1 > p.1) && p.0 < (p2.0 - p1.0) * (p.1 - p1.1) / (p2.1 - p1.1) + p1.0
}

fn point_on_boundary(p: P, p1: P, p2: P) -> bool {
    let x1 = p.0 - p1.0;
    let y1 = p.1 - p1.1;
    let x2 = p.0 - p2.0;
    let y2 = p.1 - p2.1;
    x1 * y2 - x2 * y1 == 0.0 && x1 * x2 <= 0.0 && y1 * y2 <= 0.0
}

fn perp(a: P, b: P) -> f64 {
    a.0 * b.1 - a.1 * b.0
}

fn two_sided(p1: P, p2: P, q1: P, q2: P) -> bool {
    let (x1, y1) = (p1.0 - q1.0, p1.1 - q1.1);
    let (x2, y2) = (p2.0 - q1.0, p2.1 - q1.1);
    let (x3, y3) = (q2.0 - q1.0, q2.1 - q1.1);
    let det1 = x1 * y3 - x3 * y1;
    let det2 = x2 * y3 - x3 * y2;
    (det1 > 0.0 && det2 < 0.0) || (det1 < 0.0 && det2 > 0.0)
}

fn segment_intersect(a: P, b: P, c: P, d: P) -> bool {
    let vp = (b.0 - a.0, b.1 - a.1);
    let vq = (d.0 - c.0, d.1 - c.1);
    if perp(vq, vp) == 0.0 {
        return false;
    }
    two_sided(a, b, c, d) && two_sided(c, d, a, b)
}

fn line_intersect_polygon(p1: P, p2: P, polygon: &[Vec<P>]) -> bool {
    polygon.iter().any(|ring| {
        ring.windows(2)
            .any(|w| segment_intersect(p1, p2, w[0], w[1]))
    })
}

fn point_within_polygon(point: P, rings: &[Vec<P>]) -> bool {
    let mut inside = false;
    for ring in rings {
        for w in ring.windows(2) {
            if point_on_boundary(point, w[0], w[1]) {
                return false;
            }
            if ray_intersect(point, w[0], w[1]) {
                inside = !inside;
            }
        }
    }
    inside
}

fn point_within_polygons(point: P, polygons: &[Vec<Vec<P>>]) -> bool {
    polygons.iter().any(|p| point_within_polygon(point, p))
}

fn line_within_polygon(line: &[P], polygon: &[Vec<P>]) -> bool {
    if line.iter().any(|&p| !point_within_polygon(p, polygon)) {
        return false;
    }
    !line
        .windows(2)
        .any(|w| line_intersect_polygon(w[0], w[1], polygon))
}

fn line_within_polygons(line: &[P], polygons: &[Vec<Vec<P>>]) -> bool {
    polygons.iter().any(|p| line_within_polygon(line, p))
}

fn update_point(p: &mut P, bbox: &mut BBox, poly_bbox: &BBox, world_size: f64) {
    if p.0 < poly_bbox[0] || p.0 > poly_bbox[2] {
        let half = world_size * 0.5;
        let mut shift = if p.0 - poly_bbox[0] > half {
            -world_size
        } else if poly_bbox[0] - p.0 > half {
            world_size
        } else {
            0.0
        };
        if shift == 0.0 {
            shift = if p.0 - poly_bbox[2] > half {
                -world_size
            } else if poly_bbox[2] - p.0 > half {
                world_size
            } else {
                0.0
            };
        }
        p.0 += shift;
    }
    update_bbox(bbox, *p);
}

/// A polygon (list of rings) in `[lng, lat]` coordinates.
pub type Polygon = Vec<Vec<P>>;

fn tile_polygon(coords: &Polygon, bbox: &mut BBox, z: u32) -> Vec<Vec<P>> {
    coords
        .iter()
        .map(|ring| {
            ring.iter()
                .map(|&(lng, lat)| {
                    let c = tile_coord(lng, lat, z);
                    update_bbox(bbox, c);
                    c
                })
                .collect()
        })
        .collect()
}

/// Whether the feature geometry (raw `[lng, lat]` coordinates grouped into
/// rings/lines) lies within the argument polygons (also `[lng, lat]`).
pub fn within(
    feature_geom: &[Vec<P>],
    geom_type: &str,
    canonical: (u32, u32, u32),
    polygons: &[Polygon],
) -> bool {
    let (z, _x, _y) = canonical;
    let world_size = 2f64.powi(z as i32) * EXTENT;
    let inf: BBox = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    match geom_type {
        "Point" => {
            let mut poly_bbox = inf;
            let tile_polys: Vec<Vec<Vec<P>>> = polygons
                .iter()
                .map(|p| tile_polygon(p, &mut poly_bbox, z))
                .collect();
            let mut point_bbox = inf;
            let mut points: Vec<P> = Vec::new();
            for group in feature_geom {
                for &(lng, lat) in group {
                    let mut p = tile_coord(lng, lat, z);
                    update_point(&mut p, &mut point_bbox, &poly_bbox, world_size);
                    points.push(p);
                }
            }
            if !box_within_box(&point_bbox, &poly_bbox) {
                return false;
            }
            points.iter().all(|&p| within_point(p, &tile_polys))
        }
        "LineString" => {
            let mut poly_bbox = inf;
            let tile_polys: Vec<Vec<Vec<P>>> = polygons
                .iter()
                .map(|p| tile_polygon(p, &mut poly_bbox, z))
                .collect();
            let mut line_bbox = inf;
            let mut lines: Vec<Vec<P>> = feature_geom
                .iter()
                .map(|line| {
                    line.iter()
                        .map(|&(lng, lat)| tile_coord(lng, lat, z))
                        .collect()
                })
                .collect();
            for line in &lines {
                for &p in line {
                    update_bbox(&mut line_bbox, p);
                }
            }
            if line_bbox[2] - line_bbox[0] <= world_size / 2.0 {
                reset_bbox(&mut line_bbox);
                for line in &mut lines {
                    for p in line.iter_mut() {
                        update_point(p, &mut line_bbox, &poly_bbox, world_size);
                    }
                }
            }
            if !box_within_box(&line_bbox, &poly_bbox) {
                return false;
            }
            lines.iter().all(|line| within_line(line, &tile_polys))
        }
        _ => false,
    }
}

fn within_point(p: P, polys: &[Vec<Vec<P>>]) -> bool {
    if polys.len() == 1 {
        point_within_polygon(p, &polys[0])
    } else {
        point_within_polygons(p, polys)
    }
}

fn within_line(line: &[P], polys: &[Vec<Vec<P>>]) -> bool {
    if polys.len() == 1 {
        line_within_polygon(line, &polys[0])
    } else {
        line_within_polygons(line, polys)
    }
}
