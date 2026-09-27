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
//!   of the local requests, for plotting on a world map (see [`crate::map`]:
//!   `render_ascii_maps` for the ASCII raster, `render_world_svg` for the
//!   geographic SVG maps).
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
/// request, always a multiple of [`crate::map::RASTER_DEGREES`] in practice).
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
    out.push_str(&crate::map::render_ascii_maps(stats));
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

fn top_entries_json(entries: &[TopEntry]) -> Vec<serde_json::Value> {
    entries
        .iter()
        .map(|e| serde_json::json!({ "label": e.label, "count": e.count }))
        .collect()
}

// --- Static HTML report (issue #28) ----------------------------------------

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
/// per-tile query count (see [`crate::map::render_world_svg`]).  All styling is
/// inline in a `<style>` block, so the file is self-contained and needs no
/// network access.
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
    let (offline_svg, interactive_svg) =
        crate::map::render_world_svg(stats, crate::map::MAP_WIDTH, crate::map::MAP_HEIGHT);
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
    let _ = write!(
        html,
        "  <p class=\"legend\">\
         <span><span class=\"swatch\" style=\"background:{}\"></span>water</span>\
         <span><span class=\"swatch\" style=\"background:{}\"></span>land</span>\
         <span>queries per square:</span>",
        crate::map::svg::WATER_FILL,
        crate::map::svg::LAND_FILL,
    );
    for (index, (r, g, b)) in crate::map::svg::SQUARE_RAMP.iter().enumerate() {
        let _ = write!(
            html,
            "<span class=\"swatch\" style=\"background:#{r:02x}{g:02x}{b:02x}\"></span>"
        );
        if index + 1 == crate::map::svg::SQUARE_RAMP.len() {
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
        assert!(
            html.contains(crate::map::svg::WATER_FILL),
            "very light gray water"
        );
        assert!(html.contains(crate::map::svg::LAND_FILL), "light gray land");
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
            crate::map::AsciiWorldMap::from_local_queries_filtered(&stats.local_queries, |q| {
                q.is_offline()
            });
        let interactive =
            crate::map::AsciiWorldMap::from_local_queries_filtered(&stats.local_queries, |q| {
                q.is_interactive()
            });
        assert_eq!(offline.queries(), 1);
        assert_eq!(offline.count(37, 27), 1);
        assert_eq!(interactive.queries(), 1);
        assert_eq!(interactive.count(37, 27), 1);

        let rendered = crate::map::render_ascii_maps(&stats);
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
