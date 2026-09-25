//! `bo-import-websocket` implementation (port of
//! `blitzortung/cli/imprt_websocket.py`).
//!
//! Connects to a Blitzortung live websocket, decodes and stores incoming
//! strikes, and reconnects automatically when the socket closes
//! (`run_forever` semantics).

use std::sync::Arc;

use clap::Parser;
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use crate::builder::Strike as StrikeBuilder;
use crate::db::StrikeDb;
use crate::executor::QueryExecutor;
use crate::metrics::Metrics;
use crate::websocket::decode;

/// Websocket server indices chosen from at random (`imprt_websocket.main`).
pub const SERVER_INDICES: &[u32] = &[1, 7, 8];
/// `Origin` header sent when connecting.
pub const ORIGIN: &str = "https://www.blitzortung.org";
/// Initialization message sent on open.
pub const INITIALIZATION_MESSAGE: &str = "{\"a\":111}";
/// Keepalive message sent every 30 seconds.
pub const KEEPALIVE_INTERVAL_SECONDS: u64 = 30;
/// Commit after this many strikes.
pub const COMMIT_STRIKE_COUNT: u64 = 100;
/// Commit after this many seconds.
pub const COMMIT_INTERVAL_SECONDS: u64 = 5;

/// `bo-import-websocket` command-line options (port of
/// `cli/imprt_websocket.py`).
#[derive(Parser, Debug, Clone)]
#[command(
    name = "bo-import-websocket",
    about = "Live strike import over the Blitzortung websocket",
    version
)]
pub struct WebsocketArgs {
    /// verbose output
    #[arg(short, long)]
    pub verbose: bool,

    /// debug output
    #[arg(short, long)]
    pub debug: bool,

    /// test connection only
    #[arg(short, long)]
    pub test: bool,
}

/// Parse a decoded websocket JSON message into a strike, using its `region`
/// field.  Returns `(strike, region, delay)`; `None` when the message is not a
/// strike.  `delay` is the server-reported delay in seconds.
pub fn strike_from_message(message: &str) -> Option<(crate::data::Strike, i64, f64)> {
    let value: serde_json::Value = serde_json::from_str(message).ok()?;
    let mut builder = StrikeBuilder::new();
    let strike = builder.from_json(&value).ok()?.build().ok()?;
    let region = value.get("region").and_then(|v| v.as_i64()).unwrap_or(1);
    let delay = value.get("delay").and_then(|v| v.as_f64()).unwrap_or(0.0);
    Some((strike, region, delay))
}

/// The `bo-import-websocket` options.
pub struct WebsocketOptions {
    pub verbose: bool,
    pub debug: bool,
    pub test: bool,
}

impl WebsocketOptions {
    pub fn from_args(args: &WebsocketArgs) -> Self {
        WebsocketOptions {
            verbose: args.verbose,
            debug: args.debug,
            test: args.test,
        }
    }
}

/// Pick a random websocket server index from [`SERVER_INDICES`].
pub fn random_server_index() -> u32 {
    use rand::seq::IndexedRandom;
    let mut rng = rand::rng();
    *SERVER_INDICES.choose(&mut rng).unwrap_or(&1)
}

/// The websocket URL for a server index.
pub fn server_url(index: u32) -> String {
    format!("wss://ws{index}.blitzortung.org/")
}

/// Mutable state shared across websocket messages.
struct Importer<'a> {
    db: Option<&'a StrikeDb<'a>>,
    executor: Option<&'a dyn QueryExecutor>,
    metrics: &'a dyn Metrics,
    strike_count: u64,
    last_commit: std::time::Instant,
    local_delay_sum: f64,
}

impl<'a> Importer<'a> {
    fn new(
        db: Option<&'a StrikeDb<'a>>,
        executor: Option<&'a dyn QueryExecutor>,
        metrics: &'a dyn Metrics,
    ) -> Self {
        Importer {
            db,
            executor,
            metrics,
            strike_count: 0,
            last_commit: std::time::Instant::now(),
            local_delay_sum: 0.0,
        }
    }

    /// Handle a single (already decoded) websocket payload.
    async fn on_message(
        &mut self,
        payload: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let message = decode(payload);
        log::debug!("message: {message}");

        let (strike, region, delay) = match strike_from_message(&message) {
            Some(parsed) => parsed,
            None => return Ok(()),
        };
        log::debug!("strike: {strike}");

        let local_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let strike_time = strike
            .timestamp
            .datetime
            .map(|dt| dt.timestamp() as f64 + dt.timestamp_subsec_micros() as f64 / 1e6)
            .unwrap_or(0.0);
        let local_delay = local_time - strike_time;
        self.local_delay_sum += local_delay;

        log::info!("{strike} - region {region} - delay {delay:.1}, local delay {local_delay:.1}");

        // `imprt_websocket.on_message`: count the strike and gauge its delay.
        self.metrics.for_websocket_strike(local_delay);

        if let Some(db) = self.db {
            db.insert(&strike, region).await?;
        }
        self.strike_count += 1;

        if self.strike_count > COMMIT_STRIKE_COUNT
            || (self.strike_count > 0
                && self.last_commit.elapsed().as_secs() > COMMIT_INTERVAL_SECONDS)
        {
            log::info!("commit #{}", self.strike_count);
            if let Some(executor) = self.executor {
                executor.commit().await?;
            }
            self.strike_count = 0;
            self.last_commit = std::time::Instant::now();
        }
        Ok(())
    }
}

/// Connect once and run until the socket closes.
///
/// `test` mode connects and drains messages without touching the database.
async fn run_once(
    index: u32,
    test: bool,
    executor: Option<Arc<dyn QueryExecutor>>,
    metrics: &dyn Metrics,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = server_url(index);
    log::info!("connect to {url}");

    let mut request = url.into_client_request()?;
    request
        .headers_mut()
        .insert("Origin", ORIGIN.parse().expect("valid origin header"));

    let (stream, _response) = tokio_tungstenite::connect_async(request).await?;
    let (mut write, mut read) = stream.split();

    // on_open: send the initialization message and start the keepalive.
    log::info!("{INITIALIZATION_MESSAGE}");
    write
        .send(Message::Text(INITIALIZATION_MESSAGE.to_string()))
        .await?;

    // Keepalive: send `{}` every 30s (dropped when the socket closes).
    let keepalive_write = write;
    tokio::spawn(async move {
        let mut write = keepalive_write;
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(KEEPALIVE_INTERVAL_SECONDS));
        // Skip the immediate first tick, matching the `sleep(30)` loop.
        interval.tick().await;
        loop {
            interval.tick().await;
            if write.send(Message::Text("{}".to_string())).await.is_err() {
                log::info!("refresher exiting");
                return;
            }
            log::info!("sent refresh");
        }
    });

    let db = executor
        .as_ref()
        .map(|_| StrikeDb::new(executor.as_deref().unwrap(), 4326));
    let db = db.as_ref();
    let mut importer = Importer::new(db, executor.as_deref(), metrics);

    while let Some(message) = read.next().await {
        match message {
            Ok(Message::Text(payload)) => {
                if !test {
                    importer.on_message(&payload).await?;
                }
            }
            Ok(Message::Binary(payload)) => {
                let text = String::from_utf8_lossy(&payload);
                if !test {
                    importer.on_message(&text).await?;
                }
            }
            Ok(Message::Ping(payload)) => {
                // Reply pong; the split writer is owned by the keepalive task,
                // so this is handled by the underlying stream on close.
                let _ = payload;
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(error) => {
                log::warn!("error '{error}'");
                break;
            }
        }
    }

    log::info!("finished");
    Ok(())
}

/// Entry point: connect repeatedly (with auto-reconnect) until interrupted.
pub async fn run(
    executor: Option<Arc<dyn QueryExecutor>>,
    options: &WebsocketOptions,
    metrics: &dyn Metrics,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    loop {
        let index = random_server_index();
        if let Err(error) = run_once(index, options.test, executor.clone(), metrics).await {
            log::warn!("connection error: {error}");
        }
        // Brief pause before reconnecting, matching the outer `while True`.
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_url_format() {
        assert_eq!(server_url(7), "wss://ws7.blitzortung.org/");
    }

    #[test]
    fn random_server_index_is_known() {
        for _ in 0..20 {
            assert!(SERVER_INDICES.contains(&random_server_index()));
        }
    }

    #[test]
    fn strike_from_message_parses_region_and_delay() {
        let message = "{\"time\":1650135893612088000,\"lat\":32.335748,\"lon\":-89.561516,\
                       \"alt\":0,\"pol\":0,\"mds\":9611,\"mcg\":179,\"status\":0,\"region\":3,\"delay\":4.9}";
        let (strike, region, delay) = strike_from_message(message).unwrap();
        assert_eq!(strike.x, -89.5615);
        assert_eq!(strike.y, 32.3357);
        assert_eq!(region, 3);
        assert_eq!(delay, 4.9);
    }

    #[test]
    fn strike_from_message_defaults_region_to_one() {
        let message = "{\"time\":1650135893612088000,\"lat\":32.335748,\"lon\":-89.561516,\"alt\":0,\"mds\":100}";
        let (_, region, _) = strike_from_message(message).unwrap();
        assert_eq!(region, 1);
    }

    #[test]
    fn strike_from_message_rejects_invalid_json() {
        assert!(strike_from_message("not json").is_none());
        assert!(strike_from_message("{\"lat\":1.0}").is_none());
    }

    #[tokio::test]
    async fn importer_commits_on_test_mode_without_db() {
        // Without a database the importer still counts and commits are no-ops.
        let metrics = crate::metrics::RecordingMetrics::new();
        let mut importer = Importer::new(None, None, &metrics);
        let message = "{\"time\":1650135893612088000,\"lat\":32.335748,\"lon\":-89.561516,\
                       \"alt\":0,\"mds\":100,\"region\":3}";
        importer.on_message(message).await.unwrap();
        assert_eq!(importer.strike_count, 1);
        // `strikes` counter plus the `strikes.delay` float gauge.
        let lines = metrics.lines();
        assert_eq!(lines[0], "strikes:1|c");
        assert!(lines[1].starts_with("strikes.delay:"), "got {lines:?}");
        assert!(lines[1].ends_with("|g"), "got {lines:?}");
    }
}
