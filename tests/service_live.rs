//! Live end-to-end service test (opt-in).
//!
//! Spins up the real TCP service against a live PostgreSQL with the PostGIS
//! `strikes` schema and sends a JSON-RPC request, exercising the full path
//! `transport` -> `jsonrpc` -> `service` -> async `query()`.  This guards the
//! whole async database path (previously a synchronous `block_on` here panicked
//! with `Cannot start a runtime from within a runtime`).
//!
//! Run with:
//! ```sh
//! export DATABASE_URL="host=127.0.0.1 port=5433 dbname=blitzortung user=blitzortung password=blitzortung"
//! cargo test --test service_live -- --ignored --nocapture
//! ```

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use blitzortung_srv::config::Config;
use blitzortung_srv::executor::QueryExecutor;
use blitzortung_srv::postgres::PostgresExecutor;
use blitzortung_srv::service::Service;
use blitzortung_srv::transport;

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

fn client_headers() -> &'static str {
    "User-Agent: bo-android-190\r\nContent-Type: text/json\r\n"
}

fn rpc(port: u16, body: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let framed = format!(
        "Content-Length: {}\r\n{}\r\n{}",
        body.len(),
        client_headers(),
        body
    );
    stream.write_all(framed.as_bytes()).unwrap();

    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
    }
    let headers = String::from_utf8(header).unwrap();
    let content_length: usize = headers
        .lines()
        .find_map(|l| {
            l.split_once(':')
                .filter(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
                .map(|(_, v)| v.trim().parse().unwrap())
        })
        .unwrap();

    let mut body_out = vec![0u8; content_length];
    stream.read_exact(&mut body_out).unwrap();
    String::from_utf8(body_out).unwrap()
}

/// Spin up the service on a multi-thread runtime (as `src/bin/bo-webservice.rs`
/// does) and talk to it over TCP.  Any request that runs a DB query must not
/// panic the worker thread.
#[test]
#[ignore = "requires a live PostgreSQL with the strikes schema (set DATABASE_URL)"]
fn service_survives_a_real_query_request() {
    let config = config_from_env();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();

    let executor = runtime
        .block_on(PostgresExecutor::connect(&config))
        .expect("connect");
    let executor: Arc<dyn QueryExecutor> = Arc::new(executor);
    let service = Arc::new(Service::new(executor));

    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let port = addr.port();
    let svc = service.clone();
    runtime.spawn(async move {
        let _ = transport::serve(listener, svc).await;
    });

    let body = rpc(
        port,
        r#"{"jsonrpc":"2.0","id":1,"method":"get_strikes_grid","params":{"minute_length":60,"grid_base_length":10000,"region":1}}"#,
    );
    assert!(
        body.contains("r") || body.contains("error"),
        "unexpected response: {body}"
    );
    println!("response: {}", &body[..body.len().min(160)]);
}
