//! Foreign-key referential enforcement.
//!
//! Two `MATCH` modes are implemented, selected per-constraint by
//! [`MatchType`] (`ForeignKey::match_type`):
//!
//! * **`MATCH SIMPLE`** (PostgreSQL's default): a child row satisfies a
//!   foreign key when **any** of its FK columns is NULL; otherwise a parent
//!   row matching *all* referenced columns must exist.
//! * **`MATCH FULL`**: a composite key must be either all-NULL (exempt) or
//!   all-non-NULL and matching a parent row — a row with *some but not all*
//!   FK columns NULL is itself a violation (`23503`, "MATCH FULL does not
//!   allow mixing of null and nonnull key values."), never silently exempted.
//!
//! `MATCH PARTIAL` is rejected at DDL time (`0A000`) — real PostgreSQL has
//! never implemented it either, so this is parity, not a gap (see
//! `crate::sql::ddl::fk_match_type`).
//!
//! Child-side checks run on INSERT and on UPDATEs that change an FK column's
//! value; parent-side referential actions (`NO ACTION` / `RESTRICT` /
//! `CASCADE` / `SET NULL` / `SET DEFAULT`) run when a referenced row is
//! deleted or a referenced key column actually changes value.
//!
//! # Deferred checking
//!
//! A foreign key's [`Deferrable`] declaration (`[NOT] DEFERRABLE [INITIALLY
//! {DEFERRED|IMMEDIATE}]`, parsed at DDL time — see
//! `crate::sql::ddl::fk_deferrable_mode`) controls whether its checks can run
//! at `COMMIT` instead of per-statement:
//!
//! * The child-side check — including `MATCH FULL`'s "no mixing of null and
//!   nonnull key values" shape check, not just the parent-existence lookup —
//!   and the parent-side `NO ACTION` check (including the re-check `SET
//!   DEFAULT` runs after rewriting a child row) all defer when the
//!   constraint's *current* mode — the transaction's [`SET
//!   CONSTRAINTS`][set-constraints] state, [`ConstraintModes`], defaulting to
//!   the constraint's own `INITIALLY {DEFERRED|IMMEDIATE}` — is `DEFERRED`.
//!   A deferred check is queued ([`DeferredFkCheck`]) instead of erroring
//!   immediately, and re-validated — against live state at that later time —
//!   at `COMMIT` or by `SET CONSTRAINTS ... IMMEDIATE`
//!   (`crate::sql::engine::Session::commit` / `exec_set_constraints`). This
//!   matches real PostgreSQL, verified against a live PostgreSQL 16 instance:
//!   `RI_FKey_check` (`src/backend/utils/adt/ri_triggers.c`) tests the
//!   `MATCH FULL` shape and the parent lookup in the same deferrable AFTER
//!   ROW trigger invocation, so both share that trigger's timing — the
//!   underlying reason is architectural (the whole trigger call is what's
//!   deferred), not something specific to what a later statement can or
//!   cannot change about the row.
//! * **`RESTRICT` is never deferred**, regardless of the constraint's
//!   `DEFERRABLE`/`INITIALLY DEFERRED` declaration: verified against
//!   PostgreSQL's own source (`src/backend/utils/adt/ri_triggers.c`), whose
//!   comment states the SQL standard's intent that `RESTRICT` fire exactly
//!   when the update/delete happens, and PostgreSQL's own commit history
//!   describes `NO ACTION` and `RESTRICT` as identical *except* that
//!   `RESTRICT`'s check is not deferrable. So `RESTRICT` always runs
//!   per-statement here too, exactly like today's un-deferred behavior.
//! * Outside an explicit transaction block there is nothing to gain by
//!   tracking deferral: PostgreSQL's per-statement implicit transaction
//!   commits immediately after that one statement, so "deferred to commit"
//!   and "immediate" are observably identical, and every check just runs
//!   immediately (see [`ConstraintModes`]).
//! * A deferred child-side check is keyed by the row's FK value at check
//!   time, not PostgreSQL's per-row tuple identity (this engine has no
//!   stable row id available at that call site) — see [`DeferredFkCheck`]'s
//!   doc comment for exactly what that does and does not cover.
//!
//! PostgreSQL runs referential actions with the table owner's privileges, so
//! the internal child-row reads and writes here deliberately **bypass
//! row-level security**: they never consult the statement's RLS row filters
//! and never run the new-row policy checks. Everything else goes through the
//! normal write path ([`Exec::write_update`], [`Mutation`]s, pending row
//! locks), so an aborted statement or a `ROLLBACK` undoes cascades exactly
//! like direct writes.
//!
//! [`Mutation`]: crate::sql::store::Mutation
//! [`MatchType`]: crate::relational::catalog::MatchType
//! [`Deferrable`]: crate::relational::catalog::Deferrable
//! [set-constraints]: crate::sql::engine::Session::exec_set_constraints

use crate::relational::catalog::{ForeignKey, MatchType, QualifiedName, ReferentialAction, Table};
use crate::relational::{Catalog, SqlValue, composite_key, ordered_key};
use crate::sql::error::{Result, SqlError};
use crate::sql::exec::{DeferredFkCheck, Exec};
use crate::sql::store::RowValues;
use crate::sql::trigger::TriggerOp;
use std::collections::{BTreeSet, VecDeque};

/// Upper bound on processed referential-action work items per statement, a
/// guard against pathological `ON UPDATE CASCADE` cycles that never reach a
/// fixpoint. Delete cascades terminate structurally (a deleted row leaves the
/// loaded table and can never match again), and update cascades stop when
/// values stop changing; the cap only exists so a degenerate constraint graph
/// fails typed instead of spinning.
const MAX_RI_STEPS: usize = 1_000_000;

/// Maximum depth for FK CASCADE DELETE chains. A chain longer than this
/// returns SQLSTATE 54001 (`stack_depth_limit_exceeded`), matching what
/// PostgreSQL returns when a trigger call stack overflows. Test 9 in
/// `sql_triggers_extended` verifies this guard fires at depth 26.
const MAX_CASCADE_DEPTH: u32 = 25;

/// A parent-side referential-action work item: a row of `table` that this
/// statement removed or rewrote.
pub(crate) enum RiWork {
    Deleted {
        table: QualifiedName,
        row: RowValues,
        /// How deep in the `ON DELETE CASCADE` chain this deletion is. The
        /// initial items produced by `exec_delete` start at `1`; each
        /// cascaded child gets `parent_depth + 1`. When this exceeds
        /// [`MAX_CASCADE_DEPTH`] the engine returns SQLSTATE 54001.
        cascade_depth: u32,
    },
    Updated {
        table: QualifiedName,
        old: RowValues,
        new: RowValues,
    },
}

/// A deferred-to-end-of-statement `NO ACTION`/`RESTRICT` verification.
struct PendingCheck {
    parent: QualifiedName,
    child: QualifiedName,
    fk: ForeignKey,
    key: String,
    key_vals: Vec<SqlValue>,
}

/// The tables a DML statement on `root` may touch through foreign keys:
/// `.0` — tables referential actions may *write* (all descendants through
/// referencing constraints), `.1` — tables *read* for existence checks (the
/// parents of `root` and of every writable table). `include_children` is
/// false for plain INSERT, which can only ever read parents.
pub(crate) fn fk_ripple(
    catalog: &Catalog,
    root: &QualifiedName,
    include_children: bool,
) -> (Vec<QualifiedName>, Vec<QualifiedName>) {
    let mut written: BTreeSet<QualifiedName> = BTreeSet::new();
    if include_children {
        let mut queue: VecDeque<QualifiedName> = VecDeque::from([root.clone()]);
        while let Some(q) = queue.pop_front() {
            for (child, _fk) in catalog.referencing_foreign_keys(&q) {
                if child != *root && written.insert(child.clone()) {
                    queue.push_back(child);
                }
            }
        }
    }
    let mut read: BTreeSet<QualifiedName> = BTreeSet::new();
    for q in std::iter::once(root).chain(written.iter()) {
        if let Some(table) = catalog.get_table(q) {
            for fk in &table.foreign_keys {
                if let Some(parent) =
                    catalog.resolve_table_name(Some(&fk.ref_schema), &fk.ref_table)
                {
                    read.insert(parent);
                }
            }
        }
    }
    (written.into_iter().collect(), read.into_iter().collect())
}

/// The child row's FK column values, in constraint order.
fn fk_values(fk: &ForeignKey, row: &RowValues) -> Vec<SqlValue> {
    fk.columns
        .iter()
        .map(|c| row.get(c).cloned().unwrap_or(SqlValue::Null))
        .collect()
}

/// The parent row's referenced column values, in constraint order.
fn ref_values(fk: &ForeignKey, row: &RowValues) -> Vec<SqlValue> {
    fk.ref_columns
        .iter()
        .map(|c| row.get(c).cloned().unwrap_or(SqlValue::Null))
        .collect()
}

fn display_vals(vals: &[SqlValue]) -> String {
    vals.iter()
        .map(|v| v.to_text().unwrap_or_else(|| "null".into()))
        .collect::<Vec<_>>()
        .join(", ")
}

impl Exec {
    /// Child-side enforcement for one row about to be written to `table`:
    /// every foreign key must be satisfied. With `old` given (an UPDATE), FKs
    /// whose column values did not change are skipped, like PostgreSQL's RI
    /// triggers (pre-existing references are not re-validated).
    pub(crate) fn fk_check_child(
        &self,
        table: &Table,
        new: &RowValues,
        old: Option<&RowValues>,
    ) -> Result<()> {
        for fk in &table.foreign_keys {
            if let Some(old) = old
                && ordered_key(&fk_values(fk, old)) == ordered_key(&fk_values(fk, new))
            {
                continue;
            }
            self.fk_check_one(table, fk, new)?;
        }
        Ok(())
    }

    fn fk_check_one(&self, table: &Table, fk: &ForeignKey, row: &RowValues) -> Result<()> {
        let vals = fk_values(fk, row);
        let null_count = vals.iter().filter(|v| v.is_null()).count();
        let table_q = table.qualified();
        match fk.match_type {
            MatchType::Simple => {
                // Any NULL FK column exempts the row from the check.
                if null_count > 0 {
                    return Ok(());
                }
            }
            MatchType::Full => {
                if null_count == vals.len() {
                    // All-NULL: exempt, like every MATCH type.
                    return Ok(());
                }
                if null_count > 0 {
                    // Some but not all NULL: a MATCH FULL violation in its
                    // own right, independent of whether a parent exists.
                    // Subject to the same deferred timing as the ordinary
                    // parent-existence check below: PostgreSQL runs this
                    // check inside the very same (deferrable) AFTER ROW
                    // trigger as the parent-existence check
                    // (`RI_FKey_check` in `src/backend/utils/adt/ri_triggers.c`
                    // tests the MATCH FULL null-mix shape and the parent
                    // lookup in the same function invocation), so a
                    // `DEFERRABLE INITIALLY DEFERRED` constraint defers this
                    // error to `COMMIT`/`SET CONSTRAINTS ... IMMEDIATE`
                    // exactly like it defers the parent-existence error —
                    // verified against a live PostgreSQL 16 instance:
                    // `INSERT` of a partial-NULL `MATCH FULL` row succeeds
                    // immediately under `DEFERRABLE INITIALLY DEFERRED`, and
                    // only `COMMIT` raises "MATCH FULL does not allow mixing
                    // of null and nonnull key values."
                    if self.fk_is_deferred(&table_q, fk) {
                        self.deferred_checks
                            .borrow_mut()
                            .push(DeferredFkCheck::MatchFullNullMix {
                                child: table_q,
                                fk: fk.clone(),
                                row_key: ordered_key(&vals),
                            });
                        return Ok(());
                    }
                    return Err(SqlError::ForeignKeyViolation {
                        table: table.name.clone(),
                        constraint: fk.name.clone(),
                        detail: "MATCH FULL does not allow mixing of null and nonnull key values."
                            .into(),
                    });
                }
            }
        }
        // Every FK column is non-NULL: a real composite key to look up.
        let key = composite_key(&vals).expect("null_count == 0 implies composite_key is Some");
        let parent_q = self.fk_parent(fk)?;
        if self.fk_parent_has_key(&parent_q, fk, &key)? {
            return Ok(());
        }
        if self.fk_is_deferred(&table_q, fk) {
            self.deferred_checks
                .borrow_mut()
                .push(DeferredFkCheck::Child {
                    child: table_q,
                    fk: fk.clone(),
                    key,
                    key_vals: vals,
                });
            return Ok(());
        }
        Err(SqlError::ForeignKeyViolation {
            table: table.name.clone(),
            constraint: fk.name.clone(),
            detail: format!(
                "Key ({})=({}) is not present in table \"{}\".",
                fk.columns.join(", "),
                display_vals(&vals),
                fk.ref_table
            ),
        })
    }

    /// Is `fk` (declared on `table_q`) currently checked in `DEFERRED` mode?
    /// Always `false` outside an explicit transaction block (see
    /// [`crate::sql::exec::ConstraintModes`]).
    fn fk_is_deferred(&self, table_q: &QualifiedName, fk: &ForeignKey) -> bool {
        self.constraint_modes
            .as_ref()
            .map(|cm| cm.is_deferred(table_q, fk))
            .unwrap_or(false)
    }

    /// Resolve the parent table of `fk` (schema-resolved at DDL time). A
    /// dangling reference (only possible in hand-edited catalogs) fails typed.
    fn fk_parent(&self, fk: &ForeignKey) -> Result<QualifiedName> {
        self.catalog
            .resolve_table_name(Some(&fk.ref_schema), &fk.ref_table)
            .ok_or_else(|| SqlError::UndefinedTable(format!("{}.{}", fk.ref_schema, fk.ref_table)))
    }

    /// Does any live parent row carry `key` on the FK's referenced columns?
    /// Uses an exactly-matching index (the referenced PK/unique) when loaded.
    fn fk_parent_has_key(
        &self,
        parent_q: &QualifiedName,
        fk: &ForeignKey,
        key: &str,
    ) -> Result<bool> {
        let loaded = self.fk_loaded(parent_q)?;
        for idx in &loaded.indexes {
            if idx.meta.columns == fk.ref_columns {
                return Ok(!idx.data.get(key).is_empty());
            }
        }
        Ok(loaded
            .rows
            .values()
            .any(|r| composite_key(&ref_values(fk, r)).as_deref() == Some(key)))
    }

    /// Live child rows whose FK columns all equal `key` (non-null equality —
    /// MATCH SIMPLE). Uses an exactly-matching index when one exists.
    fn fk_matching_children(
        &self,
        child_q: &QualifiedName,
        fk: &ForeignKey,
        key: &str,
    ) -> Result<Vec<(String, RowValues)>> {
        let loaded = self.fk_loaded(child_q)?;
        for idx in &loaded.indexes {
            if idx.meta.columns == fk.columns {
                return Ok(idx
                    .data
                    .get(key)
                    .iter()
                    .filter_map(|rid| loaded.rows.get(rid).map(|r| (rid.clone(), r.clone())))
                    .collect());
            }
        }
        Ok(loaded
            .rows
            .iter()
            .filter(|(_, r)| composite_key(&fk_values(fk, r)).as_deref() == Some(key))
            .map(|(rid, r)| (rid.clone(), r.clone()))
            .collect())
    }

    /// Does any live row of `child_q` currently carry exactly `row_key` (the
    /// FK columns' values *in order, NULLs included* — see [`ordered_key`])
    /// on `fk`'s referencing columns? Re-validates a deferred `MATCH FULL`
    /// null-mix check ([`DeferredFkCheck::MatchFullNullMix`]) at drain time.
    /// Unlike [`Exec::fk_matching_children`], this cannot use an index or
    /// [`composite_key`] equality: the values being matched here have
    /// already failed the "all-NULL or all-non-NULL" shape test, and
    /// `composite_key` returns `None` — deliberately excluding the row from
    /// any index — for any row with a NULL component.
    fn fk_child_row_shape_exists(
        &self,
        child_q: &QualifiedName,
        fk: &ForeignKey,
        row_key: &str,
    ) -> Result<bool> {
        let loaded = self.fk_loaded(child_q)?;
        Ok(loaded
            .rows
            .values()
            .any(|r| ordered_key(&fk_values(fk, r)) == row_key))
    }

    fn fk_loaded(&self, q: &QualifiedName) -> Result<&crate::sql::store::LoadedTable> {
        self.tables.get(q).map(|a| a.as_ref()).ok_or_else(|| {
            SqlError::Internal(format!(
                "foreign-key table {} was not preloaded",
                q.to_string_qualified()
            ))
        })
    }

    /// Run parent-side referential actions for `work`, breadth-first;
    /// cascades enqueue further work for the rows they remove or rewrite.
    /// `RESTRICT` violations (never deferred, see the module doc comment) and
    /// non-deferred `NO ACTION`/`SET DEFAULT` re-checks are collected and
    /// verified once every cascading action has applied — the per-statement
    /// check (a child row that the same statement also removes does not
    /// violate). A `NO ACTION`/`SET DEFAULT` check whose constraint is
    /// currently `DEFERRED` is queued instead (`self.deferred_checks`) and
    /// skips this per-statement verification entirely.
    pub(crate) fn fk_apply_referential_actions(&mut self, work: Vec<RiWork>) -> Result<()> {
        let mut queue: VecDeque<RiWork> = work.into();
        let mut checks: Vec<PendingCheck> = Vec::new();
        let mut steps = 0usize;
        while let Some(item) = queue.pop_front() {
            steps += 1;
            if steps > MAX_RI_STEPS {
                return Err(SqlError::Internal(
                    "foreign-key referential actions did not terminate".into(),
                ));
            }
            match item {
                RiWork::Deleted {
                    table,
                    row,
                    cascade_depth,
                } => {
                    if cascade_depth > MAX_CASCADE_DEPTH {
                        return Err(SqlError::StatementTooComplex(
                            "FK CASCADE DELETE depth limit exceeded — too many levels of cascades \
                             (max 25)"
                                .into(),
                        ));
                    }
                    self.ri_on_delete(&table, &row, cascade_depth, &mut queue, &mut checks)?
                }
                RiWork::Updated { table, old, new } => {
                    self.ri_on_update(&table, &old, &new, &mut queue, &mut checks)?
                }
            }
        }
        for c in checks {
            // Satisfied if the key reappeared (or survives on another parent
            // row) or no referencing child row is left.
            if self.fk_parent_has_key(&c.parent, &c.fk, &c.key)? {
                continue;
            }
            if !self
                .fk_matching_children(&c.child, &c.fk, &c.key)?
                .is_empty()
            {
                return Err(self.ri_referenced_error(&c.parent, &c.child, &c.fk, &c.key_vals));
            }
        }
        Ok(())
    }

    /// Re-validate a batch of previously deferred foreign-key checks
    /// ([`DeferredFkCheck`]) against their state *now* — called at `COMMIT`
    /// and by `SET CONSTRAINTS ... IMMEDIATE`
    /// (`crate::sql::engine::Session::check_deferred`), which preloads every
    /// table any of `checks` reference into `self.tables` before calling
    /// this. The first violation still standing aborts the whole batch
    /// (matching PostgreSQL: a still-violated deferred constraint fails the
    /// `COMMIT`/`SET CONSTRAINTS` outright).
    ///
    /// Both variants use the same rule: satisfied if a parent row with `key`
    /// exists now, *or* if no live row of `child` bears `key` any longer
    /// (whatever originally caused this either got deleted or updated away
    /// from that key since — see the module doc comment on why this is the
    /// chosen substitute for PostgreSQL's tuple-identity re-fetch).
    pub(crate) fn fk_drain_deferred(&self, checks: Vec<DeferredFkCheck>) -> Result<()> {
        for check in checks {
            match check {
                DeferredFkCheck::Child {
                    child,
                    fk,
                    key,
                    key_vals,
                } => {
                    let parent_q = self.fk_parent(&fk)?;
                    if self.fk_parent_has_key(&parent_q, &fk, &key)? {
                        continue;
                    }
                    if self.fk_matching_children(&child, &fk, &key)?.is_empty() {
                        continue;
                    }
                    let table_name = self
                        .catalog
                        .get_table(&child)
                        .map(|t| t.name.clone())
                        .unwrap_or_else(|| child.name.clone());
                    return Err(SqlError::ForeignKeyViolation {
                        table: table_name,
                        constraint: fk.name.clone(),
                        detail: format!(
                            "Key ({})=({}) is not present in table \"{}\".",
                            fk.columns.join(", "),
                            display_vals(&key_vals),
                            fk.ref_table
                        ),
                    });
                }
                DeferredFkCheck::Referenced {
                    parent,
                    child,
                    fk,
                    key,
                    key_vals,
                } => {
                    if self.fk_parent_has_key(&parent, &fk, &key)? {
                        continue;
                    }
                    if !self.fk_matching_children(&child, &fk, &key)?.is_empty() {
                        return Err(self.ri_referenced_error(&parent, &child, &fk, &key_vals));
                    }
                }
                DeferredFkCheck::MatchFullNullMix { child, fk, row_key } => {
                    if !self.fk_child_row_shape_exists(&child, &fk, &row_key)? {
                        continue;
                    }
                    let table_name = self
                        .catalog
                        .get_table(&child)
                        .map(|t| t.name.clone())
                        .unwrap_or_else(|| child.name.clone());
                    return Err(SqlError::ForeignKeyViolation {
                        table: table_name,
                        constraint: fk.name.clone(),
                        detail: "MATCH FULL does not allow mixing of null and nonnull key values."
                            .into(),
                    });
                }
            }
        }
        Ok(())
    }

    fn ri_on_delete(
        &mut self,
        parent_q: &QualifiedName,
        row: &RowValues,
        cascade_depth: u32,
        queue: &mut VecDeque<RiWork>,
        checks: &mut Vec<PendingCheck>,
    ) -> Result<()> {
        for (child_q, fk) in self.catalog.referencing_foreign_keys(parent_q) {
            let key_vals = ref_values(&fk, row);
            // A key with a NULL component cannot be referenced (MATCH SIMPLE).
            let Some(key) = composite_key(&key_vals) else {
                continue;
            };
            match fk.on_delete {
                ReferentialAction::NoAction => {
                    self.ri_queue_or_check(parent_q, &child_q, &fk, key, key_vals, checks);
                }
                ReferentialAction::Restrict => {
                    // Never deferred, regardless of DEFERRABLE — see the
                    // module doc comment.
                    checks.push(PendingCheck {
                        parent: parent_q.clone(),
                        child: child_q.clone(),
                        fk: fk.clone(),
                        key,
                        key_vals,
                    });
                }
                ReferentialAction::Cascade => {
                    // Get the child table definition for trigger firing.
                    let child_table = self.catalog.get_table(&child_q).cloned();
                    let has_triggers = child_table
                        .as_ref()
                        .is_some_and(|t| t.triggers.iter().any(|trg| trg.enabled));
                    let children = self.fk_matching_children(&child_q, &fk, &key)?;
                    for (rid, child_row) in children {
                        // Fire BEFORE DELETE row triggers if the child table has any.
                        if has_triggers && let Some(tbl) = &child_table {
                            let result = self.fire_before_row(
                                tbl,
                                TriggerOp::Delete,
                                Some(&child_row),
                                None,
                                None,
                            )?;
                            if result.is_none() {
                                // BEFORE trigger suppressed this cascade-delete.
                                continue;
                            }
                        }
                        self.ri_delete_child(&child_q, &rid)?;
                        queue.push_back(RiWork::Deleted {
                            table: child_q.clone(),
                            row: child_row.clone(),
                            cascade_depth: cascade_depth + 1,
                        });
                        // Fire AFTER DELETE row triggers if the child table has any.
                        if has_triggers && let Some(tbl) = &child_table {
                            self.fire_after_row(
                                tbl,
                                TriggerOp::Delete,
                                Some(&child_row),
                                None,
                                None,
                            )?;
                        }
                    }
                }
                ReferentialAction::SetNull => {
                    for (rid, child_row) in self.fk_matching_children(&child_q, &fk, &key)? {
                        let mut new = child_row.clone();
                        for c in &fk.columns {
                            new.insert(c.clone(), SqlValue::Null);
                        }
                        self.ri_update_child(&child_q, &rid, child_row, new, queue)?;
                    }
                }
                ReferentialAction::SetDefault => {
                    for (rid, child_row) in self.fk_matching_children(&child_q, &fk, &key)? {
                        let new = self.ri_defaults_for(&child_q, &fk, &child_row)?;
                        self.ri_update_child(&child_q, &rid, child_row, new, queue)?;
                    }
                    // PostgreSQL re-runs the NO ACTION check after SET
                    // DEFAULT: when the default happens to equal the removed
                    // key, the update above is a no-op and would not
                    // re-validate, yet the reference is now dangling. Subject
                    // to the same deferred timing as a plain NO ACTION check.
                    self.ri_queue_or_check(parent_q, &child_q, &fk, key, key_vals, checks);
                }
            }
        }
        Ok(())
    }

    /// Route a parent-side `NO ACTION`/`SET DEFAULT` re-check: queued for
    /// `COMMIT` (or `SET CONSTRAINTS ... IMMEDIATE`, see
    /// [`Exec::fk_drain_deferred`]) when `fk` is currently `DEFERRED`,
    /// otherwise verified at the end of this statement like today (`checks`,
    /// see [`Exec::fk_apply_referential_actions`]). `RESTRICT` never calls
    /// this — it always goes straight to `checks` (see the module doc
    /// comment on why `RESTRICT` is never deferred).
    fn ri_queue_or_check(
        &self,
        parent_q: &QualifiedName,
        child_q: &QualifiedName,
        fk: &ForeignKey,
        key: String,
        key_vals: Vec<SqlValue>,
        checks: &mut Vec<PendingCheck>,
    ) {
        if self.fk_is_deferred(child_q, fk) {
            self.deferred_checks
                .borrow_mut()
                .push(DeferredFkCheck::Referenced {
                    parent: parent_q.clone(),
                    child: child_q.clone(),
                    fk: fk.clone(),
                    key,
                    key_vals,
                });
        } else {
            checks.push(PendingCheck {
                parent: parent_q.clone(),
                child: child_q.clone(),
                fk: fk.clone(),
                key,
                key_vals,
            });
        }
    }

    fn ri_on_update(
        &mut self,
        parent_q: &QualifiedName,
        old: &RowValues,
        new: &RowValues,
        queue: &mut VecDeque<RiWork>,
        checks: &mut Vec<PendingCheck>,
    ) -> Result<()> {
        for (child_q, fk) in self.catalog.referencing_foreign_keys(parent_q) {
            let old_vals = ref_values(&fk, old);
            let Some(key) = composite_key(&old_vals) else {
                continue;
            };
            let new_vals = ref_values(&fk, new);
            // Actions fire only when a referenced column actually changed.
            if ordered_key(&old_vals) == ordered_key(&new_vals) {
                continue;
            }
            match fk.on_update {
                ReferentialAction::NoAction => {
                    self.ri_queue_or_check(parent_q, &child_q, &fk, key, old_vals, checks);
                }
                ReferentialAction::Restrict => {
                    // Never deferred, regardless of DEFERRABLE — see the
                    // module doc comment.
                    checks.push(PendingCheck {
                        parent: parent_q.clone(),
                        child: child_q.clone(),
                        fk: fk.clone(),
                        key,
                        key_vals: old_vals,
                    });
                }
                ReferentialAction::Cascade => {
                    let child_meta = self.fk_loaded(&child_q)?.meta.clone();
                    for (rid, child_row) in self.fk_matching_children(&child_q, &fk, &key)? {
                        let mut newc = child_row.clone();
                        for (i, c) in fk.columns.iter().enumerate() {
                            let v = new_vals.get(i).cloned().unwrap_or(SqlValue::Null);
                            let v = if v.is_null() {
                                SqlValue::Null
                            } else {
                                crate::sql::dml::coerce_to_col(v, &child_meta, c)?
                            };
                            newc.insert(c.clone(), v);
                        }
                        self.ri_update_child(&child_q, &rid, child_row, newc, queue)?;
                    }
                }
                ReferentialAction::SetNull => {
                    for (rid, child_row) in self.fk_matching_children(&child_q, &fk, &key)? {
                        let mut newc = child_row.clone();
                        for c in &fk.columns {
                            newc.insert(c.clone(), SqlValue::Null);
                        }
                        self.ri_update_child(&child_q, &rid, child_row, newc, queue)?;
                    }
                }
                ReferentialAction::SetDefault => {
                    for (rid, child_row) in self.fk_matching_children(&child_q, &fk, &key)? {
                        let newc = self.ri_defaults_for(&child_q, &fk, &child_row)?;
                        self.ri_update_child(&child_q, &rid, child_row, newc, queue)?;
                    }
                    // Same post-SET DEFAULT re-check as the delete path.
                    self.ri_queue_or_check(parent_q, &child_q, &fk, key, old_vals, checks);
                }
            }
        }
        Ok(())
    }

    fn ri_referenced_error(
        &self,
        parent_q: &QualifiedName,
        child_q: &QualifiedName,
        fk: &ForeignKey,
        key_vals: &[SqlValue],
    ) -> SqlError {
        SqlError::ForeignKeyViolationReferenced {
            table: parent_q.name.clone(),
            constraint: fk.name.clone(),
            referencing: child_q.name.clone(),
            detail: format!(
                "Key ({})=({}) is still referenced from table \"{}\".",
                fk.ref_columns.join(", "),
                display_vals(key_vals),
                child_q.name
            ),
        }
    }

    /// Delete one child row through the normal write path (row lock, index
    /// maintenance, storage mutation). Deliberately not filtered by RLS.
    fn ri_delete_child(&mut self, child_q: &QualifiedName, rid: &str) -> Result<()> {
        let (collection, oid) = {
            let loaded = self.fk_loaded(child_q)?;
            (loaded.meta.storage_collection.clone(), loaded.meta.oid)
        };
        self.record_pending(
            crate::sql::lock::LockObject::Row(oid, rid.to_string()),
            crate::sql::lock::LockMode::ForUpdate,
            crate::sql::lock::LockScope::Transaction,
        );
        let loaded = std::sync::Arc::make_mut(self.tables.get_mut(child_q).unwrap());
        loaded.apply_delete(rid);
        self.mutations
            .lock()
            .unwrap()
            .push(crate::sql::store::Mutation::Delete {
                collection,
                row_id: rid.to_string(),
            });
        Ok(())
    }

    /// Apply an internal child-row update produced by a referential action:
    /// validates NOT NULL / CHECK / UNIQUE and the child's *own* foreign keys
    /// on the changed columns (this is how `SET DEFAULT` re-checks against the
    /// remaining parents — defaults that reference nothing fail 23503, all-NULL
    /// defaults pass), then writes through the normal update path and enqueues
    /// follow-up work. Deliberately not subject to RLS policy checks.
    fn ri_update_child(
        &mut self,
        child_q: &QualifiedName,
        rid: &str,
        old: RowValues,
        new: RowValues,
        queue: &mut VecDeque<RiWork>,
    ) -> Result<()> {
        let table = self.fk_loaded(child_q)?.meta.clone();
        // Fixpoint guard: an action that changes nothing produces no write and
        // no further work (this is what terminates ON UPDATE CASCADE chains).
        let all_cols: Vec<String> = table.columns.iter().map(|c| c.name.clone()).collect();
        let row_key = |r: &RowValues| {
            ordered_key(
                &all_cols
                    .iter()
                    .map(|c| r.get(c).cloned().unwrap_or(SqlValue::Null))
                    .collect::<Vec<_>>(),
            )
        };
        if row_key(&old) == row_key(&new) {
            return Ok(());
        }
        for c in &table.columns {
            if !c.nullable && new.get(&c.name).map(SqlValue::is_null).unwrap_or(true) {
                return Err(SqlError::NotNullViolation {
                    column: c.name.clone(),
                    table: table.name.clone(),
                });
            }
        }
        self.check_constraints(&table, &new)?;
        self.check_unique_for(child_q, &new, Some(rid))?;
        self.fk_check_child(&table, &new, Some(&old))?;
        self.record_pending(
            crate::sql::lock::LockObject::Row(table.oid, rid.to_string()),
            crate::sql::lock::LockMode::ForUpdate,
            crate::sql::lock::LockScope::Transaction,
        );
        let collection = table.storage_collection.clone();
        self.write_update(child_q, &collection, &table, rid, new.clone())?;
        queue.push_back(RiWork::Updated {
            table: child_q.clone(),
            old,
            new,
        });
        Ok(())
    }

    /// The child row with the FK columns replaced by their column defaults
    /// (NULL when a column has no default), mirroring INSERT's default
    /// evaluation, serial sequences included.
    fn ri_defaults_for(
        &mut self,
        child_q: &QualifiedName,
        fk: &ForeignKey,
        row: &RowValues,
    ) -> Result<RowValues> {
        let table = self.fk_loaded(child_q)?.meta.clone();
        let mut new = row.clone();
        for cname in &fk.columns {
            let col = table
                .column(cname)
                .ok_or_else(|| SqlError::UndefinedColumn(cname.clone()))?;
            let value = if let Some(seq) = &col.identity_sequence {
                let n = self.catalog.next_sequence_value(&table.schema, seq)?;
                self.catalog_dirty = true;
                crate::sql::dml::coerce_to_col(SqlValue::Int8(n), &table, cname)?
            } else if let Some(def) = &col.default {
                let v = self.eval_default(def)?;
                if v.is_null() {
                    SqlValue::Null
                } else {
                    crate::sql::dml::coerce_to_col(v, &table, cname)?
                }
            } else {
                SqlValue::Null
            };
            new.insert(cname.clone(), value);
        }
        Ok(new)
    }
}
