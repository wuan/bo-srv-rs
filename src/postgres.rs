//! Production [`QueryExecutor`] backed by tokio-postgres.
//!
//! A single `tokio_postgres::Client` is shared across connections
//! (tokio-postgres multiplexes queries over the wire), which matches the
//! connection-pool behaviour of the Python layer for the purposes of this
//! port.  Queries are awaited directly by the caller; the executor never blocks
//! a runtime worker thread.

use std::error::Error;

use bytes::BytesMut;
use chrono::{DateTime, NaiveDateTime, Utc};
use tokio_postgres::types::{IsNull, ToSql, Type};
use tokio_postgres::Row as PgRow;

use crate::config::Config;
use crate::executor::{Param, QueryExecutor, Row, Value};

/// Parameter adapter that can reference borrowed data from [`Param`].
#[derive(Debug)]
enum PgParam<'a> {
    Null,
    Int(i64),
    Float(f64),
    Text(&'a str),
    Bool(bool),
    Bytea(&'a [u8]),
    /// The `"timestamp"` strike column and query parameters.  The Python layer
    /// treats these as UTC wall clock (`Timestamp.from_timestamp`).  Depending
    /// on the server-inferred type the value is bound as `TIMESTAMPTZ`
    /// (`DateTime<Utc>`, the deployed schema) or as a naive `TIMESTAMP`.
    Timestamp(DateTime<Utc>),
}

impl<'a> PgParam<'a> {
    fn from_param(p: &'a Param) -> Self {
        match p {
            Param::Null => PgParam::Null,
            Param::Int(i) => PgParam::Int(*i),
            Param::Float(f) => PgParam::Float(*f),
            Param::Text(s) => PgParam::Text(s),
            Param::Bool(b) => PgParam::Bool(*b),
            Param::Bytea(b) => PgParam::Bytea(b),
            Param::Timestamp(ts) => PgParam::Timestamp(*ts),
        }
    }
}

/// Serialize an integer parameter against the *server-inferred* target type.
///
/// tokio-postgres' own `i64` implementation only accepts `INT8`, but the
/// generated SQL binds integers where PostgreSQL infers `INT4` (e.g. the
/// `srid` argument of `ST_Transform(..., $1)`) or `INT2` (the `SMALLINT`
/// strike columns).  Dispatch on `ty` and narrow as needed.
fn int_to_sql(value: i64, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
    if *ty == Type::INT2 {
        (value as i16).to_sql(ty, out)
    } else if *ty == Type::INT4 {
        (value as i32).to_sql(ty, out)
    } else if *ty == Type::INT8 {
        value.to_sql(ty, out)
    } else if *ty == Type::OID {
        (value as u32).to_sql(ty, out)
    } else if *ty == Type::FLOAT4 {
        (value as f32).to_sql(ty, out)
    } else if *ty == Type::FLOAT8 {
        (value as f64).to_sql(ty, out)
    } else if *ty == Type::NUMERIC {
        // NUMERIC has no native Rust mapping; send the decimal text form.
        value.to_string().as_str().to_sql(ty, out)
    } else {
        value.to_sql(ty, out)
    }
}

/// Serialize a floating-point parameter against the server-inferred target
/// type (REAL/`FLOAT4` needs an `f32` payload, `FLOAT8` an `f64`).
fn float_to_sql(value: f64, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
    if *ty == Type::FLOAT4 {
        (value as f32).to_sql(ty, out)
    } else if *ty == Type::FLOAT8 {
        value.to_sql(ty, out)
    } else if *ty == Type::INT2 {
        (value.round() as i16).to_sql(ty, out)
    } else if *ty == Type::INT4 {
        (value.round() as i32).to_sql(ty, out)
    } else if *ty == Type::INT8 {
        (value.round() as i64).to_sql(ty, out)
    } else if *ty == Type::NUMERIC {
        value.to_string().as_str().to_sql(ty, out)
    } else {
        value.to_sql(ty, out)
    }
}

/// Serialize a timestamp against the server-inferred type.  `TIMESTAMPTZ`
/// (the deployed `"timestamp"` column) takes an absolute `DateTime<Utc>`;
/// a naive `TIMESTAMP` takes the UTC wall clock.
fn timestamp_to_sql(
    value: DateTime<Utc>,
    ty: &Type,
    out: &mut BytesMut,
) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
    if *ty == Type::TIMESTAMPTZ {
        value.to_sql(ty, out)
    } else if *ty == Type::TIMESTAMP {
        value.naive_utc().to_sql(ty, out)
    } else {
        value.to_sql(ty, out)
    }
}

impl<'a> ToSql for PgParam<'a> {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        match self {
            PgParam::Null => Ok(IsNull::Yes),
            PgParam::Int(i) => int_to_sql(*i, ty, out),
            PgParam::Float(f) => float_to_sql(*f, ty, out),
            PgParam::Text(s) => s.to_sql(ty, out),
            PgParam::Bool(b) => b.to_sql(ty, out),
            PgParam::Bytea(b) => b.to_sql(ty, out),
            PgParam::Timestamp(ts) => timestamp_to_sql(*ts, ty, out),
        }
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }

    fn to_sql_checked(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        match self {
            PgParam::Null => Ok(IsNull::Yes),
            PgParam::Int(i) => int_to_sql(*i, ty, out),
            PgParam::Float(f) => float_to_sql(*f, ty, out),
            PgParam::Text(s) => s.to_sql_checked(ty, out),
            PgParam::Bool(b) => b.to_sql_checked(ty, out),
            PgParam::Bytea(b) => b.to_sql_checked(ty, out),
            PgParam::Timestamp(ts) => timestamp_to_sql(*ts, ty, out),
        }
    }
}

/// Convert a tokio-postgres row into the crate's [`Row`] by mapping column
/// types to [`Value`] variants.
fn convert_row(row: &PgRow) -> Result<Row, Box<dyn Error + Sync + Send>> {
    let mut values = Vec::with_capacity(row.len());
    for i in 0..row.len() {
        let ty = row.columns()[i].type_();
        // Read each integer width with its own Rust type: tokio-postgres
        // rejects `i32` for an `int2` column ("cannot convert between the Rust
        // type `i32` and the Postgres type `int2`").
        let value = if *ty == Type::INT2 {
            Value::Int(i64::from(row.try_get::<_, i16>(i)?))
        } else if *ty == Type::INT4 {
            Value::Int(i64::from(row.try_get::<_, i32>(i)?))
        } else if *ty == Type::INT8 {
            Value::Int(row.try_get::<_, i64>(i)?)
        } else if *ty == Type::FLOAT4 {
            Value::Float(f64::from(row.try_get::<_, f32>(i)?))
        } else if *ty == Type::FLOAT8 {
            Value::Float(row.try_get::<_, f64>(i)?)
        } else if *ty == Type::TIMESTAMP {
            let naive: NaiveDateTime = row.try_get(i)?;
            Value::Timestamp(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
        } else if *ty == Type::TIMESTAMPTZ {
            Value::Timestamp(row.try_get::<_, DateTime<Utc>>(i)?)
        } else if *ty == Type::BOOL {
            Value::Bool(row.try_get::<_, bool>(i)?)
        } else if *ty == Type::BYTEA {
            Value::Bytea(row.try_get::<_, Vec<u8>>(i)?)
        } else if *ty == Type::TEXT
            || *ty == Type::VARCHAR
            || *ty == Type::NAME
            || *ty == Type::BPCHAR
        {
            Value::Text(row.try_get::<_, String>(i)?)
        } else {
            Value::Null
        };
        values.push(value);
    }
    Ok(Row::new(values))
}

/// [`QueryExecutor`] implementation over a tokio-postgres client.
///
/// The async trait methods await the driver directly, so no runtime juggling or
/// thread blocking is involved.
pub struct PostgresExecutor {
    client: tokio_postgres::Client,
}

impl PostgresExecutor {
    /// Connect asynchronously; call from within a Tokio runtime.
    pub async fn connect(config: &Config) -> Result<Self, Box<dyn Error + Sync + Send>> {
        let (client, connection) =
            tokio_postgres::connect(&config.db_connection_string(), tokio_postgres::NoTls).await?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(PostgresExecutor { client })
    }
}

impl PostgresExecutor {
    /// Bind the crate parameters into tokio-postgres `ToSql` references.
    fn bind(params: &[Param]) -> Vec<PgParam<'_>> {
        params.iter().map(PgParam::from_param).collect()
    }

    fn to_refs<'a>(pg_params: &'a [PgParam<'a>]) -> Vec<&'a (dyn ToSql + Sync)> {
        pg_params
            .iter()
            .map(|p| p as &(dyn ToSql + Sync))
            .collect()
    }
}

#[async_trait::async_trait]
impl QueryExecutor for PostgresExecutor {
    async fn query(
        &self,
        sql: &str,
        params: &[Param],
    ) -> Result<Vec<Row>, Box<dyn std::error::Error + Send + Sync>> {
        let pg_params = Self::bind(params);
        let refs = Self::to_refs(&pg_params);

        let rows = self
            .client
            .query(sql, &refs)
            .await
            .map_err(|e| -> Box<dyn Error + Send + Sync> { Box::new(e) })?;

        rows.iter().map(convert_row).collect()
    }

    /// Execute a write statement.  Each statement runs in its own implicit
    /// transaction (tokio-postgres autocommit), which matches the observable
    /// behaviour of the Python tools' batched `insert_many` + `commit`.
    async fn execute(
        &self,
        sql: &str,
        params: &[Param],
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let pg_params = Self::bind(params);
        let refs = Self::to_refs(&pg_params);

        self.client
            .execute(sql, &refs)
            .await
            .map_err(|e| -> Box<dyn Error + Send + Sync> { Box::new(e) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::Param;

    /// Serialize `param` as the driver would (via `to_sql_checked`) for the
    /// server-inferred `ty`, returning the encoded bytes.
    fn encode(param: &Param, ty: &Type) -> Result<Vec<u8>, Box<dyn Error + Sync + Send>> {
        let pg = PgParam::from_param(param);
        let mut out = BytesMut::new();
        match pg.to_sql_checked(ty, &mut out)? {
            IsNull::Yes => Ok(Vec::new()),
            IsNull::No => Ok(out.to_vec()),
        }
    }

    #[test]
fn pg_param_adapter_maps_values() {
        let param = Param::Timestamp(
            DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
        );
        let p = PgParam::from_param(&param);
        match p {
            PgParam::Timestamp(ts) => assert_eq!(ts.timestamp(), 1_700_000_000),
            _ => panic!("expected timestamp"),
        }
    }

    /// The `"timestamp"` column is `timestamptz`; a `DateTime<Utc>` must
    /// serialize for it (and a naive TIMESTAMP must also work).
    #[test]
    fn timestamp_param_serializes_for_timestamptz_and_timestamp() {
        let param = Param::Timestamp(DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap());
        assert_eq!(encode(&param, &Type::TIMESTAMPTZ).unwrap().len(), 8);
        assert_eq!(encode(&param, &Type::TIMESTAMP).unwrap().len(), 8);
    }

    /// Regression: `Param::Int` must serialize for INT2/INT4/INT8, not just
    /// INT8 (the srid argument of `ST_Transform` is INT4, `region` and the
    /// other strike columns are INT2).
    #[test]
    fn int_param_serializes_for_int2_int4_int8() {
        let param = Param::Int(4326);

        let int2 = encode(&param, &Type::INT2).expect("INT2");
        assert_eq!(int2.len(), 2);
        assert_eq!(i16::from_be_bytes([int2[0], int2[1]]), 4326);

        let int4 = encode(&param, &Type::INT4).expect("INT4");
        assert_eq!(int4.len(), 4);
        assert_eq!(i32::from_be_bytes([int4[0], int4[1], int4[2], int4[3]]), 4326);

        let int8 = encode(&param, &Type::INT8).expect("INT8");
        assert_eq!(int8.len(), 8);
    }

    /// Negative and small values also round-trip through the narrowing.
    #[test]
    fn int_param_narrows_values() {
        let neg = Param::Int(-3);
        let int2 = encode(&neg, &Type::INT2).expect("INT2");
        assert_eq!(i16::from_be_bytes([int2[0], int2[1]]), -3);

        // A value that does not fit INT2 would wrap; the tools never send
        // such values (the SMALLINT columns hold 0..=999/32767/clamped
        // regions), so this only pins the encoding width.
        assert_eq!(encode(&Param::Int(999), &Type::INT2).unwrap().len(), 2);
    }

    /// Regression: `Param::Float` must serialize for FLOAT4 (the `amplitude`
    /// REAL column) as well as FLOAT8.
    #[test]
    fn float_param_serializes_for_float4_and_float8() {
        let param = Param::Float(12.5);

        let float4 = encode(&param, &Type::FLOAT4).expect("FLOAT4");
        assert_eq!(float4.len(), 4);
        assert_eq!(f32::from_be_bytes([float4[0], float4[1], float4[2], float4[3]]), 12.5);

        let float8 = encode(&param, &Type::FLOAT8).expect("FLOAT8");
        assert_eq!(float8.len(), 8);
        assert_eq!(f64::from_be_bytes(float8.try_into().unwrap()), 12.5);
    }

    /// `insert_many` binds every strike column against the type the server
/// infers from the schema; verify each one encodes.
    #[test]
    fn insert_column_types_encode() {
        // bigserial id is INT8; timestamp TIMESTAMPTZ; nanoseconds/region/
        // error2d/stationcount/altitude INT2; amplitude FLOAT4; geog is built
        // by ST_MakePoint from two FLOAT8 coordinates.
        let columns = [
            (Param::Timestamp(DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap()), Type::TIMESTAMPTZ),
            (Param::Int(700), Type::INT2),
            (Param::Float(8.91), Type::FLOAT8),
            (Param::Float(44.28), Type::FLOAT8),
            (Param::Int(500), Type::INT2),
            (Param::Int(1), Type::INT2),
            (Param::Float(10.5), Type::FLOAT4),
            (Param::Int(250), Type::INT2),
            (Param::Int(5), Type::INT2),
        ];
        for (param, ty) in columns {
            encode(&param, &ty).unwrap_or_else(|error| {
                panic!("failed to encode {param:?} as {ty:?}: {error}")
            });
        }
        // The `srid` parameter of ST_Transform is INT4.
        encode(&Param::Int(4326), &Type::INT4).expect("srid INT4");
    }

    /// The text parameter must not be affected by the numeric dispatch.
    #[test]
    fn text_and_null_params_still_encode() {
        assert_eq!(encode(&Param::Text("abc".into()), &Type::TEXT).unwrap(), b"abc");
        let null = encode(&Param::Null, &Type::INT4).unwrap();
        assert!(null.is_empty());
    }
}