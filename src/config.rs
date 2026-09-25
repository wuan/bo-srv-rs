//! Runtime configuration.
//!
//! Mirrors the Python layer's `blitzortung/config.py`: a `blitzortung.conf`
//! INI file with `[webservice] port` and `[db] host/port/dbname/username/
//! password/connection_count` sections, searched in `.` then `/etc/`
//! (`ConfigModule.find_config_file_path`).  A legacy YAML `config.yml` is
//! **not** read.
//!
//! When no configuration file is found, [`Config::from_env`] prints a
//! prominent warning naming the searched paths and the built-in defaults in
//! use, and the [`ConfigDiagnostics`] returned by
//! [`Config::from_env_with_diagnostics`] record which file (if any) was
//! loaded.
//!
//! Environment variables supplement/override the file (kept for dev
//! convenience; they are more explicit than the INI):
//!
//! * `BO_CONFIG` — explicit path to the INI file (bypasses the search)
//! * `BO_SERVICE_PORT` (default `8080`)
//! * `BO_SERVICE_PROTOCOL` (`http`|`lsp`, default `http`) — see [`Protocol`]
//! * `BO_DB_HOST` (default `localhost`)
//! * `BO_DB_PORT` (default `5432`)
//! * `BO_DB_NAME` (default `blitzortung`)
//! * `BO_DB_USER` (default `blitzortung`)
//! * `BO_DB_PASSWORD` (default `""`)
//! * `BO_DB_CONNECTION_COUNT` (default `3`)
//! * `BO_BLITZORTUNG_USERNAME` (default `""`) — `[auth] username`
//! * `BO_BLITZORTUNG_PASSWORD` (default `""`) — `[auth] password`
//! * `BO_STATSD_HOST` (default `localhost`) — `[statsd] host`
//! * `BO_STATSD_PORT` (default `8125`) — `[statsd] port`
//! * `BO_STATSD_PREFIX` (default `org.blitzortung.service`) — `[statsd] prefix`

/// Wire protocol the service speaks.
///
/// The deployed service sits behind an Nginx `proxy_pass`, which requires real
/// HTTP; [`Protocol::Http`] is therefore the default.  [`Protocol::Lsp`] keeps
/// the original LSP-style `Content-Length` framing available for other
/// consumers/tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
    /// HTTP/1.1 (`txjsonrpc_ng`-compatible: `POST /`, `GET ?request=`).
    #[default]
    Http,
    /// LSP-style `Content-Length` framing on a raw TCP socket.
    Lsp,
}

impl Protocol {
    /// Parse a protocol name (`http`/`https`/`lsp`/`netstring`/`tcp`).
    pub fn parse(value: &str) -> Option<Protocol> {
        match value.trim().to_ascii_lowercase().as_str() {
            "http" | "https" => Some(Protocol::Http),
            "lsp" | "netstring" | "tcp" => Some(Protocol::Lsp),
            _ => None,
        }
    }

    /// The canonical lower-case name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Protocol::Http => "http",
            Protocol::Lsp => "lsp",
        }
    }
}

/// Parsed service configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub port: u16,
    /// Wire protocol (`[webservice] protocol` / `BO_SERVICE_PROTOCOL`),
    /// default [`Protocol::Http`].
    pub protocol: Protocol,
    pub db_host: String,
    pub db_port: String,
    pub db_name: String,
    pub db_user: String,
    pub db_password: String,
    /// `Config.get_db_connection_count`, default 3 (the txpostgres default).
    /// Sizes the database connection pool (`src/postgres.rs`).
    pub db_connection_count: u32,
    /// HTTP basic-auth username for the protected Blitzortung data feeds
    /// (`Config.get_username`, `[auth] username`).
    pub auth_username: String,
    /// HTTP basic-auth password for the protected Blitzortung data feeds
    /// (`Config.get_password`, `[auth] password`).
    pub auth_password: String,
    /// StatsD receiver host (`[statsd] host` / `BO_STATSD_HOST`), default
    /// `localhost` (the Python `StatsClient('localhost', 8125)`).
    pub statsd_host: String,
    /// StatsD receiver UDP port (`[statsd] port` / `BO_STATSD_PORT`), default
    /// `8125`.
    pub statsd_port: u16,
    /// StatsD metric name prefix (`[statsd] prefix` / `BO_STATSD_PREFIX`),
    /// default `org.blitzortung.service`.
    pub statsd_prefix: String,
    /// Usage-log directory (`[webservice] servicelog` /
    /// `BO_SERVICE_SERVICELOG`; `log_directory` / `BO_SERVICE_LOG_DIR` are
    /// accepted as aliases); `None` disables per-request usage logging.
    /// The binary validates existence with
    /// [`Config::service_log_directory`], which falls back to
    /// `/var/log/blitzortung` when it exists.
    pub service_log_dir: Option<String>,
    /// GeoIP database for the usage-log consumer (`[webservice] geoip_db` /
    /// `BO_GEOIP_DB`; `BO_SERVICE_GEOIP_DB` is accepted as an alias); defaults
    /// to [`service_log::DEFAULT_GEOIP_DB`](crate::service_log::DEFAULT_GEOIP_DB).
    /// Best-effort: a missing/unreadable file yields `-` for country/city.
    pub service_geoip_db: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            port: 8080,
            protocol: Protocol::Http,
            db_host: "localhost".into(),
            db_port: "5432".into(),
            db_name: "blitzortung".into(),
            db_user: "blitzortung".into(),
            db_password: String::new(),
            db_connection_count: 3,
            auth_username: String::new(),
            auth_password: String::new(),
            statsd_host: "localhost".into(),
            statsd_port: 8125,
            statsd_prefix: "org.blitzortung.service".into(),
            service_log_dir: None,
            service_geoip_db: None,
        }
    }
}

/// Where a [`Config`] was loaded from (used for diagnostics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDiagnostics {
    /// The INI file that was actually read, if any.
    pub loaded_path: Option<String>,
    /// `true` when an explicit `BO_CONFIG` path was requested but could not be
    /// read (the caller is using built-in defaults instead).
    pub explicit_but_missing: bool,
    /// The default search locations, in order.
    pub searched_paths: Vec<String>,
}

impl ConfigDiagnostics {
    /// Whether no configuration file was found at all (the tool will run with
    /// built-in defaults).
    pub fn is_missing(&self) -> bool {
        self.loaded_path.is_none()
    }

    /// A human-readable warning describing that no config file was found and
    /// which defaults are in use.
    pub fn missing_warning(&self) -> String {
        let searched = self.searched_paths.join(", ");
        format!(
            "no blitzortung configuration file found (searched: {searched}); \
             using built-in defaults.  Set BO_CONFIG or create blitzortung.conf \
             with a [db] section.  Note: the legacy YAML config.yml is NOT read."
        )
    }
}

impl Config {
    /// Load configuration from the environment (and the INI file).
    ///
    /// Emits a prominent warning on stderr when no configuration file is found
    /// (the tools then fall back to built-in defaults), matching the Python
    /// `ConfigModule` which raises `No configuration file found`.
    pub fn from_env() -> Self {
        let (config, diagnostics) = Self::from_env_with_diagnostics(|k| std::env::var(k).ok());
        if let Some(path) = &diagnostics.loaded_path {
            log::debug!("loaded configuration from {path}");
        } else if diagnostics.explicit_but_missing {
            eprintln!(
                "warning: BO_CONFIG was set but the file could not be read; \
                 using built-in defaults"
            );
        } else {
            // Always visible (not gated on the logger being initialised).
            eprintln!("warning: {}", diagnostics.missing_warning());
        }
        config
    }

    /// Testable core: `lookup` returns the value of an env var by name.
    ///
    /// The INI file is resolved the way the Python `ConfigModule` does it:
    /// an explicit `BO_CONFIG` path first, otherwise `./blitzortung.conf` and
    /// `/etc/blitzortung.conf`.  Flat env vars then override the INI.
    pub fn from_env_with(lookup: impl Fn(&str) -> Option<String>) -> Self {
        Self::from_env_with_diagnostics(lookup).0
    }

    /// Like [`Config::from_env_with`] but also returns where the configuration
    /// was loaded from (for diagnostics/tests).  Does not print anything.
    pub fn from_env_with_diagnostics(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> (Self, ConfigDiagnostics) {
        let mut config = Config::default();

        let explicit = lookup("BO_CONFIG").filter(|path| !path.is_empty());
        let searched_paths: Vec<String> = default_config_paths();
        let ini_path = explicit.clone().or_else(find_blitzortung_conf);
        let mut diagnostics = ConfigDiagnostics {
            loaded_path: None,
            explicit_but_missing: false,
            searched_paths,
        };

        if let Some(ini_path) = ini_path {
            if let Some(ini) = read_ini(&ini_path) {
                diagnostics.loaded_path = Some(ini_path);
                if let Some(section) = ini.get("webservice") {
                    if let Some(port) = section.get("port") {
                        if let Ok(port) = port.parse::<u16>() {
                            config.port = port;
                        }
                    }
                    if let Some(protocol) = section.get("protocol") {
                        if let Some(protocol) = Protocol::parse(protocol) {
                            config.protocol = protocol;
                        }
                    }
                    // `servicelog` is the documented key; `log_directory` is kept as an
                    // alias for backwards compatibility with earlier docs.
                    if let Some(dir) = section
                        .get("servicelog")
                        .or_else(|| section.get("log_directory"))
                    {
                        config.service_log_dir = if dir.is_empty() {
                            None
                        } else {
                            Some(dir.clone())
                        };
                    }
                    if let Some(db) = section.get("geoip_db") {
                        config.service_geoip_db = if db.is_empty() {
                            None
                        } else {
                            Some(db.clone())
                        };
                    }
                }
                if let Some(section) = ini.get("db") {
                    config.db_host = section.get("host").cloned().unwrap_or(config.db_host);
                    config.db_port = section.get("port").cloned().unwrap_or(config.db_port);
                    config.db_name = section.get("dbname").cloned().unwrap_or(config.db_name);
                    config.db_user = section.get("username").cloned().unwrap_or(config.db_user);
                    config.db_password = section
                        .get("password")
                        .cloned()
                        .unwrap_or(config.db_password);
                    if let Some(count) = section.get("connection_count") {
                        if let Ok(count) = count.parse::<u32>() {
                            config.db_connection_count = count;
                        }
                    }
                }
                if let Some(section) = ini.get("auth") {
                    config.auth_username = section
                        .get("username")
                        .cloned()
                        .unwrap_or(config.auth_username);
                    config.auth_password = section
                        .get("password")
                        .cloned()
                        .unwrap_or(config.auth_password);
                }
                if let Some(section) = ini.get("statsd") {
                    config.statsd_host = section.get("host").cloned().unwrap_or(config.statsd_host);
                    if let Some(port) = section.get("port") {
                        if let Ok(port) = port.parse::<u16>() {
                            config.statsd_port = port;
                        }
                    }
                    config.statsd_prefix = section
                        .get("prefix")
                        .cloned()
                        .unwrap_or(config.statsd_prefix);
                }
            }
        }

        // An explicit `BO_CONFIG` that could not be read leaves us on the
        // built-in defaults; surface that separately from the search case.
        diagnostics.explicit_but_missing = explicit.is_some() && diagnostics.loaded_path.is_none();

        if let Some(v) = lookup("BO_SERVICE_PORT") {
            if let Ok(port) = v.parse::<u16>() {
                config.port = port;
            }
        }
        if let Some(v) = lookup("BO_SERVICE_PROTOCOL") {
            if let Some(protocol) = Protocol::parse(&v) {
                config.protocol = protocol;
            }
        }
        if let Some(v) = lookup("BO_DB_HOST") {
            config.db_host = v;
        }
        if let Some(v) = lookup("BO_DB_PORT") {
            config.db_port = v;
        }
        if let Some(v) = lookup("BO_DB_NAME") {
            config.db_name = v;
        }
        if let Some(v) = lookup("BO_DB_USER") {
            config.db_user = v;
        }
        if let Some(v) = lookup("BO_DB_PASSWORD") {
            config.db_password = v;
        }
        if let Some(v) = lookup("BO_DB_CONNECTION_COUNT") {
            if let Ok(count) = v.parse::<u32>() {
                config.db_connection_count = count;
            }
        }
        if let Some(v) = lookup("BO_BLITZORTUNG_USERNAME") {
            config.auth_username = v;
        }
        if let Some(v) = lookup("BO_BLITZORTUNG_PASSWORD") {
            config.auth_password = v;
        }
        if let Some(v) = lookup("BO_STATSD_HOST") {
            config.statsd_host = v;
        }
        if let Some(v) = lookup("BO_STATSD_PORT") {
            if let Ok(port) = v.parse::<u16>() {
                config.statsd_port = port;
            }
        }
        if let Some(v) = lookup("BO_STATSD_PREFIX") {
            config.statsd_prefix = v;
        }

        // Usage-log directory: `BO_SERVICE_SERVICELOG` (documented) with
        // `BO_SERVICE_LOG_DIR` kept as an alias; an empty value disables it.
        use crate::service_log::{
            GEOIP_DB_ENV, GEOIP_DB_ENV_ALIAS, LOG_DIR_ENV, LOG_DIR_ENV_ALIAS,
        };
        if let Some(v) = lookup(LOG_DIR_ENV).or_else(|| lookup(LOG_DIR_ENV_ALIAS)) {
            config.service_log_dir = if v.is_empty() { None } else { Some(v) };
        }
        // GeoIP db: `BO_GEOIP_DB` (documented) with `BO_SERVICE_GEOIP_DB` as alias.
        if let Some(v) = lookup(GEOIP_DB_ENV).or_else(|| lookup(GEOIP_DB_ENV_ALIAS)) {
            config.service_geoip_db = if v.is_empty() { None } else { Some(v) };
        }

        (config, diagnostics)
    }

    /// `Config.get_username`: the HTTP basic-auth username from `[auth]`.
    pub fn username(&self) -> &str {
        &self.auth_username
    }

    /// `Config.get_password`: the HTTP basic-auth password from `[auth]`.
    pub fn password(&self) -> &str {
        &self.auth_password
    }

    /// The StatsD receiver `host`/`port` the service should send metrics to
    /// (`[statsd] host`/`port`, default `localhost:8125`).
    pub fn statsd_address(&self) -> (&str, u16) {
        (&self.statsd_host, self.statsd_port)
    }

    /// The effective usage-log directory.
    ///
    /// Usage logging is **off unless explicitly configured** via `--servicelog`
    /// (CLI), `BO_SERVICE_SERVICELOG`/`BO_SERVICE_LOG_DIR` (env) or
    /// `[webservice] servicelog`/`log_directory` (INI).  There is **no implicit
    /// fallback** to `/var/log/blitzortung`: with nothing configured this
    /// returns `None` and no consumer/writer is created.
    ///
    /// A configured directory must exist **and** be writable by the process
    /// (see [`directory_is_writable`](crate::service_log::directory_is_writable));
    /// otherwise `None` is returned and usage logging is disabled.  Existence
    /// alone is not enough — a directory may exist but not be writable by the
    /// service user, which must not enable a consumer that then fails on every
    /// row.
    pub fn service_log_directory(&self) -> Option<std::path::PathBuf> {
        use crate::service_log::directory_is_writable;

        self.configured_service_log_directory()
            .filter(|path| directory_is_writable(path))
    }

    /// The **explicitly configured** usage-log directory, without checking
    /// existence or writability.  `None` when nothing was configured.
    ///
    /// Used to distinguish "disabled because nothing is configured" (silent)
    /// from "a path was configured but is unusable" (one startup warning).
    pub fn configured_service_log_directory(&self) -> Option<std::path::PathBuf> {
        self.service_log_dir.as_ref().map(std::path::PathBuf::from)
    }

    /// The GeoIP database path for the usage-log consumer: the configured
    /// `service_geoip_db`, else
    /// [`DEFAULT_GEOIP_DB`](crate::service_log::DEFAULT_GEOIP_DB).
    pub fn service_geoip_db(&self) -> std::path::PathBuf {
        match &self.service_geoip_db {
            Some(path) => std::path::PathBuf::from(path),
            None => std::path::PathBuf::from(crate::service_log::DEFAULT_GEOIP_DB),
        }
    }

    /// The PostgreSQL connection string used by tokio-postgres, built like
    /// `Config.get_db_connection_string`: every field is present (including
    /// `port` and an empty `password`) and each value is escaped the way
    /// `psycopg2.extensions.make_dsn` escapes it.
    pub fn db_connection_string(&self) -> String {
        [
            ("host", &self.db_host),
            ("port", &self.db_port),
            ("dbname", &self.db_name),
            ("user", &self.db_user),
            ("password", &self.db_password),
        ]
        .iter()
        .map(|(key, value)| format!("{key}={}", quote_conninfo_value(value)))
        .collect::<Vec<_>>()
        .join(" ")
    }
}

/// `config.py._quote_conninfo_value`: escape backslashes and single quotes,
/// then quote the whole value when it is empty or contains whitespace (the
/// escaping performed by `psycopg2.extensions.make_dsn`).
fn quote_conninfo_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '\'' => escaped.push_str("\\'"),
            _ => escaped.push(ch),
        }
    }
    if value.is_empty() || value.chars().any(|ch| ch.is_whitespace()) {
        format!("'{escaped}'")
    } else {
        escaped
    }
}

/// The search order of `ConfigModule.find_config_file_path`: `.` then `/etc/`.
pub fn default_config_paths() -> Vec<String> {
    [".", "/etc/"]
        .iter()
        .map(|dir| {
            std::path::Path::new(dir)
                .join("blitzortung.conf")
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

/// The first existing `blitzortung.conf` in [`default_config_paths`].
fn find_blitzortung_conf() -> Option<String> {
    default_config_paths()
        .into_iter()
        .find(|path| std::path::Path::new(path).exists())
}

/// Read a simple INI file into a map of section name -> key/value pairs.
fn read_ini(
    path: &str,
) -> Option<std::collections::HashMap<String, std::collections::HashMap<String, String>>> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut sections: std::collections::HashMap<String, std::collections::HashMap<String, String>> =
        std::collections::HashMap::new();
    let mut current: Option<String> = None;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            current = Some(line[1..line.len() - 1].trim().to_string());
            continue;
        }
        if let Some(section) = &current {
            if let Some((key, value)) = line.split_once('=') {
                sections
                    .entry(section.clone())
                    .or_default()
                    .insert(key.trim().to_string(), value.trim().to_string());
            }
        }
    }
    Some(sections)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ini_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bo-config-test-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn env_only_config() {
        let config = Config::from_env_with(|k| match k {
            "BO_SERVICE_PORT" => Some("1234".into()),
            "BO_DB_HOST" => Some("pg.example.com".into()),
            _ => None,
        });
        assert_eq!(config.port, 1234);
        assert_eq!(config.db_host, "pg.example.com");
        assert_eq!(config.db_port, "5432");
        assert_eq!(config.db_name, "blitzortung");
        assert_eq!(config.db_connection_count, 3);
    }

    #[test]
    fn ini_file_config() {
        let dir = ini_dir("ini");
        let path = dir.join("config.ini");
        std::fs::write(
            &path,
            "[webservice]\nport = 7070\n[db]\nhost = db.local\nport = 5433\ndbname = strikes\n\
             username = u\npassword = p\nconnection_count = 7\n",
        )
        .unwrap();

        let config = Config::from_env_with(|k| {
            if k == "BO_CONFIG" {
                Some(path.to_string_lossy().into_owned())
            } else {
                None
            }
        });
        assert_eq!(config.port, 7070);
        assert_eq!(config.db_host, "db.local");
        assert_eq!(config.db_port, "5433");
        assert_eq!(config.db_name, "strikes");
        assert_eq!(config.db_user, "u");
        assert_eq!(config.db_password, "p");
        assert_eq!(config.db_connection_count, 7);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn protocol_defaults_to_http_and_reads_ini_and_env() {
        // Default.
        assert_eq!(Config::default().protocol, Protocol::Http);

        // INI `[webservice] protocol`.
        let dir = ini_dir("protocol-ini");
        let path = dir.join("config.ini");
        std::fs::write(&path, "[webservice]\nprotocol = lsp\n").unwrap();
        let config = Config::from_env_with(|k| {
            if k == "BO_CONFIG" {
                Some(path.to_string_lossy().into_owned())
            } else {
                None
            }
        });
        assert_eq!(config.protocol, Protocol::Lsp);

        // Env overrides the INI.
        let config = Config::from_env_with(|k| match k {
            "BO_CONFIG" => Some(path.to_string_lossy().into_owned()),
            "BO_SERVICE_PROTOCOL" => Some("http".into()),
            _ => None,
        });
        assert_eq!(config.protocol, Protocol::Http);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn protocol_parse_accepts_aliases() {
        assert_eq!(Protocol::parse("http"), Some(Protocol::Http));
        assert_eq!(Protocol::parse("LSP"), Some(Protocol::Lsp));
        assert_eq!(Protocol::parse("netstring"), Some(Protocol::Lsp));
        assert_eq!(Protocol::parse("bogus"), None);
    }

    #[test]
    fn env_overrides_ini() {
        let dir = ini_dir("env-overrides");
        let path = dir.join("config.ini");
        std::fs::write(&path, "[webservice]\nport = 7070\n[db]\nhost = db.local\n").unwrap();

        let config = Config::from_env_with(|k| match k {
            "BO_CONFIG" => Some(path.to_string_lossy().into_owned()),
            "BO_DB_HOST" => Some("override.example.com".into()),
            "BO_DB_CONNECTION_COUNT" => Some("12".into()),
            _ => None,
        });
        assert_eq!(config.port, 7070);
        assert_eq!(config.db_host, "override.example.com");
        assert_eq!(config.db_connection_count, 12);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn searches_blitzortung_conf_in_current_dir_first() {
        let dir = ini_dir("search");
        let path = dir.join("blitzortung.conf");
        std::fs::write(&path, "[webservice]\nport = 6060\n[db]\nhost = local\n").unwrap();
        // Simulate a CWD inside `dir`: resolve the search relative to it by
        // temporarily creating `./blitzortung.conf`.  Use a subdirectory here
        // so the fixture is cleaned up reliably.
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let config = Config::from_env_with(|_| None);
        std::env::set_current_dir(original).unwrap();
        assert_eq!(config.port, 6060);
        assert_eq!(config.db_host, "local");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ini_file_config_reads_auth_section() {
        let dir = ini_dir("auth-ini");
        let path = dir.join("config.ini");
        std::fs::write(
            &path,
            "[db]\nhost = db.local\n[auth]\nusername = alice\npassword = s3cret\n",
        )
        .unwrap();

        let config = Config::from_env_with(|k| {
            if k == "BO_CONFIG" {
                Some(path.to_string_lossy().into_owned())
            } else {
                None
            }
        });
        assert_eq!(config.username(), "alice");
        assert_eq!(config.password(), "s3cret");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn statsd_defaults_to_localhost_8125() {
        let config = Config::default();
        assert_eq!(config.statsd_address(), ("localhost", 8125));
        assert_eq!(config.statsd_prefix, "org.blitzortung.service");
    }

    #[test]
    fn ini_file_config_reads_statsd_section() {
        let dir = ini_dir("statsd-ini");
        let path = dir.join("config.ini");
        std::fs::write(
            &path,
            "[db]\nhost = db.local\n[statsd]\nhost = metrics.local\nport = 9125\nprefix = my.prefix\n",
        )
        .unwrap();

        let config = Config::from_env_with(|k| {
            if k == "BO_CONFIG" {
                Some(path.to_string_lossy().into_owned())
            } else {
                None
            }
        });
        assert_eq!(config.statsd_address(), ("metrics.local", 9125));
        assert_eq!(config.statsd_prefix, "my.prefix");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn env_overrides_statsd_section() {
        let dir = ini_dir("statsd-env");
        let path = dir.join("config.ini");
        std::fs::write(&path, "[statsd]\nhost = fromfile\nport = 1111\n").unwrap();

        let config = Config::from_env_with(|k| match k {
            "BO_CONFIG" => Some(path.to_string_lossy().into_owned()),
            "BO_STATSD_HOST" => Some("fromenv".into()),
            "BO_STATSD_PORT" => Some("2222".into()),
            "BO_STATSD_PREFIX" => Some("env.prefix".into()),
            _ => None,
        });
        assert_eq!(config.statsd_address(), ("fromenv", 2222));
        assert_eq!(config.statsd_prefix, "env.prefix");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn env_overrides_auth_section() {
        let dir = ini_dir("auth-env");
        let path = dir.join("config.ini");
        std::fs::write(&path, "[auth]\nusername = alice\npassword = fromfile\n").unwrap();

        let config = Config::from_env_with(|k| match k {
            "BO_CONFIG" => Some(path.to_string_lossy().into_owned()),
            "BO_BLITZORTUNG_USERNAME" => Some("bob".into()),
            "BO_BLITZORTUNG_PASSWORD" => Some("fromenv".into()),
            _ => None,
        });
        assert_eq!(config.username(), "bob");
        assert_eq!(config.password(), "fromenv");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn diagnostics_report_loaded_config_path() {
        let dir = ini_dir("diag-loaded");
        let path = dir.join("config.ini");
        std::fs::write(&path, "[db]\nhost = db.local\n").unwrap();

        let (config, diagnostics) = Config::from_env_with_diagnostics(|k| {
            if k == "BO_CONFIG" {
                Some(path.to_string_lossy().into_owned())
            } else {
                None
            }
        });
        assert_eq!(config.db_host, "db.local");
        assert_eq!(
            diagnostics.loaded_path.as_deref(),
            Some(path.to_string_lossy().as_ref())
        );
        assert!(!diagnostics.is_missing());
        assert!(!diagnostics.explicit_but_missing);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn diagnostics_report_missing_config_file() {
        // No BO_CONFIG and (in this temp CWD) no blitzortung.conf.
        let dir = ini_dir("diag-missing");
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let (config, diagnostics) = Config::from_env_with_diagnostics(|_| None);
        std::env::set_current_dir(original).unwrap();

        assert!(diagnostics.is_missing());
        assert!(!diagnostics.explicit_but_missing);
        // Defaults are still applied (non-breaking).
        assert_eq!(config.db_host, "localhost");
        assert_eq!(config.db_name, "blitzortung");
        // The search order is reported.
        assert_eq!(
            diagnostics.searched_paths,
            vec![
                "./blitzortung.conf".to_string(),
                "/etc/blitzortung.conf".to_string()
            ]
        );
        let warning = diagnostics.missing_warning();
        assert!(warning.contains("no blitzortung configuration file found"));
        assert!(warning.contains("./blitzortung.conf"));
        assert!(warning.contains("config.yml is NOT read"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn diagnostics_flag_explicit_but_missing_config() {
        let (_config, diagnostics) = Config::from_env_with_diagnostics(|k| {
            if k == "BO_CONFIG" {
                Some("/nonexistent/blitzortung.conf".into())
            } else {
                None
            }
        });
        assert!(diagnostics.is_missing());
        assert!(diagnostics.explicit_but_missing);
    }

    #[test]
    fn make_dsn_quoting_matches_python() {
        assert_eq!(quote_conninfo_value("localhost"), "localhost");
        assert_eq!(quote_conninfo_value(""), "''");
        assert_eq!(quote_conninfo_value("my host"), "'my host'");
        assert_eq!(quote_conninfo_value("pa'ss\\word"), "pa\\'ss\\\\word");
        let config = Config {
            db_host: "db server".into(),
            db_port: "5432".into(),
            db_name: "blitz".into(),
            db_user: "u".into(),
            db_password: "".into(),
            ..Default::default()
        };
        assert_eq!(
            config.db_connection_string(),
            "host='db server' port=5432 dbname=blitz user=u password=''"
        );
    }

    /// `[webservice] servicelog` / `BO_SERVICE_SERVICELOG` (with the
    /// `log_directory` / `BO_SERVICE_LOG_DIR` aliases) drive the usage-log
    /// directory; env wins over the INI and an existing path is required.
    #[test]
    fn service_log_directory_from_ini_and_env() {
        let dir = ini_dir("service-log");
        let ini = dir.join("config.ini");
        let log_dir = ini_dir("service-log-target");
        std::fs::write(
            &ini,
            format!("[webservice]\nservicelog = {}\n", log_dir.to_string_lossy()),
        )
        .unwrap();

        // INI value is used.
        let config = Config::from_env_with(|k| {
            (k == "BO_CONFIG").then(|| ini.to_string_lossy().into_owned())
        });
        assert_eq!(
            config.service_log_directory().as_deref(),
            Some(log_dir.as_path())
        );

        // Env overrides the INI.
        let other = ini_dir("service-log-env-target");
        let config = Config::from_env_with(|k| match k {
            "BO_CONFIG" => Some(ini.to_string_lossy().into_owned()),
            "BO_SERVICE_SERVICELOG" => Some(other.to_string_lossy().into_owned()),
            _ => None,
        });
        assert_eq!(
            config.service_log_directory().as_deref(),
            Some(other.as_path())
        );

        // The `BO_SERVICE_LOG_DIR` alias is still honoured.
        let alias = ini_dir("service-log-alias-target");
        let config = Config::from_env_with(|k| match k {
            "BO_CONFIG" => Some(ini.to_string_lossy().into_owned()),
            "BO_SERVICE_LOG_DIR" => Some(alias.to_string_lossy().into_owned()),
            _ => None,
        });
        assert_eq!(
            config.service_log_directory().as_deref(),
            Some(alias.as_path())
        );

        // An empty env value disables logging.
        let config = Config::from_env_with(|k| match k {
            "BO_CONFIG" => Some(ini.to_string_lossy().into_owned()),
            "BO_SERVICE_SERVICELOG" => Some(String::new()),
            _ => None,
        });
        assert_eq!(config.service_log_dir, None);
        assert_eq!(config.service_log_directory(), None);

        // The `log_directory` INI alias is also honoured.
        let alias_ini = ini_dir("service-log-alias-ini");
        let alias_ini_path = alias_ini.join("config.ini");
        let alias_target = ini_dir("service-log-alias-ini-target");
        std::fs::write(
            &alias_ini_path,
            format!(
                "[webservice]\nlog_directory = {}\n",
                alias_target.to_string_lossy()
            ),
        )
        .unwrap();
        let config = Config::from_env_with(|k| {
            (k == "BO_CONFIG").then(|| alias_ini_path.to_string_lossy().into_owned())
        });
        assert_eq!(
            config.service_log_directory().as_deref(),
            Some(alias_target.as_path())
        );

        // A configured but non-existent directory is disabled.
        let mut config = Config::from_env_with(|k| {
            (k == "BO_CONFIG").then(|| ini.to_string_lossy().into_owned())
        });
        config.service_log_dir = Some("/nonexistent/bo-usage-log".into());
        assert_eq!(config.service_log_directory(), None);

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&log_dir).unwrap();
        std::fs::remove_dir_all(&other).unwrap();
        std::fs::remove_dir_all(&alias).unwrap();
        std::fs::remove_dir_all(&alias_ini).unwrap();
        std::fs::remove_dir_all(&alias_target).unwrap();
    }

    /// With nothing configured (no CLI/env/INI) the usage log is **off**: no
    /// implicit fallback to `/var/log/blitzortung`, even if that directory
    /// exists.
    #[test]
    fn service_log_directory_is_none_without_explicit_config() {
        let dir = ini_dir("service-log-unconfigured");
        // An INI without a servicelog key, and no env vars.
        let ini = dir.join("config.ini");
        std::fs::write(&ini, "[webservice]\nport = 7070\n").unwrap();
        let config = Config::from_env_with(|k| {
            (k == "BO_CONFIG").then(|| ini.to_string_lossy().into_owned())
        });
        assert_eq!(config.configured_service_log_directory(), None);
        assert_eq!(config.service_log_directory(), None);

        // Even the programmatic default (no service_log_dir) is off, regardless
        // of whether /var/log/blitzortung happens to exist on this host.
        assert_eq!(Config::default().configured_service_log_directory(), None);
        assert_eq!(Config::default().service_log_directory(), None);

        // Sanity: on hosts where the Python default exists AND is writable, the
        // Rust port still does not use it implicitly.
        if std::path::Path::new("/var/log/blitzortung").is_dir() {
            let _ = crate::service_log::directory_is_writable(std::path::Path::new(
                "/var/log/blitzortung",
            ));
            assert_eq!(Config::default().service_log_directory(), None);
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A path that exists but cannot be used as a servicelog directory (a
    /// regular file, or a missing directory) is treated as **disabled**, not
    /// enabled: existence alone is not enough.
    #[test]
    fn service_log_directory_requires_a_writable_directory() {
        // A regular file: exists, but not a usable directory.  Root cannot make
        // a file writable-as-a-directory, so this is deterministic under CI.
        let dir = ini_dir("service-log-notdir");
        let file = dir.join("blocker");
        std::fs::write(&file, b"x").unwrap();
        let mut config = Config {
            service_log_dir: Some(file.to_string_lossy().into_owned()),
            ..Config::default()
        };
        assert_eq!(config.service_log_directory(), None);
        // The candidate is still reported (for the startup warning).
        assert_eq!(
            config.configured_service_log_directory().as_deref(),
            Some(file.as_path())
        );

        // A missing directory is disabled too.
        config.service_log_dir = Some("/nonexistent/bo-usage-log".into());
        assert_eq!(config.service_log_directory(), None);

        // A writable directory is enabled.
        let ok = ini_dir("service-log-ok");
        config.service_log_dir = Some(ok.to_string_lossy().into_owned());
        assert_eq!(
            config.service_log_directory().as_deref(),
            Some(ok.as_path())
        );

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&ok).unwrap();
    }

    /// `[webservice] geoip_db` / `BO_GEOIP_DB` (alias `BO_SERVICE_GEOIP_DB`)
    /// set the GeoIP database; the default is the Python tool's path.
    #[test]
    fn service_geoip_db_from_ini_and_env() {
        assert_eq!(
            Config::default().service_geoip_db(),
            std::path::PathBuf::from(crate::service_log::DEFAULT_GEOIP_DB)
        );

        let dir = ini_dir("service-geoip");
        let ini = dir.join("config.ini");
        std::fs::write(&ini, "[webservice]\ngeoip_db = /tmp/from-ini.mmdb\n").unwrap();
        let config = Config::from_env_with(|k| {
            (k == "BO_CONFIG").then(|| ini.to_string_lossy().into_owned())
        });
        assert_eq!(
            config.service_geoip_db(),
            std::path::PathBuf::from("/tmp/from-ini.mmdb")
        );

        let config = Config::from_env_with(|k| match k {
            "BO_CONFIG" => Some(ini.to_string_lossy().into_owned()),
            "BO_GEOIP_DB" => Some("/tmp/from-env.mmdb".into()),
            _ => None,
        });
        assert_eq!(
            config.service_geoip_db(),
            std::path::PathBuf::from("/tmp/from-env.mmdb")
        );

        // The `BO_SERVICE_GEOIP_DB` alias is still honoured.
        let config = Config::from_env_with(|k| match k {
            "BO_CONFIG" => Some(ini.to_string_lossy().into_owned()),
            "BO_SERVICE_GEOIP_DB" => Some("/tmp/from-alias.mmdb".into()),
            _ => None,
        });
        assert_eq!(
            config.service_geoip_db(),
            std::path::PathBuf::from("/tmp/from-alias.mmdb")
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
