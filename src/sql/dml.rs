//! DML execution: INSERT / UPDATE / DELETE with RETURNING and ON CONFLICT.

use crate::relational::catalog::{
    QualifiedName, Table, TriggerEventDef, TriggerLevel, TriggerTiming,
};
use crate::relational::{SqlType, SqlValue, composite_key, ordered_key};
use crate::sql::error::{Result, SqlError};
use crate::sql::exec::{Exec, Frame};
use crate::sql::names::{ident_name, object_name_parts, split_schema_table};
use crate::sql::result::{ExecResult, OutField};
use crate::sql::row::{FieldRef, RowSchema, Tuple};
use crate::sql::store::{Mutation, RowValues, derive_row_id, encode_row, index_values};
use crate::sql::trigger::TriggerOp;
use sqlparser::ast::{
    AssignmentTarget, Delete, FromTable, Insert, OnConflictAction, OnInsert, SelectItem, Statement,
    TableFactor,
};
use std::collections::{BTreeMap, BTreeSet};

/// An AFTER ROW event queued during a statement's write phase, fired — in
/// row order — after the statement's own writes and referential actions have
/// been applied (PostgreSQL's queued-events ordering, approximated).
enum AfterRowEvent {
    Insert(RowValues),
    Update(RowValues, RowValues),
}

impl Exec {
    // ---- INSERT --------------------------------------------------------

    pub fn exec_insert(&mut self, insert: &Insert) -> Result<ExecResult> {
        let table_name = match &insert.table {
            sqlparser::ast::TableObject::TableName(name) => name.clone(),
            other => {
                return Err(SqlError::FeatureNotSupported(format!(
                    "INSERT target not supported: {other}"
                )));
            }
        };
        let (schema, n) = split_schema_table(&table_name);
        let q = self
            .catalog
            .resolve_table_name(schema.as_deref(), &n)
            .ok_or_else(|| SqlError::UndefinedTable(n.clone()))?;
        if self.catalog.get_view(&q).is_some() {
            return self.exec_insert_on_view(&q, insert);
        }
        let table = self.catalog.require_table(&q)?.clone();

        // Triggers. `UPDATE OF` matching for the ON CONFLICT DO UPDATE path
        // uses the DO UPDATE SET list's target columns.
        let has_triggers = !table.triggers.is_empty();
        let is_do_update = matches!(
            &insert.on,
            Some(OnInsert::OnConflict(oc)) if matches!(oc.action, OnConflictAction::DoUpdate(_))
        );
        let conflict_set_cols: Option<BTreeSet<String>> = match &insert.on {
            Some(OnInsert::OnConflict(oc)) if has_triggers => match &oc.action {
                OnConflictAction::DoUpdate(du) => {
                    let mut cols = BTreeSet::new();
                    for a in &du.assignments {
                        cols.insert(assignment_column(&a.target)?);
                    }
                    Some(cols)
                }
                _ => None,
            },
            _ => None,
        };
        if has_triggers {
            // BEFORE STATEMENT triggers fire exactly once, before the source
            // rows are even evaluated (so they fire for zero-row sources
            // too), and their writes are visible to the statement.
            // PostgreSQL fires UPDATE statement triggers as well when the
            // statement carries ON CONFLICT DO UPDATE.
            self.fire_statement_triggers(&table, TriggerTiming::Before, TriggerOp::Insert, None)?;
            if is_do_update {
                self.fire_statement_triggers(
                    &table,
                    TriggerTiming::Before,
                    TriggerOp::Update,
                    conflict_set_cols.as_ref(),
                )?;
            }
        }

        // Target columns.
        let target_cols: Vec<String> = if insert.columns.is_empty() {
            table.columns.iter().map(|c| c.name.clone()).collect()
        } else {
            insert
                .columns
                .iter()
                .map(|on| {
                    on.0.last()
                        .and_then(|p| p.as_ident())
                        .map(ident_name)
                        .unwrap_or_default()
                })
                .collect()
        };

        // Build the per-row provided column maps. Handles VALUES (including the
        // `DEFAULT` keyword per cell), `DEFAULT VALUES` (no source), and SELECT
        // sources.
        let provided_rows: Vec<BTreeMap<String, SqlValue>> = match &insert.source {
            None => vec![BTreeMap::new()],
            Some(src) => match src.body.as_ref() {
                sqlparser::ast::SetExpr::Values(values) => {
                    let mut out = Vec::with_capacity(values.rows.len());
                    for row in &values.rows {
                        let content = &row.content;
                        if content.len() != target_cols.len() {
                            return Err(SqlError::Syntax(format!(
                                "INSERT has {} target columns but {} values",
                                target_cols.len(),
                                content.len()
                            )));
                        }
                        let mut map = BTreeMap::new();
                        for (col, expr) in target_cols.iter().zip(content) {
                            if is_default_expr(expr) {
                                continue; // leave absent so the column default applies
                            }
                            map.insert(col.clone(), self.eval(expr, &[])?);
                        }
                        out.push(map);
                    }
                    out
                }
                _ => {
                    let rs = self.exec_select_query(src, &[])?;
                    let mut out = Vec::with_capacity(rs.rows.len());
                    for row in rs.rows {
                        if row.len() != target_cols.len() {
                            return Err(SqlError::Syntax(format!(
                                "INSERT has {} target columns but {} values",
                                target_cols.len(),
                                row.len()
                            )));
                        }
                        out.push(target_cols.iter().cloned().zip(row).collect());
                    }
                    out
                }
            },
        };

        let mut prepared: Vec<(String, RowValues)> = Vec::new();
        for provided in provided_rows {
            // PostgreSQL's ordering: defaults/serials apply *before* BEFORE
            // ROW triggers; NOT NULL/CHECK run *after* them, on the
            // trigger-final values; and the row id derives last (a trigger
            // may rewrite primary-key columns).
            let values = self.build_row(&table, provided)?;
            let values = if has_triggers {
                match self.fire_before_row(&table, TriggerOp::Insert, None, Some(values), None)? {
                    Some(v) => v,
                    // Suppressed (`RETURN NULL`): not inserted, not counted,
                    // not in RETURNING.
                    None => continue,
                }
            } else {
                values
            };
            self.check_row_constraints(&table, &values)?;
            let row_id =
                derive_row_id(&table, &values).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            prepared.push((row_id, values));
        }

        // Conflict handling target columns.
        let conflict_target: Option<Vec<String>> = match &insert.on {
            Some(OnInsert::OnConflict(oc)) => match &oc.conflict_target {
                Some(sqlparser::ast::ConflictTarget::Columns(cols)) => {
                    Some(cols.iter().map(ident_name).collect())
                }
                _ => None,
            },
            _ => None,
        };

        let collection = table.storage_collection.clone();
        // Parent-side referential actions apply when an ON CONFLICT DO UPDATE
        // rewrites a row of a table that other tables reference.
        let table_is_referenced = !self.catalog.referencing_foreign_keys(&q).is_empty();
        let mut inserted_rows: Vec<RowValues> = Vec::new();
        let mut after_rows: Vec<AfterRowEvent> = Vec::new();
        let mut count = 0usize;

        for (mut row_id, values) in prepared {
            // Row security: every row proposed for insertion must pass the
            // INSERT policies' WITH CHECK (also under ON CONFLICT, like
            // PostgreSQL).
            self.rls_check_new_row(&q, &table, &values, crate::relational::PolicyCmd::Insert)?;
            // Detect conflict.
            let conflict = self.find_conflict(&q, &values, conflict_target.as_deref());
            if let Some(existing_id) = conflict {
                match &insert.on {
                    Some(OnInsert::OnConflict(oc)) => match &oc.action {
                        OnConflictAction::DoNothing => continue,
                        OnConflictAction::DoUpdate(do_update) => {
                            // The conflicting row must be updatable under the
                            // UPDATE policies' USING expressions.
                            let existing = self
                                .tables
                                .get(&q)
                                .and_then(|l| l.rows.get(&existing_id))
                                .cloned()
                                .unwrap_or_default();
                            if !self.rls_old_row_visible(
                                &q,
                                &table,
                                &existing,
                                crate::relational::PolicyCmd::Update,
                            )? {
                                return Err(SqlError::InsufficientPrivilege(format!(
                                    "new row violates row-level security policy \
                                     (USING expression) for table \"{}\"",
                                    table.name
                                )));
                            }
                            let new_vals = self.apply_conflict_update(
                                &table,
                                &existing_id,
                                &values,
                                &do_update.assignments,
                                do_update.selection.as_ref(),
                            )?;
                            let Some(new_vals) = new_vals else { continue };
                            // The conflict update is an UPDATE for trigger
                            // purposes: BEFORE UPDATE row triggers fire on
                            // (existing → new), suppression skips the update
                            // (PostgreSQL), and constraints re-run on the
                            // trigger-final values.
                            let new_vals = if has_triggers {
                                match self.fire_before_row(
                                    &table,
                                    TriggerOp::Update,
                                    Some(&existing),
                                    Some(new_vals),
                                    conflict_set_cols.as_ref(),
                                )? {
                                    Some(v) => v,
                                    None => continue,
                                }
                            } else {
                                new_vals
                            };
                            if has_triggers {
                                self.check_row_constraints(&table, &new_vals)?;
                            }
                            self.rls_check_new_row(
                                &q,
                                &table,
                                &new_vals,
                                crate::relational::PolicyCmd::Update,
                            )?;
                            // The conflict update is an UPDATE for foreign-key
                            // purposes: re-check FKs whose columns changed and
                            // run parent-side actions if referenced columns
                            // were rewritten.
                            self.fk_check_child(&table, &new_vals, Some(&existing))?;
                            self.write_update(
                                &q,
                                &collection,
                                &table,
                                &existing_id,
                                new_vals.clone(),
                            )?;
                            if table_is_referenced {
                                self.fk_apply_referential_actions(vec![
                                    crate::sql::fk::RiWork::Updated {
                                        table: q.clone(),
                                        old: existing.clone(),
                                        new: new_vals.clone(),
                                    },
                                ])?;
                            }
                            if has_triggers {
                                after_rows.push(AfterRowEvent::Update(existing, new_vals.clone()));
                            }
                            inserted_rows.push(new_vals);
                            count += 1;
                            continue;
                        }
                    },
                    _ => {
                        // No ON CONFLICT clause: a real unique violation.
                        self.check_unique_for(&q, &values, None)?;
                    }
                }
            }
            // Fresh insert: enforce uniqueness and foreign keys, then write.
            self.check_unique_for(&q, &values, None)?;
            self.fk_check_child(&table, &values, None)?;
            self.observe_serials(&table, &values);
            let loaded = std::sync::Arc::make_mut(self.tables.get_mut(&q).unwrap());
            let version = loaded.version_of(&row_id);
            loaded.apply_insert(row_id.clone(), values.clone());
            let doc = encode_row(&table, &row_id, &values, version);
            self.mutations.lock().unwrap().push(Mutation::Put {
                collection: collection.clone(),
                row_id: std::mem::take(&mut row_id),
                doc,
            });
            if has_triggers {
                after_rows.push(AfterRowEvent::Insert(values.clone()));
            }
            inserted_rows.push(values);
            count += 1;
        }

        if has_triggers {
            // AFTER ROW events fire once the statement's full effect —
            // including referential actions — has been applied, in row
            // order; AFTER STATEMENT triggers fire last (UPDATE before
            // INSERT for upserts, mirroring PostgreSQL), even when zero
            // rows were affected.
            for event in &after_rows {
                match event {
                    AfterRowEvent::Insert(values) => {
                        self.fire_after_row(&table, TriggerOp::Insert, None, Some(values), None)?;
                    }
                    AfterRowEvent::Update(old, new) => {
                        self.fire_after_row(
                            &table,
                            TriggerOp::Update,
                            Some(old),
                            Some(new),
                            conflict_set_cols.as_ref(),
                        )?;
                    }
                }
            }
            if is_do_update {
                self.fire_statement_triggers(
                    &table,
                    TriggerTiming::After,
                    TriggerOp::Update,
                    conflict_set_cols.as_ref(),
                )?;
            }
            self.fire_statement_triggers_ex(
                &table,
                TriggerTiming::After,
                TriggerOp::Insert,
                None,
                &inserted_rows,
                &[],
            )?;
        }

        if let Some(returning) = &insert.returning {
            return self.returning_result(&table, inserted_rows, returning);
        }
        Ok(ExecResult::empty_command(format!("INSERT 0 {count}")))
    }

    /// Build a complete row from provided values, applying defaults, serial
    /// sequences, and coercion — everything PostgreSQL applies *before*
    /// BEFORE ROW triggers run. NOT NULL / CHECK enforcement runs afterwards,
    /// on the trigger-final values (see [`Exec::check_row_constraints`]).
    fn build_row(
        &mut self,
        table: &Table,
        provided: BTreeMap<String, SqlValue>,
    ) -> Result<RowValues> {
        let mut values = BTreeMap::new();
        for col in &table.columns {
            let value = if let Some(p) = provided.get(&col.name) {
                if p.is_null() {
                    SqlValue::Null
                } else {
                    coerce_to(p.clone(), &col.ty, &col.name)?
                }
            } else if let Some(seq) = &col.identity_sequence {
                let n = self.catalog.next_sequence_value(&table.schema, seq)?;
                self.catalog_dirty = true;
                coerce_to(SqlValue::Int8(n), &col.ty, &col.name)?
            } else if let Some(def) = &col.default {
                let v = self.eval_default(def)?;
                if v.is_null() {
                    SqlValue::Null
                } else {
                    coerce_to(v, &col.ty, &col.name)?
                }
            } else {
                SqlValue::Null
            };
            values.insert(col.name.clone(), value);
        }
        Ok(values)
    }

    /// NOT NULL + CHECK enforcement for a fully-built row (after any BEFORE
    /// ROW triggers have run, matching PostgreSQL's ordering).
    pub(crate) fn check_row_constraints(&self, table: &Table, values: &RowValues) -> Result<()> {
        for col in &table.columns {
            if !col.nullable && values.get(&col.name).map(SqlValue::is_null).unwrap_or(true) {
                return Err(SqlError::NotNullViolation {
                    column: col.name.clone(),
                    table: table.name.clone(),
                });
            }
        }
        self.check_constraints(table, values)
    }

    pub(crate) fn eval_default(&self, default_sql: &str) -> Result<SqlValue> {
        let stmts = crate::sql::parser::parse_sql(&format!("SELECT {default_sql}"))?;
        if let Some(Statement::Query(q)) = stmts.into_iter().next()
            && let sqlparser::ast::SetExpr::Select(select) = q.body.as_ref()
            && let Some(SelectItem::UnnamedExpr(e))
            | Some(SelectItem::ExprWithAlias { expr: e, .. }) = select.projection.first()
        {
            return self.eval(e, &[]);
        }
        Ok(SqlValue::Null)
    }

    fn observe_serials(&mut self, table: &Table, values: &RowValues) {
        for col in &table.columns {
            if let Some(seq) = &col.identity_sequence
                && let Some(v) = values.get(&col.name).and_then(SqlValue::as_i64)
            {
                self.catalog.observe_sequence_value(&table.schema, seq, v);
            }
        }
    }

    pub(crate) fn check_constraints(&self, table: &Table, values: &RowValues) -> Result<()> {
        for check in &table.checks {
            let stmts = crate::sql::parser::parse_sql(&format!("SELECT {}", check.expr))?;
            if let Some(Statement::Query(q)) = stmts.into_iter().next()
                && let sqlparser::ast::SetExpr::Select(select) = q.body.as_ref()
                && let Some(item) = select.projection.first()
            {
                let expr = match item {
                    SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => e,
                    _ => continue,
                };
                let schema = table_schema(table, &table.name);
                let tuple = row_tuple(table, values);
                let frame = Frame {
                    schema: &schema,
                    row: &tuple,
                };
                // CHECK passes unless it evaluates to FALSE (NULL passes).
                if self.eval(expr, &[frame])?.truthy() == Some(false) {
                    return Err(SqlError::CheckViolation {
                        table: table.name.clone(),
                        constraint: check.name.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Find an existing row that conflicts with `values` on the conflict target
    /// (or any unique index when no target is given).
    fn find_conflict(
        &self,
        q: &QualifiedName,
        values: &RowValues,
        target: Option<&[String]>,
    ) -> Option<String> {
        let loaded = self.tables.get(q)?;
        for idx in &loaded.indexes {
            if !idx.meta.unique {
                continue;
            }
            if let Some(t) = target
                && idx.meta.columns != t
            {
                continue;
            }
            let key_vals = index_values(&idx.meta, values);
            if composite_key(&key_vals).is_none() {
                continue;
            }
            let key = ordered_key(&key_vals);
            if let Some(rid) = idx.data.get(&key).iter().next() {
                return Some(rid.clone());
            }
        }
        None
    }

    pub(crate) fn check_unique_for(
        &self,
        q: &QualifiedName,
        values: &RowValues,
        exclude: Option<&str>,
    ) -> Result<()> {
        if let Some(loaded) = self.tables.get(q) {
            loaded.check_unique(values, exclude)?;
        }
        Ok(())
    }

    fn apply_conflict_update(
        &mut self,
        table: &Table,
        existing_id: &str,
        excluded: &RowValues,
        assignments: &[sqlparser::ast::Assignment],
        selection: Option<&sqlparser::ast::Expr>,
    ) -> Result<Option<RowValues>> {
        let existing = self
            .tables
            .get(&table.qualified())
            .and_then(|l| l.rows.get(existing_id))
            .cloned()
            .ok_or_else(|| SqlError::Internal("conflict row vanished".into()))?;

        // Frames: existing row (table alias) + excluded row.
        let table_schema = table_schema(table, &table.name);
        let existing_tuple = row_tuple(table, &existing);
        let excluded_schema = table_schema_named(table, "excluded");
        let excluded_tuple = row_tuple(table, excluded);

        if let Some(sel) = selection {
            let frames = [
                Frame {
                    schema: &table_schema,
                    row: &existing_tuple,
                },
                Frame {
                    schema: &excluded_schema,
                    row: &excluded_tuple,
                },
            ];
            if self.eval(sel, &frames)?.truthy() != Some(true) {
                return Ok(None);
            }
        }

        let mut new_values = existing.clone();
        for a in assignments {
            let col = assignment_column(&a.target)?;
            let frames = [
                Frame {
                    schema: &table_schema,
                    row: &existing_tuple,
                },
                Frame {
                    schema: &excluded_schema,
                    row: &excluded_tuple,
                },
            ];
            let value = self.eval(&a.value, &frames)?;
            let coerced = coerce_to_col(value, table, &col)?;
            new_values.insert(col, coerced);
        }
        self.check_constraints(table, &new_values)?;
        Ok(Some(new_values))
    }

    // ---- UPDATE --------------------------------------------------------

    pub fn exec_update(&mut self, update: &sqlparser::ast::Update) -> Result<ExecResult> {
        let (alias, q) = self.resolve_target(&update.table.relation)?;
        if self.catalog.get_view(&q).is_some() {
            return self.exec_update_on_view(&q, update);
        }
        let table = self.catalog.require_table(&q)?.clone();
        let collection = table.storage_collection.clone();
        let schema = table_schema(&table, &alias);

        // Triggers. `UPDATE OF` matches on the statement's assignment-target
        // columns (the SET list, not value diffs — PostgreSQL semantics).
        let has_triggers = !table.triggers.is_empty();
        let set_cols: Option<BTreeSet<String>> = if has_triggers {
            let mut cols = BTreeSet::new();
            for a in &update.assignments {
                cols.insert(assignment_column(&a.target)?);
            }
            Some(cols)
        } else {
            None
        };
        if has_triggers {
            // BEFORE STATEMENT fires before the snapshot below, so its
            // writes to the target table are visible to this statement's own
            // scan (PostgreSQL command-counter semantics); it also fires for
            // statements that end up matching zero rows.
            self.fire_statement_triggers(
                &table,
                TriggerTiming::Before,
                TriggerOp::Update,
                set_cols.as_ref(),
            )?;
        }

        // Snapshot the rows so we can evaluate predicates with &self. Rows the
        // current role may not update (row security, UPDATE USING) are skipped.
        let snapshot: Vec<(String, RowValues)> = self
            .tables
            .get(&q)
            .map(|l| {
                let hidden = self.rls_dml_hidden(&q);
                l.rows
                    .iter()
                    .filter(|(rid, _)| hidden.map(|h| !h.contains(*rid)).unwrap_or(true))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let mut targets: Vec<(String, RowValues, RowValues)> = Vec::new();
        for (rid, values) in &snapshot {
            let tuple = row_tuple(&table, values);
            let matched = match &update.selection {
                Some(sel) => {
                    let frame = Frame {
                        schema: &schema,
                        row: &tuple,
                    };
                    self.eval(sel, &[frame])?.truthy() == Some(true)
                }
                None => true,
            };
            if !matched {
                continue;
            }
            // Compute new values.
            let mut new_values = values.clone();
            for a in &update.assignments {
                let col = assignment_column(&a.target)?;
                let frame = Frame {
                    schema: &schema,
                    row: &tuple,
                };
                let value = self.eval(&a.value, &[frame])?;
                let coerced = coerce_to_col(value, &table, &col)?;
                new_values.insert(col, coerced);
            }
            // BEFORE ROW triggers run on the assignment result, before
            // NOT NULL/CHECK/row security (which then apply to the
            // trigger-final values) — PostgreSQL's ordering.
            if has_triggers {
                match self.fire_before_row(
                    &table,
                    TriggerOp::Update,
                    Some(values),
                    Some(new_values),
                    set_cols.as_ref(),
                )? {
                    Some(v) => new_values = v,
                    // Suppressed: the row keeps its old values and is
                    // excluded from the count and RETURNING.
                    None => continue,
                }
            }
            self.check_row_constraints(&table, &new_values)?;
            // Row security: the updated row must pass UPDATE WITH CHECK
            // (falling back to USING), like PostgreSQL.
            self.rls_check_new_row(
                &q,
                &table,
                &new_values,
                crate::relational::PolicyCmd::Update,
            )?;
            targets.push((rid.clone(), values.clone(), new_values));
        }

        let table_is_referenced = !self.catalog.referencing_foreign_keys(&q).is_empty();
        let mut updated: Vec<RowValues> = Vec::new();
        let mut after_rows: Vec<(RowValues, RowValues)> = Vec::new();
        let mut ri_work: Vec<crate::sql::fk::RiWork> = Vec::new();
        let mut count = 0;
        for (rid, old_values, new_values) in targets {
            // Take a row-level FOR UPDATE lock (acquired after this statement).
            self.record_pending(
                crate::sql::lock::LockObject::Row(table.oid, rid.clone()),
                crate::sql::lock::LockMode::ForUpdate,
                crate::sql::lock::LockScope::Transaction,
            );
            self.check_unique_for(&q, &new_values, Some(&rid))?;
            // Foreign keys of this table are re-checked on the columns the
            // update actually changed.
            self.fk_check_child(&table, &new_values, Some(&old_values))?;
            self.observe_serials(&table, &new_values);
            self.write_update(&q, &collection, &table, &rid, new_values.clone())?;
            if has_triggers {
                after_rows.push((old_values.clone(), new_values.clone()));
            }
            if table_is_referenced {
                ri_work.push(crate::sql::fk::RiWork::Updated {
                    table: q.clone(),
                    old: old_values,
                    new: new_values.clone(),
                });
            }
            updated.push(new_values);
            count += 1;
        }
        // Parent-side referential actions (per statement, after all target
        // rows are rewritten).
        self.fk_apply_referential_actions(ri_work)?;

        if has_triggers {
            // AFTER ROW triggers observe the statement's full effect,
            // referential actions included; AFTER STATEMENT fires last,
            // even for zero matched rows.
            for (old, new) in &after_rows {
                self.fire_after_row(
                    &table,
                    TriggerOp::Update,
                    Some(old),
                    Some(new),
                    set_cols.as_ref(),
                )?;
            }
            self.fire_statement_triggers(
                &table,
                TriggerTiming::After,
                TriggerOp::Update,
                set_cols.as_ref(),
            )?;
        }

        if let Some(returning) = &update.returning {
            return self.returning_result(&table, updated, returning);
        }
        Ok(ExecResult::empty_command(format!("UPDATE {count}")))
    }

    /// Apply a single-row update (handles primary-key changes by relocating).
    pub(crate) fn write_update(
        &mut self,
        q: &QualifiedName,
        collection: &str,
        table: &Table,
        row_id: &str,
        new_values: RowValues,
    ) -> Result<()> {
        let new_id = derive_row_id(table, &new_values).unwrap_or_else(|| row_id.to_string());
        let loaded = std::sync::Arc::make_mut(self.tables.get_mut(q).unwrap());
        if new_id != row_id {
            loaded.apply_delete(row_id);
            self.mutations.lock().unwrap().push(Mutation::Delete {
                collection: collection.to_string(),
                row_id: row_id.to_string(),
            });
            let version = loaded.version_of(&new_id);
            loaded.apply_insert(new_id.clone(), new_values.clone());
            let doc = encode_row(table, &new_id, &new_values, version);
            self.mutations.lock().unwrap().push(Mutation::Put {
                collection: collection.to_string(),
                row_id: new_id,
                doc,
            });
        } else {
            loaded.apply_update(row_id, new_values.clone());
            let version = loaded.version_of(row_id);
            let doc = encode_row(table, row_id, &new_values, version);
            self.mutations.lock().unwrap().push(Mutation::Put {
                collection: collection.to_string(),
                row_id: row_id.to_string(),
                doc,
            });
        }
        Ok(())
    }

    // ---- DELETE --------------------------------------------------------

    pub fn exec_delete(&mut self, delete: &Delete) -> Result<ExecResult> {
        let relation = match &delete.from {
            FromTable::WithFromKeyword(items) | FromTable::WithoutKeyword(items) => items
                .first()
                .map(|twj| &twj.relation)
                .ok_or_else(|| SqlError::Syntax("DELETE without table".into()))?,
        };
        let (alias, q) = self.resolve_target(relation)?;
        if self.catalog.get_view(&q).is_some() {
            return self.exec_delete_on_view(&q, delete);
        }
        let table = self.catalog.require_table(&q)?.clone();
        let collection = table.storage_collection.clone();
        let schema = table_schema(&table, &alias);

        let has_triggers = !table.triggers.is_empty();
        if has_triggers {
            // BEFORE STATEMENT fires before the snapshot below (its writes
            // are visible to this statement's own scan) and fires even for
            // zero matched rows.
            self.fire_statement_triggers(&table, TriggerTiming::Before, TriggerOp::Delete, None)?;
        }

        // Rows the current role may not delete (row security, DELETE USING)
        // are excluded from the snapshot, so they are silently skipped.
        let snapshot: Vec<(String, RowValues)> = self
            .tables
            .get(&q)
            .map(|l| {
                let hidden = self.rls_dml_hidden(&q);
                l.rows
                    .iter()
                    .filter(|(rid, _)| hidden.map(|h| !h.contains(*rid)).unwrap_or(true))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let mut deleted: Vec<RowValues> = Vec::new();
        let mut to_delete: Vec<String> = Vec::new();
        for (rid, values) in &snapshot {
            let tuple = row_tuple(&table, values);
            let matched = match &delete.selection {
                Some(sel) => {
                    let frame = Frame {
                        schema: &schema,
                        row: &tuple,
                    };
                    self.eval(sel, &[frame])?.truthy() == Some(true)
                }
                None => true,
            };
            if matched {
                // BEFORE DELETE row triggers: `RETURN NULL` suppresses the
                // deletion — the row survives and is excluded from the count
                // and RETURNING.
                if has_triggers
                    && self
                        .fire_before_row(&table, TriggerOp::Delete, Some(values), None, None)?
                        .is_none()
                {
                    continue;
                }
                to_delete.push(rid.clone());
                deleted.push(values.clone());
            }
        }

        for rid in &to_delete {
            self.record_pending(
                crate::sql::lock::LockObject::Row(table.oid, rid.clone()),
                crate::sql::lock::LockMode::ForUpdate,
                crate::sql::lock::LockScope::Transaction,
            );
        }
        let count = to_delete.len();
        let loaded = std::sync::Arc::make_mut(self.tables.get_mut(&q).unwrap());
        for rid in &to_delete {
            loaded.apply_delete(rid);
            self.mutations.lock().unwrap().push(Mutation::Delete {
                collection: collection.clone(),
                row_id: rid.clone(),
            });
        }

        // Referential actions run after all target rows are removed, so a
        // self-referential NO ACTION check sees the statement's full effect.
        if !self.catalog.referencing_foreign_keys(&q).is_empty() {
            let work = deleted
                .iter()
                .map(|row| crate::sql::fk::RiWork::Deleted {
                    table: q.clone(),
                    row: row.clone(),
                    cascade_depth: 1,
                })
                .collect();
            self.fk_apply_referential_actions(work)?;
        }

        if has_triggers {
            // AFTER DELETE row triggers fire once referential actions have
            // applied, so a parent-table trigger observes cascaded child
            // deletions; AFTER STATEMENT fires last, zero-row statements
            // included.
            for values in &deleted {
                self.fire_after_row(&table, TriggerOp::Delete, Some(values), None, None)?;
            }
            self.fire_statement_triggers(&table, TriggerTiming::After, TriggerOp::Delete, None)?;
        }

        if let Some(returning) = &delete.returning {
            return self.returning_result(&table, deleted, returning);
        }
        Ok(ExecResult::empty_command(format!("DELETE {count}")))
    }

    // ---- INSTEAD OF view routing ---------------------------------------

    /// Route an INSERT into a view by firing its INSTEAD OF INSERT triggers.
    /// If no such triggers exist, mirrors PostgreSQL's 0A000 error.
    fn exec_insert_on_view(&mut self, q: &QualifiedName, insert: &Insert) -> Result<ExecResult> {
        let (view_cols, view_query_str, has_triggers) = {
            let view = self
                .catalog
                .get_view(q)
                .ok_or_else(|| SqlError::UndefinedTable(q.name.clone()))?;
            let has = view.triggers.iter().any(|t| {
                t.enabled
                    && t.timing == TriggerTiming::InsteadOf
                    && t.level == TriggerLevel::Row
                    && t.events
                        .iter()
                        .any(|e| matches!(e, TriggerEventDef::Insert))
            });
            (view.columns.clone(), view.query.clone(), has)
        };
        if !has_triggers {
            return Err(SqlError::FeatureNotSupported(format!(
                "cannot insert into view \"{}\"",
                q.name
            )));
        }

        let target_cols: Vec<String> = if !insert.columns.is_empty() {
            insert
                .columns
                .iter()
                .map(|on| {
                    on.0.last()
                        .and_then(|p| p.as_ident())
                        .map(ident_name)
                        .unwrap_or_default()
                })
                .collect()
        } else if !view_cols.is_empty() {
            view_cols.clone()
        } else {
            // Derive column names from the view's SELECT query (no explicit
            // column list was given at CREATE VIEW time).
            let stmts = crate::sql::parser::parse_sql(&view_query_str)?;
            let query = match stmts.into_iter().next() {
                Some(sqlparser::ast::Statement::Query(q)) => q,
                _ => return Err(SqlError::Internal("view definition is not a query".into())),
            };
            let rs = self.exec_select_query(&query, &[])?;
            rs.schema.fields.iter().map(|f| f.name.clone()).collect()
        };

        let provided_rows: Vec<RowValues> = match &insert.source {
            None => vec![RowValues::new()],
            Some(src) => match src.body.as_ref() {
                sqlparser::ast::SetExpr::Values(values) => {
                    let mut out = Vec::new();
                    for row in &values.rows {
                        let content = &row.content;
                        if content.len() != target_cols.len() {
                            return Err(SqlError::Syntax(format!(
                                "INSERT has {} target columns but {} values",
                                target_cols.len(),
                                content.len()
                            )));
                        }
                        let mut map = RowValues::new();
                        for (col, expr) in target_cols.iter().zip(content) {
                            if !is_default_expr(expr) {
                                map.insert(col.clone(), self.eval(expr, &[])?);
                            }
                        }
                        out.push(map);
                    }
                    out
                }
                _ => {
                    let rs = self.exec_select_query(src, &[])?;
                    let mut out = Vec::new();
                    for row in rs.rows {
                        if row.len() != target_cols.len() {
                            return Err(SqlError::Syntax(format!(
                                "INSERT has {} target columns but {} values",
                                target_cols.len(),
                                row.len()
                            )));
                        }
                        out.push(target_cols.iter().cloned().zip(row).collect());
                    }
                    out
                }
            },
        };

        let mut count = 0usize;
        for new_row in provided_rows {
            if self
                .fire_instead_of_row(q, TriggerOp::Insert, None, Some(new_row), None)?
                .is_some()
            {
                count += 1;
            }
        }
        Ok(ExecResult::empty_command(format!("INSERT 0 {count}")))
    }

    /// Route an UPDATE targeting a view by firing its INSTEAD OF UPDATE triggers.
    fn exec_update_on_view(
        &mut self,
        q: &QualifiedName,
        update: &sqlparser::ast::Update,
    ) -> Result<ExecResult> {
        let (view_name, has_triggers) = {
            let view = self
                .catalog
                .get_view(q)
                .ok_or_else(|| SqlError::UndefinedTable(q.name.clone()))?;
            let has = view.triggers.iter().any(|t| {
                t.enabled
                    && t.timing == TriggerTiming::InsteadOf
                    && t.level == TriggerLevel::Row
                    && t.events
                        .iter()
                        .any(|e| matches!(e, TriggerEventDef::Update { .. }))
            });
            (view.name.clone(), has)
        };
        if !has_triggers {
            return Err(SqlError::FeatureNotSupported(format!(
                "cannot update view \"{}\"",
                view_name
            )));
        }

        let set_cols: BTreeSet<String> = update
            .assignments
            .iter()
            .map(|a| assignment_column(&a.target))
            .collect::<Result<_>>()?;

        let view_query = self.catalog.get_view(q).map(|v| v.query.clone()).unwrap();
        let stmts = crate::sql::parser::parse_sql(&view_query)?;
        let query = match stmts.into_iter().next() {
            Some(sqlparser::ast::Statement::Query(q)) => q,
            _ => return Err(SqlError::Internal("view definition is not a query".into())),
        };
        let rs = self.exec_select_query(&query, &[])?;

        let mut row_pairs: Vec<(RowValues, RowValues)> = Vec::new();
        for tuple in &rs.rows {
            let matched = match &update.selection {
                Some(sel) => {
                    let frame = Frame {
                        schema: &rs.schema,
                        row: tuple,
                    };
                    self.eval(sel, &[frame])?.truthy() == Some(true)
                }
                None => true,
            };
            if !matched {
                continue;
            }
            let old_row: RowValues = rs
                .schema
                .fields
                .iter()
                .zip(tuple.iter())
                .map(|(f, v)| (f.name.clone(), v.clone()))
                .collect();
            let mut new_row = old_row.clone();
            for a in &update.assignments {
                let col = assignment_column(&a.target)?;
                let frame = Frame {
                    schema: &rs.schema,
                    row: tuple,
                };
                let value = self.eval(&a.value, &[frame])?;
                new_row.insert(col, value);
            }
            row_pairs.push((old_row, new_row));
        }

        let mut count = 0usize;
        for (old_row, new_row) in row_pairs {
            if self
                .fire_instead_of_row(
                    q,
                    TriggerOp::Update,
                    Some(&old_row),
                    Some(new_row),
                    Some(&set_cols),
                )?
                .is_some()
            {
                count += 1;
            }
        }
        Ok(ExecResult::empty_command(format!("UPDATE {count}")))
    }

    /// Route a DELETE targeting a view by firing its INSTEAD OF DELETE triggers.
    fn exec_delete_on_view(&mut self, q: &QualifiedName, delete: &Delete) -> Result<ExecResult> {
        let (view_name, has_triggers) = {
            let view = self
                .catalog
                .get_view(q)
                .ok_or_else(|| SqlError::UndefinedTable(q.name.clone()))?;
            let has = view.triggers.iter().any(|t| {
                t.enabled
                    && t.timing == TriggerTiming::InsteadOf
                    && t.level == TriggerLevel::Row
                    && t.events
                        .iter()
                        .any(|e| matches!(e, TriggerEventDef::Delete))
            });
            (view.name.clone(), has)
        };
        if !has_triggers {
            return Err(SqlError::FeatureNotSupported(format!(
                "cannot delete from view \"{}\"",
                view_name
            )));
        }

        let view_query = self.catalog.get_view(q).map(|v| v.query.clone()).unwrap();
        let stmts = crate::sql::parser::parse_sql(&view_query)?;
        let query = match stmts.into_iter().next() {
            Some(sqlparser::ast::Statement::Query(q)) => q,
            _ => return Err(SqlError::Internal("view definition is not a query".into())),
        };
        let rs = self.exec_select_query(&query, &[])?;

        let mut old_rows: Vec<RowValues> = Vec::new();
        for tuple in &rs.rows {
            let matched = match &delete.selection {
                Some(sel) => {
                    let frame = Frame {
                        schema: &rs.schema,
                        row: tuple,
                    };
                    self.eval(sel, &[frame])?.truthy() == Some(true)
                }
                None => true,
            };
            if matched {
                let old_row: RowValues = rs
                    .schema
                    .fields
                    .iter()
                    .zip(tuple.iter())
                    .map(|(f, v)| (f.name.clone(), v.clone()))
                    .collect();
                old_rows.push(old_row);
            }
        }

        let mut count = 0usize;
        for old_row in old_rows {
            if self
                .fire_instead_of_row(q, TriggerOp::Delete, Some(&old_row), None, None)?
                .is_some()
            {
                count += 1;
            }
        }
        Ok(ExecResult::empty_command(format!("DELETE {count}")))
    }

    // ---- shared helpers ------------------------------------------------

    fn resolve_target(&self, relation: &TableFactor) -> Result<(String, QualifiedName)> {
        match relation {
            TableFactor::Table { name, alias, .. } => {
                let (schema, n) = split_schema_table(name);
                let q = self
                    .catalog
                    .resolve_table_name(schema.as_deref(), &n)
                    .ok_or_else(|| SqlError::UndefinedTable(n.clone()))?;
                let alias_name = alias
                    .as_ref()
                    .map(|a| ident_name(&a.name))
                    .unwrap_or_else(|| object_name_parts(name).last().cloned().unwrap_or(n));
                Ok((alias_name, q))
            }
            other => Err(SqlError::FeatureNotSupported(format!(
                "target must be a table: {other}"
            ))),
        }
    }

    fn returning_result(
        &self,
        table: &Table,
        rows: Vec<RowValues>,
        items: &[SelectItem],
    ) -> Result<ExecResult> {
        let schema = table_schema(table, &table.name);
        // Output fields.
        let mut fields: Vec<OutField> = Vec::new();
        for item in items {
            match item {
                SelectItem::Wildcard(_) => {
                    for c in &table.columns {
                        fields.push(OutField::new(c.name.clone(), c.ty.clone()));
                    }
                }
                SelectItem::UnnamedExpr(e) => {
                    fields.push(OutField::new(
                        crate::sql::select::default_col_name(e),
                        self.infer_type(e, &schema),
                    ));
                }
                SelectItem::ExprWithAlias { expr, alias } => {
                    fields.push(OutField::new(
                        ident_name(alias),
                        self.infer_type(expr, &schema),
                    ));
                }
                _ => {
                    return Err(SqlError::FeatureNotSupported(
                        "RETURNING item not supported".into(),
                    ));
                }
            }
        }
        let mut out_rows = Vec::new();
        for values in &rows {
            let tuple = row_tuple(table, values);
            let frame = Frame {
                schema: &schema,
                row: &tuple,
            };
            let mut out = Vec::new();
            for item in items {
                match item {
                    SelectItem::Wildcard(_) => out.extend(tuple.iter().cloned()),
                    SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                        out.push(self.eval(
                            e,
                            &[Frame {
                                schema: &schema,
                                row: &tuple,
                            }],
                        )?);
                    }
                    _ => {}
                }
            }
            let _ = frame;
            out_rows.push(out);
        }
        Ok(ExecResult::Rows {
            fields,
            rows: out_rows,
        })
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

/// Build a RowSchema describing a table's columns labelled with `alias`.
pub fn table_schema(table: &Table, alias: &str) -> RowSchema {
    table_schema_named(table, alias)
}

pub fn table_schema_named(table: &Table, alias: &str) -> RowSchema {
    RowSchema::new(
        table
            .columns
            .iter()
            .map(|c| FieldRef {
                table: Some(alias.to_string()),
                name: c.name.clone(),
                ty: c.ty.clone(),
            })
            .collect(),
    )
}

/// Build a tuple of a row's values in column order.
pub fn row_tuple(table: &Table, values: &RowValues) -> Tuple {
    table
        .columns
        .iter()
        .map(|c| values.get(&c.name).cloned().unwrap_or(SqlValue::Null))
        .collect()
}

fn assignment_column(target: &AssignmentTarget) -> Result<String> {
    match target {
        AssignmentTarget::ColumnName(name) => Ok(name
            .0
            .last()
            .and_then(|p| p.as_ident())
            .map(ident_name)
            .unwrap_or_default()),
        AssignmentTarget::Tuple(_) => Err(SqlError::FeatureNotSupported(
            "tuple assignment not supported".into(),
        )),
    }
}

fn coerce_to(value: SqlValue, ty: &SqlType, column: &str) -> Result<SqlValue> {
    if value.is_null() {
        return Ok(SqlValue::Null);
    }
    value.cast(ty).map_err(|_| SqlError::DatatypeMismatch {
        column: column.to_string(),
        expected: ty.name(),
        actual: value.type_of().name(),
    })
}

/// Is this VALUES cell the bare `DEFAULT` keyword (parsed as an identifier)?
fn is_default_expr(expr: &sqlparser::ast::Expr) -> bool {
    matches!(expr, sqlparser::ast::Expr::Identifier(i) if i.value.eq_ignore_ascii_case("default"))
}

pub(crate) fn coerce_to_col(value: SqlValue, table: &Table, col: &str) -> Result<SqlValue> {
    let c = table
        .column(col)
        .ok_or_else(|| SqlError::UndefinedColumn(col.to_string()))?;
    coerce_to(value, &c.ty, col)
}

// Maintenance note 7: documents compatibility expectations without changing runtime behavior.

// Maintenance note 19: documents compatibility expectations without changing runtime behavior.

// Maintenance note: keeps SQL compatibility behavior explicit for future updates.

// Maintenance note: keeps SQL compatibility behavior explicit for future updates.

// SQL compatibility note 8: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.

// SQL compatibility note 24: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.

// SQL compatibility note 8: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.

// SQL compatibility note 24: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.
