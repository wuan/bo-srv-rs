//! `bo-db` implementation (port of `blitzortung/cli/db.py`).

use chrono::{Duration, Utc};
use clap::Parser;

use crate::cli::{exit_with, parse_local_time, parse_timezone, DATE_FORMAT};
use crate::data::GridData;
use crate::db::StrikeDb;
use crate::executor::QueryExecutor;
use crate::geom::Grid;
use crate::query::{self, Area, TimeInterval};
use crate::round::py_round;
use crate::util::Timer;

/// Default grid cell size (`cli/db.py DEFAULT_GRID`).
pub const DEFAULT_GRID: (f64, f64) = (1.0, 1.0);

/// `bo-db` command-line options (port of `cli/db.py.parse_options`).
///
/// Long names and semantics match the Python `optparse` tool.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "bo-db",
    about = "Query the blitzortung strike database (text or grid output)",
    version
)]
pub struct DbArgs {
    /// start date for data retrieval
    #[arg(long, default_value = "default")]
    pub startdate: String,

    /// start time for data retrieval
    #[arg(long, default_value = "default")]
    pub starttime: String,

    /// end date for data retrieval
    #[arg(long, default_value = "default")]
    pub enddate: String,

    /// end time for data retrieval
    #[arg(long, default_value = "default")]
    pub endtime: String,

    /// area for which strikes are selected
    #[arg(long)]
    pub area: Option<String>,

    /// used timezone
    #[arg(long, default_value = "UTC")]
    pub tz: String,

    /// use envelope of given area for query
    #[arg(long)]
    pub useenv: bool,

    /// srid for query area and results
    #[arg(long, default_value_t = 4326)]
    pub srid: i64,

    /// precision of coordinates
    #[arg(long, default_value_t = 4)]
    pub precision: i32,

    /// grid width
    #[arg(long)]
    pub grid: Option<f64>,

    /// grid x width
    #[arg(long)]
    pub x_grid: Option<f64>,

    /// grid y width
    #[arg(long)]
    pub y_grid: Option<f64>,

    /// show ascii map instead of numerical grid
    #[arg(long)]
    pub map: bool,
}

/// Resolved `bo-db` options.
#[derive(Debug, Clone)]
pub struct DbOptions {
    pub startdate: String,
    pub starttime: String,
    pub enddate: String,
    pub endtime: String,
    pub area: Option<String>,
    pub tz: String,
    pub useenv: bool,
    pub srid: i64,
    pub precision: i32,
    pub grid: Option<f64>,
    pub xgrid: Option<f64>,
    pub ygrid: Option<f64>,
    pub map: bool,
}

impl DbOptions {
    /// `cli/db.py.parse_options` defaults.
    pub fn defaults() -> Self {
        DbOptions {
            startdate: "default".to_string(),
            starttime: "default".to_string(),
            enddate: "default".to_string(),
            endtime: "default".to_string(),
            area: None,
            tz: "UTC".to_string(),
            useenv: false,
            srid: 4326,
            precision: 4,
            grid: None,
            xgrid: None,
            ygrid: None,
            map: false,
        }
    }

    /// Build from the clap-parsed command line.
    pub fn from_args(args: &DbArgs) -> Self {
        DbOptions {
            startdate: args.startdate.clone(),
            starttime: args.starttime.clone(),
            enddate: args.enddate.clone(),
            endtime: args.endtime.clone(),
            area: args.area.clone(),
            tz: args.tz.clone(),
            useenv: args.useenv,
            srid: args.srid,
            precision: args.precision,
            grid: args.grid,
            xgrid: args.x_grid,
            ygrid: args.y_grid,
            map: args.map,
        }
    }
}

/// `cli/db.py.prepare_grid_if_applicable`: return a [`Grid`] when any grid
/// option is set, requiring an `area`.
pub fn prepare_grid_if_applicable(options: &DbOptions, area: Option<&Area>) -> Option<Grid> {
    if options.grid.is_none() && options.xgrid.is_none() && options.ygrid.is_none() {
        return None;
    }
    let (mut grid_x, mut grid_y) = DEFAULT_GRID;
    if let Some(grid) = options.grid {
        grid_x = grid;
        grid_y = grid;
    }
    if let Some(x) = options.xgrid {
        grid_x = x;
    }
    if let Some(y) = options.ygrid {
        grid_y = y;
    }
    let area = match area {
        Some(area) => area,
        None => exit_with("grid options requires declaration of envelope area", 1),
    };
    // `area.envelope.bounds` -> (x_min, y_min, x_max, y_max).
    let (x_min, y_min, x_max, y_max) = area.bounds;
    Some(Grid::new(x_min, x_max, y_min, y_max, grid_x, grid_y))
}

/// Resolve the time interval the way `cli/db.py.main` does.
///
/// Returns `(start, end)` in UTC; `end` may be the fixed `now - 1min` when the
/// user did not override it (`non_default_end` is false), matching the
/// history behaviour where the end time is left implicit.
pub fn resolve_interval(
    options: &DbOptions,
    now: chrono::DateTime<Utc>,
) -> (chrono::DateTime<Utc>, chrono::DateTime<Utc>) {
    let tz = match parse_timezone(&options.tz) {
        Some(tz) => tz,
        None => exit_with(&format!("parse error in timezone \"{}\"", options.tz), 1),
    };

    let start_time = now - Duration::hours(1);
    let end_time = now - Duration::minutes(1);

    let startdate = if options.startdate == "default" {
        start_time
            .with_timezone(&tz)
            .format(DATE_FORMAT)
            .to_string()
    } else {
        options.startdate.clone()
    };
    let starttime = if options.starttime == "default" {
        start_time.with_timezone(&tz).format("%H%M").to_string()
    } else {
        options.starttime.clone()
    };
    let non_default_end = options.enddate != "default" || options.endtime != "default";
    let enddate = if options.enddate == "default" {
        end_time.with_timezone(&tz).format(DATE_FORMAT).to_string()
    } else {
        options.enddate.clone()
    };
    let endtime = if options.endtime == "default" {
        end_time.with_timezone(&tz).format("%H%M").to_string()
    } else {
        options.endtime.clone()
    };

    let parsed_start = match parse_local_time(&startdate, &starttime, tz, false) {
        Some(value) => value,
        None => exit_with(
            &format!("parse error in starttime: '{startdate} {starttime}'"),
            5,
        ),
    };
    let parsed_end = if non_default_end {
        match parse_local_time(&enddate, &endtime, tz, true) {
            Some(value) => value,
            None => exit_with(&format!("parse error in endtime: '{enddate} {endtime}'"), 5),
        }
    } else {
        end_time
    };

    (parsed_start, parsed_end)
}

/// Resolve the WKT area (and `--useenv` envelope promotion).
pub fn resolve_area(options: &DbOptions) -> Option<Area> {
    let area_text = options.area.as_ref()?;
    let area = match query::parse_wkt_polygon(area_text) {
        Some(area) => area,
        None => exit_with(&format!("parse error in area \"{area_text}\""), 1),
    };
    if options.useenv {
        // `area.envelope`: a polygon that is its own envelope.
        let (x_min, y_min, x_max, y_max) = area.bounds;
        Some(Area::from_polygon(&[vec![
            [x_min, y_min],
            [x_min, y_max],
            [x_max, y_max],
            [x_max, y_min],
            [x_min, y_min],
        ]])?)
    } else {
        Some(area)
    }
}

/// `cli/db.py.fetch_strikes`: select strikes, round coordinates to
/// `precision`, and print each strike; write the count/timing to `stderr`.
/// `cli/db.py.fetch_strikes`: select strikes, round coordinates to
/// `precision`, and return one rendered strike per line.
pub async fn fetch_strikes(
    executor: &dyn QueryExecutor,
    options: &DbOptions,
    interval: &TimeInterval,
    area: Option<&Area>,
    tz: chrono_tz::Tz,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let db = StrikeDb::new(executor, options.srid);
    let strikes = db.select(interval, area, None).await?;

    let precision_factor = 10f64.powi(options.precision);
    let mut lines = Vec::with_capacity(strikes.len());
    for mut strike in strikes {
        strike.x = py_round(strike.x * precision_factor, 0) / precision_factor;
        strike.y = py_round(strike.y * precision_factor, 0) / precision_factor;
        // `bo-db --tz` renders the strike timestamps in the selected zone
        // (`strike_db.set_timezone(tz)` + mapper conversion).
        lines.push(strike.to_string_in_tz(tz));
    }
    Ok(lines.join("\n"))
}

/// `cli/db.py.fetch_strikes_grid`: select grid data and return the arcgrid or
/// map text.
pub async fn fetch_strikes_grid(
    executor: &dyn QueryExecutor,
    options: &DbOptions,
    grid: &Grid,
    interval: &TimeInterval,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let db = StrikeDb::new(executor, options.srid);
    let grid_data: GridData = db.select_grid(grid, 0, interval, None).await?;

    if options.map {
        Ok(grid_data.to_map())
    } else {
        Ok(grid_data.to_arcgrid())
    }
}

/// Entry point for the `bo-db` binary, given a connected executor.
pub async fn run(
    executor: &dyn QueryExecutor,
    options: &DbOptions,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let now = Utc::now();
    let tz = parse_timezone(&options.tz)
        .unwrap_or_else(|| exit_with(&format!("parse error in timezone \"{}\"", options.tz), 1));
    let (start, end) = resolve_interval(options, now);
    let interval = TimeInterval::new(start, end);
    let area = resolve_area(options);

    let grid = prepare_grid_if_applicable(options, area.as_ref());
    if let Some(grid) = grid {
        let mut timer = Timer::new();
        let output = fetch_strikes_grid(executor, options, &grid, &interval).await?;
        let select_time = timer.lap();
        println!("{output}");
        eprintln!("received grid data in {select_time:.3} seconds");
    } else {
        let mut timer = Timer::new();
        let output = fetch_strikes(executor, options, &interval, area.as_ref(), tz).await?;
        let select_time = timer.lap();
        if !output.is_empty() {
            println!("{output}");
        }
        let count = output.lines().filter(|line| !line.is_empty()).count();
        eprintln!("received {count} strikes in {select_time:.3} seconds");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{Row, Value};
    use crate::mock::MockExecutor;
    use chrono::TimeZone;

    #[test]
    fn defaults_match_python() {
        let options = DbOptions::defaults();
        assert_eq!(options.startdate, "default");
        assert_eq!(options.tz, "UTC");
        assert_eq!(options.srid, 4326);
        assert_eq!(options.precision, 4);
        assert!(!options.useenv);
        assert!(!options.map);
    }

    #[test]
    fn prepare_grid_none_without_options() {
        let options = DbOptions::defaults();
        assert!(prepare_grid_if_applicable(&options, None).is_none());
    }

    #[test]
    fn prepare_grid_uses_single_grid_value() {
        let mut options = DbOptions::defaults();
        options.grid = Some(0.5);
        let area = Area::from_polygon(&[vec![
            [1.0, 1.0],
            [3.0, 1.0],
            [3.0, 4.0],
            [1.0, 4.0],
            [1.0, 1.0],
        ]])
        .unwrap();
        let grid = prepare_grid_if_applicable(&options, Some(&area)).unwrap();
        assert_eq!(grid.x_div, 0.5);
        assert_eq!(grid.y_div, 0.5);
        assert_eq!(grid.x_min, 1.0);
        assert_eq!(grid.y_min, 1.0);
        assert_eq!(grid.x_max, 3.0);
        assert_eq!(grid.y_max, 4.0);
    }

    #[test]
    fn prepare_grid_x_y_overrides() {
        let mut options = DbOptions::defaults();
        options.xgrid = Some(0.3);
        options.ygrid = Some(0.4);
        let area = Area::from_polygon(&[vec![
            [1.0, 1.0],
            [3.0, 1.0],
            [3.0, 4.0],
            [1.0, 4.0],
            [1.0, 1.0],
        ]])
        .unwrap();
        let grid = prepare_grid_if_applicable(&options, Some(&area)).unwrap();
        assert_eq!(grid.x_div, 0.3);
        assert_eq!(grid.y_div, 0.4);
    }

    #[test]
    fn resolve_interval_defaults_to_last_hour() {
        let options = DbOptions::defaults();
        let now = Utc.with_ymd_and_hms(2025, 1, 1, 12, 30, 0).unwrap();
        let (start, end) = resolve_interval(&options, now);
        // start = now - 1h, end = now - 1min (default end is implicit)
        assert_eq!(start, Utc.with_ymd_and_hms(2025, 1, 1, 11, 30, 0).unwrap());
        assert_eq!(end, Utc.with_ymd_and_hms(2025, 1, 1, 12, 29, 0).unwrap());
    }

    #[test]
    fn resolve_interval_explicit_start() {
        let mut options = DbOptions::defaults();
        options.startdate = "20250101".to_string();
        options.starttime = "1000".to_string();
        let now = Utc.with_ymd_and_hms(2025, 1, 1, 12, 30, 0).unwrap();
        let (start, _) = resolve_interval(&options, now);
        assert_eq!(start, Utc.with_ymd_and_hms(2025, 1, 1, 10, 0, 0).unwrap());
    }

    #[tokio::test]
    async fn fetch_strikes_prints_and_counts() {
        let mut mock = MockExecutor::new();
        mock.add_rows(
            "FROM strikes",
            vec![Row::new(vec![
                Value::Int(1),
                Value::Timestamp(Utc.with_ymd_and_hms(2025, 1, 1, 11, 0, 0).unwrap()),
                Value::Int(0),
                Value::Float(10.123456),
                Value::Float(20.654321),
                Value::Float(100.0),
                Value::Float(10.5),
                Value::Int(250),
                Value::Int(5),
            ])],
        );
        let mut options = DbOptions::defaults();
        options.precision = 4;
        let interval = TimeInterval::new(
            Utc.with_ymd_and_hms(2025, 1, 1, 10, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap(),
        );
        let output = fetch_strikes(&mock, &options, &interval, None, chrono_tz::UTC)
            .await
            .unwrap();
        assert_eq!(output.lines().count(), 1);
        // The strike's coordinates are rounded to `precision` decimals and the
        // altitude/amplitude/error/count suffix matches `Strike.__str__`.
        assert!(
            output.starts_with("2025-01-01 11:00:00.000000000 10.1235 20.6543 100.0 10.5 250 5")
        );
    }

    #[tokio::test]
    async fn fetch_strikes_grid_renders_arcgrid() {
        let mut mock = MockExecutor::new();
        mock.add_rows(
            "GROUP BY",
            vec![Row::new(vec![
                Value::Int(0),
                Value::Int(1),
                Value::Int(3),
                Value::Timestamp(Utc.with_ymd_and_hms(2025, 1, 1, 11, 0, 0).unwrap()),
            ])],
        );
        let options = DbOptions::defaults();
        let grid = Grid::new(0.0, 1.0, 0.0, 1.0, 1.0, 1.0);
        let interval = TimeInterval::new(
            Utc.with_ymd_and_hms(2025, 1, 1, 10, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap(),
        );
        let output = fetch_strikes_grid(&mock, &options, &grid, &interval)
            .await
            .unwrap();
        assert!(output.starts_with("NCOLS 1\nNROWS 1\nXLLCORNER 0.0000\nYLLCORNER 0.0000\nCELLSIZE 1.0000\nNODATA_VALUE 0\n3"));
    }

    #[test]
    fn strike_timestamp_converts_to_timezone() {
        let strike = crate::data::Strike::new(
            Some(1),
            crate::data::Timestamp::new(Utc.with_ymd_and_hms(2025, 1, 1, 11, 0, 0).unwrap(), 0),
            10.0,
            20.0,
            Some(0.0),
            Some(0.0),
            Some(0),
            Some(0),
            vec![],
            None,
        );
        let tz = parse_timezone("Europe/Berlin").unwrap();
        let rendered = strike.to_string_in_tz(tz);
        // 11:00 UTC == 12:00 CET in January.
        assert!(rendered.starts_with("2025-01-01 12:00:00.000000000 10.0000 20.0000"));
    }

    #[test]
    fn resolve_area_without_area_is_none() {
        let options = DbOptions::defaults();
        assert!(resolve_area(&options).is_none());
    }

    #[test]
    fn resolve_area_useenv_produces_envelope() {
        let mut options = DbOptions::defaults();
        options.area = Some("POLYGON((0 0, 2 0, 1 1, 0 2, 0 0))".to_string());
        options.useenv = true;
        let area = resolve_area(&options).unwrap();
        assert!(area.geometry_wkb.is_none());
    }
}
