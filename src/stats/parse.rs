//! Parsing of the `servicelog_YYYY-MM-DD` usage-log lines.
//!
//! The `city` column is tab-padded (see [`crate::service_log::pad_city`]), so a
//! naive `split('\t')` yields empty segments.  [`parse_row`] therefore splits on
//! tabs and **drops empty segments**, which recovers exactly the 15 logical
//! fields.  A row that does not yield 15 fields is reported as malformed and
//! skipped (the parser never panics on a truncated or hand-edited file).
//!
//! `-` is the placeholder for "unknown" in the country/city/local columns and
//! for the grid-only fields of a cluster row, and is normalised to `None`, so
//! the top lists never contain a `-` bucket.

use std::path::Path;

use crate::service_log::{QUERY_TYPE_CLUSTER, QUERY_TYPE_GRID};

/// The number of logical fields in a servicelog row.
pub const SERVICELOG_FIELDS: usize = 15;

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
    /// `A` for Android, `I` for iOS, `-` for anything else.
    pub platform: String,
    /// The parsed `bo-android-<n>` / `bo-ios-<n>` version, else `None`.
    pub version: Option<i64>,
    /// `G` ([`QUERY_TYPE_GRID`]) for a grid request, `C`
    /// ([`QUERY_TYPE_CLUSTER`]) for a cluster request.
    pub query_type: char,
    pub minute_offset: i64,
    pub minute_length: i64,
    /// Pre-clamp raster baselength for a grid request, else `None` (cluster).
    pub grid_baselength: Option<i64>,
    /// `0` global, clamped region for the region grid, `-1` local.
    pub region: i64,
    /// Grid request only, else `None` (cluster).
    pub count_threshold: Option<i64>,
    /// Local centre `x`, else `None`.
    pub x: Option<i64>,
    /// Local centre `y`, else `None`.
    pub y: Option<i64>,
    /// Local data area, else `None`.
    pub data_area: Option<i64>,
    /// Raster fill percentage as written (three decimals), else `None`
    /// (cluster).
    pub fill: Option<f64>,
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

    /// Whether this is a grid request (`query_type == 'G'`).
    pub fn is_grid(&self) -> bool {
        self.query_type == QUERY_TYPE_GRID
    }

    /// Whether this is a cluster request (`query_type == 'C'`).
    pub fn is_cluster(&self) -> bool {
        self.query_type == QUERY_TYPE_CLUSTER
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
    // them recovers exactly the 15 logical fields.
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
        query_type: parse_query_type(fields[5])?,
        minute_offset: fields[6].parse().ok()?,
        minute_length: fields[7].parse().ok()?,
        grid_baselength: dash_to_int(fields[8]),
        region: fields[9].parse().ok()?,
        count_threshold: dash_to_int(fields[10]),
        x: dash_to_int(fields[11]),
        y: dash_to_int(fields[12]),
        data_area: dash_to_int(fields[13]),
        fill: dash_to_f64(fields[14]),
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

/// `-` becomes `None`; a float string becomes `Some`.
fn dash_to_f64(value: &str) -> Option<f64> {
    if value == "-" {
        None
    } else {
        value.parse().ok()
    }
}

/// The query-type column: exactly one character (`G` or `C` in practice).
fn parse_query_type(value: &str) -> Option<char> {
    let mut chars = value.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
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
