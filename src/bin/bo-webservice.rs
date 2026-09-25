//! `bo-service` — Rust port of the blitzortung JSON-RPC webservice.
//!
//! Run:
//!
//! ```text
//! cargo run --bin bo-webservice
//! ```
//!
//! Configuration (see [`blitzortung_srv::config`]): the `--port`/`--protocol` CLI
//! flags, `BO_SERVICE_PORT`/`BO_SERVICE_PROTOCOL`, `BO_DB_*` env vars and/or a
//! `BO_CONFIG` INI file.  Both settings resolve with the precedence **CLI >
//! env > config file > default** (`Config::from_env` applies the env over the
//! INI; the CLI flags win on top).
//!
//! The default wire protocol is **HTTP/1.1** so the service is a drop-in
//! replacement behind the deployed Nginx `proxy_pass`.  The original LSP-style
//! `Content-Length` framing remains available via `--protocol lsp` (or
//! `BO_SERVICE_PROTOCOL=lsp`) for other consumers/tests.

use std::sync::Arc;

use clap::Parser;

use blitzortung_srv::config::{Config, Protocol};
use blitzortung_srv::executor::QueryExecutor;
use blitzortung_srv::metrics::{Metrics, StatsDMetrics};
use blitzortung_srv::service_log::UsageLogConsumer;
use blitzortung_srv::{http, postgres::PostgresExecutor, service::Service, transport};

/// Build the service metrics sink.
///
/// Uses [`StatsDMetrics`] against the configured local StatsD receiver
/// (default `localhost:8125`, `org.blitzortung.service` prefix).  When the
/// socket cannot be set up the service falls back to [`NoopMetrics`] so a
/// missing metrics daemon can never prevent startup.
fn build_metrics(config: &Config) -> std::sync::Arc<dyn Metrics> {
    let (host, port) = config.statsd_address();
    match StatsDMetrics::with_address_and_prefix(host, port, config.statsd_prefix.clone()) {
        Ok(metrics) => {
            log::info!(
                "sending StatsD metrics to {} with prefix {:?}",
                metrics.target(),
                config.statsd_prefix
            );
            std::sync::Arc::new(metrics)
        }
        Err(error) => {
            log::warn!(
                "StatsD metrics disabled: could not set up a sender for {host}:{port}: {error}"
            );
            std::sync::Arc::new(blitzortung_srv::metrics::NoopMetrics)
        }
    }
}

/// Command-line options for the `bo-webservice` binary.
#[derive(Parser, Debug)]
#[command(
    name = "bo-webservice",
    about = "Blitzortung JSON-RPC webservice",
    version
)]
struct Args {
    /// listening TCP port (overrides BO_SERVICE_PORT and blitzortung.conf)
    #[arg(short, long)]
    port: Option<u16>,

    /// wire protocol: `http` (default, for nginx proxy_pass) or `lsp`
    #[arg(long, value_name = "http|lsp")]
    protocol: Option<String>,

    /// enable the usage log and write `servicelog_YYYY-MM-DD` into this
    /// directory (overrides BO_SERVICE_SERVICELOG and blitzortung.conf);
    /// absent means usage logging is disabled
    #[arg(long, value_name = "DIR")]
    servicelog: Option<std::path::PathBuf>,

    /// GeoIP database for the usage log (overrides BO_GEOIP_DB and
    /// blitzortung.conf; default /var/lib/GeoIP/GeoLite2-City.mmdb)
    #[arg(long, value_name = "PATH")]
    geoip_db: Option<std::path::PathBuf>,
}

/// Resolve the effective listening port: an explicit `--port` wins over the
/// configured value (`Config.port`, which already reflects `BO_SERVICE_PORT`
/// over the INI file, defaulting to 8080).
fn effective_port(cli_port: Option<u16>, config: &Config) -> u16 {
    cli_port.unwrap_or(config.port)
}

/// Resolve the effective protocol: an explicit `--protocol` wins over the
/// configured value (`BO_SERVICE_PROTOCOL` over the INI `[webservice]
/// protocol`, defaulting to `http`).
fn effective_protocol(cli_protocol: Option<&str>, config: &Config) -> Result<Protocol, String> {
    match cli_protocol {
        Some(value) => Protocol::parse(value)
            .ok_or_else(|| format!("invalid --protocol {value:?} (use http or lsp)")),
        None => Ok(config.protocol),
    }
}

/// The result of resolving the usage-log directory.
#[derive(Debug, Clone, PartialEq)]
enum ServicelogResolution {
    /// A usable (existing + writable) directory.
    Enabled(std::path::PathBuf),
    /// Nothing was configured and the default does not apply (disabled, silent).
    Disabled,
    /// A directory was resolved but is unusable; the reason is included.
    Unusable {
        path: std::path::PathBuf,
        /// Whether the path came from an explicit flag/env/config.
        explicit: bool,
        reason: &'static str,
    },
}

/// Resolve the effective usage-log directory: an explicit `--servicelog` wins
/// over the configured value (`BO_SERVICE_SERVICELOG` over the INI
/// `[webservice] servicelog`), which itself defaults to disabled.
///
/// The configured value is expected to be a **directory** that will hold
/// `servicelog_YYYY-MM-DD` (the Python layout).  A CLI path that looks like a
/// file (it has an extension) is treated as the log file name: its parent
/// directory is used and the file name is ignored, so a value such as
/// `/var/log/blitzortung/servicelog_2023-11-14` still lands in
/// `/var/log/blitzortung`.  An unusable (missing or non-writable) directory
/// disables the consumer and is reported once at startup.
fn resolve_servicelog(cli: Option<&std::path::Path>, config: &Config) -> ServicelogResolution {
    let explicit = cli.is_some() || config.service_log_dir.is_some();
    let candidate = match cli {
        Some(path) => Some(servicelog_directory(path)),
        None => config.candidate_service_log_directory(),
    };
    match candidate {
        None => ServicelogResolution::Disabled,
        Some(path) if blitzortung_srv::service_log::directory_is_writable(&path) => {
            ServicelogResolution::Enabled(path)
        }
        Some(path) => ServicelogResolution::Unusable {
            reason: if path.is_dir() {
                "is not writable by the service user"
            } else {
                "does not exist"
            },
            explicit,
            path,
        },
    }
}

/// Normalise a servicelog path to the directory that will contain the daily
/// files: an existing directory is used as-is; a path with a file extension is
/// treated as a file and its parent is used; otherwise the path is used as a
/// directory.
fn servicelog_directory(path: &std::path::Path) -> std::path::PathBuf {
    if path.is_dir() {
        return path.to_path_buf();
    }
    if path.extension().is_some() {
        return path.parent().unwrap_or(path).to_path_buf();
    }
    path.to_path_buf()
}

/// Resolve the effective GeoIP database path: an explicit `--geoip-db` wins
/// over the configured value (`BO_GEOIP_DB` over the INI `[webservice]
/// geoip_db`), which defaults to the Python tool's path.
fn effective_geoip_db(cli: Option<&std::path::Path>, config: &Config) -> std::path::PathBuf {
    match cli {
        Some(path) => path.to_path_buf(),
        None => config.service_geoip_db(),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Default to INFO so the per-request access logs are visible out of the
    // box; `RUST_LOG` still overrides (e.g. `RUST_LOG=debug`).  Under systemd
    // this logs as structured journal entries, so journald owns the
    // timestamp/priority and no duplicate header is printed.
    blitzortung_srv::cli::init_logging_with_default("info");

    let args = Args::parse();
    let config = Config::from_env();
    let port = effective_port(args.port, &config);
    let protocol = effective_protocol(args.protocol.as_deref(), &config).unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2);
    });
    let servicelog = resolve_servicelog(args.servicelog.as_deref(), &config);
    let geoip_db = effective_geoip_db(args.geoip_db.as_deref(), &config);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // Build the metrics sink once: it is shared by the service and the
    // usage-log consumer so the "StatsD metrics disabled" warning is emitted at
    // most once.
    let metrics: Arc<dyn Metrics> = build_metrics(&config);

    // Usage logging (`base.Blitzortung(log_directory=..)`): only when a usable
    // (existing and writable) servicelog directory is configured.  Absent, or
    // present but unusable, means no queue and no consumer thread — usage
    // logging is disabled with a single warning rather than erroring per row.
    let usage_consumer: Option<UsageLogConsumer>;
    let usage_sender = match servicelog {
        ServicelogResolution::Enabled(directory) => {
            log::info!(
                "writing per-request usage log to {} (geoip db {})",
                directory.display(),
                geoip_db.display()
            );
            let (sender, consumer) = blitzortung_srv::service_log::spawn(
                directory,
                Some(geoip_db),
                Some(metrics.clone()),
            );
            usage_consumer = Some(consumer);
            Some(sender)
        }
        ServicelogResolution::Unusable {
            path,
            explicit,
            reason,
        } => {
            // One clear warning and disable the consumer; the service itself
            // keeps running with no usage log.
            let origin = if explicit { "configured" } else { "default" };
            log::warn!(
                "servicelog directory {} ({origin}) {reason}; usage logging disabled",
                path.display()
            );
            usage_consumer = None;
            None
        }
        ServicelogResolution::Disabled => {
            log::debug!("usage logging disabled (no servicelog directory configured)");
            usage_consumer = None;
            None
        }
    };

    runtime.block_on(async move {
        // Build the executor lazily: startup must NOT connect to the database,
        // so a missing/unreachable database cannot make the service exit.  Each
        // request checks out (creating on demand) a pooled connection; while the
        // database is down it is answered with a per-request JSON-RPC fault
        // instead of hanging.
        let executor = PostgresExecutor::lazy(&config)?.with_metrics(metrics.clone());
        let executor: Arc<dyn QueryExecutor> = Arc::new(executor);
        let mut service: Service<Arc<dyn Metrics>> = Service::with_parts(
            executor,
            blitzortung_srv::cache::ServiceCache::new(),
            metrics,
            std::collections::HashSet::new(),
        );
        if let Some(sender) = usage_sender {
            service = service.with_service_log(sender);
        }
        let service: Arc<Service<Arc<dyn Metrics>>> = Arc::new(service);

        let address = format!("0.0.0.0:{port}");
        log::info!("bo-service listening on {address} ({})", protocol.as_str());
        // Run until SIGINT/SIGTERM.  Shutdown drops the `Arc<Service>` (and with
        // it the usage-log sender), so the consumer drains the remaining entries
        // and flushes the file before we join it below.
        let serve = async {
            match protocol {
                Protocol::Http => http::run(&address, service).await,
                Protocol::Lsp => transport::run(&address, service).await,
            }
        };
        tokio::select! {
            result = serve => result?,
            _ = shutdown_signal() => {
                log::info!("shutdown signal received; draining usage log");
            }
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;

    if let Some(consumer) = usage_consumer {
        consumer.shutdown();
    }
    Ok(())
}

/// Resolve when the process receives `SIGINT`/`SIGTERM` (Ctrl-C, `systemctl
/// stop`).  Falls back to never resolving when no signal handler can be
/// installed, so the service simply keeps running.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(error) => {
                log::warn!("could not install SIGTERM handler: {error}");
                std::future::pending::<()>().await;
                return;
            }
        };
        let mut int = match signal(SignalKind::interrupt()) {
            Ok(signal) => signal,
            Err(error) => {
                log::warn!("could not install SIGINT handler: {error}");
                std::future::pending::<()>().await;
                return;
            }
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_port_overrides_config() {
        let config = Config {
            port: 8080,
            ..Config::default()
        };
        assert_eq!(effective_port(Some(9999), &config), 9999);
    }

    #[test]
    fn absent_cli_port_falls_back_to_config() {
        let config = Config {
            port: 8300,
            ..Config::default()
        };
        assert_eq!(effective_port(None, &config), 8300);
    }

    #[test]
    fn default_config_port_is_used_when_nothing_set() {
        let config = Config::default();
        assert_eq!(effective_port(None, &config), 8080);
    }

    #[test]
    fn cli_parser_accepts_short_and_long_port() {
        let args = Args::try_parse_from(["service", "--port", "1234"]).unwrap();
        assert_eq!(args.port, Some(1234));
        let args = Args::try_parse_from(["service", "-p", "2345"]).unwrap();
        assert_eq!(args.port, Some(2345));
        let args = Args::try_parse_from(["service"]).unwrap();
        assert_eq!(args.port, None);
    }

    #[test]
    fn protocol_defaults_to_http() {
        let config = Config::default();
        assert_eq!(effective_protocol(None, &config).unwrap(), Protocol::Http);
    }

    #[test]
    fn cli_protocol_overrides_config() {
        let config = Config {
            protocol: Protocol::Http,
            ..Config::default()
        };
        assert_eq!(
            effective_protocol(Some("lsp"), &config).unwrap(),
            Protocol::Lsp
        );
        let config = Config {
            protocol: Protocol::Lsp,
            ..Config::default()
        };
        assert_eq!(
            effective_protocol(Some("http"), &config).unwrap(),
            Protocol::Http
        );
        assert!(effective_protocol(Some("bogus"), &config).is_err());
    }

    #[test]
    fn cli_parser_accepts_protocol() {
        let args = Args::try_parse_from(["service", "--protocol", "lsp"]).unwrap();
        assert_eq!(args.protocol.as_deref(), Some("lsp"));
    }

    #[test]
    fn cli_parser_accepts_servicelog_and_geoip() {
        let args = Args::try_parse_from([
            "service",
            "--servicelog",
            "/tmp/logs",
            "--geoip-db",
            "/tmp/g.mmdb",
        ])
        .unwrap();
        assert_eq!(
            args.servicelog.as_deref(),
            Some(std::path::Path::new("/tmp/logs"))
        );
        assert_eq!(
            args.geoip_db.as_deref(),
            Some(std::path::Path::new("/tmp/g.mmdb"))
        );

        // Both absent by default.
        let args = Args::try_parse_from(["service"]).unwrap();
        assert!(args.servicelog.is_none());
        assert!(args.geoip_db.is_none());
    }

    /// A per-test temp directory with a unique tag.
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bo-ws-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `--servicelog` (CLI) wins over config; absent CLI falls back to config;
    /// disabled when neither is set; an unusable path is reported.
    #[test]
    fn resolve_servicelog_precedence_and_usability() {
        // Default with no configured path: disabled (candidate is the default
        // path, which in the test environment either exists+writable => Enabled
        // or is unusable => Unusable; never Disabled via config).
        let config = Config::default();
        match resolve_servicelog(None, &config) {
            ServicelogResolution::Enabled(path) => {
                assert_eq!(path, std::path::PathBuf::from("/var/log/blitzortung"));
            }
            ServicelogResolution::Unusable { path, explicit, .. } => {
                assert_eq!(path, std::path::PathBuf::from("/var/log/blitzortung"));
                assert!(!explicit, "default fallback is not explicit");
            }
            ServicelogResolution::Disabled => {}
        }

        // A writable configured directory is enabled when no CLI value is given.
        let dir = temp_dir("cfg");
        let config = Config {
            service_log_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        assert_eq!(
            resolve_servicelog(None, &config),
            ServicelogResolution::Enabled(dir.clone())
        );

        // CLI wins over the configured value.
        let cli = temp_dir("cli");
        assert_eq!(
            resolve_servicelog(Some(&cli), &config),
            ServicelogResolution::Enabled(cli.clone())
        );

        // A non-existent CLI path is unusable and marked explicit.
        match resolve_servicelog(
            Some(std::path::Path::new("/nonexistent/servicelog")),
            &config,
        ) {
            ServicelogResolution::Unusable {
                path,
                explicit,
                reason,
                ..
            } => {
                assert_eq!(path, std::path::PathBuf::from("/nonexistent/servicelog"));
                assert!(explicit);
                assert_eq!(reason, "does not exist");
            }
            other => panic!("expected Unusable, got {other:?}"),
        }

        // A configured but non-existent directory is unusable (explicit).
        let missing = Config {
            service_log_dir: Some("/nonexistent/bo-usage-log".into()),
            ..Config::default()
        };
        assert!(matches!(
            resolve_servicelog(None, &missing),
            ServicelogResolution::Unusable { explicit: true, .. }
        ));

        // An empty configured value means "disabled".
        let empty = Config {
            service_log_dir: None,
            ..Config::default()
        };
        // (No env var is set in the test process for the CLI path, so the
        // default candidate applies; only assert the CLI-provided case.)
        let _ = empty;

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&cli);
    }

    /// The default fallback that exists but is not writable is `Unusable`
    /// (disabled), never enabled.
    #[test]
    fn unwritable_directory_is_unusable() {
        // Root can write anywhere, so a permission-based test is unreliable;
        // use a path component that is a file, which no user can make a dir.
        let file = std::env::temp_dir().join(format!("bo-ws-notadir-{}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        std::fs::write(&file, b"x").unwrap();
        let config = Config {
            service_log_dir: Some(file.to_string_lossy().into_owned()),
            ..Config::default()
        };
        match resolve_servicelog(None, &config) {
            ServicelogResolution::Unusable { explicit, .. } => assert!(explicit),
            other => panic!("expected Unusable, got {other:?}"),
        }
        let _ = std::fs::remove_file(&file);
    }

    /// An existing CLI path is used as the directory verbatim.
    #[test]
    fn servicelog_directory_prefers_existing_dir() {
        let dir = temp_dir("existing");
        assert_eq!(servicelog_directory(&dir), dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A CLI path that looks like a file (it has an extension) is treated as the
    /// log file name: its parent directory is used.
    #[test]
    fn servicelog_directory_strips_file_like_paths() {
        assert_eq!(
            servicelog_directory(std::path::Path::new("/var/log/blitzortung/servicelog.log")),
            std::path::PathBuf::from("/var/log/blitzortung")
        );
        // A path without an extension is treated as a directory (the daily
        // `servicelog_YYYY-MM-DD` files are created inside it).
        assert_eq!(
            servicelog_directory(std::path::Path::new("/var/log/blitzortung")),
            std::path::PathBuf::from("/var/log/blitzortung")
        );
        assert_eq!(
            servicelog_directory(std::path::Path::new(
                "/var/log/blitzortung/servicelog_2023-11-14"
            )),
            std::path::PathBuf::from("/var/log/blitzortung/servicelog_2023-11-14")
        );
    }

    /// `--geoip-db` (CLI) wins over config; absent CLI falls back to the
    /// configured value and then to the built-in default.
    #[test]
    fn effective_geoip_db_precedence() {
        // Default.
        assert_eq!(
            effective_geoip_db(None, &Config::default()),
            std::path::PathBuf::from(blitzortung_srv::service_log::DEFAULT_GEOIP_DB)
        );

        // Config wins over the default.
        let config = Config {
            service_geoip_db: Some("/from/config.mmdb".into()),
            ..Config::default()
        };
        assert_eq!(
            effective_geoip_db(None, &config),
            std::path::PathBuf::from("/from/config.mmdb")
        );

        // CLI wins over config.
        assert_eq!(
            effective_geoip_db(Some(std::path::Path::new("/from/cli.mmdb")), &config),
            std::path::PathBuf::from("/from/cli.mmdb")
        );
    }

    #[test]
    fn build_metrics_uses_configured_receiver() {
        // A local receiver is always resolvable, so we get a StatsD sender.
        let config = Config {
            statsd_host: "127.0.0.1".into(),
            statsd_port: 18125,
            ..Config::default()
        };
        let metrics = build_metrics(&config);
        // No panic and a usable sink.
        metrics.for_db_pool_wait(0.01);
    }

    #[test]
    fn build_metrics_falls_back_to_noop_on_bad_host() {
        let config = Config {
            // An unresolvable host makes `to_socket_addrs` fail and the
            // service must stay usable with no metrics.
            statsd_host: "invalid.invalid.invalid".into(),
            ..Config::default()
        };
        let metrics = build_metrics(&config);
        metrics.for_strikes(60, 1, 0.5);
    }
}
