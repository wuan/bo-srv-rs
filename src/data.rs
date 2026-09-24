//! Data model ported from `blitzortung/data.py`: [`Timestamp`] (with
//! nanosecond precision) and [`Strike`], plus [`GridData`] for the CLI grid
//! output formats (`to_arcgrid` / `to_map`).

use chrono::{DateTime, Datelike, Duration, NaiveDateTime, TimeZone, Utc};

use crate::geom::Grid;
use crate::round::{py_format_fixed, py_round};

/// Nanosecond-precision timestamp (port of `blitzortung.data.Timestamp`).
///
/// A `Timestamp` combines a [`DateTime<Utc>`] (microsecond resolution) with a
/// separate 0..=999 nanosecond remainder.  The Python class supports `None`
/// datetimes (`NaT`); here an invalid timestamp is represented by
/// `datetime: None`, with [`Timestamp::value`] returning `-1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Timestamp {
    /// Wall-clock value; `None` corresponds to Python's `NaT`.
    pub datetime: Option<DateTime<Utc>>,
    /// Nanosecond remainder, normalized to `0..=999`.
    pub nanosecond: i64,
}

impl Default for Timestamp {
    /// `Timestamp()` without arguments uses the current UTC time.
    fn default() -> Self {
        Timestamp {
            datetime: Some(Utc::now()),
            nanosecond: 0,
        }
    }
}

impl Timestamp {
    /// Length above which a string carries a fractional-seconds part to be
    /// parsed as nanoseconds (Python
    /// `timestamp_string_minimal_fractional_seconds_length`).
    pub const MINIMAL_FRACTIONAL_SECONDS_LENGTH: usize = 20;
    /// Number of characters that are parsed as `%Y-%m-%d %H:%M:%S.%f` (Python
    /// `timestamp_string_microseconds_length`).
    pub const MICROSECONDS_LENGTH: usize = 26;

    /// An invalid timestamp (`data.NaT`).
    pub const NAT: Timestamp = Timestamp {
        datetime: None,
        nanosecond: 0,
    };

    /// Construct from a datetime with a nanosecond remainder, normalizing the
    /// remainder into `0..=999` like `Timestamp.__init__`.
    pub fn new(datetime: DateTime<Utc>, nanosecond: i64) -> Self {
        let mut dt = datetime;
        let mut ns = nanosecond;
        if !(0..=999).contains(&ns) {
            let microdelta = ns.div_euclid(1000);
            dt += Duration::microseconds(microdelta);
            ns -= microdelta * 1000;
        }
        Timestamp {
            datetime: Some(dt),
            nanosecond: ns,
        }
    }

    /// `Timestamp.from_nanoseconds`: split into datetime (microsecond
    /// resolution) and residual nanoseconds.
    pub fn from_nanoseconds(total_nanoseconds: i64) -> (DateTime<Utc>, i64) {
        let total_microseconds = total_nanoseconds.div_euclid(1000);
        let residual_nanoseconds = total_nanoseconds.rem_euclid(1000);
        let total_seconds = total_microseconds.div_euclid(1_000_000);
        let residual_microseconds = total_microseconds.rem_euclid(1_000_000);
        let datetime = Utc
            .timestamp_opt(total_seconds, (residual_microseconds * 1000) as u32)
            .single()
            .expect("timestamp out of range");
        (datetime, residual_nanoseconds)
    }

    /// Construct from nanoseconds since the Unix epoch
    /// (`Timestamp(int)`).  Returns `None` when out of range, mirroring the
    /// Python `ValueError` raised on `OverflowError`.
    pub fn from_nanosecond_value(total_nanoseconds: i64) -> Option<Self> {
        let (datetime, nanosecond) = Timestamp::from_nanoseconds(total_nanoseconds);
        Some(Timestamp {
            datetime: Some(datetime),
            nanosecond,
        })
    }

    /// `Timestamp.from_timestamp(timestamp_string)`: parse
    /// `%Y-%m-%d %H:%M:%S[.fraction]` as UTC, where the fraction beyond
    /// microseconds is scaled to nanoseconds.
    pub fn from_timestamp(timestamp_string: &str) -> Option<Self> {
        if timestamp_string.len() > Timestamp::MINIMAL_FRACTIONAL_SECONDS_LENGTH {
            if timestamp_string.len() < Timestamp::MICROSECONDS_LENGTH {
                return None;
            }
            // The first 26 characters are `%Y-%m-%d %H:%M:%S.%f` where
            // `.f` holds exactly six microsecond digits (Python `%f`); chrono's
            // `%f` would treat the digits as nanoseconds instead, so parse the
            // date and the microsecond part separately.
            let (head, nanosecond_string) =
                timestamp_string.split_at(Timestamp::MICROSECONDS_LENGTH);
            let (date_part, microseconds_part) = head.split_at(19);
            let microsecond_str = microseconds_part.strip_prefix('.')?;
            if microsecond_str.len() != 6 || !microsecond_str.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let microseconds: u32 = microsecond_str.parse().ok()?;
            let naive_date = NaiveDateTime::parse_from_str(date_part, "%Y-%m-%d %H:%M:%S").ok()?;
            let datetime = naive_date + Duration::microseconds(microseconds as i64);
            let nanosecond = if nanosecond_string.is_empty() {
                0
            } else {
                // Python: int(float(s) * 10 ** (3 - len(s)))
                let value: f64 = nanosecond_string.parse().ok()?;
                (value * 10f64.powi(3 - nanosecond_string.len() as i32)).trunc() as i64
            };
            Some(Timestamp {
                datetime: Some(datetime.and_utc()),
                nanosecond,
            })
        } else {
            let datetime =
                NaiveDateTime::parse_from_str(timestamp_string, "%Y-%m-%d %H:%M:%S").ok()?;
            Some(Timestamp {
                datetime: Some(datetime.and_utc()),
                nanosecond: 0,
            })
        }
    }

    /// `Timestamp.value`: nanoseconds since the Unix epoch, or `-1` for an
    /// invalid timestamp.
    pub fn value(&self) -> i64 {
        match self.datetime {
            None => -1,
            Some(dt) => {
                let micros = dt.timestamp() * 1_000_000 + dt.timestamp_subsec_micros() as i64;
                micros * 1000 + self.nanosecond
            }
        }
    }

    /// `Timestamp.is_valid`: datetime present with year > 1900.
    pub fn is_valid(&self) -> bool {
        matches!(self.datetime, Some(dt) if dt.year() > 1900)
    }

    /// `Timestamp.strftime` with an explicit format; returns an empty string
    /// for an invalid timestamp (the callers guard on validity).
    pub fn strftime(&self, datetime_format: &str) -> String {
        match self.datetime {
            Some(dt) => dt.format(datetime_format).to_string(),
            None => String::new(),
        }
    }

    /// `Timestamp.replace`: replace fields of the wrapped datetime (and
    /// optionally the nanosecond remainder).
    pub fn replace(&self, nanosecond: Option<i64>, datetime: Option<DateTime<Utc>>) -> Self {
        let base = datetime.or(self.datetime).or(None);
        match base {
            Some(dt) => Timestamp::new(dt, nanosecond.unwrap_or(self.nanosecond)),
            None => Timestamp {
                datetime: None,
                nanosecond: nanosecond.unwrap_or(self.nanosecond),
            },
        }
    }

    /// The timestamp formatted like `data.Event.__str__`:
    /// `%Y-%m-%d %H:%M:%S.%f` with the nanosecond remainder appended (or
    /// `NaT` when invalid).
    pub fn event_string(&self) -> String {
        match self.datetime {
            Some(dt) => format!(
                "{}.{:06}{:03}",
                dt.format("%Y-%m-%d %H:%M:%S"),
                dt.timestamp_subsec_micros(),
                self.nanosecond
            ),
            None => "NaT".to_string(),
        }
    }

    /// Like [`Timestamp::event_string`] but rendered in `tz`
    /// (`db.mapper.Strike.convert_to_timezone`).
    pub fn event_string_in(&self, tz: chrono_tz::Tz) -> String {
        match self.datetime {
            Some(dt) => {
                let local = dt.with_timezone(&tz);
                format!(
                    "{}.{:06}{:03}",
                    local.format("%Y-%m-%d %H:%M:%S"),
                    local.timestamp_subsec_micros(),
                    self.nanosecond
                )
            }
            None => "NaT".to_string(),
        }
    }
}

impl PartialOrd for Timestamp {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self.datetime, other.datetime) {
            (Some(a), Some(b)) => Some(a.cmp(&b).then(self.nanosecond.cmp(&other.nanosecond))),
            _ => None,
        }
    }
}

impl PartialEq<DateTime<Utc>> for Timestamp {
    fn eq(&self, other: &DateTime<Utc>) -> bool {
        self.datetime == Some(*other)
    }
}

impl PartialOrd<DateTime<Utc>> for Timestamp {
    fn partial_cmp(&self, other: &DateTime<Utc>) -> Option<std::cmp::Ordering> {
        self.datetime.map(|dt| dt.cmp(other))
    }
}

/// A lightning strike (port of `blitzortung.data.Strike`, itself extending
/// `data.Event`).
#[derive(Debug, Clone, PartialEq)]
pub struct Strike {
    /// Database id, `-1` when unset (Python default) — kept as `Option` with
    /// `None` meaning "no id".
    pub id: Option<i64>,
    pub timestamp: Timestamp,
    /// Longitude (`Event.x`).
    pub x: f64,
    /// Latitude (`Event.y`).
    pub y: f64,
    pub altitude: Option<f64>,
    pub amplitude: Option<f64>,
    pub lateral_error: Option<i64>,
    pub station_count: Option<i64>,
    pub stations: Vec<i64>,
    pub region: Option<i64>,
}

impl Strike {
    /// Construct a strike; mirrors `Strike.__init__` with default id `-1`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: Option<i64>,
        timestamp: Timestamp,
        x: f64,
        y: f64,
        altitude: Option<f64>,
        amplitude: Option<f64>,
        lateral_error: Option<i64>,
        station_count: Option<i64>,
        stations: Vec<i64>,
        region: Option<i64>,
    ) -> Self {
        Strike {
            id,
            timestamp,
            x,
            y,
            altitude,
            amplitude,
            lateral_error,
            station_count,
            stations,
            region,
        }
    }

    /// `Strike.has_participant`.
    pub fn has_participant(&self, participant: i64) -> bool {
        self.stations.contains(&participant)
    }

    /// `Event.is_valid` including the location and timestamp checks.
    pub fn is_valid(&self) -> bool {
        let x_ok = self.x.abs() < 1e-9 && self.y.abs() < 1e-9;
        (!x_ok)
            && (-180.0..=180.0).contains(&self.x)
            && (-90.0 < self.y && self.y < 90.0)
            && self.timestamp.is_valid()
    }

    /// `data.Event.difference_to` in nanoseconds (other - self).
    pub fn ns_difference_to(&self, other: &Strike) -> i64 {
        other.timestamp.value() - self.timestamp.value()
    }
}

impl std::fmt::Display for Strike {
    /// `data.Strike.__str__`: the event string plus
    /// `altitude amplitude lateral_error station_count`.
    ///
    /// `altitude` prints as Python `str(altitude)` (`'None'` when absent),
    /// amplitude with one decimal (`0.0` when falsy), and error/station count
    /// as integers (`0` when falsy).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let altitude = match self.altitude {
            Some(value) => python_float_str(value),
            None => "-".to_string(),
        };
        let amplitude = match self.amplitude {
            Some(value) if value != 0.0 => value,
            _ => 0.0,
        };
        let lateral_error = match self.lateral_error {
            Some(value) if value != 0 => value,
            _ => 0,
        };
        let station_count = match self.station_count {
            Some(value) if value != 0 => value,
            _ => 0,
        };
        write!(
            f,
            "{} {:.4} {:.4} {} {:.1} {} {}",
            self.timestamp.event_string(),
            self.x,
            self.y,
            altitude,
            amplitude,
            lateral_error,
            station_count
        )
    }
}

/// `str(altitude)` as CPython renders a float (`2500.0`, `500.5`, `-0.003`).
/// Rust's `{:?}` matches Python's shortest round-trip float repr.
fn python_float_str(value: f64) -> String {
    format!("{value:?}")
}

impl Strike {
    /// Format the strike like [`std::fmt::Display`] but with the timestamp
    /// rendered in `tz` (used by the `bo-db` CLI, whose `--tz` changes the
    /// printed timestamps via `db.mapper.Strike.convert_to_timezone`).
    pub fn to_string_in_tz(&self, tz: chrono_tz::Tz) -> String {
        let altitude = match self.altitude {
            Some(value) => python_float_str(value),
            None => "-".to_string(),
        };
        let amplitude = match self.amplitude {
            Some(value) if value != 0.0 => value,
            _ => 0.0,
        };
        let lateral_error = match self.lateral_error {
            Some(value) if value != 0 => value,
            _ => 0,
        };
        let station_count = match self.station_count {
            Some(value) if value != 0 => value,
            _ => 0,
        };
        format!(
            "{} {:.4} {:.4} {} {:.1} {} {}",
            self.timestamp.event_string_in(tz),
            self.x,
            self.y,
            altitude,
            amplitude,
            lateral_error,
            station_count
        )
    }
}

/// One grid cell (port of `blitzortung.geom.GridElement`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridElement {
    pub count: i64,
    pub timestamp: Option<Timestamp>,
}

impl GridElement {
    pub fn new(count: i64, timestamp: Option<Timestamp>) -> Self {
        GridElement { count, timestamp }
    }
}

/// Grid data used by the `bo-db` CLI for text output
/// (port of `blitzortung.data.GridData`).
///
/// Rows are indexed bottom-up like the Python implementation (row 0 is the
/// southernmost row); `to_arcgrid`/`to_map` iterate `data[::-1]`.
#[derive(Debug, Clone)]
pub struct GridData {
    pub grid: Grid,
    pub no_data: i64,
    /// `data[y][x]`.
    pub data: Vec<Vec<Option<GridElement>>>,
}

impl GridData {
    /// `GridData.__init__`: allocate `x_bin_count` columns by `y_bin_count`
    /// rows of empty cells; `no_data` defaults to `GridElement(0, None)`.
    pub fn new(grid: Grid) -> Self {
        let x_count = grid.x_bin_count().max(0) as usize;
        let y_count = grid.y_bin_count().max(0) as usize;
        GridData {
            grid,
            no_data: 0,
            data: vec![vec![None; x_count]; y_count],
        }
    }

    /// `GridData.set`; out-of-range indices are ignored.
    pub fn set(&mut self, x_index: i64, y_index: i64, value: Option<GridElement>) {
        if x_index < 0 || y_index < 0 {
            return;
        }
        if let Some(row) = self.data.get_mut(y_index as usize) {
            if let Some(cell) = row.get_mut(x_index as usize) {
                *cell = value;
            }
        }
    }

    /// `GridData.get`.
    pub fn get(&self, x_index: i64, y_index: i64) -> Option<GridElement> {
        if x_index < 0 || y_index < 0 {
            return None;
        }
        self.data
            .get(y_index as usize)
            .and_then(|row| row.get(x_index as usize))
            .copied()
            .flatten()
    }

    /// `GridData.to_arcgrid`.
    pub fn to_arcgrid(&self) -> String {
        let mut result = format!("NCOLS {}\n", self.grid.x_bin_count());
        result.push_str(&format!("NROWS {}\n", self.grid.y_bin_count()));
        result.push_str(&format!(
            "XLLCORNER {}\n",
            py_format_fixed(self.grid.x_min, 4)
        ));
        result.push_str(&format!(
            "YLLCORNER {}\n",
            py_format_fixed(self.grid.y_min, 4)
        ));
        result.push_str(&format!(
            "CELLSIZE {}\n",
            py_format_fixed(self.grid.x_div, 4)
        ));
        result.push_str(&format!("NODATA_VALUE {}\n", self.no_data));

        let rows: Vec<String> = self
            .data
            .iter()
            .rev()
            .map(|row| {
                row.iter()
                    .map(|cell| GridData::cell_to_multiplicity(*cell))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        result.push_str(&rows.join("\n"));
        result
    }

    /// `GridData.cell_to_multiplicity`.
    pub fn cell_to_multiplicity(current_cell: Option<GridElement>) -> String {
        match current_cell {
            Some(cell) => cell.count.to_string(),
            None => "0".to_string(),
        }
    }

    /// `GridData.to_map`.
    pub fn to_map(&self) -> String {
        let chars: Vec<char> = " .-o*O8".chars().collect();
        let x_count = self.grid.x_bin_count();
        let (maximum, total) = GridData::max_and_total_entries(&self.data);

        let divider = if maximum > chars.len() as i64 {
            maximum as f64 / (chars.len() - 1) as f64
        } else {
            1.0
        };

        let mut result = format!("{}\n", "-".repeat((x_count + 2).max(0) as usize));
        for row in self.data.iter().rev() {
            result.push('|');
            for cell in row {
                // Python would raise `IndexError` here for out-of-range
                // indices; clamp instead so the tool never panics.
                let index = GridData::cell_index(*cell, divider).min(chars.len() - 1);
                result.push(chars[index]);
            }
            result.push_str("|\n");
        }

        result.push_str(&format!("{}\n", "-".repeat((x_count + 2).max(0) as usize)));
        result.push_str(&format!(
            "total count: {}, max per area: {}",
            total, maximum
        ));
        result
    }

    /// `GridData.cell_index`.
    pub fn cell_index(cell: Option<GridElement>, divider: f64) -> usize {
        match cell {
            Some(cell) => ((cell.count as f64 - 1.0) / divider + 1.0).floor().max(0.0) as usize,
            None => 0,
        }
    }

    /// `GridData.max_and_total_entries`.
    pub fn max_and_total_entries(matrix: &[Vec<Option<GridElement>>]) -> (i64, i64) {
        let mut maximum = 0;
        let mut total = 0;
        for row in matrix {
            for cell in row.iter().flatten() {
                total += cell.count;
                if maximum < cell.count {
                    maximum = cell.count;
                }
            }
        }
        (maximum, total)
    }

    /// Round a coordinate to `precision` decimals like `bo-db
    /// fetch_strikes`: `round(x * 10^precision) / 10^precision`.
    pub fn round_precision(value: f64, precision: i32) -> f64 {
        let factor = 10f64.powi(precision);
        py_round(value * factor, 0) / factor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_from_nanoseconds() {
        let ts = Timestamp::from_nanosecond_value(1540935833552753700).unwrap();
        let dt = ts.datetime.unwrap();
        assert_eq!(dt.timestamp(), 1_540_935_833);
        assert_eq!(dt.timestamp_subsec_micros(), 552_753);
        assert_eq!(ts.nanosecond, 700);
    }

    #[test]
    fn timestamp_value_round_trips() {
        let ts = Timestamp::from_nanosecond_value(1_763_202_124_325_980_200).unwrap();
        assert_eq!(ts.value(), 1_763_202_124_325_980_200);
    }

    #[test]
    fn timestamp_from_nanoseconds_negative() {
        // Python divmod semantics: -1500 ns -> datetime at -2 us, remainder 500
        let (dt, ns) = Timestamp::from_nanoseconds(-1500);
        assert_eq!(ns, 500);
        assert_eq!(dt.timestamp_subsec_micros(), 999_998);
        assert_eq!(dt.timestamp(), -1);
    }

    #[test]
    fn timestamp_from_timestamp_string() {
        let ts = Timestamp::from_timestamp("2020-01-01 12:00:00.123456").unwrap();
        assert_eq!(ts.nanosecond, 0);
        assert_eq!(ts.datetime.unwrap().timestamp_subsec_micros(), 123_456);
    }

    #[test]
    fn timestamp_from_timestamp_with_extra_fraction() {
        // 26 chars are parsed as microseconds, the rest scaled to nanoseconds.
        let ts = Timestamp::from_timestamp("2020-01-01 12:00:00.1234567").unwrap();
        assert_eq!(ts.nanosecond, 700);
        assert_eq!(ts.datetime.unwrap().timestamp_subsec_micros(), 123_456);
    }

    #[test]
    fn timestamp_from_timestamp_rejects_unsupported_format() {
        // The `T`/`+00:00` layout is not what `Timestamp.from_timestamp`
        // accepts; Python returns `(None, 0)` and so do we.
        assert!(Timestamp::from_timestamp("2025-01-15T12:30:45.123456+00:00").is_none());
    }

    #[test]
    fn timestamp_from_timestamp_mirrors_length_heuristic() {
        // 19 chars => no fractional seconds branch
        let ts = Timestamp::from_timestamp("2013-08-20 12:09:00").unwrap();
        let dt = ts.datetime.unwrap();
        assert_eq!(dt.timestamp(), 1_377_000_540);
        assert_eq!(ts.nanosecond, 0);
    }

    #[test]
    fn timestamp_nat_value_and_validity() {
        assert_eq!(Timestamp::NAT.value(), -1);
        assert!(!Timestamp::NAT.is_valid());
    }

    #[test]
    fn timestamp_normalizes_nanoseconds() {
        let base = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let ts = Timestamp::new(base, 1500);
        assert_eq!(ts.nanosecond, 500);
        assert_eq!(ts.datetime.unwrap().timestamp_subsec_micros(), 1);
    }

    #[test]
    fn strike_display_matches_python() {
        use chrono::Timelike;
        let base = Utc
            .with_ymd_and_hms(2013, 9, 28, 23, 23, 38)
            .unwrap()
            .with_nanosecond(123_456_000)
            .unwrap();
        // Reference Python:
        // "2013-09-28 23:23:38.123456789 11.2000 49.3000 2500 10.5 5400 11"
        let ts = Timestamp::new(base, 789);
        let strike = Strike::new(
            Some(1),
            ts,
            11.2,
            49.3,
            Some(2500.0),
            Some(10.5),
            Some(5400),
            Some(11),
            vec![],
            None,
        );
        assert_eq!(
            strike.to_string(),
            "2013-09-28 23:23:38.123456789 11.2000 49.3000 2500.0 10.5 5400 11"
        );
    }

    #[test]
    fn strike_display_defaults_for_missing_fields() {
        let ts = Timestamp::new(Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap(), 0);
        let strike = Strike::new(None, ts, 1.0, 2.0, None, None, None, None, vec![], None);
        assert_eq!(
            strike.to_string(),
            "2020-01-01 00:00:00.000000000 1.0000 2.0000 - 0.0 0 0"
        );
    }

    fn example_grid() -> Grid {
        Grid::new(-5.0, 4.0, -3.0, 2.0, 0.5, 1.25)
    }

    #[test]
    fn grid_data_empty_arcgrid() {
        let data = GridData::new(example_grid());
        assert_eq!(data.grid.x_bin_count(), 18);
        assert_eq!(data.grid.y_bin_count(), 4);
        assert_eq!(
            data.to_arcgrid(),
            "NCOLS 18\nNROWS 4\nXLLCORNER -5.0000\nYLLCORNER -3.0000\nCELLSIZE 0.5000\nNODATA_VALUE 0\n\
             0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
             0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
             0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
             0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0"
        );
    }

    #[test]
    fn grid_data_empty_map() {
        let data = GridData::new(example_grid());
        assert_eq!(
            data.to_map(),
            "--------------------\n|                  |\n|                  |\n|                  |\n|                  |\n--------------------\ntotal count: 0, max per area: 0"
        );
    }

    #[test]
    fn grid_data_raster_arcgrid() {
        let mut data = GridData::new(example_grid());
        data.set(0, 0, Some(GridElement::new(5, None)));
        data.set(1, 1, Some(GridElement::new(10, None)));
        data.set(4, 2, Some(GridElement::new(20, None)));
        assert_eq!(
            data.to_arcgrid(),
            "NCOLS 18\nNROWS 4\nXLLCORNER -5.0000\nYLLCORNER -3.0000\nCELLSIZE 0.5000\nNODATA_VALUE 0\n\
             0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
             0 0 0 0 20 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
             0 10 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
             5 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0"
        );
    }

    #[test]
    fn grid_data_raster_map() {
        let mut data = GridData::new(example_grid());
        data.set(0, 0, Some(GridElement::new(5, None)));
        data.set(1, 1, Some(GridElement::new(10, None)));
        data.set(4, 2, Some(GridElement::new(20, None)));
        assert_eq!(
            data.to_map(),
            "--------------------\n|                  |\n|    8             |\n| o                |\n|-                 |\n--------------------\ntotal count: 35, max per area: 20"
        );
    }

    #[test]
    fn grid_data_cell_index() {
        assert_eq!(
            GridData::cell_index(Some(GridElement::new(5, None)), 5.0),
            1
        );
        assert_eq!(
            GridData::cell_index(Some(GridElement::new(6, None)), 5.0),
            2
        );
        assert_eq!(
            GridData::cell_index(Some(GridElement::new(10, None)), 5.0),
            2
        );
        assert_eq!(
            GridData::cell_index(Some(GridElement::new(11, None)), 5.0),
            3
        );
        assert_eq!(GridData::cell_index(None, 5.0), 0);
    }

    #[test]
    fn grid_data_max_and_total() {
        let matrix = vec![
            vec![
                Some(GridElement::new(4, None)),
                Some(GridElement::new(8, None)),
            ],
            vec![None, Some(GridElement::new(6, None))],
        ];
        assert_eq!(GridData::max_and_total_entries(&matrix), (8, 18));
    }
}
