//! `bo-service` — Rust port of the blitzortung JSON-RPC webservice.
//!
//! Run:
//!
//! ```text
//! export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
//! cargo run --manifest-path rust/bo-service/Cargo.toml
//! ```
//!
//! Configuration (see [`bo_service::config`]): the `--port` CLI flag,
//! `BO_SERVICE_PORT`, `BO_DB_*` env vars and/or a `BO_CONFIG` INI file.  The
//! listening port is resolved with the precedence **CLI > env > config file >
//! default** (`BO_SERVICE_PORT` and the INI `[webservice] port` are both applied
//! by `Config::from_env`, the CLI flag wins on top).  The server speaks the
//! LSP-style `Content-Length` framing over TCP.

use std::sync::Arc;

use clap::Parser;

use bo_service::config::Config;
use bo_service::executor::QueryExecutor;
use bo_service::postgres::PostgresExecutor;
use bo_service::service::Service;
use bo_service::transport;

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
}

/// Resolve the effective listening port: an explicit `--port` wins over the
/// configured value (`Config.port`, which already reflects `BO_SERVICE_PORT`
/// over the INI file, defaulting to 8080).
fn effective_port(cli_port: Option<u16>, config: &Config) -> u16 {
    cli_port.unwrap_or(config.port)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Default to INFO so the per-request access logs are visible out of the
    // box; `RUST_LOG` still overrides (e.g. `RUST_LOG=debug`).
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();
    let config = Config::from_env();
    let port = effective_port(args.port, &config);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async move {
        let executor = PostgresExecutor::connect(&config)
            .await
            .map_err(|e| format!("failed to connect to database: {e}"))?;
        let executor: Arc<dyn QueryExecutor> = Arc::new(executor);
        let service: Arc<Service> = Arc::new(Service::new(executor));

        let address = format!("0.0.0.0:{port}");
        log::info!("bo-service listening on {address}");
        transport::run(&address, service).await?;
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
}