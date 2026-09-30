//! `bo-cluster` — detect strike clusters over a time interval and print them
//! as text or JSON (read-only; port of the `blitzortung.clustering` library,
//! last full version at bo-python commit `8ba0117`).
//!
//! ```text
//! bo-cluster [--startdate YYYYMMDD] [--starttime HHMM[SS]] [--enddate ...]
//!            [--endtime ...] [--region N] [--area WKT] [--tz TZ] [--srid N]
//!            [--json]
//! ```
//!
//! The start time defaults to the last ten minutes (end: now minus one minute)
//! in the selected time zone, matching the original cluster tool; the
//! `%Y%m%d`/`%H%M[%S]` parsing is otherwise the same as `bo-db`.

use blitzortung_srv::cli::{
    cluster_tool, connect_postgres, describe_error, exit_with, parse_timezone,
};
use blitzortung_srv::config::Config;

fn main() {
    let args = <cluster_tool::ClusterArgs as clap::Parser>::parse();
    let options = cluster_tool::ClusterOptions::from_args(&args);

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
