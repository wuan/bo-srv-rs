//! Strike cluster detection, ported from the Python `blitzortung.clustering`
//! package (last full version at commit `8ba0117`, "fixed tests", 2016).
//!
//! * [`pdist`] — all-pairs great-circle distances (`clustering/pdist.pyx`).
//! * [`base::Clustering`] — the hierarchical single-linkage agglomeration and
//!   the hull/buffer/simplify geometry pipeline (`clustering/base.py`).
//! * [`geometry`] — the `geo`/`geographiclib-rs` replacements for
//!   `shapely`/`geographiclib`.
//!
//! The clustering is exposed as a library so it can be exercised by the
//! `bo-cluster` CLI without touching the database schema.

pub mod base;
pub mod geometry;
pub mod pdist;

pub use base::{single_linkage, Clustering, Merge};
pub use pdist::{distance, pdist, EARTH_RADIUS_KM};
