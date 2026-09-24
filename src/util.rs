//! Utility helpers ported from `blitzortung/util.py`.

use chrono::{DateTime, Duration, Utc};

/// A simple lap timer (`util.Timer`) used for logging durations.
#[derive(Debug)]
pub struct Timer {
    start_time: std::time::Instant,
    lap_time: std::time::Instant,
}

impl Timer {
    pub fn new() -> Self {
        let now = std::time::Instant::now();
        Timer {
            start_time: now,
            lap_time: now,
        }
    }

    /// `Timer.read`: total seconds since construction.
    pub fn read(&self) -> f64 {
        self.start_time.elapsed().as_secs_f64()
    }

    /// `Timer.lap`: seconds since the previous lap (or construction), and
    /// starts a new lap.
    pub fn lap(&mut self) -> f64 {
        let now = std::time::Instant::now();
        let lap = (now - self.lap_time).as_secs_f64();
        self.lap_time = now;
        lap
    }
}

impl Default for Timer {
    fn default() -> Self {
        Timer::new()
    }
}

/// `util.total_seconds`: seconds relative to midnight for a datetime.
pub fn total_seconds(time_value: DateTime<Utc>) -> i64 {
    use chrono::Timelike;
    time_value.hour() as i64 * 3600 + time_value.minute() as i64 * 60 + time_value.second() as i64
}

/// `util.round_time`: round a datetime down to a multiple of `duration`
/// seconds since midnight, zeroing the sub-second parts.
pub fn round_time(time_value: DateTime<Utc>, duration: Duration) -> DateTime<Utc> {
    use chrono::Timelike;
    let duration_seconds = duration.num_seconds().max(1);
    let seconds = (total_seconds(time_value) / duration_seconds) * duration_seconds;
    let hour = (seconds / 3600) as u32;
    let minute = ((seconds / 60) % 60) as u32;
    let second = (seconds % 60) as u32;
    time_value
        .with_hour(hour)
        .and_then(|v| v.with_minute(minute))
        .and_then(|v| v.with_second(second))
        .and_then(|v| v.with_nanosecond(0))
        .unwrap_or(time_value)
}

/// `util.time_intervals`: interval start times from `start_time` up to
/// `end_time` (default now), both rounded to `duration`.
///
/// Returns a vector of rounded interval start times inclusive of both
/// endpoints.
pub fn time_intervals(
    start_time: DateTime<Utc>,
    duration: Duration,
    end_time: Option<DateTime<Utc>>,
) -> Vec<DateTime<Utc>> {
    let mut current_time = round_time(start_time, duration);
    let end_time = round_time(end_time.unwrap_or_else(Utc::now), duration);
    let mut result = Vec::new();
    while current_time <= end_time {
        result.push(current_time);
        current_time += duration;
    }
    result
}

/// Format a datetime as the protected-log path component
/// (`BlitzortungDataPathGenerator.url_path_format`).
pub fn log_path(time: DateTime<Utc>) -> String {
    format!(
        "{}/{}/{}/{}.log",
        time.format("%Y"),
        time.format("%m"),
        time.format("%d"),
        time.format("%H/%M")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    #[test]
    fn round_time_rounds_down() {
        let value = utc(2013, 8, 20, 12, 3, 23);
        let result = round_time(value, Duration::minutes(2));
        assert_eq!(result.hour(), 12);
        assert_eq!(result.minute(), 2);
        assert_eq!(result.second(), 0);
    }

    #[test]
    fn time_intervals_contains_expected_starts() {
        let end = utc(2013, 8, 20, 12, 9, 0);
        let start = end - Duration::minutes(25);
        let intervals = time_intervals(start, Duration::minutes(10), Some(end));
        assert!(intervals.contains(&utc(2013, 8, 20, 11, 40, 0)));
        assert!(intervals.contains(&utc(2013, 8, 20, 11, 50, 0)));
        assert!(intervals.contains(&utc(2013, 8, 20, 12, 0, 0)));
    }

    #[test]
    fn log_path_matches_python_format() {
        assert_eq!(
            log_path(utc(2013, 8, 20, 11, 40, 0)),
            "2013/08/20/11/40.log"
        );
    }
}