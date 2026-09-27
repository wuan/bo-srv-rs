//! World-map rendering for the servicelog statistics (issues #24 and #28).
//!
//! This module owns everything map related:
//!
//! * [`ascii`] — the shared 5-degree raster ([`AsciiWorldMap`]) and its ASCII
//!   rendering, used by `--format text` / `--format map` and by the SVG maps.
//! * [`world`] — the embedded Natural Earth 110m land basemap and the
//!   equirectangular projection ([`world::project`], [`world::continent_layer`]).
//! * [`svg`] — the SVG renderers: the standalone local-query scatter
//!   ([`svg::render_local_svg`]) and the geographic world maps used by the HTML
//!   report ([`svg::render_world_svg`]).
//!
//! The map functions read [`crate::service_log_stats::ServiceLogStats`] /
//! [`crate::service_log_stats::LocalQuery`]; the statistics module in turn calls
//! back into this module to render the maps it embeds in the text and HTML
//! reports.

pub mod ascii;
pub mod svg;
pub mod world;

pub use ascii::{
    render_ascii_map, render_ascii_maps, AsciiWorldMap, MAP_RAMP, RASTER_DEGREES, WORLD_COLS,
    WORLD_ROWS,
};
pub use svg::{render_local_svg, render_world_map_svg, render_world_svg};
pub use world::{continent_layer, land_rings, project, MAP_HEIGHT, MAP_WIDTH};
