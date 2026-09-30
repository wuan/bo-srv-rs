//! `bo-cluster` — detect strike clusters over a time interval and print them
//! as text or JSON, optionally persisting them (port of the
//! `blitzortung.clustering` library, last full version at bo-python commit
//! `8ba0117`).
//!
//! ```text
//! bo-cluster [--minutes N] [--startdate YYYYMMDD] [--starttime HHMM[SS]]
//!            [--enddate ...] [--endtime ...] [--region N] [--area WKT]
//!            [--tz TZ] [--srid N] [--json] [--insert]
//! ```
//!
//! The interval is defined by its end and its length:
//!
//! * The end defaults to the **last minute start** — `now` truncated to the
//!   minute (seconds and subseconds zeroed) in the selected `--tz`; it is *not*
//!   `now minus one minute`, unlike `bo-db`.
//! * `--minutes N` (default 10) sets the window length, so
//!   `start = end - N minutes`.
//! * `--enddate`/`--endtime` override the end with the same
//!   `%Y%m%d`/`%H%M[%S]` parsing as `bo-db` (the given value marks the interval
//!   end; the end time adds one minute, or one second when seconds are given).
//! * `--startdate`/`--starttime` are an alternative to `--minutes` that pins the
//!   interval start; combining an explicit start with `--minutes` is rejected.
//!
//! The tool is read-only unless `--insert` is given.  With `--insert` the
//! detected clusters are written to the `strike_clusters` table after the usual
//! output is printed; only clusters whose timestamp is strictly newer than the
//! latest already stored for that `interval_seconds` are inserted, so re-running
//! the same window is a no-op instead of creating duplicates.  The window length
//! is stored in a `SMALLINT` and must therefore not exceed 32767 seconds, or the
//! run fails with a clear error.
//!
//! Example: at 12:34:56 UTC the default end is 12:34:00 and, with the default
//! `--minutes 10`, the window is 12:24:00..12:34:00.
//! `-v`/`--verbose` and `-d`/`--debug` control logging (and `RUST_LOG` still
//! overrides).

use blitzortung_srv::cli::{
    cluster_tool, connect_postgres, describe_error, exit_with, init_logging, parse_timezone,
};
use blitzortung_srv::config::Config;

fn main() {
    let args = <cluster_tool::ClusterArgs as clap::Parser>::parse();
    let options = cluster_tool::ClusterOptions::from_args(&args);

    init_logging(options.verbose, options.debug);

    // Validate the time zone before touching the database, like `bo-db`.
    if parse_timezone(&options.db.tz).is_none() {
        exit_with(&format!("parse error in timezone \"{}\"", options.db.tz), 1);
    }

    let config = Config::from_env();
    let (runtime, executor) = match connect_postgres(&config) {
        Ok(pair) => pair,
        Err(error) => exit_with(
            &describe_error("failed to connect to database", error.as_ref()),
            1,
        ),
    };

    if let Err(error) = runtime.block_on(cluster_tool::run(&executor, &options)) {
        exit_with(&describe_error("error", error.as_ref()), 1);
    }
}
