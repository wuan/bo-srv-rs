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

/// Build a StatsD metrics sink for the importer CLIs.
///
/// The Python importers (`cli/imprt.py`, `cli/imprt_websocket.py`,
/// `cli/update.py`) each open a `statsd.StatsClient('localhost', 8125,
/// prefix='org.blitzortung.import')`.  This mirrors that: the receiver comes
/// from the `[statsd]`/`BO_STATSD_*` configuration (`host`, `port`) but always
/// uses the importer prefix unless one is configured explicitly.  When the
/// socket cannot be set up the tools fall back to [`NoopMetrics`] so a missing
/// metrics daemon can never prevent an import.
///
/// [`NoopMetrics`]: crate::metrics::NoopMetrics
pub fn build_import_metrics(
    config: &crate::config::Config,
) -> std::sync::Arc<dyn crate::metrics::Metrics> {
    use crate::metrics::{NoopMetrics, StatsDMetrics, IMPORT_STATSD_PREFIX};

    let (host, port) = config.statsd_address();
    // The importer prefix is the default; an explicit `[statsd] prefix`/env
    // override (i.e. anything other than the service default) is honoured.
    let prefix = if config.statsd_prefix == crate::metrics::DEFAULT_STATSD_PREFIX {
        IMPORT_STATSD_PREFIX
    } else {
        &config.statsd_prefix
    };

    match StatsDMetrics::with_address_and_prefix(host, port, prefix) {
        Ok(metrics) => {
            log::info!(
                "sending StatsD metrics to {} with prefix {:?}",
                metrics.target(),
                prefix
            );
            std::sync::Arc::new(metrics)
        }
        Err(error) => {
            log::warn!(
                "StatsD metrics disabled: could not set up a sender for {host}:{port}: {error}"
            );
            std::sync::Arc::new(NoopMetrics)
        }
    }
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
    init_logging_with_default(default);
}

/// Initialise the `log` facade, using `default_filter` when `RUST_LOG` is unset
/// (e.g. `"info"` for the webservice, `"warn"` for the CLI tools).
///
/// On Linux, when the process is directly connected to the systemd journal
/// (i.e. it runs as a systemd service), records are written as structured
/// journal entries rather than formatted text.  journald then owns the
/// timestamp, PID and priority, which avoids the duplicate
/// `[timestamp LEVEL target]` header [`env_logger`] would otherwise add on top
/// of journald's own `Sep 25 ... bo-webservice[pid]:` prefix.  Everywhere else
/// (interactive shells, the macOS dev machine) logging falls back to
/// [`env_logger`] on stderr.
pub fn init_logging_with_default(default_filter: &str) {
    #[cfg(target_os = "linux")]
    if init_journald_logging(default_filter) {
        return;
    }

    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(default_filter),
    )
    .try_init();
}

/// Try to install the systemd journal logger, returning `true` on success.
///
/// Returns `false` when the process is not connected to the journal or a logger
/// is already installed, so the caller can fall back to [`env_logger`].
#[cfg(target_os = "linux")]
fn init_journald_logging(default_filter: &str) -> bool {
    if !systemd_journal_logger::connected_to_journal() {
        return false;
    }

    let mut builder = env_filter::Builder::new();
    let directives = std::env::var("RUST_LOG").unwrap_or_else(|_| default_filter.to_string());
    builder.parse(&directives);
    let filter = builder.build();
    let max_level = filter.filter();

    let sink = match systemd_journal_logger::JournalLog::new() {
        Ok(sink) => sink,
        Err(error) => {
            eprintln!(
                "{error}: could not connect to the systemd journal, falling back to stderr logging"
            );
            return false;
        }
    };

    if log::set_boxed_logger(Box::new(FilteredJournalLog { filter, sink })).is_err() {
        return false;
    }
    log::set_max_level(max_level);
    true
}

/// A [`log`] adapter that applies the `RUST_LOG` filter before forwarding to
/// journald: [`JournalLog`](systemd_journal_logger::JournalLog) performs no
/// level filtering itself, so without this every record down to `trace` would
/// be written.
#[cfg(target_os = "linux")]
struct FilteredJournalLog {
    filter: env_filter::Filter,
    sink: systemd_journal_logger::JournalLog,
}

#[cfg(target_os = "linux")]
impl log::Log for FilteredJournalLog {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        self.filter.enabled(metadata)
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.filter.matches(record) {
            self.sink.log(record);
        }
    }

    fn flush(&self) {}
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
    use std::error::Error as _;

    let mut lines: Vec<String> = Vec::new();
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    // How many "caused by" levels have been emitted (drives the indentation).
    let mut caused = 0usize;

    let push_cause = |lines: &mut Vec<String>, caused: &mut usize, message: String| {
        if lines.is_empty() {
            lines.push(message);
        } else {
            *caused += 1;
            lines.push(format!("{}caused by: {message}", "  ".repeat(*caused)));
        }
    };

    while let Some(err) = current {
        // tokio-postgres renders a server error as the bare, useless
        // "db error"; the real message lives in the `DbError` payload, which
        // is also the error's own `source()`.  Emit the payload once (with
        // severity/detail/hint) and skip the payload as a chain entry so it is
        // not printed again as "caused by".
        if let Some(db_error) = err.downcast_ref::<tokio_postgres::Error>() {
            if let Some(db) = db_error.as_db_error() {
                push_cause(
                    &mut lines,
                    &mut caused,
                    format!("{}: {}", db.severity(), db.message()),
                );
                let detail_indent = "  ".repeat(caused + 1);
                if let Some(detail) = db.detail() {
                    lines.push(format!("{detail_indent}detail: {detail}"));
                }
                if let Some(hint) = db.hint() {
                    lines.push(format!("{detail_indent}hint: {hint}"));
                }
                // Its `source()` is the same `DbError`; skip past it.
                current = db_error.source().and_then(|payload| payload.source());
                continue;
            }
        }

        let message = err.to_string();
        // Drop the information-free tokio sentinel / wrapper so it does not add
        // a redundant "db error" line.
        if !lines.is_empty() && message == "db error" {
            current = err.source();
            continue;
        }

        push_cause(&mut lines, &mut caused, message);
        current = err.source();
    }

    if lines.is_empty() {
        lines.push(error.to_string());
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
    let fmt = if has_seconds { "%H%M%S" } else { TIME_FORMAT };
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

    /// `DbError`'s own Display is concise (the chain is expanded exactly once by
    /// `format_error_chain`, avoiding a duplicated "db error").
    #[test]
    fn db_error_display_is_concise_and_chain_is_expanded_once() {
        let db_error = crate::db::DbError::Executor(Box::new(Wrapper {
            leaf: Leaf("ERROR: permission denied for table strikes".to_string()),
        }));
        // The Display is just the inner error's display...
        assert_eq!(db_error.to_string(), "db error");
        // ...and the chain walker adds the useful cause exactly once.
        let rendered = format_error_chain(&db_error);
        assert_eq!(
            rendered
                .matches("permission denied for table strikes")
                .count(),
            1,
            "cause must not be duplicated: {rendered}"
        );
        assert!(!rendered.contains("caused by: db error"));
    }

    /// A tokio-postgres-style error whose message is the bare "db error"
    /// sentinel must not be printed twice.
    #[test]
    fn error_chain_does_not_repeat_the_db_sentinel() {
        let error = Wrapper {
            leaf: Leaf("ERROR: relation \"strikes\" does not exist".to_string()),
        };
        let rendered = format_error_chain(&error);
        assert_eq!(rendered.matches("db error").count(), 1, "got: {rendered}");
        assert_eq!(
            rendered
                .matches("relation \"strikes\" does not exist")
                .count(),
            1,
            "got: {rendered}"
        );
    }

    #[test]
    fn parse_local_time_basic() {
        let result = parse_local_time("20250101", "1200", chrono_tz::UTC, false).unwrap();
        assert_eq!(
            result.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2025-01-01 12:00:00"
        );
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

    /// The importer sink uses `org.blitzortung.import` by default and honours
    /// an explicitly configured prefix.
    #[test]
    fn build_import_metrics_uses_import_prefix_by_default() {
        let receiver = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        receiver
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let port = receiver.local_addr().unwrap().port();

        let config = crate::config::Config {
            statsd_host: "127.0.0.1".into(),
            statsd_port: port,
            ..crate::config::Config::default()
        };
        let metrics = build_import_metrics(&config);
        metrics.for_update_imported(3);

        let mut buffer = [0u8; 512];
        let (len, _) = receiver.recv_from(&mut buffer).unwrap();
        assert_eq!(
            String::from_utf8(buffer[..len].to_vec()).unwrap(),
            "org.blitzortung.import.strikes.imported:3|g"
        );
    }

    /// An explicitly configured `[statsd] prefix` is passed through unchanged.
    #[test]
    fn build_import_metrics_honours_configured_prefix() {
        let receiver = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        receiver
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let port = receiver.local_addr().unwrap().port();

        let config = crate::config::Config {
            statsd_host: "127.0.0.1".into(),
            statsd_port: port,
            statsd_prefix: "my.import".into(),
            ..crate::config::Config::default()
        };
        let metrics = build_import_metrics(&config);
        metrics.for_update_imported(3);

        let mut buffer = [0u8; 512];
        let (len, _) = receiver.recv_from(&mut buffer).unwrap();
        assert_eq!(
            String::from_utf8(buffer[..len].to_vec()).unwrap(),
            "my.import.strikes.imported:3|g"
        );
    }

    /// A bad receiver host disables metrics instead of failing the import.
    #[test]
    fn build_import_metrics_falls_back_to_noop_on_bad_host() {
        let config = crate::config::Config {
            statsd_host: "invalid.invalid.invalid".into(),
            statsd_port: 8125,
            ..crate::config::Config::default()
        };
        let metrics = build_import_metrics(&config);
        // The no-op sink silently accepts everything.
        metrics.for_import(1, 0, 0.0, 0.0);
    }
}
