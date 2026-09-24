//! `bo-update` implementation (port of `blitzortung/cli/update.py`).
//!
//! Fetches recent strikes from `last_strikes.php` (one JSON object per line)
//! and inserts the ones that are within the time interval, not already present
//! (by timestamp/location/lateral error) and older than one minute.

use chrono::{DateTime, Duration, Utc};
use clap::Parser;

use crate::builder::Strike as StrikeBuilder;
use crate::data::Strike;
use crate::db::{HashableStrikeKey, StrikeDb, StrikeKey};
use crate::executor::QueryExecutor;
use crate::query::TimeInterval;
use crate::round::py_round;

/// `update.create_strike_key`: the de-duplication key for a strike.
pub fn create_strike_key(strike: &Strike) -> StrikeKey {
    (
        strike.timestamp.value(),
        py_round(strike.x, 4),
        py_round(strike.y, 4),
        strike.lateral_error,
    )
}

/// `update.fetch_strikes_from_url`: parse the JSON-lines response body into
/// [`Strike`] objects; malformed lines are logged and skipped.
pub fn parse_strikes_from_text(body: &str) -> Vec<Strike> {
    let mut builder = StrikeBuilder::new();
    let mut strikes = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(value) => match builder.from_json(&value).and_then(|b| b.build()) {
                Ok(strike) => strikes.push(strike),
                Err(error) => log::warn!("Failed to create strike object: {error} ({line})"),
            },
            Err(error) => log::warn!("Failed to parse strike: {error} ({line})"),
        }
    }
    strikes
}

/// The result of an update run.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateResult {
    pub inserted: usize,
    pub url_strikes: usize,
}

/// `update.update_strikes`: fetch, filter and insert new strikes.
///
/// `now` is injected for testability.  Returns `Err` on a database failure
/// (after rolling back), matching the Python behaviour.
pub async fn update_strikes(
    executor: &dyn QueryExecutor,
    url_strikes: &[Strike],
    hours: i64,
    now: DateTime<Utc>,
) -> Result<UpdateResult, Box<dyn std::error::Error + Send + Sync>> {
    let start_time = now - Duration::hours(hours);
    let interval = TimeInterval::new(start_time, now);

    let db = StrikeDb::new(executor, 4326);
    let existing: std::collections::HashSet<HashableStrikeKey> = db
        .select_strike_keys(&interval, None, None)
        .await?
        .into_iter()
        .map(HashableStrikeKey::from)
        .collect();
    log::info!("Found {} existing strikes in database", existing.len());

    let mut new_strikes = Vec::new();
    for strike in url_strikes {
        match strike.timestamp.datetime {
            Some(dt) if start_time <= dt && dt <= now => {}
            _ => {
                log::debug!("Strike outside time interval, skipping");
                continue;
            }
        }
        let key = HashableStrikeKey::from(create_strike_key(strike));
        if existing.contains(&key) {
            log::debug!("Strike already exists, skipping");
            continue;
        }
        let strike_time = strike.timestamp.datetime.unwrap();
        if strike_time < now - Duration::minutes(1) {
            log::debug!("Strike at {strike_time} new");
            new_strikes.push(strike.clone());
        } else {
            log::debug!("Strike at {strike_time} too new");
        }
    }

    log::info!(
        "Found {} new strikes to insert (out of {} from URL)",
        new_strikes.len(),
        url_strikes.len()
    );

    if !new_strikes.is_empty() {
        if let Err(error) = db.insert_many(&new_strikes, None).await {
            executor.rollback().await?;
            return Err(error.into());
        }
        executor.commit().await?;
        log::info!("Successfully inserted {} new strikes", new_strikes.len());
    } else {
        log::info!("No new strikes to insert");
    }

    Ok(UpdateResult {
        inserted: new_strikes.len(),
        url_strikes: url_strikes.len(),
    })
}

/// Build the URL used by `update.update_strikes`.
///
/// Python computes `int(start_time.timestamp() * 1e6) * 1000`; the float
/// multiplication is reproduced here (it differs from exact nanosecond
/// arithmetic in ~1% of cases, so matching CPython matters).
pub fn last_strikes_url(start_time: DateTime<Utc>) -> String {
    let epoch_seconds = start_time.timestamp() as f64
        + start_time.timestamp_subsec_micros() as f64 / 1_000_000.0;
    let start_timestamp_ns = (epoch_seconds * 1e6) as i64 * 1000;
    format!(
        "https://data.blitzortung.org/Data/Protected/last_strikes.php?time={start_timestamp_ns}"
    )
}

/// `bo-update` command-line options (port of `cli/update.py.parse_options`).
#[derive(Parser, Debug, Clone)]
#[command(
    name = "bo-update",
    about = "Import recent strikes from last_strikes.php into the database",
    version
)]
pub struct UpdateArgs {
    /// Number of hours to look back (default: 1)
    #[arg(long, default_value_t = 1)]
    pub hours: i64,

    /// Enable verbose logging
    #[arg(short, long)]
    pub verbose: bool,

    /// Enable debug logging
    #[arg(short, long)]
    pub debug: bool,

    /// Skip file locking (use with caution)
    #[arg(long)]
    pub no_lock: bool,
}

/// Resolved `bo-update` options.
pub struct UpdateOptions {
    pub hours: i64,
    pub verbose: bool,
    pub debug: bool,
    pub no_lock: bool,
}

impl UpdateOptions {
    pub fn from_args(args: &UpdateArgs) -> Self {
        UpdateOptions {
            hours: args.hours,
            verbose: args.verbose,
            debug: args.debug,
            no_lock: args.no_lock,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{Param, Row, Value};
    use crate::mock::MockExecutor;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    fn strike_at(dt: DateTime<Utc>, x: f64, y: f64, mds: Option<i64>) -> Strike {
        let ts = crate::data::Timestamp::new(dt, 0);
        Strike::new(Some(-1), ts, x, y, Some(0.0), Some(0.0), mds, Some(0), vec![], None)
    }

    fn empty_keys_executor() -> MockExecutor {
        let mut mock = MockExecutor::new();
        mock.add_rows("FROM strikes", vec![]);
        mock
    }

    #[test]
    fn create_strike_key_rounds() {
        let strike = strike_at(utc(2025, 1, 1, 12, 0, 0), 12.345678, 45.678901, Some(100));
        let key = create_strike_key(&strike);
        assert_eq!(key.0, crate::data::Timestamp::new(utc(2025, 1, 1, 12, 0, 0), 0).value());
        assert_eq!(key.1, 12.3457);
        assert_eq!(key.2, 45.6789);
        assert_eq!(key.3, Some(100));
    }

    #[test]
    fn parse_json_lines_successfully() {
        let body = concat!(
            "{\"time\":1763202124325980200,\"lat\":-15.296556,\"lon\":134.589548,\"alt\":0,\"mds\":12581}\n",
            "\n",
            "{\"time\":1763202124297904000,\"lat\":44.283328,\"lon\":8.910987,\"alt\":0,\"mds\":6830,\"region\":9}\n"
        );
        let strikes = parse_strikes_from_text(body);
        assert_eq!(strikes.len(), 2);
        assert_eq!(strikes[0].x, 134.5895);
        assert_eq!(strikes[0].y, -15.2966);
        assert_eq!(strikes[0].amplitude, Some(0.0));
    }

    #[test]
    fn parse_json_lines_skips_invalid() {
        let body = "invalid json line\n{\"time\":1763202124325980200,\"lat\":-15.296556,\"lon\":134.589548,\"alt\":0,\"mds\":20}";
        let strikes = parse_strikes_from_text(body);
        assert_eq!(strikes.len(), 1);
        assert_eq!(strikes[0].lateral_error, Some(20));
    }

    #[tokio::test]
    async fn update_inserts_new_strikes() {
        let now = utc(2025, 1, 1, 12, 0, 0);
        let mut mock = empty_keys_executor();
        let strikes = vec![
            strike_at(now - Duration::minutes(30), 10.5, 20.5, None),
            strike_at(now - Duration::minutes(30), 11.5, 21.5, None),
        ];
        let result = update_strikes(&mock, &strikes, 1, now).await.unwrap();
        assert_eq!(result.inserted, 2);
        assert_eq!(mock.execution_count(), 1);
        assert_eq!(mock.commit_count(), 1);
        let _ = &mut mock;
    }

    #[tokio::test]
    async fn update_skips_too_new_strikes() {
        let now = utc(2025, 1, 1, 12, 0, 0);
        let mut mock = empty_keys_executor();
        let strikes = vec![strike_at(now - Duration::seconds(59), 10.5, 20.5, None)];
        let result = update_strikes(&mock, &strikes, 1, now).await.unwrap();
        assert_eq!(result.inserted, 0);
        assert_eq!(mock.execution_count(), 0);
        assert_eq!(mock.commit_count(), 0);
        let _ = &mut mock;
    }

    #[tokio::test]
    async fn update_skips_duplicates() {
        let now = utc(2025, 1, 1, 12, 0, 0);
        let existing = strike_at(now - Duration::minutes(30), 10.5, 20.5, None);
        let mut mock = MockExecutor::new();
        mock.add_rows(
            "FROM strikes",
            vec![Row::new(vec![
                Value::Timestamp(existing.timestamp.datetime.unwrap()),
                Value::Int(0),
                Value::Float(10.5),
                Value::Float(20.5),
                Value::Null,
            ])],
        );
        let strikes = vec![strike_at(now - Duration::minutes(30), 10.5, 20.5, None)];
        let result = update_strikes(&mock, &strikes, 1, now).await.unwrap();
        assert_eq!(result.inserted, 0);
        assert_eq!(mock.execution_count(), 0);
        let _ = &mut mock;
    }

    #[tokio::test]
    async fn update_filters_by_time_interval() {
        let now = utc(2025, 1, 1, 12, 0, 0);
        let mut mock = empty_keys_executor();
        let strikes = vec![
            strike_at(now - Duration::minutes(30), 10.5, 20.5, None),
            strike_at(now - Duration::hours(2), 11.5, 21.5, None),
        ];
        let result = update_strikes(&mock, &strikes, 1, now).await.unwrap();
        assert_eq!(result.inserted, 1);
        let _ = &mut mock;
    }

    #[tokio::test]
    async fn update_rolls_back_on_insert_error() {
        let now = utc(2025, 1, 1, 12, 0, 0);
        let mut mock = MockExecutor::new();
        mock.add_rows("FROM strikes", vec![]);
        mock.add_error("INSERT INTO strikes", "database error");
        let strikes = vec![strike_at(now - Duration::minutes(30), 10.5, 20.5, None)];
        let result = update_strikes(&mock, &strikes, 1, now).await;
        assert!(result.is_err());
        assert_eq!(mock.rollback_count(), 1);
    }

    #[test]
    fn last_strikes_url_uses_nanosecond_epoch() {
        let url = last_strikes_url(utc(2025, 1, 1, 12, 0, 0));
        let ns = utc(2025, 1, 1, 12, 0, 0).timestamp() * 1_000_000_000;
        assert_eq!(
            url,
            format!(
                "https://data.blitzortung.org/Data/Protected/last_strikes.php?time={ns}"
            )
        );
    }

    #[tokio::test]
    async fn update_params_use_region_fallback() {
        // insert_many with region None must fall back to strike region / 1.
        let now = utc(2025, 1, 1, 12, 0, 0);
        let mut mock = empty_keys_executor();
        let strikes = vec![strike_at(now - Duration::minutes(30), 10.5, 20.5, None)];
        update_strikes(&mock, &strikes, 1, now).await.unwrap();
        let (_, params) = &mock.executions()[0];
        assert_eq!(params[5], Param::Int(1));
        let _ = &mut mock;
    }
}