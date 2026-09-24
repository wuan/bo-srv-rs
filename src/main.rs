//! `bo-service` — Rust port of the blitzortung JSON-RPC webservice.
//!
//! Run:
//!
//! ```text
//! export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
//! cargo run --manifest-path rust/bo-service/Cargo.toml
//! ```
//!
//! Configuration (see [`crate::config`]): `BO_SERVICE_PORT`, `BO_DB_*` env
//! vars and/or a `BO_CONFIG` INI file.  The server speaks the LSP-style
//! `Content-Length` framing over TCP.

use std::sync::Arc;

use bo_service::config::Config;
use bo_service::executor::QueryExecutor;
use bo_service::postgres::PostgresExecutor;
use bo_service::service::Service;
use bo_service::transport;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let config = Config::from_env();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async move {
        let executor = PostgresExecutor::connect(&config)
            .await
            .map_err(|e| format!("failed to connect to database: {e}"))?;
        let executor: Arc<dyn QueryExecutor> = Arc::new(executor);
        let service: Arc<Service> = Arc::new(Service::new(executor));

        let address = format!("0.0.0.0:{}", config.port);
        log::info!("bo-service listening on {address}");
        transport::run(&address, service).await?;
        Ok(())
    })
}