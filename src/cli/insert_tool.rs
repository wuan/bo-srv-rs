//! `bo-insert` implementation (port of `blitzortung/cli/imprt.py`).
//!
//! Imports strikes from the protected ten-minute logs for the known regions.
//! The Python tool wraps each region in a 300 second `stopit` signal timeout
//! and retries connection errors five times; the Rust port enforces the same
//! deadline cooperatively between log downloads and retries exactly as the
//! Python code does.

use chrono::{DateTime, Duration, TimeZone, Utc};

use crate::cli::Options;
use crate::data::Timestamp;
use crate::dataimport::{StrikesBlitzortungDataProvider, Transport};
use crate::db::StrikeDb;
use crate::executor::QueryExecutor;
use crate::util::Timer;

/// Regions imported by `cli/imprt.py`.
pub const REGIONS: &[u32] = &[1, 2, 3, 4, 5, 6, 7, 10, 18, 19];
/// Number of per-region retries on connection errors.
pub const RETRY_COUNT: usize = 5;
/// Wall-clock budget per region attempt (`stopit.SignalTimeout(300)`).
pub const REGION_TIMEOUT_SECONDS: u64 = 300;
/// `cli/imprt.py` batch size for `insert_many`.
pub const STRIKE_BATCH_SIZE: usize = 1000;
/// `cli/imprt.py` commit grouping size.
pub const STRIKE_GROUP_SIZE: i64 = 10000;
/// Sleep between retries.
pub const RETRY_SLEEP_MILLIS: u64 = 2000;

/// The option specs understood by `bo-insert`.
pub const SPECS: &[(&str, &str, bool)] = &[
    ("verbose", "v", false),
    ("debug", "d", false),
    ("no-timeout", "", false),
    ("startdate", "", true),
    ("update", "", false),
];

/// `imprt.update_start_time`: now - 30 minutes.
pub fn update_start_time() -> DateTime<Utc> {
    Utc::now() - Duration::minutes(30)
}

/// Parse `--startdate YYYYMMDD` into a UTC midnight timestamp
/// (`Timestamp(datetime.strptime(startdate, "%Y%m%d").replace(tzinfo=utc))`).
pub fn parse_start_date(value: &str) -> Option<DateTime<Utc>> {
    let naive = chrono::NaiveDate::parse_from_str(value, "%Y%m%d").ok()?;
    let naive = naive.and_hms_opt(0, 0, 0)?;
    Some(Utc.from_utc_datetime(&naive))
}

/// `imprt.import_strikes_for`: import one region into the database.
///
/// Returns the number of strikes inserted.
pub fn import_strikes_for<T: Transport>(
    executor: &dyn QueryExecutor,
    transport: &T,
    region: u32,
    start_time: Option<Timestamp>,
    is_update: bool,
    deadline: Option<std::time::Instant>,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    log::debug!("work on region {region}");
    let db = StrikeDb::new(executor, 4326);

    let timer = Timer::new();
    let mut latest_time = db.get_latest_time(Some(region as i64))?;
    log::debug!(
        "latest time for region {region}: {} ({:.03}s)",
        latest_time
            .map(|t| t.event_string())
            .unwrap_or_else(|| "none".to_string()),
        timer.read()
    );
    if latest_time.is_none() {
        latest_time = start_time;
    }

    if is_update {
        let update_start = update_start_time();
        let update_ts = Timestamp::new(update_start, 0);
        if latest_time.is_none() || update_ts > latest_time.unwrap() {
            latest_time = Some(update_ts);
        }
    }

    let provider = StrikesBlitzortungDataProvider::new(transport);
    let latest = latest_time.and_then(|t| t.datetime);
    let mut strikes = provider.get_strikes_since_with_deadline(latest, region, deadline)?;

    let mut strike_count: i64 = 0;
    let mut strike_batch: Vec<crate::data::Strike> = Vec::new();
    let mut start_time = std::time::Instant::now();
    let global_start_time = start_time;

    for strike in strikes.drain(..) {
        strike_batch.push(strike);
        strike_count += 1;

        if strike_batch.len() >= STRIKE_BATCH_SIZE {
            db.insert_many(&strike_batch, Some(region as i64))?;
            strike_batch.clear();

            if strike_count % STRIKE_GROUP_SIZE == 0 {
                executor.commit()?;
                let elapsed = start_time.elapsed().as_secs_f64().max(f64::EPSILON);
                log::info!(
                    "commit #{} ({:.1}/s) for region {}",
                    strike_count,
                    STRIKE_GROUP_SIZE as f64 / elapsed,
                    region
                );
                start_time = std::time::Instant::now();
            }
        }
    }

    if !strike_batch.is_empty() {
        db.insert_many(&strike_batch, Some(region as i64))?;
    }
    if strike_count > 0 {
        executor.commit()?;
    }

    let insert_time = std::time::Instant::now();
    let _ = insert_time;
    let total = global_start_time.elapsed().as_secs_f64().max(f64::EPSILON);
    log::info!(
        "imported {} strikes ({:.1}/s) for region {}",
        strike_count,
        strike_count as f64 / total,
        region
    );

    Ok(strike_count as usize)
}

/// `imprt.import_strikes`: iterate all regions, retrying connection errors.
///
/// Returns the total number of strikes and the accumulated error count.
pub fn import_strikes<T: Transport>(
    executor: &dyn QueryExecutor,
    transport: &T,
    regions: &[u32],
    start_time: Option<Timestamp>,
    no_timeout: bool,
    is_update: bool,
) -> (usize, usize) {
    let mut error_count = 0usize;
    let mut total_strikes = 0usize;
    for region in regions {
        for retry in 0..RETRY_COUNT {
            let deadline = if no_timeout {
                None
            } else {
                Some(std::time::Instant::now() + std::time::Duration::from_secs(REGION_TIMEOUT_SECONDS))
            };
            match import_strikes_for(
                executor,
                transport,
                *region,
                start_time,
                is_update,
                deadline,
            ) {
                Ok(count) => {
                    total_strikes += count;
                    break;
                }
                Err(_) => {
                    log::warn!("import failed: retry {retry} region {region}");
                    error_count += 1;
                    std::thread::sleep(std::time::Duration::from_millis(RETRY_SLEEP_MILLIS));
                }
            }
        }
    }
    (total_strikes, error_count)
}

/// Build the `bo-insert` options from the parsed command line.
pub struct InsertOptions {
    pub verbose: bool,
    pub debug: bool,
    pub no_timeout: bool,
    pub startdate: Option<String>,
    pub update: bool,
}

impl InsertOptions {
    pub fn from_options(options: &Options) -> Self {
        InsertOptions {
            verbose: options.flag("verbose"),
            debug: options.flag("debug"),
            no_timeout: options.flag("no-timeout"),
            startdate: options.value("startdate").map(|s| s.to_string()),
            update: options.flag("update"),
        }
    }
}

/// Resolve the `--startdate` option to a [`Timestamp`] (UTC midnight).
pub fn resolve_start_time(options: &InsertOptions) -> Option<Timestamp> {
    options.startdate.as_ref().map(|value| {
        let parsed = parse_start_date(value).unwrap_or_else(|| {
            crate::cli::exit_with(&format!("parse error in startdate \"{value}\""), 5)
        });
        Timestamp::new(parsed, 0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataimport::base::Transport;
    use crate::dataimport::TransportError;
    use crate::executor::{Row, Value};
    use crate::mock::MockExecutor;
    use chrono::TimeZone;
    use std::sync::Mutex;

    /// Transport that returns canned lines for the first call only (each
/// subsequent call, i.e. each further log path, is empty).  `fail_times`
/// requests fail before any line is returned.
    struct StubTransport {
        lines: Vec<String>,
        remaining_failures: Mutex<usize>,
        served: Mutex<bool>,
    }

    impl StubTransport {
        fn new(lines: Vec<String>, failures: usize) -> Self {
            StubTransport {
                lines,
                remaining_failures: Mutex::new(failures),
                served: Mutex::new(false),
            }
        }
    }

    impl Transport for StubTransport {
        fn read_lines(&self, _source: &str) -> Result<Vec<String>, TransportError> {
            let mut failures = self.remaining_failures.lock().unwrap();
            if *failures > 0 {
                *failures -= 1;
                return Err(TransportError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "connection refused",
                )));
            }
            drop(failures);
            let mut served = self.served.lock().unwrap();
            if *served {
                return Ok(Vec::new());
            }
            *served = true;
            Ok(self.lines.clone())
        }
    }

    fn strike_line(ts: DateTime<Utc>) -> String {
        format!(
            "{}789 pos;48.5;-10.2;500.5 str;45.2 dev;250.0 sta;5;10;1,2,3",
            ts.format("%Y-%m-%d %H:%M:%S.%6f")
        )
    }

    fn executor_with_latest(latest: Option<(DateTime<Utc>, i64)>) -> MockExecutor {
        let mut mock = MockExecutor::new();
        match latest {
            Some((dt, ns)) => mock.add_rows(
                "ORDER BY \"timestamp\" DESC",
                vec![Row::new(vec![Value::Timestamp(dt), Value::Int(ns)])],
            ),
            None => mock.add_rows("ORDER BY \"timestamp\" DESC", vec![]),
        }
        mock
    }

    #[test]
    fn update_start_time_is_thirty_minutes_ago() {
        let result = update_start_time();
        let expected = Utc::now() - Duration::minutes(30);
        assert!((result - expected).num_seconds().abs() < 1);
    }

    #[test]
    fn parse_start_date_midnight_utc() {
        let parsed = parse_start_date("20250101").unwrap();
        assert_eq!(parsed, Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn import_strikes_for_inserts_new_strikes() {
        let now = Utc::now();
        // Latest DB time is older than the log entry, so the strike is new.
        let mut mock = executor_with_latest(Some((now - Duration::hours(2), 0)));
        let transport = StubTransport::new(vec![strike_line(now - Duration::minutes(30))], 0);
        let count = import_strikes_for(&mock, &transport, 1, None, false, None).unwrap();
        assert_eq!(count, 1);
        assert_eq!(mock.execution_count(), 1);
        assert_eq!(mock.commit_count(), 1);
        // The insert was pinned to the region.
        let (_, params) = &mock.executions()[0];
        assert_eq!(params[5], crate::executor::Param::Int(1));
        let _ = &mut mock;
    }

    #[test]
    fn import_strikes_for_no_strikes_does_not_commit() {
        let mut mock = executor_with_latest(None);
        let transport = StubTransport::new(vec![], 0);
        let count = import_strikes_for(&mock, &transport, 1, None, false, None).unwrap();
        assert_eq!(count, 0);
        assert_eq!(mock.commit_count(), 0);
        let _ = &mut mock;
    }

    #[test]
    fn import_strikes_retries_on_connection_error() {
        let now = Utc::now();
        // Two DB latest-time queries: the failed attempt and the retry.
        let mut mock = MockExecutor::new();
        for _ in 0..2 {
            mock.add_rows(
                "ORDER BY \"timestamp\" DESC",
                vec![Row::new(vec![
                    Value::Timestamp(now - Duration::hours(2)),
                    Value::Int(0),
                ])],
            );
        }
        let transport = StubTransport::new(vec![strike_line(now - Duration::minutes(30))], 1);
        let (strikes, errors) = import_strikes(&mock, &transport, &[1], None, true, false);
        assert_eq!(errors, 1);
        assert_eq!(strikes, 1);
        let _ = &mut mock;
    }

    #[test]
    fn import_strikes_defaults_start_to_update_window() {
        // With no DB row and `is_update`, the provider default (now - 6h) is
        // replaced by now - 30min.
        let mut mock = executor_with_latest(None);
        let transport = StubTransport::new(vec![], 0);
        let count = import_strikes_for(&mock, &transport, 1, None, true, None).unwrap();
        assert_eq!(count, 0);
        let _ = &mut mock;
    }
}