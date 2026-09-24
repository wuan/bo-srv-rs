//! Rust port of the blitzortung JSON-RPC webservice/service layer.
//!
//! The original implementation lives in the Python package `blitzortung`
//! (this repository) and is a Twisted-based service layer that is dispatched
//! to by the JSON-RPC server in the separate `wuan/bo-server` repository.
//!
//! This crate ports:
//!
//! * the JSON-RPC transport using LSP-style `Content-Length` framing
//!   (`Content-Length: <n>\r\n\r\n<json-body>`) with async I/O (Tokio),
//! * the service/query handlers (`strikes`, `strikes_grid`,
//!   `global_strikes_grid`, `histogram`) with response shapes identical to the
//!   Python implementation (`s`, `next`, `t`, `h`, `r`, `xd`, `yd`, `x0`,
//!   `y1`, `xc`, `yc`, `dt`),
//! * the SQL generation equivalent to `blitzortung/db/query_builder.py` and
//!   `blitzortung/db/query.py` (select / grid / global-grid / histogram),
//! * the grid/geometry parameters from `blitzortung/geom.py` including the
//!   UTM-based `GridFactory` used by `wuan/bo-server` to derive grid cell
//!   sizes (ported from PROJ's Poder/Engsager implementation).
//!
//! The service layer depends on the [`QueryExecutor`](executor::QueryExecutor)
//! trait so it can be tested without a live PostgreSQL database; a mock
//! executor is provided in the [`mock`] module.

pub mod cache;
pub mod metrics;
pub mod builder;
pub mod config;
pub mod data;
pub mod dataimport;
pub mod db;
pub mod executor;
pub mod geom;
pub mod jsonrpc;
pub mod mock;
pub mod postgres;
pub mod query;
pub mod round;
pub mod service;
pub mod transport;
pub mod util;
pub mod websocket;
pub mod wkb;

pub use executor::{Param, QueryExecutor, Row, Value};