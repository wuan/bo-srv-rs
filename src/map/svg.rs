//! SVG rendering of the local-query maps for the servicelog report (issue #28).
//!
//! Two kinds of map live here:
//!
//! * [`render_local_svg`] — a standalone, value-range-normalised scatter of the
//!   raw UTM tile indices (no basemap), for `--format svg`.
//! * [`render_world_svg`] / [`render_world_map_svg`] — light-themed equirectangular
//!   world maps with the Natural Earth basemap (see [`crate::map::world`]) and
//!   the statistics aggregated onto the shared 5-degree raster (see
//!   [`crate::map::ascii`]).
//!
//! The map functions consume [`ServiceLogStats`] / [`LocalQuery`] from
//! [`crate::stats`].

use std::fmt::Write as _;

use crate::map::ascii::{AsciiWorldMap, WORLD_COLS, WORLD_ROWS};
use crate::stats::{LocalQuery, ServiceLogStats};

/// Escape the five XML/HTML metacharacters so arbitrary labels (country, city)
/// can be embedded safely in SVG/HTML.
pub(crate) fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

// --- Standalone local-query scatter (`--format svg`) -----------------------

/// Render the local query overlay as a minimal SVG world scatter, one point
/// per local request, scaled into the SVG viewport.
///
/// This is a **standalone** helper: it plots the raw UTM `(x, y)` values
/// (normalised to the value range, not projected onto a geographic world map),
/// which is enough to visualise query clusters without a map dependency.
pub fn render_local_svg(stats: &ServiceLogStats, width: u32, height: u32) -> String {
    let margin = 10.0;
    let points = &stats.local_queries;
    let (min_x, max_x, min_y, max_y) = bounds(points);

    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
         viewBox=\"0 0 {width} {height}\">\n"
    ));
    svg.push_str(&format!(
        "  <rect width=\"{width}\" height=\"{height}\" fill=\"#101820\"/>\n"
    ));
    svg.push_str(&format!(
        "  <text x=\"8\" y=\"16\" fill=\"#e0e0e0\" font-family=\"monospace\" font-size=\"12\">\
         local queries: {}</text>\n",
        points.len()
    ));

    for q in points {
        let x = scale(q.x as f64, min_x, max_x, margin, width as f64 - margin);
        // Invert y so north is up.
        let y = scale(q.y as f64, min_y, max_y, height as f64 - margin, margin);
        svg.push_str(&format!(
            "  <circle cx=\"{x:.2}\" cy=\"{y:.2}\" r=\"2\" fill=\"#ff6d3f\" fill-opacity=\"0.6\"/>\n"
        ));
    }
    svg.push_str("</svg>\n");
    svg
}

/// The min/max bounds of the points, or a degenerate `(0, 1)` box when empty.
fn bounds(points: &[LocalQuery]) -> (f64, f64, f64, f64) {
    if points.is_empty() {
        return (0.0, 1.0, 0.0, 1.0);
    }
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for q in points {
        min_x = min_x.min(q.x as f64);
        max_x = max_x.max(q.x as f64);
        min_y = min_y.min(q.y as f64);
        max_y = max_y.max(q.y as f64);
    }
    (min_x, max_x, min_y, max_y)
}

/// Map `value` from `[min, max]` onto `[out_min, out_max]`; a degenerate input
/// range maps to the midpoint so no division by zero occurs.
fn scale(value: f64, min: f64, max: f64, out_min: f64, out_max: f64) -> f64 {
    if (max - min).abs() < f64::EPSILON {
        return (out_min + out_max) / 2.0;
    }
    out_min + (value - min) / (max - min) * (out_max - out_min)
}

// --- Geographic SVG world maps (HTML report) -------------------------------

/// The water / land / coast colours of the light basemap.
pub(crate) const WATER_FILL: &str = "#f5f6f7";
/// Light-gray land fill for the basemap (see [`WATER_FILL`]).
pub(crate) const LAND_FILL: &str = "#d9dde1";
/// Coastline stroke for the basemap.
const COAST_STROKE: &str = "#aeb6bd";
/// Graticule colour (equator/prime meridian on the light basemap).
const GRATICULE_STROKE: &str = "#dde1e5";

/// The `(R, G, B)` colour ramp for the shaded squares, light to dense.
///
/// A square's shade is keyed to its **absolute** query count (see
/// [`square_colour`]) so the two separate maps are directly comparable and a
/// tile with a single query is never rendered as a hot spot.
///
/// The eight steps run a sequential yellow -> orange -> red -> dark-red "heat"
/// progression (ColorBrewer `YlOrRd`-inspired but ending in a deep red rather
/// than a magenta-pink, and with the pale end kept just saturated enough to stay
/// distinct over the light basemap).
pub(crate) const SQUARE_RAMP: [(u8, u8, u8); 8] = [
    (255, 242, 168), // pale yellow   (1 query)
    (255, 221, 82),  // yellow
    (255, 190, 26),  // amber
    (250, 144, 16),  // orange
    (239, 98, 18),   // dark orange
    (221, 58, 22),   // red-orange
    (191, 26, 22),   // red
    (143, 13, 18),   // deep red      (>= SQUARE_RAMP_MAX)
];

/// The per-tile query count that saturates the ramp: `1` maps to the palest
/// shade and `>= SQUARE_RAMP_MAX` to the deepest.  The eight shades split
/// `1..=SQUARE_RAMP_MAX` roughly geometrically (each step ~1.5x the previous
/// count) so low counts stay visually distinct over the light basemap.
const SQUARE_RAMP_MAX: u64 = 32;

/// The SVG `fill-opacity` of the shaded cells.
///
/// High enough that the eight ramp shades stay distinguishable over the light
/// basemap (the deep reds would otherwise wash out toward grey), yet below 1.0
/// so the land outline still shows through.
pub(crate) const SQUARE_FILL_OPACITY: &str = "0.7";

/// Pick a ramp colour for an absolute `count`.
///
/// The count is split into [`SQUARE_RAMP`]'s eight shades with a roughly
/// geometric scale (`1`, `2`, `3`, `4-5`, `6-8`, `9-13`, `14-21`, `>=22`) and
/// capped at [`SQUARE_RAMP_MAX`].  This is deliberately absolute rather than
/// normalised against the map's maximum: the same tile count reads the same on
/// both the background and interactive maps, and a lone query on an otherwise
/// empty map is not painted the densest shade.  A `count` of `0` is never drawn,
/// but maps to the palest shade defensively.
fn square_colour(count: u64) -> String {
    let count = count.min(SQUARE_RAMP_MAX);
    let bucket = match count {
        0 | 1 => 0,
        2 => 1,
        3 => 2,
        4..=5 => 3,
        6..=8 => 4,
        9..=13 => 5,
        14..=21 => 6,
        _ => 7,
    };
    let (r, g, b) = SQUARE_RAMP[bucket];
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Render the local-query statistics of one category as shaded cells of the
/// single underlying `WORLD_ROWS x WORLD_COLS` 5-degree raster.
///
/// Every query's footprint is added to one shared [`AsciiWorldMap`] grid: a
/// `data_area` of `n` degrees covers `n / 5` cells per side starting at the
/// tile origin, so a `data_area=10` query increments the four cells
/// `(x, y)`, `(x+1, y)`, `(x, y+1)`, `(x+1, y+1)` (see
/// [`AsciiWorldMap::add_local_query`]).  Only that one raster is drawn, so
/// overlapping queries simply accumulate into the same cells.
///
/// Each non-empty cell becomes a semi-transparent rectangle shaded by its count
/// (see [`square_colour`]) so the basemap stays visible underneath.  The
/// south-up raster rows are flipped for the north-up SVG.  `label` names the
/// group for the SVG `<title>`.
fn raster_layer(raster: &AsciiWorldMap, width: f64, height: f64, label: &str) -> String {
    let cell_w = width / WORLD_COLS as f64;
    let cell_h = height / WORLD_ROWS as f64;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "<g class=\"squares\" data-set=\"{}\" fill-opacity=\"{SQUARE_FILL_OPACITY}\">\n  <title>{}</title>\n",
        escape_html(label),
        escape_html(&format!("{label}: {} queries", raster.queries())),
    );
    for row in 0..WORLD_ROWS {
        // The raster is south-up; flip so row 0 draws at the top.
        let north_up_row = WORLD_ROWS - 1 - row;
        for col in 0..WORLD_COLS {
            let count = raster.count(col, row);
            if count == 0 {
                continue;
            }
            let x = col as f64 * cell_w;
            let y = north_up_row as f64 * cell_h;
            let fill = square_colour(count);
            let _ = writeln!(
                out,
                "  <rect x=\"{x:.2}\" y=\"{y:.2}\" width=\"{cell_w:.2}\" height=\"{cell_h:.2}\" \
                 fill=\"{fill}\"><title>{label}: {count} queries</title></rect>",
            );
        }
    }
    out.push_str("</g>\n");
    out
}

/// Render a single geographic SVG world map for one query category.
///
/// The map is light-themed: a very light gray ocean, light gray continents and
/// a faint graticule, with the category's statistics drawn on top as
/// semi-transparent shaded cells of the shared 5-degree raster (see
/// [`raster_layer`]).  Unlike [`render_local_svg`] (which normalises the raw UTM
/// tile indices into an abstract scatter), the raster cells are georeferenced by
/// their 5-degree footprint.
///
/// `queries` is the category to plot and `label` names it in the SVG title and
/// `aria-label`.
pub fn render_world_map_svg(
    queries: &[LocalQuery],
    label: &str,
    width: u32,
    height: u32,
) -> String {
    let (w, h) = (width as f64, height as f64);
    let raster = AsciiWorldMap::from_local_queries(queries);
    let mut svg = String::new();
    let _ = writeln!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" class=\"worldmap\" width=\"{width}\" \
         height=\"{height}\" viewBox=\"0 0 {width} {height}\" role=\"img\" \
         aria-label=\"{}\">",
        escape_html(&format!("World map of {label} local queries"))
    );
    // Very light gray ocean background.
    let _ = writeln!(
        svg,
        "  <rect x=\"0\" y=\"0\" width=\"{width}\" height=\"{height}\" fill=\"{WATER_FILL}\"/>"
    );
    // Faint graticule (equator + prime meridian) for orientation.
    let _ = write!(
        svg,
        "  <g class=\"graticule\" stroke=\"{GRATICULE_STROKE}\" stroke-width=\"0.5\">\n\
         \x20   <line x1=\"0\" y1=\"{}\" x2=\"{width}\" y2=\"{}\"/>\n\
         \x20   <line x1=\"{}\" y1=\"0\" x2=\"{}\" y2=\"{height}\"/>\n\
         \x20 </g>\n",
        height / 2,
        height / 2,
        width / 2,
        width / 2,
    );
    svg.push_str(&crate::map::world::continent_layer(
        width,
        height,
        LAND_FILL,
        COAST_STROKE,
    ));
    svg.push_str(&raster_layer(&raster, w, h, label));
    svg.push_str("</svg>\n");
    svg
}

/// Render the separate offline (background) and interactive world maps used by
/// the HTML report.
///
/// Returns `(offline_svg, interactive_svg)`; each map is a standalone
/// [`render_world_map_svg`] so the two query categories never overlap visually.
pub fn render_world_svg(stats: &ServiceLogStats, width: u32, height: u32) -> (String, String) {
    let offline: Vec<LocalQuery> = stats
        .local_queries
        .iter()
        .copied()
        .filter(LocalQuery::is_offline)
        .collect();
    let interactive: Vec<LocalQuery> = stats
        .local_queries
        .iter()
        .copied()
        .filter(LocalQuery::is_interactive)
        .collect();
    (
        render_world_map_svg(&offline, "background (offline)", width, height),
        render_world_map_svg(&interactive, "interactive", width, height),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::{aggregate, parse_content};

    const ISSUE_SAMPLE: &str = "\
08:44:50.596\tUS\tSun Prairie\t\t\tA\t352\t0\t10\t25000\t0\t0\t-\t-\t-\t0.036\n\
08:44:50.637\tSE\tGothenburg\t\t\tA\t352\t0\t10\t5000\t-1\t0\t4\t13\t5\t0.000\n\
08:44:50.655\tRO\tBucharest\t\t\tA\t352\t0\t10\t5000\t-1\t0\t5\t8\t5\t0.039\n\
08:44:51.106\tDE\tBerlin\t\t\t\tA\t352\t0\t60\t5000\t-1\t0\t2\t10\t5\t0.000\n\
08:44:51.238\tDE\tUlm\t\t\t\tA\t352\t0\t10\t5000\t-1\t0\t1\t9\t5\t0.000\n\
08:44:51.277\tIT\tVicenza\t\t\t\tA\t352\t0\t10\t5000\t-1\t0\t2\t9\t5\t0.000\n";

    /// Count the raster `<rect>`s in a rendered map (excluding the background
    /// rectangle that fills the whole viewport).
    fn raster_rects(svg: &str) -> usize {
        let cells = svg.split("<g class=\"squares\"").nth(1).unwrap_or("");
        cells.matches("<rect ").count()
    }

    #[test]
    fn local_svg_lists_one_circle_per_query() {
        let outcome = parse_content(ISSUE_SAMPLE);
        let stats = aggregate(&outcome.rows, 10);
        let svg = render_local_svg(&stats, 640, 320);
        assert_eq!(svg.matches("<circle").count(), stats.local_queries.len());
        let svg = render_local_svg(&ServiceLogStats::default(), 100, 100);
        assert_eq!(svg.matches("<circle").count(), 0);
    }

    /// `render_world_svg` lays a light continent basemap and the single shared
    /// 5-degree raster in each of the two separate offline/interactive maps.
    #[test]
    fn world_svg_has_basemap_and_split_squares() {
        let outcome = parse_content(ISSUE_SAMPLE);
        let stats = aggregate(&outcome.rows, 10);
        let (offline, interactive) = render_world_svg(&stats, 960, 480);

        for svg in [&offline, &interactive] {
            assert!(svg.starts_with("<svg"), "{svg}");
            assert!(svg.contains("class=\"basemap\""), "{svg}");
            assert_eq!(
                svg.matches("<path").count(),
                crate::map::world::land_rings().len()
            );
            // Light basemap: very light gray water, light gray land.
            assert!(svg.contains(WATER_FILL), "{svg}");
            assert!(svg.contains(LAND_FILL), "{svg}");
            // Cells are semi-transparent so the basemap stays visible.
            assert!(
                svg.contains(&format!("fill-opacity=\"{SQUARE_FILL_OPACITY}\"")),
                "{svg}"
            );
        }

        // Four offline local queries, each on a distinct 5-degree cell (one raster
        // rectangle each); the one interactive query on its own map.
        assert!(
            offline.contains("data-set=\"background (offline)\""),
            "{offline}"
        );
        assert!(
            interactive.contains("data-set=\"interactive\""),
            "{interactive}"
        );
        assert!(!offline.contains("<circle"), "no circle markers remain");
        assert_eq!(raster_rects(&interactive), 1, "{interactive}");
        assert_eq!(raster_rects(&offline), 4, "{offline}");
    }

    /// The square shade scales with the absolute raster-cell count, and all eight
    /// ramp shades are reachable.
    #[test]
    fn square_colour_is_an_eight_shade_ramp() {
        // The ramp has eight, perceptually separated shades.
        assert_eq!(SQUARE_RAMP.len(), 8);

        // A lone query is the palest shade; the deepest shade is reached at (and
        // beyond) SQUARE_RAMP_MAX.
        assert_eq!(square_colour(1), "#fff2a8");
        assert_eq!(square_colour(SQUARE_RAMP_MAX), "#8f0d12");
        assert_eq!(square_colour(SQUARE_RAMP_MAX + 100), "#8f0d12");

        // Zero (only reachable for a defensive empty tile) is the palest shade too.
        assert_eq!(square_colour(0), "#fff2a8");

        // Every shade in the ramp is produced by some count in 1..=SQUARE_RAMP_MAX,
        // and the sequence is strictly darkening (no two adjacent counts collapse).
        let mut shades: Vec<String> = Vec::new();
        for count in 1..=SQUARE_RAMP_MAX {
            let colour = square_colour(count);
            if shades.last() != Some(&colour) {
                shades.push(colour);
            }
        }
        assert_eq!(
            shades.len(),
            8,
            "all eight shades must be reachable: {shades:?}"
        );
        let expected: Vec<String> = SQUARE_RAMP
            .iter()
            .map(|(r, g, b)| format!("#{r:02x}{g:02x}{b:02x}"))
            .collect();
        assert_eq!(shades, expected);
    }

    /// A `data_area=10` query fills a 2x2 block of the 5-degree raster, so its
    /// footprint lands on four distinct cells.
    #[test]
    fn raster_layer_increments_the_data_area_footprint() {
        let make = |x, y, data_area| LocalQuery {
            x,
            y,
            data_area,
            grid_baselength: 5000,
            minute_length: 10,
        };
        // A single data_area=10 query covers four 5-degree cells.
        let one = AsciiWorldMap::from_local_queries(&[make(1, 1, 10)]);
        assert_eq!(one.queries(), 1);
        assert_eq!(one.total(), 4, "2x2 footprint");

        // Two identical data_area=5 queries land on the same single cell, so they
        // accumulate into one cell of count 2 (not two separate squares).
        let two = AsciiWorldMap::from_local_queries(&[make(1, 1, 5), make(1, 1, 5)]);
        assert_eq!(two.total(), 2);
        assert_eq!(two.maximum(), 2);

        // The rendered map draws one rectangle per non-empty cell.
        let svg = render_world_map_svg(&[make(1, 1, 10)], "test", 960, 480);
        assert_eq!(raster_rects(&svg), 4, "{svg}");
    }

    /// An empty report still renders empty world maps without panicking.
    #[test]
    fn world_svg_handles_no_local_queries() {
        let (offline, interactive) = render_world_svg(&ServiceLogStats::default(), 960, 480);
        for svg in [&offline, &interactive] {
            assert!(svg.contains("class=\"basemap\""));
            assert_eq!(svg.matches("<rect ").count(), 1, "only the background rect");
        }
    }

    #[test]
    fn scale_handles_degenerate_range() {
        assert_eq!(scale(5.0, 5.0, 5.0, 0.0, 10.0), 5.0);
        assert_eq!(scale(0.0, 0.0, 10.0, 0.0, 100.0), 0.0);
        assert_eq!(scale(10.0, 0.0, 10.0, 0.0, 100.0), 100.0);
    }
}
