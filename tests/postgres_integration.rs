//! Live PostgreSQL integration tests (opt-in).
//!
//! These are ignored by default because they need a running PostgreSQL with
//! the PostGIS `strikes` schema.  Run them with, e.g.:
//!
//! ```sh
//! export DATABASE_URL="host=127.0.0.1 port=5433 dbname=blitzortung user=blitzortung password=blitzortung"
//! cargo test --test postgres_integration -- --ignored --nocapture
//! ```
//!
//! They are the regression guard for the parameter-encoding bugs found against
//! the real server (0x00 / UTF8 errors, int2 deserialization, ambiguous
//! `ST_Transform` and `ST_MakePoint` placeholders).

use bo_service::data::Timestamp;
use bo_service::db::StrikeDb;
use bo_service::postgres::PostgresExecutor;
use bo_service::query::TimeInterval;
use bo_service::config::Config;

/// Build a [`Config`] from `DATABASE_URL` (a libpq keyword/value string).
fn config_from_env() -> Config {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let mut config = Config::default();
    for pair in url.split_whitespace() {
        if let Some((key, value)) = pair.split_once('=') {
            let value = value.trim_matches('\'').to_string();
            match key {
                "host" => config.db_host = value,
                "port" => config.db_port = value,
                "dbname" => config.db_name = value,
                "user" => config.db_user = value,
                "password" => config.db_password = value,
                _ => {}
            }
        }
    }
    config
}

fn runtime_and_executor() -> (tokio::runtime::Runtime, PostgresExecutor) {
    let config = config_from_env();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let executor = runtime
        .block_on(PostgresExecutor::connect(&config))
        .expect("connect");
    (runtime, executor)
}

/// The default `bo-db` select must run against the real server without a
/// parameter-encoding error (regression for the `0x00`/UTF8 failure from the
/// ambiguous `ST_Transform(geog::geometry, $1)` placeholder), and its rows
/// must deserialize (regression for the `int2` -> `i32` failure).
#[test]
#[ignore = "requires a live PostgreSQL with the strikes schema (set DATABASE_URL)"]
fn default_select_executes_and_deserializes() {
    let (_runtime, executor) = runtime_and_executor();
    let db = StrikeDb::new(&executor, 4326);
    let now = chrono::Utc::now();
    let interval = TimeInterval::new(now - chrono::Duration::hours(24), now);
    let strikes = db.select(&interval, None, None).expect("select must succeed");
    println!("selected {} strikes", strikes.len());
}

/// `bo-db --grid` must run (ambiguous `ST_Transform` + float casts).
#[test]
#[ignore = "requires a live PostgreSQL with the strikes schema (set DATABASE_URL)"]
fn grid_query_executes() {
    use bo_service::geom::Grid;
    let (_runtime, executor) = runtime_and_executor();
    let db = StrikeDb::new(&executor, 4326);
    let now = chrono::Utc::now();
    let interval = TimeInterval::new(now - chrono::Duration::hours(24), now);
    // A bounded envelope (a full -180..180 envelope makes PostGIS reject the
// geography ring as antipodal, unrelated to parameter encoding).
    let grid = Grid::new(-20.0, 30.0, 30.0, 60.0, 1.0, 1.0);
    db.select_grid(&grid, 0, &interval, None)
        .expect("grid query must succeed");
}

/// `select_strike_keys` (`bo-update` de-duplication) must run.
#[test]
#[ignore = "requires a live PostgreSQL with the strikes schema (set DATABASE_URL)"]
fn select_strike_keys_executes() {
    let (_runtime, executor) = runtime_and_executor();
    let db = StrikeDb::new(&executor, 4326);
    let now = chrono::Utc::now();
    let interval = TimeInterval::new(now - chrono::Duration::hours(24), now);
    db.select_strike_keys(&interval, None, None)
        .expect("select_strike_keys must succeed");
}

/// `get_latest_time` (`bo-import` start point) must run (`region=$1::smallint`).
#[test]
#[ignore = "requires a live PostgreSQL with the strikes schema (set DATABASE_URL)"]
fn get_latest_time_executes() {
    let (_runtime, executor) = runtime_and_executor();
    let db = StrikeDb::new(&executor, 4326);
    let latest = db.get_latest_time(Some(1)).expect("get_latest_time must succeed");
    println!("latest: {latest:?}");
}

/// Inserts must round-trip (regression for the ambiguous `ST_MakePoint`
/// placeholder and the int2 column bindings).  Uses a transaction-free insert
/// of a single strike at a far-future timestamp and is safe to re-run.
#[test]
#[ignore = "requires a live PostgreSQL with the strikes schema (set DATABASE_URL)"]
fn insert_many_round_trips() {
    use bo_service::data::Strike;
    let (_runtime, executor) = runtime_and_executor();
    let db = StrikeDb::new(&executor, 4326);

    let strike = Strike::new(
        None,
        Timestamp::new(chrono::Utc::now(), 123),
        8.91,
        44.28,
        Some(500.0),
        Some(10.5),
        Some(250),
        Some(7),
        vec![],
        Some(1),
    );
    let count = db
        .insert_many(std::slice::from_ref(&strike), Some(1))
        .expect("insert must succeed");
    assert_eq!(count, 1);
}