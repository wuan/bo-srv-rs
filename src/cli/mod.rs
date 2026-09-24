//! Shared helpers for the Blitzortung CLI tools (`bo-db`, `bo-import`,
//! `bo-update`, `bo-import-websocket`).
//!
//! The Python tools use `optparse`; the Rust port uses [`clap`] (derive) for
//! option parsing, which provides `-h`/`--help` and `--version` automatically
//! and exits non-zero on invalid options.  This module keeps the pieces shared
//! by all four tools: time-zone handling, logging setup, `sys.exit`-style
//! helpers, the PostgreSQL connection helper and an inter-process file lock.

pub mod db_tool;
pub mod import_tool;
pub mod import_websocket_tool;
pub mod update_tool;

use std::path::PathBuf;

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

/// Render an error together with its full causal chain.
///
/// tokio-postgres' `Error` displays as the unhelpful `"db error"` for server
/// errors (`Kind::Db`); the real message lives in [`tokio_postgres::Error::as_db_error`]
/// and in the error's `source()`.  This helper walks the chain and, for every
/// tokio-postgres error encountered, appends the database `severity`, `message`,
/// `detail` and `hint` fields.
///
/// The result is a single multi-line string, e.g.:
///
/// ```text
/// error: db error
///   caused by: ERROR: relation "strikes" does not exist
///     detail: <detail>
///     hint: <hint>
/// ```
pub fn format_error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    let mut depth = 0usize;

    while let Some(err) = current {
        if depth == 0 {
            lines.push(err.to_string());
        } else {
            lines.push(format!("{}caused by: {}", "  ".repeat(depth), err));
        }

        if let Some(db_error) = err.downcast_ref::<tokio_postgres::Error>() {
            if let Some(db) = db_error.as_db_error() {
                let indent = "  ".repeat(depth + 1);
                lines.push(format!("{indent}{}: {}", db.severity(), db.message()));
                if let Some(detail) = db.detail() {
                    lines.push(format!("{indent}detail: {detail}"));
                }
                if let Some(hint) = db.hint() {
                    lines.push(format!("{indent}hint: {hint}"));
                }
            }
        }

        current = err.source();
        depth += 1;
    }

    lines.join("\n")
}

/// Convenience wrapper around [`format_error_chain`] that prefixes a context
/// label, e.g. `"error: <chain>"`.
pub fn describe_error(context: &str, error: &(dyn std::error::Error + 'static)) -> String {
    format!("{context}: {}", format_error_chain(error))
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

    /// A message-bearing leaf error, used to fake a source chain.
    #[derive(Debug)]
    struct Leaf(String);

    impl std::fmt::Display for Leaf {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    impl std::error::Error for Leaf {}

    /// A wrapper error that hides the useful message in its `source()`.
    #[derive(Debug)]
    struct Wrapper {
        leaf: Leaf,
    }

    impl std::fmt::Display for Wrapper {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            // Deliberately unhelpful, like tokio-postgres' "db error".
            write!(f, "db error")
        }
    }

    impl std::error::Error for Wrapper {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.leaf)
        }
    }

    #[test]
    fn error_chain_includes_source_messages() {
        let error = Wrapper {
            leaf: Leaf("ERROR: relation \"strikes\" does not exist".to_string()),
        };
        let rendered = format_error_chain(&error);
        assert!(rendered.starts_with("db error"), "got: {rendered}");
        assert!(
            rendered.contains("caused by: ERROR: relation \"strikes\" does not exist"),
            "got: {rendered}"
        );
        // The useless top-level message is not the only thing reported.
        assert_ne!(rendered, "db error");
    }

    #[test]
    fn describe_error_prefixes_the_context() {
        let error = Wrapper {
            leaf: Leaf("connection refused".to_string()),
        };
        let rendered = describe_error("failed to connect to database", &error);
        assert!(rendered.starts_with("failed to connect to database: db error"));
        assert!(rendered.contains("caused by: connection refused"));
    }

    #[test]
    fn error_chain_handles_sourceless_errors() {
        let error = Leaf("plain failure".to_string());
        assert_eq!(format_error_chain(&error), "plain failure");
    }

    /// A `DbError::Executor` wrapping a tokio-postgres-style source must not
    /// collapse to the bare source display.
    #[test]
    fn db_error_display_walks_the_chain() {
        let db_error = crate::db::DbError::Executor(Box::new(Wrapper {
            leaf: Leaf("ERROR: permission denied for table strikes".to_string()),
        }));
        let rendered = db_error.to_string();
        assert!(rendered.contains("caused by: ERROR: permission denied for table strikes"));
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