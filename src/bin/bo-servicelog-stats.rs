//! `bo-servicelog-stats` — create daily statistics from the service's
//! `servicelog_YYYY-MM-DD` usage-log files.
//!
//! ```text
//! bo-servicelog-stats [--dir <DIR|FILE>] [--date YYYY-MM-DD] [--all] [--top N]
//!                     [--format text|json|svg|map|html] [--output FILE]
//!                     [--width N] [--height N]
//! ```
//!
//! Reads `servicelog_YYYY-MM-DD` files and reports, per day: the total request
//! count, the top countries/cities/client versions and an ASCII world map of
//! the local queries.
//!
//! `--format html` produces a standalone static HTML document with an SVG world
//! map (a coarse continent basemap plus the local-query overlay); combine it
//! with `--output report.html` to write the document to a file.
//!
//! Defaults: `--dir` is the configured servicelog directory (else
//! `/var/log/blitzortung`), the day is **today (UTC)** unless `--date` or
//! `--all` is given, and `--format text` prints the summary plus the world map.
//!
//! Exit code 1 on an unreadable directory or file, 0 otherwise.

use blitzortung_srv::cli::{describe_error, exit_with, servicelog_stats_tool};

fn main() {
    let args = <servicelog_stats_tool::ServicelogStatsArgs as clap::Parser>::parse();
    let options = match servicelog_stats_tool::ServicelogStatsOptions::from_args(&args) {
        Some(options) => options,
        None => exit_with(
            &format!(
                "invalid --format {:?}: expected text, json, svg, map or html",
                args.format
            ),
            1,
        ),
    };

    let output = match servicelog_stats_tool::run(&options) {
        Ok(output) => output,
        Err(error) => exit_with(
            &describe_error(
                &format!("failed to read servicelog source {}", options.dir.display()),
                &error,
            ),
            1,
        ),
    };

    match &args.output {
        Some(path) => {
            if let Err(error) = std::fs::write(path, &output) {
                exit_with(
                    &describe_error(
                        &format!("failed to write report to {}", path.display()),
                        &error,
                    ),
                    1,
                );
            }
        }
        None => print!("{output}"),
    }
}
