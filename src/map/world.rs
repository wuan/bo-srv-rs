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
//! The outlines are derived from the **Natural Earth** 1:110m "Land" and "Lakes"
//! physical vector datasets, `ne_110m_land` and `ne_110m_lakes`
//! (<https://www.naturalearthdata.com/downloads/110m-physical-vectors/>).
//! Natural Earth data is in the **public domain** (no attribution required), and
//! the sources are only noted here for provenance.  The
//! `assets/world-110m-land.geojson` asset was produced from the official GeoJSON
//! releases (`nvkelso/natural-earth-vector`) by applying Douglas-Peucker
//! simplification at ~0.1 degrees and quantizing the coordinates to 2 decimal
//! places; latitudes span the full `[-90, 90]` range (Antarctica reaches the
//! south pole).
//!
//! Antarctica is the one landmass that crosses the antimeridian; it is kept as a
//! single ring that closes along the antimeridian / south-pole map edge
//! (`[180, -90] -> [-180, -90]`), so no path draws a seam through the map
//! interior.
//!
//! Lakes (the US Great Lakes, the Caspian, Baikal, …) are carried as **interior
//! rings** of the landmass polygon that contains them; the renderer punches them
//! out with the SVG even-odd fill rule so they show the water background.  See
//! `assets/generate_world_basemap.py` for the reproducible pipeline.
//!
//! ## Accuracy
//!
//! The outlines are fine (about 4500 points in total) and are **only** meant as
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

/// One landmass polygon: a closed exterior ring followed by any interior rings
/// (lakes) that are punched out of it.
///
/// All rings are closed `(longitude, latitude)` degree rings and every
/// coordinate is within `[-180, 180]` / `[-90, 90]`.
pub type LandPolygon = Vec<Vec<(f64, f64)>>;

/// The parsed basemap: closed landmass polygons of `(longitude, latitude)`
/// degrees (exterior ring first, lake holes after).
///
/// Parsed once from the embedded [`LAND_GEOJSON`] on first use.
static LAND_POLYGONS: OnceLock<Vec<LandPolygon>> = OnceLock::new();

/// Every landmass ring, flattened: exteriors **and** lake holes, in polygon
/// order (exterior first, then that polygon's holes).
static LAND_RINGS: OnceLock<Vec<Vec<(f64, f64)>>> = OnceLock::new();

/// The landmass polygons of the embedded basemap (see [`LAND_POLYGONS`]).
///
/// Parse errors are impossible for the embedded, build-time asset; should the
/// asset ever be malformed the map simply renders without a basemap rather than
/// panicking.
pub fn land_polygons() -> &'static [LandPolygon] {
    LAND_POLYGONS
        .get_or_init(|| parse_land_polygons(LAND_GEOJSON))
        .as_slice()
}

/// Every landmass ring of the embedded basemap (see [`LAND_RINGS`]).
///
/// Exteriors **and** lake holes, flattened into one list in polygon order.  Parse
/// errors are impossible for the embedded, build-time asset; should the asset
/// ever be malformed the map simply renders without a basemap rather than
/// panicking.
pub fn land_rings() -> &'static [Vec<(f64, f64)>] {
    LAND_RINGS
        .get_or_init(|| {
            land_polygons()
                .iter()
                .flat_map(|polygon| polygon.iter().cloned())
                .collect()
        })
        .as_slice()
}

/// Parse the embedded GeoJSON `FeatureCollection` into landmass polygons.
///
/// A GeoJSON ring order is exterior first, then interior (lake) rings; the
/// order is preserved here so the renderer can punch the interiors out with the
/// even-odd fill rule.
fn parse_land_polygons(json: &str) -> Vec<LandPolygon> {
    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let mut polygons = Vec::new();
    let Some(features) = value.get("features").and_then(|f| f.as_array()) else {
        return polygons;
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
            "Polygon" => push_polygon(&mut polygons, coordinates),
            "MultiPolygon" => {
                if let Some(polygons_json) = coordinates.as_array() {
                    for polygon in polygons_json {
                        push_polygon(&mut polygons, polygon);
                    }
                }
            }
            _ => {}
        }
    }
    polygons
}

/// Append one GeoJSON polygon (exterior + optional interior rings) as a
/// [`LandPolygon`], skipping malformed rings.
fn push_polygon(polygons: &mut Vec<LandPolygon>, coordinates: &serde_json::Value) {
    let Some(rings_json) = coordinates.as_array() else {
        return;
    };
    let mut rings = Vec::new();
    for ring_json in rings_json {
        push_ring(&mut rings, ring_json);
    }
    // A polygon with no usable ring at all is dropped.
    if rings.is_empty() {
        return;
    }
    polygons.push(rings);
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

/// Format an SVG path `d` attribute for a polygon: its exterior ring followed by
/// any interior (lake) rings as additional subpaths.
fn polygon_path(polygon: &[Vec<(f64, f64)>], width: f64, height: f64) -> String {
    let mut d = String::new();
    for ring in polygon {
        d.push_str(&ring_path(ring, width, height));
    }
    d
}

/// Render the landmass outlines as an SVG `<g>` group.
///
/// Each polygon is one `<path>`: the exterior ring followed by any interior
/// rings, which the `evenodd` fill rule punches out so lakes show the water
/// background through the land.  `fill`/`stroke` are plain CSS colours; the
/// group carries the `basemap` class so the report stylesheet can theme it.
pub fn continent_layer(width: u32, height: u32, fill: &str, stroke: &str) -> String {
    let (w, h) = (width as f64, height as f64);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "<g class=\"basemap\" fill=\"{fill}\" fill-rule=\"evenodd\" stroke=\"{stroke}\" \
         stroke-width=\"0.5\" stroke-linejoin=\"round\">"
    );
    for polygon in land_polygons() {
        let _ = writeln!(out, "  <path d=\"{}\"/>", polygon_path(polygon, w, h));
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
        // One `<path>` per landmass polygon; lake holes are extra subpaths of
        // the enclosing polygon rather than separate paths.
        assert_eq!(layer.matches("<path").count(), land_polygons().len());
        assert!(layer.contains("fill-rule=\"evenodd\""), "{layer}");
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

    /// No ring draws a spurious meridian inside the map.
    ///
    /// A near-vertical segment (large latitude change at almost constant
    /// longitude) is only legitimate along the left/right map edge, i.e. within
    /// ~1 degree of the antimeridian.  Anything else is a closure seam drawn
    /// through the map interior — the bug where Antarctica was split at ~59W
    /// instead of at the antimeridian, leaving straight segments from the south
    /// pole up to the peninsula.
    #[test]
    fn no_ring_draws_an_interior_meridian() {
        for ring in land_rings() {
            for pair in ring.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                let dlat = (a.1 - b.1).abs();
                let dlon = (a.0 - b.0).abs();
                if dlat > 20.0 && dlon < 5.0 {
                    let lon = (a.0 + b.0) / 2.0;
                    assert!(
                        (lon.abs() - 180.0).abs() < 1.0,
                        "interior meridian at lon {lon} from {a:?} to {b:?}"
                    );
                }
            }
        }
    }

    /// Antarctica is a single split-free ring that closes along the
    /// antimeridian / south-pole edge and reaches the pole.
    #[test]
    fn antarctica_closes_on_the_map_edge() {
        let antarctica: Vec<_> = land_rings()
            .iter()
            .filter(|ring| ring.iter().any(|(_, lat)| *lat <= -84.0))
            .collect();
        assert!(!antarctica.is_empty(), "no Antarctic ring");
        assert!(
            antarctica
                .iter()
                .any(|ring| ring.iter().any(|(_, lat)| *lat == -90.0)),
            "Antarctica must reach the south pole"
        );
        // Any latitude drop of >20 degrees must happen at the map edge, never on
        // an interior meridian.
        for ring in &antarctica {
            for pair in ring.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                if (a.1 - b.1).abs() > 20.0 {
                    let lon = (a.0 + b.0) / 2.0;
                    assert!(
                        (lon.abs() - 180.0).abs() < 1.0,
                        "Antarctic closure meridian at lon {lon}: {a:?} -> {b:?}"
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
    fn parse_land_polygons_handles_invalid_json() {
        assert!(parse_land_polygons("not json").is_empty());
        assert!(parse_land_polygons("{}").is_empty(), "no features key");
        assert!(
            parse_land_polygons(r#"{"features": 42}"#).is_empty(),
            "features is not an array"
        );
    }

    /// Polygon and MultiPolygon exteriors become polygons, together with their
    /// interior (lake) rings; unknown geometry types and features without
    /// geometry are skipped.
    #[test]
    fn parse_land_polygons_reads_exteriors_and_interiors() {
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
        let polygons = parse_land_polygons(json);
        // One Polygon (exterior + one lake) + two MultiPolygon polygons = three.
        assert_eq!(polygons.len(), 3, "{polygons:?}");
        assert_eq!(polygons[0].len(), 2, "the lake hole is kept");
        for polygon in &polygons {
            for ring in polygon {
                assert_eq!(ring.first(), ring.last(), "ring closed");
            }
        }
    }

    /// The embedded asset keeps the interior (lake) rings: the Caspian and the
    /// Great Lakes are punched out of their landmasses.
    #[test]
    fn embedded_asset_has_lake_holes() {
        let holes: Vec<_> = land_polygons()
            .iter()
            .flat_map(|polygon| polygon.iter().skip(1))
            .collect();
        assert!(holes.len() >= 20, "only {} lake holes", holes.len());

        let has_lake = |lon: f64, lat: f64| {
            holes.iter().any(|ring| {
                ring.iter()
                    .any(|(rl, rt)| (rl - lon).abs() < 3.0 && (rt - lat).abs() < 3.0)
            })
        };
        // The five Great Lakes and the (already-present) Caspian.
        for (name, lon, lat) in [
            ("Superior", -87.5, 47.7),
            ("Michigan", -87.0, 44.0),
            ("Huron", -82.4, 44.8),
            ("Erie", -81.2, 42.2),
            ("Ontario", -77.7, 43.6),
            ("Caspian", 51.0, 42.0),
        ] {
            assert!(has_lake(lon, lat), "missing lake hole: {name}");
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
