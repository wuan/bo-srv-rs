//! Strike builders ported from `blitzortung/builder/strike.py` and
//! `blitzortung/util.py` (`force_range`).
//!
//! Two input formats are supported, matching the Python builder:
//!
//! * [`Strike::from_line`] parses the protected-log text format
//!   `"<ts 29 chars> pos;lat;lon;alt str;amp dev;error sta;count;mcg;stations"`.
//! * [`Strike::from_json`] parses the JSON feed (websocket / `last_strikes.php`)
//!   with `lon`/`lat`/`time`(ns)/`alt`/`mds`/`region`.

use crate::data::{Strike as DataStrike, Timestamp};
use crate::round::py_round;

/// Wrap a [`BuilderError`] for parse failures.
#[derive(Debug)]
pub struct BuilderError(pub String);

impl std::fmt::Display for BuilderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for BuilderError {}

/// `blitzortung.util.force_range`.
pub fn force_range(lower_limit: i64, value: i64, upper_limit: i64) -> i64 {
    if value < lower_limit {
        lower_limit
    } else if value > upper_limit {
        upper_limit
    } else {
        value
    }
}

/// Builder for [`DataStrike`] (port of `blitzortung.builder.strike.Strike`).
#[derive(Debug, Clone, Default)]
pub struct Strike {
    id_value: i64,
    timestamp: Option<Timestamp>,
    x: Option<f64>,
    y: Option<f64>,
    altitude: Option<f64>,
    amplitude: Option<f64>,
    lateral_error: Option<i64>,
    station_count: Option<i64>,
    stations: Vec<i64>,
    region: Option<i64>,
}

impl Strike {
    /// `Strike.__init__`: default id `-1`, all optional fields unset.
    pub fn new() -> Self {
        Strike {
            id_value: -1,
            timestamp: None,
            x: None,
            y: None,
            altitude: None,
            amplitude: None,
            lateral_error: None,
            station_count: None,
            stations: Vec::new(),
            region: None,
        }
    }

    pub fn set_id(&mut self, id_value: i64) -> &mut Self {
        self.id_value = id_value;
        self
    }

    pub fn set_timestamp(&mut self, timestamp: Timestamp) -> &mut Self {
        self.timestamp = Some(timestamp);
        self
    }

    pub fn set_x(&mut self, x: f64) -> &mut Self {
        self.x = Some(x);
        self
    }

    pub fn set_y(&mut self, y: f64) -> &mut Self {
        self.y = Some(y);
        self
    }

    pub fn set_altitude(&mut self, altitude: f64) -> &mut Self {
        self.altitude = Some(altitude);
        self
    }

    pub fn set_amplitude(&mut self, amplitude: f64) -> &mut Self {
        self.amplitude = Some(amplitude);
        self
    }

    /// `set_lateral_error` clamps into `0..=32767`.
    pub fn set_lateral_error(&mut self, lateral_error: f64) -> &mut Self {
        self.lateral_error = Some(force_range(0, lateral_error as i64, 32767));
        self
    }

    pub fn set_station_count(&mut self, station_count: i64) -> &mut Self {
        self.station_count = Some(station_count);
        self
    }

    pub fn set_stations(&mut self, stations: Vec<i64>) -> &mut Self {
        self.stations = stations;
        self
    }

    pub fn set_region(&mut self, region: Option<i64>) -> &mut Self {
        self.region = region;
        self
    }

    /// `Strike.from_line`: parse the protected-log text format.
    ///
    /// The Python parser uses three regexes plus a `sta;` regex; the field
    /// semantics are:
    /// * `pos;lat;lon;alt` — note `set_x(lon)` / `set_y(lat)`,
    /// * `str;amplitude`,
    /// * `dev;lateral_error`,
    /// * `sta;station_count;mcg;station,ids`.
    pub fn from_line(&mut self, line: &str) -> Result<&mut Self, BuilderError> {
        self.try_from_line(line)
            .map_err(|message| BuilderError(message))
    }

    fn try_from_line(&mut self, line: &str) -> Result<&mut Self, String> {
        // Timestamp is always the first 29 characters (`line[0:29]`).
        // Python tolerates an unparsable timestamp here (the resulting
        // `Timestamp` has a `None` datetime); an empty/invalid timestamp is
        // filtered later by `strike.timestamp.is_valid`.
        let timestamp = line
            .get(0..29)
            .and_then(Timestamp::from_timestamp)
            .unwrap_or(Timestamp::NAT);
        self.set_timestamp(timestamp);

        let position = find_position(line).ok_or_else(|| "missing position".to_string())?;
        // Python `position` tuple is (lat, lon, alt); x=lon, y=lat.
        self.set_x(parse_f64(&position.1)?);
        self.set_y(parse_f64(&position.0)?);
        self.set_altitude(parse_f64(&position.2)?);

        let amplitude = find_after(line, "str;").ok_or_else(|| "missing amplitude".to_string())?;
        self.set_amplitude(parse_f64(&amplitude)?);

        let deviation = find_after(line, "dev;").ok_or_else(|| "missing deviation".to_string())?;
        self.set_lateral_error(parse_f64(&deviation)?);

        let stations = find_stations(line).ok_or_else(|| "missing stations".to_string())?;
        self.set_station_count(stations.0);
        let station_list: Vec<i64> = stations
            .2
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<i64>())
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        self.set_stations(station_list);

        Ok(self)
    }

    /// `Strike.from_json`: parse a JSON-feed strike.
    ///
    /// Fields are read leniently like the Python builder: `lon`/`lat` are
    /// rounded to 4 decimals, `time` is nanoseconds since the epoch, and
    /// `alt`/`mds`/`region` default as in `from_json`.
    pub fn from_json(&mut self, json: &serde_json::Value) -> Result<&mut Self, BuilderError> {
        self.try_from_json(json)
            .map_err(BuilderError)
    }

    fn try_from_json(&mut self, json: &serde_json::Value) -> Result<&mut Self, String> {
        let lon = json
            .get("lon")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| "missing lon".to_string())?;
        let lat = json
            .get("lat")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| "missing lat".to_string())?;
        let time = json
            .get("time")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| "missing time".to_string())?;

        self.set_x(py_round(lon, 4));
        self.set_y(py_round(lat, 4));
        let timestamp =
            Timestamp::from_nanosecond_value(time).ok_or_else(|| "invalid timestamp".to_string())?;
        self.set_timestamp(timestamp);

        let altitude = json.get("alt").and_then(|v| v.as_f64()).unwrap_or(0.0);
        self.set_altitude(altitude);

        self.set_amplitude(0.0);

        let mds = json.get("mds").and_then(|v| v.as_f64()).unwrap_or(0.0);
        self.set_lateral_error(mds);

        self.set_station_count(0);

        let region = json.get("region").and_then(|v| v.as_i64());
        self.set_region(region);

        Ok(self)
    }

    /// `Strike.build`: requires a timestamp and coordinates.
    pub fn build(&self) -> Result<DataStrike, BuilderError> {
        let timestamp = self
            .timestamp
            .ok_or_else(|| BuilderError("Timestamp not set".to_string()))?;
        Ok(DataStrike::new(
            Some(self.id_value),
            timestamp,
            self.x.unwrap_or(0.0),
            self.y.unwrap_or(0.0),
            self.altitude,
            self.amplitude,
            self.lateral_error,
            self.station_count,
            self.stations.clone(),
            self.region,
        ))
    }
}

fn parse_f64(value: &str) -> Result<f64, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("empty float".to_string());
    }
    trimmed.parse::<f64>().map_err(|e| e.to_string())
}

/// Find the first occurrence of `marker` followed by a floating-point number.
fn find_after(line: &str, marker: &str) -> Option<String> {
    let start = line.find(marker)? + marker.len();
    let rest = &line[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// Find the `pos;lat;lon;alt` triple; returns `(lat, lon, alt)` like the
/// Python `position_parser.findall(...)[0]`.
fn find_position(line: &str) -> Option<(String, String, String)> {
    let start = line.find("pos;")? + 4;
    let rest = &line[start..];
    let mut parts = rest.split(';');
    let lat = parts.next()?.to_string();
    let lon = parts.next()?.to_string();
    let alt = {
        let value = parts.next()?;
        let end = value
            .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
            .unwrap_or(value.len());
        value[..end].to_string()
    };
    if lat.is_empty() || lon.is_empty() || alt.is_empty() {
        return None;
    }
    Some((lat, lon, alt))
}

/// Find the `sta;count;mcg;stations` quadruple; returns `(count, mcg,
/// stations)` like the Python `stations_parser.findall(...)[0]`.
fn find_stations(line: &str) -> Option<(i64, i64, String)> {
    let start = line.find("sta;")? + 4;
    let rest = &line[start..];
    let mut parts = rest.splitn(4, ';');
    let count = parts.next()?.trim().parse::<i64>().ok()?;
    let mcg = parts.next()?.trim().parse::<i64>().ok()?;
    // The stations list extends to the next space (Python regex `([^ ]*)`).
    let stations_raw = parts.next()?;
    let end = stations_raw.find(' ').unwrap_or(stations_raw.len());
    Some((count, mcg, stations_raw[..end].to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn from_line_valid_data() {
        let line = "2025-01-15T12:30:45.123456+00:00 pos;48.5;-10.2;500.5 str;45.2 dev;250.0 sta;5;10;1,2,3,4,5";
        let strike = Strike::new().from_line(line).unwrap().build().unwrap();
        assert_eq!(strike.x, -10.2);
        assert_eq!(strike.y, 48.5);
        assert_eq!(strike.altitude, Some(500.5));
        assert_eq!(strike.amplitude, Some(45.2));
        assert_eq!(strike.lateral_error, Some(250));
        assert_eq!(strike.station_count, Some(5));
        assert_eq!(strike.stations, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn from_line_negative_coordinates() {
        let line = "2025-01-15T12:30:45.123456+00:00 pos;-48.5;-10.2;500.5 str;45.2 dev;250.0 sta;5;10;1,2,3,4,5";
        let strike = Strike::new().from_line(line).unwrap().build().unwrap();
        assert_eq!(strike.x, -10.2);
        assert_eq!(strike.y, -48.5);
    }

    #[test]
    fn from_line_missing_stations_filtered() {
        let line = "2025-01-15T12:30:45.123456+00:00 pos;48.5;-10.2;500.5 str;45.2 dev;250.0 sta;3;10;1,,3";
        let strike = Strike::new().from_line(line).unwrap().build().unwrap();
        assert_eq!(strike.stations, vec![1, 3]);
    }

    #[test]
    fn from_line_no_stations() {
        let line = "2025-01-15T12:30:45.123456+00:00 pos;48.5;-10.2;500.5 str;45.2 dev;250.0 sta;0;10;";
        let strike = Strike::new().from_line(line).unwrap().build().unwrap();
        assert_eq!(strike.stations, Vec::<i64>::new());
    }

    #[test]
    fn from_line_invalid_format_errors() {
        assert!(Strike::new().from_line("invalid line format").is_err());
    }

    #[test]
    fn from_line_missing_position_errors() {
        let line = "2025-01-15T12:30:45.123456+00:00 str;45.2 dev;250.0 sta;5;10;1,2,3";
        assert!(Strike::new().from_line(line).is_err());
    }

    #[test]
    fn from_line_invalid_float_errors() {
        let line = "2025-01-15T12:30:45.123456+00:00 pos;invalid;-10.2;500.5 str;45.2 dev;250.0 sta;5;10;1,2,3";
        assert!(Strike::new().from_line(line).is_err());
    }

    #[test]
    fn lateral_error_clamped() {
        let line = "2025-01-15 12:30:45.123456 pos;48.5;-10.2;500.5 str;45.2 dev;-100.0 sta;5;10;1,2,3";
        let strike = Strike::new().from_line(line).unwrap().build().unwrap();
        assert_eq!(strike.lateral_error, Some(0));
        let line = "2025-01-15 12:30:45.123456 pos;48.5;-10.2;500.5 str;45.2 dev;100000.0 sta;5;10;1,2,3";
        let strike = Strike::new().from_line(line).unwrap().build().unwrap();
        assert_eq!(strike.lateral_error, Some(32767));
    }

    #[test]
    fn from_json_valid_data() {
        let data = json!({"time": 1763202124297904000i64, "lat": 44.283328, "lon": 8.910987,
                          "alt": 0, "pol": 0, "mds": 6830, "mcg": 84, "status": 2, "region": 9});
        let strike = Strike::new().from_json(&data).unwrap().build().unwrap();
        assert_eq!(strike.x, 8.911);
        assert_eq!(strike.y, 44.2833);
        assert_eq!(strike.altitude, Some(0.0));
        assert_eq!(strike.amplitude, Some(0.0));
        assert_eq!(strike.lateral_error, Some(6830));
        assert_eq!(strike.station_count, Some(0));
        assert_eq!(strike.stations, Vec::<i64>::new());
        assert_eq!(strike.region, Some(9));
    }

    #[test]
    fn from_json_missing_field_errors() {
        let data = json!({"lat": 44.283328, "lon": 8.910987});
        assert!(Strike::new().from_json(&data).is_err());
    }

    #[test]
    fn from_json_missing_region_is_none() {
        let data = json!({"time": 0i64, "lat": 1.0, "lon": 2.0});
        let strike = Strike::new().from_json(&data).unwrap().build().unwrap();
        assert_eq!(strike.region, None);
    }

    #[test]
    fn build_without_timestamp_errors() {
        let err = Strike::new().build().unwrap_err();
        assert!(err.to_string().contains("Timestamp not set"));
    }

    #[test]
    fn force_range_bounds() {
        assert_eq!(force_range(10, 15, 20), 15);
        assert_eq!(force_range(10, 9, 20), 10);
        assert_eq!(force_range(10, 21, 20), 20);
    }
}