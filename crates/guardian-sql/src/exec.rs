//! The synchronous execution context shared by the evaluator, SELECT pipeline,
//! and DDL/DML executors.
//!
//! All tables a statement references are loaded into [`Exec::tables`] *before*
//! execution (see [`crate::engine`]), so execution itself — including subqueries —
//! is fully synchronous. Only loading and commit touch async storage.

use crate::row::RowSet;
use crate::store::{LoadedTable, Mutation};
use chrono::{DateTime, Utc};
use guardian_relational::catalog::QualifiedName;
use guardian_relational::{Catalog, SqlValue};
use std::collections::HashMap;

/// A single name-resolution frame (an intermediate row + its schema).
pub struct Frame<'a> {
    pub schema: &'a crate::row::RowSchema,
    pub row: &'a crate::row::Tuple,
}

/// Per-statement execution context.
pub struct Exec {
    /// Working copy of the catalog (mutated by DDL; flushed on commit if dirty).
    pub catalog: Catalog,
    /// Tables loaded for this statement, keyed by qualified name.
    pub tables: HashMap<QualifiedName, LoadedTable>,
    /// Bound positional parameters (`$1`-based).
    pub params: Vec<SqlValue>,
    /// Statement timestamp used by `now()` / `current_timestamp`.
    pub now: DateTime<Utc>,
    /// Accumulated storage mutations.
    pub mutations: Vec<Mutation>,
    /// Set when DDL changes the catalog.
    pub catalog_dirty: bool,
    /// CTE results in scope for the current query.
    pub cte: HashMap<String, RowSet>,
    /// The session's current database name (for current_database()).
    pub database: String,
    /// Whether the connected role is a superuser (affects some catalog columns).
    pub username: String,
}

impl Exec {
    pub fn new(
        catalog: Catalog,
        tables: HashMap<QualifiedName, LoadedTable>,
        params: Vec<SqlValue>,
        now: DateTime<Utc>,
        database: String,
        username: String,
    ) -> Self {
        Self {
            catalog,
            tables,
            params,
            now,
            mutations: Vec::new(),
            catalog_dirty: false,
            cte: HashMap::new(),
            database,
            username,
        }
    }

    /// Look up a bound parameter by its 1-based index from a `$n` placeholder.
    pub fn param(&self, placeholder: &str) -> crate::error::Result<SqlValue> {
        let idx = placeholder
            .trim_start_matches('$')
            .parse::<usize>()
            .map_err(|_| {
                crate::error::SqlError::Internal(format!("invalid placeholder {placeholder}"))
            })?;
        self.params
            .get(idx.wrapping_sub(1))
            .cloned()
            .ok_or_else(|| {
                crate::error::SqlError::InvalidParameter(format!(
                    "there is no parameter {placeholder}"
                ))
            })
    }
}
