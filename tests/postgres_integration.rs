//! Live PostgreSQL integration tests against an ephemeral PostGIS container.
//!
//! The container and its canonical `strikes` schema are provided by
//! [`support::test_db`], which mirrors the Python project's testcontainers
//! fixture.  Docker must be running; no pre-provisioned database or
//! `DATABASE_URL` is required.
//!
//! They are opt-in (the `db-integration` feature) so the default `cargo test`
//! needs no Docker:
//!
//! ```sh
//! cargo test --features db-integration --test postgres_integration -- --nocapture
//! ```
//!
//! They are the regression guard for the parameter-encoding bugs found against
//! the real server (0x00 / UTF8 errors, int2 deserialization, ambiguous
//! `ST_Transform` and `ST_MakePoint` placeholders).

mod support;

use bo_service::data::Timestamp;
use bo_service::db::StrikeDb;
use bo_service::query::TimeInterval;

/// The default `bo-db` select must run against the real server without a
/// parameter-encoding error (regression for the `0x00`/UTF8 failure from the
/// ambiguous `ST_Transform(geog::geometry, $1)` placeholder), and its rows
/// must deserialize (regression for the `int2` -> `i32` failure).
#[test]
fn default_select_executes_and_deserializes() {
    let (runtime, executor) = support::test_db().runtime_and_executor();
    runtime.block_on(async {
        let db = StrikeDb::new(&executor, 4326);
        let now = chrono::Utc::now();
        let interval = TimeInterval::new(now - chrono::Duration::hours(24), now);
        let strikes = db
            .select(&interval, None, None)
            .await
            .expect("select must succeed");
        println!("selected {} strikes", strikes.len());
    });
}

/// `bo-db --grid` must run (ambiguous `ST_Transform` + float casts).
#[test]
fn grid_query_executes() {
    use bo_service::geom::Grid;
    let (runtime, executor) = support::test_db().runtime_and_executor();
    runtime.block_on(async {
        let db = StrikeDb::new(&executor, 4326);
        let now = chrono::Utc::now();
        let interval = TimeInterval::new(now - chrono::Duration::hours(24), now);
        // A bounded envelope (a full -180..180 envelope makes PostGIS reject the
        // geography ring as antipodal, unrelated to parameter encoding).
        let grid = Grid::new(-20.0, 30.0, 30.0, 60.0, 1.0, 1.0);
        db.select_grid(&grid, 0, &interval, None)
            .await
            .expect("grid query must succeed");
    });
}

/// `select_strike_keys` (`bo-update` de-duplication) must run.
#[test]
fn select_strike_keys_executes() {
    let (runtime, executor) = support::test_db().runtime_and_executor();
    runtime.block_on(async {
        let db = StrikeDb::new(&executor, 4326);
        let now = chrono::Utc::now();
        let interval = TimeInterval::new(now - chrono::Duration::hours(24), now);
        db.select_strike_keys(&interval, None, None)
            .await
            .expect("select_strike_keys must succeed");
    });
}

/// `get_latest_time` (`bo-import` start point) must run (`region=$1::smallint`).
#[test]
fn get_latest_time_executes() {
    let (runtime, executor) = support::test_db().runtime_and_executor();
    runtime.block_on(async {
        let db = StrikeDb::new(&executor, 4326);
        let latest = db
            .get_latest_time(Some(1))
            .await
            .expect("get_latest_time must succeed");
        println!("latest: {latest:?}");
    });
}

/// Inserts must round-trip (regression for the ambiguous `ST_MakePoint`
/// placeholder and the int2 column bindings) and be readable back.
#[test]
fn insert_many_round_trips() {
    use bo_service::data::Strike;
    let (runtime, executor) = support::test_db().runtime_and_executor();

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
    runtime.block_on(async {
        let db = StrikeDb::new(&executor, 4326);
        let count = db
            .insert_many(std::slice::from_ref(&strike), Some(1))
            .await
            .expect("insert must succeed");
        assert_eq!(count, 1);

        // The inserted strike is within the last hour, so a region select must
        // see at least this one row.
        let now = chrono::Utc::now();
        let interval = TimeInterval::new(now - chrono::Duration::hours(1), now);
        let rows = db
            .select(&interval, None, Some(1))
            .await
            .expect("select after insert must succeed");
        assert!(rows.iter().any(|s| (s.x - 8.91).abs() < 1e-6));
    });
}
