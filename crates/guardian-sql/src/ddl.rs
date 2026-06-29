//! DDL execution: CREATE/ALTER/DROP TABLE, schemas, indexes, views, TRUNCATE.

use crate::error::{Result, SqlError};
use crate::exec::Exec;
use crate::names::{ident_name, split_schema_table};
use crate::result::ExecResult;
use crate::store::{encode_row, Mutation};
use guardian_relational::catalog::{
    CheckConstraint, Column, ForeignKey, Index, PrimaryKey, QualifiedName, ReferentialAction,
    Table, UniqueConstraint, View,
};
use guardian_relational::SqlType;
use sqlparser::ast::{
    AlterColumnOperation, AlterTableOperation, ColumnDef, ColumnOption, CreateIndex, CreateTable,
    Statement, TableConstraint,
};

impl Exec {
    pub fn exec_create_table(&mut self, ct: &CreateTable) -> Result<ExecResult> {
        let (schema, name) = split_schema_table(&ct.name);
        let schema = self.catalog.creation_schema(schema.as_deref())?;
        let q = QualifiedName::new(schema.clone(), name.clone());
        if self.catalog.has_table(&q) {
            if ct.if_not_exists {
                return Ok(ExecResult::empty_command("CREATE TABLE"));
            }
            return Err(SqlError::DuplicateTable(q.to_string_qualified()));
        }

        let oid = self.catalog.allocate_oid();
        let mut columns = Vec::new();
        let mut pk_columns: Vec<String> = Vec::new();
        let mut uniques: Vec<UniqueConstraint> = Vec::new();
        let mut foreign_keys: Vec<ForeignKey> = Vec::new();
        let mut checks: Vec<CheckConstraint> = Vec::new();
        let mut sequences_to_create: Vec<(String, String)> = Vec::new(); // (seq, column)

        for (ordinal, col) in ct.columns.iter().enumerate() {
            let column = self.build_column(&schema, &name, col, ordinal, &mut sequences_to_create, &mut pk_columns, &mut uniques, &mut foreign_keys, &mut checks)?;
            columns.push(column);
        }

        // Table-level constraints.
        for constraint in &ct.constraints {
            self.apply_table_constraint(constraint, &mut pk_columns, &mut uniques, &mut foreign_keys, &mut checks)?;
        }

        // Mark PK columns NOT NULL.
        for c in &mut columns {
            if pk_columns.contains(&c.name) {
                c.nullable = false;
            }
        }

        let primary_key = if pk_columns.is_empty() {
            None
        } else {
            Some(PrimaryKey { name: format!("{name}_pkey"), columns: pk_columns.clone() })
        };

        let table = Table {
            oid,
            schema: schema.clone(),
            name: name.clone(),
            columns,
            primary_key: primary_key.clone(),
            uniques: uniques.clone(),
            foreign_keys,
            checks,
            storage_collection: String::new(),
        };
        self.catalog.insert_table(table)?;

        // Create sequences for serial columns.
        for (seq, _col) in &sequences_to_create {
            self.catalog.create_sequence(&schema, seq)?;
        }

        // Create the primary-key index.
        if let Some(pk) = &primary_key {
            let idx_oid = self.catalog.allocate_oid();
            self.catalog.insert_index(Index {
                oid: idx_oid,
                name: pk.name.clone(),
                schema: schema.clone(),
                table: name.clone(),
                columns: pk.columns.clone(),
                unique: true,
                primary: true,
                method: "btree".into(),
            })?;
        }
        // Create unique indexes.
        for u in &uniques {
            let idx_oid = self.catalog.allocate_oid();
            let iname = if u.name.is_empty() {
                format!("{name}_{}_key", u.columns.join("_"))
            } else {
                u.name.clone()
            };
            self.catalog.insert_index(Index {
                oid: idx_oid,
                name: iname,
                schema: schema.clone(),
                table: name.clone(),
                columns: u.columns.clone(),
                unique: true,
                primary: false,
                method: "btree".into(),
            })?;
        }

        self.catalog_dirty = true;
        Ok(ExecResult::empty_command("CREATE TABLE"))
    }

    #[allow(clippy::too_many_arguments)]
    fn build_column(
        &mut self,
        schema: &str,
        table: &str,
        col: &ColumnDef,
        ordinal: usize,
        sequences: &mut Vec<(String, String)>,
        pk_columns: &mut Vec<String>,
        uniques: &mut Vec<UniqueConstraint>,
        foreign_keys: &mut Vec<ForeignKey>,
        checks: &mut Vec<CheckConstraint>,
    ) -> Result<Column> {
        let name = ident_name(&col.name);
        let type_text = col.data_type.to_string();
        let (ty, is_serial) = match SqlType::is_serial_name(&type_text) {
            Some(t) => (t, true),
            None => (crate::eval::parse_data_type(&col.data_type)?, false),
        };

        let mut nullable = true;
        let mut default: Option<String> = None;
        let mut identity_sequence: Option<String> = None;

        if is_serial {
            let seq = format!("{table}_{name}_seq");
            default = Some(format!("nextval('{seq}')"));
            identity_sequence = Some(seq.clone());
            nullable = false;
            sequences.push((seq, name.clone()));
        }

        for opt in &col.options {
            match &opt.option {
                ColumnOption::NotNull => nullable = false,
                ColumnOption::Null => nullable = true,
                ColumnOption::Default(expr) => default = Some(expr.to_string()),
                ColumnOption::PrimaryKey(_) => {
                    if !pk_columns.contains(&name) {
                        pk_columns.push(name.clone());
                    }
                    nullable = false;
                }
                ColumnOption::Unique(u) => {
                    if u.is_primary_via_kind() {
                        if !pk_columns.contains(&name) {
                            pk_columns.push(name.clone());
                        }
                        nullable = false;
                    } else {
                        uniques.push(UniqueConstraint {
                            name: opt
                                .name
                                .as_ref()
                                .map(ident_name)
                                .unwrap_or_default(),
                            columns: vec![name.clone()],
                        });
                    }
                }
                ColumnOption::ForeignKey(fk) => {
                    let (_fs, ft) = split_schema_table(&fk.foreign_table);
                    foreign_keys.push(ForeignKey {
                        name: opt.name.as_ref().map(ident_name).unwrap_or_else(|| format!("{table}_{name}_fkey")),
                        columns: vec![name.clone()],
                        ref_schema: "public".into(),
                        ref_table: ft,
                        ref_columns: fk.referred_columns.iter().map(ident_name).collect(),
                        on_delete: map_action(fk.on_delete),
                        on_update: map_action(fk.on_update),
                    });
                }
                ColumnOption::Check(c) => {
                    checks.push(CheckConstraint {
                        name: opt.name.as_ref().map(ident_name).unwrap_or_else(|| format!("{table}_{name}_check")),
                        expr: c.expr.to_string(),
                    });
                }
                _ => {}
            }
        }
        let _ = schema;
        Ok(Column { name, ty, nullable, default, identity_sequence, ordinal })
    }

    fn apply_table_constraint(
        &self,
        constraint: &TableConstraint,
        pk_columns: &mut Vec<String>,
        uniques: &mut Vec<UniqueConstraint>,
        foreign_keys: &mut Vec<ForeignKey>,
        checks: &mut Vec<CheckConstraint>,
    ) -> Result<()> {
        match constraint {
            TableConstraint::PrimaryKey(pk) => {
                for ic in &pk.columns {
                    pk_columns.push(index_column_name(ic)?);
                }
            }
            TableConstraint::Unique(u) => {
                let cols: Result<Vec<String>> = u.columns.iter().map(index_column_name).collect();
                uniques.push(UniqueConstraint {
                    name: u.name.as_ref().map(ident_name).unwrap_or_default(),
                    columns: cols?,
                });
            }
            TableConstraint::ForeignKey(fk) => {
                let (_fs, ft) = split_schema_table(&fk.foreign_table);
                foreign_keys.push(ForeignKey {
                    name: fk.name.as_ref().map(ident_name).unwrap_or_else(|| "fk".into()),
                    columns: fk.columns.iter().map(ident_name).collect(),
                    ref_schema: "public".into(),
                    ref_table: ft,
                    ref_columns: fk.referred_columns.iter().map(ident_name).collect(),
                    on_delete: map_action(fk.on_delete),
                    on_update: map_action(fk.on_update),
                });
            }
            TableConstraint::Check(c) => {
                checks.push(CheckConstraint {
                    name: c.name.as_ref().map(ident_name).unwrap_or_else(|| "check".into()),
                    expr: c.expr.to_string(),
                });
            }
            _ => {}
        }
        Ok(())
    }

    pub fn exec_create_schema(&mut self, name: &str, if_not_exists: bool) -> Result<ExecResult> {
        self.catalog.create_schema(name, if_not_exists)?;
        self.catalog_dirty = true;
        Ok(ExecResult::empty_command("CREATE SCHEMA"))
    }

    pub fn exec_create_index(&mut self, ci: &CreateIndex) -> Result<ExecResult> {
        let (schema, table) = split_schema_table(&ci.table_name);
        let q = self
            .catalog
            .resolve_table_name(schema.as_deref(), &table)
            .ok_or_else(|| SqlError::UndefinedTable(table.clone()))?;
        let columns: Result<Vec<String>> = ci.columns.iter().map(index_column_name).collect();
        let columns = columns?;
        let name = match &ci.name {
            Some(n) => split_schema_table(n).1,
            None => format!("{}_{}_idx", q.name, columns.join("_")),
        };
        let exists = self
            .catalog
            .get_index(&QualifiedName::new(q.schema.clone(), name.clone()))
            .is_some();
        if exists {
            if ci.if_not_exists {
                return Ok(ExecResult::empty_command("CREATE INDEX"));
            }
            return Err(SqlError::DuplicateIndex(name));
        }
        let oid = self.catalog.allocate_oid();
        self.catalog.insert_index(Index {
            oid,
            name,
            schema: q.schema.clone(),
            table: q.name.clone(),
            columns,
            unique: ci.unique,
            primary: false,
            method: "btree".into(),
        })?;
        // Unique index: validate existing rows do not already violate it.
        if ci.unique {
            if let Some(loaded) = self.tables.get(&q) {
                let mut seen = std::collections::HashMap::new();
                let idx = self.catalog.indexes_for_table(&q.schema, &q.name);
                let idx = idx.last().unwrap();
                for (rid, values) in &loaded.rows {
                    let key = guardian_relational::ordered_key(&crate::store::index_values(idx, values));
                    if guardian_relational::composite_key(&crate::store::index_values(idx, values)).is_some() {
                        if let Some(_other) = seen.insert(key, rid.clone()) {
                            return Err(SqlError::UniqueViolation {
                                constraint: idx.name.clone(),
                                detail: "could not create unique index".into(),
                            });
                        }
                    }
                }
            }
        }
        self.catalog_dirty = true;
        Ok(ExecResult::empty_command("CREATE INDEX"))
    }

    pub fn exec_create_view(&mut self, cv: &sqlparser::ast::CreateView) -> Result<ExecResult> {
        if cv.materialized {
            return Err(SqlError::FeatureNotSupported(
                "materialized views are not supported".into(),
            ));
        }
        let (schema, name) = split_schema_table(&cv.name);
        let schema = self.catalog.creation_schema(schema.as_deref())?;
        let q = QualifiedName::new(schema.clone(), name.clone());
        if self.catalog.get_view(&q).is_some() && !cv.or_replace {
            return Err(SqlError::DuplicateTable(q.to_string_qualified()));
        }
        if self.catalog.get_view(&q).is_some() {
            self.catalog.drop_view(&q, true)?;
        }
        let oid = self.catalog.allocate_oid();
        let columns = cv.columns.iter().map(|c| ident_name(&c.name)).collect();
        self.catalog.insert_view(View {
            oid,
            schema,
            name,
            query: cv.query.to_string(),
            columns,
        })?;
        self.catalog_dirty = true;
        Ok(ExecResult::empty_command("CREATE VIEW"))
    }

    pub fn exec_drop(
        &mut self,
        object_type: &sqlparser::ast::ObjectType,
        if_exists: bool,
        names: &[sqlparser::ast::ObjectName],
        cascade: bool,
    ) -> Result<ExecResult> {
        use sqlparser::ast::ObjectType;
        for name in names {
            let (schema, n) = split_schema_table(name);
            match object_type {
                ObjectType::Table => {
                    match self.catalog.resolve_table_name(schema.as_deref(), &n) {
                        Some(q) => {
                            let table = self.catalog.drop_table_qualified(&q)?;
                            self.mutations.push(Mutation::Truncate { collection: table.storage_collection });
                        }
                        None if if_exists => {}
                        None => return Err(SqlError::UndefinedTable(n)),
                    }
                }
                ObjectType::View => {
                    let schema = schema.unwrap_or_else(|| "public".into());
                    self.catalog.drop_view(&QualifiedName::new(schema, n), if_exists)?;
                }
                ObjectType::Schema => {
                    self.catalog.drop_schema(&n, if_exists, cascade)?;
                }
                ObjectType::Index => {
                    self.catalog.drop_index(schema.as_deref(), &n, if_exists)?;
                }
                other => {
                    return Err(SqlError::FeatureNotSupported(format!(
                        "DROP {other:?} is not supported"
                    )))
                }
            }
        }
        self.catalog_dirty = true;
        let tag = match object_type {
            ObjectType::Table => "DROP TABLE",
            ObjectType::View => "DROP VIEW",
            ObjectType::Schema => "DROP SCHEMA",
            ObjectType::Index => "DROP INDEX",
            _ => "DROP",
        };
        Ok(ExecResult::empty_command(tag))
    }

    pub fn exec_truncate(&mut self, stmt: &Statement) -> Result<ExecResult> {
        if let Statement::Truncate(t) = stmt {
            for target in &t.table_names {
                let (schema, n) = split_schema_table(&target.name);
                let q = self
                    .catalog
                    .resolve_table_name(schema.as_deref(), &n)
                    .ok_or_else(|| SqlError::UndefinedTable(n.clone()))?;
                let collection = self.catalog.require_table(&q)?.storage_collection.clone();
                self.mutations.push(Mutation::Truncate { collection });
                if let Some(loaded) = self.tables.get_mut(&q) {
                    loaded.rows.clear();
                    loaded.rebuild_indexes();
                }
            }
        }
        Ok(ExecResult::empty_command("TRUNCATE TABLE"))
    }

    pub fn exec_alter_table(
        &mut self,
        name: &sqlparser::ast::ObjectName,
        operations: &[AlterTableOperation],
    ) -> Result<ExecResult> {
        let (schema, n) = split_schema_table(name);
        let q = self
            .catalog
            .resolve_table_name(schema.as_deref(), &n)
            .ok_or_else(|| SqlError::UndefinedTable(n.clone()))?;

        for op in operations {
            self.apply_alter_op(&q, op)?;
        }
        self.catalog_dirty = true;
        Ok(ExecResult::empty_command("ALTER TABLE"))
    }

    fn apply_alter_op(&mut self, q: &QualifiedName, op: &AlterTableOperation) -> Result<()> {
        match op {
            AlterTableOperation::AddColumn { column_def, if_not_exists, .. } => {
                let ordinal = self.catalog.require_table(q)?.columns.len();
                let mut pk = Vec::new();
                let mut uniques = Vec::new();
                let mut fks = Vec::new();
                let mut checks = Vec::new();
                let mut seqs = Vec::new();
                let column = self.build_column(&q.schema, &q.name, column_def, ordinal, &mut seqs, &mut pk, &mut uniques, &mut fks, &mut checks)?;
                let table = self.catalog.get_table_mut(q).unwrap();
                if table.column(&column.name).is_some() {
                    if *if_not_exists {
                        return Ok(());
                    }
                    return Err(SqlError::DuplicateColumn(column.name.clone(), q.name.clone()));
                }
                table.columns.push(column);
            }
            AlterTableOperation::DropColumn { column_names, if_exists, .. } => {
                let table = self.catalog.get_table_mut(q).unwrap();
                for column_name in column_names {
                    let cname = ident_name(column_name);
                    if table.column(&cname).is_none() {
                        if *if_exists {
                            continue;
                        }
                        return Err(SqlError::UndefinedColumn(cname));
                    }
                    table.columns.retain(|c| c.name != cname);
                    for (i, c) in table.columns.iter_mut().enumerate() {
                        c.ordinal = i;
                    }
                }
                let names: Vec<String> = column_names.iter().map(ident_name).collect();
                // Drop indexes referencing removed columns.
                let drop_idx: Vec<String> = self
                    .catalog
                    .indexes_for_table(&q.schema, &q.name)
                    .into_iter()
                    .filter(|i| i.columns.iter().any(|c| names.contains(c)))
                    .map(|i| i.name.clone())
                    .collect();
                for iname in drop_idx {
                    let _ = self.catalog.drop_index(Some(&q.schema), &iname, true);
                }
            }
            AlterTableOperation::RenameColumn { old_column_name, new_column_name } => {
                let old = ident_name(old_column_name);
                let new = ident_name(new_column_name);
                self.rename_column(q, &old, &new)?;
            }
            AlterTableOperation::AlterColumn { column_name, op } => {
                let cname = ident_name(column_name);
                let table = self.catalog.get_table_mut(q).unwrap();
                let col = table
                    .column_mut(&cname)
                    .ok_or_else(|| SqlError::UndefinedColumn(cname.clone()))?;
                match op {
                    AlterColumnOperation::SetNotNull => col.nullable = false,
                    AlterColumnOperation::DropNotNull => col.nullable = true,
                    AlterColumnOperation::SetDefault { value } => col.default = Some(value.to_string()),
                    AlterColumnOperation::DropDefault => col.default = None,
                    AlterColumnOperation::SetDataType { data_type, .. } => {
                        col.ty = crate::eval::parse_data_type(data_type)?;
                    }
                    other => {
                        return Err(SqlError::FeatureNotSupported(format!(
                            "ALTER COLUMN operation not supported: {other}"
                        )))
                    }
                }
            }
            AlterTableOperation::RenameTable { table_name } => {
                let object_name = match table_name {
                    sqlparser::ast::RenameTableNameKind::As(n)
                    | sqlparser::ast::RenameTableNameKind::To(n) => n,
                };
                let (_s, new_name) = split_schema_table(object_name);
                let mut table = self.catalog.drop_table_qualified(q)?;
                // Preserve storage + indexes by re-inserting under the new name.
                table.name = new_name.clone();
                let new_q = QualifiedName::new(q.schema.clone(), new_name.clone());
                let cols = table.pk_columns();
                let pk = table.primary_key.clone();
                let uniques = table.uniques.clone();
                self.catalog.insert_table(table)?;
                if let Some(pk) = pk {
                    let oid = self.catalog.allocate_oid();
                    let _ = self.catalog.insert_index(Index {
                        oid, name: pk.name, schema: new_q.schema.clone(), table: new_name.clone(),
                        columns: cols, unique: true, primary: true, method: "btree".into(),
                    });
                }
                for u in uniques {
                    let oid = self.catalog.allocate_oid();
                    let _ = self.catalog.insert_index(Index {
                        oid, name: format!("{new_name}_{}_key", u.columns.join("_")),
                        schema: new_q.schema.clone(), table: new_name.clone(),
                        columns: u.columns, unique: true, primary: false, method: "btree".into(),
                    });
                }
            }
            AlterTableOperation::AddConstraint { constraint, .. } => {
                let mut pk = Vec::new();
                let mut uniques = Vec::new();
                let mut fks = Vec::new();
                let mut checks = Vec::new();
                self.apply_table_constraint(constraint, &mut pk, &mut uniques, &mut fks, &mut checks)?;
                let table = self.catalog.get_table_mut(q).unwrap();
                if !pk.is_empty() {
                    table.primary_key = Some(PrimaryKey { name: format!("{}_pkey", q.name), columns: pk.clone() });
                }
                table.uniques.extend(uniques.clone());
                table.foreign_keys.extend(fks);
                table.checks.extend(checks);
                if !pk.is_empty() {
                    let oid = self.catalog.allocate_oid();
                    let _ = self.catalog.insert_index(Index {
                        oid, name: format!("{}_pkey", q.name), schema: q.schema.clone(),
                        table: q.name.clone(), columns: pk, unique: true, primary: true, method: "btree".into(),
                    });
                }
                for u in uniques {
                    let oid = self.catalog.allocate_oid();
                    let iname = if u.name.is_empty() { format!("{}_{}_key", q.name, u.columns.join("_")) } else { u.name.clone() };
                    let _ = self.catalog.insert_index(Index {
                        oid, name: iname, schema: q.schema.clone(), table: q.name.clone(),
                        columns: u.columns, unique: true, primary: false, method: "btree".into(),
                    });
                }
            }
            AlterTableOperation::DropConstraint { name, if_exists, .. } => {
                let cname = ident_name(name);
                let _ = self.catalog.drop_index(Some(&q.schema), &cname, true);
                let table = self.catalog.get_table_mut(q).unwrap();
                table.uniques.retain(|u| u.name != cname);
                table.foreign_keys.retain(|f| f.name != cname);
                table.checks.retain(|c| c.name != cname);
                if table.primary_key.as_ref().map(|p| p.name == cname).unwrap_or(false) {
                    table.primary_key = None;
                }
                let _ = if_exists;
            }
            other => {
                return Err(SqlError::FeatureNotSupported(format!(
                    "ALTER TABLE operation not supported: {other}"
                )))
            }
        }
        Ok(())
    }

    /// Rename a column in the catalog and rewrite stored rows.
    fn rename_column(&mut self, q: &QualifiedName, old: &str, new: &str) -> Result<()> {
        {
            let table = self.catalog.get_table_mut(q).unwrap();
            let col = table
                .column_mut(old)
                .ok_or_else(|| SqlError::UndefinedColumn(old.to_string()))?;
            col.name = new.to_string();
            if let Some(pk) = &mut table.primary_key {
                for c in &mut pk.columns {
                    if c == old {
                        *c = new.to_string();
                    }
                }
            }
            for u in &mut table.uniques {
                for c in &mut u.columns {
                    if c == old {
                        *c = new.to_string();
                    }
                }
            }
        }
        // Update index metadata.
        let idx_names: Vec<String> = self
            .catalog
            .indexes_for_table(&q.schema, &q.name)
            .into_iter()
            .map(|i| i.name.clone())
            .collect();
        for iname in idx_names {
            if let Some(idx) = self.catalog.get_index(&QualifiedName::new(q.schema.clone(), iname.clone())).cloned() {
                let mut idx = idx;
                for c in &mut idx.columns {
                    if c == old {
                        *c = new.to_string();
                    }
                }
                // Re-insert (drop + insert) to update.
                let _ = self.catalog.drop_index(Some(&q.schema), &iname, true);
                let _ = self.catalog.insert_index(idx);
            }
        }
        // Rewrite stored rows: rename the key in each row document.
        if let Some(loaded) = self.tables.get_mut(q) {
            let collection = loaded.meta.storage_collection.clone();
            let table_meta = self.catalog.require_table(q)?.clone();
            let mut renamed_rows = Vec::new();
            for (rid, values) in loaded.rows.iter_mut() {
                if let Some(v) = values.remove(old) {
                    values.insert(new.to_string(), v);
                }
                renamed_rows.push((rid.clone(), values.clone()));
            }
            for (rid, values) in renamed_rows {
                let version = loaded.version_of(&rid) + 1;
                let doc = encode_row(&table_meta, &rid, &values, version);
                self.mutations.push(Mutation::Put { collection: collection.clone(), row_id: rid, doc });
            }
        }
        Ok(())
    }
}

/// Extract a column name from an index column (must be a plain identifier).
pub fn index_column_name(ic: &sqlparser::ast::IndexColumn) -> Result<String> {
    match &ic.column.expr {
        sqlparser::ast::Expr::Identifier(ident) => Ok(ident_name(ident)),
        other => Err(SqlError::FeatureNotSupported(format!(
            "index on expression not supported: {other}"
        ))),
    }
}

fn map_action(action: Option<sqlparser::ast::ReferentialAction>) -> ReferentialAction {
    match action {
        Some(sqlparser::ast::ReferentialAction::Cascade) => ReferentialAction::Cascade,
        Some(sqlparser::ast::ReferentialAction::Restrict) => ReferentialAction::Restrict,
        Some(sqlparser::ast::ReferentialAction::SetNull) => ReferentialAction::SetNull,
        Some(sqlparser::ast::ReferentialAction::SetDefault) => ReferentialAction::SetDefault,
        _ => ReferentialAction::NoAction,
    }
}

/// Helper trait to detect a column-level UNIQUE that is actually a PRIMARY KEY.
trait UniqueKind {
    fn is_primary_via_kind(&self) -> bool;
}
impl UniqueKind for sqlparser::ast::UniqueConstraint {
    fn is_primary_via_kind(&self) -> bool {
        false
    }
}
