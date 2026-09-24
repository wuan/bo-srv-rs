//! Metrics instrumentation, ported from `blitzortung/service/metrics.py`.
//!
//! The Python service sends counters/gauges/timings to a StatsD daemon under
//! the `org.blitzortung.service` prefix.  [`StatsDMetrics`] does the same over
//! UDP against a local StatsD receiver (default `localhost:8125`); a no-op
//! implementation (the spec allows metrics to be absent) plus a recording
//! implementation used by tests round out the trait, so the metric *names*
//! that surface during dispatch stay traceable.

use std::io::ErrorKind;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};

/// The StatsD metric name components, mirroring the module-level constants of
/// `blitzortung/service/metrics.py`.
pub mod name {
    pub const STRIKES_GRID: &str = "strikes_grid";
    pub const STRIKES_GRID_QUERY: &str = "strikes_grid_query";
    pub const GLOBAL_STRIKES_GRID: &str = "global_strikes_grid";
    pub const LOCAL_STRIKES_GRID: &str = "local_strikes_grid";
    pub const HISTOGRAM: &str = "histogram";
    pub const DB: &str = "db";

    pub const TOTAL_COUNT: &str = "total_count";
    pub const BG_COUNT: &str = "bg_count";
    pub const CACHE_HITS: &str = "cache_hits";
    pub const DATA_AREA: &str = "data_area";
    pub const SIZE: &str = "size";
    pub const POOL_WAIT: &str = "pool_wait";
    pub const TOTAL: &str = "total";
    pub const QUERY: &str = "query";

    /// Metric-name leaves used by the importer CLIs (`blitzortung/cli`).
    ///
    /// The importer tools send under the `org.blitzortung.import` prefix and
    /// use a flat `strikes...` namespace rather than the service's
    /// `strikes_grid` one.
    pub const STRIKES: &str = "strikes";
    pub const COUNT: &str = "count";
    pub const GET: &str = "get";
    pub const INSERT: &str = "insert";
    pub const ERROR_COUNT: &str = "error_count";
    pub const DELAY: &str = "delay";
    pub const IMPORTED: &str = "imported";
}

/// `StatsDMetrics.name`: join the metric components with `.`.
fn metric_name(parts: &[&str]) -> String {
    parts.join(".")
}

/// Increment/gauge/timing sink for the service.
pub trait Metrics: Send + Sync {
    fn incr(&self, _key: &str, _value: u64) {}
    fn gauge(&self, _key: &str, _value: u64) {}
    fn timing(&self, _key: &str, _value: u64) {}
    /// Gauge with a fractional value (`StatsClient.gauge` takes a float).
    fn gauge_f64(&self, _key: &str, _value: f64) {}

    /// `StatsDMetrics.for_strikes(minute_length, region, cache_ratio)`.
    fn for_strikes(&self, minute_length: i64, region: i64, cache_ratio: f64) {
        self.incr(&metric_name(&[name::STRIKES_GRID, name::TOTAL_COUNT]), 1);
        self.incr(
            &metric_name(&[name::STRIKES_GRID, name::TOTAL_COUNT, &region.to_string()]),
            1,
        );
        self.gauge_f64(
            &metric_name(&[name::STRIKES_GRID, name::CACHE_HITS]),
            cache_ratio,
        );
        if minute_length == 10 {
            self.incr(&metric_name(&[name::STRIKES_GRID, name::BG_COUNT]), 1);
            self.incr(
                &metric_name(&[name::STRIKES_GRID, name::BG_COUNT, &region.to_string()]),
                1,
            );
        }
    }

    /// `StatsDMetrics.for_global_strikes(minute_length, cache_ratio)`.
    fn for_global_strikes(&self, minute_length: i64, cache_ratio: f64) {
        self.incr(&metric_name(&[name::STRIKES_GRID, name::TOTAL_COUNT]), 1);
        self.incr(
            &metric_name(&[name::GLOBAL_STRIKES_GRID, name::TOTAL_COUNT]),
            1,
        );
        self.gauge_f64(
            &metric_name(&[name::GLOBAL_STRIKES_GRID, name::CACHE_HITS]),
            cache_ratio,
        );
        if minute_length == 10 {
            self.incr(&metric_name(&[name::STRIKES_GRID, name::BG_COUNT]), 1);
            self.incr(
                &metric_name(&[name::GLOBAL_STRIKES_GRID, name::BG_COUNT]),
                1,
            );
        }
    }

    /// `StatsDMetrics.for_local_strikes(minute_length, data_area,
    /// cache_ratio)`.
    fn for_local_strikes(&self, minute_length: i64, data_area: i64, cache_ratio: f64) {
        self.incr(&metric_name(&[name::STRIKES_GRID, name::TOTAL_COUNT]), 1);
        self.incr(
            &metric_name(&[name::LOCAL_STRIKES_GRID, name::TOTAL_COUNT]),
            1,
        );
        self.incr(
            &metric_name(&[
                name::LOCAL_STRIKES_GRID,
                name::DATA_AREA,
                &data_area.to_string(),
            ]),
            1,
        );
        self.gauge_f64(
            &metric_name(&[name::LOCAL_STRIKES_GRID, name::CACHE_HITS]),
            cache_ratio,
        );
        if minute_length == 10 {
            self.incr(&metric_name(&[name::STRIKES_GRID, name::BG_COUNT]), 1);
            self.incr(&metric_name(&[name::LOCAL_STRIKES_GRID, name::BG_COUNT]), 1);
        }
    }

    /// `StatsDMetrics.for_histogram(cache_ratio, cache_size)`.
    fn for_histogram(&self, cache_ratio: f64, cache_size: usize) {
        self.incr(
            &metric_name(&[name::HISTOGRAM, name::QUERY, name::COUNT]),
            1,
        );
        self.gauge_f64(
            &metric_name(&[name::HISTOGRAM, name::CACHE_HITS]),
            cache_ratio,
        );
        self.gauge(
            &metric_name(&[name::HISTOGRAM, name::SIZE]),
            cache_size as u64,
        );
    }

    /// `StatsDMetrics.for_db_pool_wait(wait_seconds)`: report the wait as a
    /// timing in milliseconds, clamped to at least 1ms so sub-millisecond
    /// waits stay visible.
    fn for_db_pool_wait(&self, wait_seconds: f64) {
        let millis = ((wait_seconds * 1000.0) as i64).max(1) as u64;
        self.timing(&metric_name(&[name::DB, name::POOL_WAIT]), millis);
    }

    /// `TimingState.log_timing('<grid>.total')`: report how long a grid
    /// request took from the start of the producer to the fully built
    /// response, as a timing in milliseconds clamped to at least 1ms (the
    /// Python `get_milliseconds`).
    ///
    /// `grid` is the grid metric name: `strikes_grid` for the region and local
    /// flavours, `global_strikes_grid` for the global one.
    fn for_grid_total(&self, grid: &str, elapsed_seconds: f64) {
        let millis = ((elapsed_seconds * 1000.0) as i64).max(1) as u64;
        self.timing(&metric_name(&[grid, name::TOTAL]), millis);
    }

    /// Count one grid database query (`strikes_grid_query.count`).
    fn for_grid_query(&self) {
        self.incr(&metric_name(&[name::STRIKES_GRID_QUERY, name::COUNT]), 1);
    }

    /// `cli/imprt.py::import_strikes_for`: report one region's import run.
    ///
    /// Increments `strikes.<region>`, gauges `strikes.<region>.count` and
    /// reports the fetch/insert phases as millisecond timings clamped to at
    /// least `1` (the Python `max(1, int(seconds * 1000))`).
    fn for_import(&self, region: u32, strike_count: u64, get_seconds: f64, insert_seconds: f64) {
        let region = region.to_string();
        self.incr(&metric_name(&[name::STRIKES, &region]), 1);
        self.gauge(
            &metric_name(&[name::STRIKES, &region, name::COUNT]),
            strike_count,
        );
        self.timing(
            &metric_name(&[name::STRIKES, &region, name::GET]),
            seconds_to_millis(get_seconds),
        );
        self.timing(
            &metric_name(&[name::STRIKES, &region, name::INSERT]),
            seconds_to_millis(insert_seconds),
        );
    }

    /// `cli/imprt.py::import_strikes`: gauge the accumulated error count as
    /// `strikes.error_count`.
    fn for_import_error_count(&self, error_count: u64) {
        self.gauge(
            &metric_name(&[name::STRIKES, name::ERROR_COUNT]),
            error_count,
        );
    }

    /// `cli/imprt_websocket.py::on_message`: count one live strike and gauge
    /// its local delay in seconds (`strikes`, `strikes.delay`).
    fn for_websocket_strike(&self, local_delay: f64) {
        self.incr(name::STRIKES, 1);
        self.gauge_f64(&metric_name(&[name::STRIKES, name::DELAY]), local_delay);
    }

    /// `cli/update.py::update_strikes`: gauge the number of inserted strikes
    /// as `strikes.imported`.
    fn for_update_imported(&self, insert_count: u64) {
        self.gauge(&metric_name(&[name::STRIKES, name::IMPORTED]), insert_count);
    }
}

/// Seconds to whole milliseconds, clamped to at least `1`
/// (`max(1, int(seconds * 1000))` in the Python importers).
fn seconds_to_millis(seconds: f64) -> u64 {
    ((seconds * 1000.0) as i64).max(1) as u64
}

/// Metrics that drop everything.
pub struct NoopMetrics;

impl Metrics for NoopMetrics {}

/// Let a trait object be used anywhere a [`Metrics`] implementation is wanted
/// (e.g. the service picks `StatsDMetrics` at runtime and falls back to
/// [`NoopMetrics`] when the receiver cannot be reached).
impl Metrics for std::sync::Arc<dyn Metrics> {
    fn incr(&self, key: &str, value: u64) {
        (**self).incr(key, value);
    }

    fn gauge(&self, key: &str, value: u64) {
        (**self).gauge(key, value);
    }

    fn gauge_f64(&self, key: &str, value: f64) {
        (**self).gauge_f64(key, value);
    }

    fn timing(&self, key: &str, value: u64) {
        (**self).timing(key, value);
    }

    fn for_strikes(&self, minute_length: i64, region: i64, cache_ratio: f64) {
        (**self).for_strikes(minute_length, region, cache_ratio);
    }

    fn for_global_strikes(&self, minute_length: i64, cache_ratio: f64) {
        (**self).for_global_strikes(minute_length, cache_ratio);
    }

    fn for_local_strikes(&self, minute_length: i64, data_area: i64, cache_ratio: f64) {
        (**self).for_local_strikes(minute_length, data_area, cache_ratio);
    }

    fn for_histogram(&self, cache_ratio: f64, cache_size: usize) {
        (**self).for_histogram(cache_ratio, cache_size);
    }

    fn for_db_pool_wait(&self, wait_seconds: f64) {
        (**self).for_db_pool_wait(wait_seconds);
    }

    fn for_grid_total(&self, grid: &str, elapsed_seconds: f64) {
        (**self).for_grid_total(grid, elapsed_seconds);
    }

    fn for_grid_query(&self) {
        (**self).for_grid_query();
    }

    fn for_import(&self, region: u32, strike_count: u64, get_seconds: f64, insert_seconds: f64) {
        (**self).for_import(region, strike_count, get_seconds, insert_seconds);
    }

    fn for_import_error_count(&self, error_count: u64) {
        (**self).for_import_error_count(error_count);
    }

    fn for_websocket_strike(&self, local_delay: f64) {
        (**self).for_websocket_strike(local_delay);
    }

    fn for_update_imported(&self, insert_count: u64) {
        (**self).for_update_imported(insert_count);
    }
}

/// Default StatsD receiver address (`StatsClient('localhost', 8125)`).
pub const DEFAULT_STATSD_HOST: &str = "localhost";
pub const DEFAULT_STATSD_PORT: u16 = 8125;
/// `StatsClient(..., prefix='org.blitzortung.service')`.
pub const DEFAULT_STATSD_PREFIX: &str = "org.blitzortung.service";
/// `StatsClient(..., prefix='org.blitzortung.import')`: the prefix used by the
/// importer CLIs (`cli/imprt.py`, `cli/imprt_websocket.py`, `cli/update.py`).
pub const IMPORT_STATSD_PREFIX: &str = "org.blitzortung.import";

/// Render a float the way StatsD strings are written by the Python
/// `statsd` client: no exponent for ordinary values, and no trailing `.0`.
fn fmt_f64(value: f64) -> String {
    if value.is_finite() && value == value.trunc() && value.abs() < 1e15 {
        // `0.0` -> "0", `1.0` -> "1".
        format!("{}", value as i64)
    } else {
        value.to_string()
    }
}

/// Metrics sink that sends StatsD datagrams over UDP, mirroring
/// `blitzortung/service/metrics.py::StatsDMetrics`.
///
/// StatsD is fire-and-forget: a send error (or a missing daemon) is logged and
/// swallowed so instrumentation can never break request handling.
pub struct StatsDMetrics {
    socket: UdpSocket,
    prefix: String,
    /// The target address, kept for diagnostics/logging.
    target: SocketAddr,
}

impl StatsDMetrics {
    /// Connect to the default local StatsD receiver (`localhost:8125`) using
    /// the `org.blitzortung.service` prefix.
    pub fn new() -> std::io::Result<Self> {
        Self::with_address(DEFAULT_STATSD_HOST, DEFAULT_STATSD_PORT)
    }

    /// Connect to a specific receiver, resolving `host:port` synchronously.
    pub fn with_address(host: &str, port: u16) -> std::io::Result<Self> {
        Self::with_address_and_prefix(host, port, DEFAULT_STATSD_PREFIX)
    }

    /// Connect to a receiver with an explicit metric prefix.
    pub fn with_address_and_prefix(
        host: &str,
        port: u16,
        prefix: impl Into<String>,
    ) -> std::io::Result<Self> {
        let target = (host, port).to_socket_addrs()?.next().ok_or_else(|| {
            std::io::Error::new(ErrorKind::AddrNotAvailable, "no StatsD address resolved")
        })?;
        // A `connect`ed UDP socket picks the local ephemeral port and lets us
        // `send` without repeating the destination; StatsD never replies.
        let socket = UdpSocket::bind(("0.0.0.0", 0))?;
        socket.connect(target)?;
        Ok(StatsDMetrics {
            socket,
            prefix: prefix.into(),
            target,
        })
    }

    /// The resolved receiver address.
    pub fn target(&self) -> SocketAddr {
        self.target
    }

    /// Build a fully-qualified StatsD name: `<prefix>.<key>`.
    fn name(&self, key: &str) -> String {
        if self.prefix.is_empty() {
            key.to_string()
        } else {
            format!("{}.{}", self.prefix, key)
        }
    }

    /// Send one StatsD payload (`<name>:<value>|<type>`).
    fn send(&self, key: &str, value: &str, kind: &str) {
        let payload = format!("{}:{value}|{kind}", self.name(key));
        if let Err(error) = self.socket.send(payload.as_bytes()) {
            log::debug!("failed to send StatsD datagram to {}: {error}", self.target);
        }
    }
}

impl Metrics for StatsDMetrics {
    fn incr(&self, key: &str, value: u64) {
        self.send(key, &value.to_string(), "c");
    }

    fn gauge(&self, key: &str, value: u64) {
        self.send(key, &value.to_string(), "g");
    }

    fn gauge_f64(&self, key: &str, value: f64) {
        self.send(key, &fmt_f64(value), "g");
    }

    fn timing(&self, key: &str, value: u64) {
        self.send(key, &value.to_string(), "ms");
    }
}

/// Metrics that record every StatsD line for assertions.
///
/// The `for_*` helpers are the trait defaults (which build the metric names),
/// so a recording sink observes exactly what [`StatsDMetrics`] would send.
#[derive(Debug, Default)]
pub struct RecordingMetrics {
    lines: std::sync::Mutex<Vec<String>>,
}

impl RecordingMetrics {
    pub fn new() -> Self {
        RecordingMetrics::default()
    }

    /// A snapshot of the recorded `name:value|type` lines, in send order.
    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    fn record(&self, line: String) {
        self.lines.lock().unwrap().push(line);
    }
}

impl Metrics for RecordingMetrics {
    fn incr(&self, key: &str, value: u64) {
        self.record(format!("{key}:{value}|c"));
    }

    fn gauge(&self, key: &str, value: u64) {
        self.record(format!("{key}:{value}|g"));
    }

    fn gauge_f64(&self, key: &str, value: f64) {
        self.record(format!("{key}:{}|g", fmt_f64(value)));
    }

    fn timing(&self, key: &str, value: u64) {
        self.record(format!("{key}:{value}|ms"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_metrics_are_silent() {
        let metrics = NoopMetrics;
        metrics.for_strikes(60, 1, 0.5);
        metrics.for_global_strikes(60, 0.25);
        metrics.for_local_strikes(60, 5, 0.0);
        metrics.for_histogram(0.0, 0);
        metrics.for_db_pool_wait(0.01);
        metrics.for_grid_total(name::STRIKES_GRID, 0.01);
        metrics.for_grid_query();
    }

    #[test]
    fn recording_metrics_keep_calls() {
        let metrics = RecordingMetrics::new();
        metrics.for_strikes(15, 2, 0.5);
        // strikes_grid.total_count + strikes_grid.total_count.<region>
        assert_eq!(
            metrics.lines(),
            vec![
                "strikes_grid.total_count:1|c".to_string(),
                "strikes_grid.total_count.2:1|c".to_string(),
                "strikes_grid.cache_hits:0.5|g".to_string(),
            ]
        );
    }

    /// `for_strikes` with a 10-minute length also bumps the background counts.
    #[test]
    fn for_strikes_reports_bg_count_for_ten_minutes() {
        let metrics = RecordingMetrics::new();
        metrics.for_strikes(10, 1, 0.75);
        assert_eq!(
            metrics.lines(),
            vec![
                "strikes_grid.total_count:1|c".to_string(),
                "strikes_grid.total_count.1:1|c".to_string(),
                "strikes_grid.cache_hits:0.75|g".to_string(),
                "strikes_grid.bg_count:1|c".to_string(),
                "strikes_grid.bg_count.1:1|c".to_string(),
            ]
        );
    }

    #[test]
    fn for_global_strikes_matches_python() {
        let metrics = RecordingMetrics::new();
        metrics.for_global_strikes(60, 0.85);
        assert_eq!(
            metrics.lines(),
            vec![
                "strikes_grid.total_count:1|c".to_string(),
                "global_strikes_grid.total_count:1|c".to_string(),
                "global_strikes_grid.cache_hits:0.85|g".to_string(),
            ]
        );
    }

    #[test]
    fn for_local_strikes_reports_data_area() {
        let metrics = RecordingMetrics::new();
        metrics.for_local_strikes(10, 5, 0.75);
        assert_eq!(
            metrics.lines(),
            vec![
                "strikes_grid.total_count:1|c".to_string(),
                "local_strikes_grid.total_count:1|c".to_string(),
                "local_strikes_grid.data_area.5:1|c".to_string(),
                "local_strikes_grid.cache_hits:0.75|g".to_string(),
                "strikes_grid.bg_count:1|c".to_string(),
                "local_strikes_grid.bg_count:1|c".to_string(),
            ]
        );
    }

    #[test]
    fn for_histogram_reports_query_count_ratio_and_size() {
        let metrics = RecordingMetrics::new();
        metrics.for_histogram(0.75, 4);
        assert_eq!(
            metrics.lines(),
            vec![
                "histogram.query.count:1|c".to_string(),
                "histogram.cache_hits:0.75|g".to_string(),
                "histogram.size:4|g".to_string(),
            ]
        );
    }

    #[test]
    fn for_db_pool_wait_reports_milliseconds_and_never_zero() {
        let metrics = RecordingMetrics::new();
        metrics.for_db_pool_wait(0.0123);
        metrics.for_db_pool_wait(0.0);
        assert_eq!(
            metrics.lines(),
            vec![
                "db.pool_wait:12|ms".to_string(),
                "db.pool_wait:1|ms".to_string()
            ]
        );
    }

    #[test]
    fn for_grid_total_reports_milliseconds_and_never_zero() {
        let metrics = RecordingMetrics::new();
        metrics.for_grid_total(name::STRIKES_GRID, 0.0175);
        metrics.for_grid_total(name::GLOBAL_STRIKES_GRID, 0.0);
        assert_eq!(
            metrics.lines(),
            vec![
                "strikes_grid.total:17|ms".to_string(),
                "global_strikes_grid.total:1|ms".to_string(),
            ]
        );
    }

    #[test]
    fn for_grid_query_counts_queries() {
        let metrics = RecordingMetrics::new();
        metrics.for_grid_query();
        metrics.for_grid_query();
        assert_eq!(
            metrics.lines(),
            vec![
                "strikes_grid_query.count:1|c".to_string(),
                "strikes_grid_query.count:1|c".to_string(),
            ]
        );
    }

    #[test]
    fn fmt_f64_trims_whole_numbers() {
        assert_eq!(fmt_f64(0.0), "0");
        assert_eq!(fmt_f64(1.0), "1");
        assert_eq!(fmt_f64(0.75), "0.75");
        assert_eq!(fmt_f64(0.5), "0.5");
    }

    /// `cli/imprt.py::import_strikes_for`: `strikes.<region>` counter, `.count`
    /// gauge and `.get`/`.insert` millisecond timings (clamped to at least 1).
    #[test]
    fn for_import_reports_region_metrics() {
        let metrics = RecordingMetrics::new();
        metrics.for_import(3, 42, 0.0123, 0.0);
        assert_eq!(
            metrics.lines(),
            vec![
                "strikes.3:1|c".to_string(),
                "strikes.3.count:42|g".to_string(),
                "strikes.3.get:12|ms".to_string(),
                "strikes.3.insert:1|ms".to_string(),
            ]
        );
    }

    #[test]
    fn for_import_error_count_gauges_errors() {
        let metrics = RecordingMetrics::new();
        metrics.for_import_error_count(7);
        assert_eq!(metrics.lines(), vec!["strikes.error_count:7|g".to_string()]);
    }

    #[test]
    fn for_websocket_strike_counts_and_gauges_delay() {
        let metrics = RecordingMetrics::new();
        metrics.for_websocket_strike(4.5);
        assert_eq!(
            metrics.lines(),
            vec!["strikes:1|c".to_string(), "strikes.delay:4.5|g".to_string()]
        );
    }

    #[test]
    fn for_update_imported_gauges_the_count() {
        let metrics = RecordingMetrics::new();
        metrics.for_update_imported(0);
        metrics.for_update_imported(9);
        assert_eq!(
            metrics.lines(),
            vec![
                "strikes.imported:0|g".to_string(),
                "strikes.imported:9|g".to_string()
            ]
        );
    }

    #[test]
    fn seconds_to_millis_clamps_to_one() {
        assert_eq!(seconds_to_millis(0.0), 1);
        assert_eq!(seconds_to_millis(0.0001), 1);
        assert_eq!(seconds_to_millis(0.0123), 12);
    }

    /// The importer prefix puts the metrics under `org.blitzortung.import`.
    #[test]
    fn statsd_metrics_honours_import_prefix() {
        let receiver = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        receiver
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let port = receiver.local_addr().unwrap().port();

        let metrics =
            StatsDMetrics::with_address_and_prefix("127.0.0.1", port, IMPORT_STATSD_PREFIX)
                .unwrap();
        metrics.for_update_imported(2);

        let mut buffer = [0u8; 512];
        let (len, _) = receiver.recv_from(&mut buffer).unwrap();
        assert_eq!(
            String::from_utf8(buffer[..len].to_vec()).unwrap(),
            "org.blitzortung.import.strikes.imported:2|g"
        );
    }

    /// `StatsDMetrics` sends prefixed `name:value|type` datagrams to the
    /// configured receiver over UDP.
    #[test]
    fn statsd_metrics_sends_udp_datagrams() {
        let receiver = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        receiver
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let port = receiver.local_addr().unwrap().port();

        let metrics = StatsDMetrics::with_address("127.0.0.1", port).unwrap();
        metrics.for_strikes(10, 1, 0.75);

        let mut buffer = [0u8; 512];
        let mut received = Vec::new();
        // Five datagrams are sent by `for_strikes(10, ..)`.
        for _ in 0..5 {
            let (len, _) = receiver.recv_from(&mut buffer).unwrap();
            received.push(String::from_utf8(buffer[..len].to_vec()).unwrap());
        }
        assert_eq!(
            received,
            vec![
                "org.blitzortung.service.strikes_grid.total_count:1|c".to_string(),
                "org.blitzortung.service.strikes_grid.total_count.1:1|c".to_string(),
                "org.blitzortung.service.strikes_grid.cache_hits:0.75|g".to_string(),
                "org.blitzortung.service.strikes_grid.bg_count:1|c".to_string(),
                "org.blitzortung.service.strikes_grid.bg_count.1:1|c".to_string(),
            ]
        );
    }

    /// A custom prefix overrides the default one.
    #[test]
    fn statsd_metrics_honours_custom_prefix() {
        let receiver = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        receiver
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let port = receiver.local_addr().unwrap().port();

        let metrics = StatsDMetrics::with_address_and_prefix("127.0.0.1", port, "custom").unwrap();
        metrics.for_db_pool_wait(0.002);

        let mut buffer = [0u8; 512];
        let (len, _) = receiver.recv_from(&mut buffer).unwrap();
        assert_eq!(
            String::from_utf8(buffer[..len].to_vec()).unwrap(),
            "custom.db.pool_wait:2|ms"
        );
    }

    /// Sending against a closed receiver must not panic (UDP send succeeds
    /// even with no listener; this pins that instrumentation is best-effort).
    #[test]
    fn statsd_metrics_send_is_best_effort() {
        // Bind and immediately drop so the port is (most likely) closed.
        let receiver = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let port = receiver.local_addr().unwrap().port();
        drop(receiver);

        let metrics = StatsDMetrics::with_address("127.0.0.1", port).unwrap();
        metrics.for_global_strikes(60, 0.5);
    }
}
