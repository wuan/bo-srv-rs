//! Data transport and URL path helpers
//! (port of `blitzortung/dataimport/base.py`).

use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::StatusCode;

use crate::config::Config;
use crate::util::{log_path, time_intervals, Timer};

/// A source of text lines (`TransportAbstract` / `FileTransport`).
pub trait Transport {
    /// Read all lines from `source`.
    fn read_lines(&self, source: &str) -> Result<Vec<String>, TransportError>;
}

/// Errors raised by the transports.
#[derive(Debug)]
pub enum TransportError {
    Request(reqwest::Error),
    Io(std::io::Error),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Request(e) => write!(f, "{e}"),
            TransportError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TransportError {}

/// Read lines from a local file (`FileTransport`).
pub struct FileTransport;

impl Transport for FileTransport {
    fn read_lines(&self, source: &str) -> Result<Vec<String>, TransportError> {
        let content = std::fs::read_to_string(source).map_err(TransportError::Io)?;
        Ok(content.lines().map(|l| l.to_string()).collect())
    }
}

/// HTTP transport with basic auth (`HttpFileTransport`).
///
/// Python uses a 60 second timeout; the `bo-import` CLI additionally relies on
/// its whole-region timeout, so per-request failures surface as
/// [`TransportError::Request`].
pub struct HttpFileTransport {
    client: Client,
    username: String,
    password: String,
}

impl HttpFileTransport {
    /// Default per-request timeout in seconds
    /// (`HttpFileTransport.TIMEOUT_SECONDS`).
    pub const TIMEOUT_SECONDS: u64 = 60;

    /// Build a transport from the configured basic-auth credentials.
    pub fn new(config: &Config) -> Self {
        Self::with_timeout(config, Duration::from_secs(Self::TIMEOUT_SECONDS))
    }

    /// Build a transport with an explicit request timeout.
    pub fn with_timeout(config: &Config, timeout: Duration) -> Self {
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .expect("failed to build HTTP client");
        HttpFileTransport {
            client,
            username: config.username().to_string(),
            password: config.password().to_string(),
        }
    }
}

impl Transport for HttpFileTransport {
    /// `HttpFileTransport.read_lines`: GET with basic auth; a non-200 response
    /// yields no lines.
    fn read_lines(&self, source: &str) -> Result<Vec<String>, TransportError> {
        let mut timer = Timer::new();
        let response = self
            .client
            .get(source)
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .map_err(TransportError::Request)?;

        if response.status() != StatusCode::OK {
            log::debug!(
                "http status {} for get '{}' ({:.3}s)",
                response.status(),
                source,
                timer.lap()
            );
            return Ok(Vec::new());
        }
        log::debug!("get '{}' ({:.3}s)", source, timer.lap());

        let body = response.text().map_err(TransportError::Request)?;
        Ok(body.lines().map(|l| l.to_string()).collect())
    }
}

/// Base URL builder for Blitzortung data
/// (`BlitzortungDataPath`).
#[derive(Debug, Clone)]
pub struct BlitzortungDataPath {
    data_path: String,
}

impl BlitzortungDataPath {
    /// Default host name component (`BlitzortungDataPath.default_host_name`).
    pub const DEFAULT_HOST_NAME: &'static str = "data";
    /// Default region (`BlitzortungDataPath.default_region`).
    pub const DEFAULT_REGION: u32 = 1;

    /// `BlitzortungDataPath.__init__`: defaults to
    /// `https://data.blitzortung.org/Data`.
    pub fn new(base_path: Option<&str>) -> Self {
        let base = base_path.unwrap_or("https://data.blitzortung.org");
        BlitzortungDataPath {
            data_path: format!("{base}/Data"),
        }
    }

    /// `BlitzortungDataPath.build_path`.
    pub fn build_path(&self, sub_path: &str, host_name: &str, region: u32) -> String {
        let path = sub_path
            .replace("{host_name}", host_name)
            .replace("{region}", &region.to_string());
        let sub = path.trim_start_matches('/');
        format!("{}/{}", self.data_path.trim_end_matches('/'), sub)
    }
}

impl Default for BlitzortungDataPath {
    fn default() -> Self {
        BlitzortungDataPath::new(None)
    }
}

/// Generator for the ten-minute protected-log paths
/// (`BlitzortungDataPathGenerator`).
pub struct BlitzortungDataPathGenerator {
    /// `BlitzortungDataPathGenerator.time_granularity`.
    pub time_granularity: chrono::Duration,
}

impl Default for BlitzortungDataPathGenerator {
    fn default() -> Self {
        BlitzortungDataPathGenerator {
            time_granularity: chrono::Duration::minutes(10),
        }
    }
}

impl BlitzortungDataPathGenerator {
    /// `BlitzortungDataPathGenerator.get_paths`: ten-minute log paths from
    /// `start_time` to `end_time` (default now).
    pub fn get_paths(
        &self,
        start_time: chrono::DateTime<chrono::Utc>,
        end_time: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Vec<String> {
        time_intervals(start_time, self.time_granularity, end_time)
            .into_iter()
            .map(log_path)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn data_path_defaults() {
        let path = BlitzortungDataPath::default();
        assert_eq!(
            path.build_path("url_path", BlitzortungDataPath::DEFAULT_HOST_NAME, 1),
            "https://data.blitzortung.org/Data/url_path"
        );
    }

    #[test]
    fn data_path_with_region_template() {
        let path = BlitzortungDataPath::default();
        let url = path.build_path("Protected/Strikes_{region}/2013/08/20/11/40.log", "data", 3);
        assert_eq!(
            url,
            "https://data.blitzortung.org/Data/Protected/Strikes_3/2013/08/20/11/40.log"
        );
    }

    #[test]
    fn data_path_with_custom_base() {
        let path = BlitzortungDataPath::new(Some("base/path"));
        assert_eq!(
            path.build_path("url_path", "bar", 39),
            "base/path/Data/url_path"
        );
    }

    #[test]
    fn generator_yields_expected_paths() {
        let generator = BlitzortungDataPathGenerator::default();
        let start = Utc.with_ymd_and_hms(2013, 8, 20, 11, 44, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2013, 8, 20, 12, 9, 0).unwrap();
        let paths = generator.get_paths(start, Some(end));
        assert!(paths.contains(&"2013/08/20/11/40.log".to_string()));
        assert!(paths.contains(&"2013/08/20/11/50.log".to_string()));
        assert!(paths.contains(&"2013/08/20/12/00.log".to_string()));
    }
}
