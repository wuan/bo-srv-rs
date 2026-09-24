//! SQL query construction, ported from `blitzortung/db/query.py` and
//! `blitzortung/db/query_builder.py`.
//!
//! The generated SQL text uses psycopg2-style `%(name)s` placeholders so that
//! it is byte-for-byte identical with the Python implementation; parameters
//! are tracked by name like the Python `Query.parameters` dict.  Use
//! [`Query::to_postgres`] / [`Query::parameters`] to obtain the `$1, $2, ...`
//! form with parameters in the same order for tokio-postgres.

use chrono::{DateTime, Utc};

use crate::executor::Param;
use crate::geom::Grid;

/// Time interval (equivalent of `db.query.TimeInterval`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeInterval {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl TimeInterval {
    pub fn new(start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        TimeInterval { start, end }
    }

    /// `db.query.TimeInterval.duration`; the Python service layer uses
    /// `int(duration.total_seconds())` (no modulo-86400 wrapping).
    pub fn duration_seconds(&self) -> i64 {
        (self.end - self.start).num_seconds()
    }

    /// `db.query.TimeInterval.minutes`: `int(total_seconds // 60)`.
    pub fn minutes(&self) -> i64 {
        (self.end - self.start).num_seconds() / 60
    }
}

/// Id interval used by `strikes` when a positive `id_or_offset` is passed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IdInterval {
    pub start: i64,
}

/// Query builder mirroring `db.query.Query` / `SelectQuery` string rendering.
#[derive(Debug, Clone, Default)]
pub struct Query {
    table_name: String,
    columns: Vec<String>,
    conditions: Vec<String>,
    groups: Vec<String>,
    groups_having: Vec<String>,
    order: Vec<String>,
    limit: Option<i64>,
    /// Named parameters (name -> value), insertion order.
    params: Vec<(String, Param)>,
}

fn find_param_name(sql: &str, from: usize) -> Option<(usize, String)> {
    let bytes = sql.as_bytes();
    if from + 1 < bytes.len() && bytes[from] == b'%' && bytes[from + 1] == b'(' {
        let rest = &sql[from + 2..];
        if let Some(close_rel) = rest.find(')') {
            let close = from + 2 + close_rel;
            if close + 1 < bytes.len() && bytes[close + 1] == b's' {
                return Some((close + 2, sql[from + 2..close].to_string()));
            }
        }
    }
    None
}

/// Collect `%(name)s` tokens in order of appearance in the SQL text.
fn param_names_in_order(sql: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut i = 0usize;
    while i < sql.len() {
        if let Some((next, name)) = find_param_name(sql, i) {
            names.push(name);
            i = next;
        } else {
            i += sql[i..].chars().next().unwrap().len_utf8();
        }
    }
    names
}

/// Replace `%(name)s` tokens with `$1..$N` in order of *first appearance* of
/// each distinct name (named parameters may be repeated in the SQL, e.g.
/// `%(srid)s` in both the X and Y transforms; positional parameters cannot).
fn named_to_positional(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut i = 0usize;
    while i < sql.len() {
        if let Some((next, name)) = find_param_name(sql, i) {
            let next_index = index.len() + 1;
            let idx = *index.entry(name.clone()).or_insert(next_index);
            out.push('$');
            out.push_str(&idx.to_string());
            i = next;
        } else {
            let ch = sql[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

impl Query {
    pub fn new(table_name: &str) -> Self {
        Query {
            table_name: table_name.to_string(),
            ..Default::default()
        }
    }

    pub fn column(mut self, column: &str) -> Self {
        self.columns.push(column.to_string());
        self
    }

    pub fn set_columns(mut self, columns: &[&str]) -> Self {
        self.columns = columns.iter().map(|c| c.to_string()).collect();
        self
    }

    pub fn condition(mut self, condition: &str) -> Self {
        self.conditions.push(condition.to_string());
        self
    }

    pub fn param(mut self, name: &str, param: Param) -> Self {
        self.params.push((name.to_string(), param));
        self
    }

    pub fn group_by(mut self, group: &str) -> Self {
        self.groups.push(group.to_string());
        self
    }

    pub fn group_having(mut self, condition: &str, name: &str, param: Param) -> Self {
        self.groups_having.push(condition.to_string());
        self.params.push((name.to_string(), param));
        self
    }

    pub fn order_by(mut self, column: &str, desc: bool) -> Self {
        let mut s = column.to_string();
        if desc {
            s.push_str(" DESC");
        }
        self.order.push(s);
        self
    }

    pub fn limit(mut self, limit: i64) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Parameters in `$n` order: one value per distinct parameter name, in
    /// order of first appearance in the SQL text (matching the numbering of
    /// [`Query::to_postgres`]).
    pub fn parameters(&self) -> Vec<Param> {
        let sql = self.to_sql();
        let map: std::collections::HashMap<String, Param> =
            self.params.iter().cloned().collect();
        let mut seen = std::collections::HashSet::new();
        param_names_in_order(&sql)
            .into_iter()
            .filter(|name| seen.insert(name.clone()))
            .filter_map(|name| map.get(&name).cloned())
            .collect()
    }

    /// SQL text in psycopg2 `%(name)s` form (identical to Python).
    pub fn to_sql(&self) -> String {
        let mut sql = String::new();
        sql.push_str("SELECT ");
        if self.columns.is_empty() {
            sql.push(' ');
        } else {
            sql.push_str(&self.columns.join(", "));
            sql.push(' ');
        }
        sql.push_str("FROM ");
        sql.push_str(&self.table_name);
        sql.push(' ');

        if !self.conditions.is_empty() {
            sql.push_str("WHERE ");
            sql.push_str(&self.conditions.join(" AND "));
            sql.push(' ');
        }
        if !self.groups.is_empty() {
            sql.push_str("GROUP BY ");
            sql.push_str(&self.groups.join(", "));
            sql.push(' ');
            if !self.groups_having.is_empty() {
                sql.push_str("HAVING ");
                sql.push_str(&self.groups_having.join(" AND "));
                sql.push(' ');
            }
        }
        if !self.order.is_empty() {
            sql.push_str("ORDER BY ");
            sql.push_str(&self.order.join(", "));
            sql.push(' ');
        }
        if let Some(limit) = self.limit {
            sql.push_str("LIMIT ");
            sql.push_str(&limit.to_string());
            sql.push(' ');
        }
        sql.trim_end().to_string()
    }

    /// SQL text with `$1..$N` placeholders for tokio-postgres.
    pub fn to_postgres(&self) -> String {
        named_to_positional(&self.to_sql())
    }
}

fn add_time_interval(q: Query, time_interval: &TimeInterval) -> Query {
    let mut qq = q;
    qq = qq
        .condition("\"timestamp\" >= %(start_time)s")
        .param("start_time", Param::Timestamp(time_interval.start));
    qq = qq
        .condition("\"timestamp\" < %(end_time)s")
        .param("end_time", Param::Timestamp(time_interval.end));
    qq
}

/// `blitzortung.db.query_builder.Strike.select_query` as used by the
/// `strikes` method: select columns, time interval, optional id interval,
/// `ORDER BY id`.
pub fn strikes_query(time_interval: &TimeInterval, id_interval: Option<IdInterval>) -> Query {
    let mut q = Query::new("strikes")
        .set_columns(&[
            "id",
            "\"timestamp\"",
            "nanoseconds",
            "ST_X(ST_Transform(geog::geometry, %(srid)s)) AS x",
            "ST_Y(ST_Transform(geog::geometry, %(srid)s)) AS y",
            "altitude",
            "amplitude",
            "error2d",
            "stationcount",
        ])
        .param("srid", Param::Int(4326));
    q = add_time_interval(q, time_interval);
    if let Some(id) = id_interval {
        q = q
            .condition("id >= %(start_id)s")
            .param("start_id", Param::Int(id.start));
    }
    q.order_by("id", false)
}

/// `blitzortung.db.query_builder.Strike.grid_query`: envelope-filtered raster
/// grid with `x_min`/`y_min` offsets, plus a `region` condition when a region
/// is given (regions with overlapping bounding boxes would otherwise count
/// strikes twice).
pub fn grid_query(grid: &Grid, time_interval: &TimeInterval, region: Option<i64>, count_threshold: i64) -> Query {
    let env = grid.envelope().as_wkb_linear_ring();
    let mut q = Query::new("strikes");
    q = q
        .set_columns(&[
            "TRUNC((ST_X(ST_Transform(geog::geometry, %(srid)s)) - %(xmin)s) / %(xdiv)s)::integer AS rx",
            "TRUNC((ST_Y(ST_Transform(geog::geometry, %(srid)s)) - %(ymin)s) / %(ydiv)s)::integer AS ry",
            "count(*) AS strike_count",
            "max(\"timestamp\") as \"timestamp\"",
        ])
        .param("srid", Param::Int(4326))
        .param("xmin", Param::Float(grid.x_min))
        .param("xdiv", Param::Float(grid.x_div))
        .param("ymin", Param::Float(grid.y_min))
        .param("ydiv", Param::Float(grid.y_div))
        .condition("ST_GeomFromWKB(%(envelope)s, %(envelope_srid)s) && geog")
        .param("envelope", Param::Bytea(env))
        .param("envelope_srid", Param::Int(4326));
    q = add_time_interval(q, time_interval);
    if let Some(region) = region {
        q = q
            .condition("region = %(region)s")
            .param("region", Param::Int(region));
    }
    q = q.group_by("rx").group_by("ry");
    if count_threshold > 0 {
        q = q.group_having("count(*) > %(count_threshold)s", "count_threshold", Param::Int(count_threshold));
    }
    q
}

/// `blitzortung.db.query_builder.Strike.global_grid_query` (world-wide grid
/// without envelope filtering; cell indices are computed with half-cell
/// rounding, matching the Python `ROUND(...)` expressions).
pub fn global_grid_query(grid: &Grid, time_interval: &TimeInterval, count_threshold: i64) -> Query {
    let mut q = Query::new("strikes");
    q = q
        .set_columns(&[
            "ROUND((ST_X(ST_Transform(geog::geometry, %(srid)s)) - %(xdiv)s * 0.5) / %(xdiv)s)::integer AS rx",
            "ROUND((ST_Y(ST_Transform(geog::geometry, %(srid)s)) - %(ydiv)s * 0.5) / %(ydiv)s)::integer AS ry",
            "count(*) AS strike_count",
            "max(\"timestamp\") as \"timestamp\"",
        ])
        .param("srid", Param::Int(4326))
        .param("xdiv", Param::Float(grid.x_div))
        .param("ydiv", Param::Float(grid.y_div));
    q = add_time_interval(q, time_interval);
    q = q.group_by("rx").group_by("ry");
    if count_threshold > 0 {
        q = q.group_having("count(*) > %(count_threshold)s", "count_threshold", Param::Int(count_threshold));
    }
    q
}

/// `blitzortung.db.query_builder.Strike.histogram_query`.
///
/// The interval expression uses the parameterized `%(end_time)s` timestamp
/// (the Python service passes the computed `TimeInterval`), and bin sizes are
/// passed as the `%(binsize)s` parameter.
pub fn histogram_query(
    time_interval: &TimeInterval,
    binsize: i64,
    region: Option<i64>,
    envelope: Option<&Grid>,
) -> Query {
    let mut q = Query::new("strikes")
        .set_columns(&[
            "-extract( epoch from %(end_time)s - \"timestamp\")::int/60/%(binsize)s as interval",
            "count(*)",
        ])
        .param("end_time", Param::Timestamp(time_interval.end))
        .param("binsize", Param::Int(binsize));
    q = add_time_interval(q, time_interval);
    q = q.group_by("interval").order_by("interval", false);

    if let Some(region) = region {
        q = q
            .condition("region = %(region)s")
            .param("region", Param::Int(region));
    }

    if let Some(grid) = envelope {
        let env = grid.envelope().as_wkb_linear_ring();
        q = q
            .condition("ST_SetSRID(CAST(%(envelope)s AS geometry), %(envelope_srid)s) && geog")
            .param("envelope", Param::Bytea(env))
            .param("envelope_srid", Param::Int(4326));
    }

    q
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    #[test]
    fn named_to_positional_substitution() {
        let sql = "WHERE x >= %(a)s AND y < %(b)s";
        assert_eq!(named_to_positional(sql), "WHERE x >= $1 AND y < $2");
        assert_eq!(param_names_in_order(sql), vec!["a", "b"]);
    }

    #[test]
    fn strikes_sql_matches_python() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let q = strikes_query(&interval, Some(IdInterval { start: 100 }));
        let expected = "SELECT id, \"timestamp\", nanoseconds, \
             ST_X(ST_Transform(geog::geometry, %(srid)s)) AS x, \
             ST_Y(ST_Transform(geog::geometry, %(srid)s)) AS y, \
             altitude, amplitude, error2d, stationcount \
             FROM strikes WHERE \"timestamp\" >= %(start_time)s AND \"timestamp\" < %(end_time)s \
             AND id >= %(start_id)s ORDER BY id";
        assert_eq!(q.to_sql(), expected);
        assert_eq!(q.parameters().len(), 4); // srid, start_time, end_time, start_id
        assert_eq!(q.to_postgres(), named_to_positional(expected));
    }

    #[test]
    fn grid_query_sql_matches_python() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let grid = Grid::new(
            -25.0,
            56.8605750930044,
            27.0,
            71.94746107673467,
            0.14017221762500753,
            0.08865376938211966,
        );
        let q = grid_query(&grid, &interval, Some(1), 0);
        let expected = "SELECT \
             TRUNC((ST_X(ST_Transform(geog::geometry, %(srid)s)) - %(xmin)s) / %(xdiv)s)::integer AS rx, \
             TRUNC((ST_Y(ST_Transform(geog::geometry, %(srid)s)) - %(ymin)s) / %(ydiv)s)::integer AS ry, \
             count(*) AS strike_count, max(\"timestamp\") as \"timestamp\" \
             FROM strikes WHERE \
             ST_GeomFromWKB(%(envelope)s, %(envelope_srid)s) && geog AND \
             \"timestamp\" >= %(start_time)s AND \"timestamp\" < %(end_time)s \
             AND region = %(region)s \
             GROUP BY rx, ry";
        assert_eq!(q.to_sql(), expected);
        // Param order follows SQL token order
        let params = q.parameters();
        assert_eq!(params.len(), 10);
        assert!(matches!(params[0], Param::Int(4326))); // srid
        assert!(matches!(params[1], Param::Float(_))); // xmin
        assert!(matches!(params[5], Param::Bytea(_))); // envelope
        assert!(matches!(params[6], Param::Int(4326))); // envelope_srid
        assert!(matches!(params[7], Param::Timestamp(_))); // start_time
        assert!(matches!(params[8], Param::Timestamp(_))); // end_time
        assert!(matches!(params[9], Param::Int(1))); // region
    }

    #[test]
    fn grid_query_without_region_has_no_region_condition() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let grid = Grid::new(
            -25.0,
            56.8605750930044,
            27.0,
            71.94746107673467,
            0.14017221762500753,
            0.08865376938211966,
        );
        let q = grid_query(&grid, &interval, None, 0);
        assert!(!q.to_sql().contains("region"));
        assert_eq!(q.parameters().len(), 9);
    }

    #[test]
    fn global_grid_query_sql_matches_python() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let grid = Grid::new(
            -180.0,
            179.79137864480873,
            -90.0,
            89.79069969676917,
            0.3183994501281493,
            0.2356365657886883,
        );
        let q = global_grid_query(&grid, &interval, 0);
        let expected = "SELECT \
             ROUND((ST_X(ST_Transform(geog::geometry, %(srid)s)) - %(xdiv)s * 0.5) / %(xdiv)s)::integer AS rx, \
             ROUND((ST_Y(ST_Transform(geog::geometry, %(srid)s)) - %(ydiv)s * 0.5) / %(ydiv)s)::integer AS ry, \
             count(*) AS strike_count, max(\"timestamp\") as \"timestamp\" \
             FROM strikes WHERE \
             \"timestamp\" >= %(start_time)s AND \"timestamp\" < %(end_time)s \
             GROUP BY rx, ry";
        assert_eq!(q.to_sql(), expected);
        // srid, xdiv, ydiv, start_time, end_time = 5 parameters
        let params = q.parameters();
        assert_eq!(params.len(), 5);
        assert!(matches!(params[0], Param::Int(4326)));
        assert!(matches!(params[1], Param::Float(_)));
    }

    #[test]
    fn histogram_sql_matches_python() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let q = histogram_query(&interval, 5, None, None);
        let expected = "SELECT -extract( epoch from %(end_time)s - \"timestamp\")::int/60/%(binsize)s as interval, count(*) \
             FROM strikes WHERE \"timestamp\" >= %(start_time)s AND \"timestamp\" < %(end_time)s \
             GROUP BY interval ORDER BY interval";
        assert_eq!(q.to_sql(), expected);
        // parameter order follows SQL token order: end_time, binsize,
        // start_time (end_time already seen)
        let params = q.parameters();
        assert_eq!(params.len(), 3);
        assert!(matches!(params[0], Param::Timestamp(_)));
        assert!(matches!(params[1], Param::Int(5)));
        assert!(matches!(params[2], Param::Timestamp(_)));
        assert_eq!(q.to_postgres(), "SELECT -extract( epoch from $1 - \"timestamp\")::int/60/$2 as interval, count(*) FROM strikes WHERE \"timestamp\" >= $3 AND \"timestamp\" < $1 GROUP BY interval ORDER BY interval");
    }

    #[test]
    fn histogram_sql_with_region_and_envelope() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let grid = Grid::new(
            -25.0,
            56.8605750930044,
            27.0,
            71.94746107673467,
            0.14017221762500753,
            0.08865376938211966,
        );
        let q = histogram_query(&interval, 5, Some(1), Some(&grid));
        let sql = q.to_sql();
        assert!(sql.contains("AND region = %(region)s"));
        assert!(sql.contains("AND ST_SetSRID(CAST(%(envelope)s AS geometry), %(envelope_srid)s) && geog"));
        let params = q.parameters();
        assert_eq!(params.len(), 6); // end_time, binsize, start_time, region, envelope, envelope_srid
        assert!(matches!(params[3], Param::Int(1)));
        assert!(matches!(params[4], Param::Bytea(_)));
        assert!(matches!(params[5], Param::Int(4326)));
    }

    #[test]
    fn repeated_named_params_map_to_single_positional() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let grid = Grid::new(
            -25.0,
            56.8605750930044,
            27.0,
            71.94746107673467,
            0.14017221762500753,
            0.08865376938211966,
        );
        let q = grid_query(&grid, &interval, Some(1), 0);
        let sql = q.to_postgres();
        // srid appears twice in the SQL but only once as a positional param
        assert!(sql.contains("ST_X(ST_Transform(geog::geometry, $1)"));
        assert!(sql.contains("ST_Y(ST_Transform(geog::geometry, $1)"));
        // region is the tenth distinct parameter
        assert_eq!(sql.matches("$10").count(), 1);
        assert_eq!(q.parameters().len(), 10);
    }

    #[test]
    fn duration_seconds_is_total_seconds() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        assert_eq!(interval.duration_seconds(), 300);
        // 24h+ deltas do NOT wrap (total_seconds semantics)
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 2, 0, 0, 0));
        assert_eq!(interval.duration_seconds(), 86400);
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 23, 59, 0));
        assert_eq!(interval.duration_seconds(), 86340);
    }
}