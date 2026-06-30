//! The synchronous execution context shared by the evaluator, SELECT pipeline,
//! and DDL/DML executors.
//!
//! All tables a statement references are loaded into [`Exec::tables`] *before*
//! execution (see [`crate::sql::engine`]), so execution itself — including subqueries —
//! is fully synchronous. Only loading and commit touch async storage.

use crate::sql::lock::{LockManager, LockMode, LockObject, LockScope, SessionId};
use crate::sql::row::RowSet;
use crate::sql::store::{LoadedTable, Mutation};
use chrono::{DateTime, Utc};
use crate::relational::catalog::QualifiedName;
use crate::relational::{Catalog, SqlValue};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

/// A single name-resolution frame (an intermediate row + its schema).
pub struct Frame<'a> {
    pub schema: &'a crate::sql::row::RowSchema,
    pub row: &'a crate::sql::row::Tuple,
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
    /// Shared lock manager.
    pub locks: Arc<LockManager>,
    /// This connection's lock-holder id.
    pub session_id: SessionId,
    /// Blocking locks collected during synchronous execution, acquired by the
    /// engine after the statement runs (row locks, blocking advisory locks).
    pub pending_locks: RefCell<Vec<(LockObject, LockMode, LockScope)>>,
    /// For `SELECT ... FOR UPDATE SKIP LOCKED`: restricts a table's scan to the
    /// rows that were lockable.
    pub for_update_filter: Option<(QualifiedName, BTreeSet<String>)>,
}

impl Exec {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        catalog: Catalog,
        tables: HashMap<QualifiedName, LoadedTable>,
        params: Vec<SqlValue>,
        now: DateTime<Utc>,
        database: String,
        username: String,
        locks: Arc<LockManager>,
        session_id: SessionId,
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
            locks,
            session_id,
            pending_locks: RefCell::new(Vec::new()),
            for_update_filter: None,
        }
    }

    /// Queue a blocking lock to be acquired after the statement executes.
    pub fn record_pending(&self, object: LockObject, mode: LockMode, scope: LockScope) {
        self.pending_locks.borrow_mut().push((object, mode, scope));
    }

    /// Non-blocking lock acquire (for NOWAIT / SKIP LOCKED / try-advisory).
    pub fn try_lock(&self, object: LockObject, mode: LockMode, scope: LockScope) -> bool {
        self.locks.try_acquire(self.session_id, object, mode, scope)
    }

    /// Release one held lock (for advisory unlock).
    pub fn unlock_one(&self, object: LockObject, mode: LockMode) -> bool {
        self.locks.release_one(self.session_id, &object, mode)
    }

    /// Look up a bound parameter by its 1-based index from a `$n` placeholder.
    pub fn param(&self, placeholder: &str) -> crate::sql::error::Result<SqlValue> {
        let idx = placeholder
            .trim_start_matches('$')
            .parse::<usize>()
            .map_err(|_| {
                crate::sql::error::SqlError::Internal(format!("invalid placeholder {placeholder}"))
            })?;
        self.params
            .get(idx.wrapping_sub(1))
            .cloned()
            .ok_or_else(|| {
                crate::sql::error::SqlError::InvalidParameter(format!(
                    "there is no parameter {placeholder}"
                ))
            })
    }
}
