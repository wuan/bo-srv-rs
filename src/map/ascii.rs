//! ASCII world-map raster of the local-query counts (issue #24).
//!
//! The statistics renderer prints one ASCII map per query category (offline and
//! interactive); this module owns the shared 5-degree raster and its text
//! rendering.  The same [`AsciiWorldMap`] grid also backs the geographic SVG
//! maps (see [`crate::map::svg`]).

use crate::service_log_stats::{LocalQuery, ServiceLogStats, OFFLINE_MINUTE_LENGTH};

/// The number of base-raster columns spanning the world (5-degree cells):
/// `360 / 5 = 72`.
pub const WORLD_COLS: usize = 72;

/// The number of base-raster rows spanning the world (5-degree cells):
/// `180 / 5 = 36`.
pub const WORLD_ROWS: usize = 36;

/// The base raster cell size in degrees (issue #24: "use 5 as the basic
/// raster").
pub const RASTER_DEGREES: i64 = 5;

/// Density ramp for the ASCII map (light to dense), matching the symbol set of
/// `data::GridData::to_map` (`" .-o*O8"`) plus a final `#`.  Index 0 (`' '`) is
/// reserved for a zero count, so non-zero counts use indices 1..=7.
pub const MAP_RAMP: [char; 8] = [' ', '.', '-', 'o', '*', 'O', '8', '#'];

/// A 5-degree worldwide raster of local-query counts, rendered as ASCII
/// (issue #24).
///
/// ## Footprint
///
/// A local query at tile `(x, y)` with data area `data_area` covers
/// `data_area` degrees starting at the grid origin
/// `((x-1) * data_area, (y-1) * data_area)` (see [`crate::geom::LocalGrid`]).
/// In the base 5-degree raster that is an `n x n` block with `n = data_area / 5`
/// (so `data_area=5` marks one cell, `10` a `2x2`, `15` a `3x3` and `20` a
/// `4x4` block).  Every cell of the block is incremented, so overlapping
/// queries accumulate ("higher data areas can be added on top").
///
/// ## Layout
///
/// Internally rows are stored south-up (row 0 covers latitude `-90..-85`) so
/// the arithmetic reads naturally; [`render`](Self::render) prints them
/// north-up (row 0 = the top line) like `data::GridData::to_map`.  Columns wrap
/// across the antimeridian; rows outside the poles are clamped away (never
/// counted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsciiWorldMap {
    /// `WORLD_ROWS` rows (south-up) of `WORLD_COLS` counts.
    cells: Vec<Vec<u64>>,
    /// The total number of queries that contributed.
    queries: u64,
}

impl Default for AsciiWorldMap {
    fn default() -> Self {
        AsciiWorldMap {
            cells: vec![vec![0; WORLD_COLS]; WORLD_ROWS],
            queries: 0,
        }
    }
}

impl AsciiWorldMap {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of contributing queries.
    pub fn queries(&self) -> u64 {
        self.queries
    }

    /// The count at `(col, row)` (row 0 = southernmost); `0` when out of range.
    pub fn count(&self, col: usize, row: usize) -> u64 {
        self.cells
            .get(row)
            .and_then(|r| r.get(col))
            .copied()
            .unwrap_or(0)
    }

    /// The total count across all cells.
    pub fn total(&self) -> u64 {
        self.cells.iter().flatten().sum()
    }

    /// The largest single-cell count.
    pub fn maximum(&self) -> u64 {
        self.cells.iter().flatten().copied().max().unwrap_or(0)
    }

    /// Add one local query's footprint to the raster.
    ///
    /// `x`/`y` are the local-grid tile indices (1-based; `y` may be `<= 0` in
    /// the southern hemisphere).  `data_area` is the tile size in degrees,
    /// clamped to at least [`RASTER_DEGREES`]; the footprint side is
    /// `ceil(data_area / 5)` cells.
    pub fn add_local_query(&mut self, x: i64, y: i64, data_area: i64) {
        self.queries += 1;
        let data_area = data_area.max(RASTER_DEGREES);
        // Footprint side in base cells (ceil division; `data_area` is positive).
        let side = ((data_area + RASTER_DEGREES - 1) / RASTER_DEGREES).clamp(1, WORLD_COLS as i64)
            as usize;

        // The grid origin (lower-left corner) in degrees.
        let lon0 = (x - 1) * data_area;
        let lat0 = (y - 1) * data_area;

        // Column of the origin, wrapping across the antimeridian.
        let col0 = (lon0 + 180).div_euclid(RASTER_DEGREES);
        // Row of the origin (south-up), clamped away from out-of-range poles.
        let row0 = (lat0 + 90).div_euclid(RASTER_DEGREES);

        for d_row in 0..side as i64 {
            let row = row0 + d_row;
            if !(0..WORLD_ROWS as i64).contains(&row) {
                continue;
            }
            for d_col in 0..side as i64 {
                let col = (col0 + d_col).rem_euclid(WORLD_COLS as i64) as usize;
                self.cells[row as usize][col] += 1;
            }
        }
    }

    /// Add every local query of `stats` to the map.
    pub fn from_local_queries(queries: &[LocalQuery]) -> Self {
        let mut map = Self::new();
        for query in queries {
            map.add_local_query(query.x, query.y, query.data_area);
        }
        map
    }

    /// Add only the queries matching `keep` to the map.
    pub fn from_local_queries_filtered(
        queries: &[LocalQuery],
        keep: impl Fn(&LocalQuery) -> bool,
    ) -> Self {
        let mut map = Self::new();
        for query in queries.iter().filter(|q| keep(q)) {
            map.add_local_query(query.x, query.y, query.data_area);
        }
        map
    }

    /// Render the map as ASCII with the default title
    /// (`local query world map`).
    pub fn render(&self) -> String {
        self.render_titled("local query world map")
    }

    /// Render the map as ASCII, framed with a `+`/`-`/`|` border and the
    /// density ramp [`MAP_RAMP`], north-up, using `title` as the header.
    ///
    /// The ramp is scaled to the largest cell count: `0` renders as a space and
    /// the densest cell as `#`.  A header with the query/hit counts and a
    /// legend are printed around the frame.
    pub fn render_titled(&self, title: &str) -> String {
        let maximum = self.maximum();
        let mut out = String::new();

        out.push_str(&format!(
            "{title} ({}x{} cells of {} degrees, {} queries, {} hits)\n",
            WORLD_COLS,
            WORLD_ROWS,
            RASTER_DEGREES,
            self.queries,
            self.total()
        ));

        let border = format!("+{}+", "-".repeat(WORLD_COLS));
        out.push_str(&border);
        out.push('\n');

        // Print north-up: the last internal row (highest latitude) first.
        for row_index in (0..WORLD_ROWS).rev() {
            out.push('|');
            for col in 0..WORLD_COLS {
                out.push(self.symbol(self.cells[row_index][col], maximum));
            }
            out.push_str("|\n");
        }

        out.push_str(&border);
        out.push('\n');
        out.push_str(&format!(
            "legend: '{}' = 0, '{}' = max ({}); columns 5 degrees from 180W, rows 5 degrees from 90S\n",
            MAP_RAMP[0], MAP_RAMP[MAP_RAMP.len() - 1], maximum
        ));
        out
    }

    /// The ramp symbol for `count` given the map `maximum`: `0` is a space and
    /// the maximum is the last ramp symbol.
    fn symbol(&self, count: u64, maximum: u64) -> char {
        if count == 0 || maximum == 0 {
            return MAP_RAMP[0];
        }
        // Scale into 1..=len-1 so a non-zero count never renders as blank.
        let steps = MAP_RAMP.len() - 1;
        let index = ((count as f64 / maximum as f64) * steps as f64).ceil() as usize;
        MAP_RAMP[1 + index.min(steps - 1)]
    }
}

/// Render the local queries as ASCII world maps (5-degree raster, issue #24):
/// one for the **offline** queries (a fixed 10-minute window) and a separate
/// one for the **interactive** queries (any longer window).
pub fn render_ascii_map(day: &str, stats: &ServiceLogStats) -> String {
    format!(
        "servicelog local-query maps for {day}\n{}",
        render_ascii_maps(stats)
    )
}

/// The two ASCII world maps (offline and interactive), each labelled and
/// separated by a blank line.
pub fn render_ascii_maps(stats: &ServiceLogStats) -> String {
    let offline =
        AsciiWorldMap::from_local_queries_filtered(&stats.local_queries, |q| q.is_offline());
    let interactive =
        AsciiWorldMap::from_local_queries_filtered(&stats.local_queries, |q| q.is_interactive());
    format!(
        "offline queries (minute_length == {}):\n{}\ninteractive queries (minute_length > {}):\n{}",
        OFFLINE_MINUTE_LENGTH,
        offline.render_titled("offline local-query world map"),
        OFFLINE_MINUTE_LENGTH,
        interactive.render_titled("interactive local-query world map"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_map_is_blank() {
        let map = AsciiWorldMap::new();
        assert_eq!(map.queries(), 0);
        assert_eq!(map.total(), 0);
        assert_eq!(map.maximum(), 0);
        assert_eq!(map.count(0, 0), 0);
        assert_eq!(map.count(WORLD_COLS + 1, WORLD_ROWS + 1), 0);
    }

    #[test]
    fn a_five_degree_query_marks_one_cell() {
        let mut map = AsciiWorldMap::new();
        // Origin (5E, 45N) -> col floor((5+180)/5)=37, row floor((45+90)/5)=27.
        map.add_local_query(2, 10, 5);
        assert_eq!(map.queries(), 1);
        assert_eq!(map.total(), 1);
        assert_eq!(map.maximum(), 1);
        assert_eq!(map.count(37, 27), 1);
    }

    #[test]
    fn a_ten_degree_query_marks_four_cells() {
        let mut map = AsciiWorldMap::new();
        map.add_local_query(1, 1, 10);
        assert_eq!(map.total(), 4);
        assert_eq!(map.count(36, 18), 1);
        assert_eq!(map.count(37, 18), 1);
        assert_eq!(map.count(36, 19), 1);
        assert_eq!(map.count(37, 19), 1);
    }

    #[test]
    fn overlapping_queries_accumulate() {
        let mut map = AsciiWorldMap::new();
        map.add_local_query(1, 1, 5);
        map.add_local_query(1, 1, 5);
        assert_eq!(map.queries(), 2);
        assert_eq!(map.total(), 2);
        assert_eq!(map.maximum(), 2);
    }

    #[test]
    fn out_of_range_rows_are_dropped() {
        let mut map = AsciiWorldMap::new();
        // A tile far below the south pole.
        map.add_local_query(1, -100, 5);
        assert_eq!(map.queries(), 1);
        assert_eq!(map.total(), 0, "row outside the raster is dropped");
    }

    #[test]
    fn columns_wrap_across_the_antimeridian() {
        let mut map = AsciiWorldMap::new();
        // lon0 = (37-1)*5 = 180 -> col (180+180)/5 = 72 -> wraps to 0.
        map.add_local_query(37, 1, 5);
        assert_eq!(map.count(0, 18), 1);
    }

    #[test]
    fn from_local_queries_and_filtered() {
        let make = |x, y, minute_length| LocalQuery {
            x,
            y,
            data_area: 5,
            grid_baselength: 5000,
            minute_length,
        };
        let queries = [make(1, 1, 10), make(2, 2, 60)];
        let all = AsciiWorldMap::from_local_queries(&queries);
        assert_eq!(all.queries(), 2);
        let offline = AsciiWorldMap::from_local_queries_filtered(&queries, |q| q.is_offline());
        assert_eq!(offline.queries(), 1);
        let interactive =
            AsciiWorldMap::from_local_queries_filtered(&queries, |q| q.is_interactive());
        assert_eq!(interactive.queries(), 1);
    }

    #[test]
    fn render_is_framed_north_up_and_sized() {
        let mut map = AsciiWorldMap::new();
        map.add_local_query(2, 10, 5);
        let rendered = map.render_titled("test map");
        assert!(
            rendered.starts_with("test map (72x36 cells of 5 degrees"),
            "{rendered}"
        );
        let lines: Vec<&str> = rendered.lines().collect();
        let border = format!("+{}+", "-".repeat(WORLD_COLS));
        assert_eq!(lines[1], border);
        let body = &lines[2..2 + WORLD_ROWS];
        assert_eq!(body.len(), WORLD_ROWS);
        for row in body {
            assert_eq!(row.len(), WORLD_COLS + 2);
            assert!(row.starts_with('|') && row.ends_with('|'));
        }
        // row 27 (south-up) draws at north-up index WORLD_ROWS - 1 - 27.
        let expected_line = WORLD_ROWS - 1 - 27;
        assert!(body[expected_line].contains('#'), "{rendered}");
    }

    #[test]
    fn render_uses_the_density_ramp() {
        let mut map = AsciiWorldMap::new();
        map.add_local_query(1, 1, 5); // 1 hit
        map.add_local_query(2, 1, 5); // another hit elsewhere
        map.add_local_query(3, 1, 5);
        map.add_local_query(3, 1, 5); // densest cell: 2 hits
        let rendered = map.render();
        assert!(
            rendered.contains(MAP_RAMP[MAP_RAMP.len() - 1]),
            "{rendered}"
        );
    }
}
