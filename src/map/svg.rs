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
/// A square's shade is chosen from its count **relative to the map's densest
/// cell**: the range `1..=max` is split into eight equal buckets, so a map whose
/// maximum is `80` paints counts `1..=10` in shade 1, `11..=20` in shade 2, ...,
/// `71..=80` in shade 8.  Cells with a count of `0` are never drawn.
///
/// The eight steps run a sequential yellow -> orange -> red -> dark-red "heat"
/// progression (ColorBrewer `YlOrRd`-inspired but ending in a deep red rather
/// than a magenta-pink, and with the pale end kept just saturated enough to stay
/// distinct over the light basemap).
pub(crate) const SQUARE_RAMP: [(u8, u8, u8); 8] = [
    (255, 242, 168), // pale yellow   (lowest bucket)
    (255, 221, 82),  // yellow
    (255, 190, 26),  // amber
    (250, 144, 16),  // orange
    (239, 98, 18),   // dark orange
    (221, 58, 22),   // red-orange
    (191, 26, 22),   // red
    (143, 13, 18),   // deep red      (the densest cell)
];

/// The number of shades in [`SQUARE_RAMP`].
const SQUARE_SHADES: u64 = 8;

/// The SVG `fill-opacity` of the shaded cells.
///
/// High enough that the eight ramp shades stay distinguishable over the light
/// basemap (the deep reds would otherwise wash out toward grey), yet below 1.0
/// so the land outline still shows through.
pub(crate) const SQUARE_FILL_OPACITY: &str = "0.7";

/// Pick a ramp colour for a cell `count` given the map's densest cell `max`.
///
/// `1..=max` is divided into [`SQUARE_SHADES`] **equal** buckets and the shade
/// index is `ceil(count * 8 / max) - 1`, so all eight shades span the observed
/// range (e.g. `max=80`: `1..=10` -> shade 1, ..., `71..=80` -> shade 8).  The
/// densest cell always gets the deepest shade; a `count` of `0` is never drawn.
///
/// This makes the shading **relative to the map's own maximum** so the full
/// ramp is always used across each map, rather than an absolute count scale
/// that saturates well below the observed maximum.
fn square_colour(count: u64, max: u64) -> String {
    debug_assert!(count > 0, "zero-count cells are not drawn");
    let bucket = if count >= max {
        SQUARE_RAMP.len() - 1
    } else {
        // `ceil(count * 8 / max) - 1`, clamped to the ramp.
        let scaled = (count * SQUARE_SHADES).div_ceil(max.max(1));
        (scaled.saturating_sub(1) as usize).min(SQUARE_RAMP.len() - 1)
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
/// relative to the map's densest cell (see [`square_colour`]) so the basemap
/// stays visible underneath.  The south-up raster rows are flipped for the
/// north-up SVG.  `label` names the group for the SVG `<title>`.
fn raster_layer(raster: &AsciiWorldMap, width: f64, height: f64, label: &str) -> String {
    let cell_w = width / WORLD_COLS as f64;
    let cell_h = height / WORLD_ROWS as f64;
    // The densest cell anchors the eight shades (1..=max -> eight equal buckets).
    let max = raster.maximum();
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
            let fill = square_colour(count, max);
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

/// Render a compact shade legend in the map's lower-left (over the Antarctic
/// band): a row of the eight [`SQUARE_RAMP`] swatches labelled `1 .. max`, where
/// `max` is the map's densest-cell query count.
///
/// Because a cell's shade is relative to the map's own maximum, the legend
/// states the absolute count range so the scale is unambiguous.  A `max` of `0`
/// (no cells) renders nothing.
fn legend_layer(max: u64, _width: f64, height: f64) -> String {
    if max == 0 {
        return String::new();
    }
    const SWATCH_W: f64 = 18.0;
    const SWATCH_H: f64 = 12.0;
    const GAP: f64 = 2.0;
    const PAD: f64 = 6.0;
    const FONT: f64 = 10.0;
    // The value row needs room for the font's descenders below its baseline.
    const LINE_GAP: f64 = 4.0;
    const TEXT_H: f64 = FONT + 2.0;

    let swatches_w = SQUARE_RAMP.len() as f64 * SWATCH_W + (SQUARE_RAMP.len() - 1) as f64 * GAP;
    let box_w = swatches_w + 2.0 * PAD;

    // Vertical layout, top to bottom: caption, swatches, value row, each with
    // padding so nothing reaches the box edge.
    let caption_y = PAD + TEXT_H; // text baseline
    let swatch_y = caption_y + LINE_GAP;
    let value_y = swatch_y + SWATCH_H + LINE_GAP + FONT; // text baseline
    let box_h = value_y + PAD;

    // Bottom-left corner, inset from the map edges.
    let box_x = 10.0;
    let box_y = height - box_h - 10.0;
    let caption_y = box_y + caption_y;
    let swatch_y = box_y + swatch_y;
    let value_y = box_y + value_y;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "<g class=\"map-legend\" font-family=\"system-ui, sans-serif\">"
    );
    let _ = writeln!(
        out,
        "  <rect x=\"{box_x:.1}\" y=\"{box_y:.1}\" width=\"{box_w:.1}\" height=\"{box_h:.1}\" \
         rx=\"4\" fill=\"#ffffff\" fill-opacity=\"0.85\" stroke=\"{COAST_STROKE}\" \
         stroke-width=\"0.5\"/>"
    );
    let _ = writeln!(
        out,
        "  <text x=\"{:.1}\" y=\"{caption_y:.1}\" font-size=\"{FONT}\" fill=\"#24313b\">queries per cell</text>",
        box_x + PAD
    );
    for (i, (r, g, b)) in SQUARE_RAMP.iter().enumerate() {
        let x = box_x + PAD + i as f64 * (SWATCH_W + GAP);
        // The swatches are drawn at full opacity (the on-map cells are blended
        // with the basemap), so the legend shows the pure ramp colours.
        let _ = write!(
            out,
            "  <rect x=\"{x:.1}\" y=\"{swatch_y:.1}\" width=\"{SWATCH_W}\" height=\"{SWATCH_H}\" \
             fill=\"#{r:02x}{g:02x}{b:02x}\"/>"
        );
        let _ = writeln!(out);
    }
    let _ = writeln!(
        out,
        "  <text x=\"{:.1}\" y=\"{value_y:.1}\" font-size=\"{FONT}\" fill=\"#5f707d\">1</text>",
        box_x + PAD
    );
    let _ = writeln!(
        out,
        "  <text x=\"{:.1}\" y=\"{value_y:.1}\" font-size=\"{FONT}\" fill=\"#5f707d\" \
         text-anchor=\"end\">{max}</text>",
        box_x + PAD + swatches_w
    );
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
/// their 5-degree footprint.  A small [`legend_layer`] in the lower-left maps the
/// eight shades to the count range `1..=max` so the relative scale is explicit.
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
    // A small legend in the lower-left explains the relative shade scale and
    // states the map's maximum cell count.
    svg.push_str(&legend_layer(raster.maximum(), w, h));
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
        // Only the shaded cells: stop before the `map-legend` group (whose
        // swatches are also `<rect>`s).
        let cells = svg.split("<g class=\"squares\"").nth(1).unwrap_or("");
        let cells = cells.split("map-legend").next().unwrap_or(cells);
        cells.matches("<rect ").count()
    }

    /// The numeric value of `name="..."` on the first `tag` in `svg`.
    fn attr(svg: &str, tag: &str, name: &str) -> f64 {
        let seg = svg.split(tag).nth(1).unwrap_or("");
        let key = format!("{name}=\"");
        let rest = seg.split_once(&key).unwrap_or(("", "")).1;
        let value = rest.split('"').next().unwrap_or("");
        value.parse().unwrap_or(f64::NAN)
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

    /// The square shade uses eight **equal** buckets spanning `1..=max` (the
    /// densest cell of the map), so all eight shades are reachable and equidistant.
    #[test]
    fn square_colour_is_an_eight_shade_ramp() {
        // The ramp has eight, perceptually separated shades.
        assert_eq!(SQUARE_RAMP.len(), 8);
        let ramp_hex: Vec<String> = SQUARE_RAMP
            .iter()
            .map(|(r, g, b)| format!("#{r:02x}{g:02x}{b:02x}"))
            .collect();

        // The densest cell is always the deepest shade; a single-count cell on a
        // large map is the palest.
        assert_eq!(square_colour(80, 80), ramp_hex[7]);
        assert_eq!(square_colour(1, 80), ramp_hex[0]);
        assert_eq!(square_colour(1, 1_000_000), ramp_hex[0]);

        // max = 80 -> eight equal 10-wide buckets: 1..=10 -> shade 1, ..., 71..=80
        // -> shade 8.  This is the user-visible contract.
        let expected = [
            (1, 0),
            (5, 0),
            (10, 0),
            (11, 1),
            (20, 1),
            (21, 2),
            (30, 2),
            (31, 3),
            (40, 3),
            (41, 4),
            (50, 4),
            (51, 5),
            (60, 5),
            (61, 6),
            (70, 6),
            (71, 7),
            (75, 7),
            (80, 7),
        ];
        for (count, bucket) in expected {
            assert_eq!(
                square_colour(count, 80),
                ramp_hex[bucket],
                "count {count} with max 80 -> shade {}",
                bucket + 1
            );
        }

        // With enough distinct counts every shade is reachable on a max-80 map.
        let reachable: std::collections::BTreeSet<String> =
            (1..=80).map(|c| square_colour(c, 80)).collect();
        assert_eq!(
            reachable.len(),
            8,
            "all eight shades reachable: {reachable:?}"
        );
    }

    /// A small maximum still spreads its counts across the ramp and always puts the
    /// densest cell in the deepest shade.
    #[test]
    fn square_colour_handles_small_maxima() {
        let deepest = format!(
            "#{:02x}{:02x}{:02x}",
            SQUARE_RAMP[7].0, SQUARE_RAMP[7].1, SQUARE_RAMP[7].2
        );
        assert_eq!(square_colour(1, 1), deepest);
        assert_eq!(square_colour(5, 5), deepest);
        // The counts 1..=5 on a max-5 map spread over several (not just one) shades.
        let shades: std::collections::BTreeSet<String> =
            (1..=5).map(|c| square_colour(c, 5)).collect();
        assert!(shades.len() >= 3, "small maxima spread: {shades:?}");
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
            // No cells -> no legend.
            assert!(!svg.contains("map-legend"), "{svg}");
        }
    }

    /// The in-map legend lists all eight shades and the map's maximum count.
    #[test]
    fn map_legend_shows_all_shades_and_the_maximum() {
        // An empty map has no legend.
        assert!(legend_layer(0, 960.0, 480.0).is_empty());
        assert!(!legend_layer(0, 960.0, 480.0).contains("map-legend"));

        let legend = legend_layer(80, 960.0, 480.0);
        assert!(legend.contains("class=\"map-legend\""), "{legend}");
        assert!(legend.contains("queries per cell"), "{legend}");
        // Every ramp colour appears as a swatch.
        for (r, g, b) in SQUARE_RAMP {
            let hex = format!("#{r:02x}{g:02x}{b:02x}");
            assert!(legend.contains(&hex), "missing swatch {hex}: {legend}");
        }
        // The maximum and the minimum endpoints are labelled.
        assert!(legend.contains(">1</text>"), "{legend}");
        assert!(legend.contains(">80</text>"), "{legend}");

        // The value row must sit inside the containing box (regression guard:
        // the third line used to reach past the bottom edge).
        let box_y: f64 = attr(&legend, "rect", "y");
        let box_h: f64 = attr(&legend, "rect", "height");
        // The value texts are the `#5f707d`-filled ones; find their `y`.
        let value_ys: Vec<f64> = legend
            .lines()
            .filter(|l| l.contains("fill=\"#5f707d\""))
            .map(|l| attr(l, "text", "y"))
            .collect();
        assert_eq!(value_ys.len(), 2, "{legend}");
        for y in value_ys {
            assert!(
                y < box_y + box_h,
                "value baseline {y} reaches past the box bottom {}",
                box_y + box_h
            );
        }
    }

    /// The rendered world map embeds the legend with the densest cell's count.
    #[test]
    fn world_map_svg_includes_the_legend() {
        let make = |x, y| LocalQuery {
            x,
            y,
            data_area: 5,
            grid_baselength: 5000,
            minute_length: 10,
        };
        // Two queries on the same cell -> maximum 2.
        let svg = render_world_map_svg(&[make(1, 1), make(1, 1)], "test", 960, 480);
        assert!(svg.contains("class=\"map-legend\""), "{svg}");
        assert!(svg.contains(">2</text>"), "{svg}");
    }

    #[test]
    fn scale_handles_degenerate_range() {
        assert_eq!(scale(5.0, 5.0, 5.0, 0.0, 10.0), 5.0);
        assert_eq!(scale(0.0, 0.0, 10.0, 0.0, 100.0), 0.0);
        assert_eq!(scale(10.0, 0.0, 10.0, 0.0, 100.0), 100.0);
    }
}
