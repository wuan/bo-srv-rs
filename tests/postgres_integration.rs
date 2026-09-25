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
//! `ST_Transform` and `ST_MakePoint` placeholders), and port the database
//! suite from the Python project (`tests/db/test_db.py`).

mod support;

use std::collections::{BTreeSet, HashSet};

use blitzortung_srv::config::Config;
use blitzortung_srv::data::{GridData, Strike, Timestamp};
use blitzortung_srv::db::{HashableStrikeKey, StrikeDb};
use blitzortung_srv::executor::{QueryExecutor, Value};
use blitzortung_srv::geom::Grid;
use blitzortung_srv::postgres::PostgresExecutor;
use blitzortung_srv::query::{self, TimeInterval};
use blitzortung_srv::service::build_histogram;

/// Serializes every test and gives each a clean table.
///
/// The PostGIS container is shared by the whole binary, so tests that assert
/// exact row counts take [`support::serial`] and truncate `strikes` first.
struct TestContext {
    _guard: std::sync::MutexGuard<'static, ()>,
    runtime: tokio::runtime::Runtime,
    executor: blitzortung_srv::postgres::PostgresExecutor,
}

impl TestContext {
    fn new() -> Self {
        let guard = support::serial();
        let (runtime, executor) = support::test_db().runtime_and_executor();
        support::test_db().truncate(&runtime, &executor);
        TestContext {
            _guard: guard,
            runtime,
            executor,
        }
    }
}

/// A strike with a known timestamp, coordinates and optional region.
fn strike_now(x: f64, y: f64, region: Option<i64>) -> Strike {
    Strike::new(
        None,
        Timestamp::new(chrono::Utc::now(), 123),
        x,
        y,
        Some(500.0),
        Some(10.5),
        Some(250),
        Some(7),
        vec![],
        region,
    )
}

/// `now - 1h .. now + 1h`, so freshly inserted strikes are always in range.
fn recent_interval() -> TimeInterval {
    let now = chrono::Utc::now();
    TimeInterval::new(now - chrono::Duration::hours(1), now + chrono::Duration::hours(1))
}

/// Sum of all cell counts in a [`GridData`] result.
fn grid_total(data: &GridData, grid: &Grid) -> i64 {
    let mut total = 0;
    for x in 0..grid.x_bin_count() {
        for y in 0..grid.y_bin_count() {
            if let Some(cell) = data.get(x, y) {
                total += cell.count;
            }
        }
    }
    total
}

/// The default `bo-db` select must run against the real server without a
/// parameter-encoding error (regression for the `0x00`/UTF8 failure from the
/// ambiguous `ST_Transform(geog::geometry, $1)` placeholder), and its rows
/// must deserialize (regression for the `int2` -> `i32` failure).
#[test]
fn default_select_executes_and_deserializes() {
    let ctx = TestContext::new();
    ctx.runtime.block_on(async {
        let db = StrikeDb::new(&ctx.executor, 4326);
        let strikes = db
            .select(&recent_interval(), None, None)
            .await
            .expect("select must succeed");
        println!("selected {} strikes", strikes.len());
    });
}

/// Python `test_empty_query`: a select over an empty table yields no rows.
#[test]
fn empty_table_select_returns_no_rows() {
    let ctx = TestContext::new();
    ctx.runtime.block_on(async {
        let db = StrikeDb::new(&ctx.executor, 4326);
        let strikes = db
            .select(&recent_interval(), None, None)
            .await
            .expect("select must succeed");
        assert!(strikes.is_empty(), "expected no strikes, got {strikes:?}");
    });
}

/// Python `test_insert_and_select_strike` plus full field round-tripping.
#[test]
fn insert_single_then_select_round_trips() {
    let ctx = TestContext::new();
    let strike = strike_now(8.91, 44.28, None);
    ctx.runtime.block_on(async {
        let db = StrikeDb::new(&ctx.executor, 4326);
        db.insert(&strike, 1).await.expect("insert must succeed");

        let rows = db
            .select(&recent_interval(), None, Some(1))
            .await
            .expect("select after insert must succeed");
        assert_eq!(rows.len(), 1);

        let row = &rows[0];
        assert_eq!(row.id, Some(1)); // RESTART IDENTITY after truncate
        assert_eq!(row.timestamp.value(), strike.timestamp.value());
        assert!((row.x - 8.91).abs() < 1e-9);
        assert!((row.y - 44.28).abs() < 1e-9);
        assert_eq!(row.altitude, Some(500.0));
        assert_eq!(row.amplitude, Some(10.5));
        assert_eq!(row.lateral_error, Some(250));
        assert_eq!(row.station_count, Some(7));
        assert_eq!(row.region, None);
    });
}

/// Python `test_insert_many_inserts_all_strikes`: a region-pinned batch is
/// counted under that region only.
#[test]
fn insert_many_with_region_inserts_all() {
    let ctx = TestContext::new();
    let strikes: Vec<Strike> = (0..5)
        .map(|index| strike_now(11.0 + index as f64, 49.0, None))
        .collect();
    ctx.runtime.block_on(async {
        let db = StrikeDb::new(&ctx.executor, 4326);
        let inserted = db
            .insert_many(&strikes, Some(3))
            .await
            .expect("insert_many must succeed");
        assert_eq!(inserted, 5);

        let region_3 = db
            .select(&recent_interval(), None, Some(3))
            .await
            .expect("select region 3");
        assert_eq!(region_3.len(), 5);

        let region_4 = db
            .select(&recent_interval(), None, Some(4))
            .await
            .expect("select region 4");
        assert!(region_4.is_empty());
    });
}

/// Python `test_insert_many_uses_per_strike_region`: with no batch region, each
/// strike's own region is written.
#[test]
fn insert_many_uses_per_strike_region() {
    let ctx = TestContext::new();
    let strikes = vec![
        strike_now(11.0, 49.0, Some(4)),
        strike_now(12.0, 49.0, Some(5)),
    ];
    ctx.runtime.block_on(async {
        let db = StrikeDb::new(&ctx.executor, 4326);
        db.insert_many(&strikes, None)
            .await
            .expect("insert_many must succeed");

        assert_eq!(
            db.select(&recent_interval(), None, Some(4))
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db.select(&recent_interval(), None, Some(5))
                .await
                .unwrap()
                .len(),
            1
        );
    });
}

/// Python `test_select_strike_keys_matches_full_select`: the de-duplication
/// keys agree with the rounded coordinates of a full select.
#[test]
fn select_strike_keys_matches_full_select() {
    let ctx = TestContext::new();
    let strikes: Vec<Strike> = (0..5)
        .map(|index| strike_now(11.0 + index as f64, 49.0, None))
        .collect();
    ctx.runtime.block_on(async {
        let db = StrikeDb::new(&ctx.executor, 4326);
        db.insert_many(&strikes, Some(2))
            .await
            .expect("insert_many must succeed");

        let keys: HashSet<HashableStrikeKey> = db
            .select_strike_keys(&recent_interval(), None, Some(2))
            .await
            .expect("select_strike_keys must succeed")
            .into_iter()
            .map(HashableStrikeKey::from)
            .collect();

        let expected: HashSet<HashableStrikeKey> = db
            .select(&recent_interval(), None, Some(2))
            .await
            .expect("select must succeed")
            .into_iter()
            .map(|strike| HashableStrikeKey::from_strike(&strike))
            .collect();

        assert_eq!(keys, expected);
        assert_eq!(keys.len(), 5);
    });
}

/// Python `test_select_strike_keys_with_empty_table`.
#[test]
fn select_strike_keys_empty_table() {
    let ctx = TestContext::new();
    ctx.runtime.block_on(async {
        let db = StrikeDb::new(&ctx.executor, 4326);
        let keys = db
            .select_strike_keys(&recent_interval(), None, None)
            .await
            .expect("select_strike_keys must succeed");
        assert!(keys.is_empty());
    });
}

/// `bo-db --grid` must run (ambiguous `ST_Transform` + float casts).
#[test]
fn grid_query_executes() {
    let ctx = TestContext::new();
    ctx.runtime.block_on(async {
        let db = StrikeDb::new(&ctx.executor, 4326);
        // A bounded envelope (a full -180..180 envelope makes PostGIS reject the
        // geography ring as antipodal, unrelated to parameter encoding).
        let grid = Grid::new(-20.0, 30.0, 30.0, 60.0, 1.0, 1.0);
        db.select_grid(&grid, 0, &recent_interval(), None)
            .await
            .expect("grid query must succeed");
    });
}

/// Python `test_grid_query`: a strike lands in the expected cell.
#[test]
fn grid_query_places_strike_in_expected_cell() {
    let ctx = TestContext::new();
    let grid = Grid::new(10.0, 20.0, 40.0, 50.0, 1.0, 1.0);
    ctx.runtime.block_on(async {
        let db = StrikeDb::new(&ctx.executor, 4326);
        db.insert(&strike_now(11.5, 49.5, None), 1)
            .await
            .expect("insert must succeed");

        let data = db
            .select_grid(&grid, 0, &recent_interval(), None)
            .await
            .expect("grid query must succeed");

        // rx = trunc(11.5 - 10) = 1, ry = trunc(49.5 - 40) = 9, and the grid
        // result flips y so y_index = y_bin_count - ry = 10 - 9 = 1.
        assert_eq!(data.get(1, 1).map(|cell| cell.count), Some(1));
        assert_eq!(grid_total(&data, &grid), 1);
    });
}

/// Python `test_grid_query_with_count_threshold`: `count(*) > threshold` drops
/// sparse cells.
#[test]
fn grid_query_count_threshold_filters_cells() {
    let ctx = TestContext::new();
    let grid = Grid::new(10.0, 20.0, 40.0, 50.0, 1.0, 1.0);
    ctx.runtime.block_on(async {
        let db = StrikeDb::new(&ctx.executor, 4326);
        db.insert_many(
            &[
                strike_now(11.5, 49.5, None),
                strike_now(11.5, 49.5, None),
                strike_now(12.5, 49.5, None),
            ],
            Some(1),
        )
        .await
        .expect("insert_many must succeed");

        let data = db
            .select_grid(&grid, 1, &recent_interval(), None)
            .await
            .expect("grid query must succeed");

        // Cell (1,1) has two strikes, cell (2,1) only one and is filtered.
        assert_eq!(data.get(1, 1).map(|cell| cell.count), Some(2));
        assert_eq!(data.get(2, 1), None);
    });
}

/// Python `test_grid_query_region_filter`: strikes are counted per region.
#[test]
fn grid_query_region_filter() {
    let ctx = TestContext::new();
    let grid = Grid::new(10.0, 20.0, 40.0, 50.0, 1.0, 1.0);
    ctx.runtime.block_on(async {
        let db = StrikeDb::new(&ctx.executor, 4326);
        db.insert(&strike_now(11.5, 49.5, None), 1)
            .await
            .unwrap();
        db.insert(&strike_now(11.6, 49.5, None), 6)
            .await
            .unwrap();

        let region_1 = db
            .select_grid(&grid, 0, &recent_interval(), Some(1))
            .await
            .unwrap();
        let region_6 = db
            .select_grid(&grid, 0, &recent_interval(), Some(6))
            .await
            .unwrap();
        let unfiltered = db
            .select_grid(&grid, 0, &recent_interval(), None)
            .await
            .unwrap();

        assert_eq!(grid_total(&region_1, &grid), 1);
        assert_eq!(grid_total(&region_6, &grid), 1);
        assert_eq!(grid_total(&unfiltered, &grid), 2);
    });
}

/// Python `test_global_grid_query`: the envelope-less world grid bins a strike
/// with half-cell rounding.
#[test]
fn global_grid_query_places_strike() {
    let ctx = TestContext::new();
    let grid = Grid::new(-180.0, 180.0, -90.0, 90.0, 1.0, 1.0);
    ctx.runtime.block_on(async {
        let db = StrikeDb::new(&ctx.executor, 4326);
        db.insert(&strike_now(11.5, 49.5, None), 1)
            .await
            .unwrap();

        let query = query::global_grid_query(&grid, &recent_interval(), 0);
        let rows = ctx
            .executor
            .query(&query.to_postgres(), &query.parameters())
            .await
            .expect("global grid query must succeed");

        assert_eq!(rows.len(), 1);
        // rx = ROUND((11.5 - 0.5) / 1) = 11, ry = ROUND((49.5 - 0.5) / 1) = 49.
        assert_eq!(rows[0].get_i64(0), Some(11));
        assert_eq!(rows[0].get_i64(1), Some(49));
        assert_eq!(rows[0].get_i64(2), Some(1));
    });
}

/// Python `test_histogram_query`: bins carry the per-bin strike counts.
#[test]
fn histogram_query_bins_strikes() {
    let ctx = TestContext::new();
    let base = chrono::Utc::now();
    ctx.runtime.block_on(async {
        let db = StrikeDb::new(&ctx.executor, 4326);
        db.insert(
            &Strike::new(
                None,
                Timestamp::new(base - chrono::Duration::minutes(5), 0),
                11.5,
                49.5,
                None,
                None,
                None,
                None,
                vec![],
                None,
            ),
            1,
        )
        .await
        .unwrap();

        let interval = TimeInterval::new(base - chrono::Duration::minutes(30), base);
        let query = query::histogram_query(&interval, 5, Some(1), None);
        let rows = ctx
            .executor
            .query(&query.to_postgres(), &query.parameters())
            .await
            .expect("histogram query must succeed");

        let bins = build_histogram(&rows, interval.minutes(), 5)
            .expect("build_histogram must succeed");
        let counts: Vec<i64> = bins.iter().map(|value| value.as_i64().unwrap_or(-1)).collect();
        // Six 5-minute bins; the strike 5 minutes before the end lands in the
        // fifth bin (index 4).
        assert_eq!(counts, vec![0, 0, 0, 0, 1, 0]);
    });
}

/// `get_latest_time` (`bo-import` start point) must run (`region=$1::smallint`).
#[test]
fn get_latest_time_executes() {
    let ctx = TestContext::new();
    ctx.runtime.block_on(async {
        let db = StrikeDb::new(&ctx.executor, 4326);
        let latest = db
            .get_latest_time(Some(1))
            .await
            .expect("get_latest_time must succeed");
        assert!(latest.is_none());
    });
}

/// Python `test_get_latest_time*`: the newest timestamp is returned, and the
/// region filter matches or rejects accordingly.
#[test]
fn get_latest_time_region_match_and_mismatch() {
    let ctx = TestContext::new();
    let strike = strike_now(11.0, 49.0, None);
    ctx.runtime.block_on(async {
        let db = StrikeDb::new(&ctx.executor, 4326);
        db.insert(&strike, 5).await.expect("insert must succeed");

        let unfiltered = db
            .get_latest_time(None)
            .await
            .unwrap()
            .expect("row present");
        assert_eq!(unfiltered.value(), strike.timestamp.value());

        let matching = db
            .get_latest_time(Some(5))
            .await
            .unwrap()
            .expect("region 5 has a row");
        assert_eq!(matching.value(), strike.timestamp.value());

        assert!(db.get_latest_time(Some(4)).await.unwrap().is_none());
    });
}

/// `select_strike_keys` (`bo-update` de-duplication) must run.
#[test]
fn select_strike_keys_executes() {
    let ctx = TestContext::new();
    ctx.runtime.block_on(async {
        let db = StrikeDb::new(&ctx.executor, 4326);
        db.select_strike_keys(&recent_interval(), None, None)
            .await
            .expect("select_strike_keys must succeed");
    });
}

/// Python `test_select_returns_all_rows`: every seeded row is returned.
#[test]
fn select_returns_all_seeded_rows() {
    let ctx = TestContext::new();
    let strikes: Vec<Strike> = (0..500)
        .map(|index| strike_now(10.0 + (index % 100) as f64 * 0.01, 45.0, None))
        .collect();
    ctx.runtime.block_on(async {
        let db = StrikeDb::new(&ctx.executor, 4326);
        let inserted = db
            .insert_many(&strikes, Some(1))
            .await
            .expect("insert_many must succeed");
        assert_eq!(inserted, 500);

        let rows = db
            .select(&recent_interval(), None, Some(1))
            .await
            .expect("select must succeed");
        assert_eq!(rows.len(), 500);
    });
}

/// Inserts must round-trip (regression for the ambiguous `ST_MakePoint`
/// placeholder and the int2 column bindings) and be readable back.
#[test]
fn insert_many_round_trips() {
    let ctx = TestContext::new();
    let strike = strike_now(8.91, 44.28, None);
    ctx.runtime.block_on(async {
        let db = StrikeDb::new(&ctx.executor, 4326);
        let count = db
            .insert_many(std::slice::from_ref(&strike), Some(1))
            .await
            .expect("insert must succeed");
        assert_eq!(count, 1);

        let rows = db
            .select(&recent_interval(), None, Some(1))
            .await
            .expect("select after insert must succeed");
        assert!(rows.iter().any(|s| (s.x - 8.91).abs() < 1e-6));
    });
}

/// Python `test_applied_schema_matches_production_indexes`: the schema applied
/// by the fixture creates exactly the indexes deployed in production.
#[test]
fn applied_schema_has_production_indexes() {
    let ctx = TestContext::new();
    ctx.runtime.block_on(async {
        let rows = ctx
            .executor
            .query(
                "SELECT indexname FROM pg_indexes WHERE tablename = 'strikes'",
                &[],
            )
            .await
            .expect("pg_indexes query must succeed");

        let names: BTreeSet<String> = rows
            .iter()
            .filter_map(|row| match row.get(0) {
                Some(Value::Text(name)) => Some(name.clone()),
                _ => None,
            })
            .collect();

        let expected: BTreeSet<String> = [
            "strikes_pkey",
            "strikes_region_timestamp",
            "strikes_timestamp",
            "strikes_timestamp_geog",
        ]
        .iter()
        .map(|name| name.to_string())
        .collect();

        assert_eq!(names, expected, "schema drifted from production indexes");
    });
}

/// The canonical schema declares `strikes` as a RANGE-partitioned table and an
/// insert routes into the UTC day's partition.
#[test]
fn strikes_is_partitioned_and_routes_by_timestamp() {
    let ctx = TestContext::new();
    ctx.runtime.block_on(async {
        let rows = ctx
            .executor
            .query(
                "SELECT c.relkind::text FROM pg_class c WHERE c.oid = 'strikes'::regclass",
                &[],
            )
            .await
            .expect("relkind query");
        assert!(
            matches!(rows[0].get(0), Some(Value::Text(t)) if t == "p"),
            "strikes must be a partitioned table (relkind 'p')"
        );

        let db = StrikeDb::new(&ctx.executor, 4326);
        db.insert(&strike_now(11.5, 49.5, Some(1)), 1)
            .await
            .expect("insert must succeed");

        let rows = ctx
            .executor
            .query(
                "SELECT tableoid::regclass::text FROM strikes WHERE region = 1",
                &[],
            )
            .await
            .expect("tableoid query");
        let partition = match rows[0].get(0) {
            Some(Value::Text(name)) => name.clone(),
            other => panic!("unexpected tableoid: {other:?}"),
        };
        let today = chrono::Utc::now().format("%Y%m%d").to_string();
        assert_eq!(
            partition,
            format!("strikes_p{today}"),
            "insert must land in the current UTC day's partition"
        );
    });
}

/// `strikes_create_partition` / `strikes_drop_old_partitions` manage the
/// retention window.
#[test]
fn partition_maintenance_creates_and_drops_old_partitions() {
    let _guard = support::serial();
    let (runtime, executor) = support::test_db().runtime_and_executor();
    runtime.block_on(async {
        executor
            .query("SELECT strikes_create_partition('2000-01-01'::date)", &[])
            .await
            .expect("create old partition");
        let rows = executor
            .query("SELECT to_regclass('strikes_p20000101') IS NULL", &[])
            .await
            .expect("to_regclass");
        assert_eq!(
            rows[0].get(0),
            Some(&Value::Bool(false)),
            "old partition must exist before the cleanup"
        );

        executor
            .query("SELECT strikes_drop_old_partitions('2 days'::interval)", &[])
            .await
            .expect("drop old partitions");
        let rows = executor
            .query("SELECT to_regclass('strikes_p20000101') IS NULL", &[])
            .await
            .expect("to_regclass");
        assert_eq!(
            rows[0].get(0),
            Some(&Value::Bool(true)),
            "old partition must be dropped"
        );
    });
}

/// `prepare_cached` reuses the prepared statement: with a single-connection pool
/// the second run of the same SQL text is a cache hit, so that connection's
/// statement cache holds exactly one entry.
#[test]
fn prepared_statements_are_reused_per_connection() {
    let db = support::test_db();
    let config = Config {
        db_connection_count: 1,
        ..db.config().clone()
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build runtime");
    let executor = PostgresExecutor::lazy(&config).expect("build executor");

    runtime.block_on(async {
        executor.query("SELECT 1", &[]).await.expect("first query");
        executor.query("SELECT 1", &[]).await.expect("second query");

        // With a single connection both queries ran on the same client, so the
        // cache must hold exactly the one prepared statement.
        let client = executor.pool().get().await.expect("checkout");
        assert_eq!(
            client.statement_cache.size(),
            1,
            "the second run must reuse the prepared statement"
        );
    });
}