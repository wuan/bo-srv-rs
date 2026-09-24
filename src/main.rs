//! `bo-service` — Rust port of the blitzortung JSON-RPC webservice.
//!
//! Run:
//!
//! ```text
//! export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
//! cargo run --manifest-path rust/bo-service/Cargo.toml
//! ```
//!
//! Configuration (see [`bo_service::config`]): the `--port`/`--protocol` CLI
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

use bo_service::config::{Config, Protocol};
use bo_service::executor::QueryExecutor;
use bo_service::metrics::{Metrics, StatsDMetrics};
use bo_service::{http, postgres::PostgresExecutor, service::Service, transport};

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
            std::sync::Arc::new(bo_service::metrics::NoopMetrics)
        }
    }
}

/// Command-line options for the `service` binary.
#[derive(Parser, Debug)]
#[command(
    name = "service",
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
        Some(value) => {
            Protocol::parse(value).ok_or_else(|| format!("invalid --protocol {value:?} (use http or lsp)"))
        }
        None => Ok(config.protocol),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Default to INFO so the per-request access logs are visible out of the
    // box; `RUST_LOG` still overrides (e.g. `RUST_LOG=debug`).
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();
    let config = Config::from_env();
    let port = effective_port(args.port, &config);
    let protocol = effective_protocol(args.protocol.as_deref(), &config)
        .unwrap_or_else(|error| {
            eprintln!("{error}");
            std::process::exit(2);
        });

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async move {
        let executor = PostgresExecutor::connect(&config)
            .await
            .map_err(|e| format!("failed to connect to database: {e}"))?;
        let executor: Arc<dyn QueryExecutor> = Arc::new(executor);
        let metrics: Arc<dyn Metrics> = build_metrics(&config);
        let service: Arc<Service<Arc<dyn Metrics>>> = Arc::new(Service::with_parts(
            executor,
            bo_service::cache::ServiceCache::new(),
            metrics,
            std::collections::HashSet::new(),
        ));

        let address = format!("0.0.0.0:{port}");
        log::info!("bo-service listening on {address} ({})", protocol.as_str());
        match protocol {
            Protocol::Http => http::run(&address, service).await?,
            Protocol::Lsp => transport::run(&address, service).await?,
        }
        Ok(())
    })
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
        assert_eq!(effective_protocol(Some("lsp"), &config).unwrap(), Protocol::Lsp);
        let config = Config {
            protocol: Protocol::Lsp,
            ..Config::default()
        };
        assert_eq!(effective_protocol(Some("http"), &config).unwrap(), Protocol::Http);
        assert!(effective_protocol(Some("bogus"), &config).is_err());
    }

    #[test]
    fn cli_parser_accepts_protocol() {
        let args = Args::try_parse_from(["service", "--protocol", "lsp"]).unwrap();
        assert_eq!(args.protocol.as_deref(), Some("lsp"));
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
