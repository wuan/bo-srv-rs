//! A scriptable [`QueryExecutor`] for tests (no database required).
//!
//! Expectations can be registered by SQL fragment; the first unconsumed
//! expectation whose fragment appears in the incoming SQL matches, in FIFO
//! order, and its rows are returned.  Every call is recorded so tests can
//! assert on the queries that were issued.

use std::sync::Mutex;

use crate::executor::{Param, QueryExecutor, Row};

#[derive(Debug)]
enum Expectation {
    Rows { fragment: String, rows: Vec<Row> },
    Error { fragment: String, message: String },
}

/// Test double for [`QueryExecutor`].
#[derive(Debug, Default)]
pub struct MockExecutor {
    inner: Mutex<MockInner>,
}

#[derive(Debug, Default)]
struct MockInner {
    expectations: Vec<Expectation>,
    calls: Vec<(String, Vec<Param>)>,
}

impl MockExecutor {
    pub fn new() -> Self {
        MockExecutor::default()
    }

    /// Register rows to return for the first query whose SQL contains
    /// `sql_fragment`.
    pub fn add_rows(&mut self, sql_fragment: &str, rows: Vec<Row>) {
        self.inner
            .get_mut()
            .unwrap()
            .expectations
            .push(Expectation::Rows {
                fragment: sql_fragment.to_string(),
                rows,
            });
    }

    /// Register an error to return for the first query whose SQL contains
    /// `sql_fragment`.
    pub fn add_error(&mut self, sql_fragment: &str, message: &str) {
        self.inner
            .get_mut()
            .unwrap()
            .expectations
            .push(Expectation::Error {
                fragment: sql_fragment.to_string(),
                message: message.to_string(),
            });
    }

    /// The queries executed so far (SQL + parameters).
    pub fn calls(&self) -> Vec<(String, Vec<Param>)> {
        self.inner.lock().unwrap().calls.clone()
    }

    pub fn call_count(&self) -> usize {
        self.inner.lock().unwrap().calls.len()
    }
}

impl QueryExecutor for MockExecutor {
    fn query(&self, sql: &str, params: &[Param]) -> Result<Vec<Row>, Box<dyn std::error::Error + Send + Sync>> {
        let mut inner = self.inner.lock().unwrap();
        inner.calls.push((sql.to_string(), params.to_vec()));

        let pos = inner
            .expectations
            .iter()
            .position(|e| match e {
                Expectation::Rows { fragment, .. } => sql.contains(fragment),
                Expectation::Error { fragment, .. } => sql.contains(fragment),
            })
            .expect("MockExecutor: no expectation matched the query");

        match inner.expectations.remove(pos) {
            Expectation::Rows { rows, .. } => Ok(rows),
            Expectation::Error { message, .. } => Err(message.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::Value;

    #[test]
    fn returns_rows_and_records_calls() {
        let mut mock = MockExecutor::new();
        mock.add_rows("SELECT", vec![Row::new(vec![Value::Int(1)])]);
        let rows = mock.query("SELECT 1", &[]).unwrap();
        assert_eq!(rows[0].get_i64(0), Some(1));
        assert_eq!(mock.call_count(), 1);
        assert_eq!(mock.calls()[0].0, "SELECT 1");
    }

    #[test]
    fn returns_error() {
        let mut mock = MockExecutor::new();
        mock.add_error("SELECT", "boom");
        assert!(mock.query("SELECT 1", &[]).is_err());
    }
}