//! SVG world-map base layer for the servicelog HTML report (issue #28).
//!
//! The map is a plain equirectangular projection of a small, embedded set of
//! continent outlines: enough to give the local-query markers a geographic
//! frame without pulling in a GIS dependency or a large coastline shapefile.
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
//! ## Accuracy
//!
//! The outlines are hand-simplified (a few dozen points per landmass) and are
//! **only** meant as an orientation aid: they are not a survey-accurate basemap.
//! The local-query overlay uses the same projection, so a marker sits in the
//! right part of the right continent.

use std::fmt::Write as _;

/// The map viewport size of the standalone SVG (pixels).
pub const MAP_WIDTH: u32 = 960;
/// The map viewport height of the standalone SVG (pixels).
pub const MAP_HEIGHT: u32 = 480;

/// One landmass outline: a closed ring of `(longitude, latitude)` degrees.
///
/// Simplified representations of the continents.  The rings are deliberately
/// coarse; see the module docs.  Coastal detail is not required for orienting a
/// query overlay, so every ring is kept small enough to inline in the source.
pub const CONTINENT_OUTLINES: &[&[(f64, f64)]] = &[
    // Africa (including the Arabian peninsula is *not* merged; see Asia).
    &[
        (-17.0, 14.7),
        (-16.0, 19.0),
        (-10.0, 26.0),
        (-5.0, 31.5),
        (0.0, 35.5),
        (10.0, 37.0),
        (20.0, 32.5),
        (32.0, 31.5),
        (35.0, 23.0),
        (43.0, 12.0),
        (51.0, 12.0),
        (41.0, -1.0),
        (40.0, -15.0),
        (35.0, -24.0),
        (32.0, -30.0),
        (25.0, -34.0),
        (18.0, -34.5),
        (12.0, -18.0),
        (9.0, -1.0),
        (5.0, 5.0),
        (-8.0, 4.5),
        (-17.0, 14.7),
    ],
    // Eurasia (Europe + Asia), a single ring anchored at Iberia.
    &[
        (-10.0, 36.0),
        (-9.0, 43.0),
        (-2.0, 43.5),
        (-5.0, 48.5),
        (2.0, 51.0),
        (4.0, 53.0),
        (8.0, 54.0),
        (12.0, 55.5),
        (18.0, 55.0),
        (22.0, 59.0),
        (25.0, 60.5),
        (30.0, 60.0),
        (30.0, 66.0),
        (40.0, 67.0),
        (60.0, 70.0),
        (80.0, 73.0),
        (100.0, 77.0),
        (130.0, 73.0),
        (160.0, 70.0),
        (180.0, 66.0),
        (180.0, 60.0),
        (170.0, 60.0),
        (160.0, 54.0),
        (155.0, 50.0),
        (142.0, 46.0),
        (130.0, 43.0),
        (125.0, 38.0),
        (122.0, 30.0),
        (115.0, 22.0),
        (105.0, 20.0),
        (100.0, 13.0),
        (98.0, 8.0),
        (95.0, 16.0),
        (90.0, 22.0),
        (80.0, 15.0),
        (77.0, 8.0),
        (72.0, 21.0),
        (68.0, 24.0),
        (57.0, 25.0),
        (52.0, 24.0),
        (48.0, 29.0),
        (43.0, 30.0),
        (35.0, 36.0),
        (27.0, 37.0),
        (23.0, 38.0),
        (24.0, 40.5),
        (20.0, 40.0),
        (18.0, 42.5),
        (12.0, 45.0),
        (12.0, 44.0),
        (15.0, 40.0),
        (18.0, 40.0),
        (16.0, 38.0),
        (15.0, 37.0),
        (12.0, 38.0),
        (6.0, 43.0),
        (3.0, 43.0),
        (-1.5, 43.4),
        (-9.0, 38.5),
        (-10.0, 36.0),
    ],
    // North America.
    &[
        (-168.0, 66.0),
        (-160.0, 71.0),
        (-140.0, 70.0),
        (-125.0, 70.0),
        (-110.0, 69.0),
        (-95.0, 62.0),
        (-80.0, 62.0),
        (-70.0, 60.0),
        (-64.0, 58.0),
        (-56.0, 52.0),
        (-60.0, 47.0),
        (-66.0, 45.0),
        (-70.0, 43.0),
        (-74.0, 40.0),
        (-77.0, 35.0),
        (-81.0, 30.0),
        (-80.0, 25.0),
        (-84.0, 29.0),
        (-90.0, 29.0),
        (-97.0, 26.0),
        (-97.0, 22.0),
        (-92.0, 18.0),
        (-88.0, 16.0),
        (-92.0, 14.0),
        (-96.0, 15.5),
        (-105.0, 20.0),
        (-110.0, 23.0),
        (-114.0, 29.0),
        (-121.0, 34.0),
        (-124.0, 42.0),
        (-128.0, 51.0),
        (-136.0, 58.0),
        (-150.0, 60.0),
        (-158.0, 57.0),
        (-165.0, 60.0),
        (-168.0, 66.0),
    ],
    // South America.
    &[
        (-81.0, 7.0),
        (-77.0, 8.0),
        (-72.0, 12.0),
        (-62.0, 11.0),
        (-52.0, 5.0),
        (-50.0, -1.0),
        (-44.0, -3.0),
        (-35.0, -6.0),
        (-38.0, -13.0),
        (-40.0, -22.0),
        (-48.0, -26.0),
        (-54.0, -34.0),
        (-58.0, -38.0),
        (-62.0, -40.0),
        (-65.0, -45.0),
        (-68.0, -50.0),
        (-75.0, -52.0),
        (-73.0, -44.0),
        (-73.0, -37.0),
        (-71.0, -30.0),
        (-70.0, -20.0),
        (-76.0, -14.0),
        (-81.0, -6.0),
        (-81.0, 1.0),
        (-81.0, 7.0),
    ],
    // Australia.
    &[
        (114.0, -22.0),
        (114.0, -28.0),
        (115.0, -34.0),
        (122.0, -34.0),
        (130.0, -32.0),
        (138.0, -35.0),
        (145.0, -38.0),
        (150.0, -37.0),
        (153.0, -28.0),
        (146.0, -19.0),
        (142.0, -11.0),
        (136.0, -12.0),
        (130.0, -12.0),
        (127.0, -14.0),
        (122.0, -17.0),
        (114.0, -22.0),
    ],
    // Greenland.
    &[
        (-45.0, 60.0),
        (-52.0, 65.0),
        (-55.0, 70.0),
        (-60.0, 76.0),
        (-55.0, 82.0),
        (-40.0, 83.0),
        (-25.0, 82.0),
        (-20.0, 76.0),
        (-25.0, 70.0),
        (-38.0, 65.0),
        (-45.0, 60.0),
    ],
    // Madagascar.
    &[
        (43.0, -12.0),
        (50.0, -15.0),
        (50.0, -25.0),
        (45.0, -25.0),
        (43.0, -20.0),
        (43.0, -12.0),
    ],
    // Antarctica (a coarse band along the bottom edge).
    &[
        (-180.0, -70.0),
        (-120.0, -73.0),
        (-60.0, -63.0),
        (0.0, -70.0),
        (60.0, -67.0),
        (120.0, -66.0),
        (180.0, -70.0),
        (180.0, -85.0),
        (-180.0, -85.0),
        (-180.0, -70.0),
    ],
];

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

/// Render the continent outlines as an SVG `<g>` group.
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
    for ring in CONTINENT_OUTLINES {
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

    #[test]
    fn continent_layer_lists_every_outline() {
        let layer = continent_layer(360, 180, "#ccc", "#888");
        assert!(layer.starts_with("<g class=\"basemap\""));
        assert!(layer.trim_end().ends_with("</g>"));
        assert_eq!(layer.matches("<path").count(), CONTINENT_OUTLINES.len());
    }

    #[test]
    fn every_ring_is_closed_and_in_range() {
        for ring in CONTINENT_OUTLINES {
            assert!(ring.len() >= 3, "ring too small");
            assert_eq!(ring.first(), ring.last(), "ring is not closed");
            for (lon, lat) in *ring {
                assert!((-180.0..=180.0).contains(lon), "lon {lon} out of range");
                assert!((-90.0..=90.0).contains(lat), "lat {lat} out of range");
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
}
