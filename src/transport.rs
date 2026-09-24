//! LSP-style `Content-Length` framing transport and the TCP server loop.
//!
//! The blitzortung.org JSON-RPC service is consumed over a raw TCP socket
//! using the same framing as the Language Server Protocol:
//!
//! ```text
//! Content-Length: <n>\r\n\r\n<json body of exactly n bytes>
//! ```
//!
//! Requests and responses are both framed this way; a single connection
//! carries a sequence of requests which are answered in order (no pipelined
//! out-of-order responses), mirroring the sequential per-connection handling
//! of the Twisted service.
//!
//! The frame headers may carry the HTTP-style headers the service layer reads
//! (`User-Agent`, `Content-Type`, `Referer`, `X-Forwarded-For`); they are
//! parsed into a [`Request`](crate::service::Request) per frame so the
//! validation rules of `base.py` are also applied to TCP connections.  A peer
//! without such headers is treated like a request with no headers at all:
//! unusable user agent -> blocked.

use std::io;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::metrics::Metrics;
use crate::service::{Request, Service};

/// Maximum accepted frame header block (headers end with a blank line).
const MAX_HEADER_BYTES: usize = 8192;
/// Maximum accepted JSON body (the strike data can be sizable).
const MAX_BODY_BYTES: usize = 256 * 1024 * 1024;

/// Parse the frame headers and return the declared `Content-Length`.
fn parse_content_length(headers: &str) -> Result<usize, String> {
    for line in headers.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                return value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| format!("invalid Content-Length: {value:?}"));
            }
        }
    }
    Err("missing Content-Length header".into())
}

/// Build the service request metadata from the frame headers (`getHeader`
/// analogues) and the peer address (`getClientIP`).
fn request_from_headers(headers: &str, client_ip: Option<String>) -> Request {
    let mut request = Request {
        client_ip,
        ..Default::default()
    };
    for line in headers.lines() {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim().to_string();
            match name.as_str() {
                "user-agent" => request.user_agent = Some(value),
                "x-forwarded-for" => request.x_forwarded_for = Some(value),
                "content-type" => request.content_type = Some(value),
                "referer" => request.referer = Some(value),
                _ => {}
            }
        }
    }
    request
}

/// Encode a response body as a framed message.
pub fn encode_frame(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 64);
    out.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
    out.extend_from_slice(body);
    out
}

/// Decode one complete frame from a byte buffer.
///
/// Returns `None` when more data is needed; `Some(Err(e))` on protocol
/// errors.
pub fn decode_frame(buf: &[u8]) -> Option<Result<Vec<u8>, String>> {
    let header_end = find_headers_end(buf)?;
    if header_end > MAX_HEADER_BYTES {
        return Some(Err("frame headers too large".into()));
    }
    let headers = std::str::from_utf8(&buf[..header_end]).map_err(|_| "headers not utf-8".to_string());
    let headers = match headers {
        Ok(h) => h,
        Err(e) => return Some(Err(e)),
    };
    let content_length = match parse_content_length(headers) {
        Ok(n) => n,
        Err(e) => return Some(Err(e)),
    };
    if content_length > MAX_BODY_BYTES {
        return Some(Err("frame body too large".into()));
    }
    let body_start = header_end + 4; // skip \r\n\r\n
    if buf.len() < body_start + content_length {
        return None;
    }
    Some(Ok(buf[body_start..body_start + content_length].to_vec()))
}

/// Locate the `\r\n\r\n` that terminates the headers.
fn find_headers_end(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 {
        return None;
    }
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
}

/// Read a framed request body from the stream; `None` on EOF between frames.
/// Returns the header block and the body separately so the service request
/// metadata can be built from the headers.
async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> io::Result<Option<(String, Vec<u8>)>> {
    let mut header_buf = Vec::with_capacity(128);
    // Read until the end of headers (\r\n\r\n).
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte).await? {
            0 => {
                if header_buf.is_empty() {
                    return Ok(None); // clean EOF between messages
                }
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "EOF in headers"));
            }
            _ => {
                header_buf.push(byte[0]);
                if header_buf.len() > MAX_HEADER_BYTES {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "headers too large"));
                }
                if header_buf.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
        }
    }

    let headers = std::str::from_utf8(&header_buf)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "headers not utf-8"))?
        .to_string();
    let content_length = parse_content_length(&headers)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if content_length > MAX_BODY_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "body too large"));
    }

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).await?;
    Ok(Some((headers, body)))
}

/// Handle a single connection: read frames and answer them in order.
async fn handle_connection<M: Metrics>(
    stream: TcpStream,
    service: Arc<Service<M>>,
) -> io::Result<()> {
    let peer_ip = stream.peer_addr().ok().map(|addr| addr.ip().to_string());
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    loop {
        let (headers, frame) = match read_frame(&mut reader).await? {
            Some(frame) => frame,
            None => return Ok(()), // client closed
        };

        let mut request = request_from_headers(&headers, peer_ip.clone());
        let body = String::from_utf8_lossy(&frame);
        let response = crate::jsonrpc::dispatch(&service, &mut request, &body);
        if let Some(response) = response {
            write_half.write_all(&encode_frame(response.as_bytes())).await?;
            write_half.flush().await?;
        }
    }
}

/// Accept connections on `listener` and serve each one sequentially.
///
/// The service object is shared across connections; a connection pool inside
/// the executor serializes actual database work as needed.
pub async fn serve<M: Metrics + 'static>(listener: TcpListener, service: Arc<Service<M>>) -> io::Result<()> {
    loop {
        let (stream, _peer) = listener.accept().await?;
        let service = service.clone();
        tokio::spawn(async move {
            let _ = handle_connection(stream, service).await;
        });
    }
}

/// Bind and serve on the given address; returns a handle so the caller can
/// keep the runtime alive.
pub async fn run<M: Metrics + 'static>(address: &str, service: Arc<Service<M>>) -> io::Result<()> {
    let listener = TcpListener::bind(address).await?;
    serve(listener, service).await
}

/// Synchronous decode for tests: split a byte buffer holding a request and
/// return the payload.
pub fn decode_frame_sync(buf: &[u8]) -> Option<Result<Vec<u8>, String>> {
    decode_frame(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_round_trip() {
        let body = br#"{"jsonrpc":"2.0","id":1,"result":[]}"#;
        let frame = encode_frame(body);
        let decoded = decode_frame(&frame).expect("complete frame");
        assert_eq!(decoded.unwrap(), body);
    }

    #[test]
    fn decode_requires_full_frame() {
        let frame = encode_frame(b"{}");
        // Cut off the last byte: not complete yet.
        assert!(decode_frame(&frame[..frame.len() - 1]).is_none());
    }

    #[test]
    fn decode_partial_frames_accumulate() {
        let frame = encode_frame(b"hello");
        // Only the header arrives first: not complete.
        let header_len = frame.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        assert!(decode_frame(&frame[..header_len + 2]).is_none());
        // Remaining bytes complete the body.
        assert!(decode_frame(&frame).is_some());
    }

    #[test]
    fn missing_content_length_is_error() {
        let buf = b"Foo: bar\r\n\r\n{}";
        let decoded = decode_frame(buf).expect("treated as complete for parse attempt");
        assert!(decoded.is_err());
    }

    #[test]
    fn request_from_headers_maps_service_fields() {
        let headers = "Content-Length: 2\r\nUser-Agent: bo-android-190\r\nContent-Type: text/json\r\n\
                       X-Forwarded-For: 203.0.113.7, 10.0.0.1\r\nReferer: http://spam.example\r\n\r\n";
        let request = request_from_headers(headers, Some("10.0.0.9".to_string()));
        assert_eq!(request.user_agent.as_deref(), Some("bo-android-190"));
        assert_eq!(request.content_type.as_deref(), Some("text/json"));
        assert_eq!(request.x_forwarded_for.as_deref(), Some("203.0.113.7, 10.0.0.1"));
        assert_eq!(request.referer.as_deref(), Some("http://spam.example"));
        assert_eq!(request.client_ip.as_deref(), Some("10.0.0.9"));
    }

    #[test]
    fn request_from_headers_without_headers_is_default() {
        let request = request_from_headers("Content-Length: 2\r\n\r\n", None);
        assert_eq!(request.user_agent, None);
        assert_eq!(request.content_type, None);
        assert_eq!(request.client_ip, None);
    }
}