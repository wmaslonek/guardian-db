//! DML execution: INSERT / UPDATE / DELETE with RETURNING and ON CONFLICT.

use crate::error::{Result, SqlError};
use crate::exec::{Exec, Frame};
use crate::names::{ident_name, object_name_parts, split_schema_table};
use crate::result::{ExecResult, OutField};
use crate::row::{FieldRef, RowSchema, Tuple};
use crate::store::{derive_row_id, encode_row, index_values, Mutation, RowValues};
use guardian_relational::catalog::{QualifiedName, Table};
use guardian_relational::{composite_key, ordered_key, SqlType, SqlValue};
use sqlparser::ast::{
    AssignmentTarget, Delete, FromTable, Insert, OnConflictAction, OnInsert, SelectItem, Statement,
    TableFactor,
};
use std::collections::BTreeMap;

impl Exec {
    // ---- INSERT --------------------------------------------------------

    pub fn exec_insert(&mut self, insert: &Insert) -> Result<ExecResult> {
        let table_name = match &insert.table {
            sqlparser::ast::TableObject::TableName(name) => name.clone(),
            other => {
                return Err(SqlError::FeatureNotSupported(format!(
                    "INSERT target not supported: {other}"
                )))
            }
        };
        let (schema, n) = split_schema_table(&table_name);
        let q = self
            .catalog
            .resolve_table_name(schema.as_deref(), &n)
            .ok_or_else(|| SqlError::UndefinedTable(n.clone()))?;
        let table = self.catalog.require_table(&q)?.clone();

        // Target columns.
        let target_cols: Vec<String> = if insert.columns.is_empty() {
            table.columns.iter().map(|c| c.name.clone()).collect()
        } else {
            insert
                .columns
                .iter()
                .map(|on| on.0.last().and_then(|p| p.as_ident()).map(ident_name).unwrap_or_default())
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
            let values = self.prepare_row(&table, provided)?;
            let row_id = derive_row_id(&table, &values).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
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
        let mut inserted_rows: Vec<RowValues> = Vec::new();
        let mut count = 0usize;

        for (mut row_id, values) in prepared {
            // Detect conflict.
            let conflict = self.find_conflict(&q, &values, conflict_target.as_deref());
            if let Some(existing_id) = conflict {
                match &insert.on {
                    Some(OnInsert::OnConflict(oc)) => match &oc.action {
                        OnConflictAction::DoNothing => continue,
                        OnConflictAction::DoUpdate(do_update) => {
                            let new_vals = self.apply_conflict_update(
                                &table,
                                &existing_id,
                                &values,
                                &do_update.assignments,
                                do_update.selection.as_ref(),
                            )?;
                            let Some(new_vals) = new_vals else { continue };
                            self.write_update(&q, &collection, &table, &existing_id, new_vals.clone())?;
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
            // Fresh insert: enforce uniqueness, then write.
            self.check_unique_for(&q, &values, None)?;
            self.observe_serials(&table, &values);
            let loaded = self.tables.get_mut(&q).unwrap();
            let version = loaded.version_of(&row_id);
            loaded.apply_insert(row_id.clone(), values.clone());
            let doc = encode_row(&table, &row_id, &values, version);
            self.mutations.push(Mutation::Put { collection: collection.clone(), row_id: std::mem::take(&mut row_id), doc });
            inserted_rows.push(values);
            count += 1;
        }

        if let Some(returning) = &insert.returning {
            return self.returning_result(&table, inserted_rows, returning);
        }
        Ok(ExecResult::empty_command(format!("INSERT 0 {count}")))
    }

    /// Build a complete row from provided values, applying defaults, serial
    /// sequences, coercion, and NOT NULL enforcement.
    fn prepare_row(&mut self, table: &Table, provided: BTreeMap<String, SqlValue>) -> Result<RowValues> {
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
            if value.is_null() && !col.nullable {
                return Err(SqlError::NotNullViolation {
                    column: col.name.clone(),
                    table: table.name.clone(),
                });
            }
            values.insert(col.name.clone(), value);
        }
        self.check_constraints(table, &values)?;
        Ok(values)
    }

    fn eval_default(&self, default_sql: &str) -> Result<SqlValue> {
        let stmts = crate::parser::parse_sql(&format!("SELECT {default_sql}"))?;
        if let Some(Statement::Query(q)) = stmts.into_iter().next() {
            if let sqlparser::ast::SetExpr::Select(select) = q.body.as_ref() {
                if let Some(SelectItem::UnnamedExpr(e)) | Some(SelectItem::ExprWithAlias { expr: e, .. }) =
                    select.projection.first()
                {
                    return self.eval(e, &[]);
                }
            }
        }
        Ok(SqlValue::Null)
    }

    fn observe_serials(&mut self, table: &Table, values: &RowValues) {
        for col in &table.columns {
            if let Some(seq) = &col.identity_sequence {
                if let Some(v) = values.get(&col.name).and_then(SqlValue::as_i64) {
                    self.catalog.observe_sequence_value(&table.schema, seq, v);
                }
            }
        }
    }

    fn check_constraints(&self, table: &Table, values: &RowValues) -> Result<()> {
        for check in &table.checks {
            let stmts = crate::parser::parse_sql(&format!("SELECT {}", check.expr))?;
            if let Some(Statement::Query(q)) = stmts.into_iter().next() {
                if let sqlparser::ast::SetExpr::Select(select) = q.body.as_ref() {
                    if let Some(item) = select.projection.first() {
                        let expr = match item {
                            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => e,
                            _ => continue,
                        };
                        let schema = table_schema(table, &table.name);
                        let tuple = row_tuple(table, values);
                        let frame = Frame { schema: &schema, row: &tuple };
                        // CHECK passes unless it evaluates to FALSE (NULL passes).
                        if self.eval(expr, &[frame])?.truthy() == Some(false) {
                            return Err(SqlError::CheckViolation {
                                table: table.name.clone(),
                                constraint: check.name.clone(),
                            });
                        }
                    }
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
            if let Some(t) = target {
                if idx.meta.columns != t {
                    continue;
                }
            }
            let key_vals = index_values(&idx.meta, values);
            if composite_key(&key_vals).is_none() {
                continue;
            }
            let key = ordered_key(&key_vals);
            if let Some(rid) = idx.data.get(&key).into_iter().next() {
                return Some(rid);
            }
        }
        None
    }

    fn check_unique_for(&self, q: &QualifiedName, values: &RowValues, exclude: Option<&str>) -> Result<()> {
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
                Frame { schema: &table_schema, row: &existing_tuple },
                Frame { schema: &excluded_schema, row: &excluded_tuple },
            ];
            if self.eval(sel, &frames)?.truthy() != Some(true) {
                return Ok(None);
            }
        }

        let mut new_values = existing.clone();
        for a in assignments {
            let col = assignment_column(&a.target)?;
            let frames = [
                Frame { schema: &table_schema, row: &existing_tuple },
                Frame { schema: &excluded_schema, row: &excluded_tuple },
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
        let table = self.catalog.require_table(&q)?.clone();
        let collection = table.storage_collection.clone();
        let schema = table_schema(&table, &alias);

        // Snapshot the rows so we can evaluate predicates with &self.
        let snapshot: Vec<(String, RowValues)> = self
            .tables
            .get(&q)
            .map(|l| l.rows.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();

        let mut targets: Vec<(String, RowValues)> = Vec::new();
        for (rid, values) in &snapshot {
            let tuple = row_tuple(&table, values);
            let matched = match &update.selection {
                Some(sel) => {
                    let frame = Frame { schema: &schema, row: &tuple };
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
                let frame = Frame { schema: &schema, row: &tuple };
                let value = self.eval(&a.value, &[frame])?;
                let coerced = coerce_to_col(value, &table, &col)?;
                new_values.insert(col, coerced);
            }
            // NOT NULL.
            for c in &table.columns {
                if !c.nullable && new_values.get(&c.name).map(SqlValue::is_null).unwrap_or(true) {
                    return Err(SqlError::NotNullViolation { column: c.name.clone(), table: table.name.clone() });
                }
            }
            self.check_constraints(&table, &new_values)?;
            targets.push((rid.clone(), new_values));
        }

        let mut updated: Vec<RowValues> = Vec::new();
        let mut count = 0;
        for (rid, new_values) in targets {
            self.check_unique_for(&q, &new_values, Some(&rid))?;
            self.observe_serials(&table, &new_values);
            self.write_update(&q, &collection, &table, &rid, new_values.clone())?;
            updated.push(new_values);
            count += 1;
        }

        if let Some(returning) = &update.returning {
            return self.returning_result(&table, updated, returning);
        }
        Ok(ExecResult::empty_command(format!("UPDATE {count}")))
    }

    /// Apply a single-row update (handles primary-key changes by relocating).
    fn write_update(
        &mut self,
        q: &QualifiedName,
        collection: &str,
        table: &Table,
        row_id: &str,
        new_values: RowValues,
    ) -> Result<()> {
        let new_id = derive_row_id(table, &new_values).unwrap_or_else(|| row_id.to_string());
        let loaded = self.tables.get_mut(q).unwrap();
        if new_id != row_id {
            loaded.apply_delete(row_id);
            self.mutations.push(Mutation::Delete { collection: collection.to_string(), row_id: row_id.to_string() });
            let version = loaded.version_of(&new_id);
            loaded.apply_insert(new_id.clone(), new_values.clone());
            let doc = encode_row(table, &new_id, &new_values, version);
            self.mutations.push(Mutation::Put { collection: collection.to_string(), row_id: new_id, doc });
        } else {
            loaded.apply_update(row_id, new_values.clone());
            let version = loaded.version_of(row_id);
            let doc = encode_row(table, row_id, &new_values, version);
            self.mutations.push(Mutation::Put { collection: collection.to_string(), row_id: row_id.to_string(), doc });
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
        let table = self.catalog.require_table(&q)?.clone();
        let collection = table.storage_collection.clone();
        let schema = table_schema(&table, &alias);

        let snapshot: Vec<(String, RowValues)> = self
            .tables
            .get(&q)
            .map(|l| l.rows.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();

        let mut deleted: Vec<RowValues> = Vec::new();
        let mut to_delete: Vec<String> = Vec::new();
        for (rid, values) in &snapshot {
            let tuple = row_tuple(&table, values);
            let matched = match &delete.selection {
                Some(sel) => {
                    let frame = Frame { schema: &schema, row: &tuple };
                    self.eval(sel, &[frame])?.truthy() == Some(true)
                }
                None => true,
            };
            if matched {
                to_delete.push(rid.clone());
                deleted.push(values.clone());
            }
        }

        let count = to_delete.len();
        let loaded = self.tables.get_mut(&q).unwrap();
        for rid in &to_delete {
            loaded.apply_delete(rid);
            self.mutations.push(Mutation::Delete { collection: collection.clone(), row_id: rid.clone() });
        }

        if let Some(returning) = &delete.returning {
            return self.returning_result(&table, deleted, returning);
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
                    fields.push(OutField::new(crate::select::default_col_name(e), self.infer_type(e, &schema)));
                }
                SelectItem::ExprWithAlias { expr, alias } => {
                    fields.push(OutField::new(ident_name(alias), self.infer_type(expr, &schema)));
                }
                _ => {
                    return Err(SqlError::FeatureNotSupported("RETURNING item not supported".into()))
                }
            }
        }
        let mut out_rows = Vec::new();
        for values in &rows {
            let tuple = row_tuple(table, values);
            let frame = Frame { schema: &schema, row: &tuple };
            let mut out = Vec::new();
            for item in items {
                match item {
                    SelectItem::Wildcard(_) => out.extend(tuple.iter().cloned()),
                    SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                        out.push(self.eval(e, &[Frame { schema: &schema, row: &tuple }])?);
                    }
                    _ => {}
                }
            }
            let _ = frame;
            out_rows.push(out);
        }
        Ok(ExecResult::Rows { fields, rows: out_rows })
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
            .map(|c| FieldRef { table: Some(alias.to_string()), name: c.name.clone(), ty: c.ty.clone() })
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
        AssignmentTarget::Tuple(_) => {
            Err(SqlError::FeatureNotSupported("tuple assignment not supported".into()))
        }
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

fn coerce_to_col(value: SqlValue, table: &Table, col: &str) -> Result<SqlValue> {
    let c = table
        .column(col)
        .ok_or_else(|| SqlError::UndefinedColumn(col.to_string()))?;
    coerce_to(value, &c.ty, col)
}
