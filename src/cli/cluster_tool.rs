//! `bo-cluster` implementation: detect and print strike clusters over a time
//! interval.
//!
//! The Python reference (`blitzortung.clustering`) is a library only; this tool
//! wires it to the existing `bo-db` query plumbing so the algorithm can be
//! exercised against the database.  It is read-only: it selects strikes from the
//! `strikes` table and never touches the schema.

use clap::Parser;
use serde::Serialize;

use crate::cli::db_tool::{resolve_area, resolve_interval_with_lookback, DbOptions};
use crate::cli::{parse_timezone, DATE_FORMAT};
use crate::clustering::Clustering;
use crate::data::StrikeCluster;
use crate::db::StrikeDb;
use crate::executor::QueryExecutor;
use crate::query::TimeInterval;
use crate::util::Timer;

/// Default lookback for the start time: the original cluster tool ran detection
/// for the last ten minutes (a cluster interval is typically short).
pub const DEFAULT_INTERVAL_MINUTES: i64 = 10;

/// `bo-cluster` command-line options.
///
/// The time options mirror `bo-db` (`--startdate`/`--starttime`/`--enddate`/
/// `--endtime` with the same `%Y%m%d`/`%H%M[%S]` parsing), except that the start
/// time defaults to the last ten minutes instead of the last hour.  Plus
/// `--region`, the WKT `--area` filter and the output switch `--json`.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "bo-cluster",
    about = "Detect strike clusters over a time interval (read-only)",
    version
)]
pub struct ClusterArgs {
    /// start date for data retrieval
    #[arg(long, default_value = "default")]
    pub startdate: String,

    /// start time for data retrieval
    #[arg(long, default_value = "default")]
    pub starttime: String,

    /// end date for data retrieval
    #[arg(long, default_value = "default")]
    pub enddate: String,

    /// end time for data retrieval
    #[arg(long, default_value = "default")]
    pub endtime: String,

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
            region: args.region,
            json: args.json,
            verbose: args.verbose,
            debug: args.debug,
        }
    }
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
    if parse_timezone(&options.db.tz).is_none() {
        crate::cli::exit_with(&format!("parse error in timezone \"{}\"", options.db.tz), 1);
    }

    let now = chrono::Utc::now();
    let (start, end) = resolve_interval_with_lookback(
        &options.db,
        now,
        chrono::Duration::minutes(DEFAULT_INTERVAL_MINUTES),
    );
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
        start.format(DATE_FORMAT),
        end.format(DATE_FORMAT),
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
    fn default_interval_is_ten_minutes() {
        let now = Utc.with_ymd_and_hms(2025, 1, 1, 12, 30, 0).unwrap();
        let (start, end) = resolve_interval_with_lookback(
            &options().db,
            now,
            chrono::Duration::minutes(DEFAULT_INTERVAL_MINUTES),
        );
        // start = now - 10min, end = now - 1min (default end is implicit)
        assert_eq!(start, Utc.with_ymd_and_hms(2025, 1, 1, 12, 20, 0).unwrap());
        assert_eq!(end, Utc.with_ymd_and_hms(2025, 1, 1, 12, 29, 0).unwrap());
    }

    #[test]
    fn explicit_start_overrides_the_ten_minute_default() {
        let mut opts = options();
        opts.db.startdate = "20250101".into();
        opts.db.starttime = "1000".into();
        let now = Utc.with_ymd_and_hms(2025, 1, 1, 12, 30, 0).unwrap();
        let (start, _) = resolve_interval_with_lookback(
            &opts.db,
            now,
            chrono::Duration::minutes(DEFAULT_INTERVAL_MINUTES),
        );
        assert_eq!(start, Utc.with_ymd_and_hms(2025, 1, 1, 10, 0, 0).unwrap());
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
