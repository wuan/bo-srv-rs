//! Runtime configuration.
//!
//! Mirrors the Python layer's `blitzortung/config.py`: a `blitzortung.conf`
//! INI file with `[webservice] port` and `[db] host/port/dbname/username/
//! password/connection_count` sections, searched in `.` then `/etc/`
//! (`ConfigModule.find_config_file_path`).
//!
//! Environment variables supplement/override the file (kept for dev
//! convenience; they are more explicit than the INI):
//!
//! * `BO_CONFIG` — explicit path to the INI file (bypasses the search)
//! * `BO_SERVICE_PORT` (default `8080`)
//! * `BO_DB_HOST` (default `localhost`)
//! * `BO_DB_PORT` (default `5432`)
//! * `BO_DB_NAME` (default `blitzortung`)
//! * `BO_DB_USER` (default `blitzortung`)
//! * `BO_DB_PASSWORD` (default `""`)
//! * `BO_DB_CONNECTION_COUNT` (default `3`)
//! * `BO_BLITZORTUNG_USERNAME` (default `""`) — `[auth] username`
//! * `BO_BLITZORTUNG_PASSWORD` (default `""`) — `[auth] password`

/// Parsed service configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub port: u16,
    pub db_host: String,
    pub db_port: String,
    pub db_name: String,
    pub db_user: String,
    pub db_password: String,
    /// `Config.get_db_connection_count`, default 3 (the txpostgres default).
    pub db_connection_count: u32,
    /// HTTP basic-auth username for the protected Blitzortung data feeds
    /// (`Config.get_username`, `[auth] username`).
    pub auth_username: String,
    /// HTTP basic-auth password for the protected Blitzortung data feeds
    /// (`Config.get_password`, `[auth] password`).
    pub auth_password: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            port: 8080,
            db_host: "localhost".into(),
            db_port: "5432".into(),
            db_name: "blitzortung".into(),
            db_user: "blitzortung".into(),
            db_password: String::new(),
            db_connection_count: 3,
            auth_username: String::new(),
            auth_password: String::new(),
        }
    }
}

impl Config {
    /// Load configuration from the environment (and the INI file).
    pub fn from_env() -> Self {
        Self::from_env_with(|k| std::env::var(k).ok())
    }

    /// Testable core: `lookup` returns the value of an env var by name.
    ///
    /// The INI file is resolved the way the Python `ConfigModule` does it:
    /// an explicit `BO_CONFIG` path first, otherwise `./blitzortung.conf` and
    /// `/etc/blitzortung.conf`.  Flat env vars then override the INI.
    pub fn from_env_with(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let mut config = Config::default();

        let ini_path = lookup("BO_CONFIG")
            .filter(|path| !path.is_empty())
            .or_else(find_blitzortung_conf);
        if let Some(ini_path) = ini_path {
            if let Some(ini) = read_ini(&ini_path) {
                if let Some(section) = ini.get("webservice") {
                    if let Some(port) = section.get("port") {
                        if let Ok(port) = port.parse::<u16>() {
                            config.port = port;
                        }
                    }
                }
                if let Some(section) = ini.get("db") {
                    config.db_host = section.get("host").cloned().unwrap_or(config.db_host);
                    config.db_port = section.get("port").cloned().unwrap_or(config.db_port);
                    config.db_name = section.get("dbname").cloned().unwrap_or(config.db_name);
                    config.db_user = section.get("username").cloned().unwrap_or(config.db_user);
                    config.db_password = section.get("password").cloned().unwrap_or(config.db_password);
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
            }
        }

        if let Some(v) = lookup("BO_SERVICE_PORT") {
            if let Ok(port) = v.parse::<u16>() {
                config.port = port;
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

        config
    }

    /// `Config.get_username`: the HTTP basic-auth username from `[auth]`.
    pub fn username(&self) -> &str {
        &self.auth_username
    }

    /// `Config.get_password`: the HTTP basic-auth password from `[auth]`.
    pub fn password(&self) -> &str {
        &self.auth_password
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

/// Search order of `ConfigModule.find_config_file_path`: `.` then `/etc/`.
fn find_blitzortung_conf() -> Option<String> {
    [".", "/etc/"]
        .iter()
        .map(|dir| format!("{dir}/blitzortung.conf"))
        .find(|path| std::path::Path::new(path).exists())
}

/// Read a simple INI file into a map of section name -> key/value pairs.
fn read_ini(path: &str) -> Option<std::collections::HashMap<String, std::collections::HashMap<String, String>>> {
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
}