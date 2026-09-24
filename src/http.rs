//! HTTP/1.1 transport for the JSON-RPC service.
//!
//! The deployed service sits behind an Nginx `proxy_pass` (e.g.
//! `http://127.0.0.1:7081/`), so it must speak real HTTP — not the LSP-style
//! `Content-Length` framing of [`crate::transport`].  This mirrors the Python
//! service, which runs `twisted.web.server.Site` around the
//! `txjsonrpc_ng.web.jsonrpc.JSONRPC` resource:
//!
//! * `POST /` (any path) with the JSON-RPC document as the request body,
//! * `GET /?request=<json>` (txjsonrpc falls back to the `request` query
//!   parameter when the body is empty),
//! * a JSONP `?callback=<name>` wraps the body as `name(<json>)` and sets
//!   `Content-Type: text/javascript`,
//! * otherwise `Content-Type: application/json`,
//! * HTTP 200 with the JSON-RPC response body.
//!
//! Persistent connections are supported (`Connection: keep-alive`, the HTTP/1.1
//! default); `Connection: close` (and HTTP/1.0 without keep-alive) closes the
//! connection after the response.  `HEAD` answers with the headers only;
//! unsupported methods get `405 Method Not Allowed`.

use std::io;
use std::io::Write;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::metrics::Metrics;
use crate::service::Service;
use crate::transport::{log_access, request_from_headers};

/// Maximum accepted header block (bytes before `\r\n\r\n`).
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// Maximum accepted request body (the strike data can be sizable).
const MAX_BODY_BYTES: usize = 256 * 1024 * 1024;
/// Below this uncompressed size the service skips gzip
/// (`txjsonrpc_ng.web.render.Renderer.handle_compression`).
pub const COMPRESSION_THRESHOLD: usize = 1000;

/// A parsed HTTP request head plus its body.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub version: String,
    /// Raw header block (without the terminating blank line).
    pub headers: String,
    /// The request target's query string (without the leading `?`), if any.
    pub query: String,
    pub body: Vec<u8>,
}

/// A failure while reading/parsing an HTTP request.
enum HttpError {
    /// The connection closed cleanly between requests.
    Eof,
    /// A protocol error; the peer is answered with `400`/`413` where possible.
    Bad(&'static str),
    /// I/O error.
    Io(io::Error),
}

impl From<io::Error> for HttpError {
    fn from(error: io::Error) -> Self {
        HttpError::Io(error)
    }
}

/// Read one HTTP request from `reader`.
async fn read_request<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<HttpRequest, HttpError> {
    // Request line.
    let mut request_line = String::new();
    let n = reader.read_line(&mut request_line).await?;
    if n == 0 {
        return Err(HttpError::Eof);
    }
    let mut parts = request_line.trim_end().splitn(3, ' ');
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();
    if method.is_empty() || target.is_empty() {
        return Err(HttpError::Bad("malformed request line"));
    }

    // Header block up to the blank line.
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(HttpError::Bad("EOF in headers"));
        }
        if headers.len() + line.len() > MAX_HEADER_BYTES {
            return Err(HttpError::Bad("headers too large"));
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        headers.push_str(&line);
    }

    // Split the query string off the target.
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (target.clone(), String::new()),
    };

    // Body (only when Content-Length is present and non-zero).
    let content_length = header_value(&headers, "content-length")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        return Err(HttpError::Bad("body too large"));
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).await?;
    }

    Ok(HttpRequest {
        method,
        path,
        version,
        headers,
        query,
        body,
    })
}

/// Case-insensitive header lookup (first match) in a raw header block.
fn header_value(headers: &str, name: &str) -> Option<String> {
    for line in headers.lines() {
        if let Some((key, value)) = line.split_once(':') {
            if key.trim().eq_ignore_ascii_case(name) {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

/// Percent-decode a `application/x-www-form-urlencoded` value (`+` -> space,
/// `%XX` -> byte).  Used for the `?request=` / `?callback=` query parameters.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Extract a query parameter value (`?a=1&b=2`).
fn query_param(query: &str, name: &str) -> Option<String> {
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            if key == name {
                return Some(percent_decode(value));
            }
        } else if pair == name {
            return Some(String::new());
        }
    }
    None
}

/// Whether the connection should be kept alive after this response.
fn wants_keep_alive(req: &HttpRequest) -> bool {
    let connection = header_value(&req.headers, "connection").map(|v| v.to_ascii_lowercase());
    match connection.as_deref() {
        Some("close") => false,
        Some("keep-alive") => true,
        // HTTP/1.0 defaults to close, HTTP/1.1 to keep-alive.
        _ => !req.version.eq_ignore_ascii_case("HTTP/1.0"),
    }
}

/// Gzip-compress a response body (`txjsonrpc_ng.web.render` uses
/// `gzip.GzipFile`, i.e. the gzip container, not raw deflate).
fn gzip_encode(body: &[u8]) -> Option<Vec<u8>> {
    use flate2::write::GzEncoder;
    use flate2::Compression;

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(body).ok()?;
    encoder.finish().ok()
}

/// Apply the response compression policy and return `(content_encoding, body)`:
/// gzip when the client advertised it (and was not downgraded by
/// `fix_bad_accept_header`) and the body is at least
/// [`COMPRESSION_THRESHOLD`] bytes, identity otherwise.
pub(crate) fn encode_body(mut payload: Vec<u8>, accepts_gzip: bool) -> (Option<&'static str>, Vec<u8>) {
    if accepts_gzip && payload.len() >= COMPRESSION_THRESHOLD {
        if let Some(compressed) = gzip_encode(&payload) {
            payload = compressed;
            return (Some("gzip"), payload);
        }
    }
    (None, payload)
}

/// Build the HTTP response bytes for a JSON-RPC body.
fn build_response(
    status: &str,
    content_type: &str,
    body: &[u8],
    content_encoding: Option<&str>,
    keep_alive: bool,
    include_body: bool,
) -> Vec<u8> {
    let connection = if keep_alive { "keep-alive" } else { "close" };
    let encoding_header = content_encoding
        .map(|encoding| format!("Content-Encoding: {encoding}\r\n"))
        .unwrap_or_default();
    let mut out = Vec::with_capacity(body.len() + 128);
    out.extend_from_slice(
        format!(
            "HTTP/1.1 {status}\r\n\
             Content-Type: {content_type}\r\n\
             {encoding_header}\
             Content-Length: {}\r\n\
             Connection: {connection}\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    );
    if include_body {
        out.extend_from_slice(body);
    }
    out
}

/// Handle a single HTTP connection: read requests and answer them in order,
/// honouring keep-alive.
async fn handle_connection<M: Metrics>(stream: TcpStream, service: Arc<Service<M>>) -> io::Result<()> {
    let peer_ip = stream.peer_addr().ok().map(|addr| addr.ip().to_string());
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    loop {
        let request = match read_request(&mut reader).await {
            Ok(request) => request,
            Err(HttpError::Eof) => return Ok(()),
            Err(HttpError::Io(error)) => return Err(error),
            Err(HttpError::Bad(message)) => {
                let body = format!("{{\"error\":\"{message}\"}}");
                let _ = write_half
                    .write_all(&build_response(
                        "400 Bad Request",
                        "application/json",
                        body.as_bytes(),
                        None,
                        false,
                        true,
                    ))
                    .await;
                let _ = write_half.flush().await;
                return Ok(());
            }
        };

        let keep_alive = wants_keep_alive(&request);

        // Only POST and GET are supported (`POST /` is the deployed path).
        if request.method != "POST" && request.method != "GET" && request.method != "HEAD" {
            let _ = write_half
                .write_all(&build_response(
                    "405 Method Not Allowed",
                    "application/json",
                    b"{\"error\":\"method not allowed\"}",
                    None,
                    keep_alive,
                    request.method != "HEAD",
                ))
                .await;
            let _ = write_half.flush().await;
            if !keep_alive {
                return Ok(());
            }
            continue;
        }

        // txjsonrpc: the body is read first; a GET with an empty body falls
        // back to the `request` query parameter.
        let content = if !request.body.is_empty() {
            String::from_utf8_lossy(&request.body).into_owned()
        } else {
            query_param(&request.query, "request").unwrap_or_default()
        };
        let callback = query_param(&request.query, "callback");

        let mut service_request = request_from_headers(&request.headers, peer_ip.clone());

        let started = std::time::Instant::now();
        let result = crate::jsonrpc::dispatch(&service, &mut service_request, &content).await;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        log_access(&service_request, &result.meta, elapsed_ms);

        let raw_body = result.response.unwrap_or_default();
        // JSONP wrapping (`?callback=`), matching txjsonrpc `_render_text`.
        let (content_type, payload) = match &callback {
            Some(callback) if !callback.is_empty() => (
                "text/javascript",
                format!("{callback}({raw_body})").into_bytes(),
            ),
            _ => ("application/json", raw_body.into_bytes()),
        };

        // Compression is decided after dispatch so the handler's
        // `fix_bad_accept_header` (old Android clients) is already applied.
        let (content_encoding, payload) = encode_body(payload, service_request.accepts_gzip());

        if request.method == "HEAD" {
            // Headers only, but advertise the real body length.
            write_half
                .write_all(&build_response(
                    "200 OK",
                    content_type,
                    &payload,
                    content_encoding,
                    keep_alive,
                    false,
                ))
                .await?;
        } else {
            write_half
                .write_all(&build_response(
                    "200 OK",
                    content_type,
                    &payload,
                    content_encoding,
                    keep_alive,
                    true,
                ))
                .await?;
        }
        write_half.flush().await?;

        if !keep_alive {
            return Ok(());
        }
        // `path` is currently unused beyond documentation; keep the field for
        // parity with the deployed `POST /` and `GET /?request=` forms.
        let _ = &request.path;
    }
}

/// Accept connections on `listener` and serve each one.
pub async fn serve<M: Metrics + 'static>(listener: TcpListener, service: Arc<Service<M>>) -> io::Result<()> {
    loop {
        let (stream, _peer) = listener.accept().await?;
        let service = service.clone();
        tokio::spawn(async move {
            let _ = handle_connection(stream, service).await;
        });
    }
}

/// Bind and serve HTTP on the given address.
pub async fn run<M: Metrics + 'static>(address: &str, service: Arc<Service<M>>) -> io::Result<()> {
    let listener = TcpListener::bind(address).await?;
    serve(listener, service).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_percent_encoded_query() {
        assert_eq!(percent_decode("a%20b+c"), "a b c");
        assert_eq!(percent_decode("no-encoding"), "no-encoding");
        assert_eq!(percent_decode("%7B%22x%22%3A1%7D"), "{\"x\":1}");
    }

    #[test]
    fn extracts_query_parameters() {
        let q = "request=%7B%7D&callback=cb";
        assert_eq!(query_param(q, "request").as_deref(), Some("{}"));
        assert_eq!(query_param(q, "callback").as_deref(), Some("cb"));
        assert_eq!(query_param(q, "missing"), None);
    }

    #[test]
    fn keep_alive_follows_http_version_and_connection_header() {
        let base = |version: &str, connection: Option<&str>| HttpRequest {
            method: "POST".into(),
            path: "/".into(),
            version: version.into(),
            headers: connection
                .map(|c| format!("Connection: {c}\r\n"))
                .unwrap_or_default(),
            query: String::new(),
            body: Vec::new(),
        };
        assert!(wants_keep_alive(&base("HTTP/1.1", None)));
        assert!(!wants_keep_alive(&base("HTTP/1.0", None)));
        assert!(!wants_keep_alive(&base("HTTP/1.1", Some("close"))));
        assert!(wants_keep_alive(&base("HTTP/1.0", Some("keep-alive"))));
    }

    #[test]
    fn response_has_valid_http_status_and_headers() {
        let body = br#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let response = build_response("200 OK", "application/json", body, None, true, true);
        let text = String::from_utf8(response).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
        assert!(text.contains("Content-Type: application/json\r\n"));
        assert!(!text.contains("Content-Encoding"));
        assert!(text.contains(&format!("Content-Length: {}\r\n", body.len())));
        assert!(text.contains("Connection: keep-alive\r\n"));
        assert!(text.ends_with("result\":null}"));
    }

    #[test]
    fn head_response_omits_body_but_keeps_length() {
        let body = b"12345";
        let response = build_response("200 OK", "application/json", body, None, true, false);
        let text = String::from_utf8(response).unwrap();
        assert!(text.contains("Content-Length: 5\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
    }

    /// gzip is applied only when requested and the body is large enough
    /// (`Renderer.handle_compression`).
    #[test]
    fn encode_body_applies_gzip_only_when_requested_and_large() {
        let large = vec![b'x'; COMPRESSION_THRESHOLD];
        let (encoding, body) = encode_body(large.clone(), true);
        assert_eq!(encoding, Some("gzip"));
        assert!(body.len() < large.len());

        // Small bodies stay uncompressed even when gzip is accepted.
        let (encoding, body) = encode_body(b"{}".to_vec(), true);
        assert_eq!(encoding, None);
        assert_eq!(body, b"{}");

        // Clients that did not advertise gzip never get it.
        let (encoding, body) = encode_body(large.clone(), false);
        assert_eq!(encoding, None);
        assert_eq!(body, large);
    }

    /// The compressed payload must round-trip back to the original bytes.
    #[test]
    fn gzip_payload_round_trips() {
        let large = vec![b'a'; 4096];
        let (encoding, compressed) = encode_body(large.clone(), true);
        assert_eq!(encoding, Some("gzip"));

        let mut decoder = flate2::read::GzDecoder::new(compressed.as_slice());
        let mut decoded = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut decoded).unwrap();
        assert_eq!(decoded, large);
    }

    /// `Content-Encoding: gzip` is advertised when the body is compressed.
    #[test]
    fn response_declares_content_encoding_when_compressed() {
        let body = b"compressed";
        let response = build_response("200 OK", "application/json", body, Some("gzip"), true, true);
        let text = String::from_utf8(response).unwrap();
        assert!(text.contains("Content-Encoding: gzip\r\n"), "{text}");
        assert!(text.contains(&format!("Content-Length: {}\r\n", body.len())));
    }
}