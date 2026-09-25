//! SQL query construction, ported from `blitzortung/db/query.py` and
//! `blitzortung/db/query_builder.py`.
//!
//! The generated SQL text uses psycopg2-style `%(name)s` placeholders and
//! tracks parameters by name like the Python `Query.parameters` dict.  It
//! matches the Python implementation except that a no-op
//! `ST_Transform(geog::geometry, 4326)` is omitted when coordinates are read in
//! the default SRID: the fixed-4326 queries ([`strikes_query`], [`grid_query`],
//! [`global_grid_query`]) and [`select_query`] with [`DEFAULT_SRID`].  Use
//! [`Query::to_postgres`] / [`Query::parameters`] to obtain the `$1, $2, ...`
//! form with parameters in the same order for tokio-postgres.

use chrono::{DateTime, Utc};

use crate::executor::Param;
use crate::geom::{Envelope, Grid};

/// Region condition used by the strike select/grid queries
/// (`query_builder.REGION_CONDITION`).
pub const REGION_CONDITION: &str = "region = %(region)s";

/// A query area (port of the shapely geometry argument accepted by
/// `db.Strike.select(geometry=...)`).
///
/// The Python implementation always adds a bounding-box pre-filter
/// (`ST_GeomFromWKB(envelope) && geog`) and additionally an `ST_Intersects`
/// test when the geometry is not equal to its own envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct Area {
    /// WKB of the geometry envelope, used for the `&& geog` pre-filter.
    pub envelope_wkb: Vec<u8>,
    /// WKB of the full geometry, used for `ST_Intersects` when the geometry is
    /// not already its envelope.
    pub geometry_wkb: Option<Vec<u8>>,
    /// The bounding-box bounds `(x_min, y_min, x_max, y_max)` corresponding to
    /// `envelope_wkb` (`area.envelope.bounds`).
    pub bounds: (f64, f64, f64, f64),
}

impl Area {
    /// Build an area from an envelope (bounding-box only).
    pub fn from_envelope(envelope: &Envelope) -> Self {
        Area {
            envelope_wkb: envelope.as_wkb_polygon(),
            geometry_wkb: None,
            bounds: (
                envelope.x_min,
                envelope.y_min,
                envelope.x_max,
                envelope.y_max,
            ),
        }
    }

    /// Build an area from a full polygon (ring exterior + holes); the envelope
    /// is derived from the exterior ring.
    pub fn from_polygon(rings: &[Vec<[f64; 2]>]) -> Option<Self> {
        let exterior = rings.first()?;
        if exterior.is_empty() {
            return None;
        }
        let mut x_min = f64::INFINITY;
        let mut x_max = f64::NEG_INFINITY;
        let mut y_min = f64::INFINITY;
        let mut y_max = f64::NEG_INFINITY;
        for p in exterior {
            x_min = x_min.min(p[0]);
            x_max = x_max.max(p[0]);
            y_min = y_min.min(p[1]);
            y_max = y_max.max(p[1]);
        }
        let envelope = Envelope::new(x_min, x_max, y_min, y_max);
        // A ring is its own envelope when (ignoring the closing repeat) it is
        // exactly the four envelope corners.
        let mut corners: Vec<[f64; 2]> = exterior.clone();
        if corners.len() > 1 && corners.first() == corners.last() {
            corners.pop();
        }
        let expected_corners = [
            [x_min, y_min],
            [x_min, y_max],
            [x_max, y_min],
            [x_max, y_max],
        ];
        let is_envelope = rings.len() == 1
            && corners.len() == 4
            && corners.iter().all(|p| expected_corners.contains(p))
            && expected_corners
                .iter()
                .all(|corner| corners.iter().any(|p| p == corner));
        Some(Area {
            envelope_wkb: envelope.as_wkb_polygon(),
            geometry_wkb: if is_envelope {
                None
            } else {
                Some(crate::wkb::polygon(rings))
            },
            bounds: (x_min, y_min, x_max, y_max),
        })
    }
}

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
    /// Optional SQL type cast per parameter name (`name -> sql type`), applied
    /// only in the positional/PostgreSQL rendering ([`Query::to_postgres`]).
    ///
    /// PostgreSQL cannot always infer a placeholder's type from context: e.g.
    /// `ST_Transform(geog::geometry, $1)` is ambiguous between the
    /// `(geometry, integer)` and `(geometry, text)` overloads, so the server
    /// defaults the parameter to `text`.  tokio-postgres then sends the
    /// integer's *binary* form, whose bytes contain `0x00`, and the server
    /// rejects it as an invalid UTF-8 byte sequence.  An explicit
    /// `$1::integer` cast removes the ambiguity.  The psycopg2-form SQL
    /// ([`Query::to_sql`]) is intentionally left unchanged so it stays
    /// byte-for-byte identical to the Python implementation.
    casts: Vec<(String, String)>,
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
#[cfg(test)]
fn named_to_positional(sql: &str) -> String {
    named_to_positional_with_casts(sql, &[])
}

/// Like [`named_to_positional`] but appends an explicit `::type` cast after
/// every placeholder whose parameter name has a registered cast.  Used for the
/// PostgreSQL rendering so the server does not have to infer an ambiguous
/// placeholder type (see [`Query::casts`]).
fn named_to_positional_with_casts(sql: &str, casts: &[(String, String)]) -> String {
    let cast_map: std::collections::HashMap<&str, &str> = casts
        .iter()
        .map(|(name, ty)| (name.as_str(), ty.as_str()))
        .collect();
    let mut out = String::with_capacity(sql.len());
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut i = 0usize;
    while i < sql.len() {
        if let Some((next, name)) = find_param_name(sql, i) {
            let next_index = index.len() + 1;
            let idx = *index.entry(name.clone()).or_insert(next_index);
            out.push('$');
            out.push_str(&idx.to_string());
            if let Some(ty) = cast_map.get(name.as_str()) {
                out.push_str("::");
                out.push_str(ty);
            }
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

    /// Like [`Query::set_columns`] but takes owned strings, for projections
    /// built at runtime (e.g. [`SelectColumns::sql_columns`]).
    pub fn set_columns_owned(mut self, columns: Vec<String>) -> Self {
        self.columns = columns;
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

    /// Register an explicit SQL type cast for a parameter name; applied only in
    /// [`Query::to_postgres`] (see [`Query::casts`]).
    pub fn cast(mut self, name: &str, sql_type: &str) -> Self {
        self.casts.push((name.to_string(), sql_type.to_string()));
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
        let map: std::collections::HashMap<String, Param> = self.params.iter().cloned().collect();
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

    /// SQL text with `$1..$N` placeholders for tokio-postgres, including the
    /// explicit `::type` casts registered via [`Query::cast`].
    pub fn to_postgres(&self) -> String {
        named_to_positional_with_casts(&self.to_sql(), &self.casts)
    }
}

fn add_time_interval(q: Query, time_interval: &TimeInterval) -> Query {
    let mut qq = q;
    qq = qq
        .condition("\"timestamp\" >= %(start_time)s")
        .param("start_time", Param::Timestamp(time_interval.start))
        .cast("start_time", "timestamptz");
    qq = qq
        .condition("\"timestamp\" < %(end_time)s")
        .param("end_time", Param::Timestamp(time_interval.end))
        .cast("end_time", "timestamptz");
    qq
}

/// `query.Query.add_geometry`: bounding-box pre-filter plus an `ST_Intersects`
/// condition when the geometry is not already its own envelope.  When
/// `default_srid` is set the SRID is inlined as a literal and the no-op
/// `ST_Transform` is skipped, so no `srid` parameter is required.
fn add_geometry(q: Query, area: &Area, default_srid: bool) -> Query {
    let (srid, geog) = if default_srid {
        (DEFAULT_SRID.to_string(), "geog".to_string())
    } else {
        let srid = "%(srid)s".to_string();
        (
            srid.clone(),
            format!("ST_Transform(geog::geometry, {srid})"),
        )
    };
    let mut qq = q
        .condition(&format!("ST_GeomFromWKB(%(envelope)s, {srid}) && geog"))
        .param("envelope", Param::Bytea(area.envelope_wkb.clone()));
    if !default_srid {
        qq = qq.cast("srid", "integer");
    }
    if let Some(geometry) = &area.geometry_wkb {
        qq = qq
            .condition(&format!(
                "ST_Intersects(ST_GeomFromWKB(%(geometry)s, {srid}), {geog})"
            ))
            .param("geometry", Param::Bytea(geometry.clone()));
    }
    qq
}

/// Parse a WKT polygon (the only geometry form the CLI tools need) into an
/// [`Area`].  Supports `POLYGON ((x y, ...), (hole...))` and `POLYGON EMPTY`.
///
/// Returns `None` when the text is not a polygon or is malformed, matching the
/// Python CLI's error handling (`shapely.wkt.loads` failure).
pub fn parse_wkt_polygon(wkt: &str) -> Option<Area> {
    let trimmed = wkt.trim();
    let upper = trimmed.to_ascii_uppercase();
    if !upper.starts_with("POLYGON") {
        return None;
    }
    let body = trimmed[7..].trim();
    if body.eq_ignore_ascii_case("EMPTY") {
        return None;
    }
    let body = body.strip_prefix('(')?.strip_suffix(')')?;

    let mut rings: Vec<Vec<[f64; 2]>> = Vec::new();
    // Split on the ring separator `),(`.
    let mut depth = 0usize;
    let mut current = String::new();
    for ch in body.chars() {
        match ch {
            '(' => {
                depth += 1;
                if depth == 1 {
                    current.clear();
                    continue;
                }
                current.push(ch);
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    rings.push(parse_wkt_ring(&current)?);
                    continue;
                }
                current.push(ch);
            }
            _ => current.push(ch),
        }
    }
    Area::from_polygon(&rings)
}

fn parse_wkt_ring(ring: &str) -> Option<Vec<[f64; 2]>> {
    let mut points = Vec::new();
    for pair in ring.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let mut coords = pair.split_whitespace();
        let x: f64 = coords.next()?.parse().ok()?;
        let y: f64 = coords.next()?.parse().ok()?;
        points.push([x, y]);
    }
    if points.is_empty() {
        None
    } else {
        Some(points)
    }
}

/// Add the time interval, optional region and optional geometry conditions to
/// a strike select query.
fn add_select_conditions(
    q: Query,
    time_interval: &TimeInterval,
    area: Option<&Area>,
    region: Option<i64>,
    default_srid: bool,
) -> Query {
    let mut qq = add_time_interval(q, time_interval);
    if let Some(region) = region {
        qq = qq
            .condition(REGION_CONDITION)
            .param("region", Param::Int(region))
            .cast("region", "smallint");
    }
    if let Some(area) = area {
        qq = add_geometry(qq, area, default_srid);
    }
    qq
}

/// Columns fetched by [`select_query`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectColumns {
    /// Full strike row (`id`, `"timestamp"`, `nanoseconds`, `x`, `y`, `altitude`,
    /// `amplitude`, `error2d`, `stationcount`).
    Strike,
    /// The fields needed to identify a strike (`"timestamp"`, `nanoseconds`,
    /// `x`, `y`, `error2d`), used by `bo-update` for de-duplication.
    StrikeKey,
}

impl SelectColumns {
    /// SQL projection for this variant.  When `default_srid` is set the
    /// `ST_Transform(geog::geometry, 4326)` no-op is omitted from the
    /// coordinate expressions, matching [`strikes_query`].
    fn sql_columns(self, default_srid: bool) -> Vec<String> {
        let x = coordinate_column("ST_X", "x", default_srid);
        let y = coordinate_column("ST_Y", "y", default_srid);
        match self {
            SelectColumns::Strike => vec![
                "id".to_string(),
                "\"timestamp\"".to_string(),
                "nanoseconds".to_string(),
                x,
                y,
                "altitude".to_string(),
                "amplitude".to_string(),
                "error2d".to_string(),
                "stationcount".to_string(),
            ],
            SelectColumns::StrikeKey => vec![
                "\"timestamp\"".to_string(),
                "nanoseconds".to_string(),
                x,
                y,
                "error2d".to_string(),
            ],
        }
    }
}

/// The CRS the `geog` column is stored in.  A `ST_Transform` to this SRID is a
/// no-op, so it is omitted.
pub const DEFAULT_SRID: i64 = 4326;

/// A `ST_X`/`ST_Y` projection that skips the no-op `ST_Transform` when the
/// coordinates are already in [`DEFAULT_SRID`].
fn coordinate_column(function: &str, alias: &str, default_srid: bool) -> String {
    if default_srid {
        format!("{function}(geog::geometry) AS {alias}")
    } else {
        format!("{function}(ST_Transform(geog::geometry, %(srid)s)) AS {alias}")
    }
}

/// `ORDER BY` behaviour of [`select_query`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectOrder {
    /// Chronological (`"timestamp", nanoseconds`), the `bo-db` default.  `id` is
    /// deliberately not the sort key: it is a `bigserial` assigned at insert
    /// time, so delayed/backfilled imports make `id` order diverge from time
    /// order.
    Timestamp,
    /// Insertion order (`id`).
    Id,
    /// Leave the result unordered, e.g. for de-duplication where order is
    /// irrelevant.
    None,
}

impl SelectOrder {
    fn apply(self, q: Query) -> Query {
        match self {
            SelectOrder::Timestamp => q
                .order_by("\"timestamp\"", false)
                .order_by("nanoseconds", false),
            SelectOrder::Id => q.order_by("id", false),
            SelectOrder::None => q,
        }
    }
}

/// `blitzortung.db.query_builder.Strike.select_query`: select the requested
/// columns over a time interval with optional region/geometry, ordered by
/// `order`.
pub fn select_query(
    time_interval: &TimeInterval,
    area: Option<&Area>,
    region: Option<i64>,
    srid: i64,
    columns: SelectColumns,
    order: SelectOrder,
) -> Query {
    let default_srid = srid == DEFAULT_SRID;
    let mut q = Query::new("strikes").set_columns_owned(columns.sql_columns(default_srid));
    if !default_srid {
        q = q.param("srid", Param::Int(srid)).cast("srid", "integer");
    }
    let q = add_select_conditions(q, time_interval, area, region, default_srid);
    order.apply(q)
}

/// `blitzortung.db.query_builder.Strike.select_query` as used by the
/// `strikes` method: select columns, time interval, optional id interval,
/// `ORDER BY id`.
pub fn strikes_query(time_interval: &TimeInterval, id_interval: Option<IdInterval>) -> Query {
    let mut q = Query::new("strikes").set_columns_owned(SelectColumns::Strike.sql_columns(true));
    q = add_time_interval(q, time_interval);
    if let Some(id) = id_interval {
        q = q
            .condition("id >= %(start_id)s")
            .param("start_id", Param::Int(id.start))
            .cast("start_id", "bigint");
    }
    q.order_by("id", false)
}

/// `blitzortung.db.query_builder.Strike.grid_query`: envelope-filtered raster
/// grid with `x_min`/`y_min` offsets, plus a `region` condition when a region
/// is given (regions with overlapping bounding boxes would otherwise count
/// strikes twice).
pub fn grid_query(
    grid: &Grid,
    time_interval: &TimeInterval,
    region: Option<i64>,
    count_threshold: i64,
) -> Query {
    let env = grid.envelope().as_wkb_linear_ring();
    let mut q = Query::new("strikes");
    q = q
        .set_columns(&[
            "TRUNC((ST_X(geog::geometry) - %(xmin)s) / %(xdiv)s)::integer AS rx",
            "TRUNC((ST_Y(geog::geometry) - %(ymin)s) / %(ydiv)s)::integer AS ry",
            "count(*) AS strike_count",
            "max(\"timestamp\") as \"timestamp\"",
        ])
        .param("xmin", Param::Float(grid.x_min))
        .cast("xmin", "double precision")
        .param("xdiv", Param::Float(grid.x_div))
        .cast("xdiv", "double precision")
        .param("ymin", Param::Float(grid.y_min))
        .cast("ymin", "double precision")
        .param("ydiv", Param::Float(grid.y_div))
        .cast("ydiv", "double precision")
        .condition("ST_GeomFromWKB(%(envelope)s, %(envelope_srid)s) && geog")
        .param("envelope", Param::Bytea(env))
        .param("envelope_srid", Param::Int(4326))
        .cast("envelope_srid", "integer");
    q = add_time_interval(q, time_interval);
    if let Some(region) = region {
        q = q
            .condition("region = %(region)s")
            .param("region", Param::Int(region))
            .cast("region", "smallint");
    }
    q = q.group_by("rx").group_by("ry");
    if count_threshold > 0 {
        q = q
            .group_having(
                "count(*) > %(count_threshold)s",
                "count_threshold",
                Param::Int(count_threshold),
            )
            .cast("count_threshold", "bigint");
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
            "ROUND((ST_X(geog::geometry) - %(xdiv)s * 0.5) / %(xdiv)s)::integer AS rx",
            "ROUND((ST_Y(geog::geometry) - %(ydiv)s * 0.5) / %(ydiv)s)::integer AS ry",
            "count(*) AS strike_count",
            "max(\"timestamp\") as \"timestamp\"",
        ])
        .param("xdiv", Param::Float(grid.x_div))
        .cast("xdiv", "double precision")
        .param("ydiv", Param::Float(grid.y_div))
        .cast("ydiv", "double precision");
    q = add_time_interval(q, time_interval);
    q = q.group_by("rx").group_by("ry");
    if count_threshold > 0 {
        q = q
            .group_having(
                "count(*) > %(count_threshold)s",
                "count_threshold",
                Param::Int(count_threshold),
            )
            .cast("count_threshold", "bigint");
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
        .cast("end_time", "timestamptz")
        .param("binsize", Param::Int(binsize))
        .cast("binsize", "integer");
    q = add_time_interval(q, time_interval);
    q = q.group_by("interval").order_by("interval", false);

    if let Some(region) = region {
        q = q
            .condition("region = %(region)s")
            .param("region", Param::Int(region))
            .cast("region", "smallint");
    }

    if let Some(grid) = envelope {
        let env = grid.envelope().as_wkb_linear_ring();
        q = q
            .condition("ST_SetSRID(CAST(%(envelope)s AS geometry), %(envelope_srid)s) && geog")
            .param("envelope", Param::Bytea(env))
            // `CAST($n AS geometry)` is ambiguous: PostGIS registers both a
            // `bytea -> geometry` and a `text -> geometry` cast, so PostgreSQL
            // cannot infer the placeholder type and defaults it to `text`.
            // tokio-postgres then refuses to send the bytea value as text
            // (`error serializing parameter 4`), aborting every region/local
            // histogram request.  `::bytea` pins the type (psycopg2 is
            // unaffected because `psycopg2.Binary` already binds bytea).
            .cast("envelope", "bytea")
            .param("envelope_srid", Param::Int(4326))
            .cast("envelope_srid", "integer");
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
    fn strikes_query_omits_noop_transform() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let q = strikes_query(&interval, Some(IdInterval { start: 100 }));
        let expected = "SELECT id, \"timestamp\", nanoseconds, \
             ST_X(geog::geometry) AS x, \
             ST_Y(geog::geometry) AS y, \
             altitude, amplitude, error2d, stationcount \
             FROM strikes WHERE \"timestamp\" >= %(start_time)s AND \"timestamp\" < %(end_time)s \
             AND id >= %(start_id)s ORDER BY id";
        assert_eq!(q.to_sql(), expected);
        assert_eq!(q.parameters().len(), 3); // start_time, end_time, start_id
                                             // The PostgreSQL rendering adds explicit casts (see `Query::casts`).
        assert_eq!(
            q.to_postgres(),
            "SELECT id, \"timestamp\", nanoseconds, \
             ST_X(geog::geometry) AS x, \
             ST_Y(geog::geometry) AS y, \
             altitude, amplitude, error2d, stationcount \
             FROM strikes WHERE \"timestamp\" >= $1::timestamptz AND \"timestamp\" < $2::timestamptz \
             AND id >= $3::bigint ORDER BY id"
        );
    }

    /// Regression for the `0x00` / "invalid byte sequence for encoding UTF8"
    /// failure: without an explicit `::integer`, PostgreSQL cannot choose between
    /// `ST_Transform(geometry, integer)` and `ST_Transform(geometry, text)` and
    /// defaults the placeholder to `text`; tokio-postgres then sends the integer
    /// in binary form and the server rejects the NUL byte.  Any emitted
    /// `ST_Transform` must therefore carry the cast (a non-default SRID here).
    #[test]
    fn to_postgres_adds_explicit_casts() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let q = select_query(
            &interval,
            None,
            Some(1),
            3857,
            SelectColumns::Strike,
            SelectOrder::Timestamp,
        );
        let sql = q.to_postgres();
        assert!(sql.contains("ST_X(ST_Transform(geog::geometry, $1::integer))"));
        assert!(sql.contains("ST_Y(ST_Transform(geog::geometry, $1::integer))"));
        assert!(sql.contains("\"timestamp\" >= $2::timestamptz"));
        assert!(sql.contains("\"timestamp\" < $3::timestamptz"));
        assert!(sql.contains("region = $4::smallint"));
        // No bare (ambiguous, server-inferred) integer placeholder remains.
        assert!(!sql.contains("geog::geometry, $1)"));
    }

    /// A query without geometry still casts its integer/timestamp params.
    #[test]
    fn grid_query_to_postgres_casts_all_numeric_params() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let grid = Grid::new(-25.0, 56.0, 27.0, 71.0, 0.14, 0.08);
        let sql = grid_query(&grid, &interval, Some(1), 0).to_postgres();
        assert!(!sql.contains("ST_Transform"));
        assert!(!sql.contains("%(xmin)s"));
        assert!(sql.contains("$1::double precision")); // xmin
        assert!(sql.contains("$6::integer")); // envelope_srid
        assert!(sql.contains("region = $9::smallint"));
    }

    #[test]
    fn grid_query_omits_noop_transform() {
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
             TRUNC((ST_X(geog::geometry) - %(xmin)s) / %(xdiv)s)::integer AS rx, \
             TRUNC((ST_Y(geog::geometry) - %(ymin)s) / %(ydiv)s)::integer AS ry, \
             count(*) AS strike_count, max(\"timestamp\") as \"timestamp\" \
             FROM strikes WHERE \
             ST_GeomFromWKB(%(envelope)s, %(envelope_srid)s) && geog AND \
             \"timestamp\" >= %(start_time)s AND \"timestamp\" < %(end_time)s \
             AND region = %(region)s \
             GROUP BY rx, ry";
        assert_eq!(q.to_sql(), expected);
        // Param order follows SQL token order
        let params = q.parameters();
        assert_eq!(params.len(), 9);
        assert!(matches!(params[0], Param::Float(_))); // xmin
        assert!(matches!(params[4], Param::Bytea(_))); // envelope
        assert!(matches!(params[5], Param::Int(4326))); // envelope_srid
        assert!(matches!(params[6], Param::Timestamp(_))); // start_time
        assert!(matches!(params[7], Param::Timestamp(_))); // end_time
        assert!(matches!(params[8], Param::Int(1))); // region
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
        assert_eq!(q.parameters().len(), 8);
    }

    #[test]
    fn global_grid_query_omits_noop_transform() {
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
             ROUND((ST_X(geog::geometry) - %(xdiv)s * 0.5) / %(xdiv)s)::integer AS rx, \
             ROUND((ST_Y(geog::geometry) - %(ydiv)s * 0.5) / %(ydiv)s)::integer AS ry, \
             count(*) AS strike_count, max(\"timestamp\") as \"timestamp\" \
             FROM strikes WHERE \
             \"timestamp\" >= %(start_time)s AND \"timestamp\" < %(end_time)s \
             GROUP BY rx, ry";
        assert_eq!(q.to_sql(), expected);
        // xdiv, ydiv, start_time, end_time = 4 parameters
        let params = q.parameters();
        assert_eq!(params.len(), 4);
        assert!(matches!(params[0], Param::Float(_)));
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
        assert_eq!(q.to_postgres(), "SELECT -extract( epoch from $1::timestamptz - \"timestamp\")::int/60/$2::integer as interval, count(*) FROM strikes WHERE \"timestamp\" >= $3::timestamptz AND \"timestamp\" < $1::timestamptz GROUP BY interval ORDER BY interval");
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
        assert!(sql
            .contains("AND ST_SetSRID(CAST(%(envelope)s AS geometry), %(envelope_srid)s) && geog"));
        let params = q.parameters();
        assert_eq!(params.len(), 6); // end_time, binsize, start_time, region, envelope, envelope_srid
        assert!(matches!(params[3], Param::Int(1)));
        assert!(matches!(params[4], Param::Bytea(_)));
        assert!(matches!(params[5], Param::Int(4326)));
    }

    /// Regression: the histogram envelope parameter sits inside
    /// `CAST($n AS geometry)`, which PostGIS registers for both `bytea` and
    /// `text`.  Without an explicit `::bytea` PostgreSQL defaults the
    /// placeholder to `text` and tokio-postgres fails to serialize the bytea
    /// value ("error serializing parameter 4"), aborting every region/local
    /// histogram request.  (The psycopg2 form stays unchanged.)
    #[test]
    fn histogram_envelope_param_is_cast_to_bytea() {
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
        let sql = q.to_postgres();
        assert!(
            sql.contains("CAST($5::bytea AS geometry)"),
            "expected an explicit bytea cast, got: {sql}"
        );
        // The psycopg2 rendering keeps the Python placeholder untouched.
        assert!(q.to_sql().contains("CAST(%(envelope)s AS geometry)"));
    }

    #[test]
    fn repeated_named_params_map_to_single_positional() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let area = Area::from_polygon(&[vec![
            [0.0, 0.0],
            [2.0, 0.0],
            [1.0, 1.0],
            [0.0, 2.0],
            [0.0, 0.0],
        ]])
        .unwrap();
        let q = select_query(
            &interval,
            Some(&area),
            None,
            3857,
            SelectColumns::Strike,
            SelectOrder::Timestamp,
        );
        let sql = q.to_postgres();
        // `srid` appears in both coordinate transforms and in the three
        // geometry calls, but is a single positional parameter.
        assert!(sql.contains("ST_X(ST_Transform(geog::geometry, $1::integer))"));
        assert!(sql.contains("ST_Y(ST_Transform(geog::geometry, $1::integer))"));
        assert_eq!(sql.matches("$1::integer").count(), 5);
        assert_eq!(q.parameters().len(), 5); // srid, start, end, envelope, geometry
    }

    #[test]
    fn select_query_orders_by_timestamp() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let q = select_query(
            &interval,
            None,
            None,
            4326,
            SelectColumns::Strike,
            SelectOrder::Timestamp,
        );
        let expected = "SELECT id, \"timestamp\", nanoseconds, \
             ST_X(geog::geometry) AS x, \
             ST_Y(geog::geometry) AS y, \
             altitude, amplitude, error2d, stationcount \
             FROM strikes WHERE \"timestamp\" >= %(start_time)s AND \"timestamp\" < %(end_time)s \
             ORDER BY \"timestamp\", nanoseconds";
        assert_eq!(q.to_sql(), expected);
    }

    #[test]
    fn select_query_default_srid_omits_noop_transform() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let sql = |srid| {
            select_query(
                &interval,
                None,
                None,
                srid,
                SelectColumns::Strike,
                SelectOrder::Timestamp,
            )
            .to_sql()
        };
        assert!(sql(DEFAULT_SRID).contains("ST_X(geog::geometry) AS x"));
        assert!(!sql(DEFAULT_SRID).contains("ST_Transform"));
        assert!(sql(3857).contains("ST_X(ST_Transform(geog::geometry, %(srid)s)) AS x"));
    }

    #[test]
    fn select_query_order_variants() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let sql = |order| {
            select_query(&interval, None, None, 4326, SelectColumns::StrikeKey, order).to_sql()
        };
        assert!(!sql(SelectOrder::None).contains("ORDER BY"));
        assert!(sql(SelectOrder::Id).ends_with("ORDER BY id"));
        assert!(sql(SelectOrder::Timestamp).ends_with("ORDER BY \"timestamp\", nanoseconds"));
    }

    #[test]
    fn select_strike_key_columns_match_python() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let q = select_query(
            &interval,
            None,
            None,
            4326,
            SelectColumns::StrikeKey,
            SelectOrder::None,
        );
        let expected = "SELECT \"timestamp\", nanoseconds, \
             ST_X(geog::geometry) AS x, \
             ST_Y(geog::geometry) AS y, error2d \
             FROM strikes WHERE \"timestamp\" >= %(start_time)s AND \"timestamp\" < %(end_time)s";
        assert_eq!(q.to_sql(), expected);
        // The default SRID is not referenced, so only the interval remains.
        assert_eq!(q.parameters().len(), 2);
    }

    #[test]
    fn select_strike_key_columns_with_region() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let q = select_query(
            &interval,
            None,
            Some(7),
            4326,
            SelectColumns::StrikeKey,
            SelectOrder::None,
        );
        assert!(q.to_sql().contains("region = %(region)s"));
        assert!(matches!(q.parameters().last(), Some(Param::Int(7))));
    }

    #[test]
    fn select_query_with_envelope_area_adds_bbox_only() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let area = Area::from_envelope(&Envelope::new(10.0, 12.0, 50.0, 52.0));
        let q = select_query(
            &interval,
            Some(&area),
            None,
            4326,
            SelectColumns::Strike,
            SelectOrder::Timestamp,
        );
        let sql = q.to_sql();
        // Default SRID is inlined, so no `srid` parameter is declared.
        assert!(sql.contains("ST_GeomFromWKB(%(envelope)s, 4326) && geog"));
        assert!(!sql.contains("%(srid)s"));
        assert!(!sql.contains("ST_Intersects"));
        assert!(q.parameters().iter().any(|p| matches!(p, Param::Bytea(_))));
    }

    #[test]
    fn select_query_with_polygon_area_adds_intersects() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let area = Area::from_polygon(&[vec![
            [0.0, 0.0],
            [2.0, 0.0],
            [1.0, 1.0],
            [0.0, 2.0],
            [0.0, 0.0],
        ]])
        .unwrap();
        let q = select_query(
            &interval,
            Some(&area),
            None,
            4326,
            SelectColumns::Strike,
            SelectOrder::Timestamp,
        );
        let sql = q.to_sql();
        // Default SRID is inlined and the no-op transform is dropped.
        assert!(sql.contains("ST_Intersects(ST_GeomFromWKB(%(geometry)s, 4326), geog)"));
        assert!(!sql.contains("%(srid)s"));
        // two bytea params: envelope and geometry
        let bytea = q
            .parameters()
            .iter()
            .filter(|p| matches!(p, Param::Bytea(_)))
            .count();
        assert_eq!(bytea, 2);
    }

    #[test]
    fn select_query_area_matching_envelope_skips_intersects() {
        let interval = TimeInterval::new(utc(2020, 1, 1, 0, 0, 0), utc(2020, 1, 1, 0, 5, 0));
        let area = Area::from_polygon(&[vec![
            [0.0, 0.0],
            [0.0, 2.0],
            [2.0, 2.0],
            [2.0, 0.0],
            [0.0, 0.0],
        ]])
        .unwrap();
        assert!(area.geometry_wkb.is_none());
        let q = select_query(
            &interval,
            Some(&area),
            None,
            4326,
            SelectColumns::Strike,
            SelectOrder::Timestamp,
        );
        assert!(!q.to_sql().contains("ST_Intersects"));
    }

    #[test]
    fn parse_wkt_polygon_envelope() {
        let area = parse_wkt_polygon("POLYGON((0 0, 1 0, 1 1, 0 1, 0 0))").unwrap();
        assert!(area.geometry_wkb.is_none(), "square is its own envelope");
    }

    #[test]
    fn parse_wkt_polygon_non_envelope() {
        let area = parse_wkt_polygon("POLYGON((0 0, 2 0, 1 1, 0 2, 0 0))").unwrap();
        assert!(area.geometry_wkb.is_some());
    }

    #[test]
    fn parse_wkt_polygon_with_hole() {
        let area =
            parse_wkt_polygon("POLYGON((0 0, 4 0, 4 4, 0 4, 0 0),(1 1, 2 1, 2 2, 1 2, 1 1))")
                .unwrap();
        assert!(area.geometry_wkb.is_some());
    }

    #[test]
    fn parse_wkt_rejects_non_polygon() {
        assert!(parse_wkt_polygon("POINT(0 0)").is_none());
        assert!(parse_wkt_polygon("POLYGON EMPTY").is_none());
        assert!(parse_wkt_polygon("not wkt").is_none());
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
