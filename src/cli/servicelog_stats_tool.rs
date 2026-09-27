//! `bo-servicelog-stats` implementation: daily statistics from the service's
//! `servicelog_YYYY-MM-DD` usage-log files.
//!
//! Unlike `bo-db`/`bo-import`/`bo-update`, this tool has no Python counterpart
//! (the issue asks for new statistics); it reuses the servicelog format the
//! service writes (see [`crate::service_log`]) and the aggregation helpers in
//! [`crate::service_log_stats`].

use std::path::{Path, PathBuf};

use clap::Parser;

use crate::service_log_stats::{
    day_from_filename, day_report, parse_file, render_json, render_local_svg, render_text,
    DayReport, DEFAULT_TOP_N,
};

/// The output format of the statistics report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Human-readable text summary.
    Text,
    /// Machine-readable JSON.
    Json,
    /// Standalone SVG scatter of the local query locations.
    Svg,
}

impl OutputFormat {
    /// Parse a `--format` value (`text`/`json`/`svg`; case-insensitive).
    pub fn parse(value: &str) -> Option<OutputFormat> {
        match value.to_ascii_lowercase().as_str() {
            "text" => Some(OutputFormat::Text),
            "json" => Some(OutputFormat::Json),
            "svg" => Some(OutputFormat::Svg),
            _ => None,
        }
    }
}

/// `bo-servicelog-stats` command-line options.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "bo-servicelog-stats",
    about = "Create daily statistics from the service's servicelog files",
    version
)]
pub struct ServicelogStatsArgs {
    /// servicelog directory (contains `servicelog_YYYY-MM-DD` files) or a
    /// single servicelog file
    #[arg(long)]
    pub dir: PathBuf,

    /// restrict to a single day, `YYYY-MM-DD` (default: every file in the
    /// directory)
    #[arg(long)]
    pub date: Option<String>,

    /// number of entries in each top-N list
    #[arg(long, default_value_t = DEFAULT_TOP_N)]
    pub top: usize,

    /// output format: text, json or svg
    #[arg(long, default_value = "text")]
    pub format: String,

    /// SVG width in pixels (only for `--format svg`)
    #[arg(long, default_value_t = 1024)]
    pub width: u32,

    /// SVG height in pixels (only for `--format svg`)
    #[arg(long, default_value_t = 512)]
    pub height: u32,
}

/// Resolved `bo-servicelog-stats` options.
#[derive(Debug, Clone)]
pub struct ServicelogStatsOptions {
    pub dir: PathBuf,
    pub date: Option<String>,
    pub top: usize,
    pub format: OutputFormat,
    pub width: u32,
    pub height: u32,
}

impl ServicelogStatsOptions {
    /// Build from the clap-parsed command line; `None` on an unknown format.
    pub fn from_args(args: &ServicelogStatsArgs) -> Option<Self> {
        Some(ServicelogStatsOptions {
            dir: args.dir.clone(),
            date: args.date.clone(),
            top: args.top,
            format: OutputFormat::parse(&args.format)?,
            width: args.width,
            height: args.height,
        })
    }
}

/// List the servicelog files in `dir`, sorted by day.
///
/// A regular file is returned as a single-element list (so `--dir` may point at
/// one `servicelog_YYYY-MM-DD` file).  A missing directory is an error.
pub fn list_servicelog_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    if dir.is_file() {
        return Ok(vec![dir.to_path_buf()]);
    }
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_file() && day_from_filename(&path).is_some() {
            files.push(path);
        }
    }
    // Sort by the day encoded in the file name (lexicographic == chronological).
    files.sort_by_key(|p| day_from_filename(p).unwrap_or_default());
    Ok(files)
}

/// Filter the file list to a single day when `date` is set.
pub fn filter_day(files: Vec<PathBuf>, date: Option<&str>) -> Vec<PathBuf> {
    match date {
        None => files,
        Some(day) => files
            .into_iter()
            .filter(|p| day_from_filename(p).as_deref() == Some(day))
            .collect(),
    }
}

/// Run the tool, writing the report(s) to stdout.
///
/// One report per matched file.  Text/JSON reports are separated by a blank
/// line; SVG output for multiple days is emitted back to back.
pub fn run(options: &ServicelogStatsOptions) -> std::io::Result<String> {
    let files = filter_day(
        list_servicelog_files(&options.dir)?,
        options.date.as_deref(),
    );

    match options.format {
        OutputFormat::Text | OutputFormat::Json => {
            let mut reports: Vec<DayReport> = Vec::new();
            for file in &files {
                let outcome = parse_file(file)?;
                let day = day_from_filename(file).unwrap_or_else(|| "unknown".to_string());
                reports.push(day_report(day, &outcome, options.top));
            }
            let rendered: Vec<String> = reports
                .iter()
                .map(|report| match options.format {
                    OutputFormat::Text => render_text(&report.day, &report.stats),
                    OutputFormat::Json => render_json(&report.day, &report.stats),
                    OutputFormat::Svg => unreachable!(),
                })
                .collect();
            Ok(rendered.join("\n"))
        }
        OutputFormat::Svg => {
            let mut svgs: Vec<String> = Vec::new();
            for file in &files {
                let outcome = parse_file(file)?;
                let stats = crate::service_log_stats::aggregate(&outcome.rows, options.top);
                svgs.push(render_local_svg(&stats, options.width, options.height));
            }
            Ok(svgs.join("\n"))
        }
    }
}

/// Convenience: the single-day report for a `--date` or single-file run.
pub fn statistics_for_single_file(path: &Path, top: usize) -> std::io::Result<DayReport> {
    let outcome = parse_file(path)?;
    Ok(day_report(
        day_from_filename(path).unwrap_or_else(|| "unknown".to_string()),
        &outcome,
        top,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(dir: PathBuf) -> ServicelogStatsArgs {
        ServicelogStatsArgs {
            dir,
            date: None,
            top: DEFAULT_TOP_N,
            format: "text".to_string(),
            width: 1024,
            height: 512,
        }
    }

    const SAMPLE: &str = "\
08:44:50.596\tUS\tSun Prairie\t\t\tA\t352\t0\t10\t25000\t0\t0\t-\t-\t-\t0.036\n\
08:44:50.637\tSE\tGothenburg\t\t\tA\t352\t0\t10\t5000\t-1\t0\t4\t13\t5\t0.000\n\
08:44:50.655\tRO\tBucharest\t\t\tA\t352\t0\t10\t5000\t-1\t0\t5\t8\t5\t0.039\n";

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bo-stats-cli-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn format_parsing() {
        assert_eq!(OutputFormat::parse("text"), Some(OutputFormat::Text));
        assert_eq!(OutputFormat::parse("JSON"), Some(OutputFormat::Json));
        assert_eq!(OutputFormat::parse("Svg"), Some(OutputFormat::Svg));
        assert_eq!(OutputFormat::parse("xml"), None);
    }

    #[test]
    fn from_args_rejects_unknown_format() {
        let mut a = args(PathBuf::from("/tmp"));
        a.format = "bogus".to_string();
        assert!(ServicelogStatsOptions::from_args(&a).is_none());
    }

    #[test]
    fn lists_only_servicelog_files_sorted() {
        let dir = temp_dir("list");
        std::fs::write(dir.join("servicelog_2023-11-15"), SAMPLE).unwrap();
        std::fs::write(dir.join("servicelog_2023-11-14"), SAMPLE).unwrap();
        std::fs::write(dir.join("other.log"), "x").unwrap();
        std::fs::create_dir_all(dir.join("subdir")).unwrap();

        let files = list_servicelog_files(&dir).unwrap();
        let days: Vec<String> = files
            .iter()
            .map(|p| day_from_filename(p).unwrap())
            .collect();
        assert_eq!(days, ["2023-11-14", "2023-11-15"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_single_file_is_listed_as_itself() {
        let dir = temp_dir("single");
        let file = dir.join("servicelog_2023-11-14");
        std::fs::write(&file, SAMPLE).unwrap();
        assert_eq!(list_servicelog_files(&file).unwrap(), vec![file.clone()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn filters_to_one_day() {
        let files = vec![
            PathBuf::from("/x/servicelog_2023-11-14"),
            PathBuf::from("/x/servicelog_2023-11-15"),
        ];
        let filtered = filter_day(files.clone(), Some("2023-11-14"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(day_from_filename(&filtered[0]).unwrap(), "2023-11-14");
        assert_eq!(filter_day(files, None).len(), 2);
    }

    #[test]
    fn run_text_for_a_directory() {
        let dir = temp_dir("run-text");
        std::fs::write(dir.join("servicelog_2023-11-14"), SAMPLE).unwrap();
        let options = ServicelogStatsOptions {
            top: 10,
            ..ServicelogStatsOptions::from_args(&args(dir.clone())).unwrap()
        };
        let output = run(&options).unwrap();
        assert!(output.contains("servicelog statistics for 2023-11-14"));
        assert!(output.contains("total requests: 3"));
        assert!(output.contains("local query locations: 2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_svg_for_a_directory() {
        let dir = temp_dir("run-svg");
        std::fs::write(dir.join("servicelog_2023-11-14"), SAMPLE).unwrap();
        let a = ServicelogStatsArgs {
            format: "svg".to_string(),
            width: 200,
            height: 100,
            ..args(dir.clone())
        };
        let options = ServicelogStatsOptions::from_args(&a).unwrap();
        let output = run(&options).unwrap();
        assert!(output.contains("local queries: 2"));
        assert!(output.contains("width=\"200\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_on_empty_directory_is_empty() {
        let dir = temp_dir("empty");
        let options = ServicelogStatsOptions::from_args(&args(dir.clone())).unwrap();
        assert_eq!(run(&options).unwrap(), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn statistics_for_single_file_reports_day() {
        let dir = temp_dir("single-report");
        let file = dir.join("servicelog_2023-11-14");
        std::fs::write(&file, SAMPLE).unwrap();
        let report = statistics_for_single_file(&file, 10).unwrap();
        assert_eq!(report.day, "2023-11-14");
        assert_eq!(report.stats.total_requests, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
