//! Statistics from the daily `servicelog_YYYY-MM-DD` usage-log files.
//!
//! The service writes one tab-separated row per successful grid request (see
//! [`crate::service_log`]).  This module parses those files back into rows and
//! aggregates them into daily statistics:
//!
//! * **total requests** — the number of parsed rows (optionally split by the
//!   local / global / region request flavours);
//! * **top countries** — requests grouped by the GeoIP country column;
//! * **top cities** — requests grouped by the GeoIP city column;
//! * **top client versions** — requests grouped by the parsed
//!   `bo-android-<n>` version;
//! * **local query overlay** — the `(x, y, raster baselength)` grid locations
//!   of the local requests, for plotting on a world map (`render_ascii_maps`
//!   for the ASCII raster, `render_world_svg` for the geographic SVG maps).
//!
//! Besides the text, JSON and SVG renderers this module provides
//! [`render_html`], a standalone static HTML report (issue #28) that embeds the
//! statistics tables and two light-themed SVG world maps (background/offline and
//! interactive) in a single self-contained document.
//!
//! ## Parsing
//!
//! The `city` column is tab-padded (see [`crate::service_log::pad_city`]), so a
//! naive `split('\t')` yields empty segments.  [`parse_row`] therefore splits on
//! tabs and **drops empty segments**, which recovers exactly the 14 logical
//! fields.  A row that does not yield 14 fields is reported as malformed and
//! skipped (the parser never panics on a truncated or hand-edited file).
//!
//! `-` is the placeholder for "unknown" in the country/city/local columns and
//! is normalised to `None`, so the top lists never contain a `-` bucket.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

/// The number of logical fields in a servicelog row.
pub const SERVICELOG_FIELDS: usize = 14;

/// The default `N` of the "top N" lists.
pub const DEFAULT_TOP_N: usize = 10;

/// A parsed servicelog row.
///
/// `country`, `city` and `version` are `None` when the row carries the `-`
/// placeholder (or, for the version, an unparsable value).  `country`/`city`
/// are kept as the raw strings from the log; `version` is parsed to its integer
/// `versionCode`.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceLogRow {
    /// Time of day as written (`HH:MM:SS.nnn`).
    pub timestamp: String,
    pub country: Option<String>,
    pub city: Option<String>,
    /// `A` for Android, `-` for anything else.
    pub platform: String,
    /// The parsed `bo-android-<n>` version, else `None`.
    pub version: Option<i64>,
    pub minute_offset: i64,
    pub minute_length: i64,
    pub grid_baselength: i64,
    /// `0` global, clamped region for the region grid, `-1` local.
    pub region: i64,
    pub count_threshold: i64,
    /// Local-grid centre `x`, else `None`.
    pub x: Option<i64>,
    /// Local-grid centre `y`, else `None`.
    pub y: Option<i64>,
    /// Local-grid data area, else `None`.
    pub data_area: Option<i64>,
    /// Raster fill percentage as written (three decimals).
    pub fill: f64,
}

impl ServiceLogRow {
    /// Whether this is a local-grid request (`region == -1`).
    pub fn is_local(&self) -> bool {
        self.region == -1
    }

    /// Whether this is a global-grid request (`region == 0`).
    pub fn is_global(&self) -> bool {
        self.region == 0
    }

    /// Whether this is a region-grid request (`region > 0`).
    pub fn is_region(&self) -> bool {
        self.region > 0
    }
}

/// Parse one servicelog line into a [`ServiceLogRow`].
///
/// Returns `None` when the line has fewer/more than [`SERVICELOG_FIELDS`]
/// non-empty tab-separated segments or a field that cannot be parsed.  Empty
/// input (blank line) is `None` too.
pub fn parse_row(line: &str) -> Option<ServiceLogRow> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim().is_empty() {
        return None;
    }
    // The city padding is made of tabs, which produce empty segments; dropping
    // them recovers exactly the 14 logical fields.
    let fields: Vec<&str> = line.split('\t').filter(|s| !s.is_empty()).collect();
    if fields.len() != SERVICELOG_FIELDS {
        return None;
    }

    Some(ServiceLogRow {
        timestamp: fields[0].to_string(),
        country: dash_to_none(fields[1]),
        city: dash_to_none(fields[2]),
        platform: fields[3].to_string(),
        version: parse_version(fields[4]),
        minute_offset: fields[5].parse().ok()?,
        minute_length: fields[6].parse().ok()?,
        grid_baselength: fields[7].parse().ok()?,
        region: fields[8].parse().ok()?,
        count_threshold: fields[9].parse().ok()?,
        x: dash_to_int(fields[10]),
        y: dash_to_int(fields[11]),
        data_area: dash_to_int(fields[12]),
        fill: fields[13].parse().ok()?,
    })
}

/// `-` (the "unknown" placeholder) becomes `None`; every other value is `Some`.
fn dash_to_none(value: &str) -> Option<String> {
    if value == "-" {
        None
    } else {
        Some(value.to_string())
    }
}

/// `-` becomes `None`; an integer string becomes `Some`.
fn dash_to_int(value: &str) -> Option<i64> {
    if value == "-" {
        None
    } else {
        value.parse().ok()
    }
}

/// The version column: `None` for the `-`/`None` placeholders and for anything
/// that is not an integer.
fn parse_version(value: &str) -> Option<i64> {
    if value == "-" || value == "None" {
        None
    } else {
        value.parse().ok()
    }
}

/// Parse a whole servicelog file, returning the rows and the number of lines
/// that could not be parsed.
pub fn parse_file(path: &Path) -> std::io::Result<ParseOutcome> {
    let content = std::fs::read_to_string(path)?;
    Ok(parse_content(&content))
}

/// Parse servicelog content, returning the rows and the number of malformed
/// lines skipped.
pub fn parse_content(content: &str) -> ParseOutcome {
    let mut rows = Vec::new();
    let mut malformed = 0usize;
    for line in content.lines() {
        match parse_row(line) {
            Some(row) => rows.push(row),
            None => {
                if !line.trim().is_empty() {
                    malformed += 1;
                }
            }
        }
    }
    ParseOutcome { rows, malformed }
}

/// The result of parsing a servicelog file/content.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParseOutcome {
    pub rows: Vec<ServiceLogRow>,
    /// The number of non-empty lines that did not parse.
    pub malformed: usize,
}

/// One entry of a "top N" list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopEntry {
    pub label: String,
    pub count: u64,
}

/// One `data_area` bucket of the local-query distribution.
///
/// `data_area` is the tile size in degrees (the `data_area` field of a local
/// request, always a multiple of [`RASTER_DEGREES`] in practice).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataAreaBucket {
    pub data_area: i64,
    pub count: u64,
}

/// A local query location for the world-map overlay.
///
/// `x`/`y` are the local-grid tile indices written in the servicelog; the grid
/// origin is `((x-1) * data_area, (y-1) * data_area)` degrees and `data_area` is
/// the tile size in degrees (see [`crate::geom::LocalGrid`]).  `grid_baselength`
/// is the raster baselength in metres, so an overlay can scale each marker.
///
/// `data_area` was added for the ASCII world map (issue #24): it determines the
/// footprint of a query in the base 5-degree raster (`data_area / 5` cells per
/// side).  It is `5` (the minimum) when the row did not carry a usable value.
///
/// `minute_length` classifies the query as offline or interactive (see
/// [`is_offline`](Self::is_offline) / [`is_interactive`](Self::is_interactive)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalQuery {
    pub x: i64,
    pub y: i64,
    pub data_area: i64,
    pub grid_baselength: i64,
    /// The requested time span in minutes (`minute_length` in the servicelog).
    pub minute_length: i64,
}

/// The `minute_length` of an **offline** query: the cached widget requests a
/// fixed 10-minute window.  Every longer window is an interactive query.
pub const OFFLINE_MINUTE_LENGTH: i64 = 10;

impl LocalQuery {
    /// Whether this is an offline query: a fixed 10-minute window.
    pub fn is_offline(&self) -> bool {
        self.minute_length == OFFLINE_MINUTE_LENGTH
    }

    /// Whether this is an interactive query: any window longer than the offline
    /// 10 minutes.
    pub fn is_interactive(&self) -> bool {
        self.minute_length > OFFLINE_MINUTE_LENGTH
    }

    /// The tile centre as `(longitude, latitude)` degrees.
    ///
    /// A local grid at `(x, y)` with tile size `data_area` starts at
    /// `((x-1) * data_area, (y-1) * data_area)` and spans `data_area * 3`
    /// degrees (see [`crate::geom::LocalGrid`]); the centre is therefore offset
    /// by `1.5 * data_area` from the origin.  This is the geographic point the
    /// SVG world map plots for the query.
    pub fn center_lon_lat(&self) -> (f64, f64) {
        let data_area = self.data_area.max(1) as f64;
        let lon = (self.x - 1) as f64 * data_area + data_area * 1.5;
        let lat = (self.y - 1) as f64 * data_area + data_area * 1.5;
        (lon, lat)
    }
}

/// The aggregated daily statistics.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServiceLogStats {
    /// The total number of parsed requests.
    pub total_requests: u64,
    /// Requests with `region == -1`.
    pub local_requests: u64,
    /// Requests with `region == 0`.
    pub global_requests: u64,
    /// Requests with `region > 0`.
    pub region_requests: u64,
    /// Local requests with a 10-minute window (the offline widget).
    pub offline_requests: u64,
    /// Local requests with a window longer than 10 minutes (interactive).
    pub interactive_requests: u64,
    /// Requests whose country was unknown (`-`).
    pub unknown_country: u64,
    /// Requests whose city was unknown (`-`).
    pub unknown_city: u64,
    /// Requests whose client version could not be determined.
    pub unknown_version: u64,
    pub countries: Vec<TopEntry>,
    pub cities: Vec<TopEntry>,
    pub versions: Vec<TopEntry>,
    /// The distribution of `data_area` over the local queries, sorted by
    /// `data_area` ascending (not by count): a small `data_area` is a small,
    /// fine-grained query, a large one a coarse, wide-area query.
    pub data_area_distribution: Vec<DataAreaBucket>,
    /// All local query grid locations, in file order.
    pub local_queries: Vec<LocalQuery>,
}

/// Count occurrences of `label` in `counter`.
fn bump(counter: &mut HashMap<String, u64>, label: &str) {
    *counter.entry(label.to_string()).or_insert(0) += 1;
}

/// Sort a counter into a "top N" list: descending by count, then ascending by
/// label so ties have a stable, deterministic order.
fn take_top(counter: HashMap<String, u64>, n: usize) -> Vec<TopEntry> {
    let mut entries: Vec<TopEntry> = counter
        .into_iter()
        .map(|(label, count)| TopEntry { label, count })
        .collect();
    entries.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
    entries.truncate(n);
    entries
}

/// Aggregate parsed rows into daily statistics.
///
/// `top_n` is the size of each top list (e.g. 10); `0` yields empty lists but
/// still counts the totals.
pub fn aggregate(rows: &[ServiceLogRow], top_n: usize) -> ServiceLogStats {
    let mut stats = ServiceLogStats {
        local_queries: Vec::new(),
        ..ServiceLogStats::default()
    };
    let mut countries: HashMap<String, u64> = HashMap::new();
    let mut cities: HashMap<String, u64> = HashMap::new();
    let mut versions: HashMap<String, u64> = HashMap::new();
    let mut data_areas: HashMap<i64, u64> = HashMap::new();

    for row in rows {
        stats.total_requests += 1;
        if row.is_local() {
            stats.local_requests += 1;
        } else if row.is_global() {
            stats.global_requests += 1;
        } else if row.is_region() {
            stats.region_requests += 1;
        }

        match &row.country {
            Some(country) => bump(&mut countries, country),
            None => stats.unknown_country += 1,
        }
        match &row.city {
            Some(city) => bump(&mut cities, city),
            None => stats.unknown_city += 1,
        }
        match row.version {
            Some(version) => bump(&mut versions, &version.to_string()),
            None => stats.unknown_version += 1,
        }

        if row.is_local() {
            if let (Some(x), Some(y)) = (row.x, row.y) {
                let data_area = row.data_area.unwrap_or(5).max(5);
                *data_areas.entry(data_area).or_insert(0) += 1;
                let query = LocalQuery {
                    x,
                    y,
                    data_area,
                    grid_baselength: row.grid_baselength,
                    minute_length: row.minute_length,
                };
                if query.is_interactive() {
                    stats.interactive_requests += 1;
                } else {
                    stats.offline_requests += 1;
                }
                stats.local_queries.push(query);
            }
        }
    }

    stats.countries = take_top(countries, top_n);
    stats.cities = take_top(cities, top_n);
    stats.versions = take_top(versions, top_n);
    stats.data_area_distribution = take_distribution(data_areas);
    stats
}

/// Sort a `data_area` counter into a distribution: ascending by `data_area`
/// (so the histogram reads fine -> coarse from left to right).
fn take_distribution(counter: HashMap<i64, u64>) -> Vec<DataAreaBucket> {
    let mut buckets: Vec<DataAreaBucket> = counter
        .into_iter()
        .map(|(data_area, count)| DataAreaBucket { data_area, count })
        .collect();
    buckets.sort_by_key(|b| b.data_area);
    buckets
}

/// Parse and aggregate a single servicelog file.
pub fn statistics_for_file(
    path: &Path,
    top_n: usize,
) -> std::io::Result<(ParseOutcome, ServiceLogStats)> {
    let outcome = parse_file(path)?;
    let stats = aggregate(&outcome.rows, top_n);
    Ok((outcome, stats))
}

/// Render the statistics as a human-readable text report.
pub fn render_text(day: &str, stats: &ServiceLogStats) -> String {
    let mut out = String::new();
    out.push_str(&format!("servicelog statistics for {day}\n"));
    out.push_str(&format!("total requests: {}\n", stats.total_requests));
    out.push_str(&format!(
        "  local:  {}  global: {}  region: {}\n",
        stats.local_requests, stats.global_requests, stats.region_requests
    ));
    out.push_str(&format!(
        "  offline: {}  interactive: {}\n",
        stats.offline_requests, stats.interactive_requests
    ));
    out.push_str(&format!(
        "  unknown country: {}  unknown city: {}  unknown version: {}\n",
        stats.unknown_country, stats.unknown_city, stats.unknown_version
    ));

    for (title, entries) in [
        ("top countries", &stats.countries),
        ("top cities", &stats.cities),
        ("top client versions", &stats.versions),
    ] {
        out.push_str(&format!("\n{title}:\n"));
        if entries.is_empty() {
            out.push_str("  (none)\n");
        }
        for entry in entries {
            out.push_str(&format!("  {:>8}  {}\n", entry.count, entry.label));
        }
    }

    out.push_str("\ndata_area distribution (local queries):\n");
    if stats.data_area_distribution.is_empty() {
        out.push_str("  (none)\n");
    }
    let max = stats
        .data_area_distribution
        .iter()
        .map(|b| b.count)
        .max()
        .unwrap_or(0);
    for bucket in &stats.data_area_distribution {
        out.push_str(&format!(
            "  {:>4}  {:>8}  {}\n",
            bucket.data_area,
            bucket.count,
            histogram_bar(bucket.count, max, 40)
        ));
    }

    out.push_str(&format!(
        "\nlocal query locations: {} (offline {}, interactive {})\n\n",
        stats.local_queries.len(),
        stats.offline_requests,
        stats.interactive_requests
    ));
    // The text report includes the ASCII world maps (issue #24): one for the
    // offline queries and one for the interactive queries; `--format map`
    // prints the maps alone.
    out.push_str(&render_ascii_maps(stats));
    out
}

/// A `#` bar of `width` columns scaled to `max` (at least one `#` for a
/// non-zero count so no bucket renders as blank).
fn histogram_bar(count: u64, max: u64, width: usize) -> String {
    if count == 0 || max == 0 {
        return String::new();
    }
    let columns = ((count as f64 / max as f64) * width as f64).ceil() as usize;
    "#".repeat(columns.clamp(1, width))
}

/// Render the statistics as pretty-printed JSON.
pub fn render_json(day: &str, stats: &ServiceLogStats) -> String {
    let value = serde_json::json!({
        "day": day,
        "total_requests": stats.total_requests,
        "local_requests": stats.local_requests,
        "global_requests": stats.global_requests,
        "region_requests": stats.region_requests,
        "offline_requests": stats.offline_requests,
        "interactive_requests": stats.interactive_requests,
        "unknown_country": stats.unknown_country,
        "unknown_city": stats.unknown_city,
        "unknown_version": stats.unknown_version,
        "countries": top_entries_json(&stats.countries),
        "cities": top_entries_json(&stats.cities),
        "versions": top_entries_json(&stats.versions),
        "data_area_distribution": stats.data_area_distribution.iter().map(|b| serde_json::json!({
            "data_area": b.data_area,
            "count": b.count,
        })).collect::<Vec<_>>(),
        "local_queries": stats.local_queries.iter().map(|q| serde_json::json!({
            "x": q.x,
            "y": q.y,
            "data_area": q.data_area,
            "grid_baselength": q.grid_baselength,
            "minute_length": q.minute_length,
            "interactive": q.is_interactive(),
        })).collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
}

/// Render the local queries as ASCII world maps (5-degree raster, issue #24):
/// one for the **offline** queries (a fixed 10-minute window) and a separate
/// one for the **interactive** queries (any longer window).
pub fn render_ascii_map(day: &str, stats: &ServiceLogStats) -> String {
    format!(
        "servicelog local-query maps for {day}\n{}",
        render_ascii_maps(stats)
    )
}

/// The two ASCII world maps (offline and interactive), each labelled and
/// separated by a blank line.
pub fn render_ascii_maps(stats: &ServiceLogStats) -> String {
    let offline =
        AsciiWorldMap::from_local_queries_filtered(&stats.local_queries, |q| q.is_offline());
    let interactive =
        AsciiWorldMap::from_local_queries_filtered(&stats.local_queries, |q| q.is_interactive());
    format!(
        "offline queries (minute_length == {}):\n{}\ninteractive queries (minute_length > {}):\n{}",
        OFFLINE_MINUTE_LENGTH,
        offline.render_titled("offline local-query world map"),
        OFFLINE_MINUTE_LENGTH,
        interactive.render_titled("interactive local-query world map"),
    )
}

fn top_entries_json(entries: &[TopEntry]) -> Vec<serde_json::Value> {
    entries
        .iter()
        .map(|e| serde_json::json!({ "label": e.label, "count": e.count }))
        .collect()
}

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

// --- Geographic SVG world map + static HTML report (issue #28) -------------

/// Escape the five XML/HTML metacharacters so arbitrary labels (country, city)
/// can be embedded safely in SVG/HTML.
fn escape_html(value: &str) -> String {
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

/// The water / land / coast colours of the light basemap.
const WATER_FILL: &str = "#f5f6f7";
/// Light-gray land fill for the basemap (see [`WATER_FILL`]).
const LAND_FILL: &str = "#d9dde1";
/// Coastline stroke for the basemap.
const COAST_STROKE: &str = "#aeb6bd";
/// Graticule colour (equator/prime meridian on the light basemap).
const GRATICULE_STROKE: &str = "#dde1e5";

/// The `(R, G, B)` colour ramp for the shaded squares, light to dense.
///
/// A square's shade is keyed to its **absolute** query count (see
/// [`square_colour`]) so the two separate maps are directly comparable and a
/// tile with a single query is never rendered as a hot spot.
const SQUARE_RAMP: [(u8, u8, u8); 6] = [
    (255, 224, 138), // pale amber
    (255, 197, 82),
    (251, 160, 47),
    (240, 116, 34),
    (214, 75, 36),
    (176, 42, 44), // deep red
];

/// The per-tile query count that saturates the ramp: `1` maps to the palest
/// shade and `>= SQUARE_RAMP_MAX` to the deepest.
const SQUARE_RAMP_MAX: u64 = 32;

/// Pick a ramp colour for an absolute `count`.
///
/// The count is mapped onto the ramp with a logarithmic scale (so low counts
/// stay visually distinct) and capped at [`SQUARE_RAMP_MAX`].  This is
/// deliberately absolute rather than normalised against the map's maximum: the
/// same tile count reads the same on both the background and interactive maps,
/// and a lone query on an otherwise empty map is not painted the densest shade.
fn square_colour(count: u64) -> String {
    let count = count.min(SQUARE_RAMP_MAX);
    let bucket = match count {
        0 => 0,
        1 => 1,
        2..=3 => 2,
        4..=7 => 3,
        8..=15 => 4,
        _ => 5,
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
        "<g class=\"squares\" data-set=\"{}\" fill-opacity=\"0.55\">\n  <title>{}</title>\n",
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
    svg.push_str(&crate::world_map::continent_layer(
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

/// Render a "top N" list as HTML table rows (`<tr><td>label</td><td>count</td>`).
fn top_table_rows(entries: &[TopEntry]) -> String {
    if entries.is_empty() {
        return "      <tr><td colspan=\"2\" class=\"empty\">(none)</td></tr>\n".to_string();
    }
    let mut rows = String::new();
    for entry in entries {
        let _ = writeln!(
            rows,
            "      <tr><td>{}</td><td class=\"num\">{}</td></tr>",
            escape_html(&entry.label),
            entry.count
        );
    }
    rows
}

/// Render the `data_area` distribution as HTML table rows with a scaled bar.
fn data_area_rows(buckets: &[DataAreaBucket]) -> String {
    if buckets.is_empty() {
        return "      <tr><td colspan=\"3\" class=\"empty\">(none)</td></tr>\n".to_string();
    }
    let max = buckets.iter().map(|b| b.count).max().unwrap_or(0);
    let mut rows = String::new();
    for bucket in buckets {
        let pct = if max == 0 {
            0.0
        } else {
            bucket.count as f64 / max as f64 * 100.0
        };
        let _ = writeln!(
            rows,
            "      <tr><td class=\"num\">{}</td><td class=\"num\">{}</td>\
             <td><span class=\"bar\" style=\"width:{pct:.1}%\"></span></td></tr>",
            bucket.data_area, bucket.count
        );
    }
    rows
}

/// Render a complete, standalone static HTML report for `day`.
///
/// The document embeds the statistics (totals, top countries/cities/versions,
/// `data_area` distribution) and two separate SVG world maps — one for the
/// background/offline queries and one for the interactive queries — each with a
/// light-gray continent basemap under semi-transparent squares shaded by the
/// per-tile query count (see [`render_world_svg`]).  All styling is inline in a
/// `<style>` block, so the file is self-contained and needs no network access.
pub fn render_html(day: &str, stats: &ServiceLogStats) -> String {
    let escape = |value: &str| escape_html(value);
    let offline = stats
        .local_queries
        .iter()
        .filter(|q| q.is_offline())
        .count();
    let interactive = stats.local_queries.len() - offline;

    let mut html = String::new();
    html.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    html.push_str("  <meta charset=\"utf-8\">\n");
    html.push_str(&format!(
        "  <title>servicelog statistics for {}</title>\n",
        escape(day)
    ));
    html.push_str("  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    html.push_str("  <style>\n");
    html.push_str(
        "    :root { color-scheme: light; }\n\
         \x20   body { margin: 0; padding: 2rem; background: #ffffff; color: #24313b;\n\
         \x20          font-family: system-ui, -apple-system, Segoe UI, sans-serif; }\n\
         \x20   h1 { font-size: 1.4rem; margin: 0 0 .25rem; }\n\
         \x20   h2 { font-size: 1.05rem; margin: 1.5rem 0 .5rem; color: #2b5d8a; }\n\
         \x20   .sub { color: #5f707d; margin: 0 0 1.5rem; }\n\
         \x20   .cards { display: flex; flex-wrap: wrap; gap: .75rem; margin-bottom: 1rem; }\n\
         \x20   .card { background: #f4f6f8; border: 1px solid #dde1e5; border-radius: 8px;\n\
         \x20           padding: .6rem .9rem; min-width: 8rem; }\n\
         \x20   .card .label { font-size: .72rem; text-transform: uppercase; letter-spacing: .05em;\n\
         \x20                  color: #5f707d; }\n\
         \x20   .card .value { font-size: 1.3rem; font-variant-numeric: tabular-nums; }\n\
         \x20   svg.worldmap { display: block; max-width: 100%; height: auto;\n\
         \x20                 border: 1px solid #dde1e5; border-radius: 8px; background: #f5f6f7; }\n\
         \x20   .maps { display: flex; flex-wrap: wrap; gap: 1rem; }\n\
         \x20   .maps figure { flex: 1 1 26rem; margin: 0; }\n\
         \x20   .maps figcaption { font-size: .85rem; color: #5f707d; margin: .4rem 0 0; }\n\
         \x20   .legend { display: flex; flex-wrap: wrap; align-items: center; gap: .5rem 1rem;\n\
         \x20             margin: .5rem 0 0; font-size: .85rem; color: #5f707d; }\n\
         \x20   .swatch { display: inline-block; width: .8rem; height: .8rem; border-radius: 2px;\n\
         \x20             vertical-align: middle; margin-right: .35rem; }\n\
         \x20   table { border-collapse: collapse; width: 100%; max-width: 34rem; }\n\
         \x20   th, td { text-align: left; padding: .3rem .6rem; border-bottom: 1px solid #e7eaee; }\n\
         \x20   th { color: #5f707d; font-weight: 600; font-size: .8rem; }\n\
         \x20   td.num { text-align: right; font-variant-numeric: tabular-nums; width: 6rem; }\n\
         \x20   td.empty { color: #8a99a5; font-style: italic; }\n\
         \x20   .bar { display: inline-block; height: .7rem; min-width: 1px; background: #2f9e6a;\n\
         \x20          border-radius: 3px; }\n\
         \x20   footer { margin-top: 2rem; color: #8a99a5; font-size: .8rem; }\n\
         \x20 </style>\n",
    );
    html.push_str("</head>\n<body>\n");
    html.push_str(&format!(
        "  <h1>servicelog statistics for {}</h1>\n  <p class=\"sub\">{} requests</p>\n",
        escape(day),
        stats.total_requests
    ));

    // Summary cards.
    html.push_str("  <div class=\"cards\">\n");
    for (label, value) in [
        ("total", stats.total_requests),
        ("local", stats.local_requests),
        ("global", stats.global_requests),
        ("region", stats.region_requests),
        ("offline", stats.offline_requests),
        ("interactive", stats.interactive_requests),
        ("unknown country", stats.unknown_country),
        ("unknown city", stats.unknown_city),
        ("unknown version", stats.unknown_version),
    ] {
        let _ = writeln!(
            html,
            "    <div class=\"card\"><div class=\"label\">{}</div>\
             <div class=\"value\">{value}</div></div>",
            escape(label)
        );
    }
    html.push_str("  </div>\n");

    // World maps: one per query category so the overlays never overlap.
    html.push_str("  <h2>Local query locations</h2>\n");
    html.push_str(&format!(
        "  <p class=\"sub\">{} local query locations (background {}, interactive {}); \
         each square is a 5-degree raster cell, shaded by its query count; the land \
         outline is a coarse orientation aid.</p>\n",
        stats.local_queries.len(),
        offline,
        interactive
    ));
    let (offline_svg, interactive_svg) = render_world_svg(
        stats,
        crate::world_map::MAP_WIDTH,
        crate::world_map::MAP_HEIGHT,
    );
    html.push_str("  <div class=\"maps\">\n");
    let _ = write!(
        html,
        "    <figure>{offline_svg}<figcaption>Background / offline queries \
         (minute_length == 10): {offline}</figcaption></figure>\n\
         \x20   <figure>{interactive_svg}<figcaption>Interactive queries \
         (minute_length &gt; 10): {interactive}</figcaption></figure>\n"
    );
    html.push_str("  </div>\n");
    // Shade legend: the square ramp runs from one query to the densest tile.
    html.push_str(
        "  <p class=\"legend\">\
         <span><span class=\"swatch\" style=\"background:#f5f6f7\"></span>water</span>\
         <span><span class=\"swatch\" style=\"background:#d9dde1\"></span>land</span>\
         <span>queries per square:</span>",
    );
    for (index, (r, g, b)) in SQUARE_RAMP.iter().enumerate() {
        let _ = write!(
            html,
            "<span class=\"swatch\" style=\"background:#{r:02x}{g:02x}{b:02x}\"></span>"
        );
        if index + 1 == SQUARE_RAMP.len() {
            html.push_str("<span>more</span>");
        }
    }
    html.push_str("</p>\n");

    // Top lists.
    for (title, entries) in [
        ("Top countries", &stats.countries),
        ("Top cities", &stats.cities),
        ("Top client versions", &stats.versions),
    ] {
        let _ = write!(
            html,
            "  <h2>{title}</h2>\n  <table>\n    <thead><tr><th>{}</th>\
             <th class=\"num\">requests</th></tr></thead>\n    <tbody>\n{}    </tbody>\n  </table>\n",
            escape(title),
            top_table_rows(entries)
        );
    }

    // data_area distribution.
    html.push_str("  <h2>data_area distribution (local queries)</h2>\n");
    html.push_str(
        "  <table>\n    <thead><tr><th class=\"num\">data_area</th>\
         <th class=\"num\">count</th><th></th></tr></thead>\n    <tbody>\n",
    );
    html.push_str(&data_area_rows(&stats.data_area_distribution));
    html.push_str("    </tbody>\n  </table>\n");

    html.push_str("  <footer>Generated by bo-servicelog-stats (static HTML report).</footer>\n");
    html.push_str("</body>\n</html>\n");
    html
}

/// The number of base-raster columns spanning the world (5-degree cells):
/// `360 / 5 = 72`.
pub const WORLD_COLS: usize = 72;

/// The number of base-raster rows spanning the world (5-degree cells):
/// `180 / 5 = 36`.
pub const WORLD_ROWS: usize = 36;

/// The base raster cell size in degrees (issue #24: "use 5 as the basic
/// raster").
pub const RASTER_DEGREES: i64 = 5;

/// Density ramp for the ASCII map (light to dense), matching the symbol set of
/// `data::GridData::to_map` (`" .-o*O8"`) plus a final `#`.  Index 0 (`' '`) is
/// reserved for a zero count, so non-zero counts use indices 1..=7.
pub const MAP_RAMP: [char; 8] = [' ', '.', '-', 'o', '*', 'O', '8', '#'];

/// A 5-degree worldwide raster of local-query counts, rendered as ASCII
/// (issue #24).
///
/// ## Footprint
///
/// A local query at tile `(x, y)` with data area `data_area` covers
/// `data_area` degrees starting at the grid origin
/// `((x-1) * data_area, (y-1) * data_area)` (see [`crate::geom::LocalGrid`]).
/// In the base 5-degree raster that is an `n x n` block with `n = data_area / 5`
/// (so `data_area=5` marks one cell, `10` a `2x2`, `15` a `3x3` and `20` a
/// `4x4` block).  Every cell of the block is incremented, so overlapping
/// queries accumulate ("higher data areas can be added on top").
///
/// ## Layout
///
/// Internally rows are stored south-up (row 0 covers latitude `-90..-85`) so
/// the arithmetic reads naturally; [`render`](Self::render) prints them
/// north-up (row 0 = the top line) like `data::GridData::to_map`.  Columns wrap
/// across the antimeridian; rows outside the poles are clamped away (never
/// counted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsciiWorldMap {
    /// `WORLD_ROWS` rows (south-up) of `WORLD_COLS` counts.
    cells: Vec<Vec<u64>>,
    /// The total number of queries that contributed.
    queries: u64,
}

impl Default for AsciiWorldMap {
    fn default() -> Self {
        AsciiWorldMap {
            cells: vec![vec![0; WORLD_COLS]; WORLD_ROWS],
            queries: 0,
        }
    }
}

impl AsciiWorldMap {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of contributing queries.
    pub fn queries(&self) -> u64 {
        self.queries
    }

    /// The count at `(col, row)` (row 0 = southernmost); `0` when out of range.
    pub fn count(&self, col: usize, row: usize) -> u64 {
        self.cells
            .get(row)
            .and_then(|r| r.get(col))
            .copied()
            .unwrap_or(0)
    }

    /// The total count across all cells.
    pub fn total(&self) -> u64 {
        self.cells.iter().flatten().sum()
    }

    /// The largest single-cell count.
    pub fn maximum(&self) -> u64 {
        self.cells.iter().flatten().copied().max().unwrap_or(0)
    }

    /// Add one local query's footprint to the raster.
    ///
    /// `x`/`y` are the local-grid tile indices (1-based; `y` may be `<= 0` in
    /// the southern hemisphere).  `data_area` is the tile size in degrees,
    /// clamped to at least [`RASTER_DEGREES`]; the footprint side is
    /// `ceil(data_area / 5)` cells.
    pub fn add_local_query(&mut self, x: i64, y: i64, data_area: i64) {
        self.queries += 1;
        let data_area = data_area.max(RASTER_DEGREES);
        // Footprint side in base cells (ceil division; `data_area` is positive).
        let side = ((data_area + RASTER_DEGREES - 1) / RASTER_DEGREES).clamp(1, WORLD_COLS as i64)
            as usize;

        // The grid origin (lower-left corner) in degrees.
        let lon0 = (x - 1) * data_area;
        let lat0 = (y - 1) * data_area;

        // Column of the origin, wrapping across the antimeridian.
        let col0 = (lon0 + 180).div_euclid(RASTER_DEGREES);
        // Row of the origin (south-up), clamped away from out-of-range poles.
        let row0 = (lat0 + 90).div_euclid(RASTER_DEGREES);

        for d_row in 0..side as i64 {
            let row = row0 + d_row;
            if !(0..WORLD_ROWS as i64).contains(&row) {
                continue;
            }
            for d_col in 0..side as i64 {
                let col = (col0 + d_col).rem_euclid(WORLD_COLS as i64) as usize;
                self.cells[row as usize][col] += 1;
            }
        }
    }

    /// Add every local query of `stats` to the map.
    pub fn from_local_queries(queries: &[LocalQuery]) -> Self {
        let mut map = Self::new();
        for query in queries {
            map.add_local_query(query.x, query.y, query.data_area);
        }
        map
    }

    /// Add only the queries matching `keep` to the map.
    pub fn from_local_queries_filtered(
        queries: &[LocalQuery],
        keep: impl Fn(&LocalQuery) -> bool,
    ) -> Self {
        let mut map = Self::new();
        for query in queries.iter().filter(|q| keep(q)) {
            map.add_local_query(query.x, query.y, query.data_area);
        }
        map
    }

    /// Render the map as ASCII with the default title
    /// (`local query world map`).
    pub fn render(&self) -> String {
        self.render_titled("local query world map")
    }

    /// Render the map as ASCII, framed with a `+`/`-`/`|` border and the
    /// density ramp [`MAP_RAMP`], north-up, using `title` as the header.
    ///
    /// The ramp is scaled to the largest cell count: `0` renders as a space and
    /// the densest cell as `#`.  A header with the query/hit counts and a
    /// legend are printed around the frame.
    pub fn render_titled(&self, title: &str) -> String {
        let maximum = self.maximum();
        let mut out = String::new();

        out.push_str(&format!(
            "{title} ({}x{} cells of {} degrees, {} queries, {} hits)\n",
            WORLD_COLS,
            WORLD_ROWS,
            RASTER_DEGREES,
            self.queries,
            self.total()
        ));

        let border = format!("+{}+", "-".repeat(WORLD_COLS));
        out.push_str(&border);
        out.push('\n');

        // Print north-up: the last internal row (highest latitude) first.
        for row_index in (0..WORLD_ROWS).rev() {
            out.push('|');
            for col in 0..WORLD_COLS {
                out.push(self.symbol(self.cells[row_index][col], maximum));
            }
            out.push_str("|\n");
        }

        out.push_str(&border);
        out.push('\n');
        out.push_str(&format!(
            "legend: '{}' = 0, '{}' = max ({}); columns 5 degrees from 180W, rows 5 degrees from 90S\n",
            MAP_RAMP[0], MAP_RAMP[MAP_RAMP.len() - 1], maximum
        ));
        out
    }

    /// The ramp symbol for `count` given the map `maximum`: `0` is a space and
    /// the maximum is the last ramp symbol.
    fn symbol(&self, count: u64, maximum: u64) -> char {
        if count == 0 || maximum == 0 {
            return MAP_RAMP[0];
        }
        // Scale into 1..=len-1 so a non-zero count never renders as blank.
        let steps = MAP_RAMP.len() - 1;
        let index = ((count as f64 / maximum as f64) * steps as f64).ceil() as usize;
        MAP_RAMP[1 + index.min(steps - 1)]
    }
}

/// The day name (`YYYY-MM-DD`) of a `servicelog_YYYY-MM-DD` file, or `None`.
pub fn day_from_filename(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let day = name.strip_prefix("servicelog_")?;
    if day.len() == 10 && day.as_bytes()[4] == b'-' && day.as_bytes()[7] == b'-' {
        Some(day.to_string())
    } else {
        None
    }
}

/// The current UTC day as `YYYY-MM-DD`, the default day of the report.
///
/// The servicelog file name carries the UTC date of its rows (see
/// [`crate::service_log::entry_day`]), so "today" must be UTC-based too.
pub fn today_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// A servicelog source is a day plus its parsed rows, kept for per-day reports.
#[derive(Debug, Clone, PartialEq)]
pub struct DayReport {
    pub day: String,
    pub stats: ServiceLogStats,
    pub malformed: usize,
}

/// Build a [`DayReport`] from parsed rows and the day label.
pub fn day_report(day: String, outcome: &ParseOutcome, top_n: usize) -> DayReport {
    DayReport {
        stats: aggregate(&outcome.rows, top_n),
        malformed: outcome.malformed,
        day,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sample row copied verbatim from the issue (city padded with tabs).
    const ISSUE_SAMPLE: &str = "\
08:44:50.596\tUS\tSun Prairie\t\t\tA\t352\t0\t10\t25000\t0\t0\t-\t-\t-\t0.036\n\
08:44:50.637\tSE\tGothenburg\t\t\tA\t352\t0\t10\t5000\t-1\t0\t4\t13\t5\t0.000\n\
08:44:50.655\tRO\tBucharest\t\t\tA\t352\t0\t10\t5000\t-1\t0\t5\t8\t5\t0.039\n\
08:44:51.106\tDE\tBerlin\t\t\t\tA\t352\t0\t60\t5000\t-1\t0\t2\t10\t5\t0.000\n\
08:44:51.238\tDE\tUlm\t\t\t\tA\t352\t0\t10\t5000\t-1\t0\t1\t9\t5\t0.000\n\
08:44:51.277\tIT\tVicenza\t\t\t\tA\t352\t0\t10\t5000\t-1\t0\t2\t9\t5\t0.000\n";

    #[test]
    fn parses_the_issue_example_rows() {
        let outcome = parse_content(ISSUE_SAMPLE);
        assert_eq!(outcome.malformed, 0);
        assert_eq!(outcome.rows.len(), 6);

        let first = &outcome.rows[0];
        assert_eq!(first.timestamp, "08:44:50.596");
        assert_eq!(first.country.as_deref(), Some("US"));
        assert_eq!(first.city.as_deref(), Some("Sun Prairie"));
        assert_eq!(first.platform, "A");
        assert_eq!(first.version, Some(352));
        assert_eq!(first.region, 0);
        assert_eq!(first.grid_baselength, 25000);
        assert!(first.is_global());
        assert_eq!(first.x, None);

        let local = &outcome.rows[1];
        assert!(local.is_local());
        assert_eq!(local.x, Some(4));
        assert_eq!(local.y, Some(13));
        assert_eq!(local.data_area, Some(5));
        assert_eq!(local.fill, 0.0);
    }

    #[test]
    fn ignores_city_padding_when_counting_fields() {
        // A long city spans more tab blocks; the logical fields are unchanged.
        let line =
            "08:44:51.106\tDE\tFrankfurt am Main\t\tA\t352\t0\t60\t5000\t-1\t0\t2\t10\t5\t0.000";
        let row = parse_row(line).expect("parses");
        assert_eq!(row.city.as_deref(), Some("Frankfurt am Main"));
        assert_eq!(row.x, Some(2));
    }

    #[test]
    fn malformed_lines_are_skipped_not_panicking() {
        let content = "garbage\n08:44:50.596\tUS\n\n";
        let outcome = parse_content(content);
        assert!(outcome.rows.is_empty());
        assert_eq!(outcome.malformed, 2);
    }

    #[test]
    fn dash_placeholders_become_none() {
        // A non-Android client (`-` platform) with unknown country/city and no
        // version: 14 logical fields, city padded with tabs.
        let line = "08:44:50.596\t-\t-\t\t\t\t-\tNone\t0\t10\t0\t0\t0\t-\t-\t-\t0.000";
        let row = parse_row(line).expect("parses");
        assert_eq!(row.country, None);
        assert_eq!(row.city, None);
        assert_eq!(row.platform, "-");
        assert_eq!(row.version, None);
        assert_eq!(row.x, None);
    }

    #[test]
    fn aggregates_totals_and_flavours() {
        let outcome = parse_content(ISSUE_SAMPLE);
        let stats = aggregate(&outcome.rows, 10);
        assert_eq!(stats.total_requests, 6);
        assert_eq!(stats.global_requests, 1);
        assert_eq!(stats.local_requests, 5);
        assert_eq!(stats.region_requests, 0);
        assert_eq!(stats.unknown_country, 0);
        assert_eq!(stats.unknown_city, 0);
        assert_eq!(stats.unknown_version, 0);
    }

    #[test]
    fn top_lists_are_ordered_by_count_then_label() {
        let outcome = parse_content(ISSUE_SAMPLE);
        let stats = aggregate(&outcome.rows, 10);
        // DE appears twice; the rest once (ties alphabetical).
        assert_eq!(
            stats.countries[0],
            TopEntry {
                label: "DE".into(),
                count: 2
            }
        );
        let labels: Vec<&str> = stats.countries[1..]
            .iter()
            .map(|e| e.label.as_str())
            .collect();
        assert_eq!(labels, ["IT", "RO", "SE", "US"]);
        // Every city is unique here.
        assert_eq!(stats.cities.len(), 6);
        assert_eq!(stats.cities[0].label, "Berlin");
        // All versions are 352.
        assert_eq!(
            stats.versions,
            vec![TopEntry {
                label: "352".into(),
                count: 6
            }]
        );
    }

    #[test]
    fn top_n_limits_the_lists() {
        let outcome = parse_content(ISSUE_SAMPLE);
        let stats = aggregate(&outcome.rows, 2);
        assert_eq!(stats.countries.len(), 2);
        assert_eq!(stats.cities.len(), 2);
        assert_eq!(stats.versions.len(), 1);
    }

    #[test]
    fn count_zero_yields_empty_lists_but_keeps_totals() {
        let outcome = parse_content(ISSUE_SAMPLE);
        let stats = aggregate(&outcome.rows, 0);
        assert_eq!(stats.total_requests, 6);
        assert!(stats.countries.is_empty());
        assert!(stats.cities.is_empty());
        assert!(stats.versions.is_empty());
    }

    #[test]
    fn local_queries_collect_only_local_rows_with_coordinates() {
        let outcome = parse_content(ISSUE_SAMPLE);
        let stats = aggregate(&outcome.rows, 10);
        assert_eq!(stats.local_queries.len(), 5);
        assert_eq!(
            stats.local_queries[0],
            LocalQuery {
                x: 4,
                y: 13,
                data_area: 5,
                grid_baselength: 5000,
                minute_length: 10
            }
        );
    }

    #[test]
    fn unknown_geo_and_version_are_counted_separately() {
        let content = "08:44:50.596\t-\t-\t\t\t\t-\tNone\t0\t10\t0\t0\t0\t-\t-\t-\t0.000\n";
        let stats = aggregate(&parse_content(content).rows, 10);
        assert_eq!(stats.total_requests, 1);
        assert_eq!(stats.unknown_country, 1);
        assert_eq!(stats.unknown_city, 1);
        assert_eq!(stats.unknown_version, 1);
        assert!(stats.countries.is_empty());
        assert!(stats.cities.is_empty());
        assert!(stats.versions.is_empty());
    }

    /// A local row builder for the distribution tests.
    fn local_row(x: i64, y: i64, data_area: &str) -> String {
        format!("08:00:00.000\tDE\tCity\t\t\t\tA\t352\t0\t60\t5000\t-1\t0\t{x}\t{y}\t{data_area}\t0.000")
    }

    /// The `data_area` distribution counts every local query, sorted ascending
    /// by `data_area`.
    #[test]
    fn data_area_distribution_counts_and_sorts() {
        let content = [
            local_row(1, 1, "5"),
            local_row(2, 1, "10"),
            local_row(3, 1, "5"),
            local_row(4, 1, "20"),
            local_row(5, 1, "10"),
            local_row(6, 1, "5"),
        ]
        .join("\n");
        let stats = aggregate(&parse_content(&content).rows, 10);
        assert_eq!(
            stats.data_area_distribution,
            vec![
                DataAreaBucket {
                    data_area: 5,
                    count: 3
                },
                DataAreaBucket {
                    data_area: 10,
                    count: 2
                },
                DataAreaBucket {
                    data_area: 20,
                    count: 1
                },
            ]
        );
    }

    /// Non-local rows do not contribute to the distribution.
    #[test]
    fn data_area_distribution_ignores_non_local_rows() {
        let content =
            "08:00:00.000\tDE\tBerlin\t\t\t\tA\t352\t0\t60\t10000\t0\t0\t-\t-\t-\t0.000\n";
        let stats = aggregate(&parse_content(content).rows, 10);
        assert!(stats.data_area_distribution.is_empty());
    }

    /// A missing (`-`) data_area counts as the 5-degree minimum, matching the
    /// map footprint.
    #[test]
    fn data_area_distribution_defaults_missing_to_minimum() {
        let content = local_row(1, 1, "-");
        let stats = aggregate(&parse_content(&content).rows, 10);
        assert_eq!(
            stats.data_area_distribution,
            vec![DataAreaBucket {
                data_area: 5,
                count: 1
            }]
        );
    }

    /// The text report lists the distribution with a scaled bar.
    #[test]
    fn text_render_includes_data_area_distribution() {
        let content = [
            local_row(1, 1, "5"),
            local_row(2, 1, "5"),
            local_row(3, 1, "10"),
            local_row(4, 1, "5"),
        ]
        .join("\n");
        let stats = aggregate(&parse_content(&content).rows, 10);
        let text = render_text("2023-11-14", &stats);
        assert!(text.contains("data_area distribution (local queries):"));
        assert!(text.contains("5         3"), "{text}");
        assert!(text.contains("10         1"), "{text}");
        // The largest bucket gets the full-width bar.
        assert!(text.contains("######"), "{text}");
    }

    /// `histogram_bar` scales to the max and keeps non-zero buckets visible.
    #[test]
    fn histogram_bar_scaling() {
        assert_eq!(histogram_bar(0, 10, 40), "");
        assert_eq!(histogram_bar(10, 10, 40), "#".repeat(40));
        assert_eq!(histogram_bar(5, 10, 40), "#".repeat(20));
        // A tiny count relative to the max still shows one `#`.
        assert_eq!(histogram_bar(1, 1000, 40), "#");
        // A zero max renders nothing (all buckets zero).
        assert_eq!(histogram_bar(0, 0, 40), "");
    }

    /// The JSON report carries the distribution as objects.
    #[test]
    fn json_render_includes_data_area_distribution() {
        let outcome = parse_content(ISSUE_SAMPLE);
        let stats = aggregate(&outcome.rows, 10);
        let value: serde_json::Value =
            serde_json::from_str(&render_json("2023-11-14", &stats)).unwrap();
        assert_eq!(value["data_area_distribution"][0]["data_area"], 5);
        assert_eq!(value["data_area_distribution"][0]["count"], 5);
    }

    #[test]
    fn text_render_contains_totals_and_tops() {
        let outcome = parse_content(ISSUE_SAMPLE);
        let stats = aggregate(&outcome.rows, 10);
        let text = render_text("2023-11-14", &stats);
        assert!(text.contains("servicelog statistics for 2023-11-14"));
        assert!(text.contains("total requests: 6"));
        assert!(text.contains("top countries:"));
        assert!(text.contains("DE"));
        assert!(text.contains("local query locations: 5"));
        // The Berlin row uses `minute_length=60` -> one interactive query; the
        // other four local rows use `minute_length=10` -> offline.
        assert!(text.contains("offline: 4  interactive: 1"));
        // The text report includes separate ASCII world maps for the offline
        // (minute_length == 10) and interactive (minute_length > 10) queries.
        assert!(text.contains("offline queries (minute_length == 10):"));
        assert!(text.contains("offline local-query world map (72x36 cells of 5 degrees"));
        assert!(text.contains("interactive queries (minute_length > 10):"));
        assert!(text.contains("interactive local-query world map (72x36 cells of 5 degrees"));
        assert!(text.contains("4 queries, 4 hits"));
    }

    #[test]
    fn json_render_is_valid_and_structured() {
        let outcome = parse_content(ISSUE_SAMPLE);
        let stats = aggregate(&outcome.rows, 10);
        let json = render_json("2023-11-14", &stats);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["day"], "2023-11-14");
        assert_eq!(value["total_requests"], 6);
        assert_eq!(value["countries"][0]["label"], "DE");
        assert_eq!(value["countries"][0]["count"], 2);
        assert_eq!(value["local_queries"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn svg_lists_one_circle_per_local_query() {
        let outcome = parse_content(ISSUE_SAMPLE);
        let stats = aggregate(&outcome.rows, 10);
        let svg = render_local_svg(&stats, 640, 320);
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("local queries: 5"));
        assert_eq!(svg.matches("<circle").count(), 5);
    }

    #[test]
    fn svg_handles_no_local_queries() {
        let svg = render_local_svg(&ServiceLogStats::default(), 100, 100);
        assert!(svg.contains("local queries: 0"));
        assert_eq!(svg.matches("<circle").count(), 0);
    }

    // --- geographic world map + HTML report (issue #28) --------------------

    /// The tile centre is offset from the grid origin by `1.5 * data_area`.
    #[test]
    fn local_query_center_lon_lat_uses_tile_centre() {
        let query = LocalQuery {
            x: 4,
            y: 13,
            data_area: 5,
            grid_baselength: 5000,
            minute_length: 10,
        };
        // origin (15, 60) + 1.5*5 = (22.5, 67.5)
        assert_eq!(query.center_lon_lat(), (22.5, 67.5));
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
                crate::world_map::land_rings().len()
            );
            // Light basemap: very light gray water, light gray land.
            assert!(svg.contains(WATER_FILL), "{svg}");
            assert!(svg.contains(LAND_FILL), "{svg}");
            // Cells are semi-transparent so the basemap stays visible.
            assert!(svg.contains("fill-opacity=\"0.55\""), "{svg}");
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

    /// Count the raster `<rect>`s in a rendered map (excluding the background
    /// rectangle that fills the whole viewport).
    fn raster_rects(svg: &str) -> usize {
        let cells = svg.split("<g class=\"squares\"").nth(1).unwrap_or("");
        cells.matches("<rect ").count()
    }

    /// The square shade scales with the absolute raster-cell count.
    #[test]
    fn square_colour_is_a_shade_ramp() {
        // A lone query is the palest (second) shade, not the densest.
        assert_eq!(square_colour(1), "#ffc552");
        // The ramp darkens monotonically and saturates at the deepest shade.
        assert_ne!(square_colour(1), square_colour(2));
        assert_ne!(square_colour(2), square_colour(4));
        assert_eq!(square_colour(SQUARE_RAMP_MAX), "#b02a2c");
        assert_eq!(square_colour(SQUARE_RAMP_MAX + 100), "#b02a2c");
        // Zero (only reachable for a defensive empty tile) is the palest shade.
        assert_eq!(square_colour(0), "#ffe08a");
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

    /// The HTML report is a standalone document containing the map and tables.
    #[test]
    fn html_report_is_standalone_and_structured() {
        let outcome = parse_content(ISSUE_SAMPLE);
        let stats = aggregate(&outcome.rows, 10);
        let html = render_html("2023-11-14", &stats);

        assert!(html.starts_with("<!DOCTYPE html>"), "{html}");
        assert!(html.trim_end().ends_with("</html>"));
        assert!(html.contains("<style>"), "styling must be inline");
        assert!(html.contains("servicelog statistics for 2023-11-14"));
        assert!(html.contains("<div class=\"label\">total</div>"));
        // Two separate SVG world maps with the light continent basemap and the
        // background/interactive split.
        assert_eq!(html.matches("<svg").count(), 2, "one map per category");
        assert!(html.contains("class=\"basemap\""));
        assert!(html.contains(WATER_FILL), "very light gray water");
        assert!(html.contains(LAND_FILL), "light gray land");
        assert!(html.contains("data-set=\"background (offline)\""));
        assert!(html.contains("data-set=\"interactive\""));
        assert!(
            html.contains("fill-opacity=\"0.55\""),
            "transparent squares"
        );
        // The top lists are rendered as tables.
        assert!(html.contains("Top countries"));
        assert!(html.contains("Top cities"));
        assert!(html.contains("Top client versions"));
        assert!(html.contains("data_area distribution (local queries)"));
        // No external resources: the document is self-contained (the only URLs are
        // the SVG namespace declarations, which are not fetched).
        assert!(!html.contains("href="), "{html}");
        assert!(!html.contains("src="), "{html}");
        assert!(!html.contains("<link"), "{html}");
        assert_eq!(
            html.matches("http").count(),
            2,
            "only the two SVG xmlns URLs"
        );
    }

    /// Labels from the log are HTML-escaped so a crafted city cannot inject
    /// markup into the report.
    #[test]
    fn html_report_escapes_labels() {
        let stats = ServiceLogStats {
            total_requests: 1,
            countries: vec![TopEntry {
                label: "<script>alert(1)</script>".to_string(),
                count: 1,
            }],
            ..ServiceLogStats::default()
        };
        let html = render_html("2023-11-14", &stats);
        assert!(!html.contains("<script>alert"), "{html}");
        assert!(
            html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "{html}"
        );
    }

    /// `escape_html` covers the five metacharacters.
    #[test]
    fn escape_html_covers_metacharacters() {
        assert_eq!(
            escape_html("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&#39;f"
        );
    }

    #[test]
    fn day_is_parsed_from_filename() {
        assert_eq!(
            day_from_filename(Path::new("/var/log/blitzortung/servicelog_2023-11-14")),
            Some("2023-11-14".to_string())
        );
        assert_eq!(day_from_filename(Path::new("/tmp/other.log")), None);
        assert_eq!(day_from_filename(Path::new("/tmp/servicelog_bad")), None);
    }

    #[test]
    fn statistics_for_a_real_file() {
        let dir = std::env::temp_dir().join(format!("bo-stats-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("servicelog_2023-11-14");
        std::fs::write(&path, ISSUE_SAMPLE).unwrap();

        let (outcome, stats) = statistics_for_file(&path, 10).unwrap();
        assert_eq!(outcome.rows.len(), 6);
        assert_eq!(stats.total_requests, 6);
        assert_eq!(day_from_filename(&path).as_deref(), Some("2023-11-14"));

        let report = day_report("2023-11-14".to_string(), &outcome, 10);
        assert_eq!(report.malformed, 0);
        assert_eq!(report.stats.total_requests, 6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scale_handles_degenerate_range() {
        assert_eq!(scale(5.0, 5.0, 5.0, 0.0, 100.0), 50.0);
        assert_eq!(scale(0.0, 0.0, 10.0, 0.0, 100.0), 0.0);
        assert_eq!(scale(10.0, 0.0, 10.0, 0.0, 100.0), 100.0);
    }

    // --- ASCII world map (issue #24) ---------------------------------------

    /// A `data_area=5` query marks exactly one base cell at its origin.
    #[test]
    fn ascii_map_data_area_5_marks_one_cell() {
        let mut map = AsciiWorldMap::new();
        map.add_local_query(2, 10, 5);
        assert_eq!(map.queries(), 1);
        assert_eq!(map.total(), 1);
        assert_eq!(map.maximum(), 1);
        // Origin (5E, 45N) -> col floor((5+180)/5)=37, row floor((45+90)/5)=27.
        assert_eq!(map.count(37, 27), 1);
    }

    /// `data_area=10` (2x2), `15` (3x3) and `20` (4x4) blocks, anchored at the
    /// grid origin, incrementing every covered cell.
    #[test]
    fn ascii_map_data_area_scales_the_footprint() {
        for (data_area, side) in [(10_i64, 2_usize), (15, 3), (20, 4)] {
            let mut map = AsciiWorldMap::new();
            map.add_local_query(2, 1, data_area);
            assert_eq!(map.queries(), 1);
            assert_eq!(
                map.total(),
                (side * side) as u64,
                "data_area={data_area} should cover {side}x{side}"
            );
            // Origin (data_area, 0) -> the block starts at its lower-left cell.
            let col0 = (data_area + 180) / 5;
            let row0 = 90 / 5;
            for d in 0..side {
                assert_eq!(map.count(col0 as usize + d, row0 as usize), 1);
            }
            // The cell (side, 0) is outside the block.
            assert_eq!(map.count(col0 as usize + side, row0 as usize), 0);
        }
    }

    /// Two overlapping queries accumulate counts ("added on top").
    #[test]
    fn ascii_map_overlapping_queries_accumulate() {
        let mut map = AsciiWorldMap::new();
        // A 2x2 block anchored at (10E, 0N): cells cols 38..39, rows 18..19.
        map.add_local_query(2, 1, 10);
        // A 1x1 query at (15E, 5N): col 39, row 19 -- inside the 2x2 block.
        map.add_local_query(4, 2, 5);
        assert_eq!(map.queries(), 2);
        assert_eq!(map.total(), 5); // 4 + 1
        assert_eq!(map.count(39, 19), 2);
        assert_eq!(map.maximum(), 2);
    }

    /// Longitudes wrap across the antimeridian; rows outside the poles are
    /// clamped away rather than counted.
    #[test]
    fn ascii_map_wraps_longitude_and_drops_out_of_range_latitude() {
        let mut map = AsciiWorldMap::new();
        // x=73, data_area=5 -> origin lon = 72*5 = 360 -> wraps to 0 (col 36).
        map.add_local_query(73, 10, 5);
        assert_eq!(map.count(36, 27), 1);

        // Far-north tile: y=37, data_area=5 -> lat0 = 180 -> row 54, out of range.
        let mut polar = AsciiWorldMap::new();
        polar.add_local_query(2, 37, 5);
        assert_eq!(polar.total(), 0, "out-of-range latitude is not counted");

        // Southern hemisphere: y=-1, data_area=5 -> lat0=-10 -> row 16.
        let mut south = AsciiWorldMap::new();
        south.add_local_query(2, -1, 5);
        assert_eq!(south.total(), 1);
        assert_eq!(south.count((5 + 180) / 5, 16), 1);
    }

    /// A missing/`-` data_area falls back to the 5-degree minimum.
    #[test]
    fn ascii_map_missing_data_area_defaults_to_minimum() {
        let mut map = AsciiWorldMap::new();
        map.add_local_query(2, 10, 0);
        assert_eq!(map.total(), 1);
    }

    /// The map is `WORLD_COLS` wide and `WORLD_ROWS` tall, framed, north-up.
    #[test]
    fn ascii_map_render_dimensions_and_orientation() {
        let mut map = AsciiWorldMap::new();
        map.add_local_query(2, 10, 5); // 45N -> south-up row 27

        let rendered = map.render();
        let lines: Vec<&str> = rendered.lines().collect();
        let border = lines.iter().find(|l| l.starts_with('+')).unwrap();
        assert_eq!(border.len(), WORLD_COLS + 2);

        let body: Vec<&&str> = lines
            .iter()
            .filter(|l| l.starts_with('|') && l.ends_with('|'))
            .collect();
        assert_eq!(body.len(), WORLD_ROWS);
        for row in &body {
            assert_eq!(row.len(), WORLD_COLS + 2, "row: {row}");
        }

        // North-up: the marker (south-up row 27) is printed at line index
        // `WORLD_ROWS - 1 - 27` within the body.
        let expected_line = WORLD_ROWS - 1 - 27;
        assert!(
            body[expected_line].contains('#'),
            "marker not at expected line: {expected_line}"
        );
        // No marker south of the equator.
        for (index, row) in body.iter().enumerate() {
            if index > expected_line {
                assert!(
                    !row.contains(|c: char| c != ' ' && c != '|'),
                    "unexpected marker in line {index}: {row}"
                );
            }
        }
    }

    /// The renderer includes a header, a frame and a legend.
    #[test]
    fn ascii_map_render_has_header_and_legend() {
        let mut map = AsciiWorldMap::new();
        map.add_local_query(2, 10, 5);
        let rendered = map.render();
        assert!(rendered.contains("local query world map (72x36 cells of 5 degrees"));
        assert!(rendered.contains("1 queries, 1 hits"));
        assert!(rendered.contains("legend:"));
        assert!(rendered.contains("columns 5 degrees from 180W"));
    }

    /// `render_ascii_map` frames the day and builds the offline + interactive maps.
    #[test]
    fn ascii_map_from_stats_renders_the_day() {
        let outcome = parse_content(ISSUE_SAMPLE);
        let stats = aggregate(&outcome.rows, 10);
        let rendered = render_ascii_map("2023-11-14", &stats);
        assert!(rendered.starts_with("servicelog local-query maps for 2023-11-14"));
        // Four offline local queries (`minute_length=10`) and one interactive
        // (the Berlin row, `minute_length=60`).
        assert!(rendered.contains("offline queries (minute_length == 10):"));
        assert!(rendered.contains(
            "offline local-query world map (72x36 cells of 5 degrees, 4 queries, 4 hits)"
        ));
        assert!(rendered.contains("interactive queries (minute_length > 10):"));
        assert!(rendered.contains(
            "interactive local-query world map (72x36 cells of 5 degrees, 1 queries, 1 hits)"
        ));
    }

    /// An empty map renders a blank frame without panicking.
    #[test]
    fn ascii_map_empty_is_blank() {
        let map = AsciiWorldMap::new();
        let rendered = map.render();
        assert!(rendered.contains("0 queries, 0 hits"));
        // No non-space symbol inside any body row.
        let marks: usize = rendered
            .lines()
            .filter(|l| l.starts_with('|'))
            .map(|l| l.chars().filter(|c| *c != ' ' && *c != '|').count())
            .sum();
        assert_eq!(marks, 0);
    }

    // --- offline / interactive split ---------------------------------------

    /// A local row builder with an explicit `minute_length`.
    fn local_row_minutes(x: i64, y: i64, data_area: &str, minute_length: i64) -> String {
        format!(
            "08:00:00.000\tDE\tCity\t\t\t\tA\t352\t0\t{minute_length}\t5000\t-1\t0\t{x}\t{y}\t{data_area}\t0.000"
        )
    }

    #[test]
    fn local_query_classifies_offline_and_interactive() {
        let offline = LocalQuery {
            x: 1,
            y: 1,
            data_area: 5,
            grid_baselength: 5000,
            minute_length: 10,
        };
        assert!(offline.is_offline());
        assert!(!offline.is_interactive());

        for len in [11, 60, 120] {
            let interactive = LocalQuery {
                minute_length: len,
                ..offline
            };
            assert!(interactive.is_interactive(), "minute_length {len}");
            assert!(!interactive.is_offline(), "minute_length {len}");
        }
    }

    /// `aggregate` counts offline (`minute_length == 10`) and interactive
    /// (`minute_length > 10`) local requests separately.
    #[test]
    fn aggregate_counts_offline_and_interactive() {
        let content = [
            local_row_minutes(1, 1, "5", 10),
            local_row_minutes(2, 1, "5", 10),
            local_row_minutes(3, 1, "5", 60),
            local_row_minutes(4, 1, "5", 1440),
        ]
        .join("\n");
        let stats = aggregate(&parse_content(&content).rows, 10);
        assert_eq!(stats.local_requests, 4);
        assert_eq!(stats.offline_requests, 2);
        assert_eq!(stats.interactive_requests, 2);
    }

    /// The two maps separate the offline and interactive queries.
    #[test]
    fn separate_maps_for_offline_and_interactive() {
        let content = [
            // Offline at (5E, 45N) -> col 37, row 27.
            local_row_minutes(2, 10, "5", 10),
            // Interactive at (5E, 45N) too, so the same cell appears in both.
            local_row_minutes(2, 10, "5", 60),
        ]
        .join("\n");
        let stats = aggregate(&parse_content(&content).rows, 10);

        let offline =
            AsciiWorldMap::from_local_queries_filtered(&stats.local_queries, |q| q.is_offline());
        let interactive = AsciiWorldMap::from_local_queries_filtered(&stats.local_queries, |q| {
            q.is_interactive()
        });
        assert_eq!(offline.queries(), 1);
        assert_eq!(offline.count(37, 27), 1);
        assert_eq!(interactive.queries(), 1);
        assert_eq!(interactive.count(37, 27), 1);

        let rendered = render_ascii_maps(&stats);
        assert!(rendered.contains("offline local-query world map"));
        assert!(rendered.contains("interactive local-query world map"));
    }

    /// The JSON report marks each query and reports the split counts.
    #[test]
    fn json_reports_offline_and_interactive() {
        let content = [
            local_row_minutes(1, 1, "5", 10),
            local_row_minutes(2, 1, "5", 60),
        ]
        .join("\n");
        let stats = aggregate(&parse_content(&content).rows, 10);
        let value: serde_json::Value =
            serde_json::from_str(&render_json("2023-11-14", &stats)).unwrap();
        assert_eq!(value["offline_requests"], 1);
        assert_eq!(value["interactive_requests"], 1);
        assert_eq!(value["local_queries"][0]["interactive"], false);
        assert_eq!(value["local_queries"][0]["minute_length"], 10);
        assert_eq!(value["local_queries"][1]["interactive"], true);
        assert_eq!(value["local_queries"][1]["minute_length"], 60);
    }
}
