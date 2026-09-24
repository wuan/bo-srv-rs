//! Regression test for the "server freezes under load" report.
//!
//! Root cause: `ObjectCache::get_result` held a `std::sync::Mutex` across the
//! producer (the database query).  The producers block their calling thread
//! (`PostgresExecutor` uses `block_in_place` + `block_on`), and every other
//! request missing the same key parked a runtime worker thread on
//! `Mutex::lock`.  Tokio compensates for the one thread inside `block_in_place`
//! but cannot compensate for threads parked in a synchronous mutex wait, so
//! once the number of waiters reached the worker-thread count every worker was
//! consumed, the lock holder could not proceed, and the service froze until a
//! restart.  A cold-cache burst (many clients hitting the same key at once)
//! triggered it immediately.
//!
//! The fix computes the payload without holding the cache lock.  This test
//! drives the real HTTP transport on a multi-thread runtime (the production
//! configuration) with a cold cache and many concurrent clients; it froze
//! permanently before the fix.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bo_service::executor::{Param, QueryExecutor, Row};
use bo_service::http;
use bo_service::service::Service;

/// Executor that reproduces `PostgresExecutor`'s blocking pattern: it awaits a
/// timer on the serving runtime via `block_in_place` + `Handle::block_on`.  The
/// delay simulates database latency.
struct BlockingExecutor {
    delay: Duration,
    queries: Arc<AtomicUsize>,
}

impl QueryExecutor for BlockingExecutor {
    fn query(
        &self,
        _sql: &str,
        _params: &[Param],
    ) -> Result<Vec<Row>, Box<dyn std::error::Error + Send + Sync>> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        let delay = self.delay;
        let handle = tokio::runtime::Handle::current();
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| {
                handle.block_on(async { tokio::time::sleep(delay).await });
            });
        } else {
            handle.block_on(async { tokio::time::sleep(delay).await });
        }
        Ok(Vec::new())
    }
}

fn server(executor: impl QueryExecutor + 'static) -> u16 {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let listener = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    let executor: Arc<dyn QueryExecutor> = Arc::new(executor);
    let service: Arc<Service> = Arc::new(Service::new(executor));
    std::thread::spawn(move || {
        rt.block_on(http::serve(listener, service)).unwrap();
    });
    port
}

/// One `POST /` request with the headers a real Android client sends; returns
/// once the response body has been read.
fn post(port: u16, id: usize) -> Result<(), String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .map_err(|e| e.to_string())?;
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"get_strikes_grid","params":[30,10000,0,1,0]}}"#
    );
    let request = format!(
        "POST / HTTP/1.1\r\nHost: localhost\r\nUser-Agent: bo-android-190\r\n\
         Content-Type: text/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).map_err(|e| e.to_string())?;

    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        stream.read_exact(&mut byte).map_err(|e| e.to_string())?;
        header.push(byte[0]);
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let headers = String::from_utf8(header).map_err(|e| e.to_string())?;
    let content_length: usize = headers
        .lines()
        .find_map(|l| {
            l.split_once(':')
                .filter(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
                .map(|(_, v)| v.trim().parse().unwrap())
        })
        .unwrap_or(0);
    let mut response = vec![0u8; content_length];
    stream.read_exact(&mut response).map_err(|e| e.to_string())?;
    if !response.starts_with(b"{") && !response.starts_with(b"[") {
        return Err(format!("unexpected body: {}", String::from_utf8_lossy(&response)));
    }
    Ok(())
}

/// A cold-cache burst of concurrent data requests must be answered.  Before the
/// fix the server froze permanently (all clients timed out, only the first
/// query ever ran).
#[test]
fn cold_cache_burst_does_not_freeze_the_server() {
    let queries = Arc::new(AtomicUsize::new(0));
    let port = server(BlockingExecutor {
        delay: Duration::from_millis(50),
        queries: queries.clone(),
    });

    // More clients than worker threads (4) so the mutex waiters exhausted the
    // pool before the fix.
    let clients = 48usize;
    let started = Instant::now();
    let mut handles = Vec::new();
    for id in 0..clients {
        handles.push(std::thread::spawn(move || {
            post(port, id + 1).map_err(|error| (id, error))
        }));
    }

    let mut failures = Vec::new();
    for handle in handles {
        if let Err(failure) = handle.join().unwrap() {
            failures.push(failure);
        }
    }
    let elapsed = started.elapsed();

    assert!(failures.is_empty(), "some requests failed: {failures:?}");
    assert!(
        elapsed < Duration::from_secs(15),
        "burst took too long (possible freeze): {elapsed:?}"
    );
    assert!(
        queries.load(Ordering::SeqCst) >= 2,
        "the grid producer must have run its queries"
    );
}