//! `bo-db` — query the strike database and print strikes as text or a grid
//! (port of `blitzortung/cli/db.py`).
//!
//! ```text
//! bo-db [--startdate YYYYMMDD] [--starttime HHMM[SS]] [--enddate ...]
//!       [--endtime ...] [--area WKT] [--useenv] [--tz TZ] [--srid N]
//!       [--precision N] [--grid F | --x-grid F --y-grid F] [--map]
//! ```
//!
//! Times default to the last hour (end: now minus one minute) in the selected
//! time zone.

use bo_service::cli::{connect_postgres, db_tool, describe_error, exit_with, parse_timezone};
use bo_service::config::Config;

fn main() {
    let args = <db_tool::DbArgs as clap::Parser>::parse();
    let db_options = db_tool::DbOptions::from_args(&args);

    // Validate the time zone before touching the database, like
    // `cli/db.py.main`.
    if parse_timezone(&db_options.tz).is_none() {
        exit_with(&format!("parse error in timezone \"{}\"", db_options.tz), 1);
    }

    let config = Config::from_env();
    let (runtime, executor) = match connect_postgres(&config) {
        Ok(pair) => pair,
        Err(error) => exit_with(
            &describe_error("failed to connect to database", error.as_ref()),
            1,
        ),
    };

    if let Err(error) = runtime.block_on(db_tool::run(&executor, &db_options)) {
        exit_with(&describe_error("error", error.as_ref()), 1);
    }
}
