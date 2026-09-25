//! Shared integration-test support: an ephemeral PostGIS container with the
//! canonical `strikes` schema.
//!
//! Mirrors the Python project's `tests/conftest.py` (testcontainers +
//! `docs/schema/strikes.sql`): one container is started per test binary and
//! reused by every test, so the integration tests need no pre-provisioned
//! database or `DATABASE_URL`.
//!
//! The image defaults to the multi-arch community build
//! (`imresamu/postgis:16-3.5`) and can be overridden with
//! `BLITZORTUNG_TEST_POSTGIS_IMAGE`, like the Python suite.

// Each integration-test crate that pulls this in as `mod support;` only uses
// part of the API, so silence the per-crate dead-code warnings.
#![allow(dead_code)]

use std::sync::OnceLock;

use testcontainers::runners::SyncRunner;
use testcontainers::{Container, ImageExt};
use testcontainers_modules::postgres::Postgres;

use bo_service::config::Config;
use bo_service::executor::QueryExecutor;
use bo_service::postgres::PostgresExecutor;

/// Canonical schema/index set, applied verbatim on container startup (the
/// Docker entrypoint runs everything in `/docker-entrypoint-initdb.d`).
const SCHEMA_SQL: &[u8] = include_bytes!("../schema/strikes.sql");

const DEFAULT_IMAGE: &str = "imresamu/postgis:16-3.5";
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
}

static TEST_DB: OnceLock<TestDb> = OnceLock::new();

/// The process-wide PostGIS container, started on first use.
pub fn test_db() -> &'static TestDb {
    TEST_DB.get_or_init(TestDb::start)
}
