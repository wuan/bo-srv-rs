//! `bo-insert-websocket` — live strike import over the Blitzortung websocket
//! (port of `blitzortung/cli/imprt_websocket.py`).
//!
//! ```text
//! bo-insert-websocket [-v] [-d] [-t]
//! ```
//!
//! Connects to `wss://ws{1|7|8}.blitzortung.org/`, decodes each message and
//! inserts the strikes, reconnecting automatically when the socket closes.
//! `-t/--test` connects without writing to the database.

use std::sync::Arc;

use bo_service::cli::{exit_with, init_logging, websocket_tool, LockWithTimeout, Options};
use bo_service::config::Config;
use bo_service::executor::QueryExecutor;
use bo_service::postgres::PostgresExecutor;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = Options::parse(&args, websocket_tool::SPECS);
    let ws_options = websocket_tool::WebsocketOptions::from_options(&options);
    init_logging(ws_options.verbose, ws_options.debug);

    let mut lock = LockWithTimeout::new("/tmp/.bo-import-websocket.lock");
    if let Err(error) = lock.lock(10) {
        log::warn!("could not acquire lock: {error}");
        std::process::exit(1);
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => exit_with(&format!("failed to build runtime: {error}"), 1),
    };

    let executor: Option<Arc<dyn QueryExecutor>> = if ws_options.test {
        None
    } else {
        let config = Config::from_env();
        match runtime.block_on(PostgresExecutor::connect(&config)) {
            Ok(executor) => Some(Arc::new(executor)),
            Err(error) => exit_with(&format!("failed to connect to database: {error}"), 1),
        }
    };

    if let Err(error) = runtime.block_on(websocket_tool::run(executor, &ws_options)) {
        exit_with(&format!("websocket import failed: {error}"), 1);
    }
}