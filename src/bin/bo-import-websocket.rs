//! `bo-import-websocket` — live strike import over the Blitzortung websocket
//! (port of `blitzortung/cli/imprt_websocket.py`).
//!
//! ```text
//! bo-import-websocket [-v] [-d] [-t]
//! ```
//!
//! Connects to `wss://ws{1|7|8}.blitzortung.org/`, decodes each message and
//! inserts the strikes, reconnecting automatically when the socket closes.
//! `-t/--test` connects without writing to the database.

use std::sync::Arc;

use bo_service::cli::{
    build_import_metrics, describe_error, exit_with, import_websocket_tool, init_logging,
    LockWithTimeout,
};
use bo_service::config::Config;
use bo_service::executor::QueryExecutor;
use bo_service::postgres::PostgresExecutor;

fn main() {
    let args = <import_websocket_tool::WebsocketArgs as clap::Parser>::parse();
    let ws_options = import_websocket_tool::WebsocketOptions::from_args(&args);
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
        Err(error) => exit_with(&describe_error("failed to build runtime", &error), 1),
    };

    // Metrics use the importer prefix (`org.blitzortung.import`) with the
    // configured `[statsd]` receiver; a missing daemon never blocks the import.
    // In `--test` mode the database (and thus the config) is not opened, so the
    // importer metrics fall back to the built-in receiver defaults.
    let config = Config::from_env();

    let executor: Option<Arc<dyn QueryExecutor>> = if ws_options.test {
        None
    } else {
        match runtime.block_on(PostgresExecutor::connect(&config)) {
            Ok(executor) => Some(Arc::new(executor)),
            Err(error) => exit_with(
                &describe_error("failed to connect to database", error.as_ref()),
                1,
            ),
        }
    };

    let metrics = build_import_metrics(&config);

    if let Err(error) = runtime.block_on(import_websocket_tool::run(
        executor,
        &ws_options,
        metrics.as_ref(),
    )) {
        exit_with(
            &describe_error("websocket import failed", error.as_ref()),
            1,
        );
    }
}
