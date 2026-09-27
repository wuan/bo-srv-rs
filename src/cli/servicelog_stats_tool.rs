//! `bo-servicelog-stats` implementation: daily statistics from the service's
//! `servicelog_YYYY-MM-DD` usage-log files.
//!
//! Unlike `bo-db`/`bo-import`/`bo-update`, this tool has no Python counterpart
//! (the issue asks for new statistics); it reuses the servicelog format the
//! service writes (see [`crate::service_log`]) and the aggregation helpers in
//! [`crate::service_log_stats`].

use std::path::{Path, PathBuf};

use clap::Parser;

use crate::config::Config;
use crate::service_log_stats::{
    day_from_filename, day_report, parse_file, render_ascii_map, render_html, render_json,
    render_local_svg, render_text, today_utc, DayReport, DEFAULT_TOP_N,
};

/// The default servicelog directory when nothing is configured: the location
/// the Python service used (`/var/log/blitzortung`).
pub const DEFAULT_LOG_DIR: &str = "/var/log/blitzortung";

/// The output format of the statistics report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Human-readable text summary.
    Text,
    /// Machine-readable JSON.
    Json,
    /// Standalone SVG scatter of the local query locations.
    Svg,
    /// ASCII world map (5-degree raster) of the local query locations.
    Map,
    /// Standalone static HTML report with an SVG world map (continent basemap).
    Html,
}

impl OutputFormat {
    /// Parse a `--format` value (`text`/`json`/`svg`/`map`/`html`;
    /// case-insensitive).
    pub fn parse(value: &str) -> Option<OutputFormat> {
        match value.to_ascii_lowercase().as_str() {
            "text" => Some(OutputFormat::Text),
            "json" => Some(OutputFormat::Json),
            "svg" => Some(OutputFormat::Svg),
            "map" | "ascii" => Some(OutputFormat::Map),
            "html" => Some(OutputFormat::Html),
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
    /// single servicelog file (default: the configured servicelog directory,
    /// else `/var/log/blitzortung`)
    #[arg(long)]
    pub dir: Option<PathBuf>,

    /// restrict to a single day, `YYYY-MM-DD` (default: today, UTC)
    #[arg(long)]
    pub date: Option<String>,

    /// report every servicelog file instead of only the current day
    #[arg(long)]
    pub all: bool,

    /// number of entries in each top-N list
    #[arg(long, default_value_t = DEFAULT_TOP_N)]
    pub top: usize,

    /// output format: text, json, svg, map or html
    #[arg(long, default_value = "text")]
    pub format: String,

    /// write the report to this file instead of stdout (useful for a static
    /// HTML document: `--format html --output report.html`)
    #[arg(long)]
    pub output: Option<PathBuf>,

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
    /// The day to report, or `None` to report every file (`--all`).
    pub date: Option<String>,
    pub top: usize,
    pub format: OutputFormat,
    pub width: u32,
    pub height: u32,
}

impl ServicelogStatsOptions {
    /// Build from the clap-parsed command line, resolving defaults.
    ///
    /// `--dir` falls back to the configured servicelog directory
    /// (`BO_SERVICE_SERVICELOG`/`[webservice] servicelog`) and then to
    /// [`DEFAULT_LOG_DIR`].  Without `--all`, the day defaults to today (UTC)
    /// and `--date` overrides it.
    ///
    /// Returns `None` on an unknown `--format`.
    pub fn from_args(args: &ServicelogStatsArgs) -> Option<Self> {
        // Only consult the configuration when the directory is not given on the
        // command line: this is a read-only stats tool, so it should not emit
        // the service's DB-oriented "no configuration file" warning when `--dir`
        // already pins the source.
        let config = if args.dir.is_none() {
            Config::from_env_with(|k| std::env::var(k).ok())
        } else {
            Config::default()
        };
        Self::from_args_with_config(args, &config)
    }

    /// Like [`from_args`](Self::from_args) but with an explicit [`Config`],
    /// so the default directory can be tested without touching the process
    /// environment.
    pub fn from_args_with_config(args: &ServicelogStatsArgs, config: &Config) -> Option<Self> {
        let dir = resolve_dir(args.dir.as_deref(), config);
        let date = if args.all {
            None
        } else {
            Some(args.date.clone().unwrap_or_else(today_utc))
        };
        Some(ServicelogStatsOptions {
            dir,
            date,
            top: args.top,
            format: OutputFormat::parse(&args.format)?,
            width: args.width,
            height: args.height,
        })
    }
}

/// Resolve the servicelog directory: an explicit `--dir` wins, then the
/// configured directory, then [`DEFAULT_LOG_DIR`].
pub fn resolve_dir(cli: Option<&Path>, config: &Config) -> PathBuf {
    match cli {
        Some(path) => path.to_path_buf(),
        None => config
            .configured_service_log_directory()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_LOG_DIR)),
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

/// Run the tool, returning the report(s) for stdout.
///
/// One report per matched file.  Text/JSON/ASCII-map/HTML reports are separated
/// by a blank line; SVG output for multiple days is emitted back to back.  When
/// no file matches (e.g. no servicelog written yet for the default day) a short
/// note naming the directory and day is returned instead of an empty string, so
/// the caller always has something meaningful to print.
pub fn run(options: &ServicelogStatsOptions) -> std::io::Result<String> {
    let files = filter_day(
        list_servicelog_files(&options.dir)?,
        options.date.as_deref(),
    );
    if files.is_empty() {
        return Ok(no_data_note(options));
    }

    match options.format {
        OutputFormat::Text | OutputFormat::Json | OutputFormat::Map | OutputFormat::Html => {
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
                    OutputFormat::Map => render_ascii_map(&report.day, &report.stats),
                    OutputFormat::Html => render_html(&report.day, &report.stats),
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

/// The message shown when no servicelog file matched the requested scope.
fn no_data_note(options: &ServicelogStatsOptions) -> String {
    match &options.date {
        Some(day) => format!(
            "no servicelog file for {day} in {} (nothing to report)\n",
            options.dir.display()
        ),
        None => format!(
            "no servicelog files in {} (nothing to report)\n",
            options.dir.display()
        ),
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
            dir: Some(dir),
            date: None,
            all: true,
            top: DEFAULT_TOP_N,
            format: "text".to_string(),
            output: None,
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
        assert_eq!(OutputFormat::parse("map"), Some(OutputFormat::Map));
        assert_eq!(OutputFormat::parse("ASCII"), Some(OutputFormat::Map));
        assert_eq!(OutputFormat::parse("html"), Some(OutputFormat::Html));
        assert_eq!(OutputFormat::parse("HTML"), Some(OutputFormat::Html));
        assert_eq!(OutputFormat::parse("xml"), None);
    }

    #[test]
    fn from_args_rejects_unknown_format() {
        let mut a = args(PathBuf::from("/tmp"));
        a.format = "bogus".to_string();
        assert!(ServicelogStatsOptions::from_args_with_config(&a, &Config::default()).is_none());
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
            ..ServicelogStatsOptions::from_args_with_config(&args(dir.clone()), &Config::default())
                .unwrap()
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
        let options =
            ServicelogStatsOptions::from_args_with_config(&a, &Config::default()).unwrap();
        let output = run(&options).unwrap();
        assert!(output.contains("local queries: 2"));
        assert!(output.contains("width=\"200\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_html_for_a_directory() {
        let dir = temp_dir("run-html");
        std::fs::write(dir.join("servicelog_2023-11-14"), SAMPLE).unwrap();
        let a = ServicelogStatsArgs {
            format: "html".to_string(),
            ..args(dir.clone())
        };
        let options =
            ServicelogStatsOptions::from_args_with_config(&a, &Config::default()).unwrap();
        let output = run(&options).unwrap();
        assert!(output.starts_with("<!DOCTYPE html>"), "{output}");
        assert!(
            output.contains("servicelog statistics for 2023-11-14"),
            "{output}"
        );
        assert!(output.contains("<svg"), "{output}");
        assert!(output.contains("class=\"basemap\""), "{output}");
        // Both offline/background and interactive overlays are present, on
        // separate maps.
        assert!(
            output.contains("data-set=\"background (offline)\""),
            "{output}"
        );
        assert!(output.contains("data-set=\"interactive\""), "{output}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_map_for_a_directory() {
        let dir = temp_dir("run-map");
        std::fs::write(dir.join("servicelog_2023-11-14"), SAMPLE).unwrap();
        let a = ServicelogStatsArgs {
            format: "map".to_string(),
            ..args(dir.clone())
        };
        let options =
            ServicelogStatsOptions::from_args_with_config(&a, &Config::default()).unwrap();
        let output = run(&options).unwrap();
        assert!(
            output.contains("servicelog local-query maps for 2023-11-14"),
            "{output}"
        );
        assert!(output.contains("72x36 cells of 5 degrees"), "{output}");
        // Separate maps for the offline and interactive queries.
        assert!(output.contains("offline local-query world map"), "{output}");
        assert!(
            output.contains("interactive local-query world map"),
            "{output}"
        );
        // Both SAMPLE local rows use `minute_length=10`, so both are offline.
        assert!(
            output.contains(
                "offline local-query world map (72x36 cells of 5 degrees, 2 queries, 2 hits)"
            ),
            "{output}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_on_empty_directory_reports_no_data() {
        let dir = temp_dir("empty");
        let options =
            ServicelogStatsOptions::from_args_with_config(&args(dir.clone()), &Config::default())
                .unwrap();
        let output = run(&options).unwrap();
        assert!(output.contains("no servicelog files"), "{output}");
        assert!(output.contains("nothing to report"), "{output}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The default day is today (UTC) and only the matching file is reported.
    #[test]
    fn default_day_is_today_and_filters_to_it() {
        let dir = temp_dir("today");
        std::fs::write(dir.join(format!("servicelog_{}", today_utc())), SAMPLE).unwrap();
        std::fs::write(dir.join("servicelog_2023-11-14"), SAMPLE).unwrap();

        let a = ServicelogStatsArgs {
            all: false,
            ..args(dir.clone())
        };
        let options =
            ServicelogStatsOptions::from_args_with_config(&a, &Config::default()).unwrap();
        assert_eq!(options.date.as_deref(), Some(today_utc().as_str()));

        let output = run(&options).unwrap();
        assert!(output.contains(&format!("servicelog statistics for {}", today_utc())));
        assert!(!output.contains("2023-11-14"), "{output}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `--date` overrides the today default; `--all` clears the day filter.
    #[test]
    fn explicit_date_and_all_control_the_scope() {
        let mut a = args(PathBuf::from("/tmp"));
        a.all = false;
        a.date = Some("2023-11-14".to_string());
        let options =
            ServicelogStatsOptions::from_args_with_config(&a, &Config::default()).unwrap();
        assert_eq!(options.date.as_deref(), Some("2023-11-14"));

        a.all = true;
        a.date = Some("2023-11-14".to_string());
        let options =
            ServicelogStatsOptions::from_args_with_config(&a, &Config::default()).unwrap();
        assert_eq!(options.date, None, "--all clears the day filter");
    }

    /// `--dir` falls back to the configured servicelog directory, then to the
    /// built-in default.
    #[test]
    fn dir_defaults_from_config_then_builtin() {
        let configured = Config {
            service_log_dir: Some("/custom/servicelog".to_string()),
            ..Config::default()
        };
        assert_eq!(
            resolve_dir(None, &configured),
            PathBuf::from("/custom/servicelog")
        );
        // An explicit `--dir` wins over the config.
        assert_eq!(
            resolve_dir(Some(Path::new("/explicit")), &configured),
            PathBuf::from("/explicit")
        );
        // Nothing configured -> the built-in default.
        assert_eq!(
            resolve_dir(None, &Config::default()),
            PathBuf::from(DEFAULT_LOG_DIR)
        );
    }

    /// `statistics_for_single_file` reports the file's day.
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
