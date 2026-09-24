//! Strike data provider (port of
//! `blitzortung/dataimport/strike.py`).

use chrono::{DateTime, Duration, Utc};

use crate::builder::{BuilderError, Strike as StrikeBuilder};
use crate::data::Strike;
use crate::dataimport::base::{BlitzortungDataPath, BlitzortungDataPathGenerator, Transport};
use crate::dataimport::TransportError;
use crate::util::Timer;

/// Provider that downloads protected strike logs and builds strikes
/// (`StrikesBlitzortungDataProvider`).
pub struct StrikesBlitzortungDataProvider<'a, T: Transport> {
    transport: &'a T,
    data_url: BlitzortungDataPath,
    url_path_generator: BlitzortungDataPathGenerator,
}

/// Error returned while fetching strikes for a region.
#[derive(Debug)]
pub enum ImportError {
    Transport(TransportError),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Transport(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl<'a, T: Transport> StrikesBlitzortungDataProvider<'a, T> {
    pub fn new(transport: &'a T) -> Self {
        StrikesBlitzortungDataProvider {
            transport,
            data_url: BlitzortungDataPath::default(),
            url_path_generator: BlitzortungDataPathGenerator::default(),
        }
    }

    /// `StrikesBlitzortungDataProvider.get_strikes_since`: for every ten-minute
    /// log from `latest_strike` (default now-6h) up to now, download the
    /// region's log and yield strikes with a valid timestamp newer than
    /// `latest_strike`.
    ///
    /// Parse errors are logged and skipped (`builder.BuilderError`); transport
    /// errors abort the current region.
    pub fn get_strikes_since(
        &self,
        latest_strike: Option<DateTime<Utc>>,
        region: u32,
    ) -> Result<Vec<Strike>, ImportError> {
        self.get_strikes_since_with_deadline(latest_strike, region, None)
    }

    /// Same as [`Self::get_strikes_since`] but stops downloading further log
    /// files once `deadline` has passed (the Rust equivalent of the Python
    /// per-region `stopit.SignalTimeout`).
    pub fn get_strikes_since_with_deadline(
        &self,
        latest_strike: Option<DateTime<Utc>>,
        region: u32,
        deadline: Option<std::time::Instant>,
    ) -> Result<Vec<Strike>, ImportError> {
        let latest_strike = latest_strike
            .unwrap_or_else(|| Utc::now() - Duration::hours(6));
        log::debug!("import strikes since {latest_strike}");

        let mut strikes = Vec::new();
        for url_path in self.url_path_generator.get_paths(latest_strike, None) {
            if let Some(deadline) = deadline {
                if std::time::Instant::now() > deadline {
                    log::warn!("stopping import for region {region}: time budget exceeded");
                    break;
                }
            }
            let target_url = self.data_url.build_path(
                &format!("Protected/Strikes_{{region}}/{url_path}"),
                BlitzortungDataPath::DEFAULT_HOST_NAME,
                region,
            );
            log::debug!("import region {region} from {target_url}");

            let mut strike_count = 0usize;
            let timer = Timer::new();
            let lines = self
                .transport
                .read_lines(&target_url)
                .map_err(ImportError::Transport)?;
            for line in lines {
                match build_strike_from_line(&line) {
                    Ok(strike) => {
                        if strike.timestamp.is_valid()
                            && strike
                                .timestamp
                                .datetime
                                .map(|dt| dt > latest_strike)
                                .unwrap_or(false)
                        {
                            strike_count += 1;
                            strikes.push(strike);
                        }
                    }
                    Err(BuilderError(message)) => {
                        log::warn!("BuilderError: {message} ({line})");
                    }
                }
            }
            log::debug!(
                "imported {strike_count} strikes for region {region} in {:.2}s from {target_url}",
                timer.read()
            );
        }
        Ok(strikes)
    }

    /// The ten-minute log paths that would be fetched for `start_time`
    /// (exposed for tests).
    pub fn paths_for(
        &self,
        start_time: DateTime<Utc>,
        end_time: Option<DateTime<Utc>>,
    ) -> Vec<String> {
        self.url_path_generator.get_paths(start_time, end_time)
    }
}

/// Build a strike from a protected-log data line.
fn build_strike_from_line(line: &str) -> Result<Strike, BuilderError> {
    let mut builder = StrikeBuilder::new();
    let built = builder.from_line(line)?.build()?;
    Ok(built)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataimport::base::{FileTransport, Transport, TransportError};
    use std::sync::Mutex;

    /// In-memory transport returning canned line sets per call.
    struct StubTransport {
        outputs: Mutex<Vec<Vec<String>>>,
    }

    impl Transport for StubTransport {
        fn read_lines(&self, _source: &str) -> Result<Vec<String>, TransportError> {
            Ok(self.outputs.lock().unwrap().pop().unwrap_or_default())
        }
    }

    #[test]
    fn provider_filters_older_strikes() {
        let now = Utc::now();
        let older = now - Duration::hours(2);
        let newer = now;

        // The protected-log timestamp is a 29 character
        // `%Y-%m-%d %H:%M:%S.%f` plus three nanosecond digits.
        let line_old = format!(
            "{}789 pos;48.5;-10.2;500.5 str;45.2 dev;250.0 sta;5;10;1,2,3",
            older.format("%Y-%m-%d %H:%M:%S.%6f")
        );
        let line_new = format!(
            "{}789 pos;48.5;-10.2;500.5 str;45.2 dev;250.0 sta;5;10;1,2,3",
            newer.format("%Y-%m-%d %H:%M:%S.%6f")
        );

        let transport = StubTransport {
            outputs: Mutex::new(vec![vec![line_old, line_new]]),
        };
        let provider = StrikesBlitzortungDataProvider::new(&transport);
        let strikes = provider
            .get_strikes_since(Some(now - Duration::hours(1)), 1)
            .unwrap();
        assert_eq!(strikes.len(), 1);
    }

    #[test]
    fn provider_skips_builder_errors() {
        let transport = StubTransport {
            outputs: Mutex::new(vec![vec!["invalid line".to_string()]]),
        };
        let provider = StrikesBlitzortungDataProvider::new(&transport);
        let strikes = provider.get_strikes_since(None, 1).unwrap();
        assert!(strikes.is_empty());
    }

    #[test]
    fn file_transport_reads_lines() {
        let dir = std::env::temp_dir().join(format!("bo-strike-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.txt");
        std::fs::write(&path, "line1\nline2\n").unwrap();
        let transport = FileTransport;
        let lines = transport.read_lines(path.to_str().unwrap()).unwrap();
        assert_eq!(lines, vec!["line1", "line2"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}