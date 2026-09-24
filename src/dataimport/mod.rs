//! Data import helpers (port of `blitzortung/dataimport/`).
//!
//! * [`base`] — URL path construction and the HTTP/file transports,
//! * [`strike`] — the provider that downloads protected strike logs and builds
//!   strikes.

pub mod base;
pub mod strike;

pub use base::{
    BlitzortungDataPath, BlitzortungDataPathGenerator, FileTransport, HttpFileTransport, Transport,
    TransportError,
};
pub use strike::{ImportError, StrikesBlitzortungDataProvider};