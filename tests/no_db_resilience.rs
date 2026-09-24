//! Regression tests for "the webservice must tolerate a missing database".
//!
//! The service used to connect eagerly at startup and exit when the database
//! was unreachable.  The executor now connects lazily, so:
//!
//! * building the service never touches the database,
//! * a request that needs the database gets a normal JSON-RPC **fault**
//!   envelope (not a hang, a panic or a silent `null` result), and
//! * the first failure is logged once, the rest are silent.
//!
//! These tests point the executor at a TCP port nothing is listening on, so no
//! live PostgreSQL is required.

use std::net::TcpListener;
use std::sync::Arc;

use bo_service::config::Config;
use bo_service::executor::QueryExecutor;
use bo_service::jsonrpc;
use bo_service::metrics::NoopMetrics;
use bo_service::postgres::PostgresExecutor;
use bo_service::service::{Request, Service};

/// A config pointing at a dead local port (bound then released).
fn config_for_dead_port() -> Config {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    drop(listener);
    Config {
        db_host: "127.0.0.1".into(),
        db_port: port,
        db_name: "blitzortung".into(),
        db_user: "blitzortung".into(),
        db_password: "blitzortung".into(),
        ..Config::default()
    }
}

/// A lazy executor + service wired exactly like `main.rs` does.
fn service_with_dead_database() -> Service<NoopMetrics> {
    let executor = PostgresExecutor::lazy(&config_for_dead_port());
    let executor: Arc<dyn bo_service::executor::QueryExecutor> = Arc::new(executor);
    Service::with_parts(
        executor,
        bo_service::cache::ServiceCache::new(),
        NoopMetrics,
        Default::default(),
    )
}

/// A data request that passes validation but needs the database: with no
/// database it must return a v2 JSON-RPC **fault**, not hang or panic.
#[test]
fn data_request_without_database_returns_a_fault_envelope() {
    let service = service_with_dead_database();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let body = rt.block_on(async {
        let mut request = Request {
            user_agent: Some("bo-android-190".to_string()),
            content_type: Some("text/json".to_string()),
            ..Default::default()
        };
        jsonrpc::dispatch(
            &service,
            &mut request,
            r#"{"jsonrpc":"2.0","id":42,"method":"get_strikes_grid","params":[60,10000,0,1,0]}"#,
        )
        .await
        .expect_response()
    });

    let value: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    assert_eq!(value["id"], 42);
    assert!(
        value["result"].is_null(),
        "a fault must not carry a result: {value}"
    );
    assert!(
        value["error"].is_object(),
        "expected a fault envelope: {value}"
    );
    assert_eq!(value["error"]["code"], jsonrpc::FAILURE);
    assert!(
        value["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("database error")),
        "expected a database error message: {value}"
    );
}

/// The pre-1.0 dialect also renders the database failure as a fault dict.
#[test]
fn legacy_dialect_without_database_returns_a_fault() {
    let service = service_with_dead_database();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let body = rt.block_on(async {
        let mut request = Request {
            user_agent: Some("bo-android-190".to_string()),
            content_type: Some("text/json".to_string()),
            ..Default::default()
        };
        jsonrpc::dispatch(
            &service,
            &mut request,
            r#"{"method":"get_strikes_grid","params":[60,10000,0,1,0],"id":0}"#,
        )
        .await
        .expect_response()
    });

    let value: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    assert_eq!(value["faultCode"], jsonrpc::FAILURE);
    // A pre-1.0 fault is a bare dict, not wrapped in an array.
    assert!(value.is_object(), "expected a pre-1.0 fault dict: {value}");
}

/// Repeated requests against a database that stays down are answered normally
/// (no panic/hang) and only the first failure is logged.
#[tokio::test]
async fn repeated_failures_are_logged_once() {
    let executor = Arc::new(PostgresExecutor::lazy(&config_for_dead_port()));
    let tracker = executor.failures().clone();
    let executor: Arc<dyn bo_service::executor::QueryExecutor> = executor;
    let service = Service::with_parts(
        executor,
        bo_service::cache::ServiceCache::new(),
        NoopMetrics,
        Default::default(),
    );

    for _ in 0..4 {
        let mut request = Request {
            user_agent: Some("bo-android-190".to_string()),
            content_type: Some("text/json".to_string()),
            ..Default::default()
        };
        let result = jsonrpc::dispatch(
            &service,
            &mut request,
            r#"{"jsonrpc":"2.0","id":1,"method":"get_strikes_grid","params":[60,10000,0,1,0]}"#,
        )
        .await
        .expect_response();
        let value: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(value["error"].is_object(), "expected a fault: {value}");
    }

    assert_eq!(
        tracker.log_count(),
        1,
        "a sustained outage must be logged exactly once"
    );
}

/// A lazy test against a real database: a plain query must succeed, while the
/// same executor pointed at a dead port must fail (per request, no hang).
///
/// It deliberately uses `SELECT 1` so it only needs *any* PostgreSQL, not the
/// PostGIS `strikes` schema; the full grid request is covered by
/// `tests/service_live.rs` and `tests/postgres_integration.rs`.
#[test]
#[ignore = "requires a live PostgreSQL (set DATABASE_URL)"]
fn live_database_serves_and_dead_port_faults() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    // 1) Live database (from DATABASE_URL): the lazy executor connects on the
    // first query and returns rows.
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let mut live = Config::default();
    for pair in url.split_whitespace() {
        if let Some((key, value)) = pair.split_once('=') {
            let value = value.trim_matches('\'').to_string();
            match key {
                "host" => live.db_host = value,
                "port" => live.db_port = value,
                "dbname" => live.db_name = value,
                "user" => live.db_user = value,
                "password" => live.db_password = value,
                _ => {}
            }
        }
    }
    let live_executor = PostgresExecutor::lazy(&live);
    let rows = rt
        .block_on(live_executor.query("SELECT 1", &[]))
        .expect("live query");
    assert_eq!(rows.len(), 1);
    assert_eq!(live_executor.failures().log_count(), 0);

    // 2) Dead port: the same request shape must fault (and not panic/hang).
    let dead_service = {
        let executor: Arc<dyn bo_service::executor::QueryExecutor> =
            Arc::new(PostgresExecutor::lazy(&config_for_dead_port()));
        Service::with_parts(
            executor,
            bo_service::cache::ServiceCache::new(),
            NoopMetrics,
            Default::default(),
        )
    };
    let dead_body = rt.block_on(async {
        let mut request = Request {
            user_agent: Some("bo-android-190".to_string()),
            content_type: Some("text/json".to_string()),
            ..Default::default()
        };
        jsonrpc::dispatch(
            &dead_service,
            &mut request,
            r#"{"jsonrpc":"2.0","id":2,"method":"get_strikes_grid","params":[60,10000,0,1,0]}"#,
        )
        .await
        .expect_response()
    });
    let value: serde_json::Value = serde_json::from_str(&dead_body).unwrap();
    assert!(value["error"].is_object(), "dead port must fault: {value}");
}
