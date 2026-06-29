//! The database engine: sessions, transactions, statement dispatch, and the
//! load → execute → commit lifecycle.

use crate::error::{Result, SqlError};
use crate::exec::Exec;
use crate::result::ExecResult;
use crate::store::{LoadedTable, Mutation};
use guardian_relational::catalog::QualifiedName;
use guardian_relational::{Catalog, RelationalStorage, SqlValue};
use serde_json::Value as Json;
use sqlparser::ast::{Query, Statement};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// A shared, storage-backed relational database.
pub struct Database<S: RelationalStorage> {
    storage: Arc<S>,
    pub name: String,
}

impl<S: RelationalStorage> Database<S> {
    pub fn new(storage: Arc<S>, name: impl Into<String>) -> Self {
        Self { storage, name: name.into() }
    }

    pub fn storage(&self) -> &Arc<S> {
        &self.storage
    }
}

/// An in-flight explicit transaction (BEGIN ... COMMIT/ROLLBACK).
struct Transaction {
    catalog: Catalog,
    catalog_dirty: bool,
    /// collection -> row_id -> Some(doc) (upsert) / None (delete)
    overlay: HashMap<String, HashMap<String, Option<Json>>>,
    truncated: HashSet<String>,
}

/// A connection-scoped session.
pub struct Session<S: RelationalStorage> {
    db: Arc<Database<S>>,
    username: String,
    txn: Option<Transaction>,
}

/// A parsed, reusable prepared statement.
#[derive(Clone)]
pub struct Prepared {
    pub sql: String,
    pub statement: Statement,
    pub param_count: usize,
}

impl<S: RelationalStorage> Session<S> {
    pub fn new(db: Arc<Database<S>>, username: impl Into<String>) -> Self {
        Self { db, username: username.into(), txn: None }
    }

    pub fn in_transaction(&self) -> bool {
        self.txn.is_some()
    }

    /// Parse and execute a (possibly multi-statement) SQL string.
    pub async fn execute(&mut self, sql: &str) -> Result<Vec<ExecResult>> {
        let statements = crate::parser::parse_sql(sql)?;
        let mut results = Vec::with_capacity(statements.len());
        for stmt in statements {
            results.push(self.execute_one(&stmt, &[]).await?);
        }
        Ok(results)
    }

    /// Prepare a statement for the extended query protocol.
    pub fn prepare(&self, sql: &str) -> Result<Prepared> {
        let mut statements = crate::parser::parse_sql(sql)?;
        let statement = match statements.len() {
            0 => Statement::Query(Box::new(empty_query())),
            1 => statements.remove(0),
            _ => {
                return Err(SqlError::Syntax(
                    "cannot insert multiple commands into a prepared statement".into(),
                ))
            }
        };
        let param_count = count_placeholders(sql);
        Ok(Prepared { sql: sql.to_string(), statement, param_count })
    }

    /// Execute one statement with bound parameters.
    pub async fn execute_one(&mut self, stmt: &Statement, params: &[SqlValue]) -> Result<ExecResult> {
        // Transaction control.
        match stmt {
            Statement::StartTransaction { .. } => return self.begin().await,
            Statement::Commit { .. } => return self.commit().await,
            Statement::Rollback { .. } => return self.rollback().await,
            _ => {}
        }

        // Load the working catalog (from txn snapshot or storage).
        let catalog = match &self.txn {
            Some(txn) => txn.catalog.clone(),
            None => self.load_catalog().await?,
        };

        // Preload referenced tables.
        let mut names = Vec::new();
        collect_stmt(stmt, &mut names);
        let mut tables: HashMap<QualifiedName, LoadedTable> = HashMap::new();
        for (schema, name) in &names {
            if let Some(q) = catalog.resolve_table_name(schema.as_deref(), name) {
                if !tables.contains_key(&q) {
                    if let Some(loaded) = self.load_table(&catalog, &q).await? {
                        tables.insert(q, loaded);
                    }
                }
            }
        }

        // Build the synchronous execution context and run.
        let now = chrono::Utc::now();
        let mut exec = Exec::new(catalog, tables, params.to_vec(), now, self.db.name.clone(), self.username.clone());
        // Pre-materialize top-level CTEs.
        if let Statement::Query(q) = stmt {
            if let Some(with) = &q.with {
                for cte in &with.cte_tables {
                    let name = crate::names::ident_name(&cte.alias.name);
                    let rs = exec.exec_select_query(&cte.query, &[])?;
                    let rs = relabel_cte(rs, &name);
                    exec.cte.insert(name, rs);
                }
            }
        }
        let result = self.dispatch(&mut exec, stmt)?;

        // Commit or stage the produced mutations / catalog changes.
        let mutations = std::mem::take(&mut exec.mutations);
        let catalog_dirty = exec.catalog_dirty;
        let new_catalog = exec.catalog;
        match &mut self.txn {
            Some(txn) => {
                txn.catalog = new_catalog;
                txn.catalog_dirty |= catalog_dirty;
                stage_mutations(txn, mutations);
            }
            None => {
                self.apply_mutations(mutations).await?;
                if catalog_dirty {
                    self.save_catalog(&new_catalog).await?;
                }
            }
        }
        Ok(result)
    }

    fn dispatch(&self, exec: &mut Exec, stmt: &Statement) -> Result<ExecResult> {
        match stmt {
            Statement::Query(q) => {
                let rs = exec.exec_select_query(q, &[])?;
                let fields = rs
                    .schema
                    .fields
                    .iter()
                    .map(|f| crate::result::OutField::new(f.name.clone(), f.ty.clone()))
                    .collect();
                Ok(ExecResult::Rows { fields, rows: rs.rows })
            }
            Statement::Insert(insert) => exec.exec_insert(insert),
            Statement::Update(update) => exec.exec_update(update),
            Statement::Delete(delete) => exec.exec_delete(delete),
            Statement::CreateTable(ct) => exec.exec_create_table(ct),
            Statement::CreateSchema { schema_name, if_not_exists, .. } => {
                let name = schema_name_to_string(schema_name);
                exec.exec_create_schema(&name, *if_not_exists)
            }
            Statement::CreateIndex(ci) => exec.exec_create_index(ci),
            Statement::CreateView(cv) => exec.exec_create_view(cv),
            Statement::AlterTable(alter) => exec.exec_alter_table(&alter.name, &alter.operations),
            Statement::Drop { object_type, if_exists, names, cascade, .. } => {
                exec.exec_drop(object_type, *if_exists, names, *cascade)
            }
            Statement::Truncate(_) => exec.exec_truncate(stmt),
            Statement::Set(_) => Ok(ExecResult::empty_command("SET")),
            other => self.dispatch_fallback(other),
        }
    }

    /// Handle utility statements (SET/SHOW/RESET/...) by inspecting the text.
    fn dispatch_fallback(&self, stmt: &Statement) -> Result<ExecResult> {
        let text = stmt.to_string();
        let mut words = text.split_whitespace();
        let first = words.next().unwrap_or("").to_ascii_uppercase();
        let second = words.next().unwrap_or("").to_ascii_uppercase();
        // Extension / sequence management is a no-op (sequences are managed
        // implicitly by serial columns; no extensions are required).
        if matches!(
            (first.as_str(), second.as_str()),
            ("CREATE", "EXTENSION")
                | ("DROP", "EXTENSION")
                | ("CREATE", "SEQUENCE")
                | ("ALTER", "SEQUENCE")
                | ("DROP", "SEQUENCE")
        ) {
            return Ok(ExecResult::empty_command(format!("{first} {second}")));
        }
        match first.as_str() {
            "SET" | "RESET" | "DISCARD" | "DEALLOCATE" | "LISTEN" | "UNLISTEN" | "CHECKPOINT"
            | "CLOSE" | "ANALYZE" | "VACUUM" | "COMMENT" | "GRANT" | "REVOKE" | "SAVEPOINT"
            | "RELEASE" | "PREPARE" | "EXECUTE" => Ok(ExecResult::empty_command(first)),
            "SHOW" => {
                let var = text.split_whitespace().nth(1).unwrap_or("").trim_end_matches(';').to_string();
                let value = show_value(&var);
                Ok(ExecResult::Rows {
                    fields: vec![crate::result::OutField::new(
                        if var.is_empty() { "show".to_string() } else { var },
                        guardian_relational::SqlType::Text,
                    )],
                    rows: vec![vec![SqlValue::Text(value)]],
                })
            }
            _ => Err(SqlError::FeatureNotSupported(format!(
                "statement not supported: {first}"
            ))),
        }
    }

    // ---- transaction control -------------------------------------------

    async fn begin(&mut self) -> Result<ExecResult> {
        if self.txn.is_none() {
            let catalog = self.load_catalog().await?;
            self.txn = Some(Transaction {
                catalog,
                catalog_dirty: false,
                overlay: HashMap::new(),
                truncated: HashSet::new(),
            });
        }
        Ok(ExecResult::empty_command("BEGIN"))
    }

    async fn commit(&mut self) -> Result<ExecResult> {
        if let Some(txn) = self.txn.take() {
            for c in &txn.truncated {
                self.db.storage.truncate(c).await?;
            }
            for (collection, rows) in &txn.overlay {
                for (rid, val) in rows {
                    match val {
                        Some(doc) => self.db.storage.put(collection, rid, doc).await?,
                        None => self.db.storage.delete(collection, rid).await?,
                    }
                }
            }
            if txn.catalog_dirty {
                self.save_catalog(&txn.catalog).await?;
            }
        }
        Ok(ExecResult::empty_command("COMMIT"))
    }

    async fn rollback(&mut self) -> Result<ExecResult> {
        self.txn = None;
        Ok(ExecResult::empty_command("ROLLBACK"))
    }

    // ---- storage helpers -----------------------------------------------

    async fn load_catalog(&self) -> Result<Catalog> {
        match self.db.storage.load_catalog().await? {
            Some(json) => serde_json::from_value(json)
                .map_err(|e| SqlError::Storage(format!("corrupt catalog: {e}"))),
            None => Ok(Catalog::new(&self.db.name)),
        }
    }

    async fn save_catalog(&self, catalog: &Catalog) -> Result<()> {
        let json = serde_json::to_value(catalog)
            .map_err(|e| SqlError::Storage(format!("serialize catalog: {e}")))?;
        self.db.storage.save_catalog(&json).await
    }

    async fn load_table(&self, catalog: &Catalog, q: &QualifiedName) -> Result<Option<LoadedTable>> {
        let Some(table) = catalog.get_table(q) else {
            return Ok(None);
        };
        let collection = table.storage_collection.clone();
        let mut docs = self.db.storage.scan(&collection).await?;
        if let Some(txn) = &self.txn {
            let truncated = txn.truncated.contains(&collection);
            let overlay = txn.overlay.get(&collection);
            if truncated || overlay.is_some() {
                let mut map: std::collections::BTreeMap<String, Json> = if truncated {
                    std::collections::BTreeMap::new()
                } else {
                    docs.into_iter().collect()
                };
                if let Some(ov) = overlay {
                    for (rid, val) in ov {
                        match val {
                            Some(doc) => {
                                map.insert(rid.clone(), doc.clone());
                            }
                            None => {
                                map.remove(rid);
                            }
                        }
                    }
                }
                docs = map.into_iter().collect();
            }
        }
        let index_defs = catalog
            .indexes_for_table(&q.schema, &q.name)
            .into_iter()
            .cloned()
            .collect();
        Ok(Some(LoadedTable::build(table.clone(), docs, index_defs)?))
    }

    async fn apply_mutations(&self, mutations: Vec<Mutation>) -> Result<()> {
        for m in mutations {
            match m {
                Mutation::Put { collection, row_id, doc } => {
                    self.db.storage.put(&collection, &row_id, &doc).await?
                }
                Mutation::Delete { collection, row_id } => {
                    self.db.storage.delete(&collection, &row_id).await?
                }
                Mutation::Truncate { collection } => self.db.storage.truncate(&collection).await?,
            }
        }
        Ok(())
    }
}

fn stage_mutations(txn: &mut Transaction, mutations: Vec<Mutation>) {
    for m in mutations {
        match m {
            Mutation::Put { collection, row_id, doc } => {
                txn.overlay.entry(collection).or_default().insert(row_id, Some(doc));
            }
            Mutation::Delete { collection, row_id } => {
                txn.overlay.entry(collection).or_default().insert(row_id, None);
            }
            Mutation::Truncate { collection } => {
                txn.truncated.insert(collection.clone());
                txn.overlay.remove(&collection);
            }
        }
    }
}

fn relabel_cte(mut rs: crate::row::RowSet, name: &str) -> crate::row::RowSet {
    for f in &mut rs.schema.fields {
        f.table = Some(name.to_string());
    }
    rs
}

fn show_value(var: &str) -> String {
    match var.to_ascii_lowercase().as_str() {
        "server_version" => "15.0".into(),
        "server_version_num" => "150000".into(),
        "server_encoding" | "client_encoding" => "UTF8".into(),
        "standard_conforming_strings" | "transaction_read_only" => "on".into(),
        "search_path" => "\"$user\", public".into(),
        "timezone" | "time zone" => "UTC".into(),
        "integer_datetimes" => "on".into(),
        _ => String::new(),
    }
}

fn schema_name_to_string(name: &sqlparser::ast::SchemaName) -> String {
    use sqlparser::ast::SchemaName;
    match name {
        SchemaName::Simple(n) => crate::names::split_schema_table(n).1,
        SchemaName::NamedAuthorization(n, _) => crate::names::split_schema_table(n).1,
        SchemaName::UnnamedAuthorization(ident) => crate::names::ident_name(ident),
    }
}

fn empty_query() -> Query {
    // A harmless `SELECT NULL WHERE false`-style placeholder is overkill; reuse a
    // parsed empty SELECT.
    let stmts = crate::parser::parse_sql("SELECT 1 WHERE 1=0").unwrap();
    match stmts.into_iter().next() {
        Some(Statement::Query(q)) => *q,
        _ => unreachable!(),
    }
}

/// Count `$n` placeholders in a SQL string (ignoring those inside string literals).
fn count_placeholders(sql: &str) -> usize {
    let bytes = sql.as_bytes();
    let mut max = 0usize;
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if c == b'\'' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'\'' => in_string = true,
            b'$' => {
                let mut j = i + 1;
                let mut num = 0usize;
                let mut found = false;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    num = num * 10 + (bytes[j] - b'0') as usize;
                    j += 1;
                    found = true;
                }
                if found {
                    max = max.max(num);
                    i = j;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    max
}

// ---------------------------------------------------------------------------
// Table-reference collection (for preloading)
// ---------------------------------------------------------------------------

type NameOut = Vec<(Option<String>, String)>;

fn collect_stmt(stmt: &Statement, out: &mut NameOut) {
    match stmt {
        Statement::Query(q) => collect_query(q, out),
        Statement::Insert(i) => {
            if let sqlparser::ast::TableObject::TableName(name) = &i.table {
                push_name(name, out);
            }
            if let Some(src) = &i.source {
                collect_query(src, out);
            }
            if let Some(sqlparser::ast::OnInsert::OnConflict(oc)) = &i.on {
                if let sqlparser::ast::OnConflictAction::DoUpdate(du) = &oc.action {
                    if let Some(sel) = &du.selection {
                        collect_expr(sel, out);
                    }
                }
            }
        }
        Statement::Update(u) => {
            collect_tf(&u.table.relation, out);
            for j in &u.table.joins {
                collect_tf(&j.relation, out);
            }
            for a in &u.assignments {
                collect_expr(&a.value, out);
            }
            if let Some(sel) = &u.selection {
                collect_expr(sel, out);
            }
        }
        Statement::Delete(d) => {
            match &d.from {
                sqlparser::ast::FromTable::WithFromKeyword(items)
                | sqlparser::ast::FromTable::WithoutKeyword(items) => {
                    for twj in items {
                        collect_twj(twj, out);
                    }
                }
            }
            if let Some(using) = &d.using {
                for twj in using {
                    collect_twj(twj, out);
                }
            }
            if let Some(sel) = &d.selection {
                collect_expr(sel, out);
            }
        }
        Statement::AlterTable(alter) => push_name(&alter.name, out),
        Statement::CreateIndex(ci) => push_name(&ci.table_name, out),
        Statement::CreateView(cv) => collect_query(&cv.query, out),
        Statement::Truncate(t) => {
            for target in &t.table_names {
                push_name(&target.name, out);
            }
        }
        _ => {}
    }
}

fn collect_query(q: &Query, out: &mut NameOut) {
    if let Some(with) = &q.with {
        for cte in &with.cte_tables {
            collect_query(&cte.query, out);
        }
    }
    collect_setexpr(&q.body, out);
}

fn collect_setexpr(s: &sqlparser::ast::SetExpr, out: &mut NameOut) {
    use sqlparser::ast::SetExpr;
    match s {
        SetExpr::Select(sel) => collect_select(sel, out),
        SetExpr::Query(q) => collect_query(q, out),
        SetExpr::SetOperation { left, right, .. } => {
            collect_setexpr(left, out);
            collect_setexpr(right, out);
        }
        SetExpr::Values(v) => {
            for row in &v.rows {
                for e in &row.content {
                    collect_expr(e, out);
                }
            }
        }
        _ => {}
    }
}

fn collect_select(sel: &sqlparser::ast::Select, out: &mut NameOut) {
    for twj in &sel.from {
        collect_twj(twj, out);
    }
    if let Some(w) = &sel.selection {
        collect_expr(w, out);
    }
    if let Some(h) = &sel.having {
        collect_expr(h, out);
    }
    for item in &sel.projection {
        if let sqlparser::ast::SelectItem::UnnamedExpr(e)
        | sqlparser::ast::SelectItem::ExprWithAlias { expr: e, .. } = item
        {
            collect_expr(e, out);
        }
    }
}

fn collect_twj(twj: &sqlparser::ast::TableWithJoins, out: &mut NameOut) {
    collect_tf(&twj.relation, out);
    for j in &twj.joins {
        collect_tf(&j.relation, out);
        if let sqlparser::ast::JoinOperator::Inner(sqlparser::ast::JoinConstraint::On(e))
        | sqlparser::ast::JoinOperator::Left(sqlparser::ast::JoinConstraint::On(e))
        | sqlparser::ast::JoinOperator::Right(sqlparser::ast::JoinConstraint::On(e))
        | sqlparser::ast::JoinOperator::FullOuter(sqlparser::ast::JoinConstraint::On(e)) =
            &j.join_operator
        {
            collect_expr(e, out);
        }
    }
}

fn collect_tf(tf: &sqlparser::ast::TableFactor, out: &mut NameOut) {
    use sqlparser::ast::TableFactor;
    match tf {
        TableFactor::Table { name, .. } => push_name(name, out),
        TableFactor::Derived { subquery, .. } => collect_query(subquery, out),
        _ => {}
    }
}

fn collect_expr(e: &sqlparser::ast::Expr, out: &mut NameOut) {
    use sqlparser::ast::Expr;
    match e {
        Expr::Subquery(q) | Expr::Exists { subquery: q, .. } | Expr::InSubquery { subquery: q, .. } => {
            collect_query(q, out)
        }
        Expr::BinaryOp { left, right, .. } => {
            collect_expr(left, out);
            collect_expr(right, out);
        }
        Expr::UnaryOp { expr, .. }
        | Expr::Nested(expr)
        | Expr::IsNull(expr)
        | Expr::IsNotNull(expr)
        | Expr::Cast { expr, .. } => collect_expr(expr, out),
        Expr::Between { expr, low, high, .. } => {
            collect_expr(expr, out);
            collect_expr(low, out);
            collect_expr(high, out);
        }
        Expr::InList { expr, list, .. } => {
            collect_expr(expr, out);
            for e in list {
                collect_expr(e, out);
            }
        }
        Expr::Case { conditions, else_result, operand, .. } => {
            if let Some(o) = operand {
                collect_expr(o, out);
            }
            for w in conditions {
                collect_expr(&w.condition, out);
                collect_expr(&w.result, out);
            }
            if let Some(e) = else_result {
                collect_expr(e, out);
            }
        }
        _ => {}
    }
}

fn push_name(name: &sqlparser::ast::ObjectName, out: &mut NameOut) {
    out.push(crate::names::split_schema_table(name));
}
