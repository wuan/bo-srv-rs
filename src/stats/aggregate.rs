//! Types and aggregation for the servicelog statistics.

use std::collections::HashMap;
use std::path::Path;

use crate::service_log::{QUERY_TYPE_CLUSTER, QUERY_TYPE_GRID};
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
/// `x`/`y` are the **client's requested tile** indices written in the servicelog
/// (`x = floor(lon / data_area)`, `y = floor(lat / data_area)`), so the tile
/// spans `[x * data_area, (x+1) * data_area)` degrees; `data_area` is the tile
/// size in degrees.  This is *not* the server's [`crate::geom::LocalGrid`]
/// `reference_longitude`/`reference_latitude` (the lower-left of the
/// `3 * data_area` neighbourhood grid used for the strike query).
/// `grid_baselength` is the raster baselength in metres, so an overlay can scale
/// each marker; `None` for a cluster request (logged as `-`).
///
/// `data_area` was added for the ASCII world map (issue #24): it determines the
/// footprint of a query in the base 5-degree raster (`data_area / 5` cells per
/// side).  It is `5` (the minimum) when the row did not carry a usable value.
///
/// `query_type` and `minute_length` classify the query as offline (background)
/// or interactive (see [`is_offline`](Self::is_offline) /
/// [`is_interactive`](Self::is_interactive)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalQuery {
    pub x: i64,
    pub y: i64,
    pub data_area: i64,
    pub grid_baselength: Option<i64>,
    /// The requested time span in minutes (`minute_length` in the servicelog).
    pub minute_length: i64,
    /// `G` ([`QUERY_TYPE_GRID`]) for a grid query, `C` ([`QUERY_TYPE_CLUSTER`])
    /// for a cluster query.
    pub query_type: char,
}

/// The `minute_length` of a **background (offline)** query: the cached widget
/// requests a fixed 10-minute window.  Only grid queries with this window are
/// background; a cluster query with a 10-minute window is interactive.
pub const OFFLINE_MINUTE_LENGTH: i64 = 10;

impl LocalQuery {
    /// Whether this is a grid query (`query_type == 'G'`).
    pub fn is_grid(&self) -> bool {
        self.query_type == QUERY_TYPE_GRID
    }

    /// Whether this is a cluster query (`query_type == 'C'`).
    pub fn is_cluster(&self) -> bool {
        self.query_type == QUERY_TYPE_CLUSTER
    }

    /// Whether this is a background (offline) query: a **grid** query with the
    /// fixed 10-minute window.  Cluster queries are never background.
    pub fn is_offline(&self) -> bool {
        self.is_grid() && self.minute_length == OFFLINE_MINUTE_LENGTH
    }

    /// Whether this is an interactive query: everything that is not
    /// background — a cluster query (even with a 10-minute window) or a grid
    /// query with any other window.
    pub fn is_interactive(&self) -> bool {
        !self.is_offline()
    }

    /// The tile centre as `(longitude, latitude)` degrees.
    ///
    /// `(x, y)` is the client's requested tile (`x = floor(lon / data_area)`),
    /// so the tile spans `[x * data_area, (x+1) * data_area)` and its centre is
    /// `(x * data_area + data_area / 2, y * data_area + data_area / 2)`.
    pub fn center_lon_lat(&self) -> (f64, f64) {
        let data_area = self.data_area.max(1) as f64;
        let lon = self.x as f64 * data_area + data_area / 2.0;
        let lat = self.y as f64 * data_area + data_area / 2.0;
        (lon, lat)
    }
}

/// The grid/cluster x local/global/region request-count matrix.
///
/// Rows are the query type (grid `G` / cluster `C`), columns the request
/// flavour (local `region == -1` / global `region == 0` / region
/// `region > 0`).  Every row lands in exactly one cell, so the row sums equal
/// [`ServiceLogStats::grid_requests`] / [`ServiceLogStats::cluster_requests`],
/// the column sums equal `local_requests` / `global_requests` /
/// [`ServiceLogStats::region_requests`], and the grand total equals
/// [`ServiceLogStats::total_requests`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryMatrix {
    /// Grid queries with `region == -1` (`get_local_strikes_grid`).
    pub grid_local: u64,
    /// Grid queries with `region == 0` (`get_global_strikes_grid`).
    pub grid_global: u64,
    /// Grid queries with `region > 0` (`get_strikes_grid` for a region).
    pub grid_region: u64,
    /// Cluster queries with `region == -1` (`get_local_clusters`).
    pub cluster_local: u64,
    /// Cluster queries with `region == 0` (`get_global_clusters`).
    pub cluster_global: u64,
    /// Cluster queries with `region > 0`.
    pub cluster_region: u64,
}

impl QueryMatrix {
    /// The `grid` row sum (local + global + region grid queries); equals
    /// [`ServiceLogStats::grid_requests`].
    pub fn grid_sum(&self) -> u64 {
        self.grid_local + self.grid_global + self.grid_region
    }

    /// The `cluster` row sum (local + global + region cluster queries); equals
    /// [`ServiceLogStats::cluster_requests`].
    pub fn cluster_sum(&self) -> u64 {
        self.cluster_local + self.cluster_global + self.cluster_region
    }

    /// The `local query` column sum (grid + cluster local queries); equals
    /// [`ServiceLogStats::local_requests`].
    pub fn local_sum(&self) -> u64 {
        self.grid_local + self.cluster_local
    }

    /// The `global query` column sum (grid + cluster global queries); equals
    /// [`ServiceLogStats::global_requests`].
    pub fn global_sum(&self) -> u64 {
        self.grid_global + self.cluster_global
    }

    /// The `region query` column sum (grid + cluster region queries); equals
    /// [`ServiceLogStats::region_requests`].
    pub fn region_sum(&self) -> u64 {
        self.grid_region + self.cluster_region
    }

    /// The grand total of the matrix (all six cells); equals
    /// [`ServiceLogStats::total_requests`].
    pub fn total(&self) -> u64 {
        self.grid_sum() + self.cluster_sum()
    }
}

/// The aggregated daily statistics.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServiceLogStats {
    /// The total number of parsed requests.
    pub total_requests: u64,
    /// Requests with `query_type == 'G'` (grid requests).
    pub grid_requests: u64,
    /// Requests with `query_type == 'C'` (cluster requests).
    pub cluster_requests: u64,
    /// Requests with `region == -1`.
    pub local_requests: u64,
    /// Requests with `region == 0`.
    pub global_requests: u64,
    /// Requests with `region > 0`.
    pub region_requests: u64,
    /// The grid/cluster x local/global/region request-count matrix; its grand
    /// total equals [`ServiceLogStats::total_requests`].
    pub query_matrix: QueryMatrix,
    /// Local grid requests with a 10-minute window (the background/offline
    /// widget).
    pub offline_requests: u64,
    /// Local requests that are not background: cluster queries (even with a
    /// 10-minute window) and grid queries with any other window.
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
        if row.is_grid() {
            stats.grid_requests += 1;
            if row.is_local() {
                stats.query_matrix.grid_local += 1;
            } else if row.is_global() {
                stats.query_matrix.grid_global += 1;
            } else if row.is_region() {
                stats.query_matrix.grid_region += 1;
            }
        } else if row.is_cluster() {
            stats.cluster_requests += 1;
            if row.is_local() {
                stats.query_matrix.cluster_local += 1;
            } else if row.is_global() {
                stats.query_matrix.cluster_global += 1;
            } else if row.is_region() {
                stats.query_matrix.cluster_region += 1;
            }
        }
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
                    query_type: row.query_type,
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
