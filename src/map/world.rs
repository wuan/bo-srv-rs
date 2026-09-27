//! SVG world-map base layer for the servicelog HTML report (issue #28).
//!
//! The map is a plain equirectangular projection of a coarse landmass basemap
//! embedded in the binary, so the report needs no GIS dependency and no network
//! access.  The basemap gives the local-query overlay a geographic frame.
//!
//! Coordinates are `(longitude, latitude)` pairs in degrees.  The projection
//! maps them onto a `width x height` viewport:
//!
//! * `x = (lon + 180) / 360 * width`
//! * `y = (90 - lat) / 180 * height`
//!
//! so the map covers the whole world with north up and the antimeridian at the
//! left/right edges.
//!
//! ## Provenance and licence
//!
//! The outlines are derived from the **Natural Earth** 1:110m "Land" physical
//! vector dataset, `ne_110m_land`
//! (<https://www.naturalearthdata.com/downloads/110m-physical-vectors/110m-land/>).
//! Natural Earth data is in the **public domain** (no attribution required), and
//! the source is only noted here for provenance.  The `assets/world-110m-land.geojson`
//! asset was produced from the official GeoJSON release
//! (`nvkelso/natural-earth-vector`, `geojson/ne_110m_land.geojson`) by applying
//! Douglas-Peucker simplification at ~0.1 degrees and quantizing the coordinates
//! to 0.02 degrees; latitudes span the full `[-90, 90]` range (Antarctica reaches
//! the south pole) and rings crossing the antimeridian were split so no path
//! draws a horizontal streak across the map.
//!
//! ## Accuracy
//!
//! The outlines are fine (about 4200 points in total) and are **only** meant as
//! an orientation aid, not a survey-accurate basemap.  The local-query overlay
//! uses the same projection, so a marker sits in the right part of the right
//! continent.

use std::fmt::Write as _;
use std::sync::OnceLock;

/// The map viewport size of the standalone SVG (pixels).
pub const MAP_WIDTH: u32 = 960;
/// The map viewport height of the standalone SVG (pixels).
pub const MAP_HEIGHT: u32 = 480;

/// The simplified Natural Earth 110m land basemap (see the module docs).
const LAND_GEOJSON: &str = include_str!("../../assets/world-110m-land.geojson");

/// The parsed basemap: closed landmass rings of `(longitude, latitude)` degrees.
///
/// Parsed once from the embedded [`LAND_GEOJSON`] on first use.  Every ring is
/// closed (its last point repeats its first) and all coordinates are within
/// `[-180, 180]` / `[-90, 90]`.
static LAND_RINGS: OnceLock<Vec<Vec<(f64, f64)>>> = OnceLock::new();

/// The landmass rings of the embedded basemap (see [`LAND_RINGS`]).
///
/// Parse errors are impossible for the embedded, build-time asset; should the
/// asset ever be malformed the map simply renders without a basemap rather than
/// panicking.
pub fn land_rings() -> &'static [Vec<(f64, f64)>] {
    LAND_RINGS
        .get_or_init(|| parse_land_rings(LAND_GEOJSON))
        .as_slice()
}

/// Parse the embedded GeoJSON `FeatureCollection` into closed landmass rings.
fn parse_land_rings(json: &str) -> Vec<Vec<(f64, f64)>> {
    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let mut rings = Vec::new();
    let Some(features) = value.get("features").and_then(|f| f.as_array()) else {
        return rings;
    };
    for feature in features {
        let geometry = feature.get("geometry");
        let Some(kind) = geometry
            .and_then(|g| g.get("type"))
            .and_then(|t| t.as_str())
        else {
            continue;
        };
        let Some(coordinates) = geometry.and_then(|g| g.get("coordinates")) else {
            continue;
        };
        match kind {
            "Polygon" => {
                // A Polygon's coordinates are its rings; only the exterior ring
                // (index 0) is filled for this coarse basemap.
                if let Some(exterior) = coordinates.get(0) {
                    push_ring(&mut rings, exterior);
                }
            }
            "MultiPolygon" => {
                if let Some(polygons) = coordinates.as_array() {
                    for polygon in polygons {
                        if let Some(exterior) = polygon.get(0) {
                            push_ring(&mut rings, exterior);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    rings
}

/// Append one GeoJSON ring of `[lon, lat]` pairs as a closed `(f64, f64)` ring.
fn push_ring(rings: &mut Vec<Vec<(f64, f64)>>, coordinates: &serde_json::Value) {
    let Some(points) = coordinates.as_array() else {
        return;
    };
    let mut ring: Vec<(f64, f64)> = Vec::with_capacity(points.len() + 1);
    for point in points {
        let Some(pair) = point.as_array() else {
            continue;
        };
        let (Some(lon), Some(lat)) = (
            pair.first().and_then(|v| v.as_f64()),
            pair.get(1).and_then(|v| v.as_f64()),
        ) else {
            continue;
        };
        ring.push((lon, lat));
    }
    if ring.len() < 3 {
        return;
    }
    // Close the ring (the SVG path builder also appends `Z`, but keeping the
    // repeated first point makes `first == last` a simple invariant to test).
    if ring.first() != ring.last() {
        let first = ring[0];
        ring.push(first);
    }
    rings.push(ring);
}

/// Project a `(longitude, latitude)` degree pair into the SVG viewport.
///
/// Longitudes are assumed to be within `[-180, 180]`; latitudes within
/// `[-90, 90]`.  See the module docs for the projection.
pub fn project(lon: f64, lat: f64, width: f64, height: f64) -> (f64, f64) {
    let x = (lon + 180.0) / 360.0 * width;
    let y = (90.0 - lat) / 180.0 * height;
    (x, y)
}

/// Format an SVG path `d` attribute for a closed ring of `(lon, lat)` points.
fn ring_path(ring: &[(f64, f64)], width: f64, height: f64) -> String {
    let mut d = String::new();
    for (index, (lon, lat)) in ring.iter().enumerate() {
        let (x, y) = project(*lon, *lat, width, height);
        let command = if index == 0 { 'M' } else { 'L' };
        let _ = write!(d, "{command}{x:.1},{y:.1}");
    }
    d.push('Z');
    d
}

/// Render the landmass outlines as an SVG `<g>` group.
///
/// `fill`/`stroke` are plain CSS colours; the group carries the `basemap`
/// class so the report stylesheet can theme it.
pub fn continent_layer(width: u32, height: u32, fill: &str, stroke: &str) -> String {
    let (w, h) = (width as f64, height as f64);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "<g class=\"basemap\" fill=\"{fill}\" stroke=\"{stroke}\" \
         stroke-width=\"0.5\" stroke-linejoin=\"round\">"
    );
    for ring in land_rings() {
        let _ = writeln!(out, "  <path d=\"{}\"/>", ring_path(ring, w, h));
    }
    out.push_str("</g>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_maps_the_corners() {
        assert_eq!(project(-180.0, 90.0, 360.0, 180.0), (0.0, 0.0));
        assert_eq!(project(180.0, -90.0, 360.0, 180.0), (360.0, 180.0));
        assert_eq!(project(0.0, 0.0, 360.0, 180.0), (180.0, 90.0));
    }

    #[test]
    fn projection_scales_to_the_viewport() {
        let (x, y) = project(0.0, 0.0, MAP_WIDTH as f64, MAP_HEIGHT as f64);
        assert!((x - MAP_WIDTH as f64 / 2.0).abs() < 1e-9);
        assert!((y - MAP_HEIGHT as f64 / 2.0).abs() < 1e-9);
    }

    /// The embedded Natural Earth asset parses into a non-trivial basemap.
    #[test]
    fn embedded_asset_parses_into_land_rings() {
        let rings = land_rings();
        // Natural Earth 110m land has ~120+ polygons; after the antimeridian
        // split we still expect well over a hundred rings.
        assert!(rings.len() > 100, "only {} rings parsed", rings.len());
        let points: usize = rings.iter().map(|r| r.len()).sum();
        assert!(points > 500, "only {points} points parsed");
        // A recognizable basemap: every continent contributes points.
        assert!(rings.iter().any(|r| r.len() > 50), "no large landmass");
    }

    #[test]
    fn continent_layer_lists_every_outline() {
        let layer = continent_layer(360, 180, "#ccc", "#888");
        assert!(layer.starts_with("<g class=\"basemap\""));
        assert!(layer.trim_end().ends_with("</g>"));
        assert_eq!(layer.matches("<path").count(), land_rings().len());
    }

    #[test]
    fn every_ring_is_closed_and_in_range() {
        for ring in land_rings() {
            assert!(ring.len() >= 3, "ring too small");
            assert_eq!(ring.first(), ring.last(), "ring is not closed");
            for (lon, lat) in ring {
                assert!((-180.0..=180.0).contains(lon), "lon {lon} out of range");
                assert!((-90.0..=90.0).contains(lat), "lat {lat} out of range");
            }
        }
        // The basemap covers the full latitude range: Antarctica reaches -90.
        let min_lat = land_rings()
            .iter()
            .flatten()
            .map(|(_, lat)| *lat)
            .fold(f64::INFINITY, f64::min);
        assert_eq!(min_lat, -90.0, "basemap must extend to the south pole");
    }

    /// No ring draws a horizontal streak across the map: consecutive points may
    /// not jump by ~360 degrees except along the flat Antarctic bottom.
    #[test]
    fn no_ring_crosses_the_antimeridian() {
        for ring in land_rings() {
            for pair in ring.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                if (a.0 - b.0).abs() > 180.0 {
                    assert!(
                        a.1 <= -84.0 && b.1 <= -84.0,
                        "antimeridian streak from {a:?} to {b:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn ring_path_is_closed() {
        let d = ring_path(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)], 360.0, 180.0);
        assert!(d.starts_with('M'));
        assert!(d.ends_with('Z'));
        assert_eq!(d.matches('L').count(), 2);
    }

    /// A malformed asset renders no basemap instead of panicking.
    #[test]
    fn parse_land_rings_handles_invalid_json() {
        assert!(parse_land_rings("not json").is_empty());
        assert!(parse_land_rings("{}").is_empty(), "no features key");
        assert!(
            parse_land_rings(r#"{"features": 42}"#).is_empty(),
            "features is not an array"
        );
    }

    /// Polygon and MultiPolygon exteriors become rings; interior rings, unknown
    /// geometry types and features without geometry are skipped.
    #[test]
    fn parse_land_rings_reads_polygons_and_multipolygons() {
        let json = r#"{
          "features": [
            {"geometry": {"type": "Polygon", "coordinates": [
               [[0,0],[10,0],[10,10]],
               [[1,1],[2,2],[3,3]]
            ]}},
            {"geometry": {"type": "MultiPolygon", "coordinates": [
               [[[0,0],[1,0],[1,1]]],
               [[[5,5],[6,5],[6,6]]]
            ]}},
            {"geometry": {"type": "Point", "coordinates": [0,0]}},
            {"properties": {}}
          ]
        }"#;
        let rings = parse_land_rings(json);
        // One Polygon exterior + two MultiPolygon exteriors = three rings.
        assert_eq!(rings.len(), 3, "{rings:?}");
        for ring in &rings {
            assert_eq!(ring.first(), ring.last(), "ring closed");
        }
    }

    /// Degenerate and malformed rings are dropped rather than emitted.
    #[test]
    fn push_ring_drops_degenerate_and_malformed() {
        let mut rings: Vec<Vec<(f64, f64)>> = Vec::new();
        // Not an array.
        push_ring(&mut rings, &serde_json::json!("nope"));
        // Too few points.
        push_ring(&mut rings, &serde_json::json!([[0, 0], [1, 1]]));
        // A non-array point and a point with a missing/non-numeric latitude are
        // skipped; the three valid points survive and the ring is closed.
        push_ring(
            &mut rings,
            &serde_json::json!([[0, 0], "x", [1, 0], [2, "y"], [3, 3]]),
        );
        assert_eq!(rings.len(), 1, "only the three valid points survive");
        assert_eq!(
            rings[0],
            vec![(0.0, 0.0), (1.0, 0.0), (3.0, 3.0), (0.0, 0.0)]
        );

        // An already-closed ring is not double-closed.
        let mut closed: Vec<Vec<(f64, f64)>> = Vec::new();
        push_ring(
            &mut closed,
            &serde_json::json!([[0, 0], [1, 0], [1, 1], [0, 0]]),
        );
        assert_eq!(closed[0].len(), 4, "no extra closing point");
    }
}
