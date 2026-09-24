//! Metrics instrumentation, ported from `blitzortung/service/metrics.py`.
//!
//! The Python service sends counters/gauges/timings to a StatsD daemon under
//! the `org.blitzortung.service` prefix.  The Rust port provides a no-op
//! implementation (the spec allows metrics to be absent) plus a recording
//! implementation used by tests, so the metric *names* surface during
//! dispatch stay traceable.

/// Increment/gauge a StatsD-style metric (no-op in this port).
pub trait Metrics: Send + Sync {
    fn incr(&self, _key: &str, _value: u64) {}
    fn gauge(&self, _key: &str, _value: u64) {}
    fn timing(&self, _key: &str, _value: u64) {}

    /// `StatsDMetrics.for_strikes(minute_length, region, cache_ratio)`.
    fn for_strikes(&self, minute_length: i64, region: i64, cache_ratio: f64) {
        let _ = (minute_length, region, cache_ratio);
    }

    /// `StatsDMetrics.for_global_strikes(minute_length, cache_ratio)`.
    fn for_global_strikes(&self, minute_length: i64, cache_ratio: f64) {
        let _ = (minute_length, cache_ratio);
    }

    /// `StatsDMetrics.for_local_strikes(minute_length, data_area,
    /// cache_ratio)`.
    fn for_local_strikes(&self, minute_length: i64, data_area: i64, cache_ratio: f64) {
        let _ = (minute_length, data_area, cache_ratio);
    }

    /// `StatsDMetrics.for_histogram(cache_ratio, cache_size)`.
    fn for_histogram(&self, cache_ratio: f64, cache_size: usize) {
        let _ = (cache_ratio, cache_size);
    }

    /// `StatsDMetrics.for_db_pool_wait(wait_seconds)`.
    fn for_db_pool_wait(&self, wait_seconds: f64) {
        let _ = wait_seconds;
    }
}

/// Metrics that drop everything.
pub struct NoopMetrics;

impl Metrics for NoopMetrics {}

/// Metrics that record every `for_*` call for assertions.
#[derive(Debug, Default)]
pub struct RecordingMetrics {
    pub strikes_calls: std::sync::Mutex<Vec<(i64, i64, f64)>>,
    pub global_calls: std::sync::Mutex<Vec<(i64, f64)>>,
    pub local_calls: std::sync::Mutex<Vec<(i64, i64, f64)>>,
    pub histogram_calls: std::sync::Mutex<Vec<(f64, usize)>>,
}

impl RecordingMetrics {
    pub fn new() -> Self {
        RecordingMetrics::default()
    }
}

impl Metrics for RecordingMetrics {
    fn for_strikes(&self, minute_length: i64, region: i64, cache_ratio: f64) {
        self.strikes_calls
            .lock()
            .unwrap()
            .push((minute_length, region, cache_ratio));
    }

    fn for_global_strikes(&self, minute_length: i64, cache_ratio: f64) {
        self.global_calls.lock().unwrap().push((minute_length, cache_ratio));
    }

    fn for_local_strikes(&self, minute_length: i64, data_area: i64, cache_ratio: f64) {
        self.local_calls
            .lock()
            .unwrap()
            .push((minute_length, data_area, cache_ratio));
    }

    fn for_histogram(&self, cache_ratio: f64, cache_size: usize) {
        self.histogram_calls
            .lock()
            .unwrap()
            .push((cache_ratio, cache_size));
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
    }

    #[test]
    fn recording_metrics_keep_calls() {
        let metrics = RecordingMetrics::new();
        metrics.for_strikes(15, 2, 0.5);
        metrics.for_local_strikes(60, 5, 0.0);
        assert_eq!(metrics.strikes_calls.lock().unwrap()[0], (15, 2, 0.5));
        assert_eq!(metrics.local_calls.lock().unwrap()[0], (60, 5, 0.0));
    }
}