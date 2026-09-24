//! Verifies that a dispatched JSON-RPC request produces one access log line at
//! the expected level, by installing a capturing logger and calling the real
//! `transport::log_access`.
//!
//! This lives in its own integration-test binary because `log::set_logger` is a
//! process-global one-shot.

use std::sync::{Mutex, OnceLock};

use bo_service::jsonrpc::{Outcome, RequestMeta};
use bo_service::service::Request;
use bo_service::transport;

/// A capturing logger: keeps `(level, message)` for every record.
struct CaptureLogger {
    records: Mutex<Vec<(log::Level, String)>>,
}

static LOGGER: OnceLock<&'static CaptureLogger> = OnceLock::new();

/// Serializes the tests: they share the process-global logger, so running them
/// in parallel would let one test's clear/emit interleave with another's.
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn logger() -> &'static CaptureLogger {
    LOGGER.get_or_init(|| {
        let logger: &'static CaptureLogger = Box::leak(Box::new(CaptureLogger {
            records: Mutex::new(Vec::new()),
        }));
        log::set_logger(logger).expect("logger set once");
        log::set_max_level(log::LevelFilter::Trace);
        logger
    })
}

impl log::Log for CaptureLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        self.records
            .lock()
            .unwrap()
            .push((record.level(), record.args().to_string()));
    }

    fn flush(&self) {}
}

fn last_record(logger: &CaptureLogger) -> (log::Level, String) {
    logger
        .records
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("at least one record")
}

fn client_request() -> Request {
    Request {
        user_agent: Some("bo-android-190".to_string()),
        client_ip: Some("203.0.113.7".to_string()),
        content_type: Some("text/json".to_string()),
        ..Default::default()
    }
}

#[test]
fn success_is_logged_at_info_with_method_and_client() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let logger = logger();
    logger.records.lock().unwrap().clear();
    let meta = RequestMeta {
        method: Some("get_strikes_grid".to_string()),
        id: "1".to_string(),
        params: "[60,10000,0,1,0]".to_string(),
        outcome: Some(Outcome::Success),
    };
    transport::log_access(&client_request(), &meta, 12.3);
    let (level, message) = last_record(logger);
    assert_eq!(level, log::Level::Info);
    assert!(message.contains("get_strikes_grid([60,10000,0,1,0])"), "{message}");
    assert!(message.contains("client=203.0.113.7"), "{message}");
    assert!(message.contains("ua=bo-android-190"), "{message}");
    assert!(message.contains("12.3ms"), "{message}");
    // No credentials leaked.
    assert!(!message.to_lowercase().contains("password"));
}

#[test]
fn blocked_is_logged_at_warn_with_blocked_marker() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let logger = logger();
    logger.records.lock().unwrap().clear();
    let meta = RequestMeta {
        method: Some("get_strikes_grid".to_string()),
        id: "1".to_string(),
        params: "[60,10000,0,1,0]".to_string(),
        outcome: Some(Outcome::Blocked("invalid user agent \"Mozilla/5.0\"".to_string())),
    };
    let mut request = client_request();
    request.user_agent = Some("Mozilla/5.0".to_string());
    transport::log_access(&request, &meta, 0.5);
    let (level, message) = last_record(logger);
    assert_eq!(level, log::Level::Warn);
    assert!(message.contains("BLOCKED"), "{message}");
    assert!(message.contains("invalid user agent"), "{message}");
}

#[test]
fn fault_is_logged_at_warn_with_code() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let logger = logger();
    logger.records.lock().unwrap().clear();
    let meta = RequestMeta {
        method: Some("nope".to_string()),
        id: "1".to_string(),
        params: "[]".to_string(),
        outcome: Some(Outcome::Fault {
            code: -32601,
            message: "function nope not found".to_string(),
        }),
    };
    transport::log_access(&client_request(), &meta, 0.1);
    let (level, message) = last_record(logger);
    assert_eq!(level, log::Level::Warn);
    assert!(message.contains("fault -32601"), "{message}");
}