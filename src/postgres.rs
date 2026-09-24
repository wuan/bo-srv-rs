//! Production [`QueryExecutor`] backed by tokio-postgres.
//!
//! A single `tokio_postgres::Client` is shared across connections
//! (tokio-postgres multiplexes queries over the wire), which matches the
//! connection-pool behaviour of the Python layer for the purposes of this
//! port.  Queries are executed synchronously from the caller's perspective by
//! blocking on the Tokio runtime handle.

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
    /// `timestamp` columns in the strike table are interpreted as UTC wall
    /// clock (matching the Python `Timestamp.from_timestamp` behaviour), so
    /// parameters are bound as naive UTC timestamps.
    Timestamp(NaiveDateTime),
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
            Param::Timestamp(ts) => PgParam::Timestamp(ts.naive_utc()),
        }
    }
}

impl<'a> ToSql for PgParam<'a> {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        match self {
            PgParam::Null => Ok(IsNull::Yes),
            PgParam::Int(i) => i.to_sql(ty, out),
            PgParam::Float(f) => f.to_sql(ty, out),
            PgParam::Text(s) => s.to_sql(ty, out),
            PgParam::Bool(b) => b.to_sql(ty, out),
            PgParam::Bytea(b) => b.to_sql(ty, out),
            PgParam::Timestamp(ts) => ts.to_sql(ty, out),
        }
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }

    fn to_sql_checked(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        match self {
            PgParam::Null => Ok(IsNull::Yes),
            PgParam::Int(i) => i.to_sql_checked(ty, out),
            PgParam::Float(f) => f.to_sql_checked(ty, out),
            PgParam::Text(s) => s.to_sql_checked(ty, out),
            PgParam::Bool(b) => b.to_sql_checked(ty, out),
            PgParam::Bytea(b) => b.to_sql_checked(ty, out),
            PgParam::Timestamp(ts) => ts.to_sql_checked(ty, out),
        }
    }
}

/// Convert a tokio-postgres row into the crate's [`Row`] by mapping column
/// types to [`Value`] variants.
fn convert_row(row: &PgRow) -> Result<Row, Box<dyn Error + Sync + Send>> {
    let mut values = Vec::with_capacity(row.len());
    for i in 0..row.len() {
        let ty = row.columns()[i].type_();
        let value = if *ty == Type::INT2 || *ty == Type::INT4 {
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
pub struct PostgresExecutor {
    client: tokio_postgres::Client,
    handle: tokio::runtime::Handle,
}

impl PostgresExecutor {
    /// Connect asynchronously; call from within a Tokio runtime.
    pub async fn connect(config: &Config) -> Result<Self, Box<dyn Error + Sync + Send>> {
        let (client, connection) =
            tokio_postgres::connect(&config.db_connection_string(), tokio_postgres::NoTls).await?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let handle = tokio::runtime::Handle::current();
        Ok(PostgresExecutor { client, handle })
    }
}

impl QueryExecutor for PostgresExecutor {
    fn query(&self, sql: &str, params: &[Param]) -> Result<Vec<Row>, Box<dyn std::error::Error + Send + Sync>> {
        let pg_params: Vec<PgParam<'_>> = params.iter().map(PgParam::from_param).collect();
        let refs: Vec<&(dyn ToSql + Sync)> = pg_params
            .iter()
            .map(|p| p as &(dyn ToSql + Sync))
            .collect();

        let rows = self
            .handle
            .block_on(self.client.query(sql, &refs))
            .map_err(|e| -> Box<dyn Error + Send + Sync> { Box::new(e) })?;

        rows.iter().map(convert_row).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::Param;

    #[test]
    fn pg_param_adapter_maps_values() {
        let param = Param::Timestamp(
            DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
        );
        let p = PgParam::from_param(&param);
        match p {
            PgParam::Timestamp(naive) => assert_eq!(naive.and_utc().timestamp(), 1_700_000_000),
            _ => panic!("expected timestamp"),
        }
    }
}