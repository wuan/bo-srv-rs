//! Geometry pipeline for strike clustering (replaces the Python
//! `shapely`/`geographiclib` stack).
//!
//! The Python `Clustering.build_clusters` post-processes each merged cluster of
//! more than two strikes with:
//!
//! ```text
//! hull = ConvexHull(points)                        # points are (x=lon, y=lat)
//! shape_points = [round(hull vertex, 4) ...]       # -> LinearRing
//! ring.buffer(0.02)                               # round-join buffer
//!      .simplify(0.01, preserve_topology=False)   # Douglas-Peucker
//!      .exterior                                  # outer ring
//! round each coordinate to 4 decimals
//! ```
//!
//! This module maps each step onto the [`geo`] crate:
//!
//! * [`convex_hull_ring`] — `geo::ConvexHull`, then round the ring vertices.
//! * [`buffer_ring`] — `geo::Buffer` (`BufferStyle::new(0.02)`, round caps and
//!   joins, matching shapely's default `quad_segs=8` round buffer), then take
//!   the exterior of the largest resulting polygon.
//! * [`simplify_ring`] — `geo::Simplify` (Douglas-Peucker; `geo`'s
//!   `simplify` is the topology-*discarding* variant, i.e. shapely's
//!   `preserve_topology=False`).
//! * [`polygon_area_km2`] — `geographiclib-rs` `PolygonArea` on WGS84.
//!
//! ## Deviation from the Python reference
//!
//! The *simplification* step cannot be made byte-for-byte identical: the 2016
//! Python test was recorded against GEOS ~3.4 (shapely `>= 1.3.0`), whose
//! Douglas-Peucker implementation produced a coarser ring than current GEOS
//! (`11.04 51.05` was dropped and five vertices remained).  Every modern
//! geometry engine — current GEOS, and `geo`'s Douglas-Peucker — keeps that
//! corner, so the simplified ring has a different vertex set than the 2016
//! expectation.  The convex hull, the buffer and the area are reproduced
//! exactly; see the crate PR for details.

use geo::algorithm::convex_hull::ConvexHull;
use geo::algorithm::simplify::Simplify;
use geo::{Buffer, Coord, LineString, MultiPoint, Point, Polygon};
use geographiclib_rs::{Geodesic, PolygonArea, Winding};

use crate::round::py_round;

/// Scale a coordinate to a rounded value like Python's `round(x, precision)`.
fn round_coord(value: f64, precision: i32) -> f64 {
    py_round(value, precision)
}

/// Convex hull of `points` (`(x, y)` = `(lon, lat)`) as a closed ring of rounded
/// vertices, or `None` when a hull cannot be built (fewer than three points, or
/// all points collinear — Python's `QhullError`).
pub fn convex_hull_ring(points: &[(f64, f64)], precision: i32) -> Option<Vec<(f64, f64)>> {
    if points.len() < 3 {
        return None;
    }
    let multi_point: MultiPoint<f64> = points.iter().map(|&(x, y)| Point::new(x, y)).collect();
    let hull = multi_point.convex_hull();
    let exterior = hull.exterior();
    if exterior.0.len() < 4 {
        // A degenerate hull (e.g. collinear points) collapses to fewer than
        // three distinct vertices.
        return None;
    }
    Some(
        exterior
            .0
            .iter()
            .map(|c| (round_coord(c.x, precision), round_coord(c.y, precision)))
            .collect(),
    )
}

/// Buffer a ring (a closed list of `(x, y)`) by `distance` degrees and return
/// the exterior ring of the resulting polygon, rounded to four decimals.
///
/// Mirrors `LinearRing(ring).buffer(buffer_size).exterior`: the ring is
/// buffered as a closed line string with round joins, then the outer boundary
/// of the largest polygon is taken.
pub fn buffer_ring(ring: &[(f64, f64)], distance: f64) -> Vec<(f64, f64)> {
    let line: LineString<f64> = ring.iter().map(|&(x, y)| Coord { x, y }).collect();
    let buffered = line.buffer(distance);
    // shapely's buffer of a ring yields a single polygon; pick the largest to be
    // robust against numerical slivers.
    let polygon = buffered.0.iter().max_by(|a, b| {
        polygon_area_deg2(a)
            .partial_cmp(&polygon_area_deg2(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    match polygon {
        Some(polygon) => polygon
            .exterior()
            .coords()
            .map(|c| (round_coord(c.x, 4), round_coord(c.y, 4)))
            .collect(),
        None => Vec::new(),
    }
}

/// Simplify a ring with Douglas-Peucker at tolerance `tolerance` (degrees),
/// returning the exterior ring rounded to four decimals.
///
/// Mirrors `Polygon(ring).simplify(tolerance, preserve_topology=False)`:
/// `geo::Simplify` discards topology like the `preserve_topology=False` variant.
pub fn simplify_ring(ring: &[(f64, f64)], tolerance: f64) -> Vec<(f64, f64)> {
    let line: LineString<f64> = ring.iter().map(|&(x, y)| Coord { x, y }).collect();
    if line.0.len() < 4 {
        return ring.to_vec();
    }
    // `simplify` on a closed LineString keeps it closed; `geo`'s polygon
    // simplifier is applied to the polygon built from the ring so the result has
    // an exterior ring, like the Python `.exterior`.
    let polygon = Polygon::new(line, Vec::new());
    let simplified = polygon.simplify(tolerance);
    simplified
        .exterior()
        .coords()
        .map(|c| (round_coord(c.x, 4), round_coord(c.y, 4)))
        .collect()
}

/// Geodesic polygon area in km², rounded to one decimal (port of
/// `builder.strike_cluster.StrikeCluster.build` with `geographiclib`).
///
/// `None` mirrors the Python `area = None` when the shape has no coordinates.
///
/// geographiclib-rs' [`Winding`] parameter is interpreted relative to the
/// ring's actual orientation; to stay independent of whether the exterior ring
/// comes out clockwise or counter-clockwise, the area is always computed with
/// [`Winding::CounterClockwise`] and a result larger than half the ellipsoid is
/// reflected back (`geoid_area - area`).  Python's `PolygonArea` returns the
/// same unsigned magnitude regardless of orientation.
pub fn polygon_area_km2(ring: &[(f64, f64)]) -> Option<f64> {
    if ring.is_empty() {
        return None;
    }
    let geod = Geodesic::wgs84();
    let geoid_area = geod.area();
    let mut area = PolygonArea::new(&geod, Winding::CounterClockwise);
    for &(x, y) in ring {
        // Python: `poly_area.AddPoint(x, y)` — the first argument is longitude.
        area.add_point(x, y);
    }
    let (_perimeter, area_m2, _points) = area.compute(false);
    // A ring whose winding disagrees with the declared convention is measured as
    // the complement; reflect it back to the enclosed area.
    let area_m2 = if area_m2 > geoid_area / 2.0 {
        geoid_area - area_m2
    } else {
        area_m2
    };
    Some(crate::round::py_round(area_m2 / 1e6, 1))
}

/// Planar polygon area in degree² (used only to pick the largest buffered
/// polygon).
fn polygon_area_deg2(polygon: &Polygon<f64>) -> f64 {
    use geo::algorithm::area::Area;
    polygon.unsigned_area()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convex_hull_of_reference_triangle() {
        let points = [
            (11.0, 51.0),
            (11.02, 51.02),
            (11.02, 51.05),
            (11.015, 51.025),
        ];
        let ring = convex_hull_ring(&points, 4).unwrap();
        // The strictly-interior point (11.015, 51.025) is dropped; the ring is
        // closed.  The starting vertex is implementation-defined.
        assert_eq!(ring.len(), 4);
        assert_eq!(ring.first(), ring.last());
        let mut unique: Vec<(f64, f64)> = ring[..3].to_vec();
        unique.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(unique, vec![(11.0, 51.0), (11.02, 51.02), (11.02, 51.05)]);
    }

    #[test]
    fn convex_hull_needs_three_points() {
        assert!(convex_hull_ring(&[(1.0, 1.0), (2.0, 2.0)], 4).is_none());
        assert!(convex_hull_ring(&[], 4).is_none());
    }

    #[test]
    fn convex_hull_collinear_is_none() {
        let points = [(0.0, 0.0), (1.0, 1.0), (2.0, 2.0), (3.0, 3.0)];
        assert!(convex_hull_ring(&points, 4).is_none());
    }

    #[test]
    fn buffer_contains_hull_vertices() {
        let ring = vec![(11.0, 51.0), (11.02, 51.02), (11.02, 51.05), (11.0, 51.0)];
        let buffered = buffer_ring(&ring, 0.02);
        assert!(buffered.len() > ring.len());
        let xs: Vec<f64> = buffered.iter().map(|p| p.0).collect();
        let ys: Vec<f64> = buffered.iter().map(|p| p.1).collect();
        // The buffer expands the hull's bounding box (x 11.0..11.02,
        // y 51.0..51.05) by roughly the buffer size.
        assert!(xs.iter().cloned().fold(f64::INFINITY, f64::min) <= 10.981);
        assert!(xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max) >= 11.039);
        assert!(ys.iter().cloned().fold(f64::INFINITY, f64::min) <= 50.981);
        assert!(ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max) >= 51.069);
    }

    #[test]
    fn simplify_keeps_closed_ring() {
        let ring = buffer_ring(
            &[(11.0, 51.0), (11.02, 51.02), (11.02, 51.05), (11.0, 51.0)],
            0.02,
        );
        let simplified = simplify_ring(&ring, 0.01);
        assert!(simplified.len() >= 4);
        assert_eq!(simplified.first(), simplified.last());
    }

    #[test]
    fn polygon_area_matches_python() {
        // The reference cluster's modern simplified ring: Python reports 38.2 km².
        let ring = [
            (11.0355, 51.0073),
            (11.0095, 50.9824),
            (10.9874, 50.9845),
            (10.9803, 51.0037),
            (11.0044, 51.0625),
            (11.0276, 51.0685),
        ];
        let area = polygon_area_km2(&ring);
        assert_eq!(area, Some(38.2));
    }

    #[test]
    fn polygon_area_none_for_empty() {
        assert_eq!(polygon_area_km2(&[]), None);
    }
}
