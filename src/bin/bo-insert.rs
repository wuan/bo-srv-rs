//! `bo-insert` — import protected strike logs from data.blitzortung.org into
//! the database (port of `blitzortung/cli/imprt.py`).
//!
//! ```text
//! bo-insert [-v] [-d] [--no-timeout] [--startdate YYYYMMDD] [--update]
//! ```
//!
//! For each known region the latest recorded timestamp is read from the
//! database and the ten-minute logs from there up to now are downloaded (with
//! HTTP basic auth) and inserted in batches of 1000.

use bo_service::cli::{connect_postgres, exit_with, init_logging, insert_tool, LockWithTimeout, Options};
use bo_service::config::Config;
use bo_service::dataimport::HttpFileTransport;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = Options::parse(&args, insert_tool::SPECS);
    let insert_options = insert_tool::InsertOptions::from_options(&options);
    init_logging(insert_options.verbose, insert_options.debug);

    let mut lock = LockWithTimeout::new("/tmp/.bo-import.lock");
    if let Err(error) = lock.lock(10) {
        log::warn!("could not acquire lock: {error}");
        std::process::exit(1);
    }

    let start_time = insert_tool::resolve_start_time(&insert_options);

    let config = Config::from_env();
    let (_runtime, executor) = match connect_postgres(&config) {
        Ok(pair) => pair,
        Err(error) => exit_with(&format!("failed to connect to database: {error}"), 1),
    };

    // Per-file HTTP timeout: the task pins this at 30 seconds (the Python
// `HttpFileTransport` default is 60s); the overall per-region budget is
// enforced cooperatively in `import_strikes`.
let transport =
    HttpFileTransport::with_timeout(&config, std::time::Duration::from_secs(30));
    let (strikes, errors) = insert_tool::import_strikes(
        &executor,
        &transport,
        insert_tool::REGIONS,
        start_time,
        insert_options.no_timeout,
        insert_options.update,
    );
    log::info!("imported {strikes} strikes (error count {errors})");
}