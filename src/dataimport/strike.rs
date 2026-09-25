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
    /// A failure while handing a batch of strikes to a [`StrikeSink`].
    Sink(Box<dyn std::error::Error + Send + Sync>),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Transport(e) => write!(f, "{e}"),
            ImportError::Sink(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ImportError {}

/// Receiver for strike batches produced while a region is imported.
///
/// The provider hands strikes over in bounded batches ([`StrikesBlitzortungDataProvider::get_strikes_since_with_deadline_into`])
/// so a caller can persist and commit them incrementally instead of buffering
/// a whole region, which may contain several million strikes.
#[async_trait::async_trait]
pub trait StrikeSink: Send {
    /// Persist one batch of strikes.
    async fn accept(
        &mut self,
        strikes: &[Strike],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

/// [`StrikeSink`] that collects every batch, used by the convenience
/// `get_strikes_since*` helpers.
#[derive(Default)]
struct CollectStrikes(Vec<Strike>);

#[async_trait::async_trait]
impl StrikeSink for CollectStrikes {
    async fn accept(
        &mut self,
        strikes: &[Strike],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.0.extend_from_slice(strikes);
        Ok(())
    }
}

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
    /// region's log and collect strikes with a valid timestamp newer than
    /// `latest_strike`.
    ///
    /// Parse errors are logged and skipped (`builder.BuilderError`); a log file
    /// that is missing on the server (404 or request timeout) is skipped, while
    /// other transport errors abort the current region.
    pub async fn get_strikes_since(
        &self,
        latest_strike: Option<DateTime<Utc>>,
        region: u32,
    ) -> Result<Vec<Strike>, ImportError> {
        self.get_strikes_since_with_deadline(latest_strike, region, None)
            .await
    }

    /// Same as [`Self::get_strikes_since`] but stops downloading further log
    /// files once `deadline` has passed (the Rust equivalent of the Python
    /// per-region `stopit.SignalTimeout`).
    pub async fn get_strikes_since_with_deadline(
        &self,
        latest_strike: Option<DateTime<Utc>>,
        region: u32,
        deadline: Option<std::time::Instant>,
    ) -> Result<Vec<Strike>, ImportError> {
        let mut sink = CollectStrikes::default();
        self.get_strikes_since_with_deadline_into(
            latest_strike,
            region,
            deadline,
            usize::MAX,
            &mut sink,
        )
        .await?;
        Ok(sink.0)
    }

    /// Streaming variant of [`Self::get_strikes_since_with_deadline`]: instead of
    /// returning a whole region's strikes, hands them to `sink` in batches of at
    /// most `chunk_size` as they are parsed.  This keeps the importer's memory
    /// bounded even for regions with millions of strikes.
    ///
    /// Returns the number of strikes handed to the sink.
    pub async fn get_strikes_since_with_deadline_into<S: StrikeSink>(
        &self,
        latest_strike: Option<DateTime<Utc>>,
        region: u32,
        deadline: Option<std::time::Instant>,
        chunk_size: usize,
        sink: &mut S,
    ) -> Result<usize, ImportError> {
        let latest_strike = latest_strike.unwrap_or_else(|| Utc::now() - Duration::hours(6));
        log::debug!("import strikes since {latest_strike}");

        let chunk_size = chunk_size.max(1);
        // `chunk_size` may be `usize::MAX` for the collecting helper; never
        // pre-allocate more than a modest batch up front.
        let mut buffer: Vec<Strike> = Vec::with_capacity(chunk_size.min(1024));
        let mut total = 0usize;

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
            let lines = match self.transport.read_lines(&target_url).await {
                Ok(lines) => lines,
                Err(error) if error.is_missing() => {
                    log::debug!("missing log file {target_url}: {error}, skipping");
                    continue;
                }
                Err(error) => return Err(ImportError::Transport(error)),
            };
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
                            buffer.push(strike);
                            if buffer.len() >= chunk_size {
                                sink.accept(&buffer).await.map_err(ImportError::Sink)?;
                                total += buffer.len();
                                buffer.clear();
                            }
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

        // Flush a final partial batch.
        if !buffer.is_empty() {
            sink.accept(&buffer).await.map_err(ImportError::Sink)?;
            total += buffer.len();
        }

        Ok(total)
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

    #[async_trait::async_trait]
    impl Transport for StubTransport {
        async fn read_lines(&self, _source: &str) -> Result<Vec<String>, TransportError> {
            Ok(self.outputs.lock().unwrap().pop().unwrap_or_default())
        }
    }

    /// Transport whose first call reports a missing file, whose second call
    /// serves `lines`, and whose later calls are empty.
    struct MissingThenOnceTransport {
        state: Mutex<u8>,
        lines: Vec<String>,
    }

    #[async_trait::async_trait]
    impl Transport for MissingThenOnceTransport {
        async fn read_lines(&self, _source: &str) -> Result<Vec<String>, TransportError> {
            let mut state = self.state.lock().unwrap();
            match *state {
                0 => {
                    *state = 1;
                    Err(TransportError::NotFound)
                }
                1 => {
                    *state = 2;
                    Ok(self.lines.clone())
                }
                _ => Ok(Vec::new()),
            }
        }
    }

    /// [`StrikeSink`] recording the batches it receives.
    #[derive(Default)]
    struct RecordingSink {
        batches: Vec<usize>,
        strikes: Vec<Strike>,
    }

    #[async_trait::async_trait]
    impl StrikeSink for RecordingSink {
        async fn accept(
            &mut self,
            strikes: &[Strike],
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.batches.push(strikes.len());
            self.strikes.extend_from_slice(strikes);
            Ok(())
        }
    }

    #[tokio::test]
    async fn provider_streams_strikes_to_sink_in_chunks() {
        let now = Utc::now();
        let line_a = format!(
            "{}789 pos;48.5;-10.2;500.5 str;45.2 dev;250.0 sta;5;10;1,2,3",
            (now - Duration::minutes(5)).format("%Y-%m-%d %H:%M:%S.%6f")
        );
        let line_b = format!(
            "{}789 pos;48.5;-10.2;500.5 str;45.2 dev;250.0 sta;5;10;1,2,3",
            (now - Duration::minutes(4)).format("%Y-%m-%d %H:%M:%S.%6f")
        );
        let transport = StubTransport {
            outputs: Mutex::new(vec![vec![line_a, line_b]]),
        };
        let provider = StrikesBlitzortungDataProvider::new(&transport);

        let mut sink = RecordingSink::default();
        let total = provider
            .get_strikes_since_with_deadline_into(
                Some(now - Duration::minutes(20)),
                1,
                None,
                1,
                &mut sink,
            )
            .await
            .unwrap();

        // One strike per batch, so a million-strike region never has more than
        // the chunk in memory.
        assert_eq!(total, 2);
        assert_eq!(sink.batches, vec![1, 1]);
        assert_eq!(sink.strikes.len(), 2);
    }

    #[tokio::test]
    async fn provider_flushes_final_partial_batch() {
        let now = Utc::now();
        let line = format!(
            "{}789 pos;48.5;-10.2;500.5 str;45.2 dev;250.0 sta;5;10;1,2,3",
            (now - Duration::minutes(5)).format("%Y-%m-%d %H:%M:%S.%6f")
        );
        let transport = StubTransport {
            outputs: Mutex::new(vec![vec![line]]),
        };
        let provider = StrikesBlitzortungDataProvider::new(&transport);

        let mut sink = RecordingSink::default();
        let total = provider
            .get_strikes_since_with_deadline_into(
                Some(now - Duration::minutes(20)),
                1,
                None,
                1000,
                &mut sink,
            )
            .await
            .unwrap();

        assert_eq!(total, 1);
        assert_eq!(sink.batches, vec![1]);
    }

    #[tokio::test]
    async fn provider_skips_missing_files_and_continues() {
        let now = Utc::now();
        let line = format!(
            "{}789 pos;48.5;-10.2;500.5 str;45.2 dev;250.0 sta;5;10;1,2,3",
            now.format("%Y-%m-%d %H:%M:%S.%6f")
        );
        let transport = MissingThenOnceTransport {
            state: Mutex::new(0),
            lines: vec![line],
        };
        let provider = StrikesBlitzortungDataProvider::new(&transport);
        // Three ten-minute slots: the first is missing, the second serves the
        // strike, the third is empty.
        let strikes = provider
            .get_strikes_since(Some(now - Duration::minutes(20)), 1)
            .await
            .unwrap();
        assert_eq!(strikes.len(), 1);
    }

    #[tokio::test]
    async fn provider_filters_older_strikes() {
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
            .await
            .unwrap();
        assert_eq!(strikes.len(), 1);
    }

    #[tokio::test]
    async fn provider_skips_builder_errors() {
        let transport = StubTransport {
            outputs: Mutex::new(vec![vec!["invalid line".to_string()]]),
        };
        let provider = StrikesBlitzortungDataProvider::new(&transport);
        let strikes = provider.get_strikes_since(None, 1).await.unwrap();
        assert!(strikes.is_empty());
    }

    #[tokio::test]
    async fn file_transport_reads_lines() {
        let dir = std::env::temp_dir().join(format!("bo-strike-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.txt");
        std::fs::write(&path, "line1\nline2\n").unwrap();
        let transport = FileTransport;
        let lines = transport.read_lines(path.to_str().unwrap()).await.unwrap();
        assert_eq!(lines, vec!["line1", "line2"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
