//! Strike cluster detection, ported from the Python `blitzortung.clustering`
//! package (last full version at commit `8ba0117`, "fixed tests", 2016).
//!
//! * [`pdist`] — all-pairs great-circle distances (`clustering/pdist.pyx`).
//! * [`base::Clustering`] — the hierarchical single-linkage agglomeration, the
//!   hull/buffer/simplify geometry pipeline (`clustering/base.py`) and the
//!   merging of overlapping same-timestamp cluster areas.
//! * [`geometry`] — the `geo`/`geographiclib-rs` replacements for
//!   `shapely`/`geographiclib`.
//!
//! The single-linkage cut looks only at strike-to-strike distance while the
//! emitted areas are the *buffered* hulls, so two detected clusters can be
//! farther apart than the cut yet have overlapping areas (an elongated or
//! enclosing hull reaches over a separate group).  The port therefore merges
//! every same-interval cluster whose buffered area overlaps, by polygon union
//! (with a convex hull fallback when the union is not a single ring), *before*
//! emitting — a Python difference documented in the top-level `README`.
//!
//! The clustering is exposed as a library so it can be exercised by the
//! `bo-cluster` CLI without touching the database schema.

pub mod base;
pub mod geometry;
pub mod pdist;

pub use base::{single_linkage, Clustering, Merge};
pub use pdist::{distance, pdist, EARTH_RADIUS_KM};
