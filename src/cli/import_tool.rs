//! `bo-import` implementation (port of `blitzortung/cli/imprt.py`).
//!
//! Imports strikes from the protected ten-minute logs for the known regions.
//! The Python tool wraps each region in a 300 second `stopit` signal timeout
//! and retries connection errors five times; the Rust port enforces the same
//! deadline cooperatively between log downloads and retries exactly as the
//! Python code does.

use chrono::{DateTime, Duration, TimeZone, Utc};
use clap::Parser;

use crate::data::{Strike, Timestamp};
use crate::dataimport::{StrikeSink, StrikesBlitzortungDataProvider, Transport};
use crate::db::StrikeDb;
use crate::executor::QueryExecutor;
use crate::metrics::Metrics;
use crate::util::Timer;

/// Regions imported by `cli/imprt.py`.
pub const REGIONS: &[u32] = &[1, 2, 3, 4, 5, 6, 7, 10, 18, 19];
/// Number of per-region retries on connection errors.
pub const RETRY_COUNT: usize = 5;
/// Wall-clock budget per region attempt (`stopit.SignalTimeout(300)`).
pub const REGION_TIMEOUT_SECONDS: u64 = 300;
/// Per-request HTTP timeout used by `bo-import`.
///
/// Kept short so an unresponsive (typically missing) log file cannot stall a
/// region for long; the provider skips such files and continues.
pub const REQUEST_TIMEOUT_SECONDS: u64 = 10;
/// `cli/imprt.py` batch size for `insert_many`.
pub const STRIKE_BATCH_SIZE: usize = 1000;
/// `cli/imprt.py` commit grouping size.
pub const STRIKE_GROUP_SIZE: i64 = 10000;
/// Sleep between retries.
pub const RETRY_SLEEP_MILLIS: u64 = 2000;

/// `bo-import` command-line options (port of `cli/imprt.py.parse_options`).
#[derive(Parser, Debug, Clone)]
#[command(
    name = "bo-import",
    about = "Import protected strike logs from data.blitzortung.org",
    version
)]
pub struct ImportArgs {
    /// verbose output
    #[arg(short, long)]
    pub verbose: bool,

    /// debug output
    #[arg(short, long)]
    pub debug: bool,

    /// do not apply 5 minute timeout
    #[arg(long)]
    pub no_timeout: bool,

    /// import start date
    #[arg(long)]
    pub startdate: Option<String>,

    /// run as regular update
    #[arg(long)]
    pub update: bool,
}

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
/// Returns the number of strikes inserted.  Reports the same StatsD metrics
/// as the Python tool (`strikes.<region>`, `.count`, `.get`, `.insert`) through
/// `metrics`.
pub async fn import_strikes_for<T: Transport>(
    executor: &dyn QueryExecutor,
    transport: &T,
    region: u32,
    start_time: Option<Timestamp>,
    is_update: bool,
    deadline: Option<std::time::Instant>,
    metrics: &dyn Metrics,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    log::debug!("work on region {region}");
    let db = StrikeDb::new(executor, 4326);

    let timer = Timer::new();
    let mut latest_time = db.get_latest_time(Some(region as i64)).await?;
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

    // Stream the region's strikes into the database in bounded batches rather
    // than collecting them all first: a single region can contain millions of
    // strikes, which must not be held in memory at once.  `ChunkInserter`
    // submits at most `STRIKE_BATCH_SIZE` rows per call (`StrikeDb::insert_many`
    // applies any further PostgreSQL parameter-limit split) and commits after
    // each complete `STRIKE_GROUP_SIZE` group.
    let reference_time = std::time::Instant::now();
    let provider = StrikesBlitzortungDataProvider::new(transport);
    let latest = latest_time.and_then(|t| t.datetime);

    let mut inserter = ChunkInserter::new(db, executor, region);
    provider
        .get_strikes_since_with_deadline_into(
            latest,
            region,
            deadline,
            STRIKE_BATCH_SIZE,
            &mut inserter,
        )
        .await?;
    inserter.finish().await?;

    let strike_count = inserter.inserted;
    let insert_seconds = inserter.insert_seconds;
    // Fetch and insert are interleaved while streaming, so the `.get` phase is
    // the total minus the time spent inside the database sink.
    let get_seconds = (reference_time.elapsed().as_secs_f64() - insert_seconds).max(0.0);

    // `imprt.import_strikes_for`: `strikes.<region>` counter, `.count` gauge
    // and the `.get`/`.insert` phase timings.
    metrics.for_import(region, strike_count as u64, get_seconds, insert_seconds);

    let total = reference_time.elapsed().as_secs_f64().max(f64::EPSILON);
    log::info!(
        "imported {} strikes ({:.1}/s) for region {}",
        strike_count,
        strike_count as f64 / total,
        region
    );

    Ok(strike_count)
}

/// [`StrikeSink`] that inserts each batch and commits complete groups.
///
/// This is what keeps `bo-import`'s memory bounded: the provider hands over at
/// most one `STRIKE_BATCH_SIZE` batch at a time and this sink persists it (and
/// commits every `STRIKE_GROUP_SIZE` rows) before the next batch is fetched.
struct ChunkInserter<'a> {
    db: StrikeDb<'a>,
    executor: &'a dyn QueryExecutor,
    region: u32,
    inserted: usize,
    committed_groups: usize,
    group_started: std::time::Instant,
    insert_seconds: f64,
}

impl<'a> ChunkInserter<'a> {
    fn new(db: StrikeDb<'a>, executor: &'a dyn QueryExecutor, region: u32) -> Self {
        ChunkInserter {
            db,
            executor,
            region,
            inserted: 0,
            committed_groups: 0,
            group_started: std::time::Instant::now(),
            insert_seconds: 0.0,
        }
    }

    /// Commit a final partial group (if any).
    async fn finish(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let group_size = STRIKE_GROUP_SIZE as usize;
        if self.inserted > self.committed_groups * group_size {
            let started = std::time::Instant::now();
            self.executor.commit().await?;
            self.insert_seconds += started.elapsed().as_secs_f64();
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl<'a> StrikeSink for ChunkInserter<'a> {
    async fn accept(
        &mut self,
        strikes: &[Strike],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let started = std::time::Instant::now();
        self.inserted += self
            .db
            .insert_many(strikes, Some(self.region as i64))
            .await?;

        // Commits are issued after each complete `STRIKE_GROUP_SIZE` group; use
        // a division-based check so changing either constant cannot skip a
        // commit at a group boundary.
        let group_size = STRIKE_GROUP_SIZE as usize;
        let groups = self.inserted / group_size;
        while self.committed_groups < groups {
            self.committed_groups += 1;
            self.executor.commit().await?;
            let elapsed = self.group_started.elapsed().as_secs_f64().max(f64::EPSILON);
            log::info!(
                "commit #{} ({:.1}/s) for region {}",
                self.committed_groups * group_size,
                group_size as f64 / elapsed,
                self.region
            );
            self.group_started = std::time::Instant::now();
        }
        self.insert_seconds += started.elapsed().as_secs_f64();
        Ok(())
    }
}

/// `imprt.import_strikes`: iterate all regions, retrying connection errors.
///
/// Returns the total number of strikes and the accumulated error count.
/// Reports the accumulated error count as `strikes.error_count` like the
/// Python tool.
pub async fn import_strikes<T: Transport>(
    executor: &dyn QueryExecutor,
    transport: &T,
    regions: &[u32],
    start_time: Option<Timestamp>,
    no_timeout: bool,
    is_update: bool,
    metrics: &dyn Metrics,
) -> (usize, usize) {
    let mut error_count = 0usize;
    let mut total_strikes = 0usize;
    for region in regions {
        for retry in 0..RETRY_COUNT {
            let deadline = if no_timeout {
                None
            } else {
                Some(
                    std::time::Instant::now()
                        + std::time::Duration::from_secs(REGION_TIMEOUT_SECONDS),
                )
            };
            match import_strikes_for(
                executor, transport, *region, start_time, is_update, deadline, metrics,
            )
            .await
            {
                Ok(count) => {
                    total_strikes += count;
                    break;
                }
                Err(error) => {
                    log::warn!(
                        "import failed: retry {retry} region {region}: {}",
                        crate::cli::describe_error("error", error.as_ref())
                    );
                    error_count += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(RETRY_SLEEP_MILLIS)).await;
                }
            }
        }
    }
    metrics.for_import_error_count(error_count as u64);
    (total_strikes, error_count)
}

/// Build the `bo-import` options from the parsed command line.
pub struct ImportOptions {
    pub verbose: bool,
    pub debug: bool,
    pub no_timeout: bool,
    pub startdate: Option<String>,
    pub update: bool,
}

impl ImportOptions {
    pub fn from_args(args: &ImportArgs) -> Self {
        ImportOptions {
            verbose: args.verbose,
            debug: args.debug,
            no_timeout: args.no_timeout,
            startdate: args.startdate.clone(),
            update: args.update,
        }
    }
}

/// Resolve the `--startdate` option to a [`Timestamp`] (UTC midnight).
pub fn resolve_start_time(options: &ImportOptions) -> Option<Timestamp> {
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
    use crate::metrics::NoopMetrics;
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

    #[async_trait::async_trait]
    impl Transport for StubTransport {
        async fn read_lines(&self, _source: &str) -> Result<Vec<String>, TransportError> {
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

    /// Transport that reports every requested log file as missing.
    struct MissingTransport;

    #[async_trait::async_trait]
    impl Transport for MissingTransport {
        async fn read_lines(&self, _source: &str) -> Result<Vec<String>, TransportError> {
            Err(TransportError::NotFound)
        }
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

    #[tokio::test]
    async fn import_strikes_for_inserts_new_strikes() {
        let now = Utc::now();
        // Latest DB time is older than the log entry, so the strike is new.
        let mut mock = executor_with_latest(Some((now - Duration::hours(2), 0)));
        let transport = StubTransport::new(vec![strike_line(now - Duration::minutes(30))], 0);
        let count = import_strikes_for(&mock, &transport, 1, None, false, None, &NoopMetrics)
            .await
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(mock.execution_count(), 1);
        assert_eq!(mock.commit_count(), 1);
        // The insert was pinned to the region.
        let (_, params) = &mock.executions()[0];
        assert_eq!(params[5], crate::executor::Param::Int(1));
        let _ = &mut mock;
    }

    /// `import_strikes_for` reports the Python metric set through the sink.
    /// The importer hands the database bounded batches and flushes the final
    /// partial batch instead of trying to insert the whole region at once.
    #[tokio::test]
    async fn import_strikes_for_flushes_batches_and_tail() {
        let now = Utc::now();
        let base = now - Duration::hours(2) + Duration::seconds(1);
        let mut mock = executor_with_latest(Some((now - Duration::hours(3), 0)));
        let lines = (0..=STRIKE_BATCH_SIZE)
            .map(|index| strike_line(base + Duration::milliseconds(index as i64)))
            .collect();
        let transport = StubTransport::new(lines, 0);

        let count = import_strikes_for(&mock, &transport, 4, None, false, None, &NoopMetrics)
            .await
            .unwrap();
        assert_eq!(count, STRIKE_BATCH_SIZE + 1);
        assert_eq!(mock.execution_count(), 2);
        assert_eq!(mock.executions()[0].1.len(), STRIKE_BATCH_SIZE * 9);
        assert_eq!(mock.executions()[1].1.len(), 9);
        for (_, params) in mock.executions() {
            assert_eq!(params[5], crate::executor::Param::Int(4));
        }
        assert_eq!(mock.commit_count(), 1);
        let _ = &mut mock;
    }

    /// Strikes are committed as they are streamed: a region larger than one
    /// group is persisted in a first committed group plus a final partial
    /// group, without ever buffering the whole region.
    #[tokio::test]
    async fn import_strikes_for_commits_multiple_groups_while_streaming() {
        let now = Utc::now();
        let base = now - Duration::hours(2) + Duration::seconds(1);
        let mut mock = executor_with_latest(Some((now - Duration::hours(3), 0)));
        let total = STRIKE_GROUP_SIZE as usize + 1;
        let lines = (0..total)
            .map(|index| strike_line(base + Duration::milliseconds(index as i64)))
            .collect();
        let transport = StubTransport::new(lines, 0);

        let count = import_strikes_for(&mock, &transport, 2, None, false, None, &NoopMetrics)
            .await
            .unwrap();

        assert_eq!(count, total);
        // Ten 1,000-row batches plus the final single-row batch.
        assert_eq!(mock.execution_count(), total.div_ceil(STRIKE_BATCH_SIZE));
        // One commit at the 10,000-row boundary and one for the partial group.
        assert_eq!(mock.commit_count(), 2);
        let _ = &mut mock;
    }

    /// `import_strikes_for` reports the Python metric set through the sink.
    #[tokio::test]
    async fn import_strikes_for_reports_metrics() {
        let now = Utc::now();
        let mut mock = executor_with_latest(Some((now - Duration::hours(2), 0)));
        let transport = StubTransport::new(vec![strike_line(now - Duration::minutes(30))], 0);
        let metrics = crate::metrics::RecordingMetrics::new();
        import_strikes_for(&mock, &transport, 3, None, false, None, &metrics)
            .await
            .unwrap();
        let lines = metrics.lines();
        assert_eq!(lines[0], "strikes.3:1|c");
        assert_eq!(lines[1], "strikes.3.count:1|g");
        assert!(lines[2].starts_with("strikes.3.get:"), "got {lines:?}");
        assert!(lines[2].ends_with("|ms"), "got {lines:?}");
        assert!(lines[3].starts_with("strikes.3.insert:"), "got {lines:?}");
        assert!(lines[3].ends_with("|ms"), "got {lines:?}");
        let _ = &mut mock;
    }

    #[tokio::test]
    async fn import_strikes_for_no_strikes_does_not_commit() {
        let mut mock = executor_with_latest(None);
        let transport = StubTransport::new(vec![], 0);
        let count = import_strikes_for(&mock, &transport, 1, None, false, None, &NoopMetrics)
            .await
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(mock.commit_count(), 0);
        let _ = &mut mock;
    }

    #[tokio::test]
    async fn import_strikes_skips_missing_files_without_retry() {
        let mut mock = executor_with_latest(None);
        let transport = MissingTransport;
        let (strikes, errors) =
            import_strikes(&mock, &transport, &[1], None, true, false, &NoopMetrics).await;
        assert_eq!(strikes, 0);
        assert_eq!(errors, 0);
        // A missing file must not restart the region: only the latest-time
        // lookup runs, no retry (which would query it `RETRY_COUNT` times).
        assert_eq!(mock.call_count(), 1);
        let _ = &mut mock;
    }

    #[tokio::test]
    async fn import_strikes_retries_on_connection_error() {
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
        let (strikes, errors) =
            import_strikes(&mock, &transport, &[1], None, true, false, &NoopMetrics).await;
        assert_eq!(errors, 1);
        assert_eq!(strikes, 1);
        let _ = &mut mock;
    }

    /// `import_strikes` gauges the accumulated error count like Python.
    #[tokio::test]
    async fn import_strikes_reports_error_count_metric() {
        // Each retry re-reads the latest DB time before the transport fails.
        let mut mock = MockExecutor::new();
        for _ in 0..RETRY_COUNT {
            mock.add_rows("ORDER BY \"timestamp\" DESC", vec![]);
        }
        let transport = StubTransport::new(vec![], RETRY_COUNT);
        let metrics = crate::metrics::RecordingMetrics::new();
        let (_, errors) =
            import_strikes(&mock, &transport, &[1], None, true, false, &metrics).await;
        assert_eq!(errors, RETRY_COUNT);
        assert_eq!(metrics.lines(), vec!["strikes.error_count:5|g".to_string()]);
        let _ = &mut mock;
    }

    #[tokio::test]
    async fn import_strikes_defaults_start_to_update_window() {
        // With no DB row and `is_update`, the provider default (now - 6h) is
        // replaced by now - 30min.
        let mut mock = executor_with_latest(None);
        let transport = StubTransport::new(vec![], 0);
        let count = import_strikes_for(&mock, &transport, 1, None, true, None, &NoopMetrics)
            .await
            .unwrap();
        assert_eq!(count, 0);
        let _ = &mut mock;
    }
}
