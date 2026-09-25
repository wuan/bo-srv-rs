//! Shared integration-test support: an ephemeral PostGIS container with the
//! canonical `strikes` schema.
//!
//! Mirrors the Python project's `tests/conftest.py` (testcontainers +
//! `docs/schema/strikes.sql`): one container is started per test binary and
//! reused by every test, so the integration tests need no pre-provisioned
//! database or `DATABASE_URL`.
//!
//! The image defaults to the multi-arch community build
//! (`imresamu/postgis:18-3.6`) and can be overridden with
//! `BLITZORTUNG_TEST_POSTGIS_IMAGE`, like the Python suite.

// Each integration-test crate that pulls this in as `mod support;` only uses
// part of the API, so silence the per-crate dead-code warnings.
#![allow(dead_code)]

use std::sync::{Mutex, MutexGuard, OnceLock};

use testcontainers::runners::SyncRunner;
use testcontainers::{Container, ImageExt};
use testcontainers_modules::postgres::Postgres;

use blitzortung_srv::config::Config;
use blitzortung_srv::executor::QueryExecutor;
use blitzortung_srv::postgres::PostgresExecutor;

/// Canonical schema/index set, applied verbatim on container startup (the
/// Docker entrypoint runs everything in `/docker-entrypoint-initdb.d`).
const SCHEMA_SQL: &[u8] = include_bytes!("../schema/strikes.sql");

const DEFAULT_IMAGE: &str = "imresamu/postgis:18-3.6";
const DB_NAME: &str = "blitzortung";
const DB_USER: &str = "blitzortung";
const DB_PASSWORD: &str = "blitzortung";

/// A started PostGIS container plus the [`Config`] that points at it.
///
/// The container is stopped and removed when the process exits. It is started
/// lazily and exactly once via [`test_db`].
pub struct TestDb {
    /// Kept alive so the container is not removed while tests run.
    _container: Container<Postgres>,
    config: Config,
}

impl TestDb {
    fn start() -> Self {
        let image = std::env::var("BLITZORTUNG_TEST_POSTGIS_IMAGE")
            .unwrap_or_else(|_| DEFAULT_IMAGE.to_string());
        let (name, tag) = image
            .rsplit_once(':')
            .map(|(name, tag)| (name.to_string(), tag.to_string()))
            .unwrap_or((image, "latest".to_string()));

        let container = Postgres::default()
            .with_user(DB_USER)
            .with_password(DB_PASSWORD)
            .with_db_name(DB_NAME)
            .with_init_sql(SCHEMA_SQL.to_vec())
            .with_name(name)
            .with_tag(tag)
            .start()
            .expect("failed to start the PostGIS testcontainer (is Docker running?)");

        let config = Config {
            db_host: container.get_host().expect("container host").to_string(),
            db_port: container
                .get_host_port_ipv4(5432)
                .expect("mapped postgres port")
                .to_string(),
            db_name: DB_NAME.into(),
            db_user: DB_USER.into(),
            db_password: DB_PASSWORD.into(),
            ..Config::default()
        };

        TestDb {
            _container: container,
            config,
        }
    }

    /// The connection configuration for the ephemeral database.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// A Tokio runtime plus an eagerly-connected [`PostgresExecutor`] for this
    /// container. `PostgresExecutor::connect` spawns the connection driver, so
    /// it must be called from within the returned runtime.
    pub fn runtime_and_executor(&self) -> (tokio::runtime::Runtime, PostgresExecutor) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build runtime");
        let executor = runtime
            .block_on(PostgresExecutor::connect(&self.config))
            .expect("connect to the testcontainer");
        (runtime, executor)
    }

    /// Convenience: a connected executor as a trait object.
    pub fn executor(&self) -> (tokio::runtime::Runtime, std::sync::Arc<dyn QueryExecutor>) {
        let (runtime, executor) = self.runtime_and_executor();
        (runtime, std::sync::Arc::new(executor))
    }

    /// Remove every row and restart the `bigserial` sequence.
    ///
    /// The Python fixture recreates the schema before each test; the Rust
    /// container is process-wide, so tests that assert exact row counts call
    /// this while holding the [`serial`] guard.  `RESTART IDENTITY` keeps
    /// `id`/`timestamp` values deterministic across tests.
    pub fn truncate(
        &self,
        runtime: &tokio::runtime::Runtime,
        executor: &dyn QueryExecutor,
    ) {
        runtime
            .block_on(executor.execute("TRUNCATE strikes RESTART IDENTITY", &[]))
            .expect("truncate strikes");
    }
}

static TEST_DB: OnceLock<TestDb> = OnceLock::new();

/// The process-wide PostGIS container, started on first use.
pub fn test_db() -> &'static TestDb {
    TEST_DB.get_or_init(TestDb::start)
}

/// Serializes tests that inspect or mutate the whole `strikes` table.
///
/// The container is shared by every test in a binary, so tests that assert
/// exact row counts, [truncate](TestDb::truncate) the table, or read all rows
/// must take this lock first.  A poisoned lock is recovered: one failing test
/// should not cascade into the rest.
static DATA_LOCK: Mutex<()> = Mutex::new(());

/// Take the data lock; see [`DATA_LOCK`].
pub fn serial() -> MutexGuard<'static, ()> {
    DATA_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
