//! Day/file helpers for the servicelog statistics.

use std::path::Path;

use crate::stats::aggregate::{aggregate, ServiceLogStats};
use crate::stats::parse::ParseOutcome;

/// The day name (`YYYY-MM-DD`) of a `servicelog_YYYY-MM-DD` file, or `None`.
pub fn day_from_filename(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let day = name.strip_prefix("servicelog_")?;
    if day.len() == 10 && day.as_bytes()[4] == b'-' && day.as_bytes()[7] == b'-' {
        Some(day.to_string())
    } else {
        None
    }
}

/// The current UTC day as `YYYY-MM-DD`, the default day of the report.
///
/// The servicelog file name carries the UTC date of its rows (see
/// [`crate::service_log::entry_day`]), so "today" must be UTC-based too.
pub fn today_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// A servicelog source is a day plus its parsed rows, kept for per-day reports.
#[derive(Debug, Clone, PartialEq)]
pub struct DayReport {
    pub day: String,
    pub stats: ServiceLogStats,
    pub malformed: usize,
}

/// Build a [`DayReport`] from parsed rows and the day label.
pub fn day_report(day: String, outcome: &ParseOutcome, top_n: usize) -> DayReport {
    DayReport {
        stats: aggregate(&outcome.rows, top_n),
        malformed: outcome.malformed,
        day,
    }
}
