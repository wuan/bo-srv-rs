//! `bo-import` — import protected strike logs from data.blitzortung.org into
//! the database (port of `blitzortung/cli/imprt.py`).
//!
//! ```text
//! bo-import [-v] [-d] [--no-timeout] [--startdate YYYYMMDD] [--update]
//! ```
//!
//! For each known region the latest recorded timestamp is read from the
//! database and the ten-minute logs from there up to now are downloaded (with
//! HTTP basic auth) and inserted in batches of 1000.

use bo_service::cli::{
    build_import_metrics, connect_postgres, describe_error, exit_with, import_tool, init_logging,
    LockWithTimeout,
};
use bo_service::config::Config;
use bo_service::dataimport::HttpFileTransport;

fn main() {
    let args = <import_tool::ImportArgs as clap::Parser>::parse();
    let import_options = import_tool::ImportOptions::from_args(&args);
    init_logging(import_options.verbose, import_options.debug);

    let mut lock = LockWithTimeout::new("/tmp/.bo-import.lock");
    if let Err(error) = lock.lock(10) {
        log::warn!("could not acquire lock: {error}");
        std::process::exit(1);
    }

    let start_time = import_tool::resolve_start_time(&import_options);

    let config = Config::from_env();
    let metrics = build_import_metrics(&config);
    let (runtime, executor) = match connect_postgres(&config) {
        Ok(pair) => pair,
        Err(error) => exit_with(
            &describe_error("failed to connect to database", error.as_ref()),
            1,
        ),
    };

    // Per-file HTTP timeout: the task pins this at 30 seconds (the Python
    // `HttpFileTransport` default is 60s); the overall per-region budget is
    // enforced cooperatively in `import_strikes`.
    let transport = HttpFileTransport::with_timeout(&config, std::time::Duration::from_secs(30));
    let (strikes, errors) = runtime.block_on(import_tool::import_strikes(
        &executor,
        &transport,
        import_tool::REGIONS,
        start_time,
        import_options.no_timeout,
        import_options.update,
        metrics.as_ref(),
    ));
    log::info!("imported {strikes} strikes (error count {errors})");
}
