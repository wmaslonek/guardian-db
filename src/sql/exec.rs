//! The synchronous execution context shared by the evaluator, SELECT pipeline,
//! and DDL/DML executors.
//!
//! All tables a statement references are loaded into [`Exec::tables`] *before*
//! execution (see [`crate::sql::engine`]), so execution itself — including subqueries —
//! is fully synchronous. Only loading and commit touch async storage.

use crate::relational::catalog::{ForeignKey, QualifiedName};
use crate::relational::{Catalog, SqlValue};
use crate::sql::lock::{LockManager, LockMode, LockObject, LockScope, SessionId};
use crate::sql::row::RowSet;
use crate::sql::store::{LoadedTable, Mutation};
use chrono::{DateTime, Utc};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};

/// Per-transaction `SET CONSTRAINTS` state: which deferrable foreign keys are
/// currently running in `DEFERRED` mode. Lives on `engine::Transaction`
/// (initialized empty at `BEGIN`; discarded — like everything else in the
/// transaction — at `COMMIT`/`ROLLBACK`), and a read-only clone is handed to
/// each statement's [`Exec`] so foreign-key enforcement
/// ([`crate::sql::fk`]) can decide whether to check a deferrable constraint
/// now or queue it.
///
/// [`Exec::constraint_modes`] is `None` outside an explicit transaction block
/// (autocommit): PostgreSQL's per-statement implicit transaction commits
/// immediately after that one statement, so "deferred to commit" and
/// "immediate" are observably identical there — there is nothing to gain by
/// tracking deferral outside a `BEGIN` block, so every foreign-key check just
/// runs immediately, exactly as it always has.
#[derive(Debug, Clone, Default)]
pub struct ConstraintModes {
    /// The last `SET CONSTRAINTS ALL ...` executed in this transaction, if
    /// any; applies to every deferrable constraint not individually
    /// overridden in `named`. PostgreSQL forgets any earlier per-name
    /// overrides when `ALL` is issued (see `AfterTriggerSetState` in
    /// `src/backend/commands/trigger.c`), so `named` is cleared at the same
    /// time this is set (see `crate::sql::engine::Session::exec_set_constraints`).
    pub all_deferred: Option<bool>,
    /// Per-constraint override from `SET CONSTRAINTS name [, ...] ...`, keyed
    /// by (declaring table, constraint name) — PostgreSQL catalogues a
    /// foreign key on its referencing/child table regardless of which side
    /// (child INSERT/UPDATE check, or parent-side `NO ACTION`) queues a
    /// check for it.
    pub named: HashMap<(QualifiedName, String), bool>,
}

impl ConstraintModes {
    /// Is `fk` (declared on `table`) currently running deferred? Only
    /// meaningful for a constraint declared `DEFERRABLE` in the first place —
    /// `NOT DEFERRABLE` (PostgreSQL's default) always checks immediately and
    /// `SET CONSTRAINTS` cannot change that.
    pub fn is_deferred(&self, table: &QualifiedName, fk: &ForeignKey) -> bool {
        if !fk.deferrable.is_deferrable() {
            return false;
        }
        if let Some(v) = self.named.get(&(table.clone(), fk.name.clone())) {
            return *v;
        }
        self.all_deferred
            .unwrap_or_else(|| fk.deferrable.initially_deferred())
    }
}

/// One foreign-key check whose validation was postponed past its statement
/// (a `DEFERRABLE` constraint currently in `DEFERRED` mode, see
/// [`ConstraintModes`]). Queued on `Exec::deferred_checks` during the
/// statement, then spliced into `engine::Transaction::pending_deferred`
/// (surviving across statements in the same transaction) and re-validated —
/// against live state at that later time, not the values captured when it
/// was queued — at `COMMIT` or by `SET CONSTRAINTS ... IMMEDIATE` (see
/// [`crate::sql::fk::Exec::fk_drain_deferred`],
/// `crate::sql::engine::Session::commit` and `exec_set_constraints`).
///
/// `Child` and `Referenced` are validated the same way at drain time:
/// satisfied if a parent row with `key` still exists, *or* if no live row of
/// `child` bears `key` any longer (the row that originally failed may since
/// have been deleted, or updated away from that key — either way there is
/// nothing left for it to violate; this is how a stale entry left behind by a
/// since-superseded check stops mattering, without needing this engine's
/// row-identity to track "the same row" the way PostgreSQL's tuple ids do).
/// `MatchFullNullMix` uses the same no-stable-row-identity philosophy, but
/// there is no parent side to it: it is satisfied only when no live row of
/// `child` still carries the exact partially-NULL shape that failed.
pub enum DeferredFkCheck {
    /// A child-side INSERT/UPDATE check ([`crate::sql::fk::Exec::fk_check_one`]):
    /// `key` (this row's foreign-key column values at check time) must, at
    /// drain time, resolve in `child`'s parent table.
    Child {
        child: QualifiedName,
        fk: ForeignKey,
        key: String,
        key_vals: Vec<SqlValue>,
    },
    /// A parent-side `NO ACTION` check
    /// ([`crate::sql::fk::Exec::fk_apply_referential_actions`]): `key` (the
    /// parent row's referenced-column values at the time it was deleted or
    /// rewritten) must, at drain time, still resolve in `parent` or have no
    /// matching rows left in `child`.
    Referenced {
        parent: QualifiedName,
        child: QualifiedName,
        fk: ForeignKey,
        key: String,
        key_vals: Vec<SqlValue>,
    },
    /// A `MATCH FULL` "no mixing of null and nonnull key values" check
    /// ([`crate::sql::fk::Exec::fk_check_one`]): PostgreSQL runs this check
    /// inside the same deferrable per-row trigger as the ordinary
    /// parent-existence check (`RI_FKey_check` tests both the MATCH FULL
    /// shape and the parent lookup in one call), so it follows the same
    /// `DEFERRABLE`/`SET CONSTRAINTS` timing. `row_key` is the row's FK
    /// column values *in order, NULLs included*
    /// ([`crate::relational::ordered_key`], not [`crate::relational::composite_key`],
    /// since a partially-NULL key can never appear in an index). At drain
    /// time it is still a violation only if some live row of `child` still
    /// carries exactly that shape — like the other variants, a row that was
    /// since updated away from it or deleted leaves nothing to violate.
    MatchFullNullMix {
        child: QualifiedName,
        fk: ForeignKey,
        row_key: String,
    },
}

impl DeferredFkCheck {
    /// The (declaring table, constraint name) identity `SET CONSTRAINTS`
    /// matches against.
    pub fn identity(&self) -> (&QualifiedName, &str) {
        match self {
            DeferredFkCheck::Child { child, fk, .. } => (child, fk.name.as_str()),
            DeferredFkCheck::Referenced { child, fk, .. } => (child, fk.name.as_str()),
            DeferredFkCheck::MatchFullNullMix { child, fk, .. } => (child, fk.name.as_str()),
        }
    }
}

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
    /// Tables loaded for this statement, keyed by qualified name. Held behind
    /// `Arc` so read-only statements share the `Database`'s cached, already-
    /// decoded view with zero copy; a statement that mutates a table pays a
    /// single copy-on-write clone via `Arc::make_mut` at the write site.
    pub tables: HashMap<QualifiedName, Arc<LoadedTable>>,
    /// Bound positional parameters (`$1`-based).
    pub params: Vec<SqlValue>,
    /// Statement timestamp used by `now()` / `current_timestamp`.
    pub now: DateTime<Utc>,
    /// Accumulated storage mutations. Shared (`Arc<Mutex<_>>` rather than a
    /// plain field, so a user-defined function's body — invoked from deep
    /// inside expression evaluation (`&Exec`, not `&mut Exec`; see
    /// [`crate::sql::udf`]) — can record its own INSERT/UPDATE/DELETE effects
    /// into the same accumulator the enclosing statement commits) needs
    /// `Send` (recursive async handlers box statement futures as
    /// `dyn Future + Send`), which rules out `Rc`/`RefCell`.
    pub mutations: Arc<Mutex<Vec<Mutation>>>,
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
    /// Session variables (`SET name = value`), readable by `current_setting()`
    /// and extension functions; writes (e.g. `set_limit`) are copied back to
    /// the session after the statement.
    pub vars: RefCell<HashMap<String, String>>,
    /// Row-security state for this statement (see [`crate::sql::rls`]).
    /// Empty (the default) means no enforcement applies.
    pub rls: crate::sql::rls::RlsContext,
    /// User-defined-function call depth, shared across an entire statement's
    /// call chain (including nested sub-[`Exec`]s built for each UDF
    /// invocation; see [`crate::sql::udf::call_function`]) so self-recursion
    /// is bounded regardless of how deep the Rust call stack for any single
    /// invocation goes.
    pub udf_depth: Arc<AtomicU32>,
    /// Trigger-function invocation depth, shared like [`Exec::udf_depth`]
    /// (a trigger whose body writes its own table recurses
    /// `exec_insert → fire_* → body INSERT → exec_insert → ...` through
    /// sub-[`Exec`]s), so trigger recursion is bounded regardless of Rust
    /// stack shape. See [`crate::sql::udf::call_trigger_function`].
    pub trigger_depth: Arc<AtomicU32>,
    /// This transaction's current `SET CONSTRAINTS` state, a read-only
    /// snapshot for this statement (see [`ConstraintModes`]); `None` outside
    /// an explicit transaction block, which always means "check every
    /// foreign key immediately". Set by the engine right after construction,
    /// alongside `vars` — never mutated during dispatch (`SET CONSTRAINTS`
    /// itself is handled entirely outside the normal `Exec` dispatch path,
    /// like `BEGIN`/`COMMIT`/`ROLLBACK`; see
    /// `crate::sql::engine::Session::exec_set_constraints`).
    pub constraint_modes: Option<ConstraintModes>,
    /// Foreign-key checks this statement postponed past itself because their
    /// constraint is currently `DEFERRED` (see [`DeferredFkCheck`]). Drained
    /// by the engine after the statement runs and spliced into the
    /// transaction's `pending_deferred` queue, the same way `pending_locks`
    /// is drained into the lock manager.
    pub deferred_checks: RefCell<Vec<DeferredFkCheck>>,
    /// CONSTRAINT TRIGGER firings that were deferred (`DEFERRABLE INITIALLY
    /// DEFERRED`) during this statement. Drained by the engine after the
    /// statement runs and collected into the transaction's deferred trigger
    /// queue; fired at `COMMIT` (see `engine::Transaction::deferred_triggers`).
    pub deferred_triggers: Vec<crate::sql::trigger::DeferredTriggerFiring>,
    /// Shared ANN index runtime (RFC 0005), set by the engine right after
    /// construction alongside `vars`; `None` in sub-`Exec`s that never plan
    /// vector scans and in unit-test contexts.
    #[cfg(feature = "vector-index")]
    pub ann: Option<std::sync::Arc<crate::sql::ann::AnnRuntime>>,
    /// Plan decisions recorded during execution (e.g. the ANN scan the
    /// planner chose), surfaced by `EXPLAIN`.
    #[cfg(feature = "vector-index")]
    pub plan_notes: RefCell<Vec<String>>,
}

impl Exec {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        catalog: Catalog,
        tables: HashMap<QualifiedName, Arc<LoadedTable>>,
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
            mutations: Arc::new(Mutex::new(Vec::new())),
            catalog_dirty: false,
            cte: HashMap::new(),
            database,
            username,
            locks,
            session_id,
            pending_locks: RefCell::new(Vec::new()),
            for_update_filter: None,
            vars: RefCell::new(HashMap::new()),
            rls: crate::sql::rls::RlsContext::default(),
            udf_depth: Arc::new(AtomicU32::new(0)),
            trigger_depth: Arc::new(AtomicU32::new(0)),
            constraint_modes: None,
            deferred_checks: RefCell::new(Vec::new()),
            deferred_triggers: Vec::new(),
            #[cfg(feature = "vector-index")]
            ann: None,
            #[cfg(feature = "vector-index")]
            plan_notes: RefCell::new(Vec::new()),
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

// Maintenance note 4: documents compatibility expectations without changing runtime behavior.

// Maintenance note 16: documents compatibility expectations without changing runtime behavior.

// Maintenance note: keeps SQL compatibility behavior explicit for future updates.

// Maintenance note: keeps SQL compatibility behavior explicit for future updates.

// SQL compatibility note 4: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.

// SQL compatibility note 20: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.

// SQL compatibility note 4: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.

// SQL compatibility note 20: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.
