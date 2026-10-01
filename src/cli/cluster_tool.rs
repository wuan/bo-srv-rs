//! `bo-cluster` implementation: detect and print strike clusters over a time
//! interval.
//!
//! The Python reference (`blitzortung.clustering`) is a library only; this tool
//! wires it to the existing `bo-db` query plumbing so the algorithm can be
//! exercised against the database.  It is read-only by default (it selects
//! strikes from the `strikes` table and prints the clusters); with `--insert` it
//! additionally persists the detected clusters to `strike_clusters`.
//!
//! With `--metrics` the tool emits StatsD samples via the [`Metrics`] sink it is
//! handed: `clusters.strikes` / `clusters.produced` gauges and the
//! `clusters.calculate` timing for the calculation, plus `clusters.inserted` /
//! `clusters.insert` for the `--insert` phase.  Nothing is emitted otherwise.
//!
//! [`Metrics`]: crate::metrics::Metrics

use chrono::Timelike;
use clap::Parser;
use serde::Serialize;

use crate::cli::db_tool::{resolve_area, DbOptions};
use crate::cli::{parse_local_time, parse_timezone, DATE_FORMAT};
use crate::clustering::Clustering;
use crate::data::StrikeCluster;
use crate::db::{StrikeClusterDb, StrikeDb};
use crate::executor::QueryExecutor;
use crate::query::TimeInterval;
use crate::util::Timer;

/// Default length of the detection window when `--minutes` is not given: the
/// original cluster tool ran detection for the last ten minutes (a cluster
/// interval is typically short).
pub const DEFAULT_INTERVAL_MINUTES: i64 = 10;

/// `bo-cluster` command-line options.
///
/// The window is defined by its *end* and its length in minutes:
///
/// * The end defaults to the **last minute start** — `now` truncated to the
///   minute (seconds and subseconds zeroed) in the selected `--tz`; it is *not*
///   `now - 1min` like `bo-db`.
/// * `--minutes N` (default [`DEFAULT_INTERVAL_MINUTES`]) sets the length, so
///   `start = end - N minutes`.
/// * `--enddate`/`--endtime` override the end using the same `%Y%m%d`/`%H%M[%S]`
///   parsing as `bo-db` (an explicit end marks the interval end; the end time
///   adds one minute, or one second when seconds are given).
/// * `--startdate`/`--starttime` are an alternative to `--minutes` and pin the
///   interval start; giving both an explicit start and `--minutes` is rejected.
///
/// Plus `--region`, the WKT `--area` filter, `--tz`/`--srid` and the output
/// switch `--json`.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "bo-cluster",
    about = "Detect strike clusters over a time interval and print/persist them",
    version
)]
pub struct ClusterArgs {
    /// start date for data retrieval (alternative to --minutes)
    #[arg(long, default_value = "default")]
    pub startdate: String,

    /// start time for data retrieval (alternative to --minutes)
    #[arg(long, default_value = "default")]
    pub starttime: String,

    /// end date for data retrieval
    #[arg(long, default_value = "default")]
    pub enddate: String,

    /// end time for data retrieval
    #[arg(long, default_value = "default")]
    pub endtime: String,

    /// length of the detection window in minutes (start = end - minutes)
    #[arg(long)]
    pub minutes: Option<i64>,

    /// region id to restrict the strikes to
    #[arg(long)]
    pub region: Option<i64>,

    /// area for which strikes are selected (WKT polygon)
    #[arg(long)]
    pub area: Option<String>,

    /// used timezone
    #[arg(long, default_value = "UTC")]
    pub tz: String,

    /// srid for query area and results
    #[arg(long, default_value_t = 4326)]
    pub srid: i64,

    /// print the clusters as JSON
    #[arg(long)]
    pub json: bool,

    /// persist the detected clusters to the `strike_clusters` table
    #[arg(long)]
    pub insert: bool,

    /// emit StatsD metrics for the cluster calculation and insert
    #[arg(long)]
    pub metrics: bool,

    /// enable verbose (info level) logging
    #[arg(short = 'v', long)]
    pub verbose: bool,

    /// enable debug logging
    #[arg(short = 'd', long)]
    pub debug: bool,
}

/// Resolved `bo-cluster` options.
#[derive(Debug, Clone)]
pub struct ClusterOptions {
    pub db: DbOptions,
    /// Explicit `--minutes` length, or `None` to fall back to
    /// [`DEFAULT_INTERVAL_MINUTES`] (or to an explicit start).
    pub minutes: Option<i64>,
    pub region: Option<i64>,
    pub json: bool,
    /// Persist detected clusters to `strike_clusters` (see
    /// [`insert_clusters`] for the idempotency rule).
    pub insert: bool,
    /// Emit StatsD metrics for the calculation and insert phases (see
    /// [`Metrics`](crate::metrics::Metrics)); a missing StatsD receiver falls
    /// back to a no-op sink.
    pub metrics: bool,
    pub verbose: bool,
    pub debug: bool,
}

impl ClusterOptions {
    /// Build from the clap-parsed command line.
    pub fn from_args(args: &ClusterArgs) -> Self {
        // Reuse `bo-db`'s option resolution for the shared time/area/tz fields.
        let db = DbOptions {
            startdate: args.startdate.clone(),
            starttime: args.starttime.clone(),
            enddate: args.enddate.clone(),
            endtime: args.endtime.clone(),
            area: args.area.clone(),
            tz: args.tz.clone(),
            useenv: false,
            srid: args.srid,
            precision: 4,
            grid: None,
            xgrid: None,
            ygrid: None,
            map: false,
            verbose: args.verbose,
            debug: args.debug,
        };
        ClusterOptions {
            db,
            minutes: args.minutes,
            region: args.region,
            json: args.json,
            insert: args.insert,
            metrics: args.metrics,
            verbose: args.verbose,
            debug: args.debug,
        }
    }
}

/// True when the user pinned the interval start via `--startdate`/`--starttime`.
fn has_explicit_start(options: &DbOptions) -> bool {
    options.startdate != "default" || options.starttime != "default"
}

/// True when the user pinned the interval end via `--enddate`/`--endtime`.
fn has_explicit_end(options: &DbOptions) -> bool {
    options.enddate != "default" || options.endtime != "default"
}

/// Resolve the `bo-cluster` time interval from the parsed options.
///
/// Precedence (see the `ClusterArgs` docs for the full rules):
///
/// 1. **Explicit start** (`--startdate`/`--starttime`) and `--minutes` are
///    mutually exclusive; supplying both is an error (`Err`).
/// 2. An **explicit end** (`--enddate`/`--endtime`) overrides the default end
///    using `bo-db`'s parsing (the end time adds one minute, or one second when
///    seconds are given), so the given value marks the *end* of the interval.
/// 3. The default end is the **last minute start**: `now` truncated to the
///    minute (seconds and subseconds zeroed).
/// 4. With an explicit start and no `--minutes`, `start` is that start and no
///    lookback is applied.
/// 5. Otherwise `minutes` is `--minutes` or [`DEFAULT_INTERVAL_MINUTES`] and
///    `start = end - minutes`.
///
/// `now` is injected as a parameter so the resolution can be unit-tested.
pub fn resolve_cluster_interval(
    options: &DbOptions,
    minutes: Option<i64>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>), String> {
    let explicit_start = has_explicit_start(options);
    if explicit_start && minutes.is_some() {
        return Err(
            "--startdate/--starttime and --minutes are mutually exclusive: give the window \
             length with --minutes, or pin the start with --startdate/--starttime"
                .to_string(),
        );
    }

    let tz = parse_timezone(&options.tz)
        .ok_or_else(|| format!("parse error in timezone \"{}\"", options.tz))?;

    // Default end: the last minute start (`now` truncated to the minute, in UTC;
    // formatted in the selected zone below).
    let default_end = now
        .with_second(0)
        .and_then(|value| value.with_nanosecond(0))
        .expect("truncating seconds/nanoseconds to zero is always valid");

    let end = if has_explicit_end(options) {
        let enddate = if options.enddate == "default" {
            default_end
                .with_timezone(&tz)
                .format(DATE_FORMAT)
                .to_string()
        } else {
            options.enddate.clone()
        };
        let endtime = if options.endtime == "default" {
            default_end.with_timezone(&tz).format("%H%M").to_string()
        } else {
            options.endtime.clone()
        };
        parse_local_time(&enddate, &endtime, tz, true)
            .ok_or_else(|| format!("parse error in endtime: '{enddate} {endtime}'"))?
    } else {
        default_end
    };

    let start = if explicit_start {
        let startdate = if options.startdate == "default" {
            end.with_timezone(&tz).format(DATE_FORMAT).to_string()
        } else {
            options.startdate.clone()
        };
        let starttime = if options.starttime == "default" {
            end.with_timezone(&tz).format("%H%M").to_string()
        } else {
            options.starttime.clone()
        };
        parse_local_time(&startdate, &starttime, tz, false)
            .ok_or_else(|| format!("parse error in starttime: '{startdate} {starttime}'"))?
    } else {
        let minutes = minutes.unwrap_or(DEFAULT_INTERVAL_MINUTES);
        end - chrono::Duration::minutes(minutes)
    };

    Ok((start, end))
}

/// The JSON shape emitted by `--json` (one object per cluster).
#[derive(Debug, Serialize)]
struct ClusterJson {
    id: i64,
    timestamp: String,
    interval_seconds: i64,
    strike_count: i64,
    area: Option<f64>,
    shape: Vec<(f64, f64)>,
}

impl From<&StrikeCluster> for ClusterJson {
    fn from(cluster: &StrikeCluster) -> Self {
        ClusterJson {
            id: cluster.id,
            timestamp: cluster.timestamp.event_string(),
            interval_seconds: cluster.interval_seconds,
            strike_count: cluster.strike_count,
            area: cluster.area,
            shape: cluster.shape.clone().unwrap_or_default(),
        }
    }
}

/// Render one cluster as a text line:
/// `id timestamp interval_seconds strike_count area shape`.
pub fn render_text(cluster: &StrikeCluster) -> String {
    let shape = match &cluster.shape {
        Some(ring) => ring
            .iter()
            .map(|(x, y)| format!("({x:.4}, {y:.4})"))
            .collect::<Vec<_>>()
            .join(", "),
        None => "-".to_string(),
    };
    let area = match cluster.area {
        Some(area) => format!("{area:.1}"),
        None => "-".to_string(),
    };
    format!(
        "{} {} {} {} {} [{}]",
        cluster.id,
        cluster.timestamp.event_string(),
        cluster.interval_seconds,
        cluster.strike_count,
        area,
        shape
    )
}

/// The result of one cluster calculation: the number of strikes selected, the
/// clusters built and the wall time spent in the clustering algorithm.
#[derive(Debug, Clone)]
pub struct ClusterCalculation {
    /// Strikes selected from the `strikes` table for the interval.
    pub strikes: usize,
    /// Clusters produced by the clustering algorithm.
    pub clusters: Vec<StrikeCluster>,
    /// Wall time of the clustering algorithm (`Clustering::build_clusters`),
    /// in seconds.  This excludes the strike `SELECT`; it is exactly the
    /// "calculate the clusters" phase reported as `clusters.calculate`.
    pub cluster_time: f64,
}

/// Select the strikes for `interval` and build the clusters.
pub async fn collect_clusters(
    executor: &dyn QueryExecutor,
    options: &ClusterOptions,
    interval: &TimeInterval,
) -> Result<ClusterCalculation, Box<dyn std::error::Error + Send + Sync>> {
    let db = StrikeDb::new(executor, options.db.srid);
    let area = resolve_area(&options.db);
    let strikes = db.select(interval, area.as_ref(), options.region).await?;
    log::debug!("selected {} strikes for clustering", strikes.len());

    let mut timer = Timer::new();
    let clusters = Clustering::new().build_clusters(&strikes, interval);
    let cluster_time = timer.lap();
    log::info!(
        "built {} clusters from {} strikes in {cluster_time:.3} seconds",
        clusters.len(),
        strikes.len()
    );
    Ok(ClusterCalculation {
        strikes: strikes.len(),
        clusters,
        cluster_time,
    })
}

/// Render `clusters` as JSON (`--json`) or one text line each.
pub fn render_clusters(
    clusters: &[StrikeCluster],
    json: bool,
) -> Result<String, serde_json::Error> {
    if json {
        let json: Vec<ClusterJson> = clusters.iter().map(ClusterJson::from).collect();
        serde_json::to_string(&json)
    } else {
        Ok(clusters
            .iter()
            .map(render_text)
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

/// Select the strikes for `interval`, cluster them and render the output.
pub async fn fetch_clusters(
    executor: &dyn QueryExecutor,
    options: &ClusterOptions,
    interval: &TimeInterval,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let calculation = collect_clusters(executor, options, interval).await?;
    Ok(render_clusters(&calculation.clusters, options.json)?)
}

/// The largest `interval_seconds` value the `SMALLINT` column can hold.
pub const MAX_INTERVAL_SECONDS: i64 = i16::MAX as i64;

/// Persist `clusters` to `strike_clusters`, skipping ones that are already
/// stored for this interval length.
///
/// Idempotency follows the Python `get_latest_time` continuation approach: the
/// newest stored `"timestamp"` for `interval_seconds` is read once, and only
/// clusters whose timestamp is **strictly newer** than that are inserted.  A
/// re-run of the same window therefore inserts nothing instead of creating
/// duplicates.  Returns the number of clusters actually inserted.
///
/// `interval_seconds` is stored in a `SMALLINT` column, so an interval larger
/// than [`MAX_INTERVAL_SECONDS`] (32767 s) is rejected rather than truncated.
pub async fn insert_clusters(
    executor: &dyn QueryExecutor,
    options: &ClusterOptions,
    interval: &TimeInterval,
    clusters: &[StrikeCluster],
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    let interval_seconds = interval.duration_seconds();
    if interval_seconds > MAX_INTERVAL_SECONDS {
        return Err(format!(
            "interval of {interval_seconds} seconds exceeds the strike_clusters SMALLINT limit \
             of {MAX_INTERVAL_SECONDS} seconds; shorten the window with --minutes"
        )
        .into());
    }

    let db = StrikeClusterDb::new(executor, options.db.srid);
    let latest = db.get_latest_time(interval_seconds).await?;
    log::debug!(
        "latest stored cluster for interval_seconds={interval_seconds}: {}",
        match latest {
            Some(value) => value.to_rfc3339(),
            None => "none".to_string(),
        }
    );

    let mut inserted = 0usize;
    for cluster in clusters {
        let timestamp = cluster
            .timestamp
            .datetime
            .ok_or_else(|| "cluster has no timestamp".to_string())?;
        // `strike_clusters."timestamp"` is a `timestamptz`, whose resolution is
        // microseconds: a cluster timestamp with sub-microsecond digits comes
        // back from `get_latest_time` truncated (or rounded) to microseconds, so
        // comparing the raw nanosecond value would treat an already-stored
        // interval as newer and re-insert it.  Compare at microsecond precision
        // (floored, matching the truncation `timestamptz` performs) so a re-run
        // of the same interval is skipped.
        if latest.is_some_and(|value| floor_to_micros(timestamp) <= value) {
            continue;
        }
        db.insert(cluster).await?;
        inserted += 1;
    }
    Ok(inserted)
}

/// Truncate a timestamp to whole microseconds (the `timestamptz` resolution).
///
/// `strike_clusters."timestamp"` is stored as `timestamptz`, which keeps
/// microsecond precision; comparing an in-memory nanosecond timestamp against a
/// stored one must therefore ignore the sub-microsecond remainder.
fn floor_to_micros(timestamp: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
    let micros = timestamp.timestamp_micros();
    chrono::DateTime::from_timestamp_micros(micros)
        .expect("timestamp_micros is always representable")
}

/// Entry point for the `bo-cluster` binary, given a connected executor.
///
/// `metrics` receives the StatsD samples when `--metrics` is set; pass
/// [`NoopMetrics`](crate::metrics::NoopMetrics) to disable emission.
pub async fn run(
    executor: &dyn QueryExecutor,
    options: &ClusterOptions,
    metrics: &dyn crate::metrics::Metrics,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Validate the timezone before touching the database, like `bo-db`.
    let tz = match parse_timezone(&options.db.tz) {
        Some(tz) => tz,
        None => crate::cli::exit_with(&format!("parse error in timezone \"{}\"", options.db.tz), 1),
    };

    let now = chrono::Utc::now();
    let (start, end) = match resolve_cluster_interval(&options.db, options.minutes, now) {
        Ok(interval) => interval,
        Err(error) => crate::cli::exit_with(&error, 1),
    };
    let interval = TimeInterval::new(start, end);

    let mut timer = Timer::new();
    let calculation = collect_clusters(executor, options, &interval).await?;
    let output = render_clusters(&calculation.clusters, options.json)?;
    let elapsed = timer.lap();

    if !output.is_empty() {
        println!("{output}");
    }

    // -- metrics: strikes selected, clusters produced and the calculation time.
    // The select+render `elapsed` above is not reported; `clusters.calculate`
    // is the pure clustering algorithm time measured in `collect_clusters`.
    if options.metrics {
        metrics.for_cluster_calculation(
            calculation.strikes as u64,
            calculation.clusters.len() as u64,
            calculation.cluster_time,
        );
    }

    // `--insert` persists the clusters after the (unchanged) output is printed.
    let inserted = if options.insert {
        let mut insert_timer = Timer::new();
        let inserted =
            match insert_clusters(executor, options, &interval, &calculation.clusters).await {
                Ok(inserted) => inserted,
                Err(error) => {
                    return Err(format!("failed to insert clusters: {error}").into());
                }
            };
        if options.metrics {
            metrics.for_cluster_insert(inserted as u64, insert_timer.lap());
        }
        inserted
    } else {
        0
    };

    let count = calculation.clusters.len();
    eprintln!(
        "built {count} clusters from {} to {} in {elapsed:.3} seconds",
        start.with_timezone(&tz).format("%Y-%m-%d %H:%M:%S"),
        end.with_timezone(&tz).format("%Y-%m-%d %H:%M:%S"),
    );
    if options.insert {
        if inserted == 0 {
            eprintln!(
                "inserted 0 clusters: no newer interval than the latest stored \
                 interval_seconds={} (already up to date)",
                interval.duration_seconds()
            );
        } else {
            eprintln!("inserted {inserted} clusters into strike_clusters");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{Row, Value};
    use crate::mock::MockExecutor;
    use chrono::TimeZone;

    fn options() -> ClusterOptions {
        ClusterOptions::from_args(&ClusterArgs {
            startdate: "default".into(),
            starttime: "default".into(),
            enddate: "default".into(),
            endtime: "default".into(),
            minutes: None,
            region: None,
            area: None,
            tz: "UTC".into(),
            srid: 4326,
            json: false,
            insert: false,
            metrics: false,
            verbose: false,
            debug: false,
        })
    }

    #[test]
    fn verbose_and_debug_flags_are_parsed() {
        let args =
            ClusterArgs::try_parse_from(["bo-cluster", "-v", "-d", "--json"]).expect("parse");
        assert!(args.verbose);
        assert!(args.debug);
        assert!(args.json);
        let options = ClusterOptions::from_args(&args);
        assert!(options.verbose);
        assert!(options.debug);
        // The `-v`/`-d` flags are forwarded to the shared `bo-db` options too.
        assert!(options.db.verbose);
        assert!(options.db.debug);
    }

    #[test]
    fn minutes_option_is_parsed_and_defaults_to_none() {
        let args = ClusterArgs::try_parse_from(["bo-cluster"]).expect("parse");
        assert_eq!(args.minutes, None);
        let args = ClusterArgs::try_parse_from(["bo-cluster", "--minutes", "5"]).expect("parse");
        assert_eq!(args.minutes, Some(5));
    }

    #[test]
    fn default_end_is_last_minute_start() {
        // The clock is injected: 12:34:56 must resolve to the 12:34:00 minute.
        let opts = options();
        let now = Utc.with_ymd_and_hms(2025, 1, 1, 12, 34, 56).unwrap();
        let (start, end) = resolve_cluster_interval(&opts.db, opts.minutes, now).unwrap();
        assert_eq!(end, Utc.with_ymd_and_hms(2025, 1, 1, 12, 34, 0).unwrap());
        // start = end - 10min (the default --minutes).
        assert_eq!(start, Utc.with_ymd_and_hms(2025, 1, 1, 12, 24, 0).unwrap());
    }

    #[test]
    fn default_end_truncates_subseconds() {
        let opts = options();
        let now = Utc
            .with_ymd_and_hms(2025, 1, 1, 12, 34, 56)
            .unwrap()
            .with_nanosecond(123_456_789)
            .unwrap();
        let (_, end) = resolve_cluster_interval(&opts.db, opts.minutes, now).unwrap();
        assert_eq!(end, Utc.with_ymd_and_hms(2025, 1, 1, 12, 34, 0).unwrap());
        assert_eq!(end.nanosecond(), 0);
    }

    #[test]
    fn explicit_minutes_sets_the_window_length() {
        let mut opts = options();
        opts.minutes = Some(5);
        let now = Utc.with_ymd_and_hms(2025, 1, 1, 12, 34, 56).unwrap();
        let (start, end) = resolve_cluster_interval(&opts.db, opts.minutes, now).unwrap();
        assert_eq!(end, Utc.with_ymd_and_hms(2025, 1, 1, 12, 34, 0).unwrap());
        assert_eq!(start, Utc.with_ymd_and_hms(2025, 1, 1, 12, 29, 0).unwrap());
    }

    #[test]
    fn explicit_end_overrides_the_default_and_resets_the_start() {
        let mut opts = options();
        opts.db.enddate = "20250101".into();
        opts.db.endtime = "1000".into();
        let now = Utc.with_ymd_and_hms(2025, 1, 1, 12, 34, 56).unwrap();
        let (start, end) = resolve_cluster_interval(&opts.db, opts.minutes, now).unwrap();
        // `bo-db` end semantics: an explicit HHMM end time adds one minute.
        assert_eq!(end, Utc.with_ymd_and_hms(2025, 1, 1, 10, 1, 0).unwrap());
        // start = end - 10min.
        assert_eq!(start, Utc.with_ymd_and_hms(2025, 1, 1, 9, 51, 0).unwrap());
    }

    #[test]
    fn explicit_end_with_seconds_marks_the_interval_end() {
        let mut opts = options();
        opts.db.enddate = "20250101".into();
        opts.db.endtime = "100030".into();
        let now = Utc.with_ymd_and_hms(2025, 1, 1, 12, 34, 56).unwrap();
        let (start, end) = resolve_cluster_interval(&opts.db, opts.minutes, now).unwrap();
        // `bo-db` end semantics: HHMMSS adds one second.
        assert_eq!(end, Utc.with_ymd_and_hms(2025, 1, 1, 10, 0, 31).unwrap());
        assert_eq!(start, Utc.with_ymd_and_hms(2025, 1, 1, 9, 50, 31).unwrap());
    }

    #[test]
    fn explicit_start_is_used_when_minutes_is_absent() {
        let mut opts = options();
        opts.db.startdate = "20250101".into();
        opts.db.starttime = "1000".into();
        let now = Utc.with_ymd_and_hms(2025, 1, 1, 12, 34, 56).unwrap();
        let (start, end) = resolve_cluster_interval(&opts.db, opts.minutes, now).unwrap();
        assert_eq!(start, Utc.with_ymd_and_hms(2025, 1, 1, 10, 0, 0).unwrap());
        // The end default (last minute start) is still applied.
        assert_eq!(end, Utc.with_ymd_and_hms(2025, 1, 1, 12, 34, 0).unwrap());
    }

    #[test]
    fn explicit_start_and_minutes_are_mutually_exclusive() {
        let mut opts = options();
        opts.minutes = Some(5);
        opts.db.startdate = "20250101".into();
        let now = Utc.with_ymd_and_hms(2025, 1, 1, 12, 34, 56).unwrap();
        let error = resolve_cluster_interval(&opts.db, opts.minutes, now).unwrap_err();
        assert!(error.contains("mutually exclusive"), "error: {error}");
    }

    #[test]
    fn default_end_respects_the_selected_timezone() {
        // In Europe/Berlin the default end is the last minute start as well.
        let mut opts = options();
        opts.db.tz = "Europe/Berlin".into();
        let now = Utc.with_ymd_and_hms(2025, 1, 1, 12, 34, 56).unwrap();
        let (start, end) = resolve_cluster_interval(&opts.db, opts.minutes, now).unwrap();
        assert_eq!(end, Utc.with_ymd_and_hms(2025, 1, 1, 12, 34, 0).unwrap());
        assert_eq!(start, Utc.with_ymd_and_hms(2025, 1, 1, 12, 24, 0).unwrap());
    }

    fn strike_row(id: i64, x: f64, y: f64) -> Row {
        Row::new(vec![
            Value::Int(id),
            Value::Timestamp(Utc.with_ymd_and_hms(2025, 1, 1, 11, 55, 0).unwrap()),
            Value::Int(0),
            Value::Float(x),
            Value::Float(y),
            Value::Float(0.0),
            Value::Float(0.0),
            Value::Int(0),
            Value::Int(0),
        ])
    }

    use chrono::Utc;

    #[tokio::test]
    async fn fetch_clusters_renders_one_cluster() {
        let mut mock = MockExecutor::new();
        mock.add_rows(
            "FROM strikes",
            vec![
                strike_row(1, 11.0, 51.0),
                strike_row(2, 11.02, 51.02),
                strike_row(3, 11.02, 51.05),
                strike_row(4, 11.4, 51.4),
                strike_row(5, 12.0, 52.0),
            ],
        );
        let interval = TimeInterval::new(
            Utc.with_ymd_and_hms(2025, 1, 1, 11, 50, 0).unwrap(),
            Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap(),
        );
        let output = fetch_clusters(&mock, &options(), &interval).await.unwrap();
        assert_eq!(output.lines().count(), 1);
        assert!(output.contains(" 3 "), "strike_count: {output}");
        assert!(output.contains("37.3"), "area: {output}");
    }

    #[tokio::test]
    async fn fetch_clusters_json_output() {
        let mut mock = MockExecutor::new();
        mock.add_rows(
            "FROM strikes",
            vec![
                strike_row(1, 11.0, 51.0),
                strike_row(2, 11.02, 51.02),
                strike_row(3, 11.02, 51.05),
            ],
        );
        let mut opts = options();
        opts.json = true;
        let interval = TimeInterval::new(
            Utc.with_ymd_and_hms(2025, 1, 1, 11, 50, 0).unwrap(),
            Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap(),
        );
        let output = fetch_clusters(&mock, &opts, &interval).await.unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["strike_count"], 3);
        assert_eq!(parsed[0]["interval_seconds"], 600);
    }

    #[tokio::test]
    async fn fetch_clusters_empty_for_too_few_strikes() {
        let mut mock = MockExecutor::new();
        mock.add_rows(
            "FROM strikes",
            vec![strike_row(1, 11.0, 51.0), strike_row(2, 11.02, 51.02)],
        );
        let interval = TimeInterval::new(
            Utc.with_ymd_and_hms(2025, 1, 1, 11, 50, 0).unwrap(),
            Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap(),
        );
        let output = fetch_clusters(&mock, &options(), &interval).await.unwrap();
        assert!(output.is_empty());
    }

    // -- `--insert` ---------------------------------------------------------

    fn cluster_at(timestamp: chrono::DateTime<Utc>) -> crate::data::StrikeCluster {
        crate::data::StrikeCluster {
            id: -1,
            timestamp: crate::data::Timestamp::new(timestamp, 0),
            interval_seconds: 600,
            shape: Some(vec![
                (11.0, 51.0),
                (11.1, 51.0),
                (11.1, 51.1),
                (11.0, 51.1),
                (11.0, 51.0),
            ]),
            strike_count: 3,
            area: None,
        }
    }

    fn minute_interval() -> TimeInterval {
        TimeInterval::new(
            Utc.with_ymd_and_hms(2025, 1, 1, 11, 50, 0).unwrap(),
            Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap(),
        )
    }

    #[test]
    fn insert_flag_is_parsed() {
        let args = ClusterArgs::try_parse_from(["bo-cluster", "--insert"]).expect("parse");
        assert!(args.insert);
        assert!(ClusterOptions::from_args(&args).insert);
        let args = ClusterArgs::try_parse_from(["bo-cluster"]).expect("parse");
        assert!(!ClusterOptions::from_args(&args).insert);
    }

    #[tokio::test]
    async fn insert_clusters_writes_each_cluster() {
        let mut mock = MockExecutor::new();
        // No stored cluster yet for interval_seconds=600.
        mock.add_rows("FROM strike_clusters", vec![]);
        let mut opts = options();
        opts.insert = true;
        let interval = minute_interval();
        let end = Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap();
        let clusters = vec![
            cluster_at(end - chrono::Duration::minutes(2)),
            cluster_at(end),
        ];

        let inserted = insert_clusters(&mock, &opts, &interval, &clusters)
            .await
            .unwrap();
        assert_eq!(inserted, 2);
        let executions = mock.executions();
        assert_eq!(executions.len(), 2, "one INSERT per cluster");
        for (sql, params) in &executions {
            assert!(sql.contains("INSERT INTO strike_clusters"), "sql: {sql}");
            assert_eq!(params[1], crate::executor::Param::Int(600));
            assert_eq!(params[4], crate::executor::Param::Int(3));
        }
        // The guard read the latest stored timestamp for this interval length.
        assert!(mock.calls()[0].0.contains("FROM strike_clusters"));
    }

    #[tokio::test]
    async fn insert_clusters_skips_when_not_newer() {
        let mut mock = MockExecutor::new();
        let end = Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap();
        // Latest stored cluster is exactly the cluster we would insert.
        mock.add_rows(
            "FROM strike_clusters",
            vec![Row::new(vec![Value::Timestamp(end)])],
        );
        let opts = options();
        let clusters = vec![cluster_at(end)];

        let inserted = insert_clusters(&mock, &opts, &minute_interval(), &clusters)
            .await
            .unwrap();
        assert_eq!(inserted, 0, "nothing newer than the stored timestamp");
        assert!(
            mock.executions().is_empty(),
            "no INSERT when the guard skips"
        );
    }

    #[tokio::test]
    async fn insert_clusters_skips_within_same_microsecond() {
        // Regression for the CI PostGIS failure: `timestamptz` keeps only
        // microseconds, so a cluster timestamp with a sub-microsecond remainder
        // (e.g. `Utc::now()`) reads back truncated.  The guard must still treat
        // it as "already stored" and not re-insert the same interval.
        let mut mock = MockExecutor::new();
        let end = Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap();
        // 500 ns past the whole microsecond: `timestamptz` truncates this back
        // to `end`, so a raw nanosecond comparison sees it as "newer".
        let cluster_time = end + chrono::Duration::nanoseconds(500);
        assert_eq!(cluster_time.nanosecond() % 1000, 500);
        // What Postgres would have stored: the same instant floored to micros.
        let stored = end;
        mock.add_rows(
            "FROM strike_clusters",
            vec![Row::new(vec![Value::Timestamp(stored)])],
        );
        let opts = options();
        let clusters = vec![cluster_at(cluster_time)];

        let inserted = insert_clusters(&mock, &opts, &minute_interval(), &clusters)
            .await
            .unwrap();
        assert_eq!(
            inserted, 0,
            "sub-microsecond remainder must not defeat the guard"
        );
        assert!(mock.executions().is_empty());
    }

    #[test]
    fn floor_to_micros_truncates_submicrosecond_digits() {
        let value = Utc
            .with_ymd_and_hms(2025, 1, 1, 12, 0, 0)
            .unwrap()
            .with_nanosecond(123_456_789)
            .unwrap();
        let floored = floor_to_micros(value);
        assert_eq!(floored.nanosecond() % 1000, 0, "no sub-microsecond digits");
        // 123_456_789 ns truncates to 123_456 us = .123456 s.
        assert_eq!(
            floored,
            Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0)
                .unwrap()
                .with_nanosecond(123_456_000)
                .unwrap()
        );
    }

    #[tokio::test]
    async fn insert_clusters_inserts_newer_than_latest() {
        let mut mock = MockExecutor::new();
        let end = Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap();
        // Latest stored is older than the cluster's timestamp.
        mock.add_rows(
            "FROM strike_clusters",
            vec![Row::new(vec![Value::Timestamp(
                end - chrono::Duration::minutes(10),
            )])],
        );
        let opts = options();
        let clusters = vec![cluster_at(end)];

        let inserted = insert_clusters(&mock, &opts, &minute_interval(), &clusters)
            .await
            .unwrap();
        assert_eq!(inserted, 1);
        assert_eq!(mock.execution_count(), 1);
    }

    #[tokio::test]
    async fn insert_clusters_rejects_interval_over_smallint() {
        let mock = MockExecutor::new();
        let opts = options();
        // 40000s window: > i16::MAX, must error before touching the database.
        let interval = TimeInterval::new(
            Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2025, 1, 1, 11, 6, 40).unwrap(),
        );
        let error = insert_clusters(&mock, &opts, &interval, &[cluster_at(interval.end)])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("SMALLINT"), "error: {error}");
        assert_eq!(mock.call_count(), 0, "no query must be issued");
        assert_eq!(mock.execution_count(), 0, "no insert must be issued");
    }

    // -- `--metrics` --------------------------------------------------------

    use crate::metrics::{name, Metrics, RecordingMetrics};

    /// Five strikes around (11, 51) that cluster into exactly one cluster.
    fn five_clusterable_strikes() -> Vec<Row> {
        vec![
            strike_row(1, 11.0, 51.0),
            strike_row(2, 11.02, 51.02),
            strike_row(3, 11.02, 51.05),
            strike_row(4, 11.4, 51.4),
            strike_row(5, 12.0, 52.0),
        ]
    }

    #[test]
    fn metrics_flag_is_parsed() {
        let args = ClusterArgs::try_parse_from(["bo-cluster", "--metrics"]).expect("parse");
        assert!(args.metrics);
        assert!(ClusterOptions::from_args(&args).metrics);
        let args = ClusterArgs::try_parse_from(["bo-cluster"]).expect("parse");
        assert!(!ClusterOptions::from_args(&args).metrics);
    }

    /// The documented metric names/units (importer prefix + `clusters.*`).
    #[test]
    fn cluster_metric_names_are_documented() {
        assert_eq!(name::STRIKES_CLUSTERED, "clusters.strikes");
        assert_eq!(name::CLUSTERS_PRODUCED, "clusters.produced");
        assert_eq!(name::CLUSTERS_CALCULATE, "clusters.calculate");
        assert_eq!(name::CLUSTERS_INSERT, "clusters.insert");
        assert_eq!(name::CLUSTERS_INSERTED, "clusters.inserted");
        // Exercised through the trait so the leaf names and units are fixed.
        let metrics = RecordingMetrics::new();
        metrics.for_cluster_calculation(5, 1, 0.01);
        metrics.for_cluster_insert(1, 0.02);
        assert_eq!(
            metrics.lines(),
            vec![
                "clusters.strikes:5|g".to_string(),
                "clusters.produced:1|g".to_string(),
                "clusters.calculate:10|ms".to_string(),
                "clusters.inserted:1|g".to_string(),
                "clusters.insert:20|ms".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn run_with_metrics_emits_calculation_metrics() {
        let mut mock = MockExecutor::new();
        mock.add_rows("FROM strikes", five_clusterable_strikes());
        let metrics = RecordingMetrics::new();
        let mut opts = options();
        opts.metrics = true;

        run(&mock, &opts, &metrics).await.unwrap();

        let lines = metrics.lines();
        assert!(
            lines.contains(&"clusters.strikes:5|g".to_string()),
            "lines: {lines:?}"
        );
        assert!(
            lines.contains(&"clusters.produced:1|g".to_string()),
            "lines: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("clusters.calculate:") && line.ends_with("|ms")),
            "lines: {lines:?}"
        );
        // No `--insert`: no insert metrics.
        assert!(!lines.iter().any(|line| line.starts_with("clusters.insert")));
    }

    #[tokio::test]
    async fn run_without_metrics_emits_nothing() {
        let mut mock = MockExecutor::new();
        mock.add_rows("FROM strikes", five_clusterable_strikes());
        let metrics = RecordingMetrics::new();
        // `options().metrics` is false by default.
        run(&mock, &options(), &metrics).await.unwrap();
        assert!(
            metrics.lines().is_empty(),
            "no metrics without --metrics: {:?}",
            metrics.lines()
        );
    }

    #[tokio::test]
    async fn run_insert_with_metrics_emits_insert_metrics() {
        let mut mock = MockExecutor::new();
        mock.add_rows("FROM strikes", five_clusterable_strikes());
        // `get_latest_time` finds nothing, so the cluster is inserted.
        mock.add_rows("FROM strike_clusters", vec![]);
        let metrics = RecordingMetrics::new();
        let mut opts = options();
        opts.metrics = true;
        opts.insert = true;

        run(&mock, &opts, &metrics).await.unwrap();

        let lines = metrics.lines();
        assert!(
            lines.contains(&"clusters.inserted:1|g".to_string()),
            "lines: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("clusters.insert:") && line.ends_with("|ms")),
            "lines: {lines:?}"
        );
        // The calculation metrics are still emitted too.
        assert!(lines.contains(&"clusters.strikes:5|g".to_string()));
        assert!(lines.contains(&"clusters.produced:1|g".to_string()));
    }
}
