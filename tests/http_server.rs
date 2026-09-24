//! End-to-end HTTP transport tests: spin up the HTTP server with a mock
//! executor and speak real HTTP/1.1 over TCP (as Nginx `proxy_pass` would),
//! asserting valid status lines, headers and JSON-RPC bodies.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use bo_service::executor::QueryExecutor;
use bo_service::http;
use bo_service::mock::MockExecutor;
use bo_service::service::Service;

fn run_server(eq: impl QueryExecutor + 'static) -> u16 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let listener = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let addr = listener.local_addr().unwrap();
    let eq: Arc<dyn QueryExecutor> = Arc::new(eq);
    let service: Arc<Service> = Arc::new(Service::new(eq));
    std::thread::spawn(move || {
        rt.block_on(http::serve(listener, service)).unwrap();
    });
    addr.port()
}

/// A parsed HTTP response.
struct HttpResponse {
    status_line: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Read one HTTP response from `stream`.
fn read_response(stream: &mut TcpStream) -> HttpResponse {
    read_response_inner(stream, true)
}

/// Read a response head only (for `HEAD`, where no body is sent).
fn read_response_head_only(stream: &mut TcpStream) -> HttpResponse {
    read_response_inner(stream, false)
}

fn read_response_inner(stream: &mut TcpStream, expect_body: bool) -> HttpResponse {
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
    }
    let header_text = String::from_utf8(header).unwrap();
    let mut lines = header_text.split("\r\n");
    let status_line = lines.next().unwrap().to_string();
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    let content_length: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .map(|(_, v)| v.parse().unwrap())
        .unwrap_or(0);
    let body = if expect_body && content_length > 0 {
        let mut body = vec![0u8; content_length];
        stream.read_exact(&mut body).unwrap();
        body
    } else {
        Vec::new()
    };
    HttpResponse {
        status_line,
        headers,
        body: String::from_utf8(body).unwrap(),
    }
}

/// Send an HTTP request on a fresh connection and read one response.
fn http_roundtrip(port: u16, request: &str) -> HttpResponse {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    read_response(&mut stream)
}

fn post(port: u16, headers: &str, body: &str) -> HttpResponse {
    // `headers` is a full block whose lines already end in CRLF.
    http_roundtrip(
        port,
        &format!(
            "POST / HTTP/1.1\r\nHost: localhost\r\n{headers}Content-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    )
}

const ANDROID_HEADERS: &str = "User-Agent: bo-android-190\r\nContent-Type: text/json\r\n";

#[test]
fn post_returns_valid_http_200_json() {
    let port = run_server(MockExecutor::new());
    let response = post(port, ANDROID_HEADERS, r#"{"jsonrpc":"2.0","id":1,"method":"check","params":[]}"#);
    assert_eq!(response.status_line, "HTTP/1.1 200 OK", "{:?}", response.status_line);
    assert_eq!(response.header("content-type"), Some("application/json"));
    let v: serde_json::Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(v["result"]["count"], 1);
    assert_eq!(v["id"], 1);
}

#[test]
fn missing_user_agent_is_blocked_but_still_http_200() {
    let port = run_server(MockExecutor::new());
    // No User-Agent -> base.py blocks data requests; the HTTP layer still
    // returns a well-formed 200 with the (empty-grid) body.
    let response = post(port, "Content-Type: text/json\r\n", r#"{"jsonrpc":"2.0","id":1,"method":"get_strikes_grid","params":[60,10000,0,1,0]}"#);
    assert_eq!(response.status_line, "HTTP/1.1 200 OK");
    assert_eq!(response.header("content-type"), Some("application/json"));
    let v: serde_json::Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(v["result"], serde_json::json!({}));
}

#[test]
fn keep_alive_serves_two_requests_on_one_connection() {
    let port = run_server(MockExecutor::new());
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    for id in [1, 2] {
        let body = format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"check","params":[]}}"#);
        let request = format!(
            "POST / HTTP/1.1\r\nHost: localhost\r\n{ANDROID_HEADERS}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).unwrap();
        let response = read_response(&mut stream);
        assert_eq!(response.status_line, "HTTP/1.1 200 OK");
        assert_eq!(response.header("connection"), Some("keep-alive"));
        let v: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(v["id"], id);
    }
}

#[test]
fn connection_close_is_honoured() {
    let port = run_server(MockExecutor::new());
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"check","params":[]}"#;
    let request = format!(
        "POST / HTTP/1.1\r\nHost: localhost\r\n{ANDROID_HEADERS}Connection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let response = http_roundtrip(port, &request);
    assert_eq!(response.status_line, "HTTP/1.1 200 OK");
    assert_eq!(response.header("connection"), Some("close"));
    let v: serde_json::Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(v["id"], 1);
}

#[test]
fn get_with_request_query_parameter() {
    let port = run_server(MockExecutor::new());
    let request = format!(
        "GET /?request=%7B%22jsonrpc%22%3A%222.0%22%2C%22id%22%3A7%2C%22method%22%3A%22check%22%2C%22params%22%3A%5B%5D%7D HTTP/1.1\r\nHost: localhost\r\n{ANDROID_HEADERS}\r\n"
    );
    let response = http_roundtrip(port, &request);
    assert_eq!(response.status_line, "HTTP/1.1 200 OK");
    let v: serde_json::Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(v["id"], 7);
    assert_eq!(v["result"]["count"], 1);
}

#[test]
fn jsonp_callback_sets_text_javascript() {
    let port = run_server(MockExecutor::new());
    let request = format!(
        "GET /?callback=cb&request=%7B%22jsonrpc%22%3A%222.0%22%2C%22id%22%3A1%2C%22method%22%3A%22check%22%2C%22params%22%3A%5B%5D%7D HTTP/1.1\r\nHost: localhost\r\n{ANDROID_HEADERS}\r\n"
    );
    let response = http_roundtrip(port, &request);
    assert_eq!(response.header("content-type"), Some("text/javascript"));
    assert!(response.body.starts_with("cb("), "{}", response.body);
    assert!(response.body.ends_with(')'));
}

#[test]
fn head_returns_headers_without_body() {
    let port = run_server(MockExecutor::new());
    let request = format!("HEAD / HTTP/1.1\r\nHost: localhost\r\n{ANDROID_HEADERS}\r\n");
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let response = read_response_head_only(&mut stream);
    assert_eq!(response.status_line, "HTTP/1.1 200 OK");
    assert_eq!(response.header("content-type"), Some("application/json"));
    assert!(response.header("content-length").is_some());
    assert!(response.body.is_empty());
}

#[test]
fn unsupported_method_is_405() {
    let port = run_server(MockExecutor::new());
    let response = http_roundtrip(port, &format!("PUT / HTTP/1.1\r\nHost: localhost\r\n{ANDROID_HEADERS}\r\n"));
    assert_eq!(response.status_line, "HTTP/1.1 405 Method Not Allowed");
}

/// Live HTTP test against a real PostgreSQL (`DATABASE_URL`): the full
/// service path (HTTP -> jsonrpc -> service -> DB) must answer a real grid
/// query with a valid JSON-RPC object.
#[test]
#[ignore = "requires a live PostgreSQL with the strikes schema (set DATABASE_URL)"]
fn live_http_grid_query() {
    use bo_service::config::Config;
    use bo_service::postgres::PostgresExecutor;

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

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let executor = rt
        .block_on(PostgresExecutor::connect(&config))
        .expect("connect");
    let executor: Arc<dyn QueryExecutor> = Arc::new(executor);
    let service: Arc<Service> = Arc::new(Service::new(executor));
    let listener = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    rt.spawn(async move {
        let _ = http::serve(listener, service).await;
    });

    let response = post(
        port,
        ANDROID_HEADERS,
        r#"{"jsonrpc":"2.0","id":1,"method":"get_strikes_grid","params":{"minute_length":60,"grid_base_length":10000,"region":1}}"#,
    );
    assert_eq!(response.status_line, "HTTP/1.1 200 OK");
    assert_eq!(response.header("content-type"), Some("application/json"));
    let v: serde_json::Value = serde_json::from_str(&response.body).unwrap();
    assert!(v.get("result").is_some(), "unexpected response: {v}");
}

#[test]
fn parse_error_still_returns_http_200_with_fault() {
    let port = run_server(MockExecutor::new());
    // Valid HTTP, invalid JSON body: a JSON-RPC fault with HTTP 200 (txjsonrpc
    // renders pre-1.0 faults as `faultCode`/`faultString`).
    let response = post(port, ANDROID_HEADERS, "{not json");
    assert_eq!(response.status_line, "HTTP/1.1 200 OK");
    let v: serde_json::Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(v["faultCode"], -32600);
}

/// A raw (still compressed) response, for the gzip tests.
struct RawResponse {
    status_line: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl RawResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

fn read_raw_response(stream: &mut TcpStream) -> RawResponse {
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
    }
    let header_text = String::from_utf8(header).unwrap();
    let mut lines = header_text.split("\r\n");
    let status_line = lines.next().unwrap().to_string();
    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    let content_length: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .map(|(_, v)| v.parse().unwrap())
        .unwrap_or(0);
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        stream.read_exact(&mut body).unwrap();
    }
    RawResponse {
        status_line,
        headers,
        body,
    }
}

/// POST and return the raw response bytes (no decompression).
fn post_raw(port: u16, headers: &str, body: &str) -> RawResponse {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let request = format!(
        "POST / HTTP/1.1\r\nHost: localhost\r\n{headers}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    read_raw_response(&mut stream)
}

/// A mock executor whose grid query returns a large raster so the rendered
/// response crosses the 1000 byte gzip threshold.
fn large_response_executor() -> MockExecutor {
    use bo_service::executor::{Row, Value};

    let mut mock = MockExecutor::new();
    let rows: Vec<Row> = (0..400)
        .map(|i| {
            Row::new(vec![
                Value::Int(i % 100),
                Value::Int(i % 50 + 1),
                Value::Int(3),
                Value::Timestamp(chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_000_000, 0).unwrap()),
            ])
        })
        .collect();
    mock.add_rows("TRUNC((ST_X", rows);
    mock.add_rows("-extract( epoch", vec![]);
    mock
}

const LARGE_GRID_BODY: &str =
    r#"{"jsonrpc":"2.0","id":1,"method":"get_strikes_grid","params":[60,10000,0,1,0]}"#;

#[test]
fn gzip_is_applied_for_large_responses_when_requested() {
    let port = run_server(large_response_executor());
    let identity_headers =
        "User-Agent: bo-android-190\r\nContent-Type: text/json\r\nAccept-Encoding: identity\r\n";
    let plain = post_raw(port, identity_headers, LARGE_GRID_BODY);
    assert_eq!(plain.status_line, "HTTP/1.1 200 OK");
    assert_eq!(plain.header("content-encoding"), None);
    assert!(plain.body.len() > bo_service::http::COMPRESSION_THRESHOLD);

    let gzip_headers =
        "User-Agent: bo-android-190\r\nContent-Type: text/json\r\nAccept-Encoding: gzip\r\n";
    let compressed = post_raw(port, gzip_headers, LARGE_GRID_BODY);
    assert_eq!(compressed.header("content-encoding"), Some("gzip"));

    // The gzip body decodes back to a valid JSON-RPC grid response.
    let mut decoder = flate2::read::GzDecoder::new(compressed.body.as_slice());
    let mut decoded = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut decoded).unwrap();
    let v: serde_json::Value = serde_json::from_str(&decoded).unwrap();
    assert!(v["result"].get("r").is_some(), "unexpected body: {decoded}");
}

#[test]
fn old_android_clients_never_receive_gzip() {
    let port = run_server(large_response_executor());
    let headers =
        "User-Agent: bo-android-177\r\nContent-Type: text/json\r\nAccept-Encoding: gzip\r\n";
    let response = post_raw(port, headers, LARGE_GRID_BODY);
    assert_eq!(response.header("content-encoding"), None);
    assert!(response.body.len() > bo_service::http::COMPRESSION_THRESHOLD);
}

#[test]
fn small_responses_are_not_gzipped_even_when_requested() {
    let port = run_server(large_response_executor());
    let headers =
        "User-Agent: bo-android-190\r\nContent-Type: text/json\r\nAccept-Encoding: gzip\r\n";
    let response = post_raw(
        port,
        headers,
        r#"{"jsonrpc":"2.0","id":1,"method":"check","params":[]}"#,
    );
    assert_eq!(response.header("content-encoding"), None);
    assert!(response.body.len() < bo_service::http::COMPRESSION_THRESHOLD);
}

/// Regression: a region grid request with `minute_length > 10` also runs the
/// histogram query whose envelope parameter is bound as bytea.  Before the
/// explicit `::bytea` cast this aborted the handler and the JSON-RPC result
/// was `null`.
#[test]
#[ignore = "requires a live PostgreSQL with the strikes schema (set DATABASE_URL)"]
fn live_region_grid_with_histogram_returns_result() {
    use bo_service::config::Config;
    use bo_service::postgres::PostgresExecutor;

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

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let executor = rt
        .block_on(PostgresExecutor::connect(&config))
        .expect("connect");
    let executor: Arc<dyn QueryExecutor> = Arc::new(executor);
    let service: Arc<Service> = Arc::new(Service::new(executor));
    let listener = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    rt.spawn(async move {
        let _ = http::serve(listener, service).await;
    });

    // minute_length 60 enables the histogram (which uses the failing envelope).
    let response = post(
        port,
        ANDROID_HEADERS,
        r#"{"jsonrpc":"2.0","id":1,"method":"get_strikes_grid","params":[60,10000,0,1,0]}"#,
    );
    let v: serde_json::Value = serde_json::from_str(&response.body).unwrap();
    let result = v.get("result").expect("unexpected response");
    assert!(!result.is_null(), "region histogram returned null: {}", response.body);
    assert!(result.get("h").is_some(), "missing histogram: {result}");
}