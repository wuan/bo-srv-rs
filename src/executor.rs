//! Database abstraction: the [`QueryExecutor`] trait and the value types used
//! to exchange parameters and rows between the service layer and the
//! database.
//!
//! The service layer talks exclusively to this trait; the production
//! implementation lives in [`crate::postgres`] and a scriptable mock in
//! [`crate::mock`], which is what the unit tests use.

use chrono::{DateTime, Utc};

/// A query parameter sent to the database.
#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Bool(bool),
    Bytea(Vec<u8>),
    Timestamp(DateTime<Utc>),
}

/// A value returned from a query row.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Bool(bool),
    Bytea(Vec<u8>),
    /// Naive `timestamp` column interpreted as UTC (as the Python layer
    /// does with `Timestamp.from_timestamp`).
    Timestamp(DateTime<Utc>),
}

impl Value {
    /// Decimal value as i64 (used for int4/int8 and `count(*)`).
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
}

/// A single result row, addressed by position like a `psycopg2`
/// `DictRow` is addressed by column name.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    values: Vec<Value>,
}

impl Row {
    pub fn new(values: Vec<Value>) -> Self {
        Row { values }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&Value> {
        self.values.get(index)
    }

    pub fn get_i64(&self, index: usize) -> Option<i64> {
        match self.get(index) {
            Some(Value::Int(i)) => Some(*i),
            Some(Value::Float(f)) => Some(*f as i64),
            _ => None,
        }
    }

    pub fn get_f64(&self, index: usize) -> Option<f64> {
        match self.get(index) {
            Some(Value::Float(f)) => Some(*f),
            Some(Value::Int(i)) => Some(*i as f64),
            _ => None,
        }
    }

    pub fn get_timestamp(&self, index: usize) -> Option<DateTime<Utc>> {
        match self.get(index) {
            Some(Value::Timestamp(ts)) => Some(*ts),
            _ => None,
        }
    }
}

/// Executes parameterized queries against an SQL store.
///
/// The methods mirror what the Python service layer uses from the connection
/// pool (`connection.runQuery(sql, params)`).
///
/// The trait is **asynchronous**: the service handlers `.await` database work
/// instead of parking a runtime worker thread on it.  This removes the
/// synchronous-executor throughput ceiling and lets the cache coalesce
/// concurrent in-flight queries (see [`crate::cache::ObjectCache`]).
#[async_trait::async_trait]
pub trait QueryExecutor: Send + Sync {
    /// Run a query returning rows.
    async fn query(
        &self,
        sql: &str,
        params: &[Param],
    ) -> Result<Vec<Row>, Box<dyn std::error::Error + Send + Sync>>;

    /// Execute a statement that does not return rows (INSERT/UPDATE/DDL) and
    /// report the number of affected rows.
    ///
    /// The default implementation rejects the call so existing executor
    /// implementations remain valid; the write-capable implementations used by
    /// the CLI tools (`postgres` and `mock`) override it.
    async fn execute(
        &self,
        _sql: &str,
        _params: &[Param],
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        Err("execute is not supported by this executor".into())
    }

    /// Commit the current transaction (no-op by default).
    async fn commit(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }

    /// Roll back the current transaction (no-op by default).
    async fn rollback(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }
}

/// Blanket implementation so `&E where E: QueryExecutor` also works.
#[async_trait::async_trait]
impl<T: QueryExecutor + ?Sized> QueryExecutor for &T {
    async fn query(
        &self,
        sql: &str,
        params: &[Param],
    ) -> Result<Vec<Row>, Box<dyn std::error::Error + Send + Sync>> {
        (**self).query(sql, params).await
    }

    async fn execute(
        &self,
        sql: &str,
        params: &[Param],
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        (**self).execute(sql, params).await
    }

    async fn commit(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        (**self).commit().await
    }

    async fn rollback(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        (**self).rollback().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_accessors() {
        let row = Row::new(vec![
            Value::Int(7),
            Value::Float(1.5),
            Value::Text("x".into()),
        ]);
        assert_eq!(row.get_i64(0), Some(7));
        assert_eq!(row.get_f64(1), Some(1.5));
        assert_eq!(row.get_i64(2), None);
        assert_eq!(row.get(3), None);
    }
}
