//! Production [`QueryExecutor`] backed by a tokio-postgres connection pool.
//!
//! The executor owns a [`deadpool_postgres::Pool`] of `tokio_postgres::Client`
//! connections sized from `Config::db_connection_count` (the Python layer's
//! `connection_count`).  Each `query`/`execute` checks out a connection for the
//! duration of the statement, so up to `max_size` statements run against the
//! database in parallel instead of sharing one multiplexed socket.
//!
//! The pool is created **without connecting**: connections are opened on demand
//! and reused, and deadpool drops a connection whose socket has died
//! (`RecyclingMethod::Fast` checks `Client::is_closed`).  Queries are awaited
//! directly by the caller, so the executor never blocks a runtime worker
//! thread.
//!
//! Statements are prepared through each connection's deadpool statement cache
//! (`prepare_cached`), so a given SQL text is parsed and planned once per
//! connection instead of on every request.
//!
//! # Lazy / reconnecting mode
//!
//! [`PostgresExecutor::connect`] connects eagerly (checks out a connection) and
//! propagates a connect failure to the caller; the `service` binary used to use
//! it at startup and therefore **exited** whenever the database was
//! unreachable.  The webservice instead builds the executor with
//! [`PostgresExecutor::lazy`]: no connection is attempted at startup, and each
//! request that needs the database (`query`/`execute`) checks out (and creates
//! on demand) a connection.
//!
//! * A failed checkout/query returns an error *for that request only* — it
//!   never panics, hangs or exits the process.
//! * The failure is logged **once** (see [`FailureLog`]); repeated failures are
//!   silent so a missing database cannot spam the log.  A successful request
//!   re-arms the log, so a *later* outage is reported once again.
//! * If the database comes back the next request transparently reconnects
//!   (recovery without a restart); a connection that dies while held is dropped
//!   by the pool's recycle check and the following request creates a fresh one.
//!
//! The CLI tools keep using [`PostgresExecutor::connect`] (eager, error on
//! failure), so their behaviour is unchanged.

use std::error::Error;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use bytes::BytesMut;
use chrono::{DateTime, NaiveDateTime, Utc};
use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod, Runtime};
use tokio_postgres::types::{IsNull, ToSql, Type};
use tokio_postgres::Row as PgRow;

use crate::config::Config;
use crate::executor::{Param, QueryExecutor, Row, Value};
use crate::metrics::Metrics;

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
fn int_to_sql(
    value: i64,
    ty: &Type,
    out: &mut BytesMut,
) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
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
fn float_to_sql(
    value: f64,
    ty: &Type,
    out: &mut BytesMut,
) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
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
    fn to_sql(
        &self,
        ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
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

    fn to_sql_checked(
        &self,
        ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
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

/// First-occurrence-only failure log, shared by every request the executor
/// serves (and, if desired, several executors).
///
/// A connect/query failure calls [`FailureLog::note_failure`]; only the first
/// failure while the executor is "down" is logged.  [`FailureLog::note_success`]
/// re-arms the log, so *one* line is emitted per outage (an outage that follows
/// a recovery is reported again, once).  This keeps a missing database from
/// spamming the log with a line per request.
///
/// The [`FailureLog::log_count`] counter is exposed so tests can assert the
/// log-once behaviour without a capturing logger.
#[derive(Debug, Default)]
pub struct FailureLog {
    /// `true` once the current outage has been logged (and not yet re-armed by
    /// a success).
    logged: AtomicBool,
    /// How many times a failure has actually been logged.
    log_count: AtomicUsize,
}

impl FailureLog {
    pub fn new() -> Self {
        FailureLog::default()
    }

    /// Claim the right to log the current failure.  Returns `true` exactly once
    /// per outage (false while the outage has already been logged).
    pub fn should_log(&self) -> bool {
        !self.logged.swap(true, Ordering::SeqCst)
    }

    /// Record a failure; logs at WARN the first time (per outage).
    pub fn note_failure(&self, error: &(dyn Error + 'static)) {
        if self.should_log() {
            self.log_count.fetch_add(1, Ordering::SeqCst);
            log::warn!(
                "database unavailable: {}",
                crate::cli::format_error_chain(error)
            );
        }
    }

    /// A request succeeded: re-arm the log so a later outage is reported once
    /// more.
    pub fn note_success(&self) {
        self.logged.store(false, Ordering::SeqCst);
    }

    /// Number of failures that were actually logged (for tests/metrics).
    pub fn log_count(&self) -> usize {
        self.log_count.load(Ordering::SeqCst)
    }
}

/// [`QueryExecutor`] implementation over a deadpool-managed tokio-postgres
/// connection pool.
///
/// The async trait methods check out a connection from the pool, await the
/// driver directly and return the connection to the pool (a dropped
/// [`deadpool_postgres::Object`] is recycled), so no runtime juggling or thread
/// blocking is involved.
///
/// The pool may be built without a database being available: it starts out
/// empty and creates connections on demand, so a webservice instance can start
/// even when PostgreSQL is down.  A checkout that cannot create a connection
/// fails *for that request only*.
pub struct PostgresExecutor {
    pool: Pool,
    failures: Arc<FailureLog>,
    /// Optional `db.pool_wait` sink; the CLI tools run without metrics.
    metrics: Option<Arc<dyn Metrics>>,
}

impl PostgresExecutor {
    /// Connect eagerly; call from within a Tokio runtime.
    ///
    /// Checks out one connection to validate that the database is reachable and
    /// returns an error otherwise.  This is the constructor the CLI tools rely
    /// on; the webservice should use [`PostgresExecutor::lazy`] instead.
    pub async fn connect(config: &Config) -> Result<Self, Box<dyn Error + Sync + Send>> {
        let executor = Self::build(config)?;
        // The checkout (and thus the first connection attempt) happens now so a
        // down database is reported to the caller instead of at the first
        // request.
        drop(executor.acquire().await?);
        Ok(executor)
    }

    /// Build an executor that connects *lazily*: startup never touches the
    /// database, and each request checks out (creating on demand) a connection.
    /// A missing or unreachable database therefore cannot prevent the service
    /// from starting or from answering requests (with a per-request fault).
    ///
    /// Returns an error only when the configuration itself is unusable (e.g. a
    /// malformed connection string); no connection is attempted.
    pub fn lazy(config: &Config) -> Result<Self, Box<dyn Error + Sync + Send>> {
        Self::build(config)
    }

    /// Attach a metrics sink so connection checkout time is reported as
    /// `db.pool_wait` (the Python `StatsDMetrics.for_db_pool_wait`).
    pub fn with_metrics(mut self, metrics: Arc<dyn Metrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// The shared first-failure-only log (for tests and diagnostics).
    pub fn failures(&self) -> &Arc<FailureLog> {
        &self.failures
    }

    /// The underlying pool (for diagnostics/tests).
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Build the pool from the configuration without opening any connection.
    ///
    /// The connection string is the same make_dsn-style string the previous
    /// single-client executor passed to `tokio_postgres::connect`, so the
    /// INI/env configuration keeps working unchanged.  `db_connection_count`
    /// now sizes the pool (clamped to at least one).
    fn build(config: &Config) -> Result<Self, Box<dyn Error + Sync + Send>> {
        let pg_config: tokio_postgres::Config = config.db_connection_string().parse()?;
        let manager = Manager::from_config(
            pg_config,
            tokio_postgres::NoTls,
            ManagerConfig {
                recycling_method: RecyclingMethod::Fast,
            },
        );
        let max_size = config.db_connection_count.max(1) as usize;
        let pool = Pool::builder(manager)
            .max_size(max_size)
            .runtime(Runtime::Tokio1)
            .build()?;
        Ok(PostgresExecutor {
            pool,
            failures: Arc::new(FailureLog::new()),
            metrics: None,
        })
    }

    /// Prepare `sql` on `client`, reusing that connection's statement cache.
    ///
    /// tokio-postgres' own `query(&str, ...)`/`execute(&str, ...)` parse and
    /// plan the statement on every call.  `ClientWrapper::prepare_cached`
    /// instead keeps the [`tokio_postgres::Statement`] in a per-connection
    /// cache keyed by the SQL text, so only the first use of a given statement
    /// on a connection costs a round trip (the SQL text is fixed per query
    /// shape; values are always bound as parameters).  The cache lives in the
    /// connection, so a `Statement` is never used on the client that did not
    /// prepare it.
    ///
    /// A prepare failure on a closed connection is treated like a query outage
    /// and logged once; an ordinary SQL error is returned without touching the
    /// failure log.
    async fn prepare_cached(
        &self,
        client: &deadpool_postgres::Object,
        sql: &str,
    ) -> Result<tokio_postgres::Statement, Box<dyn Error + Sync + Send>> {
        match client.prepare_cached(sql).await {
            Ok(statement) => Ok(statement),
            Err(error) => {
                if client.is_closed() {
                    self.failures.note_failure(&error);
                }
                Err(Box::new(error))
            }
        }
    }

    /// Check out a pooled connection, reporting the wait as `db.pool_wait`.
    ///
    /// A checkout failure (no connection could be created) is an outage: log it
    /// once via [`FailureLog`].  A success re-arms the log, so a later outage is
    /// reported once more.
    async fn acquire(&self) -> Result<deadpool_postgres::Object, Box<dyn Error + Sync + Send>> {
        let started = Instant::now();
        let result = self.pool.get().await;
        if let Some(metrics) = &self.metrics {
            metrics.for_db_pool_wait(started.elapsed().as_secs_f64());
        }
        match result {
            Ok(client) => {
                self.failures.note_success();
                Ok(client)
            }
            Err(error) => {
                self.failures.note_failure(&error);
                Err(Box::new(error))
            }
        }
    }
}

impl PostgresExecutor {
    /// Bind the crate parameters into tokio-postgres `ToSql` references.
    fn bind(params: &[Param]) -> Vec<PgParam<'_>> {
        params.iter().map(PgParam::from_param).collect()
    }

    fn to_refs<'a>(pg_params: &'a [PgParam<'a>]) -> Vec<&'a (dyn ToSql + Sync)> {
        pg_params.iter().map(|p| p as &(dyn ToSql + Sync)).collect()
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

        let client = self.acquire().await?;
        let statement = self.prepare_cached(&client, sql).await?;
        let rows = match client.query(&statement, &refs).await {
            Ok(rows) => {
                self.failures.note_success();
                rows
            }
            Err(error) => {
                // A closed connection is a database outage: the pool drops it
                // when the connection is recycled, and the first occurrence is
                // logged.
                if client.is_closed() {
                    self.failures.note_failure(&error);
                }
                return Err(Box::new(error));
            }
        };

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

        let client = self.acquire().await?;
        let statement = self.prepare_cached(&client, sql).await?;
        match client.execute(&statement, &refs).await {
            Ok(affected) => {
                self.failures.note_success();
                Ok(affected)
            }
            Err(error) => {
                if client.is_closed() {
                    self.failures.note_failure(&error);
                }
                Err(Box::new(error))
            }
        }
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
        let param = Param::Timestamp(DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap());
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
        assert_eq!(
            i32::from_be_bytes([int4[0], int4[1], int4[2], int4[3]]),
            4326
        );

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
        assert_eq!(
            f32::from_be_bytes([float4[0], float4[1], float4[2], float4[3]]),
            12.5
        );

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
            (
                Param::Timestamp(DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap()),
                Type::TIMESTAMPTZ,
            ),
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
            encode(&param, &ty)
                .unwrap_or_else(|error| panic!("failed to encode {param:?} as {ty:?}: {error}"));
        }
        // The `srid` parameter of ST_Transform is INT4.
        encode(&Param::Int(4326), &Type::INT4).expect("srid INT4");
    }

    /// The text parameter must not be affected by the numeric dispatch.
    #[test]
    fn text_and_null_params_still_encode() {
        assert_eq!(
            encode(&Param::Text("abc".into()), &Type::TEXT).unwrap(),
            b"abc"
        );
        let null = encode(&Param::Null, &Type::INT4).unwrap();
        assert!(null.is_empty());
    }

    // -----------------------------------------------------------------
    // lazy/pooled executor + first-failure-only logging
    // -----------------------------------------------------------------

    use crate::config::Config;
    use std::net::TcpListener;

    /// A config pointing at a TCP port nothing is listening on.  Binding an
    /// ephemeral port and immediately dropping the listener reserves the port
    /// for long enough that the connection attempt is refused.
    fn config_for_dead_port() -> Config {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port().to_string();
        drop(listener);
        Config {
            db_host: "127.0.0.1".into(),
            db_port: port,
            db_name: "blitzortung".into(),
            db_user: "blitzortung".into(),
            db_password: "blitzortung".into(),
            ..Config::default()
        }
    }

    /// The lazy executor never attempts a connection at construction time, so
    /// building it cannot fail or block on a dead database.
    #[tokio::test]
    async fn lazy_executor_construction_does_not_connect() {
        let executor = PostgresExecutor::lazy(&config_for_dead_port()).unwrap();
        // No connection has been created yet, so nothing was logged and the
        // pool is empty.
        assert_eq!(executor.failures().log_count(), 0);
        assert_eq!(executor.pool().status().size, 0);
    }

    /// A query against a dead database returns an error (not a panic/hang) and
    /// leaves the pool empty so the next request retries.
    #[tokio::test]
    async fn query_against_unreachable_database_errors_without_panicking() {
        let executor = PostgresExecutor::lazy(&config_for_dead_port()).unwrap();
        let error = executor.query("SELECT 1", &[]).await;
        assert!(error.is_err(), "expected a connection error");
        // The connection-refused path is a normal error, not a panic, and the
        // failed attempt is logged once.
        assert_eq!(executor.pool().status().size, 0);
        assert_eq!(executor.failures().log_count(), 1);
    }

    /// The pool is sized from `db_connection_count` (and never zero).
    #[tokio::test]
    async fn pool_is_sized_from_connection_count() {
        let config = Config {
            db_connection_count: 7,
            ..config_for_dead_port()
        };
        let executor = PostgresExecutor::lazy(&config).unwrap();
        assert_eq!(executor.pool().status().max_size, 7);

        let zero = Config {
            db_connection_count: 0,
            ..config_for_dead_port()
        };
        let executor = PostgresExecutor::lazy(&zero).unwrap();
        assert_eq!(executor.pool().status().max_size, 1);
    }

    /// The first failed connect logs once; repeated failures while the database
    /// stays down are silent.  A success re-arms the log so a *later* failure
    /// is reported once more.
    #[tokio::test]
    async fn consecutive_failures_are_logged_once_until_a_success_rearms() {
        let executor = PostgresExecutor::lazy(&config_for_dead_port()).unwrap();
        for _ in 0..5 {
            assert!(executor.query("SELECT 1", &[]).await.is_err());
        }
        assert_eq!(
            executor.failures().log_count(),
            1,
            "only the first failure of an outage is logged"
        );

        // A success re-arms the tracker (simulated directly: there is no live
        // database in this unit test).
        executor.failures().note_success();
        assert!(executor.query("SELECT 1", &[]).await.is_err());
        assert_eq!(
            executor.failures().log_count(),
            2,
            "a failure after a recovery is logged once more"
        );
        // ...and then stays quiet again.
        assert!(executor.query("SELECT 1", &[]).await.is_err());
        assert_eq!(executor.failures().log_count(), 2);
    }

    /// The tracker itself: `should_log` is true exactly once per outage.
    #[test]
    fn failure_log_claims_the_log_once() {
        let log = FailureLog::new();
        assert!(log.should_log());
        assert!(!log.should_log());
        assert!(!log.should_log());
        log.note_success();
        assert!(log.should_log());
    }
}
