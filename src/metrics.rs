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
}

/// Default StatsD receiver address (`StatsClient('localhost', 8125)`).
pub const DEFAULT_STATSD_HOST: &str = "localhost";
pub const DEFAULT_STATSD_PORT: u16 = 8125;
/// `StatsClient(..., prefix='org.blitzortung.service')`.
pub const DEFAULT_STATSD_PREFIX: &str = "org.blitzortung.service";

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
    fn for_histogram_reports_ratio_and_size() {
        let metrics = RecordingMetrics::new();
        metrics.for_histogram(0.75, 4);
        assert_eq!(
            metrics.lines(),
            vec![
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
    fn fmt_f64_trims_whole_numbers() {
        assert_eq!(fmt_f64(0.0), "0");
        assert_eq!(fmt_f64(1.0), "1");
        assert_eq!(fmt_f64(0.75), "0.75");
        assert_eq!(fmt_f64(0.5), "0.5");
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
