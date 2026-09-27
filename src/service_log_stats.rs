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
//!   of the local requests, for plotting on a world map.
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

/// A local query location for the world-map overlay.
///
/// `x`/`y` are the UTM grid coordinates written as the local-grid centre and
/// `grid_baselength` is the raster baselength in metres, so an overlay can
/// scale each marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalQuery {
    pub x: i64,
    pub y: i64,
    pub grid_baselength: i64,
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
    /// Requests whose country was unknown (`-`).
    pub unknown_country: u64,
    /// Requests whose city was unknown (`-`).
    pub unknown_city: u64,
    /// Requests whose client version could not be determined.
    pub unknown_version: u64,
    pub countries: Vec<TopEntry>,
    pub cities: Vec<TopEntry>,
    pub versions: Vec<TopEntry>,
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
                stats.local_queries.push(LocalQuery {
                    x,
                    y,
                    grid_baselength: row.grid_baselength,
                });
            }
        }
    }

    stats.countries = take_top(countries, top_n);
    stats.cities = take_top(cities, top_n);
    stats.versions = take_top(versions, top_n);
    stats
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

    out.push_str(&format!(
        "\nlocal query locations: {} (with x/y)\n",
        stats.local_queries.len()
    ));
    out
}

/// Render the statistics as pretty-printed JSON.
pub fn render_json(day: &str, stats: &ServiceLogStats) -> String {
    let value = serde_json::json!({
        "day": day,
        "total_requests": stats.total_requests,
        "local_requests": stats.local_requests,
        "global_requests": stats.global_requests,
        "region_requests": stats.region_requests,
        "unknown_country": stats.unknown_country,
        "unknown_city": stats.unknown_city,
        "unknown_version": stats.unknown_version,
        "countries": top_entries_json(&stats.countries),
        "cities": top_entries_json(&stats.cities),
        "versions": top_entries_json(&stats.versions),
        "local_queries": stats.local_queries.iter().map(|q| serde_json::json!({
            "x": q.x,
            "y": q.y,
            "grid_baselength": q.grid_baselength,
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
                grid_baselength: 5000
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
}
