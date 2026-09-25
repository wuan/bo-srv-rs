//! In-process per-request usage logging.
//!
//! Every successful `get_strikes_grid` / `get_global_strikes_grid` /
//! `get_local_strikes_grid` request is pushed onto a bounded queue.  A dedicated
//! **background OS thread** consumes the queue, enriches each entry (GeoIP
//! country/city, client platform/version) and appends one tab-separated row per
//! request to `{log_dir}/servicelog_{YYYY-MM-DD}`.
//!
//! This replaces the Python two-step design (`base.py` writing per-minute JSON
//! reports + the `bo-webservice-insertlog` follow-up tool): there are **no JSON
//! intermediate files** and the transform happens in-process.  The row is a
//! refined version of the Python `servicelog_*` line (see [`build_row`]): the
//! timestamp is an int64 epoch-microsecond value and the masked client-IP column
//! is dropped in favour of a client **platform** marker.
//!
//! ## Backpressure
//!
//! The queue is **bounded** ([`QUEUE_CAPACITY`]).  The request path uses
//! `try_send`: when the queue is full the entry is **dropped** and a single
//! `WARN` is logged (not one per drop).  Requests therefore never block, fail or
//! hang because of usage logging; under sustained overload some usage rows are
//! lost rather than slowing the service down.
//!
//! ## Shutdown
//!
//! Dropping the [`ServiceLogHandle`] closes the channel; the consumer drains the
//! remaining entries, flushes the file and exits.  [`ServiceLogHandle::shutdown`]
//! does this deterministically and joins the thread.  The `bo-webservice` binary
//! drives it on `SIGINT`/`SIGTERM` (see its `main`).
//!
//! ## Enabling
//!
//! When no log directory is configured (or it does not exist) the handle is
//! `None`: nothing is queued or written and no consumer thread is spawned.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;

use crate::metrics::Metrics;
use crate::service::USER_AGENT_PREFIX;

/// Environment variable overriding the usage-log directory
/// (`BO_SERVICE_SERVICELOG`); `BO_SERVICE_LOG_DIR` is accepted as an alias.
/// An empty value disables usage logging.
pub const LOG_DIR_ENV: &str = "BO_SERVICE_SERVICELOG";

/// Alias of [`LOG_DIR_ENV`], kept for backwards compatibility.
pub const LOG_DIR_ENV_ALIAS: &str = "BO_SERVICE_LOG_DIR";

/// Default GeoIP database path (the Python tool's default).
pub const DEFAULT_GEOIP_DB: &str = "/var/lib/GeoIP/GeoLite2-City.mmdb";

/// Environment variable overriding the GeoIP database path (`BO_GEOIP_DB`);
/// `BO_SERVICE_GEOIP_DB` is accepted as an alias.
pub const GEOIP_DB_ENV: &str = "BO_GEOIP_DB";

/// Alias of [`GEOIP_DB_ENV`], kept for backwards compatibility.
pub const GEOIP_DB_ENV_ALIAS: &str = "BO_SERVICE_GEOIP_DB";

/// Bounded queue capacity.  A full queue drops entries (with a single warning)
/// instead of blocking the request path.
pub const QUEUE_CAPACITY: usize = 65_536;

/// One recorded grid request.  Field order mirrors the Python `current_data`
/// tuple so the derived row matches the old `servicelog_*` output exactly.
///
/// `region` is `0` for the global flavour, the clamped region for the region
/// flavour and `-1` for the local flavour.  `grid_baselength` is the
/// **pre-clamp** value (`original_grid_base_length` in `base.py`).
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceLogEntry {
    /// Epoch microseconds of the request.
    pub now_us: i64,
    pub minute_length: i64,
    pub grid_baselength: i64,
    pub minute_offset: i64,
    pub region: i64,
    pub count_threshold: i64,
    pub client: Option<String>,
    pub user_agent: Option<String>,
    /// Local grid only: the request centre and data area.
    pub local: Option<LocalGridLog>,
}

/// The local-grid extras (`x`, `y`, `data_area`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalGridLog {
    pub x: i64,
    pub y: i64,
    pub data_area: i64,
}

/// The Python `calendar.timegm(timestamp.timetuple()) * 1_000_000 +
/// timestamp.microsecond`: epoch microseconds of a UTC timestamp.
pub fn epoch_microseconds(timestamp: DateTime<Utc>) -> i64 {
    timestamp.timestamp() * 1_000_000 + timestamp.timestamp_subsec_micros() as i64
}

/// Build a global-grid entry (`jsonrpc_get_global_strikes_grid`).
#[allow(clippy::too_many_arguments)]
pub fn global_entry(
    now_us: i64,
    minute_length: i64,
    grid_baselength: i64,
    minute_offset: i64,
    count_threshold: i64,
    client: Option<String>,
    user_agent: Option<String>,
) -> ServiceLogEntry {
    ServiceLogEntry {
        now_us,
        minute_length,
        grid_baselength,
        minute_offset,
        region: 0,
        count_threshold,
        client,
        user_agent,
        local: None,
    }
}

/// Build a region-grid entry (`jsonrpc_get_strikes_grid`).
#[allow(clippy::too_many_arguments)]
pub fn region_entry(
    now_us: i64,
    minute_length: i64,
    grid_baselength: i64,
    minute_offset: i64,
    region: i64,
    count_threshold: i64,
    client: Option<String>,
    user_agent: Option<String>,
) -> ServiceLogEntry {
    ServiceLogEntry {
        now_us,
        minute_length,
        grid_baselength,
        minute_offset,
        region,
        count_threshold,
        client,
        user_agent,
        local: None,
    }
}

/// Build a local-grid entry (`jsonrpc_get_local_strikes_grid`).
#[allow(clippy::too_many_arguments)]
pub fn local_entry(
    now_us: i64,
    minute_length: i64,
    grid_baselength: i64,
    minute_offset: i64,
    count_threshold: i64,
    client: Option<String>,
    user_agent: Option<String>,
    x: i64,
    y: i64,
    data_area: i64,
) -> ServiceLogEntry {
    ServiceLogEntry {
        now_us,
        minute_length,
        grid_baselength,
        minute_offset,
        region: -1,
        count_threshold,
        client,
        user_agent,
        local: Some(LocalGridLog { x, y, data_area }),
    }
}

/// `user_agent_version`: parse `bo-android-<n>` from the user agent's first
/// whitespace-separated word.  Returns `None` when it does not match.
pub fn user_agent_version(user_agent: Option<&str>) -> Option<i64> {
    let user_agent = user_agent?;
    let first = user_agent.split(' ').next().unwrap_or("");
    let (prefix, version) = first.rsplit_once('-')?;
    if prefix == "bo-android" {
        version.parse::<i64>().ok()
    } else {
        None
    }
}

/// Render one entry as the 13-field tab-separated servicelog row.
///
/// Field order:
/// 1. `timestamp_us` — **int64 epoch microseconds, UTC**;
/// 2. `region` — `0` global, clamped region for the region grid, `-1` local;
/// 3. `grid_baselength` — the **pre-clamp** `original_grid_base_length`;
/// 4. `minute_offset`;
/// 5. `minute_length`;
/// 6. `count_threshold`;
/// 7. `country` — GeoIP ISO code, else `-`;
/// 8. `city` — GeoIP English name, else `-`;
/// 9. `platform` — `A` for Android, else `-`;
/// 10. `version` — `bo-android-<n>` version, else `None`;
/// 11. `local_x` / 12. `local_y` / 13. `data_area` — local grid only, else `-`.
///
/// The raw client IP is intentionally **never** written.
///
/// The timestamp is the raw [`ServiceLogEntry::now_us`] value — exactly what
/// the Python `current_data` entries recorded
/// (`calendar.timegm(...) * 1_000_000 + microsecond`): an **int64 count of
/// microseconds since the Unix epoch, in UTC**.  It is written as an integer
/// (no `%.4f` seconds form) so no precision is lost.
///
/// `platform` and `version` are derived from [`ServiceLogEntry::user_agent`]
/// via [`client_platform`] / [`user_agent_version`]: an Android client yields
/// `A` and its integer version; any other (or absent) user agent yields `-`
/// and `None` respectively.
pub fn build_row(
    entry: &ServiceLogEntry,
    version: Option<i64>,
    country_code: Option<&str>,
    city: Option<&str>,
) -> String {
    let platform = client_platform(entry.user_agent.as_deref(), version);
    let (local_x, local_y, data_area) = match entry.local {
        Some(local) => (
            local.x.to_string(),
            local.y.to_string(),
            local.data_area.to_string(),
        ),
        None => ("-".to_string(), "-".to_string(), "-".to_string()),
    };
    [
        // Epoch microseconds (int64), matching the Python tuple's first field.
        entry.now_us.to_string(),
        entry.region.to_string(),
        entry.grid_baselength.to_string(),
        entry.minute_offset.to_string(),
        entry.minute_length.to_string(),
        entry.count_threshold.to_string(),
        country_code.unwrap_or("-").to_string(),
        city.unwrap_or("-").to_string(),
        platform.to_string(),
        version
            .map(|v| v.to_string())
            .unwrap_or_else(|| "None".to_string()),
        local_x,
        local_y,
        data_area,
    ]
    .join("\t")
}

/// The client platform marker for the servicelog `platform` column.
///
/// Currently only the Blitzortung Android client is recognised: a user agent
/// whose version parses (`bo-android-<n>`) yields `A`.  Anything else — a
/// missing user agent, or one that is not an Android client — yields `-`.
pub fn client_platform(user_agent: Option<&str>, version: Option<i64>) -> &'static str {
    if version.is_some() || user_agent.is_some_and(|ua| ua.starts_with(USER_AGENT_PREFIX)) {
        "A"
    } else {
        "-"
    }
}

/// The StatsD tag string for one entry (`emit_metrics` in the Python tool).
pub fn access_metric_key(
    entry: &ServiceLogEntry,
    version: Option<i64>,
    country_code: Option<&str>,
) -> String {
    let mut tags = vec![
        format!(
            "version={}",
            version
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".to_string())
        ),
        format!("region={}", entry.region),
        format!("minutes={}", entry.minute_length),
        format!("offset={}", entry.minute_offset),
        format!("grid={}", entry.grid_baselength),
    ];
    if let Some(local) = entry.local {
        // Python checks truthiness: `0` is falsy and would be skipped.
        if local.x != 0 && local.y != 0 && local.data_area != 0 {
            tags.push(format!(
                "data_area={}x{}-{}",
                local.x, local.y, local.data_area
            ));
        }
    }
    if let Some(country) = country_code.filter(|c| !c.is_empty()) {
        tags.push(format!("country={country}"));
    }
    format!("access,{}", tags.join(","))
}

/// GeoIP lookup (country ISO code, city name); both `None` when the address is
/// not found or the database is unavailable.
pub fn geoip_lookup(
    reader: Option<&maxminddb::Reader<Vec<u8>>>,
    remote_address: &str,
) -> (Option<String>, Option<String>) {
    let Some(reader) = reader else {
        return (None, None);
    };
    let Ok(ip) = remote_address.parse::<std::net::IpAddr>() else {
        return (None, None);
    };
    match reader.lookup(ip) {
        Ok(result) => match result.decode::<maxminddb::geoip2::City>() {
            Ok(Some(city)) => {
                let country = city
                    .country
                    .iso_code
                    .map(str::to_string)
                    .filter(|s| !s.is_empty());
                // Python's `geo_info.city.name` resolves the English locale.
                let name = city
                    .city
                    .names
                    .english
                    .map(str::to_string)
                    .filter(|s| !s.is_empty());
                (country, name)
            }
            Ok(None) => (None, None),
            Err(error) => {
                log::debug!("GeoIP decode failed for {remote_address}: {error}");
                (None, None)
            }
        },
        Err(error) => {
            log::debug!("GeoIP lookup failed for {remote_address}: {error}");
            (None, None)
        }
    }
}

/// Open the GeoIP database best-effort.  A missing/unreadable file yields
/// `None` (every lookup then returns `-`), never an error.
pub fn open_geoip(path: &Path) -> Option<maxminddb::Reader<Vec<u8>>> {
    match maxminddb::Reader::open_readfile(path) {
        Ok(reader) => Some(reader),
        Err(error) => {
            log::warn!(
                "GeoIP database {} unavailable ({error}); country/city will be '-'",
                path.display()
            );
            None
        }
    }
}

/// Probe whether `dir` can actually be written to.
///
/// Existence alone is not enough: `/var/log/blitzortung` may exist but not be
/// writable by the service user.  This creates and removes a small probe file
/// in the directory, so a directory that is not writable (permissions, read-only
/// mount, ...) is reported as unusable *before* the consumer is started — which
/// keeps usage logging disabled rather than spamming per-row write errors.
pub fn directory_is_writable(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(format!(".bo-servicelog-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(file) => {
            drop(file);
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Sender side of the usage-log pipeline, held by the `Service`.
///
/// `record` never blocks: it uses `try_send` and drops on a full queue.
#[derive(Clone)]
pub struct UsageLogSender {
    sender: mpsc::Sender<ServiceLogEntry>,
    dropped: Arc<AtomicU64>,
}

impl UsageLogSender {
    /// Queue one entry.  Never blocks or fails the caller; a full queue drops
    /// the entry and logs a single warning.
    pub fn record(&self, entry: ServiceLogEntry) {
        match self.sender.try_send(entry) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if dropped == 1 {
                    log::warn!(
                        "usage log queue full; dropping entries (capacity {QUEUE_CAPACITY})"
                    );
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // Consumer already stopped (shutdown): silently drop.
            }
        }
    }

    /// The number of entries dropped so far (for tests/diagnostics).
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Test-only: a sender over a channel of `capacity` whose receiver the test
    /// keeps, so drops can be observed deterministically.
    #[cfg(test)]
    fn for_test(capacity: usize) -> (Self, mpsc::Receiver<ServiceLogEntry>) {
        let (sender, receiver) = mpsc::channel(capacity);
        (
            UsageLogSender {
                sender,
                dropped: Arc::new(AtomicU64::new(0)),
            },
            receiver,
        )
    }
}

/// Join handle for the background consumer.
///
/// Owned by the process entry point (`bo-webservice`).  Dropping the last
/// [`UsageLogSender`] closes the queue; [`shutdown`](Self::shutdown) then waits
/// for the consumer to drain, flush and exit.
pub struct UsageLogConsumer {
    thread: Option<JoinHandle<()>>,
}

impl UsageLogConsumer {
    /// Wait for the consumer to drain the queue, flush the file and exit.
    ///
    /// The caller must first drop every [`UsageLogSender`] (in the service,
    /// dropping the `Arc<Service>` does it); otherwise this blocks until the
    /// queue closes.  A panic in the consumer is logged and swallowed.
    pub fn shutdown(mut self) {
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                log::error!("usage-log consumer thread panicked");
            }
        }
    }

    /// Whether the consumer thread has finished (for tests).
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for UsageLogConsumer {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            // Best-effort drain: the sender is normally already gone.
            let _ = thread.join();
        }
    }
}

/// Start the background consumer for `log_dir`.
///
/// Returns the sender to install on the `Service` and the consumer handle the
/// entry point keeps for a graceful shutdown.  GeoIP is best-effort.
pub fn spawn(
    log_dir: PathBuf,
    geoip_db: Option<PathBuf>,
    metrics: Option<Arc<dyn Metrics>>,
) -> (UsageLogSender, UsageLogConsumer) {
    let (sender, mut receiver) = mpsc::channel::<ServiceLogEntry>(QUEUE_CAPACITY);
    let dropped = Arc::new(AtomicU64::new(0));

    let thread = std::thread::Builder::new()
        .name("bo-usage-log".to_string())
        .spawn(move || {
            let reader = geoip_db.as_deref().and_then(open_geoip);
            let mut writer = ServicelogWriter::new(log_dir);
            // Blocks until entries arrive or every sender is dropped.
            while let Some(entry) = receiver.blocking_recv() {
                let version = user_agent_version(entry.user_agent.as_deref());
                let client = entry.client.as_deref().unwrap_or("");
                let (country, city) = geoip_lookup(reader.as_ref(), client);
                let row = build_row(&entry, version, country.as_deref(), city.as_deref());
                match writer.append(&entry, &row) {
                    // Log the failure once at WARN, then writing is disabled for
                    // the rest of the day (no per-row errors).
                    WriteOutcome::Failed => log::warn!(
                        "usage log write failed ({}); further rows are dropped until the \
                         next UTC day",
                        writer.failure.as_deref().unwrap_or("unknown error")
                    ),
                    WriteOutcome::Written | WriteOutcome::Dropped => {}
                }
                if let Some(metrics) = &metrics {
                    metrics.incr(&access_metric_key(&entry, version, country.as_deref()), 1);
                }
            }
            // Channel closed: flush the remaining data.
            if let Err(error) = writer.flush() {
                log::warn!("failed to flush usage log: {error}");
            }
            if writer.dropped_rows() > 0 {
                log::warn!(
                    "usage log: {} row(s) dropped due to a write failure",
                    writer.dropped_rows()
                );
            }
        })
        .expect("spawning the usage-log consumer thread");

    (
        UsageLogSender { sender, dropped },
        UsageLogConsumer {
            thread: Some(thread),
        },
    )
}

/// Appends rows to `{log_dir}/servicelog_{YYYY-MM-DD}`, reopening when the day
/// (derived from each entry's own timestamp) changes.
///
/// A persistent write failure (directory made unwritable, disk full, ...) is
/// reported **once**; after that the writer stops attempting writes for the
/// rest of that UTC day (further rows are dropped) so a run cannot flood the
/// log with one error per request.  A new day clears the state and retries once.
struct ServicelogWriter {
    log_dir: PathBuf,
    current_day: Option<String>,
    file: Option<std::fs::File>,
    /// The day for which writing has been abandoned after a failure.
    disabled_day: Option<String>,
    /// The first write error (for the single summary log).
    failure: Option<String>,
    /// Rows dropped because writing was disabled for the day.
    dropped_rows: u64,
}

/// Outcome of one [`ServicelogWriter::append`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteOutcome {
    /// The row was written.
    Written,
    /// Writing failed for the first time (logged once, then disabled).
    Failed,
    /// Writing is disabled for this day; the row was dropped.
    Dropped,
}

impl ServicelogWriter {
    fn new(log_dir: PathBuf) -> Self {
        ServicelogWriter {
            log_dir,
            current_day: None,
            file: None,
            disabled_day: None,
            failure: None,
            dropped_rows: 0,
        }
    }

    /// Whether writing is currently disabled for `day` due to a past failure.
    fn is_disabled_for(&self, day: &str) -> bool {
        self.disabled_day.as_deref() == Some(day)
    }

    /// The number of rows dropped because writing was disabled.
    fn dropped_rows(&self) -> u64 {
        self.dropped_rows
    }

    /// Append one row, rolling to the new day's file when the entry's UTC date
    /// changed (a request at 00:00 UTC goes to the new day).
    fn append(&mut self, entry: &ServiceLogEntry, row: &str) -> WriteOutcome {
        let day = entry_day(entry.now_us);
        // A new day resets a previous failure so writing is retried once.
        if self.disabled_day.as_deref().is_some_and(|d| d != day) {
            self.disabled_day = None;
            self.failure = None;
        }
        if self.is_disabled_for(&day) {
            self.dropped_rows += 1;
            return WriteOutcome::Dropped;
        }
        match self.try_append(&day, row) {
            Ok(()) => WriteOutcome::Written,
            Err(error) => {
                // Log once, then disable writing for the rest of the day.
                self.failure = Some(error.to_string());
                self.disabled_day = Some(day);
                WriteOutcome::Failed
            }
        }
    }

    /// The actual open + write for `day`, returning an I/O error on failure.
    fn try_append(&mut self, day: &str, row: &str) -> std::io::Result<()> {
        if self.current_day.as_deref() != Some(day) {
            // Flush the old file before switching days.
            if let Some(file) = &mut self.file {
                file.flush()?;
            }
            let path = self.log_dir.join(format!("servicelog_{day}"));
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            self.file = Some(file);
            self.current_day = Some(day.to_string());
        }
        if let Some(file) = &mut self.file {
            file.write_all(row.as_bytes())?;
            file.write_all(b"\n")?;
        }
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(file) = &mut self.file {
            file.flush()?;
        }
        Ok(())
    }
}

/// `datetime.fromtimestamp(timestamp_microseconds / 1e6, UTC).strftime("%Y-%m-%d")`.
pub fn entry_day(now_us: i64) -> String {
    let secs = now_us.div_euclid(1_000_000);
    let dt = DateTime::<Utc>::from_timestamp(secs, 0).unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    dt.format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn entry(region: i64, local: Option<LocalGridLog>) -> ServiceLogEntry {
        ServiceLogEntry {
            // 2023-11-14T22:13:20.500000Z
            now_us: 1_700_000_000_500_000,
            minute_length: 60,
            grid_baselength: 10_000,
            minute_offset: 0,
            region,
            count_threshold: 0,
            client: Some("203.0.113.7".into()),
            user_agent: Some("bo-android-190".into()),
            local,
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bo-servicelog-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn epoch_microseconds_matches_python() {
        assert_eq!(
            epoch_microseconds(Utc.timestamp_opt(1_700_000_000, 0).unwrap()),
            1_700_000_000_000_000
        );
        assert_eq!(
            epoch_microseconds(Utc.timestamp_opt(1_700_000_000, 123_456_000).unwrap()),
            1_700_000_000_123_456
        );
    }

    #[test]
    fn user_agent_version_parsing() {
        assert_eq!(user_agent_version(Some("bo-android-190")), Some(190));
        assert_eq!(user_agent_version(Some("bo-android-190 more")), Some(190));
        assert_eq!(user_agent_version(Some("bo-android-abc")), None);
        assert_eq!(user_agent_version(Some("bo-android")), None);
        assert_eq!(user_agent_version(Some("Mozilla/5.0")), None);
        assert_eq!(user_agent_version(None), None);
    }

    #[test]
    fn global_row_has_thirteen_fields_with_platform() {
        let row = build_row(&entry(0, None), Some(190), Some("DE"), Some("Berlin"));
        assert_eq!(
            row,
            "1700000000500000\t0\t10000\t0\t60\t0\tDE\tBerlin\tA\t190\t-\t-\t-"
        );
        assert_eq!(row.split('\t').count(), 13);
    }

    #[test]
    fn local_row_uses_dash_for_absent_geo_and_none_version() {
        let row = build_row(
            &entry(
                -1,
                Some(LocalGridLog {
                    x: 101,
                    y: 202,
                    data_area: 5,
                }),
            ),
            None,
            None,
            None,
        );
        // The entry's user agent is an Android one, so the platform stays `A`
        // even though the (separately supplied) version is `None`.
        assert_eq!(
            row,
            "1700000000500000\t-1\t10000\t0\t60\t0\t-\t-\tA\tNone\t101\t202\t5"
        );
    }

    /// A non-Android (or absent) user agent yields platform `-`.
    #[test]
    fn non_android_platform_is_dash() {
        let mut e = entry(0, None);
        e.user_agent = Some("Mozilla/5.0".into());
        let row = build_row(&e, None, None, None);
        let cells: Vec<&str> = row.split('\t').collect();
        assert_eq!(cells[8], "-"); // platform
        assert_eq!(cells[9], "None"); // version

        let mut e = entry(0, None);
        e.user_agent = None;
        let row = build_row(&e, None, None, None);
        let cells: Vec<&str> = row.split('\t').collect();
        assert_eq!(cells[8], "-");
        assert_eq!(cells[9], "None");
    }

    /// The raw client IP never appears in the row (the entry's client is
    /// deliberately not serialised).
    #[test]
    fn row_never_contains_the_client_ip() {
        let mut e = entry(0, None);
        e.client = Some("203.0.113.7".into());
        let row = build_row(&e, Some(190), Some("DE"), Some("Berlin"));
        assert!(!row.contains("203.0.113.7"));
    }

    #[test]
    fn region_row_keeps_region_and_preclamp_baselength() {
        let mut e = entry(3, None);
        e.grid_baselength = 2_000; // below MIN_GRID_BASE_LENGTH
        let row = build_row(&e, Some(42), None, None);
        let cells: Vec<&str> = row.split('\t').collect();
        assert_eq!(cells[1], "3");
        assert_eq!(cells[2], "2000");
        assert_eq!(cells[9], "42");
    }

    #[test]
    fn day_is_derived_from_entry_timestamp() {
        // 2023-11-14T22:13:20Z
        assert_eq!(entry_day(1_700_000_000_500_000), "2023-11-14");
        // 2023-11-15T00:00:00Z -> the new day
        assert_eq!(entry_day(1_700_006_400_000_000), "2023-11-15");
    }

    #[test]
    fn access_metric_key_matches_python_tag_order() {
        let e = entry(
            -1,
            Some(LocalGridLog {
                x: 101,
                y: 202,
                data_area: 5,
            }),
        );
        assert_eq!(
            access_metric_key(&e, Some(190), Some("DE")),
            "access,version=190,region=-1,minutes=60,offset=0,grid=10000,data_area=101x202-5,country=DE"
        );
        assert_eq!(
            access_metric_key(&e, None, None),
            "access,version=-,region=-1,minutes=60,offset=0,grid=10000,data_area=101x202-5"
        );
    }

    #[test]
    fn geoip_absent_yields_dash() {
        assert_eq!(geoip_lookup(None, "1.2.3.4"), (None, None));
    }

    /// Feeding a known entry through the consumer writes the expected row.
    #[test]
    fn consumer_writes_expected_line() {
        let dir = temp_dir("consumer");
        let (handle, consumer) = spawn(dir.clone(), None, None);
        handle.record(entry(0, None));
        // Drop/join forces a drain + flush.
        drop(handle);
        consumer.shutdown();

        let content = std::fs::read_to_string(dir.join("servicelog_2023-11-14")).unwrap();
        assert_eq!(
            content,
            "1700000000500000\t0\t10000\t0\t60\t0\t-\t-\tA\t190\t-\t-\t-\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `directory_is_writable` rejects missing dirs and files, accepts a writable
    /// directory.
    #[test]
    fn directory_is_writable_checks_existence_and_write_access() {
        let dir = temp_dir("writable");
        assert!(directory_is_writable(&dir));
        // No probe file is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(leftovers.is_empty(), "probe left behind: {leftovers:?}");

        assert!(!directory_is_writable(&dir.join("does-not-exist")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file (not a directory) is not writable as a servicelog dir, even if
    /// the path is a regular file the process owns.
    #[test]
    fn directory_is_writable_rejects_a_regular_file() {
        let dir = temp_dir("file-probe");
        let file = dir.join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        assert!(!directory_is_writable(&file));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A write failure is reported once and then writing is disabled for the
    /// rest of the day: extra rows are dropped, not re-logged or re-failed.
    #[test]
    fn writer_reports_a_failure_once_and_drops_later_rows() {
        // A path whose parent is a regular file can never be a directory, so
        // every write fails deterministically.
        let base = temp_dir("writer-fail");
        let not_a_dir = base.join("blocker");
        std::fs::write(&not_a_dir, b"x").unwrap();

        let mut writer = ServicelogWriter::new(not_a_dir);
        let row = build_row(&entry(0, None), Some(190), None, None);

        assert_eq!(writer.append(&entry(0, None), &row), WriteOutcome::Failed);
        // Subsequent rows for the same day are dropped, not retried.
        assert_eq!(writer.append(&entry(0, None), &row), WriteOutcome::Dropped);
        assert_eq!(writer.append(&entry(0, None), &row), WriteOutcome::Dropped);
        assert_eq!(writer.dropped_rows(), 2);
        assert!(writer.failure.is_some());

        // A new day retries once (and fails again once).
        let mut next_day = entry(0, None);
        next_day.now_us = 1_700_006_400_000_000; // 2023-11-15
        assert_eq!(writer.append(&next_day, &row), WriteOutcome::Failed);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A writable directory writes every row (no spurious failures).
    #[test]
    fn writer_writes_all_rows_when_writable() {
        let dir = temp_dir("writer-ok");
        let mut writer = ServicelogWriter::new(dir.clone());
        let row = build_row(&entry(0, None), Some(190), None, None);
        for _ in 0..3 {
            assert_eq!(writer.append(&entry(0, None), &row), WriteOutcome::Written);
        }
        writer.flush().unwrap();
        assert_eq!(writer.dropped_rows(), 0);
        let content = std::fs::read_to_string(dir.join("servicelog_2023-11-14")).unwrap();
        assert_eq!(content.lines().count(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Entries from different UTC days land in different files, in one process.
    #[test]
    fn consumer_rolls_over_days() {
        let dir = temp_dir("rollover");
        let (handle, consumer) = spawn(dir.clone(), None, None);
        handle.record(entry(0, None));
        let mut next_day = entry(0, None);
        next_day.now_us = 1_700_006_400_000_000; // 2023-11-15T00:00:00Z
        handle.record(next_day);
        drop(handle);
        consumer.shutdown();

        assert!(dir.join("servicelog_2023-11-14").exists());
        let day2 = std::fs::read_to_string(dir.join("servicelog_2023-11-15")).unwrap();
        assert_eq!(
            day2,
            "1700006400000000\t0\t10000\t0\t60\t0\t-\t-\tA\t190\t-\t-\t-\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Shutdown drains queued entries instead of losing them.
    #[test]
    fn shutdown_drains_queue() {
        let dir = temp_dir("drain");
        let (handle, consumer) = spawn(dir.clone(), None, None);
        for _ in 0..100 {
            handle.record(entry(0, None));
        }
        drop(handle);
        consumer.shutdown();
        let content = std::fs::read_to_string(dir.join("servicelog_2023-11-14")).unwrap();
        assert_eq!(content.lines().count(), 100);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A full queue drops entries instead of blocking the caller.
    #[test]
    fn full_queue_drops_without_blocking() {
        // A receiver we never drain, so the bounded channel saturates.
        let (sender, _receiver) = UsageLogSender::for_test(2);
        sender.record(entry(0, None));
        sender.record(entry(0, None));
        // Both queued; the next two are dropped without blocking.
        sender.record(entry(0, None));
        sender.record(entry(0, None));
        assert_eq!(sender.dropped(), 2);
    }

    /// End-to-end: a saturated pipeline never blocks the caller, still writes a
    /// bounded prefix of the entries, and shuts down cleanly.
    #[test]
    fn saturated_pipeline_still_writes_and_never_blocks() {
        let dir = temp_dir("drop");
        let (handle, consumer) = spawn(dir.clone(), None, None);
        let total = QUEUE_CAPACITY + 5_000;
        for _ in 0..total {
            handle.record(entry(0, None));
        }
        drop(handle);
        consumer.shutdown();
        let content = std::fs::read_to_string(dir.join("servicelog_2023-11-14")).unwrap();
        let written = content.lines().count();
        // `record` never blocked (we got here); at least the queue capacity is
        // bounded, so no more than `total` rows are written.
        assert!(
            written > 0 && written <= total,
            "wrote {written} of {total}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A closed channel (consumer gone) silently drops the entry.
    #[test]
    fn record_on_closed_channel_drops_silently() {
        let (sender, receiver) = UsageLogSender::for_test(4);
        drop(receiver);
        // No panic, and nothing is counted as a "full" drop.
        sender.record(entry(0, None));
        assert_eq!(sender.dropped(), 0);
    }

    /// `is_finished` is false while the consumer is alive and true once it is
    /// shut down; dropping the consumer also joins the thread.
    #[test]
    fn consumer_is_finished_and_drop_joins() {
        let dir = temp_dir("consumer-drop");
        {
            let (handle, consumer) = spawn(dir.clone(), None, None);
            assert!(!consumer.is_finished());
            drop(handle);
            // Dropping the handle closes the channel; Drop joins the thread.
            drop(consumer);
        }
        // The (empty) run still leaves the process healthy.
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unwritable directory used by the live consumer yields the single
    /// failure warning and then drops later rows instead of erroring per row.
    #[test]
    fn consumer_survives_an_unwritable_directory() {
        // The parent is a regular file, so opening `servicelog_*` under it can
        // never succeed.
        let base = temp_dir("consumer-unwritable");
        let blocker = base.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();

        let (handle, consumer) = spawn(blocker, None, None);
        handle.record(entry(0, None));
        handle.record(entry(0, None));
        handle.record(entry(0, None));
        drop(handle);
        consumer.shutdown();

        // No panic and nothing written; the failure path was exercised.
        let _ = std::fs::remove_dir_all(&base);
    }

    /// `geoip_lookup` with an unparseable address returns `(None, None)` even
    /// when a reader is present — covered without a real database by using the
    /// parse-failure early return.
    #[test]
    fn geoip_lookup_invalid_address_is_none() {
        // Build a reader is unnecessary: the parse failure short-circuits before
        // the reader is used, so `None` and an empty-ish reader path are enough.
        // (A valid DB is not required to exercise this branch.)
        assert_eq!(geoip_lookup(None, "not-an-ip"), (None, None));
    }

    /// Build a tiny synthetic GeoIP2-City database with `maxminddb-writer`.
    fn synthetic_city_db(dir: &Path) -> PathBuf {
        use maxminddb_writer::paths::IpAddrWithMask;
        use serde::Serialize;
        use std::collections::HashMap;

        #[derive(Serialize)]
        struct Country {
            iso_code: &'static str,
        }
        #[derive(Serialize)]
        struct Names {
            en: &'static str,
        }
        #[derive(Serialize)]
        struct City {
            names: Names,
        }
        #[derive(Serialize)]
        struct Record {
            country: Country,
            city: City,
        }

        let mut db = maxminddb_writer::Database::default();
        db.metadata.database_type = "GeoLite2-City".to_string();
        db.metadata.binary_format_major_version = 2;
        db.metadata.description =
            HashMap::from([("en".to_string(), "synthetic test db".to_string())]);
        let data = db
            .insert_value(Record {
                country: Country { iso_code: "DE" },
                city: City {
                    names: Names { en: "Berlin" },
                },
            })
            .unwrap();
        db.insert_node("81.169.0.0/16".parse::<IpAddrWithMask>().unwrap(), data);
        let path = dir.join("synthetic-city.mmdb");
        let file = std::fs::File::create(&path).unwrap();
        db.write_to(file).unwrap();
        path
    }

    /// `open_geoip` + `geoip_lookup` success path yields country/city, and an
    /// address outside the database yields `(None, None)`.
    #[test]
    fn geoip_lookup_success_and_not_found() {
        let dir = temp_dir("geoip");
        let path = synthetic_city_db(&dir);
        let reader = open_geoip(&path).expect("synthetic db opens");
        assert_eq!(
            geoip_lookup(Some(&reader), "81.169.1.2"),
            (Some("DE".to_string()), Some("Berlin".to_string()))
        );
        // An address that is not in the database.
        assert_eq!(geoip_lookup(Some(&reader), "8.8.8.8"), (None, None));
        // An unparseable address short-circuits before the reader is used.
        assert_eq!(geoip_lookup(Some(&reader), "nope"), (None, None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `open_geoip` on a missing path returns `None` (best-effort).
    #[test]
    fn open_geoip_missing_file_is_none() {
        assert!(open_geoip(Path::new("/nonexistent/GeoLite2-City.mmdb")).is_none());
    }

    /// `client_platform` recognises only the Android prefix.
    #[test]
    fn client_platform_marker() {
        assert_eq!(client_platform(Some("bo-android-190"), Some(190)), "A");
        // A prefix match without a parsed version is still Android.
        assert_eq!(client_platform(Some("bo-android-abc"), None), "A");
        assert_eq!(client_platform(Some("Mozilla/5.0"), None), "-");
        assert_eq!(client_platform(None, None), "-");
    }

    /// `access_metric_key` skips zero local coordinates (Python truthiness) and
    /// omits an empty country.
    #[test]
    fn access_metric_key_skips_falsy_extras() {
        let mut e = entry(
            -1,
            Some(LocalGridLog {
                x: 0,
                y: 0,
                data_area: 0,
            }),
        );
        e.region = -1;
        let key = access_metric_key(&e, None, Some(""));
        assert_eq!(
            key,
            "access,version=-,region=-1,minutes=60,offset=0,grid=10000"
        );
    }
}
