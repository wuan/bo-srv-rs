//! End-to-end test: spin up the TCP server with a mock executor and talk to
//! it using LSP-style framing, verifying the JSON-RPC dispatch path finds its
//! way through `transport` -> `jsonrpc` -> `service`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use bo_service::executor::{QueryExecutor, Row, Value};
use bo_service::mock::MockExecutor;
use bo_service::service::Service;
use bo_service::transport;

fn run_server(eq: impl QueryExecutor + 'static) -> u16 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let listener = rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let eq: Arc<dyn QueryExecutor> = Arc::new(eq);
    let service: Arc<Service> = Arc::new(Service::new(eq));
    std::thread::spawn(move || {
        rt.block_on(transport::serve(listener, service)).unwrap();
    });
    addr.port()
}

/// Frame headers a real Android client would send; without these the service
/// blocks every data request (the `User-Agent` rule of `base.py`).
fn client_headers() -> &'static str {
    "User-Agent: bo-android-190\r\nContent-Type: text/json\r\n"
}

fn rpc(port: u16, headers: &str, body: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let framed = format!("Content-Length: {}\r\n{headers}\r\n{body}", body.len());
    stream.write_all(framed.as_bytes()).unwrap();

    // read the response headers
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

    let mut body_vec = vec![0u8; content_length];
    stream.read_exact(&mut body_vec).unwrap();
    String::from_utf8(body_vec).unwrap()
}

#[test]
fn check_over_tcp() {
    let mock = MockExecutor::new();
    let port = run_server(mock);
    // legacy protocol: id 0, no version -> bare array
    let response = rpc(port, "", r#"{"method":"check","params":[],"id":0}"#);
    let v: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert!(v.is_array(), "expected bare array, got {v}");
    assert_eq!(v[0]["count"], 1);
}

#[test]
fn check_over_tcp_v2() {
    let mock = MockExecutor::new();
    let port = run_server(mock);
    let response = rpc(
        port,
        "",
        r#"{"jsonrpc":"2.0","id":9,"method":"check","params":[]}"#,
    );
    let v: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(v["id"], 9);
    assert_eq!(v["jsonrpc"], "2.0");
    assert_eq!(v["result"]["count"], 1);
}

#[test]
fn get_strikes_grid_over_tcp_with_headers() {
    let mut mock = MockExecutor::new();
    // region grid query + histogram query (minute_length 30 > 10)
    mock.add_rows(
        "TRUNC((ST_X",
        vec![Row::new(vec![
            Value::Int(0),
            Value::Int(1),
            Value::Int(4),
            Value::Timestamp(chrono::DateTime::from_timestamp(1_699_999_500, 0).unwrap()),
        ])],
    );
    mock.add_rows(
        "-extract( epoch",
        vec![
            Row::new(vec![Value::Int(-2), Value::Int(1)]),
            Row::new(vec![Value::Int(0), Value::Int(7)]),
        ],
    );
    let port = run_server(mock);
    let response = rpc(
        port,
        client_headers(),
        r#"{"jsonrpc":"2.0","id":7,"method":"get_strikes_grid","params":[30,10000,0,1,0]}"#,
    );
    let v: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(v["id"], 7);
    let obj = v["result"].as_object().expect("grid response object");
    // the ten grid keys (region shape)
    for key in ["r", "xd", "yd", "x0", "y1", "xc", "yc", "t", "dt", "h"] {
        assert!(obj.contains_key(key), "missing key {key}");
    }
    // the region row was within range and is present (the age depends on the
    // test-run clock; only the row count and shape are asserted)
    let rows = obj["r"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], serde_json::json!(0));
    assert_eq!(rows[0][2], serde_json::json!(4));
    // histogram is attached because minute_length > 10; 30 minutes / 5 =
    // 6 bins, interval -2 -> index 3, interval 0 -> index 5
    assert_eq!(obj["h"], serde_json::json!([0, 0, 0, 1, 0, 7]));
    // 30-minute interval -> 1800 seconds
    assert_eq!(obj["dt"].as_i64(), Some(1800));
}

#[test]
fn data_request_without_headers_is_blocked() {
    let mock = MockExecutor::new();
    let port = run_server(mock);
    // No User-Agent / Content-Type headers -> blocked -> {}
    let response = rpc(
        port,
        "",
        r#"{"jsonrpc":"2.0","id":5,"method":"get_strikes_grid","params":[60,10000,0,1,0]}"#,
    );
    let v: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(v["result"], serde_json::json!({}));
}

#[test]
fn method_not_found_over_tcp() {
    let mock = MockExecutor::new();
    let port = run_server(mock);
    let response = rpc(port, "", r#"{"jsonrpc":"2.0","id":1,"method":"bogus"}"#);
    let v: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(v["error"]["code"], bo_service::jsonrpc::METHOD_NOT_FOUND);
    assert_eq!(v["id"], 1);
}

#[test]
fn batch_is_rejected_as_invalid_request() {
    let mock = MockExecutor::new();
    let port = run_server(mock);
    // txjsonrpc rejects non-object bodies with an invalid-request fault
    let response = rpc(
        port,
        "",
        r#"[{"jsonrpc":"2.0","id":1,"method":"bogus"},{"jsonrpc":"2.0","id":2,"method":"check","params":[]}]"#,
    );
    let v: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(v["faultCode"], bo_service::jsonrpc::INVALID_REQUEST);
    assert_eq!(v["fault"], "Fault");
}

#[test]
fn notification_still_gets_a_response() {
    let mock = MockExecutor::new();
    let port = run_server(mock);
    // txjsonrpc answers every request; id-less requests use the pre-1.0
    // dialect: a bare array.
    let response = rpc(port, "", r#"{"method":"check","params":[]}"#);
    let v: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert!(v.is_array());
    assert_eq!(v[0]["count"], 1);
}
