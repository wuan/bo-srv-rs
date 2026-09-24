//! Shared helpers for the Blitzortung CLI tools (`bo-db`, `bo-import`,
//! `bo-update`, `bo-import-websocket`).
//!
//! The Python tools use `optparse`; the Rust port uses a small hand-written
//! parser instead of pulling in a CLI crate, matching the project's preference
//! for minimal dependencies.

pub mod db_tool;
pub mod import_tool;
pub mod import_websocket_tool;
pub mod update_tool;

use std::collections::HashMap;
use std::path::PathBuf;

/// A single command-line option: long name, optional short name, whether it
/// takes a value, and the help text shown by `-h`/`--help`.
///
/// Construct with [`spec`] for the common cases.
#[derive(Debug, Clone, Copy)]
pub struct OptionSpec {
    pub long: &'static str,
    pub short: &'static str,
    pub takes_value: bool,
    pub help: &'static str,
}

/// Build an [`OptionSpec`] (`short` may be `""` for a long-only option).
pub const fn spec(
    long: &'static str,
    short: &'static str,
    takes_value: bool,
    help: &'static str,
) -> OptionSpec {
    OptionSpec {
        long,
        short,
        takes_value,
        help,
    }
}

/// A parsed command line: long/short options plus positional arguments.
#[derive(Debug, Default)]
pub struct Options {
    values: HashMap<String, String>,
    flags: HashMap<String, bool>,
    positional: Vec<String>,
}

/// Exit code used for command-line errors (matches Python `optparse`, which
/// calls `parser.error()` -> `sys.exit(2)`).
pub const USAGE_ERROR_EXIT_CODE: i32 = 2;

/// The non-success outcomes of [`Options::try_parse`].
#[derive(Debug, PartialEq, Eq)]
pub enum ParseOutcome {
    /// `-h`/`--help` was requested (exit 0).
    Help,
    /// The command line was invalid (exit 2); the string is the error message.
    Error(String),
}

impl Options {
    /// Parse `args` against a list of specs, handling `-h`/`--help` and
    /// rejecting unknown options the way Python's `optparse` does.
    ///
    /// `-h`/`--help` prints the usage/option summary and exits 0.  An unknown
    /// option or a missing value prints an error to stderr and exits 2.
    /// `--long value`, `--long=value`, `-s value` and `-s=value` are accepted;
    /// flags may be repeated but the last occurrence wins.
    pub fn parse(program: &str, args: &[String], specs: &[OptionSpec]) -> Self {
        match Options::try_parse(args, specs) {
            Ok(options) => options,
            Err(ParseOutcome::Help) => print_help_and_exit(program, specs),
            Err(ParseOutcome::Error(message)) => usage_error(program, specs, &message),
        }
    }

    /// Pure parser core: returns the parsed options or the desired outcome
    /// (`Help` or a usage `Error`).  Kept separate from [`Options::parse`] so
    /// the behaviour can be unit tested without exiting the process.
    pub fn try_parse(args: &[String], specs: &[OptionSpec]) -> Result<Self, ParseOutcome> {
        let mut options = Options::default();
        let mut index = 0;
        while index < args.len() {
            let arg = &args[index];
            if arg == "--" {
                options.positional.extend_from_slice(&args[index + 1..]);
                break;
            } else if arg == "--help" || arg == "-h" {
                return Err(ParseOutcome::Help);
            } else if let Some(rest) = arg.strip_prefix("--") {
                let (name, inline_value) = match rest.split_once('=') {
                    Some((name, value)) => (name, Some(value.to_string())),
                    None => (rest, None),
                };
                match specs.iter().find(|s| s.long == name) {
                    Some(found) => options.apply(found, inline_value, args, &mut index)?,
                    None => return Err(ParseOutcome::Error(format!("no such option: --{name}"))),
                }
            } else if let Some(rest) = arg.strip_prefix('-') {
                if rest.is_empty() {
                    options.positional.push(arg.clone());
                } else {
                    let (name, inline_value) = match rest.split_once('=') {
                        Some((name, value)) => (name, Some(value.to_string())),
                        None => (rest, None),
                    };
                    match specs.iter().find(|s| s.short == name) {
                        Some(found) => options.apply(found, inline_value, args, &mut index)?,
                        None => return Err(ParseOutcome::Error(format!("no such option: -{name}"))),
                    }
                }
            } else {
                options.positional.push(arg.clone());
            }
            index += 1;
        }
        Ok(options)
    }

    fn apply(
        &mut self,
        found: &OptionSpec,
        inline_value: Option<String>,
        args: &[String],
        index: &mut usize,
    ) -> Result<(), ParseOutcome> {
        if found.takes_value {
            let value = match inline_value {
                Some(value) => value,
                None => {
                    if *index + 1 < args.len() {
                        *index += 1;
                        args[*index].clone()
                    } else {
                        return Err(ParseOutcome::Error(format!(
                            "--{} option requires an argument",
                            found.long
                        )));
                    }
                }
            };
            self.values.insert(found.long.to_string(), value);
        } else {
            self.flags.insert(found.long.to_string(), true);
        }
        Ok(())
    }

    /// A string option value (empty when unset).
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(|s| s.as_str())
    }

    /// Parse a typed option with a default.
    pub fn parse_or<T: std::str::FromStr>(&self, name: &str, default: T) -> T {
        self.value(name)
            .and_then(|v| v.parse::<T>().ok())
            .unwrap_or(default)
    }

    /// Parse a typed option returning `None` when absent.
    pub fn parse_opt<T: std::str::FromStr>(&self, name: &str) -> Option<T> {
        self.value(name).and_then(|v| v.parse::<T>().ok())
    }

    /// Whether a boolean flag was set.
    pub fn flag(&self, name: &str) -> bool {
        self.flags.get(name).copied().unwrap_or(false)
    }
}

/// Render the `optparse`-style help text for `program`.
///
/// Layout matches Python's `optparse`:
///
/// ```text
/// Usage: <program> [options]
///
/// Options:
///   -h, --help            show this help message and exit
///   --startdate=STARTDATE
///                         start date for data retrieval
/// ```
pub fn render_help(program: &str, specs: &[OptionSpec]) -> String {
    let mut out = String::new();
    out.push_str(&format!("Usage: {program} [options]\n\nOptions:\n"));

    // The implicit help option, like `optparse.OptionParser`.
    let mut entries: Vec<(String, String)> = vec![(
        "-h, --help".to_string(),
        "show this help message and exit".to_string(),
    )];
    for s in specs {
        let flag = if s.takes_value {
            let metavar = s.long.to_uppercase().replace('-', "_");
            if s.short.is_empty() {
                format!("--{}={}", s.long, metavar)
            } else {
                format!("-{}, --{}={}", s.short, s.long, metavar)
            }
        } else if s.short.is_empty() {
            format!("--{}", s.long)
        } else {
            format!("-{}, --{}", s.short, s.long)
        };
        entries.push((flag, s.help.to_string()));
    }

    let width = entries.iter().map(|(flag, _)| flag.len()).max().unwrap_or(0);
    for (flag, help) in entries {
        out.push_str(&format!("  {flag:<width$}  {help}\n", width = width));
    }
    out
}

/// Print the help text and exit 0 (`optparse -h`).
fn print_help_and_exit(program: &str, specs: &[OptionSpec]) -> ! {
    print!("{}", render_help(program, specs));
    std::process::exit(0)
}

/// Print an `optparse`-style usage error to stderr and exit 2.
fn usage_error(program: &str, specs: &[OptionSpec], message: &str) -> ! {
    eprint!("{}", render_help(program, specs));
    eprintln!("{program}: error: {message}");
    std::process::exit(USAGE_ERROR_EXIT_CODE)
}

/// Build a Tokio runtime and connect a [`PostgresExecutor`] to the configured
/// database.  Used by all CLI tools except `bo-import-websocket`, which builds
/// its own runtime for the websocket event loop.
///
/// [`PostgresExecutor`]: crate::postgres::PostgresExecutor
pub fn connect_postgres(
    config: &crate::config::Config,
) -> Result<
    (tokio::runtime::Runtime, crate::postgres::PostgresExecutor),
    Box<dyn std::error::Error + Send + Sync>,
> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let executor = runtime.block_on(crate::postgres::PostgresExecutor::connect(config))?;
    Ok((runtime, executor))
}

/// Configure console logging from the `-v`/`-d` flags plus the `RUST_LOG`
/// environment override.
pub fn init_logging(verbose: bool, debug: bool) {
    let default = if debug {
        "debug"
    } else if verbose {
        "info"
    } else {
        "warn"
    };
    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(default),
    );
    if debug {
        builder.filter_level(log::LevelFilter::Debug);
    }
    let _ = builder.try_init();
}

/// Abort with an error message and exit code, like the Python tools'
/// `sys.exit(code)` after printing a parse error.
pub fn exit_with(message: &str, code: i32) -> ! {
    eprintln!("{message}");
    std::process::exit(code)
}

/// Labels used by `cli/db.py` for the default date/time formats.
pub const DATE_FORMAT: &str = "%Y%m%d";
/// Time format without seconds (`cli/db.py TIME_FORMAT`).
pub const TIME_FORMAT: &str = "%H%M";

/// Parse a `--tz` value into a `chrono_tz::Tz`; defaults to UTC.
pub fn parse_timezone(tz: &str) -> Option<chrono_tz::Tz> {
    use std::str::FromStr;
    if tz.eq_ignore_ascii_case("UTC") {
        return Some(chrono_tz::UTC);
    }
    chrono_tz::Tz::from_str(tz).ok()
}

/// `cli/db.py.parse_time`: combine a `%Y%m%d` date and `%H%M[%S]` time in
/// `tz`.  When `is_end_time` is set, add one second if seconds were given,
/// otherwise one minute.
///
/// Returns a [`chrono::DateTime<chrono::Utc>`] (the instant) or `None` on a
/// parse error.
pub fn parse_local_time(
    date_string: &str,
    time_string: &str,
    tz: chrono_tz::Tz,
    is_end_time: bool,
) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::TimeZone;
    let has_seconds = time_string.len() > 4;
    let naive_date = chrono::NaiveDate::parse_from_str(date_string, DATE_FORMAT).ok()?;
    let fmt = if has_seconds {
        "%H%M%S"
    } else {
        TIME_FORMAT
    };
    let naive_time = chrono::NaiveTime::parse_from_str(time_string, fmt).ok()?;
    let naive = naive_date.and_time(naive_time);
    let local = tz.from_local_datetime(&naive).single()?;
    let result = if is_end_time {
        if has_seconds {
            local + chrono::Duration::seconds(1)
        } else {
            local + chrono::Duration::minutes(1)
        }
    } else {
        local
    };
    Some(result.with_timezone(&chrono::Utc))
}

/// A simple PID/flock based inter-process lock with a timeout, mirroring
/// `blitzortung/lock.py` (`LockWithTimeout` + `FailedToAcquireException`).
pub struct LockWithTimeout {
    path: PathBuf,
    handle: Option<std::fs::File>,
}

/// Raised when a lock could not be acquired within the timeout.
#[derive(Debug)]
pub struct FailedToAcquireException;

impl std::fmt::Display for FailedToAcquireException {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "failed to acquire lock")
    }
}

impl std::error::Error for FailedToAcquireException {}

impl LockWithTimeout {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        LockWithTimeout {
            path: path.into(),
            handle: None,
        }
    }

    /// Acquire the lock within `timeout_seconds`, polling at 100 ms intervals
    /// like the Python `fasteners.InterProcessLock`.
    pub fn lock(&mut self, timeout_seconds: u64) -> Result<(), FailedToAcquireException> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_seconds);
        loop {
            match self.try_lock() {
                Ok(()) => return Ok(()),
                Err(()) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(FailedToAcquireException);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
    }

    fn try_lock(&mut self) -> Result<(), ()> {
        use std::os::unix::io::AsRawFd;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&self.path)
            .map_err(|_| ())?;
        let rc = libc_flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB);
        if rc == 0 {
            self.handle = Some(file);
            Ok(())
        } else {
            Err(())
        }
    }

    /// Release the lock.
    pub fn unlock(&mut self) {
        use std::os::unix::io::AsRawFd;
        if let Some(file) = self.handle.take() {
            libc_flock(file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

impl Drop for LockWithTimeout {
    fn drop(&mut self) {
        self.unlock();
    }
}

/// Minimal `flock(2)` binding (avoids an extra dependency).
mod libc {
    pub const LOCK_EX: i32 = 2;
    pub const LOCK_NB: i32 = 4;
    pub const LOCK_UN: i32 = 8;

    extern "C" {
        pub fn flock(fd: i32, operation: i32) -> i32;
    }
}

fn libc_flock(fd: i32, operation: i32) -> i32 {
    unsafe { libc::flock(fd, operation) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    const SPECS: &[OptionSpec] = &[
        spec("startdate", "", true, "start date for data retrieval"),
        spec("verbose", "v", false, "verbose output"),
        spec("debug", "d", false, "debug output"),
        spec("precision", "", true, "precision of coordinates"),
    ];

    /// Parse without exiting the process (test helper).
    fn parse(items: &[&str]) -> Options {
        Options::try_parse(&args(items), SPECS).expect("parse ok")
    }

    #[test]
    fn parses_long_and_short_options() {
        let options = parse(&["--startdate", "20250101", "-v", "--precision=2"]);
        assert_eq!(options.value("startdate"), Some("20250101"));
        assert!(options.flag("verbose"));
        assert_eq!(options.parse_or("precision", 4), 2);
        assert!(!options.flag("debug"));
    }

    #[test]
    fn parses_short_equals() {
        let options = parse(&["-d"]);
        assert!(options.flag("debug"));
    }

    #[test]
    fn defaults_are_used_when_absent() {
        let options = parse(&[]);
        assert_eq!(options.parse_or("precision", 4), 4);
        assert_eq!(options.value("startdate"), None);
    }

    #[test]
    fn help_flag_is_reported_for_h_and_long_help() {
        assert_eq!(
            Options::try_parse(&args(&["-h"]), SPECS).unwrap_err(),
            ParseOutcome::Help
        );
        assert_eq!(
            Options::try_parse(&args(&["--help"]), SPECS).unwrap_err(),
            ParseOutcome::Help
        );
        // `-h` wins even when combined with other options.
        assert_eq!(
            Options::try_parse(&args(&["--startdate", "20250101", "--help"]), SPECS).unwrap_err(),
            ParseOutcome::Help
        );
    }

    #[test]
    fn unknown_option_is_an_error() {
        assert_eq!(
            Options::try_parse(&args(&["--bogus"]), SPECS).unwrap_err(),
            ParseOutcome::Error("no such option: --bogus".to_string())
        );
        assert_eq!(
            Options::try_parse(&args(&["-z"]), SPECS).unwrap_err(),
            ParseOutcome::Error("no such option: -z".to_string())
        );
    }

    #[test]
    fn missing_value_is_an_error() {
        assert_eq!(
            Options::try_parse(&args(&["--startdate"]), SPECS).unwrap_err(),
            ParseOutcome::Error("--startdate option requires an argument".to_string())
        );
    }

    #[test]
    fn usage_error_exit_code_matches_optparse() {
        assert_eq!(USAGE_ERROR_EXIT_CODE, 2);
    }

    #[test]
    fn render_help_matches_optparse_layout() {
        let help = render_help("bo-db", SPECS);
        assert!(help.starts_with("Usage: bo-db [options]\n\nOptions:\n"));
        // The implicit help entry is listed first, like optparse.
        assert!(help.contains("-h, --help"));
        assert!(help.contains("show this help message and exit"));
        // Value options render as `--name=METAVAR`.
        assert!(help.contains("--startdate=STARTDATE"));
        assert!(help.contains("start date for data retrieval"));
        // Short flags render as `-v, --verbose`.
        assert!(help.contains("-d, --debug"));
        assert!(help.contains("precision of coordinates"));
    }

    #[test]
    fn parse_local_time_basic() {
        let result = parse_local_time("20250101", "1200", chrono_tz::UTC, false).unwrap();
        assert_eq!(result.format("%Y-%m-%d %H:%M:%S").to_string(), "2025-01-01 12:00:00");
    }

    #[test]
    fn parse_local_time_with_seconds() {
        let result = parse_local_time("20250101", "123045", chrono_tz::UTC, false).unwrap();
        assert_eq!(result.format("%H:%M:%S").to_string(), "12:30:45");
    }

    #[test]
    fn parse_local_time_end_time_adds_minute() {
        let result = parse_local_time("20250101", "1200", chrono_tz::UTC, true).unwrap();
        assert_eq!(result.format("%H:%M:%S").to_string(), "12:01:00");
    }

    #[test]
    fn parse_local_time_end_time_with_seconds_adds_second() {
        let result = parse_local_time("20250101", "120030", chrono_tz::UTC, true).unwrap();
        assert_eq!(result.format("%H:%M:%S").to_string(), "12:00:31");
    }

    #[test]
    fn parse_local_time_uses_timezone() {
        // 12:00 Berlin (CET, UTC+1) == 11:00 UTC in January.
        let tz = parse_timezone("Europe/Berlin").unwrap();
        let result = parse_local_time("20250101", "1200", tz, false).unwrap();
        assert_eq!(result.format("%H:%M:%S").to_string(), "11:00:00");
    }

    #[test]
    fn parse_local_time_invalid_is_none() {
        assert!(parse_local_time("bogus", "1200", chrono_tz::UTC, false).is_none());
        assert!(parse_local_time("20250101", "bogus", chrono_tz::UTC, false).is_none());
    }

    #[test]
    fn lock_excludes_second_holder() {
        let path = std::env::temp_dir().join(format!("bo-cli-lock-{}", std::process::id()));
        let mut first = LockWithTimeout::new(&path);
        assert!(first.lock(1).is_ok());

        // A second lock on the same path must time out.
        let mut second = LockWithTimeout::new(&path);
        assert!(second.lock(1).is_err());

        first.unlock();
        assert!(second.lock(1).is_ok());
        second.unlock();
        let _ = std::fs::remove_file(&path);
    }
}