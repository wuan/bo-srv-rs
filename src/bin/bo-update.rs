//! `bo-update` — fetch recent strikes from `last_strikes.php` and insert the
//! ones not already present (port of `blitzortung/cli/update.py`).
//!
//! ```text
//! bo-update [--hours N] [-v] [-d] [--no-lock]
//! ```
//!
//! Exit code 1 on lock failure or any error, 0 otherwise.

use chrono::{Duration, Utc};

use bo_service::cli::{
    connect_postgres, describe_error, exit_with, init_logging, update_tool, LockWithTimeout,
};
use bo_service::config::Config;

fn main() {
    let args = <update_tool::UpdateArgs as clap::Parser>::parse();
    let update_options = update_tool::UpdateOptions::from_args(&args);
    init_logging(update_options.verbose, update_options.debug);

    let mut lock = LockWithTimeout::new("/tmp/.bo-update.lock");
    if !update_options.no_lock {
        if let Err(error) = lock.lock(10) {
            log::warn!("Could not acquire lock - another import may be running: {error}");
            std::process::exit(1);
        }
    }

    let now = Utc::now();
    let start_time = now - Duration::hours(update_options.hours);
    let url = update_tool::last_strikes_url(start_time);
    let config = Config::from_env();

    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(client) => client,
        Err(error) => exit_with(&describe_error("failed to build HTTP client", &error), 1),
    };

    let body = match client
        .get(&url)
        .basic_auth(config.username(), Some(config.password()))
        .send()
        .and_then(|response| response.error_for_status())
    {
        Ok(response) => match response.text() {
            Ok(body) => body,
            Err(error) => exit_with(&describe_error("failed to read response", &error), 1),
        },
        Err(error) => exit_with(
            &describe_error(&format!("failed to fetch strikes from URL {url}"), &error),
            1,
        ),
    };

    let url_strikes = update_tool::parse_strikes_from_text(&body);
    log::info!("Fetched {} strikes from URL", url_strikes.len());

    let (runtime, executor) = match connect_postgres(&config) {
        Ok(pair) => pair,
        Err(error) => exit_with(&describe_error("failed to connect to database", error.as_ref()), 1),
    };

    match runtime.block_on(update_tool::update_strikes(
        &executor,
        &url_strikes,
        update_options.hours,
        now,
    )) {
        Ok(result) => {
            log::info!("Import completed: {} new strikes inserted", result.inserted);
        }
        Err(error) => exit_with(&describe_error("Import failed", error.as_ref()), 1),
    }
}