//! Strike table access ported from `blitzortung/db/table.py`
//! (`db.Strike`) plus the `bo-db` select/grid queries.
//!
//! Everything goes through the [`QueryExecutor`] trait so the tools can be
//! tested with [`crate::mock::MockExecutor`] without a database.

use crate::data::{GridData, GridElement, Strike, Timestamp};
use crate::executor::{Param, QueryExecutor, Row};
use crate::geom::Grid;
use crate::query::{self, Area, TimeInterval};
use crate::round::py_round;

/// A de-duplication key: `(timestamp ns value, round(x,4), round(y,4),
/// lateral_error)` (`db.Strike._create_strike_key` / `update.create_strike_key`).
pub type StrikeKey = (i64, f64, f64, Option<i64>);

/// A hashable/sortable form of [`StrikeKey`] (float coordinates compared by
/// their bit pattern; the values are rounded to four decimals beforehand, so
/// bit equality matches value equality).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HashableStrikeKey {
    pub timestamp: i64,
    pub x: u64,
    pub y: u64,
    pub lateral_error: Option<i64>,
}

impl From<StrikeKey> for HashableStrikeKey {
    fn from(key: StrikeKey) -> Self {
        HashableStrikeKey {
            timestamp: key.0,
            x: key.1.to_bits(),
            y: key.2.to_bits(),
            lateral_error: key.3,
        }
    }
}

impl HashableStrikeKey {
    /// Build directly from a [`Strike`].
    pub fn from_strike(strike: &Strike) -> Self {
        HashableStrikeKey {
            timestamp: strike.timestamp.value(),
            x: crate::round::py_round(strike.x, 4).to_bits(),
            y: crate::round::py_round(strike.y, 4).to_bits(),
            lateral_error: strike.lateral_error,
        }
    }
}

/// Errors returned by the strike table helpers.
#[derive(Debug)]
pub enum DbError {
    /// Underlying executor/database error.
    Executor(Box<dyn std::error::Error + Send + Sync>),
    /// A required column was missing or had an unexpected type.
    Column(String),
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbError::Executor(e) => write!(f, "{e}"),
            DbError::Column(name) => write!(f, "invalid or missing column: {name}"),
        }
    }
}

impl std::error::Error for DbError {}

impl From<Box<dyn std::error::Error + Send + Sync>> for DbError {
    fn from(error: Box<dyn std::error::Error + Send + Sync>) -> Self {
        DbError::Executor(error)
    }
}

/// Table name used by the Python `db.Strike` (`Strike.table_name`).
pub const TABLE_NAME: &str = "strikes";

/// Strike table access object (port of `blitzortung.db.table.Strike`).
pub struct StrikeDb<'a> {
    executor: &'a dyn QueryExecutor,
    srid: i64,
}

impl<'a> StrikeDb<'a> {
    /// Create a strike table access object for the given SRID (default 4326).
    pub fn new(executor: &'a dyn QueryExecutor, srid: i64) -> Self {
        StrikeDb { executor, srid }
    }

    /// `Strike.insert_many`: insert all strikes in a single multi-value
    /// `INSERT`, using `ST_MakePoint(lon, lat)` for the `geog` column.
    ///
    /// `region` pins every strike to the same region (the `bo-import` case);
    /// when it is `None` each strike's own region is used, falling back to 1
    /// (the `bo-update` case).
    pub fn insert_many(&self, strikes: &[Strike], region: Option<i64>) -> Result<usize, DbError> {
        if strikes.is_empty() {
            return Ok(0);
        }
        let mut sql = String::from(
            "INSERT INTO strikes \
             (\"timestamp\", nanoseconds, geog, altitude, region, amplitude, error2d, stationcount) VALUES ",
        );
        let mut params: Vec<Param> = Vec::with_capacity(strikes.len() * 9);
        let mut placeholders: Vec<String> = Vec::with_capacity(strikes.len());

        for (index, strike) in strikes.iter().enumerate() {
            let base = index * 9;
            placeholders.push(format!(
                "(${}, ${}, ST_MakePoint(${}, ${}), ${}, ${}, ${}, ${}, ${})",
                base + 1,
                base + 2,
                base + 3,
                base + 4,
                base + 5,
                base + 6,
                base + 7,
                base + 8,
                base + 9
            ));
            let timestamp = strike
                .timestamp
                .datetime
                .ok_or_else(|| DbError::Column("timestamp".to_string()))?;
            let effective_region = region.or(strike.region).unwrap_or(1);
            params.push(Param::Timestamp(timestamp));
            params.push(Param::Int(strike.timestamp.nanosecond));
            params.push(Param::Float(strike.x));
            params.push(Param::Float(strike.y));
            params.push(match strike.altitude {
                Some(v) => Param::Float(v),
                None => Param::Null,
            });
            params.push(Param::Int(effective_region));
            params.push(match strike.amplitude {
                Some(v) => Param::Float(v),
                None => Param::Null,
            });
            params.push(match strike.lateral_error {
                Some(v) => Param::Int(v),
                None => Param::Null,
            });
            params.push(match strike.station_count {
                Some(v) => Param::Int(v),
                None => Param::Null,
            });
        }

        sql.push_str(&placeholders.join(", "));
        self.executor.execute(&sql, &params)?;
        Ok(strikes.len())
    }

    /// `Strike.insert`: insert a single strike (used by the websocket importer).
    pub fn insert(&self, strike: &Strike, region: i64) -> Result<(), DbError> {
        self.insert_many(std::slice::from_ref(strike), Some(region))?;
        Ok(())
    }

    /// `Strike.get_latest_time`: the newest `(timestamp, nanoseconds)` for a
    /// region, or `None` when the table has no matching row.
    pub fn get_latest_time(&self, region: Option<i64>) -> Result<Option<Timestamp>, DbError> {
        let sql = match region {
            Some(_) => {
                "SELECT \"timestamp\", nanoseconds FROM strikes WHERE region=$1 \
                 ORDER BY \"timestamp\" DESC, nanoseconds DESC LIMIT 1"
            }
            None => {
                "SELECT \"timestamp\", nanoseconds FROM strikes \
                 ORDER BY \"timestamp\" DESC, nanoseconds DESC LIMIT 1"
            }
        };
        let params = match region {
            Some(region) => vec![Param::Int(region)],
            None => Vec::new(),
        };
        let rows = self.executor.query(sql, &params)?;
        match rows.first() {
            None => Ok(None),
            Some(row) => {
                let datetime = row
                    .get_timestamp(0)
                    .ok_or_else(|| DbError::Column("timestamp".to_string()))?;
                let nanosecond = row
                    .get_i64(1)
                    .ok_or_else(|| DbError::Column("nanoseconds".to_string()))?;
                Ok(Some(Timestamp::new(datetime, nanosecond)))
            }
        }
    }

    /// `Strike.select`: build and run the select query, mapping rows into
    /// [`Strike`] objects (`db.mapper.Strike.create_object`).
    pub fn select(
        &self,
        time_interval: &TimeInterval,
        area: Option<&Area>,
        region: Option<i64>,
    ) -> Result<Vec<Strike>, DbError> {
        let query = query::select_query(time_interval, area, region, self.srid);
        let rows = self
            .executor
            .query(&query.to_postgres(), &query.parameters())?;
        rows.iter().map(|row| self.map_strike(row)).collect()
    }

    /// `Strike.select_strike_keys`: the de-duplication keys for the interval.
    pub fn select_strike_keys(
        &self,
        time_interval: &TimeInterval,
        area: Option<&Area>,
        region: Option<i64>,
    ) -> Result<Vec<StrikeKey>, DbError> {
        let query = query::select_key_query(time_interval, area, region, self.srid);
        let rows = self
            .executor
            .query(&query.to_postgres(), &query.parameters())?;
        rows.iter()
            .map(|row| {
                let datetime = row
                    .get_timestamp(0)
                    .ok_or_else(|| DbError::Column("timestamp".to_string()))?;
                let nanosecond = row
                    .get_i64(1)
                    .ok_or_else(|| DbError::Column("nanoseconds".to_string()))?;
                let timestamp = Timestamp::new(datetime, nanosecond);
                let x = row
                    .get_f64(2)
                    .ok_or_else(|| DbError::Column("x".to_string()))?;
                let y = row
                    .get_f64(3)
                    .ok_or_else(|| DbError::Column("y".to_string()))?;
                let lateral_error = row.get_i64(4);
                Ok((
                    timestamp.value(),
                    py_round(x, 4),
                    py_round(y, 4),
                    lateral_error,
                ))
            })
            .collect()
    }

    /// `Strike.select_grid`: run the grid query and build a [`GridData`]
    /// (`db.grid_result.build_grid_result`).
    pub fn select_grid(
        &self,
        grid: &Grid,
        count_threshold: i64,
        time_interval: &TimeInterval,
        region: Option<i64>,
    ) -> Result<GridData, DbError> {
        let query = query::grid_query(grid, time_interval, region, count_threshold);
        let rows = self
            .executor
            .query(&query.to_postgres(), &query.parameters())?;

        let x_bin_count = grid.x_bin_count();
        let y_bin_count = grid.y_bin_count();
        let mut grid_data = GridData::new(*grid);
        for row in &rows {
            let rx = row
                .get_i64(0)
                .ok_or_else(|| DbError::Column("rx".to_string()))?;
            let ry = row
                .get_i64(1)
                .ok_or_else(|| DbError::Column("ry".to_string()))?;
            let strike_count = row
                .get_i64(2)
                .ok_or_else(|| DbError::Column("strike_count".to_string()))?;
            let timestamp = row.get_timestamp(3);
            if (0..x_bin_count).contains(&rx) && ry > 0 && ry <= y_bin_count {
                let y_index = y_bin_count - ry;
                grid_data.set(
                    rx,
                    y_index,
                    Some(GridElement::new(
                        strike_count,
                        timestamp.map(|dt| Timestamp::new(dt, 0)),
                    )),
                );
            }
        }
        Ok(grid_data)
    }

    fn map_strike(&self, row: &Row) -> Result<Strike, DbError> {
        let id = row.get_i64(0);
        let datetime = row
            .get_timestamp(1)
            .ok_or_else(|| DbError::Column("timestamp".to_string()))?;
        let nanosecond = row
            .get_i64(2)
            .ok_or_else(|| DbError::Column("nanoseconds".to_string()))?;
        let x = row
            .get_f64(3)
            .ok_or_else(|| DbError::Column("x".to_string()))?;
        let y = row
            .get_f64(4)
            .ok_or_else(|| DbError::Column("y".to_string()))?;
        let altitude = row.get_f64(5);
        let amplitude = row.get_f64(6);
        let lateral_error = row.get_i64(7).map(|v| v.clamp(0, 32767));
        let station_count = row.get_i64(8);
        Ok(Strike::new(
            id,
            Timestamp::new(datetime, nanosecond),
            x,
            y,
            altitude,
            amplitude,
            lateral_error,
            station_count,
            Vec::new(),
            None,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::Value;
    use crate::mock::MockExecutor;
    use chrono::{TimeZone, Utc};

    fn ts(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    fn strike(x: f64, y: f64) -> Strike {
        Strike::new(
            Some(-1),
            Timestamp::new(ts(2025, 1, 1, 12, 0, 0), 123),
            x,
            y,
            Some(100.0),
            Some(50.5),
            Some(250),
            Some(5),
            vec![],
            None,
        )
    }

    #[test]
    fn insert_many_builds_multi_value_insert() {
        let mut mock = MockExecutor::new();
        mock.add_rows("SELECT", vec![]);
        let db = StrikeDb::new(&mock, 4326);
        let count = db
            .insert_many(&[strike(1.0, 2.0), strike(3.0, 4.0)], Some(1))
            .unwrap();
        assert_eq!(count, 2);
        let executions = mock.executions();
        assert_eq!(executions.len(), 1);
        let (sql, params) = &executions[0];
        assert!(sql.starts_with("INSERT INTO strikes"));
        assert!(sql.contains("ST_MakePoint($3, $4)"));
        assert!(sql.contains("ST_MakePoint($12, $13)"));
        assert_eq!(params.len(), 18);
        assert!(matches!(params[0], Param::Timestamp(_)));
        assert_eq!(params[1], Param::Int(123)); // nanoseconds
        assert_eq!(params[5], Param::Int(1)); // region
    }

    #[test]
    fn insert_many_uses_strike_region_when_unset() {
        let mut mock = MockExecutor::new();
        mock.add_rows("SELECT", vec![]);
        let db = StrikeDb::new(&mock, 4326);
        let mut s = strike(1.0, 2.0);
        s.region = Some(9);
        db.insert_many(&[s], None).unwrap();
        let (_, params) = &mock.executions()[0];
        assert_eq!(params[5], Param::Int(9));
    }

    #[test]
    fn insert_many_falls_back_to_region_one() {
        let mut mock = MockExecutor::new();
        mock.add_rows("SELECT", vec![]);
        let db = StrikeDb::new(&mock, 4326);
        db.insert_many(&[strike(1.0, 2.0)], None).unwrap();
        let (_, params) = &mock.executions()[0];
        assert_eq!(params[5], Param::Int(1));
    }

    #[test]
    fn insert_many_empty_is_noop() {
        let mock = MockExecutor::new();
        let db = StrikeDb::new(&mock, 4326);
        assert_eq!(db.insert_many(&[], Some(1)).unwrap(), 0);
        assert_eq!(mock.execution_count(), 0);
    }

    #[test]
    fn get_latest_time_reads_row() {
        let mut mock = MockExecutor::new();
        mock.add_rows(
            "ORDER BY \"timestamp\" DESC",
            vec![Row::new(vec![
                Value::Timestamp(ts(2025, 1, 1, 12, 0, 0)),
                Value::Int(700),
            ])],
        );
        let db = StrikeDb::new(&mock, 4326);
        let latest = db.get_latest_time(Some(1)).unwrap().unwrap();
        assert_eq!(latest.nanosecond, 700);
        assert_eq!(latest.datetime.unwrap(), ts(2025, 1, 1, 12, 0, 0));
        let (sql, params) = &mock.calls()[0];
        assert!(sql.contains("region=$1"));
        assert_eq!(params[0], Param::Int(1));
    }

    #[test]
    fn get_latest_time_no_row_is_none() {
        let mut mock = MockExecutor::new();
        mock.add_rows("ORDER BY", vec![]);
        let db = StrikeDb::new(&mock, 4326);
        assert!(db.get_latest_time(None).unwrap().is_none());
    }

    #[test]
    fn select_maps_rows_to_strikes() {
        let mut mock = MockExecutor::new();
        mock.add_rows(
            "FROM strikes",
            vec![Row::new(vec![
                Value::Int(42),
                Value::Timestamp(ts(2025, 1, 1, 12, 0, 0)),
                Value::Int(700),
                Value::Float(8.910987),
                Value::Float(44.283328),
                Value::Float(0.0),
                Value::Float(12.5),
                Value::Int(6830),
                Value::Int(7),
            ])],
        );
        let db = StrikeDb::new(&mock, 4326);
        let interval = TimeInterval::new(ts(2025, 1, 1, 0, 0, 0), ts(2025, 1, 1, 1, 0, 0));
        let strikes = db.select(&interval, None, None).unwrap();
        assert_eq!(strikes.len(), 1);
        assert_eq!(strikes[0].id, Some(42));
        assert_eq!(strikes[0].x, 8.910987);
        assert_eq!(strikes[0].lateral_error, Some(6830));
        assert_eq!(strikes[0].station_count, Some(7));
        assert_eq!(strikes[0].timestamp.nanosecond, 700);
    }

    #[test]
    fn select_strike_keys_returns_keys() {
        let mut mock = MockExecutor::new();
        mock.add_rows(
            "FROM strikes",
            vec![Row::new(vec![
                Value::Timestamp(ts(2025, 1, 1, 12, 0, 0)),
                Value::Int(123),
                Value::Float(8.910987),
                Value::Float(44.283328),
                Value::Int(6830),
            ])],
        );
        let db = StrikeDb::new(&mock, 4326);
        let interval = TimeInterval::new(ts(2025, 1, 1, 0, 0, 0), ts(2025, 1, 1, 1, 0, 0));
        let keys = db.select_strike_keys(&interval, None, None).unwrap();
        let expected_ts = Timestamp::new(ts(2025, 1, 1, 12, 0, 0), 123).value();
        assert_eq!(keys, vec![(expected_ts, 8.911, 44.2833, Some(6830))]);
    }

    #[test]
    fn select_grid_builds_grid_data() {
        let mut mock = MockExecutor::new();
        // grid with x_bin_count 2, y_bin_count 2
        mock.add_rows(
            "GROUP BY",
            vec![
                Row::new(vec![
                    Value::Int(0),
                    Value::Int(2),
                    Value::Int(5),
                    Value::Timestamp(ts(2025, 1, 1, 12, 0, 0)),
                ]),
                // out of range ry=0 is filtered
                Row::new(vec![
                    Value::Int(0),
                    Value::Int(0),
                    Value::Int(1),
                    Value::Timestamp(ts(2025, 1, 1, 12, 0, 0)),
                ]),
            ],
        );
        let grid = Grid::new(0.0, 2.0, 0.0, 2.0, 1.0, 1.0);
        let db = StrikeDb::new(&mock, 4326);
        let interval = TimeInterval::new(ts(2025, 1, 1, 0, 0, 0), ts(2025, 1, 1, 1, 0, 0));
        let data = db.select_grid(&grid, 0, &interval, None).unwrap();
        // rx=0, ry=2 => y_index = 2 - 2 = 0
        assert_eq!(data.get(0, 0).unwrap().count, 5);
        assert_eq!(data.get(0, 1), None);
    }
}