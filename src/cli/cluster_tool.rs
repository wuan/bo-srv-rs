//! `bo-cluster` implementation: detect and print strike clusters over a time
//! interval.
//!
//! The Python reference (`blitzortung.clustering`) is a library only; this tool
//! wires it to the existing `bo-db` query plumbing so the algorithm can be
//! exercised against the database.  It is read-only: it selects strikes from the
//! `strikes` table and never touches the schema.

use chrono::Timelike;
use clap::Parser;
use serde::Serialize;

use crate::cli::db_tool::{resolve_area, DbOptions};
use crate::cli::{parse_local_time, parse_timezone, DATE_FORMAT};
use crate::clustering::Clustering;
use crate::data::StrikeCluster;
use crate::db::StrikeDb;
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
    about = "Detect strike clusters over a time interval (read-only)",
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

/// Select the strikes for `interval`, cluster them and render the output.
pub async fn fetch_clusters(
    executor: &dyn QueryExecutor,
    options: &ClusterOptions,
    interval: &TimeInterval,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
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

    if options.json {
        let json: Vec<ClusterJson> = clusters.iter().map(ClusterJson::from).collect();
        Ok(serde_json::to_string(&json)?)
    } else {
        Ok(clusters
            .iter()
            .map(render_text)
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

/// Entry point for the `bo-cluster` binary, given a connected executor.
pub async fn run(
    executor: &dyn QueryExecutor,
    options: &ClusterOptions,
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
    let output = fetch_clusters(executor, options, &interval).await?;
    let elapsed = timer.lap();

    if !output.is_empty() {
        println!("{output}");
    }
    let count = if options.json {
        serde_json::from_str::<Vec<serde_json::Value>>(&output)
            .map(|values| values.len())
            .unwrap_or(0)
    } else {
        output.lines().filter(|line| !line.is_empty()).count()
    };
    eprintln!(
        "built {count} clusters from {} to {} in {elapsed:.3} seconds",
        start.with_timezone(&tz).format("%Y-%m-%d %H:%M:%S"),
        end.with_timezone(&tz).format("%Y-%m-%d %H:%M:%S"),
    );
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
}
