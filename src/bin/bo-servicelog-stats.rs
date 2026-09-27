//! `bo-servicelog-stats` — create daily statistics from the service's
//! `servicelog_YYYY-MM-DD` usage-log files.
//!
//! ```text
//! bo-servicelog-stats --dir <DIR|FILE> [--date YYYY-MM-DD] [--top N]
//!                     [--format text|json|svg] [--width N] [--height N]
//! ```
//!
//! Reads every `servicelog_YYYY-MM-DD` file in `--dir` (or one file directly)
//! and reports, per day: the total request count, the top countries/cities/
//! client versions and the local query locations for a world-map overlay.
//!
//! Exit code 1 on an unreadable directory or file, 0 otherwise.

use blitzortung_srv::cli::{describe_error, exit_with, servicelog_stats_tool};

fn main() {
    let args = <servicelog_stats_tool::ServicelogStatsArgs as clap::Parser>::parse();
    let options = match servicelog_stats_tool::ServicelogStatsOptions::from_args(&args) {
        Some(options) => options,
        None => exit_with(
            &format!(
                "invalid --format {:?}: expected text, json or svg",
                args.format
            ),
            1,
        ),
    };

    match servicelog_stats_tool::run(&options) {
        Ok(output) => {
            if !output.is_empty() {
                println!("{output}");
            }
        }
        Err(error) => exit_with(
            &describe_error(
                &format!("failed to read servicelog source {}", options.dir.display()),
                &error,
            ),
            1,
        ),
    }
}
