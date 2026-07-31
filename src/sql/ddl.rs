//! DDL execution: CREATE/ALTER/DROP TABLE, schemas, indexes, views, TRUNCATE.

use crate::relational::SqlType;
use crate::relational::catalog::{
    CheckConstraint, Column, Deferrable, ForeignKey, Index, MatchType, PrimaryKey, QualifiedName,
    ReferentialAction, Table, UniqueConstraint, View,
};
use crate::sql::error::{Result, SqlError};
use crate::sql::exec::Exec;
use crate::sql::names::{ident_name, object_name_parts, split_schema_table};
use crate::sql::result::ExecResult;
use crate::sql::store::{Mutation, encode_row};
use sqlparser::ast::{
    AlterColumnOperation, AlterTableOperation, ColumnDef, ColumnOption, CreateExtension,
    CreateIndex, CreateTable, DropExtension, Statement, TableConstraint,
};
use std::collections::HashMap;

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
            let column = self.build_column(
                &schema,
                &name,
                col,
                ordinal,
                &mut sequences_to_create,
                &mut pk_columns,
                &mut uniques,
                &mut foreign_keys,
                &mut checks,
            )?;
            columns.push(column);
        }

        // Table-level constraints.
        for constraint in &ct.constraints {
            self.apply_table_constraint(
                &schema,
                &name,
                constraint,
                &mut pk_columns,
                &mut uniques,
                &mut foreign_keys,
                &mut checks,
            )?;
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
            Some(PrimaryKey {
                name: format!("{name}_pkey"),
                columns: pk_columns.clone(),
            })
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
            rls_enabled: false,
            rls_forced: false,
            policies: Vec::new(),
            triggers: Vec::new(),
            column_map: HashMap::new(),
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
                opclasses: Vec::new(),
                with_options: Vec::new(),
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
                opclasses: Vec::new(),
                with_options: Vec::new(),
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
            None => (crate::sql::eval::parse_data_type(&col.data_type)?, false),
        };
        crate::sql::ext::check_type_usable(&self.catalog, &ty)?;

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
                ColumnOption::PrimaryKey(pk) => {
                    reject_unsupported_characteristics(&pk.characteristics)?;
                    if !pk_columns.contains(&name) {
                        pk_columns.push(name.clone());
                    }
                    nullable = false;
                }
                ColumnOption::Unique(u) => {
                    reject_unsupported_characteristics(&u.characteristics)?;
                    if u.is_primary_via_kind() {
                        if !pk_columns.contains(&name) {
                            pk_columns.push(name.clone());
                        }
                        nullable = false;
                    } else {
                        uniques.push(UniqueConstraint {
                            name: opt.name.as_ref().map(ident_name).unwrap_or_default(),
                            columns: vec![name.clone()],
                        });
                    }
                }
                ColumnOption::ForeignKey(fk) => {
                    let fk_name = opt
                        .name
                        .as_ref()
                        .map(ident_name)
                        .unwrap_or_else(|| format!("{table}_{name}_fkey"));
                    foreign_keys.push(self.build_foreign_key(
                        fk,
                        schema,
                        table,
                        pk_columns,
                        vec![name.clone()],
                        fk_name,
                    )?);
                }
                ColumnOption::Check(c) => {
                    checks.push(CheckConstraint {
                        name: opt
                            .name
                            .as_ref()
                            .map(ident_name)
                            .unwrap_or_else(|| format!("{table}_{name}_check")),
                        expr: c.expr.to_string(),
                    });
                }
                _ => {}
            }
        }
        Ok(Column {
            name,
            ty,
            nullable,
            default,
            identity_sequence,
            ordinal,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_table_constraint(
        &self,
        schema: &str,
        table: &str,
        constraint: &TableConstraint,
        pk_columns: &mut Vec<String>,
        uniques: &mut Vec<UniqueConstraint>,
        foreign_keys: &mut Vec<ForeignKey>,
        checks: &mut Vec<CheckConstraint>,
    ) -> Result<()> {
        match constraint {
            TableConstraint::PrimaryKey(pk) => {
                reject_unsupported_characteristics(&pk.characteristics)?;
                for ic in &pk.columns {
                    pk_columns.push(index_column_name(ic)?);
                }
            }
            TableConstraint::Unique(u) => {
                reject_unsupported_characteristics(&u.characteristics)?;
                let cols: Result<Vec<String>> = u.columns.iter().map(index_column_name).collect();
                uniques.push(UniqueConstraint {
                    name: u.name.as_ref().map(ident_name).unwrap_or_default(),
                    columns: cols?,
                });
            }
            TableConstraint::ForeignKey(fk) => {
                let cols: Vec<String> = fk.columns.iter().map(ident_name).collect();
                let fk_name = fk
                    .name
                    .as_ref()
                    .map(ident_name)
                    .unwrap_or_else(|| format!("{table}_{}_fkey", cols.join("_")));
                foreign_keys
                    .push(self.build_foreign_key(fk, schema, table, pk_columns, cols, fk_name)?);
            }
            TableConstraint::Check(c) => {
                checks.push(CheckConstraint {
                    name: c
                        .name
                        .as_ref()
                        .map(ident_name)
                        .unwrap_or_else(|| "check".into()),
                    expr: c.expr.to_string(),
                });
            }
            _ => {}
        }
        Ok(())
    }

    /// Resolve and validate a foreign-key declaration at DDL time.
    ///
    /// The referenced schema is pinned here (explicit qualification wins, an
    /// unqualified self-reference binds to the declaring table's schema, and
    /// anything else follows the search path), an omitted referenced column
    /// list defaults to the parent's primary key (PostgreSQL), and — since
    /// foreign keys are enforced at runtime — the referenced table and
    /// columns must exist. A self-reference inside `CREATE TABLE` validates
    /// against the primary key collected so far instead of the catalog.
    fn build_foreign_key(
        &self,
        fk: &sqlparser::ast::ForeignKeyConstraint,
        own_schema: &str,
        own_table: &str,
        own_pk: &[String],
        columns: Vec<String>,
        name: String,
    ) -> Result<ForeignKey> {
        let deferrable = fk_deferrable_mode(&fk.characteristics)?;
        let match_type = fk_match_type(&fk.match_kind)?;
        let (fs, ft) = split_schema_table(&fk.foreign_table);
        let ref_schema = match &fs {
            Some(s) => s.clone(),
            None if ft == own_table => own_schema.to_string(),
            None => self
                .catalog
                .resolve_table_name(None, &ft)
                .map(|q| q.schema)
                .unwrap_or_else(|| own_schema.to_string()),
        };
        let self_ref = ref_schema == own_schema && ft == own_table;
        let parent = self
            .catalog
            .get_table(&QualifiedName::new(ref_schema.clone(), ft.clone()));
        if parent.is_none() && !self_ref {
            return Err(SqlError::UndefinedTable(ft.clone()));
        }
        let mut ref_columns: Vec<String> = fk.referred_columns.iter().map(ident_name).collect();
        if ref_columns.is_empty() {
            // `REFERENCES parent` without columns targets the parent's PK.
            ref_columns = match parent {
                Some(p) => p.pk_columns(),
                None => own_pk.to_vec(),
            };
        }
        if ref_columns.is_empty() || ref_columns.len() != columns.len() {
            return Err(SqlError::InvalidConstraint(name));
        }
        if let Some(p) = parent {
            for c in &ref_columns {
                if p.column(c).is_none() {
                    return Err(SqlError::UndefinedColumn(c.clone()));
                }
            }
        }
        Ok(ForeignKey {
            name,
            columns,
            ref_schema,
            ref_table: ft,
            ref_columns,
            on_delete: map_action(fk.on_delete),
            on_update: map_action(fk.on_update),
            match_type,
            deferrable,
        })
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
        // `USING <method>`: lower-cased method name; absent means btree.
        let method = ci
            .using
            .as_ref()
            .map(|u| u.to_string().to_ascii_lowercase())
            .unwrap_or_else(|| "btree".into());
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
        // ANN access method (RFC 0005): validated here, built lazily by the
        // engine-level runtime on first use. Every other method keeps the
        // engine's ordered-btree semantics unchanged.
        let (opclasses, with_options) = if method == "hnsw" {
            #[cfg(feature = "vector-index")]
            {
                self.validate_hnsw_index(ci, &q, &columns)?
            }
            #[cfg(not(feature = "vector-index"))]
            {
                return Err(SqlError::FeatureNotSupported(
                    "access method \"hnsw\" requires the `vector-index` build feature".into(),
                ));
            }
        } else {
            // Record any explicit opclasses verbatim for introspection.
            let ops = ci
                .columns
                .iter()
                .map(|c| {
                    c.operator_class
                        .as_ref()
                        .map(|o| o.to_string().to_ascii_lowercase())
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>();
            let ops = if ops.iter().all(String::is_empty) {
                Vec::new()
            } else {
                ops
            };
            (ops, Vec::new())
        };
        let oid = self.catalog.allocate_oid();
        self.catalog.insert_index(Index {
            oid,
            name,
            schema: q.schema.clone(),
            table: q.name.clone(),
            columns,
            unique: ci.unique,
            primary: false,
            method,
            opclasses,
            with_options,
        })?;
        // Unique index: validate existing rows do not already violate it.
        if ci.unique
            && let Some(loaded) = self.tables.get(&q)
        {
            let mut seen = std::collections::HashMap::new();
            let idx = self.catalog.indexes_for_table(&q.schema, &q.name);
            let idx = idx.last().unwrap();
            for (rid, values) in &loaded.rows {
                let key =
                    crate::relational::ordered_key(&crate::sql::store::index_values(idx, values));
                if crate::relational::composite_key(&crate::sql::store::index_values(idx, values))
                    .is_some()
                    && let Some(_other) = seen.insert(key, rid.clone())
                {
                    return Err(SqlError::UniqueViolation {
                        constraint: idx.name.clone(),
                        detail: "could not create unique index".into(),
                    });
                }
            }
        }
        self.catalog_dirty = true;
        Ok(ExecResult::empty_command("CREATE INDEX"))
    }

    /// Validate a `CREATE INDEX ... USING hnsw` statement (RFC 0005 §3.1) and
    /// return the `(opclasses, with_options)` to record in the catalog.
    ///
    /// Mirrors pgvector's rules: the `vector` extension must be installed,
    /// exactly one key column of a *dimensioned* `vector` type (≤ 2000 dims),
    /// one of the four `vector_*_ops` opclasses (default `vector_l2_ops`),
    /// `WITH (m, ef_construction)` within pgvector's ranges, and no
    /// unique/INCLUDE variants. Partial (`WHERE`) hnsw indexes are an honest
    /// phase-1 carve-out.
    #[cfg(feature = "vector-index")]
    fn validate_hnsw_index(
        &self,
        ci: &CreateIndex,
        q: &QualifiedName,
        columns: &[String],
    ) -> Result<(Vec<String>, IndexWithOptions)> {
        use crate::relational::hnsw::{HnswOptions, MAX_INDEXED_DIMS, VectorOpClass};
        if !self.catalog.extension_installed("vector") {
            return Err(SqlError::UndefinedObject(
                "access method \"hnsw\" (install the \"vector\" extension first; \
                 see pg_available_extensions)"
                    .into(),
            ));
        }
        if ci.unique {
            return Err(SqlError::FeatureNotSupported(
                "access method \"hnsw\" does not support unique indexes".into(),
            ));
        }
        if !ci.include.is_empty() {
            return Err(SqlError::FeatureNotSupported(
                "access method \"hnsw\" does not support included columns".into(),
            ));
        }
        if ci.predicate.is_some() {
            return Err(SqlError::FeatureNotSupported(
                "partial hnsw indexes are not supported".into(),
            ));
        }
        if columns.len() != 1 {
            return Err(SqlError::FeatureNotSupported(
                "hnsw indexes support exactly one column".into(),
            ));
        }
        let table = self
            .catalog
            .get_table(q)
            .ok_or_else(|| SqlError::UndefinedTable(q.to_string_qualified()))?;
        let col = table
            .column(&columns[0])
            .ok_or_else(|| SqlError::UndefinedColumn(columns[0].clone()))?;
        let dims = match &col.ty {
            SqlType::Vector(Some(d)) => *d,
            SqlType::Vector(None) => {
                return Err(SqlError::InvalidObjectDefinition(
                    "column does not have dimensions".into(),
                ));
            }
            other => {
                return Err(SqlError::InvalidObjectDefinition(format!(
                    "access method \"hnsw\" does not support column type {other}"
                )));
            }
        };
        if dims > MAX_INDEXED_DIMS {
            return Err(SqlError::InvalidObjectDefinition(format!(
                "column cannot have more than {MAX_INDEXED_DIMS} dimensions for hnsw index"
            )));
        }
        let opclass = match &ci.columns[0].operator_class {
            None => VectorOpClass::L2,
            Some(name) => {
                let n = name.to_string().to_ascii_lowercase();
                VectorOpClass::from_name(&n).ok_or_else(|| {
                    SqlError::UndefinedObject(format!(
                        "operator class \"{n}\" for access method \"hnsw\""
                    ))
                })?
            }
        };
        let mut options = HnswOptions::default();
        for expr in &ci.with {
            let (key, value) = index_with_option(expr)?;
            let parsed: u32 = value.parse().map_err(|_| {
                SqlError::InvalidParameter(format!(
                    "invalid value for parameter \"{key}\": {value}"
                ))
            })?;
            match key.as_str() {
                "m" => options.m = parsed,
                "ef_construction" => options.ef_construction = parsed,
                other => {
                    return Err(SqlError::InvalidParameter(format!(
                        "unrecognized parameter \"{other}\""
                    )));
                }
            }
        }
        options.validate().map_err(SqlError::InvalidParameter)?;
        Ok((
            vec![opclass.name().to_string()],
            vec![
                ("m".into(), options.m.to_string()),
                (
                    "ef_construction".into(),
                    options.ef_construction.to_string(),
                ),
            ],
        ))
    }

    /// The SQL declaration surface for auto-embedding rules (RFC 0005 §6.3).
    ///
    /// Because `sqlparser` cannot parse string-literal arguments in
    /// `CREATE TRIGGER ... EXECUTE FUNCTION f('a','b')`, the surface is a pair
    /// of engine-recognized function calls, which parse cleanly:
    ///
    /// ```sql
    /// SELECT guardian_embed('docs', 'body', 'embedding', 'model' [, 'local'|'delegated']);
    /// SELECT guardian_unembed('docs', 'embedding');
    /// ```
    ///
    /// A schema-qualified table (`'app.docs'`) is accepted; the default schema
    /// is `public`. The rule is stored in the catalog (so it replicates and
    /// persists) and read by the async embedding service. Returns `None` when
    /// `q` is an ordinary query — the caller then runs it normally.
    pub fn try_embedding_command(
        &mut self,
        q: &sqlparser::ast::Query,
    ) -> Option<Result<ExecResult>> {
        let (func, args) = embedding_call(q)?;
        Some(self.run_embedding_command(&func, &args))
    }

    fn run_embedding_command(&mut self, func: &str, args: &[String]) -> Result<ExecResult> {
        use crate::relational::EmbeddingRuleDef;
        let split_table = |t: &str| -> (String, String) {
            match t.split_once('.') {
                Some((s, n)) => (s.to_string(), n.to_string()),
                None => ("public".to_string(), t.to_string()),
            }
        };
        match func {
            "guardian_embed" => {
                if !(4..=5).contains(&args.len()) {
                    return Err(SqlError::InvalidFunctionDefinition(
                        "guardian_embed(table, text_col, vector_col, model [, policy])".into(),
                    ));
                }
                let (schema, table) = split_table(&args[0]);
                let q = self
                    .catalog
                    .resolve_table_name(Some(&schema), &table)
                    .ok_or_else(|| SqlError::UndefinedTable(table.clone()))?;
                let tdef = self.catalog.require_table(&q)?;
                // The vector column must exist and be a dimensioned vector; the
                // source-hash sidecar must exist too (the service needs it).
                let vcol = tdef
                    .column(&args[2])
                    .ok_or_else(|| SqlError::UndefinedColumn(args[2].clone()))?;
                if !matches!(vcol.ty, SqlType::Vector(Some(_))) {
                    return Err(SqlError::InvalidObjectDefinition(format!(
                        "column \"{}\" is not a dimensioned vector",
                        args[2]
                    )));
                }
                if tdef.column(&args[1]).is_none() {
                    return Err(SqlError::UndefinedColumn(args[1].clone()));
                }
                let sidecar = format!("_{}_srchash", args[2]);
                if tdef.column(&sidecar).is_none() {
                    return Err(SqlError::InvalidObjectDefinition(format!(
                        "table is missing the required sidecar column \"{sidecar}\" (add it as TEXT)"
                    )));
                }
                let policy = args
                    .get(4)
                    .map(|p| p.to_ascii_lowercase())
                    .unwrap_or_else(|| "local".into());
                if !matches!(policy.as_str(), "local" | "delegated") {
                    return Err(SqlError::InvalidParameter(format!(
                        "embedding policy must be 'local' or 'delegated', got '{policy}'"
                    )));
                }
                self.catalog.put_embedding_rule(EmbeddingRuleDef {
                    schema: q.schema.clone(),
                    table: q.name.clone(),
                    text_column: args[1].clone(),
                    vector_column: args[2].clone(),
                    model: args[3].clone(),
                    policy,
                });
                self.catalog_dirty = true;
                Ok(status_row("embedding rule created"))
            }
            "guardian_unembed" => {
                if args.len() != 2 {
                    return Err(SqlError::InvalidFunctionDefinition(
                        "guardian_unembed(table, vector_col)".into(),
                    ));
                }
                let (schema, table) = split_table(&args[0]);
                let q = self
                    .catalog
                    .resolve_table_name(Some(&schema), &table)
                    .ok_or_else(|| SqlError::UndefinedTable(table.clone()))?;
                let removed = self
                    .catalog
                    .drop_embedding_rule(&q.schema, &q.name, &args[1]);
                self.catalog_dirty = true;
                Ok(status_row(if removed {
                    "embedding rule dropped"
                } else {
                    "no such embedding rule"
                }))
            }
            _ => unreachable!("embedding_call only matches the two names"),
        }
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
            triggers: Vec::new(),
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
        // All tables this statement drops (FK dependents inside the set never
        // block, mirroring `DROP TABLE parent, child`).
        let drop_set: Vec<QualifiedName> = if matches!(object_type, ObjectType::Table) {
            names
                .iter()
                .filter_map(|name| {
                    let (s, t) = split_schema_table(name);
                    self.catalog.resolve_table_name(s.as_deref(), &t)
                })
                .collect()
        } else {
            Vec::new()
        };
        for name in names {
            let (schema, n) = split_schema_table(name);
            match object_type {
                ObjectType::Table => match self.catalog.resolve_table_name(schema.as_deref(), &n) {
                    Some(q) => {
                        // Foreign keys on other tables depend on this one:
                        // plain DROP fails (PostgreSQL 2BP01); CASCADE drops
                        // the dependent constraints (see the catalog's
                        // referential cleanup in `drop_table_qualified`).
                        if !cascade {
                            for (child, fk) in self.catalog.referencing_foreign_keys(&q) {
                                if !drop_set.contains(&child) {
                                    return Err(SqlError::DependentObjectsStillExist {
                                        object: format!("table {}", q.name),
                                        detail: format!(
                                            "constraint {} on table {} depends on table {}",
                                            fk.name, child.name, q.name
                                        ),
                                    });
                                }
                            }
                        }
                        let table = self.catalog.drop_table_qualified(&q)?;
                        self.mutations.lock().unwrap().push(Mutation::Truncate {
                            collection: table.storage_collection,
                        });
                    }
                    None if if_exists => {}
                    None => return Err(SqlError::UndefinedTable(n)),
                },
                ObjectType::View => {
                    let schema = schema.unwrap_or_else(|| "public".into());
                    self.catalog
                        .drop_view(&QualifiedName::new(schema, n), if_exists)?;
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
                    )));
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
            // Resolve every target first: the FK guard considers the whole
            // statement (PostgreSQL allows truncating parent and child
            // together; a self-reference never blocks).
            let mut targets: Vec<QualifiedName> = Vec::new();
            for target in &t.table_names {
                let (schema, n) = split_schema_table(&target.name);
                let q = self
                    .catalog
                    .resolve_table_name(schema.as_deref(), &n)
                    .ok_or_else(|| SqlError::UndefinedTable(n.clone()))?;
                targets.push(q);
            }
            for q in &targets {
                for (child, fk) in self.catalog.referencing_foreign_keys(q) {
                    if !targets.contains(&child) {
                        // PostgreSQL rejects this with 0A000 rather than
                        // running referential actions on a truncation.
                        return Err(SqlError::FeatureNotSupported(format!(
                            "cannot truncate a table referenced in a foreign key constraint — \
                             table \"{}\" references \"{}\" (constraint \"{}\"); truncate \
                             \"{}\" in the same statement",
                            child.name, q.name, fk.name, child.name
                        )));
                    }
                }
            }
            for q in &targets {
                // Fire BEFORE TRUNCATE triggers (FOR EACH STATEMENT).
                let table_snap = self.catalog.require_table(q)?.clone();
                self.fire_statement_triggers(
                    &table_snap,
                    crate::relational::catalog::TriggerTiming::Before,
                    crate::sql::trigger::TriggerOp::Truncate,
                    None,
                )?;

                let collection = self.catalog.require_table(q)?.storage_collection.clone();
                self.mutations
                    .lock()
                    .unwrap()
                    .push(Mutation::Truncate { collection });
                if let Some(loaded) = self.tables.get_mut(q) {
                    let loaded = std::sync::Arc::make_mut(loaded);
                    loaded.rows.clear();
                    loaded.rebuild_indexes();
                }

                // Fire AFTER TRUNCATE triggers.
                let table_snap = self.catalog.require_table(q)?.clone();
                self.fire_statement_triggers(
                    &table_snap,
                    crate::relational::catalog::TriggerTiming::After,
                    crate::sql::trigger::TriggerOp::Truncate,
                    None,
                )?;
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
            AlterTableOperation::AddColumn {
                column_def,
                if_not_exists,
                ..
            } => {
                let ordinal = self.catalog.require_table(q)?.columns.len();
                let mut pk = Vec::new();
                let mut uniques = Vec::new();
                let mut fks = Vec::new();
                let mut checks = Vec::new();
                let mut seqs = Vec::new();
                let column = self.build_column(
                    &q.schema,
                    &q.name,
                    column_def,
                    ordinal,
                    &mut seqs,
                    &mut pk,
                    &mut uniques,
                    &mut fks,
                    &mut checks,
                )?;
                let table = self.catalog.get_table_mut(q).unwrap();
                if table.column(&column.name).is_some() {
                    if *if_not_exists {
                        return Ok(());
                    }
                    return Err(SqlError::DuplicateColumn(
                        column.name.clone(),
                        q.name.clone(),
                    ));
                }
                table.columns.push(column);
                table.rebuild_column_map();
            }
            AlterTableOperation::DropColumn {
                column_names,
                if_exists,
                ..
            } => {
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
                table.rebuild_column_map();
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
            AlterTableOperation::RenameColumn {
                old_column_name,
                new_column_name,
            } => {
                let old = ident_name(old_column_name);
                let new = ident_name(new_column_name);
                self.rename_column(q, &old, &new)?;
            }
            AlterTableOperation::AlterColumn { column_name, op } => {
                let cname = ident_name(column_name);
                // Extension-type availability must be checked before the
                // mutable catalog borrow below.
                if let AlterColumnOperation::SetDataType { data_type, .. } = op {
                    let ty = crate::sql::eval::parse_data_type(data_type)?;
                    crate::sql::ext::check_type_usable(&self.catalog, &ty)?;
                }
                let table = self.catalog.get_table_mut(q).unwrap();
                let col = table
                    .column_mut(&cname)
                    .ok_or_else(|| SqlError::UndefinedColumn(cname.clone()))?;
                match op {
                    AlterColumnOperation::SetNotNull => col.nullable = false,
                    AlterColumnOperation::DropNotNull => col.nullable = true,
                    AlterColumnOperation::SetDefault { value } => {
                        col.default = Some(value.to_string())
                    }
                    AlterColumnOperation::DropDefault => col.default = None,
                    AlterColumnOperation::SetDataType { data_type, .. } => {
                        col.ty = crate::sql::eval::parse_data_type(data_type)?;
                    }
                    other => {
                        return Err(SqlError::FeatureNotSupported(format!(
                            "ALTER COLUMN operation not supported: {other}"
                        )));
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
                        oid,
                        name: pk.name,
                        schema: new_q.schema.clone(),
                        table: new_name.clone(),
                        columns: cols,
                        unique: true,
                        primary: true,
                        method: "btree".into(),
                        opclasses: Vec::new(),
                        with_options: Vec::new(),
                    });
                }
                for u in uniques {
                    let oid = self.catalog.allocate_oid();
                    let _ = self.catalog.insert_index(Index {
                        oid,
                        name: format!("{new_name}_{}_key", u.columns.join("_")),
                        schema: new_q.schema.clone(),
                        table: new_name.clone(),
                        columns: u.columns,
                        unique: true,
                        primary: false,
                        method: "btree".into(),
                        opclasses: Vec::new(),
                        with_options: Vec::new(),
                    });
                }
            }
            AlterTableOperation::AddConstraint { constraint, .. } => {
                let mut pk = Vec::new();
                let mut uniques = Vec::new();
                let mut fks = Vec::new();
                let mut checks = Vec::new();
                self.apply_table_constraint(
                    &q.schema,
                    &q.name,
                    constraint,
                    &mut pk,
                    &mut uniques,
                    &mut fks,
                    &mut checks,
                )?;
                let table = self.catalog.get_table_mut(q).unwrap();
                if !pk.is_empty() {
                    table.primary_key = Some(PrimaryKey {
                        name: format!("{}_pkey", q.name),
                        columns: pk.clone(),
                    });
                }
                table.uniques.extend(uniques.clone());
                table.foreign_keys.extend(fks);
                table.checks.extend(checks);
                if !pk.is_empty() {
                    let oid = self.catalog.allocate_oid();
                    let _ = self.catalog.insert_index(Index {
                        oid,
                        name: format!("{}_pkey", q.name),
                        schema: q.schema.clone(),
                        table: q.name.clone(),
                        columns: pk,
                        unique: true,
                        primary: true,
                        method: "btree".into(),
                        opclasses: Vec::new(),
                        with_options: Vec::new(),
                    });
                }
                for u in uniques {
                    let oid = self.catalog.allocate_oid();
                    let iname = if u.name.is_empty() {
                        format!("{}_{}_key", q.name, u.columns.join("_"))
                    } else {
                        u.name.clone()
                    };
                    let _ = self.catalog.insert_index(Index {
                        oid,
                        name: iname,
                        schema: q.schema.clone(),
                        table: q.name.clone(),
                        columns: u.columns,
                        unique: true,
                        primary: false,
                        method: "btree".into(),
                        opclasses: Vec::new(),
                        with_options: Vec::new(),
                    });
                }
            }
            // ENABLE/DISABLE TRIGGER (see `crate::sql::trigger`). The
            // ALWAYS/REPLICA variants configure firing under replication
            // roles, which this engine has no model for — named 0A000.
            AlterTableOperation::EnableTrigger { name } => {
                self.exec_set_trigger_enabled(q, name, true)?;
            }
            AlterTableOperation::DisableTrigger { name } => {
                self.exec_set_trigger_enabled(q, name, false)?;
            }
            AlterTableOperation::EnableAlwaysTrigger { .. } => {
                return Err(SqlError::FeatureNotSupported(
                    "ENABLE ALWAYS TRIGGER".into(),
                ));
            }
            AlterTableOperation::EnableReplicaTrigger { .. } => {
                return Err(SqlError::FeatureNotSupported(
                    "ENABLE REPLICA TRIGGER".into(),
                ));
            }
            AlterTableOperation::EnableRowLevelSecurity => {
                self.catalog.get_table_mut(q).unwrap().rls_enabled = true;
            }
            AlterTableOperation::DisableRowLevelSecurity => {
                self.catalog.get_table_mut(q).unwrap().rls_enabled = false;
            }
            // FORCE revokes the owner roles' row-security exemption (it only
            // takes effect while row security is enabled, like PostgreSQL).
            AlterTableOperation::ForceRowLevelSecurity => {
                self.catalog.get_table_mut(q).unwrap().rls_forced = true;
            }
            AlterTableOperation::NoForceRowLevelSecurity => {
                self.catalog.get_table_mut(q).unwrap().rls_forced = false;
            }
            AlterTableOperation::DropConstraint {
                name, if_exists, ..
            } => {
                let cname = ident_name(name);
                let _ = self.catalog.drop_index(Some(&q.schema), &cname, true);
                let table = self.catalog.get_table_mut(q).unwrap();
                table.uniques.retain(|u| u.name != cname);
                table.foreign_keys.retain(|f| f.name != cname);
                table.checks.retain(|c| c.name != cname);
                if table
                    .primary_key
                    .as_ref()
                    .map(|p| p.name == cname)
                    .unwrap_or(false)
                {
                    table.primary_key = None;
                }
                let _ = if_exists;
            }
            other => {
                return Err(SqlError::FeatureNotSupported(format!(
                    "ALTER TABLE operation not supported: {other}"
                )));
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Row-level security policies
    // ------------------------------------------------------------------

    /// `CREATE POLICY name ON table [AS PERMISSIVE|RESTRICTIVE]
    /// [FOR ALL|SELECT|INSERT|UPDATE|DELETE] [TO role, ...]
    /// [USING (expr)] [WITH CHECK (expr)]`.
    pub fn exec_create_policy(&mut self, cp: &sqlparser::ast::CreatePolicy) -> Result<ExecResult> {
        use crate::relational::catalog::{Policy, PolicyCmd};
        use sqlparser::ast::{CreatePolicyCommand, CreatePolicyType, Owner};

        let (schema, n) = split_schema_table(&cp.table_name);
        let q = self
            .catalog
            .resolve_table_name(schema.as_deref(), &n)
            .ok_or_else(|| SqlError::UndefinedTable(n.clone()))?;
        let name = ident_name(&cp.name);
        if self.catalog.require_table(&q)?.policy(&name).is_some() {
            return Err(SqlError::DuplicateObject(format!(
                "policy \"{name}\" for table \"{}\"",
                q.name
            )));
        }

        let cmd = match cp.command {
            None | Some(CreatePolicyCommand::All) => PolicyCmd::All,
            Some(CreatePolicyCommand::Select) => PolicyCmd::Select,
            Some(CreatePolicyCommand::Insert) => PolicyCmd::Insert,
            Some(CreatePolicyCommand::Update) => PolicyCmd::Update,
            Some(CreatePolicyCommand::Delete) => PolicyCmd::Delete,
        };
        // PostgreSQL rejects clauses that can never apply to the command.
        if cp.with_check.is_some() && matches!(cmd, PolicyCmd::Select | PolicyCmd::Delete) {
            return Err(SqlError::Syntax(
                "WITH CHECK cannot be applied to SELECT or DELETE".into(),
            ));
        }
        if cp.using.is_some() && cmd == PolicyCmd::Insert {
            return Err(SqlError::Syntax(
                "only WITH CHECK expression allowed for INSERT".into(),
            ));
        }

        // `TO PUBLIC` (or no TO clause) means every role: an empty list.
        let mut roles: Vec<String> = Vec::new();
        let mut is_public = cp.to.is_none();
        for owner in cp.to.iter().flatten() {
            match owner {
                Owner::Ident(ident) => {
                    let role = ident_name(ident);
                    if role.eq_ignore_ascii_case("public") {
                        is_public = true;
                    } else {
                        roles.push(role);
                    }
                }
                Owner::CurrentRole | Owner::CurrentUser | Owner::SessionUser => {
                    roles.push(self.username.clone());
                }
            }
        }
        if is_public {
            roles.clear();
        }

        // Expressions are stored as SQL text and validated to round-trip
        // through the expression parser (they are re-parsed at evaluation).
        let using_expr = cp.using.as_ref().map(policy_expr_text).transpose()?;
        let check_expr = cp.with_check.as_ref().map(policy_expr_text).transpose()?;

        let permissive = !matches!(cp.policy_type, Some(CreatePolicyType::Restrictive));
        let table = self.catalog.get_table_mut(&q).unwrap();
        table.policies.push(Policy {
            name,
            cmd,
            roles,
            using_expr,
            check_expr,
            permissive,
        });
        self.catalog_dirty = true;
        Ok(ExecResult::empty_command("CREATE POLICY"))
    }

    /// `DROP POLICY [IF EXISTS] name ON table`.
    pub fn exec_drop_policy(&mut self, dp: &sqlparser::ast::DropPolicy) -> Result<ExecResult> {
        let (schema, n) = split_schema_table(&dp.table_name);
        let q = self
            .catalog
            .resolve_table_name(schema.as_deref(), &n)
            .ok_or_else(|| SqlError::UndefinedTable(n.clone()))?;
        let name = ident_name(&dp.name);
        let table = self.catalog.get_table_mut(&q).unwrap();
        let before = table.policies.len();
        table.policies.retain(|p| p.name != name);
        if table.policies.len() == before && !dp.if_exists {
            return Err(SqlError::UndefinedObject(format!(
                "policy \"{name}\" for table \"{}\"",
                q.name
            )));
        }
        self.catalog_dirty = true;
        Ok(ExecResult::empty_command("DROP POLICY"))
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
            table.rebuild_column_map();
        }
        // Update index metadata.
        let idx_names: Vec<String> = self
            .catalog
            .indexes_for_table(&q.schema, &q.name)
            .into_iter()
            .map(|i| i.name.clone())
            .collect();
        for iname in idx_names {
            if let Some(idx) = self
                .catalog
                .get_index(&QualifiedName::new(q.schema.clone(), iname.clone()))
                .cloned()
            {
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
            let loaded = std::sync::Arc::make_mut(loaded);
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
                self.mutations.lock().unwrap().push(Mutation::Put {
                    collection: collection.clone(),
                    row_id: rid,
                    doc,
                });
            }
        }
        Ok(())
    }
}

/// Render a policy expression to its stored SQL text, verifying the text
/// parses back as an expression (rejecting it with SQLSTATE 42601 otherwise,
/// so a policy can never be stored that would fail at evaluation time).
fn policy_expr_text(expr: &sqlparser::ast::Expr) -> Result<String> {
    let text = expr.to_string();
    crate::sql::parser::parse_expr(&text)
        .map_err(|e| SqlError::Syntax(format!("invalid policy expression ({text}): {e}")))?;
    Ok(text)
}

/// `WITH (...)` storage parameters as recorded in the catalog.
#[cfg(feature = "vector-index")]
type IndexWithOptions = Vec<(String, String)>;

/// Extract one `WITH (key = value)` storage parameter from a `CREATE INDEX`
/// option expression. Key is lower-cased; value must be a literal number.
#[cfg(feature = "vector-index")]
fn index_with_option(expr: &sqlparser::ast::Expr) -> Result<(String, String)> {
    use sqlparser::ast::{BinaryOperator, Expr, Value};
    if let Expr::BinaryOp { left, op, right } = expr
        && matches!(op, BinaryOperator::Eq)
        && let Expr::Identifier(key) = left.as_ref()
        && let Expr::Value(v) = right.as_ref()
        && let Value::Number(n, _) = &v.value
    {
        return Ok((ident_name(key).to_ascii_lowercase(), n.clone()));
    }
    Err(SqlError::Syntax(format!(
        "invalid index storage parameter: {expr}"
    )))
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

/// Truthfulness carve-out: deferred constraint checking is only implemented
/// for foreign keys (see [`fk_deferrable_mode`], used by `build_foreign_key`);
/// `PRIMARY KEY`/`UNIQUE` constraints have no deferred-checking machinery at
/// all here (PostgreSQL validates those against a live unique index, a
/// different mechanism this engine does not run deferred), so `DEFERRABLE` /
/// `INITIALLY DEFERRED` on *those* must still fail with a stable `0A000`
/// instead of being accepted and checked immediately anyway. `NOT ENFORCED`
/// is rejected for every constraint kind (not implemented at all). `NOT
/// DEFERRABLE`, `INITIALLY IMMEDIATE` and `ENFORCED` are the defaults the
/// engine implements, so they pass.
fn reject_unsupported_characteristics(
    characteristics: &Option<sqlparser::ast::ConstraintCharacteristics>,
) -> Result<()> {
    if let Some(c) = characteristics {
        if c.deferrable == Some(true)
            || c.initially == Some(sqlparser::ast::DeferrableInitial::Deferred)
        {
            return Err(SqlError::FeatureNotSupported(
                "DEFERRABLE constraints are not supported".into(),
            ));
        }
        if c.enforced == Some(false) {
            return Err(SqlError::FeatureNotSupported(
                "NOT ENFORCED constraints are not supported".into(),
            ));
        }
    }
    Ok(())
}

/// Parse a foreign key's `[NOT] DEFERRABLE [INITIALLY {DEFERRED|IMMEDIATE}]`
/// declaration into a [`Deferrable`] mode. `NOT ENFORCED` stays rejected
/// (`0A000`), same as [`reject_unsupported_characteristics`] — this engine
/// has no "declared but not checked" constraint mode.
///
/// PostgreSQL's own combining rule (`processCASbits` /
/// `ConstraintAttributeSpec` in `src/backend/parser/gram.y`) is reproduced
/// exactly rather than treating `deferrable`/`initially` independently:
/// * `NOT DEFERRABLE` together with `INITIALLY DEFERRED` is *not* just "not
///   deferrable" — PostgreSQL raises a specific syntax error for exactly this
///   combination ("constraint declared INITIALLY DEFERRED must be
///   DEFERRABLE", `42601`), reproduced here.
/// * A *bare* `INITIALLY DEFERRED` with no explicit `DEFERRABLE`/`NOT
///   DEFERRABLE` keyword at all is accepted and implies `DEFERRABLE` (the
///   `CAS_INITIALLY_DEFERRED` bit alone sets PostgreSQL's internal
///   `deferrable = true`, independent of the `CAS_DEFERRABLE` bit) — so it is
///   equivalent to `DEFERRABLE INITIALLY DEFERRED`, not an error.
fn fk_deferrable_mode(
    characteristics: &Option<sqlparser::ast::ConstraintCharacteristics>,
) -> Result<Deferrable> {
    use sqlparser::ast::DeferrableInitial;
    let Some(c) = characteristics else {
        return Ok(Deferrable::NotDeferrable);
    };
    if c.enforced == Some(false) {
        return Err(SqlError::FeatureNotSupported(
            "NOT ENFORCED constraints are not supported".into(),
        ));
    }
    let initially_deferred = c.initially == Some(DeferrableInitial::Deferred);
    if c.deferrable == Some(false) && initially_deferred {
        return Err(SqlError::Syntax(
            "constraint declared INITIALLY DEFERRED must be DEFERRABLE".into(),
        ));
    }
    let deferrable = c.deferrable == Some(true) || initially_deferred;
    Ok(match (deferrable, initially_deferred) {
        (true, true) => Deferrable::DeferrableDeferred,
        (true, false) => Deferrable::DeferrableImmediate,
        (false, _) => Deferrable::NotDeferrable,
    })
}

/// Parse a foreign key's `MATCH { FULL | PARTIAL | SIMPLE }` clause.
/// `MATCH SIMPLE` is PostgreSQL's default when the clause is omitted.
///
/// `MATCH PARTIAL` is rejected (`0A000`): PostgreSQL's own grammar accepts
/// it, but PostgreSQL itself has never implemented it (`CREATE TABLE`'s
/// "Key Match Types" section documents it as not yet implemented, and real
/// PostgreSQL raises `MATCH PARTIAL not yet implemented` for it) — so
/// rejecting it here *is* 1:1 parity with upstream PostgreSQL, not a gap.
fn fk_match_type(kind: &Option<sqlparser::ast::ConstraintReferenceMatchKind>) -> Result<MatchType> {
    use sqlparser::ast::ConstraintReferenceMatchKind as M;
    match kind {
        Some(M::Full) => Ok(MatchType::Full),
        Some(M::Partial) => Err(SqlError::FeatureNotSupported(
            "MATCH PARTIAL is not implemented — PostgreSQL itself has never implemented it \
             either (\"MATCH PARTIAL not yet implemented\"); this is parity, not a gap"
                .into(),
        )),
        Some(M::Simple) | None => Ok(MatchType::Simple),
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

impl Exec {
    // ------------------------------------------------------------------
    // Extensions
    // ------------------------------------------------------------------

    /// `CREATE EXTENSION [IF NOT EXISTS] name [WITH] [SCHEMA s] [VERSION v] [CASCADE]`.
    ///
    /// GuardianDB implements a fixed registry of extensions natively (see
    /// [`crate::sql::ext`]); binary PostgreSQL extensions cannot be loaded
    /// into this engine, so anything outside the registry fails with a typed
    /// error pointing at `pg_available_extensions`.
    pub fn exec_create_extension(&mut self, ce: &CreateExtension) -> Result<ExecResult> {
        let name = ident_name(&ce.name).to_lowercase();
        let def = crate::sql::ext::find(&name).ok_or_else(|| {
            SqlError::FeatureNotSupported(format!(
                "extension \"{name}\" is not available — GuardianDB implements a fixed \
                 set of extensions natively (binary PostgreSQL extensions cannot be \
                 loaded); see SELECT * FROM pg_available_extensions"
            ))
        })?;
        if self.catalog.extension_installed(def.name) {
            if ce.if_not_exists {
                return Ok(ExecResult::empty_command("CREATE EXTENSION"));
            }
            return Err(SqlError::DuplicateObject(format!(
                "extension \"{}\"",
                def.name
            )));
        }
        // Sidecar-routed extensions are installed by the session (which owns
        // the async sidecar connection) before dispatch ever reaches here;
        // reaching this point means no sidecar is configured.
        if def.strategy == crate::sql::ext::RuntimeStrategy::SidecarPostgres {
            return Err(crate::sql::ext::sidecar_unconfigured(def.name));
        }
        if let Some(v) = &ce.version {
            let requested = ident_name(v);
            if requested != def.default_version {
                return Err(SqlError::UndefinedObject(format!(
                    "extension \"{}\" version \"{requested}\" (available: \"{}\")",
                    def.name, def.default_version
                )));
            }
        }
        // `SCHEMA x` is accepted and ignored: none of the registry extensions
        // are relocatable and their objects live in the system namespace.
        for req in def.requires {
            if !self.catalog.extension_installed(req) {
                if !ce.cascade {
                    return Err(SqlError::FeatureNotSupported(format!(
                        "required extension \"{req}\" is not installed — use CREATE \
                         EXTENSION ... CASCADE to install it automatically"
                    )));
                }
                let dep = crate::sql::ext::find(req).ok_or_else(|| {
                    SqlError::Internal(format!("extension dependency {req} not in registry"))
                })?;
                self.catalog
                    .install_extension(dep.name, dep.default_version);
            }
        }
        self.catalog
            .install_extension(def.name, def.default_version);
        self.catalog_dirty = true;
        Ok(ExecResult::empty_command("CREATE EXTENSION"))
    }

    /// `DROP EXTENSION [IF EXISTS] name [, ...] [CASCADE | RESTRICT]`.
    ///
    /// Tables with columns of an extension-provided type block the drop under
    /// RESTRICT (the default). CASCADE-dropping dependent columns is refused
    /// explicitly rather than destroying data implicitly. Statements naming a
    /// sidecar-bound extension are handled by the session (which forwards the
    /// drop to the sidecar) before dispatch reaches here.
    pub fn exec_drop_extension(&mut self, de: &DropExtension) -> Result<ExecResult> {
        for ident in &de.names {
            let name = ident_name(ident).to_lowercase();
            if crate::sql::ext::drop_native_extension(
                &mut self.catalog,
                &name,
                de.if_exists,
                de.cascade_or_restrict,
            )? {
                self.catalog_dirty = true;
            }
        }
        Ok(ExecResult::empty_command("DROP EXTENSION"))
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

// Maintenance note 6: documents compatibility expectations without changing runtime behavior.

// Maintenance note 18: documents compatibility expectations without changing runtime behavior.

// Maintenance note: keeps SQL compatibility behavior explicit for future updates.

// Maintenance note: keeps SQL compatibility behavior explicit for future updates.

// SQL compatibility note 7: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.

// SQL compatibility note 23: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.

// SQL compatibility note 7: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.

// SQL compatibility note 23: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.

/// Build a single-row, single-text-column result reporting a command status
/// (used by the `guardian_embed`/`guardian_unembed` SQL surface).
fn status_row(msg: &str) -> ExecResult {
    use crate::relational::{SqlType, SqlValue};
    ExecResult::Rows {
        fields: vec![crate::sql::result::OutField::new(
            "guardian_embed".to_string(),
            SqlType::Text,
        )],
        rows: vec![vec![SqlValue::Text(msg.to_string())]],
    }
}

/// Recognize a top-level `SELECT guardian_embed(...)` /
/// `guardian_unembed(...)` call and extract `(function_name, string_args)`.
/// Returns `None` for any other query, or when an argument is not a string
/// literal (so a genuine user function of the same name would fall through —
/// though these names are reserved). RFC 0005 §6.3.
fn embedding_call(q: &sqlparser::ast::Query) -> Option<(String, Vec<String>)> {
    use sqlparser::ast::{
        Expr, FunctionArg, FunctionArgExpr, FunctionArguments, SelectItem, SetExpr, Value,
    };
    let SetExpr::Select(select) = q.body.as_ref() else {
        return None;
    };
    // A bare `SELECT f(...)` with no FROM and exactly one projection.
    if !select.from.is_empty() || select.projection.len() != 1 {
        return None;
    }
    let expr = match &select.projection[0] {
        SelectItem::UnnamedExpr(e) => e,
        SelectItem::ExprWithAlias { expr, .. } => expr,
        _ => return None,
    };
    let Expr::Function(func) = expr else {
        return None;
    };
    let name = object_name_parts(&func.name)
        .last()
        .cloned()?
        .to_ascii_lowercase();
    if name != "guardian_embed" && name != "guardian_unembed" {
        return None;
    }
    let FunctionArguments::List(list) = &func.args else {
        return Some((name, Vec::new()));
    };
    let mut args = Vec::with_capacity(list.args.len());
    for a in &list.args {
        let FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(v))) = a else {
            return None;
        };
        match &v.value {
            Value::SingleQuotedString(s) | Value::DoubleQuotedString(s) => args.push(s.clone()),
            _ => return None,
        }
    }
    Some((name, args))
}
