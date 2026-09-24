//! Regression test for the "server freezes under load" report.
//!
//! History: `ObjectCache::get_result` once held a `std::sync::Mutex` across the
//! producer (the database query).  With a synchronous executor, every other
//! request missing the same key parked a runtime worker thread on
//! `Mutex::lock`, so once the waiters reached the worker-thread count the lock
//! holder could not proceed and the service froze until a restart.
//!
//! The service now awaits asynchronous queries and the cache stores the
//! **in-flight computation** for a key, so concurrent misses share one producer
//! instead of contending on a lock.  This test drives the real HTTP transport
//! on a multi-thread runtime with a cold cache and many concurrent clients; it
//! froze permanently before the fix.
//!
//! It also asserts the single-flight property: the producer runs a small,
//! bounded number of times (not once per client).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bo_service::executor::{Param, QueryExecutor, Row};
use bo_service::http;
use bo_service::service::Service;

/// Executor that simulates database latency asynchronously.  `delay` stands in
/// for a real query round-trip.
struct SlowExecutor {
    delay: Duration,
    queries: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl QueryExecutor for SlowExecutor {
    async fn query(
        &self,
        _sql: &str,
        _params: &[Param],
    ) -> Result<Vec<Row>, Box<dyn std::error::Error + Send + Sync>> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        // Simulate database latency: purely async now, no thread blocking.
        tokio::time::sleep(self.delay).await;
        Ok(Vec::new())
    }
}

fn server(executor: impl QueryExecutor + 'static) -> u16 {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let listener = rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
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
    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;

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
    stream
        .read_exact(&mut response)
        .map_err(|e| e.to_string())?;
    if !response.starts_with(b"{") && !response.starts_with(b"[") {
        return Err(format!(
            "unexpected body: {}",
            String::from_utf8_lossy(&response)
        ));
    }
    Ok(())
}

/// A cold-cache burst of concurrent data requests must be answered, and the
/// expensive grid+histogram producer must run only once (single-flight) rather
/// than once per client.
#[test]
fn cold_cache_burst_does_not_freeze_the_server() {
    let queries = Arc::new(AtomicUsize::new(0));
    let port = server(SlowExecutor {
        delay: Duration::from_millis(50),
        queries: queries.clone(),
    });

    // More clients than worker threads (4) so the old mutex waiters exhausted
    // the pool before the fix.
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
    // The grid producer (2 queries: grid + histogram) is coalesced across the
    // burst; allow a little slack for the histogram sub-producer.
    assert!(
        queries.load(Ordering::SeqCst) <= 4,
        "the producer should be coalesced, not run per client (ran {} times)",
        queries.load(Ordering::SeqCst)
    );
}
