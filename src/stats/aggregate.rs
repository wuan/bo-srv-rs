//! Types and aggregation for the servicelog statistics.

use std::collections::HashMap;
use std::path::Path;

use crate::stats::parse::{parse_file, ParseOutcome, ServiceLogRow};

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
