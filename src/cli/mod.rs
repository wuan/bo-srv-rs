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

/// A parsed command line: long/short options plus positional arguments.
#[derive(Debug, Default)]
pub struct Options {
    values: HashMap<String, String>,
    flags: HashMap<String, bool>,
    positional: Vec<String>,
}

impl Options {
    /// Parse `args` against a list of specs.
    ///
    /// Each spec is `(long_name, short_name, takes_value)`.  `--long value`,
    /// `--long=value`, `-s value` and `-s=value` are accepted; flags may be
    /// repeated but the last occurrence wins.
    pub fn parse(args: &[String], specs: &[(&str, &str, bool)]) -> Self {
        let mut options = Options::default();
        let mut index = 0;
        while index < args.len() {
            let arg = &args[index];
            if arg == "--" {
                options.positional.extend_from_slice(&args[index + 1..]);
                break;
            } else if let Some(rest) = arg.strip_prefix("--") {
                let (name, inline_value) = match rest.split_once('=') {
                    Some((name, value)) => (name, Some(value.to_string())),
                    None => (rest, None),
                };
                if let Some((long, _, takes_value)) = specs.iter().find(|(long, _, _)| *long == name) {
                    options.apply(long, *takes_value, inline_value, args, &mut index);
                } else {
                    eprintln!("unknown option: --{name}");
                }
            } else if let Some(rest) = arg.strip_prefix('-') {
                if rest.is_empty() {
                    options.positional.push(arg.clone());
                } else {
                    let (name, inline_value) = match rest.split_once('=') {
                        Some((name, value)) => (name, Some(value.to_string())),
                        None => (rest, None),
                    };
                    if let Some((long, _, takes_value)) =
                        specs.iter().find(|(_, short, _)| *short == name)
                    {
                        options.apply(long, *takes_value, inline_value, args, &mut index);
                    } else {
                        eprintln!("unknown option: -{name}");
                    }
                }
            } else {
                options.positional.push(arg.clone());
            }
            index += 1;
        }
        options
    }

    fn apply(
        &mut self,
        long: &str,
        takes_value: bool,
        inline_value: Option<String>,
        args: &[String],
        index: &mut usize,
    ) {
        if takes_value {
            let value = inline_value.or_else(|| {
                if *index + 1 < args.len() {
                    *index += 1;
                    Some(args[*index].clone())
                } else {
                    eprintln!("missing value for option --{long}");
                    None
                }
            });
            if let Some(value) = value {
                self.values.insert(long.to_string(), value);
            }
        } else {
            self.flags.insert(long.to_string(), true);
        }
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

    const SPECS: &[(&str, &str, bool)] = &[
        ("startdate", "", true),
        ("verbose", "v", false),
        ("debug", "d", false),
        ("precision", "", true),
    ];

    #[test]
    fn parses_long_and_short_options() {
        let options = Options::parse(
            &args(&["--startdate", "20250101", "-v", "--precision=2"]),
            SPECS,
        );
        assert_eq!(options.value("startdate"), Some("20250101"));
        assert!(options.flag("verbose"));
        assert_eq!(options.parse_or("precision", 4), 2);
        assert!(!options.flag("debug"));
    }

    #[test]
    fn parses_short_equals() {
        let options = Options::parse(&args(&["-d"]), SPECS);
        assert!(options.flag("debug"));
    }

    #[test]
    fn defaults_are_used_when_absent() {
        let options = Options::parse(&args(&[]), SPECS);
        assert_eq!(options.parse_or("precision", 4), 4);
        assert_eq!(options.value("startdate"), None);
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