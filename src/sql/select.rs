//! The SELECT execution pipeline (synchronous, over pre-loaded tables).

use crate::relational::{SqlType, SqlValue};
use crate::sql::error::{Result, SqlError};
use crate::sql::exec::{Exec, Frame};
use crate::sql::funcs;
use crate::sql::names::{ident_name, object_name_parts, split_schema_table};
use crate::sql::row::{FieldRef, RowSchema, RowSet, Tuple};
use sqlparser::ast::{
    Distinct, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, GroupByExpr, Join,
    JoinConstraint, JoinOperator, LimitClause, OrderBy, OrderByKind, Query, Select, SelectItem,
    SetExpr, SetOperator, TableFactor, TableWithJoins,
};
use std::cmp::Ordering;
use std::collections::HashMap;

/// One projected column before materialization.
struct OutCol {
    name: String,
    ty: SqlType,
}

impl Exec {
    /// Apply `SELECT ... FOR UPDATE/FOR SHARE [NOWAIT | SKIP LOCKED]` row locking
    /// over a single base table. Records the row locks to acquire and, for SKIP
    /// LOCKED, restricts the result to the rows that were lockable.
    pub fn prepare_for_update(&mut self, query: &Query) -> Result<()> {
        use crate::sql::lock::{LockMode, LockObject, LockScope, WaitPolicy};
        use sqlparser::ast::{LockType, NonBlock};
        if query.locks.is_empty() {
            return Ok(());
        }
        let clause = &query.locks[0];
        let mode = match clause.lock_type {
            LockType::Update => LockMode::ForUpdate,
            LockType::Share => LockMode::ForShare,
        };
        let policy = match clause.nonblock {
            Some(NonBlock::Nowait) => WaitPolicy::NoWait,
            Some(NonBlock::SkipLocked) => WaitPolicy::SkipLocked,
            None => WaitPolicy::Wait,
        };
        let select = match query.body.as_ref() {
            SetExpr::Select(s) => s,
            _ => {
                return Err(SqlError::FeatureNotSupported(
                    "row locks require a single-table SELECT".into(),
                ));
            }
        };
        if select.from.len() != 1 || !select.from[0].joins.is_empty() {
            return Err(SqlError::FeatureNotSupported(
                "FOR UPDATE/SHARE on joins is not supported".into(),
            ));
        }
        let (name, alias) = match &select.from[0].relation {
            TableFactor::Table { name, alias, .. } => (name, alias),
            _ => {
                return Err(SqlError::FeatureNotSupported(
                    "FOR UPDATE/SHARE requires a base table".into(),
                ));
            }
        };
        let (schema, tname) = split_schema_table(name);
        let q = self
            .catalog
            .resolve_table_name(schema.as_deref(), &tname)
            .ok_or_else(|| SqlError::UndefinedTable(tname.clone()))?;
        let table = self.catalog.require_table(&q)?.clone();
        let oid = table.oid;
        let alias_name = alias.as_ref().map(|a| ident_name(&a.name)).unwrap_or(tname);
        let tschema = crate::sql::dml::table_schema(&table, &alias_name);

        let rows: Vec<(String, _)> = self
            .tables
            .get(&q)
            .map(|l| {
                let rls_hidden = self.rls_select_hidden(&q);
                l.rows
                    .iter()
                    .filter(|(rid, _)| rls_hidden.map(|h| !h.contains(*rid)).unwrap_or(true))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let mut allow = std::collections::BTreeSet::new();
        for (rid, values) in rows {
            let tuple = crate::sql::dml::row_tuple(&table, &values);
            let matched = match &select.selection {
                Some(sel) => {
                    let frame = Frame {
                        schema: &tschema,
                        row: &tuple,
                    };
                    self.eval(sel, &[frame])?.truthy() == Some(true)
                }
                None => true,
            };
            if !matched {
                continue;
            }
            let object = LockObject::Row(oid, rid.clone());
            match policy {
                WaitPolicy::Wait => {
                    self.record_pending(object, mode, LockScope::Transaction);
                    allow.insert(rid);
                }
                WaitPolicy::NoWait => {
                    if self.try_lock(object, mode, LockScope::Transaction) {
                        allow.insert(rid);
                    } else {
                        return Err(SqlError::LockNotAvailable(format!(
                            "row in relation \"{}\"",
                            q.name
                        )));
                    }
                }
                WaitPolicy::SkipLocked => {
                    if self.try_lock(object, mode, LockScope::Transaction) {
                        allow.insert(rid);
                    }
                }
            }
        }
        if matches!(policy, WaitPolicy::SkipLocked) {
            self.for_update_filter = Some((q, allow));
        }
        Ok(())
    }

    /// Execute a subquery (used by the evaluator), inheriting outer frames.
    pub fn exec_subquery(&self, query: &Query, outer: &[Frame]) -> Result<RowSet> {
        self.exec_select_query(query, outer)
    }

    /// Execute a full `Query` (a SELECT with optional WITH/ORDER BY/LIMIT).
    pub fn exec_select_query(&self, query: &Query, outer: &[Frame]) -> Result<RowSet> {
        // Nested WITH that was not pre-materialized at the statement top level is
        // not supported (top-level CTEs — including recursive ones — are
        // materialized by `materialize_with` before execution).
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                let name = ident_name(&cte.alias.name);
                if !self.cte.contains_key(&name) {
                    return Err(SqlError::FeatureNotSupported(
                        "WITH inside a subquery is not supported (use a top-level WITH)".into(),
                    ));
                }
            }
        }
        let mut rowset = match query.body.as_ref() {
            // For a single SELECT block, ORDER BY is resolved with the input
            // (pre-projection) columns available, matching PostgreSQL.
            SetExpr::Select(select) => {
                // `LIMIT + OFFSET` as a top-k hint for the ANN planner hook
                // (RFC 0005); `None` when absent or not constant-evaluable.
                let limit_hint = self.limit_hint(query.limit_clause.as_ref(), outer);
                self.exec_select(select, outer, query.order_by.as_ref(), limit_hint)?
            }
            _ => {
                let mut rs = self.exec_set_expr(&query.body, outer)?;
                if let Some(order_by) = &query.order_by {
                    self.apply_order_by(&mut rs, order_by, outer)?;
                }
                rs
            }
        };
        self.apply_limit(&mut rowset, query.limit_clause.as_ref(), outer)?;
        Ok(rowset)
    }

    /// Evaluate `LIMIT + OFFSET` to a row-count hint for top-k planning.
    /// `None` when there is no LIMIT or it does not evaluate to a
    /// non-negative integer (the authoritative application remains
    /// [`Self::apply_limit`]).
    fn limit_hint(&self, limit: Option<&LimitClause>, outer: &[Frame]) -> Option<usize> {
        let (limit_expr, offset_expr) = match limit {
            None => return None,
            Some(LimitClause::LimitOffset { limit, offset, .. }) => {
                (limit.clone()?, offset.as_ref().map(|o| o.value.clone()))
            }
            Some(LimitClause::OffsetCommaLimit { offset, limit }) => {
                (limit.clone(), Some(offset.clone()))
            }
        };
        let lim = self.eval(&limit_expr, outer).ok()?.as_i64()?;
        if lim < 0 {
            return None;
        }
        let off = match offset_expr {
            Some(e) => self.eval(&e, outer).ok()?.as_i64()?.max(0),
            None => 0,
        };
        usize::try_from(lim).ok()?.checked_add(off as usize)
    }

    fn exec_set_expr(&self, body: &SetExpr, outer: &[Frame]) -> Result<RowSet> {
        match body {
            SetExpr::Select(select) => self.exec_select(select, outer, None, None),
            SetExpr::Query(q) => self.exec_select_query(q, outer),
            SetExpr::Values(values) => self.exec_values(values, outer),
            SetExpr::SetOperation {
                left,
                op,
                set_quantifier,
                right,
            } => {
                let l = self.exec_set_expr(left, outer)?;
                let r = self.exec_set_expr(right, outer)?;
                self.apply_set_op(l, r, op, set_quantifier)
            }
            other => Err(SqlError::FeatureNotSupported(format!(
                "set expression not supported: {other}"
            ))),
        }
    }

    fn exec_values(&self, values: &sqlparser::ast::Values, outer: &[Frame]) -> Result<RowSet> {
        let mut rows = Vec::new();
        let mut width = 0;
        for row in &values.rows {
            let mut tuple = Vec::new();
            for e in &row.content {
                tuple.push(self.eval(e, outer)?);
            }
            width = width.max(tuple.len());
            rows.push(tuple);
        }
        let fields = (0..width)
            .map(|i| FieldRef {
                table: None,
                name: format!("column{}", i + 1),
                ty: rows
                    .iter()
                    .find_map(|r| r.get(i).map(|v| v.type_of()))
                    .unwrap_or(SqlType::Text),
            })
            .collect();
        Ok(RowSet {
            schema: RowSchema::new(fields),
            rows,
        })
    }

    fn apply_set_op(
        &self,
        left: RowSet,
        right: RowSet,
        op: &SetOperator,
        quantifier: &sqlparser::ast::SetQuantifier,
    ) -> Result<RowSet> {
        let all = matches!(
            quantifier,
            sqlparser::ast::SetQuantifier::All | sqlparser::ast::SetQuantifier::AllByName
        );
        let key = |t: &Tuple| -> Vec<String> { t.iter().map(|v| v.index_key()).collect() };
        let mut rows = Vec::new();
        match op {
            SetOperator::Union => {
                rows.extend(left.rows.iter().cloned());
                rows.extend(right.rows.iter().cloned());
                if !all {
                    rows = dedupe(rows);
                }
            }
            SetOperator::Except | SetOperator::Minus => {
                let remove: std::collections::HashSet<Vec<String>> =
                    right.rows.iter().map(key).collect();
                for r in &left.rows {
                    if !remove.contains(&key(r)) {
                        rows.push(r.clone());
                    }
                }
                if !all {
                    rows = dedupe(rows);
                }
            }
            SetOperator::Intersect => {
                let keep: std::collections::HashSet<Vec<String>> =
                    right.rows.iter().map(key).collect();
                for r in &left.rows {
                    if keep.contains(&key(r)) {
                        rows.push(r.clone());
                    }
                }
                if !all {
                    rows = dedupe(rows);
                }
            }
        }
        Ok(RowSet {
            schema: left.schema,
            rows,
        })
    }

    // ---- core single-block SELECT --------------------------------------

    #[cfg_attr(not(feature = "vector-index"), allow(unused_variables))]
    fn exec_select(
        &self,
        select: &Select,
        outer: &[Frame],
        order_by: Option<&OrderBy>,
        limit_hint: Option<usize>,
    ) -> Result<RowSet> {
        // Window functions are valid in the SELECT list and ORDER BY only;
        // PostgreSQL rejects them in the other clauses with 42P20.
        if select.selection.as_ref().is_some_and(expr_has_window) {
            return Err(SqlError::WindowingError(
                "window functions are not allowed in WHERE".into(),
            ));
        }
        if let GroupByExpr::Expressions(exprs, _) = &select.group_by
            && exprs.iter().any(expr_has_window)
        {
            return Err(SqlError::WindowingError(
                "window functions are not allowed in GROUP BY".into(),
            ));
        }
        if select.having.as_ref().is_some_and(expr_has_window) {
            return Err(SqlError::WindowingError(
                "window functions are not allowed in HAVING".into(),
            ));
        }
        // Planner. Highest preference: ANN top-k candidate scan for
        // `ORDER BY <col> <dist-op> <query> LIMIT k` over an hnsw index
        // (RFC 0005 §3.2 — the hook only *selects candidates*; ordering,
        // filtering and projection below stay exact and unchanged). Next: an
        // index scan when a single base table is filtered by an equality on
        // an indexed column. Otherwise a full scan.
        #[cfg(feature = "vector-index")]
        let ann_from = self.try_ann_scan(select, outer, order_by, limit_hint)?;
        #[cfg(not(feature = "vector-index"))]
        let ann_from: Option<RowSet> = None;
        let from = match ann_from {
            Some(rs) => rs,
            None => match self.try_index_scan(select, outer)? {
                Some(rs) => rs,
                None => self.exec_from(&select.from, outer)?,
            },
        };
        let filtered = self.apply_where(from, select.selection.as_ref(), outer)?;
        let group_exprs = match &select.group_by {
            GroupByExpr::Expressions(exprs, _) => exprs.clone(),
            GroupByExpr::All(_) => Vec::new(),
        };
        let has_aggregate = select_has_aggregate(select);
        let distinct = matches!(select.distinct, Some(Distinct::Distinct));

        if !group_exprs.is_empty() || has_aggregate {
            let mut rowset = self.exec_grouped(select, &filtered, &group_exprs, outer, order_by)?;
            if distinct {
                rowset.rows = dedupe(std::mem::take(&mut rowset.rows));
            }
            Ok(rowset)
        } else {
            self.exec_projection_ordered(select, &filtered, outer, order_by, distinct)
        }
    }

    /// Attempt an index scan: a single base table filtered by `col = const` on an
    /// indexed column. Returns the candidate rows (a superset that the subsequent
    /// WHERE filter narrows to the exact result, so results equal a full scan).
    fn try_index_scan(&self, select: &Select, outer: &[Frame]) -> Result<Option<RowSet>> {
        if select.from.len() != 1 || !select.from[0].joins.is_empty() {
            return Ok(None);
        }
        let (name, alias) = match &select.from[0].relation {
            TableFactor::Table { name, alias, .. } => (name, alias),
            _ => return Ok(None),
        };
        let (schema, tname) = split_schema_table(name);
        let Some(q) = self.catalog.resolve_table_name(schema.as_deref(), &tname) else {
            return Ok(None);
        };
        let Some(loaded) = self.tables.get(&q) else {
            return Ok(None);
        };
        let Some(selection) = &select.selection else {
            return Ok(None);
        };
        // Find an equality predicate on an indexed column (only descending ANDs).
        let Some((column, value_expr)) = find_indexed_equality(selection, loaded) else {
            return Ok(None);
        };
        let Ok(value) = self.eval(value_expr, outer) else {
            return Ok(None);
        };
        let Some(col_def) = loaded.meta.column(&column) else {
            return Ok(None);
        };
        let Ok(coerced) = value.cast(&col_def.ty) else {
            return Ok(None);
        };
        let Some(row_ids) = loaded.index_lookup_eq(&column, &coerced) else {
            return Ok(None);
        };
        let alias_name = alias
            .as_ref()
            .map(|a| ident_name(&a.name))
            .unwrap_or_else(|| tname.clone());
        let fields = loaded
            .meta
            .columns
            .iter()
            .map(|c| FieldRef {
                table: Some(alias_name.clone()),
                name: c.name.clone(),
                ty: c.ty.clone(),
            })
            .collect();
        let schema = RowSchema::new(fields);
        let rls_hidden = self.rls_select_hidden(&q);
        let rows = row_ids
            .iter()
            .filter(|rid| rls_hidden.map(|h| !h.contains(*rid)).unwrap_or(true))
            .filter_map(|rid| loaded.rows.get(rid))
            .map(|values| {
                loaded
                    .meta
                    .columns
                    .iter()
                    .map(|c| values.get(&c.name).cloned().unwrap_or(SqlValue::Null))
                    .collect()
            })
            .collect();
        Ok(Some(RowSet { schema, rows }))
    }

    /// Attempt an ANN top-k candidate scan (RFC 0005 §3.2 / §6.2).
    ///
    /// Pattern gate: single base table, no joins/grouping/aggregates/
    /// DISTINCT, `ORDER BY <indexed vector column> <dist-op> <constant
    /// vector> [ASC] [, tie-breakers...] LIMIT k`, a healthy `hnsw` index
    /// whose opclass serves exactly that operator, and the `vector`
    /// extension installed. Anything else returns `None` and the exact path
    /// runs unchanged.
    ///
    /// On a hit the hook returns a *candidate* `RowSet`: the ANN-ranked rows
    /// (a superset of the answer) plus every NULL-vector row appended (they
    /// sort last under ASC/NULLS LAST, so including them is always correct
    /// and lets them fill an under-full LIMIT exactly like PostgreSQL).
    /// The normal WHERE / ORDER BY / LIMIT pipeline then runs *exactly* over
    /// those candidates — approximation lives only in candidate selection.
    ///
    /// Filtered-search strategy (§6.2), both halves:
    /// - **selective cutover**: when the WHERE clause has an indexed
    ///   equality whose measured candidate count is within
    ///   `hnsw.selectivity_threshold × k`, ANN is skipped entirely — the
    ///   existing `try_index_scan` + exact sort is cheaper *and* exact;
    /// - **adaptive growth**: otherwise candidates are post-filtered; while
    ///   fewer than `k` survive the WHERE clause, the fetch size doubles
    ///   (raising `ef` with it) until `k` survivors, the whole graph, or the
    ///   `hnsw.ef_search × hnsw.ef_growth_cap` ceiling — past the ceiling
    ///   the hook falls back to the exact scan rather than under-filling.
    #[cfg(feature = "vector-index")]
    fn try_ann_scan(
        &self,
        select: &Select,
        outer: &[Frame],
        order_by: Option<&OrderBy>,
        limit_hint: Option<usize>,
    ) -> Result<Option<RowSet>> {
        use crate::relational::hnsw::VectorOpClass;
        let k = match limit_hint {
            Some(k) if k > 0 => k,
            _ => return Ok(None),
        };
        let Some(ann) = self.ann.as_ref() else {
            return Ok(None);
        };
        // `SET enable_indexscan = off` disables the approximate path — the
        // documented way to force exact results (RFC 0005 §3.2).
        if let Some(v) = self.vars.borrow().get("enable_indexscan")
            && matches!(v.as_str(), "off" | "false" | "0")
        {
            return Ok(None);
        }
        if !self.catalog.extension_installed("vector") {
            return Ok(None);
        }
        // Result-shape gates: candidate pre-selection must commute with the
        // rest of the pipeline, which it only does for a plain projection.
        if select_has_aggregate(select)
            || select.distinct.is_some()
            || !matches!(
                &select.group_by,
                GroupByExpr::Expressions(exprs, _) if exprs.is_empty()
            )
        {
            return Ok(None);
        }
        if select.from.len() != 1 || !select.from[0].joins.is_empty() {
            return Ok(None);
        }
        let (name, alias) = match &select.from[0].relation {
            TableFactor::Table {
                name,
                alias,
                args: None,
                ..
            } => (name, alias),
            _ => return Ok(None),
        };
        let (schema, tname) = split_schema_table(name);
        let Some(q) = self.catalog.resolve_table_name(schema.as_deref(), &tname) else {
            return Ok(None);
        };
        let Some(loaded) = self.tables.get(&q) else {
            return Ok(None);
        };
        let alias_name = alias
            .as_ref()
            .map(|a| ident_name(&a.name))
            .unwrap_or_else(|| tname.clone());

        // ORDER BY: first key must be `col <dist-op> const-vector` ascending
        // with NULLS LAST (the index's native order); extra keys are fine —
        // the exact sort below handles them over the candidates.
        let exprs = match order_by.map(|ob| &ob.kind) {
            Some(OrderByKind::Expressions(exprs)) if !exprs.is_empty() => exprs,
            _ => return Ok(None),
        };
        let first = &exprs[0];
        if !first.options.asc.unwrap_or(true) || first.options.nulls_first.unwrap_or(false) {
            return Ok(None);
        }
        let Expr::BinaryOp { left, op, right } = &first.expr else {
            return Ok(None);
        };
        let op_token = {
            use sqlparser::ast::BinaryOperator as BO;
            match op {
                BO::LtDashGt => "<->",
                BO::Spaceship => "<=>",
                BO::Custom(s) if s == "<#>" => "<#>",
                BO::Custom(s) if s == "<+>" => "<+>",
                _ => return Ok(None),
            }
        };
        // One side names the vector column, the other must be row-constant.
        let column_of = |e: &Expr| -> Option<String> {
            let (qual, col) = match e {
                Expr::Identifier(ident) => (None, ident_name(ident)),
                Expr::CompoundIdentifier(parts) if parts.len() == 2 => {
                    (Some(ident_name(&parts[0])), ident_name(&parts[1]))
                }
                _ => return None,
            };
            if let Some(t) = qual
                && t != alias_name
            {
                return None;
            }
            loaded.meta.column(&col).map(|_| col)
        };
        let (column, query_expr) = match (column_of(left), column_of(right)) {
            (Some(c), None) => (c, right.as_ref()),
            (None, Some(c)) => (c, left.as_ref()),
            _ => return Ok(None),
        };
        let col_def = loaded.meta.column(&column).expect("resolved above");
        let dims = match &col_def.ty {
            SqlType::Vector(Some(d)) => *d as usize,
            _ => return Ok(None),
        };
        // The matching hnsw index: same column, opclass serving this operator.
        let Some(opclass) = VectorOpClass::for_operator(op_token) else {
            return Ok(None);
        };
        let indexes = self.catalog.indexes_for_table(&q.schema, &q.name);
        let Some(idx) = indexes.iter().find(|i| {
            i.method == "hnsw"
                && i.columns.len() == 1
                && i.columns[0] == column
                && i.opclasses.first().map(String::as_str) == Some(opclass.name())
        }) else {
            return Ok(None);
        };
        // The query vector must be row-constant: evaluation with no row frame
        // fails on any column reference, which is exactly the bail we want.
        let Ok(query_val) = self.eval(query_expr, outer) else {
            return Ok(None);
        };
        let Ok(SqlValue::Vector(query_vec)) = query_val.cast(&col_def.ty) else {
            // Let the exact path surface the proper error/NULL semantics.
            return Ok(None);
        };
        if query_vec.len() != dims {
            return Ok(None);
        }

        // GUCs (registered on the `vector` extension; SET overrides).
        let guc = |name: &str, default: i64, lo: i64, hi: i64| -> i64 {
            self.vars
                .borrow()
                .get(name)
                .cloned()
                .or_else(|| crate::sql::ext::default_guc(name).map(str::to_string))
                .and_then(|v| v.trim().parse::<i64>().ok())
                .unwrap_or(default)
                .clamp(lo, hi)
        };
        let ef_search = guc("hnsw.ef_search", 40, 1, 1000) as usize;
        let growth_cap = guc("hnsw.ef_growth_cap", 10, 1, 100) as usize;
        let selectivity = guc("hnsw.selectivity_threshold", 10, 1, 1000) as usize;
        let ef_ceiling = ef_search.saturating_mul(growth_cap);

        // §6.2 selective cutover: measured (not estimated) via the existing
        // secondary-index machinery. Within threshold → let `try_index_scan`
        // + exact sort handle it.
        if let Some(selection) = &select.selection
            && let Some((eq_col, eq_expr)) = find_indexed_equality(selection, loaded)
            && let Ok(value) = self.eval(eq_expr, outer)
            && let Some(eq_def) = loaded.meta.column(&eq_col)
            && let Ok(coerced) = value.cast(&eq_def.ty)
            && let Some(ids) = loaded.index_lookup_eq(&eq_col, &coerced)
            && ids.len() <= selectivity.saturating_mul(k)
        {
            self.plan_notes.borrow_mut().push(format!(
                "Index Scan on {} (ann skipped: equality candidates {} within {} \u{00d7} k)",
                q.name,
                ids.len(),
                selectivity
            ));
            return Ok(None);
        }

        let rls_hidden = self.rls_select_hidden(&q);
        let visible = |rid: &str| rls_hidden.map(|h| !h.contains(rid)).unwrap_or(true);
        let fields: Vec<FieldRef> = loaded
            .meta
            .columns
            .iter()
            .map(|c| FieldRef {
                table: Some(alias_name.clone()),
                name: c.name.clone(),
                ty: c.ty.clone(),
            })
            .collect();
        let row_schema = RowSchema::new(fields);
        let tuple_of = |rid: &str| -> Option<Tuple> {
            loaded.rows.get(rid).map(|values| {
                loaded
                    .meta
                    .columns
                    .iter()
                    .map(|c| values.get(&c.name).cloned().unwrap_or(SqlValue::Null))
                    .collect()
            })
        };
        // NULL-vector rows sort last under the index's order; always append
        // them so they can fill an under-full LIMIT (PostgreSQL semantics).
        let null_rows: Vec<Tuple> = loaded
            .rows
            .iter()
            .filter(|(rid, values)| {
                visible(rid) && !matches!(values.get(&column), Some(SqlValue::Vector(_)))
            })
            .map(|(_, values)| {
                loaded
                    .meta
                    .columns
                    .iter()
                    .map(|c| values.get(&c.name).cloned().unwrap_or(SqlValue::Null))
                    .collect()
            })
            .collect();

        // Adaptive growth loop (§6.2).
        let mut fetch = k;
        loop {
            let ef = ef_search.max(fetch).min(ef_ceiling);
            let req = crate::sql::ann::AnnQuery {
                idx,
                column: &column,
                column_ty: &col_def.ty,
                op: op_token,
                query: &query_vec,
                k: fetch,
                ef,
            };
            let Some(ranked) = ann.candidates(&req, loaded) else {
                return Ok(None);
            };
            // Exhaustion means "every live indexed row is already in the
            // candidate set" — measured against the index's live count, NOT
            // `ranked.len() < fetch`: an ef-bounded (filtered) HNSW
            // traversal can legitimately return fewer than `fetch` results
            // while plenty of unvisited nodes remain.
            let exhausted = ann.live_count(idx).is_some_and(|live| ranked.len() >= live);
            let rows: Vec<Tuple> = ranked
                .iter()
                .filter(|(rid, _)| visible(rid))
                .filter_map(|(rid, _)| tuple_of(rid))
                .collect();
            let candidate_count = rows.len();
            let mut rowset = RowSet {
                schema: row_schema.clone(),
                rows,
            };
            rowset.rows.extend(null_rows.iter().cloned());
            // Enough WHERE survivors among the candidates?
            let survivors = match &select.selection {
                Some(selection) => self
                    .apply_where(rowset.clone(), Some(selection), outer)?
                    .rows
                    .len(),
                None => rowset.rows.len(),
            };
            if survivors >= k || exhausted {
                self.plan_notes.borrow_mut().push(format!(
                    "Ann Index Scan using {} on {} (op {}, ef {}, candidates {})",
                    idx.name, q.name, op_token, ef, candidate_count
                ));
                return Ok(Some(rowset));
            }
            // Grow. Past the ceiling, fall back to the exact scan rather
            // than returning fewer than k rows the table could satisfy.
            let next = fetch.saturating_mul(2);
            if ef_search.max(next) > ef_ceiling {
                self.plan_notes.borrow_mut().push(format!(
                    "Seq Scan on {} (ann abandoned: ef ceiling {} reached with {} of {} \
                     survivors)",
                    q.name, ef_ceiling, survivors, k
                ));
                return Ok(None);
            }
            fetch = next;
        }
    }

    fn exec_from(&self, from: &[TableWithJoins], outer: &[Frame]) -> Result<RowSet> {
        if from.is_empty() {
            return Ok(RowSet {
                schema: RowSchema::default(),
                rows: vec![vec![]],
            });
        }
        let mut acc: Option<RowSet> = None;
        for twj in from {
            let mut current = self.exec_table_factor(&twj.relation, outer)?;
            for join in &twj.joins {
                current = self.exec_join(current, join, outer)?;
            }
            acc = Some(match acc {
                None => current,
                Some(prev) => cross_join(prev, current),
            });
        }
        Ok(acc.unwrap())
    }

    fn exec_table_factor(&self, tf: &TableFactor, outer: &[Frame]) -> Result<RowSet> {
        match tf {
            // A table function call such as `FROM current_schema()`.
            TableFactor::Table {
                name,
                alias,
                args: Some(args),
                ..
            } => {
                let arg_exprs: Vec<&Expr> = args
                    .args
                    .iter()
                    .filter_map(|a| match a {
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
                        | FunctionArg::Named {
                            arg: FunctionArgExpr::Expr(e),
                            ..
                        } => Some(e),
                        _ => None,
                    })
                    .collect();
                self.exec_table_function(name, &arg_exprs, alias, outer)
            }
            TableFactor::Function {
                name, args, alias, ..
            } => {
                let arg_exprs: Vec<&Expr> = args
                    .iter()
                    .filter_map(|a| match a {
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
                        | FunctionArg::Named {
                            arg: FunctionArgExpr::Expr(e),
                            ..
                        } => Some(e),
                        _ => None,
                    })
                    .collect();
                self.exec_table_function(name, &arg_exprs, alias, outer)
            }
            TableFactor::Table { name, alias, .. } => {
                let parts = object_name_parts(name);
                let bare = parts.last().cloned().unwrap_or_default();
                let alias_name = alias
                    .as_ref()
                    .map(|a| ident_name(&a.name))
                    .unwrap_or_else(|| bare.clone());
                // CTE?
                if parts.len() == 1
                    && let Some(cte) = self.cte.get(&bare)
                {
                    return Ok(relabel(cte.clone(), &alias_name, alias));
                }
                let (schema, tname) = split_schema_table(name);
                // pg_catalog.pg_locks — synthesized from the live lock manager.
                if tname == "pg_locks"
                    && (schema.as_deref() == Some("pg_catalog") || schema.is_none())
                {
                    return Ok(relabel(self.pg_locks_rows(), &alias_name, alias));
                }
                // Catalog table?
                if let Some(q) = self.catalog.resolve_table_name(schema.as_deref(), &tname) {
                    if let Some(loaded) = self.tables.get(&q) {
                        let filter = self
                            .for_update_filter
                            .as_ref()
                            .filter(|(fq, _)| fq == &q)
                            .map(|(_, s)| s);
                        return Ok(relabel(
                            loaded_to_rowset(
                                loaded,
                                &alias_name,
                                filter,
                                self.rls_select_hidden(&q),
                            ),
                            &alias_name,
                            alias,
                        ));
                    }
                    if let Some(view) = self.catalog.get_view(&q).cloned() {
                        return self.exec_view(&view, &alias_name, alias, outer);
                    }
                }
                // Catalog introspection view?
                if let Some(rs) = crate::sql::catalog_views::view_rows(
                    &self.catalog,
                    schema.as_deref(),
                    &tname,
                    &alias_name,
                )? {
                    return Ok(relabel(rs, &alias_name, alias));
                }
                Err(SqlError::UndefinedTable(match schema {
                    Some(s) => format!("{s}.{tname}"),
                    None => tname,
                }))
            }
            TableFactor::Derived {
                subquery, alias, ..
            } => {
                let rs = self.exec_select_query(subquery, outer)?;
                let alias_name = alias
                    .as_ref()
                    .map(|a| ident_name(&a.name))
                    .unwrap_or_else(|| "subquery".to_string());
                Ok(relabel(rs, &alias_name, alias))
            }
            other => Err(SqlError::FeatureNotSupported(format!(
                "table factor not supported: {other}"
            ))),
        }
    }

    /// Execute a scalar table function in FROM position (e.g. `current_schema()`,
    /// `version()`), producing a one-row, one-column result named after it.
    /// Set-returning functions are not supported.
    fn exec_table_function(
        &self,
        name: &sqlparser::ast::ObjectName,
        args: &[&Expr],
        alias: &Option<sqlparser::ast::TableAlias>,
        outer: &[Frame],
    ) -> Result<RowSet> {
        let fname = crate::sql::names::function_dispatch_name(name);
        if matches!(
            fname.as_str(),
            "generate_series" | "unnest" | "jsonb_array_elements" | "json_array_elements"
        ) {
            return Err(SqlError::FeatureNotSupported(format!(
                "set-returning function {fname} is not supported"
            )));
        }
        let mut values = Vec::with_capacity(args.len());
        for e in args {
            values.push(self.eval(e, outer)?);
        }
        let value = funcs::call_scalar(self, &fname, values)?;
        let alias_name = alias
            .as_ref()
            .map(|a| ident_name(&a.name))
            .unwrap_or_else(|| fname.clone());
        let col_name = alias
            .as_ref()
            .and_then(|a| a.columns.first().map(|c| ident_name(&c.name)))
            .unwrap_or(fname);
        let field = FieldRef {
            table: Some(alias_name),
            name: col_name,
            ty: value.type_of(),
        };
        Ok(RowSet {
            schema: RowSchema::new(vec![field]),
            rows: vec![vec![value]],
        })
    }

    fn exec_view(
        &self,
        view: &crate::relational::catalog::View,
        alias_name: &str,
        alias: &Option<sqlparser::ast::TableAlias>,
        outer: &[Frame],
    ) -> Result<RowSet> {
        let stmts = crate::sql::parser::parse_sql(&view.query)?;
        let query = match stmts.into_iter().next() {
            Some(sqlparser::ast::Statement::Query(q)) => q,
            _ => return Err(SqlError::Internal("view definition is not a query".into())),
        };
        let rs = self.exec_select_query(&query, outer)?;
        Ok(relabel(rs, alias_name, alias))
    }

    /// Build `pg_catalog.pg_locks` rows from the live lock-manager snapshot.
    fn pg_locks_rows(&self) -> RowSet {
        let fields = vec![
            FieldRef {
                table: None,
                name: "locktype".into(),
                ty: SqlType::Text,
            },
            FieldRef {
                table: None,
                name: "relation".into(),
                ty: SqlType::Text,
            },
            FieldRef {
                table: None,
                name: "mode".into(),
                ty: SqlType::Text,
            },
            FieldRef {
                table: None,
                name: "granted".into(),
                ty: SqlType::Boolean,
            },
            FieldRef {
                table: None,
                name: "pid".into(),
                ty: SqlType::Integer,
            },
        ];
        let rows = self
            .locks
            .snapshot()
            .into_iter()
            .map(|r| {
                vec![
                    SqlValue::Text(r.locktype),
                    SqlValue::Text(r.object),
                    SqlValue::Text(r.mode),
                    SqlValue::Bool(r.granted),
                    SqlValue::Int4(r.holder as i32),
                ]
            })
            .collect();
        RowSet {
            schema: RowSchema::new(fields),
            rows,
        }
    }

    fn exec_join(&self, left: RowSet, join: &Join, outer: &[Frame]) -> Result<RowSet> {
        let right = self.exec_table_factor(&join.relation, outer)?;
        let (kind, constraint) = match &join.join_operator {
            JoinOperator::Inner(c) | JoinOperator::Join(c) => (JoinKind::Inner, Some(c)),
            JoinOperator::Left(c) | JoinOperator::LeftOuter(c) => (JoinKind::Left, Some(c)),
            JoinOperator::Right(c) | JoinOperator::RightOuter(c) => (JoinKind::Right, Some(c)),
            JoinOperator::FullOuter(c) => (JoinKind::Full, Some(c)),
            JoinOperator::CrossJoin(_) => (JoinKind::Cross, None),
            other => {
                return Err(SqlError::FeatureNotSupported(format!(
                    "join type not supported: {other:?}"
                )));
            }
        };
        if matches!(kind, JoinKind::Cross) {
            return Ok(cross_join(left, right));
        }
        let combined_schema = left.schema.concat(&right.schema);
        let predicate = constraint.and_then(|c| match c {
            JoinConstraint::On(e) => Some(JoinPredicate::On(Box::new(e.clone()))),
            JoinConstraint::Using(cols) => Some(JoinPredicate::Using(
                cols.iter()
                    .filter_map(|c| c.0.last())
                    .filter_map(|p| p.as_ident())
                    .map(ident_name)
                    .collect(),
            )),
            JoinConstraint::Natural => Some(JoinPredicate::Natural),
            JoinConstraint::None => None,
        });

        let mut rows = Vec::new();
        let right_width = right.schema.len();
        let left_width = left.schema.len();
        let mut right_matched = vec![false; right.rows.len()];

        for l in &left.rows {
            let mut any = false;
            for (ri, r) in right.rows.iter().enumerate() {
                let mut combined = l.clone();
                combined.extend(r.iter().cloned());
                if self.join_matches(
                    &predicate,
                    &left.schema,
                    &right.schema,
                    &combined_schema,
                    &combined,
                    outer,
                )? {
                    rows.push(combined);
                    any = true;
                    right_matched[ri] = true;
                }
            }
            if !any && matches!(kind, JoinKind::Left | JoinKind::Full) {
                let mut combined = l.clone();
                combined.extend(std::iter::repeat_n(SqlValue::Null, right_width));
                rows.push(combined);
            }
        }
        if matches!(kind, JoinKind::Right | JoinKind::Full) {
            for (ri, matched) in right_matched.iter().enumerate() {
                if !matched {
                    let mut combined: Tuple =
                        std::iter::repeat_n(SqlValue::Null, left_width).collect();
                    combined.extend(right.rows[ri].iter().cloned());
                    rows.push(combined);
                }
            }
        }
        Ok(RowSet {
            schema: combined_schema,
            rows,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn join_matches(
        &self,
        predicate: &Option<JoinPredicate>,
        left_schema: &RowSchema,
        right_schema: &RowSchema,
        combined_schema: &RowSchema,
        combined: &Tuple,
        outer: &[Frame],
    ) -> Result<bool> {
        match predicate {
            None => Ok(true),
            Some(JoinPredicate::On(expr)) => {
                let mut frames: Vec<Frame> = outer
                    .iter()
                    .map(|f| Frame {
                        schema: f.schema,
                        row: f.row,
                    })
                    .collect();
                frames.push(Frame {
                    schema: combined_schema,
                    row: combined,
                });
                Ok(self.eval(expr, &frames)?.truthy() == Some(true))
            }
            Some(JoinPredicate::Using(cols)) | Some(JoinPredicate::NaturalCols(cols)) => {
                for col in cols {
                    let li = left_schema.resolve(None, col)?;
                    let ri = right_schema.resolve(None, col)?;
                    let lv = &combined[li];
                    let rv = &combined[left_schema.len() + ri];
                    if lv.sql_eq(rv) != Some(true) {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Some(JoinPredicate::Natural) => {
                let common: Vec<String> = left_schema
                    .fields
                    .iter()
                    .filter(|lf| right_schema.fields.iter().any(|rf| rf.name == lf.name))
                    .map(|lf| lf.name.clone())
                    .collect();
                self.join_matches(
                    &Some(JoinPredicate::NaturalCols(common)),
                    left_schema,
                    right_schema,
                    combined_schema,
                    combined,
                    outer,
                )
            }
        }
    }

    fn apply_where(
        &self,
        input: RowSet,
        selection: Option<&Expr>,
        outer: &[Frame],
    ) -> Result<RowSet> {
        let Some(predicate) = selection else {
            return Ok(input);
        };
        let mut rows = Vec::new();
        if !input.rows.is_empty() {
            // Build the outer-frame prefix once; reuse the Vec by updating the
            // last slot each iteration instead of allocating a new Vec per row.
            let mut frames: Vec<Frame> = outer
                .iter()
                .map(|f| Frame {
                    schema: f.schema,
                    row: f.row,
                })
                .collect();
            frames.push(Frame {
                schema: &input.schema,
                row: &input.rows[0],
            });
            let last_idx = frames.len() - 1;
            for row in &input.rows {
                frames[last_idx] = Frame {
                    schema: &input.schema,
                    row,
                };
                if self.eval(predicate, &frames)?.truthy() == Some(true) {
                    rows.push(row.clone());
                }
            }
        }
        Ok(RowSet {
            schema: input.schema,
            rows,
        })
    }

    fn exec_projection_ordered(
        &self,
        select: &Select,
        input: &RowSet,
        outer: &[Frame],
        order_by: Option<&OrderBy>,
        distinct: bool,
    ) -> Result<RowSet> {
        let cols = self.projection_columns(select, &input.schema)?;
        let out_schema = RowSchema::new(
            cols.iter()
                .map(|c| FieldRef {
                    table: None,
                    name: c.name.clone(),
                    ty: c.ty.clone(),
                })
                .collect(),
        );
        // Window calls in the SELECT list / ORDER BY are computed over the full
        // filtered input, before projection, DISTINCT and ORDER BY (PostgreSQL
        // evaluation order).
        let win_maps =
            self.compute_window_maps(select, order_by, &input.schema, &input.rows, None, outer)?;
        let win_at = |i: usize| win_maps.as_ref().map(|m| &m[i]);
        // Project each row, retaining the input row so ORDER BY can reference
        // input columns (which need not appear in the select list). A projection
        // containing a set-returning `UNNEST(...)` expands each input row into one
        // output row per array element (parallel UNNESTs expand in lockstep).
        let has_unnest = select
            .projection
            .iter()
            .any(|it| matches!(it, SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } | SelectItem::ExprWithAliases { expr: e, .. } if is_unnest(e)));
        // (input row index, output tuple) so ORDER BY can consult both.
        let mut paired: Vec<(usize, Tuple)> = Vec::with_capacity(input.rows.len());
        if !input.rows.is_empty() {
            // Build the outer-frame prefix once; reuse by updating the last slot
            // each iteration instead of allocating a new Vec<Frame> per row.
            let mut frames: Vec<Frame> = outer
                .iter()
                .map(|f| Frame {
                    schema: f.schema,
                    row: f.row,
                })
                .collect();
            frames.push(Frame {
                schema: &input.schema,
                row: &input.rows[0],
            });
            let last_idx = frames.len() - 1;
            for (ri, row) in input.rows.iter().enumerate() {
                frames[last_idx] = Frame {
                    schema: &input.schema,
                    row,
                };
                if has_unnest {
                    for out in self.project_row_unnest(select, &frames, win_at(ri))? {
                        paired.push((ri, out));
                    }
                } else {
                    let out = self.project_row(select, &input.schema, &frames, win_at(ri))?;
                    paired.push((ri, out));
                }
            }
        }

        if let Some(OrderBy {
            kind: OrderByKind::Expressions(exprs),
            ..
        }) = order_by
        {
            let directions: Vec<(bool, bool)> = exprs
                .iter()
                .map(|o| {
                    let asc = o.options.asc.unwrap_or(true);
                    (asc, o.options.nulls_first.unwrap_or(!asc))
                })
                .collect();
            let mut keyed: Vec<(Vec<SqlValue>, (usize, Tuple))> = Vec::with_capacity(paired.len());
            for (ri, out) in paired {
                let mut keys = Vec::with_capacity(exprs.len());
                for o in exprs {
                    keys.push(self.order_key_paired(
                        &o.expr,
                        &out_schema,
                        &out,
                        &input.schema,
                        &input.rows[ri],
                        win_at(ri),
                        outer,
                    )?);
                }
                keyed.push((keys, (ri, out)));
            }
            keyed.sort_by(|a, b| {
                for (i, (asc, nf)) in directions.iter().enumerate() {
                    let ord = compare_sort(&a.0[i], &b.0[i], *asc, *nf);
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
                Ordering::Equal
            });
            paired = keyed.into_iter().map(|(_, p)| p).collect();
        }

        let mut out_rows: Vec<Tuple> = paired.into_iter().map(|(_, o)| o).collect();
        if distinct {
            out_rows = dedupe(out_rows);
        }
        Ok(RowSet {
            schema: out_schema,
            rows: out_rows,
        })
    }

    /// Expand a projection containing `UNNEST(...)` into multiple output rows.
    fn project_row_unnest(
        &self,
        select: &Select,
        frames: &[Frame],
        win: Option<&HashMap<String, SqlValue>>,
    ) -> Result<Vec<Tuple>> {
        enum Col {
            Array(Vec<SqlValue>),
            Scalar(SqlValue),
        }
        let mut cols = Vec::new();
        let mut max_len = 0usize;
        for item in &select.projection {
            let expr = match item {
                SelectItem::UnnamedExpr(e)
                | SelectItem::ExprWithAlias { expr: e, .. }
                | SelectItem::ExprWithAliases { expr: e, .. } => e,
                _ => {
                    return Err(SqlError::FeatureNotSupported(
                        "wildcard with UNNEST is not supported".into(),
                    ));
                }
            };
            if let Some(arg) = unnest_arg(expr) {
                let values = match self.eval_opt_agg(arg, frames, win)? {
                    SqlValue::Array(items) => items,
                    SqlValue::Null => Vec::new(),
                    single => vec![single],
                };
                max_len = max_len.max(values.len());
                cols.push(Col::Array(values));
            } else {
                cols.push(Col::Scalar(self.eval_opt_agg(expr, frames, win)?));
            }
        }
        let mut rows = Vec::with_capacity(max_len);
        for i in 0..max_len {
            let tuple = cols
                .iter()
                .map(|c| match c {
                    Col::Array(a) => a.get(i).cloned().unwrap_or(SqlValue::Null),
                    Col::Scalar(s) => s.clone(),
                })
                .collect();
            rows.push(tuple);
        }
        Ok(rows)
    }

    /// Resolve an ORDER BY key against output aliases/positions, falling back to
    /// the pre-projection input columns (with any window values in scope).
    #[allow(clippy::too_many_arguments)]
    fn order_key_paired(
        &self,
        expr: &Expr,
        out_schema: &RowSchema,
        out_row: &Tuple,
        in_schema: &RowSchema,
        in_row: &Tuple,
        win: Option<&HashMap<String, SqlValue>>,
        outer: &[Frame],
    ) -> Result<SqlValue> {
        if let Expr::Value(v) = expr
            && let sqlparser::ast::Value::Number(n, _) = &v.value
            && let Ok(pos) = n.parse::<usize>()
            && pos >= 1
            && pos <= out_row.len()
        {
            return Ok(out_row[pos - 1].clone());
        }
        if let Expr::Identifier(ident) = expr {
            let name = ident_name(ident);
            if let Some(i) = out_schema.fields.iter().position(|f| f.name == name) {
                return Ok(out_row[i].clone());
            }
        }
        let mut frames: Vec<Frame> = outer
            .iter()
            .map(|f| Frame {
                schema: f.schema,
                row: f.row,
            })
            .collect();
        frames.push(Frame {
            schema: in_schema,
            row: in_row,
        });
        self.eval_opt_agg(expr, &frames, win)
    }

    /// Expand projection into concrete output columns (names + types).
    fn projection_columns(&self, select: &Select, input: &RowSchema) -> Result<Vec<OutCol>> {
        let mut cols = Vec::new();
        for item in &select.projection {
            match item {
                SelectItem::Wildcard(_) => {
                    for f in &input.fields {
                        cols.push(OutCol {
                            name: f.name.clone(),
                            ty: f.ty.clone(),
                        });
                    }
                }
                SelectItem::QualifiedWildcard(kind, _) => {
                    let table = qualified_wildcard_table(kind);
                    for f in &input.fields {
                        if f.table.as_deref() == Some(table.as_str()) {
                            cols.push(OutCol {
                                name: f.name.clone(),
                                ty: f.ty.clone(),
                            });
                        }
                    }
                }
                SelectItem::UnnamedExpr(e) => {
                    cols.push(OutCol {
                        name: default_col_name(e),
                        ty: self.infer_type(e, input),
                    });
                }
                SelectItem::ExprWithAlias { expr, alias } => {
                    cols.push(OutCol {
                        name: ident_name(alias),
                        ty: self.infer_type(expr, input),
                    });
                }
                SelectItem::ExprWithAliases { expr, aliases } => {
                    let name = aliases
                        .first()
                        .map(ident_name)
                        .unwrap_or_else(|| default_col_name(expr));
                    cols.push(OutCol {
                        name,
                        ty: self.infer_type(expr, input),
                    });
                }
            }
        }
        Ok(cols)
    }

    fn project_row(
        &self,
        select: &Select,
        input: &RowSchema,
        frames: &[Frame],
        win: Option<&HashMap<String, SqlValue>>,
    ) -> Result<Tuple> {
        let mut tuple = Vec::new();
        let row = frames.last().unwrap().row;
        for item in &select.projection {
            match item {
                SelectItem::Wildcard(_) => {
                    tuple.extend(row.iter().cloned());
                }
                SelectItem::QualifiedWildcard(kind, _) => {
                    let table = qualified_wildcard_table(kind);
                    for (i, f) in input.fields.iter().enumerate() {
                        if f.table.as_deref() == Some(table.as_str()) {
                            tuple.push(row[i].clone());
                        }
                    }
                }
                SelectItem::UnnamedExpr(e)
                | SelectItem::ExprWithAlias { expr: e, .. }
                | SelectItem::ExprWithAliases { expr: e, .. } => {
                    tuple.push(self.eval_opt_agg(e, frames, win)?);
                }
            }
        }
        Ok(tuple)
    }

    // ---- grouping / aggregation ----------------------------------------

    fn exec_grouped(
        &self,
        select: &Select,
        input: &RowSet,
        group_exprs: &[Expr],
        outer: &[Frame],
        order_by: Option<&OrderBy>,
    ) -> Result<RowSet> {
        // Partition input rows into groups keyed by the group expressions.
        let mut groups: Vec<(Vec<String>, Vec<usize>)> = Vec::new();
        let mut group_index: HashMap<Vec<String>, usize> = HashMap::new();
        if !input.rows.is_empty() {
            // Build the outer-frame prefix once; reuse by updating the last slot
            // each iteration instead of allocating a new Vec<Frame> per row.
            let mut frames: Vec<Frame> = outer
                .iter()
                .map(|f| Frame {
                    schema: f.schema,
                    row: f.row,
                })
                .collect();
            frames.push(Frame {
                schema: &input.schema,
                row: &input.rows[0],
            });
            let last_idx = frames.len() - 1;
            for (ri, row) in input.rows.iter().enumerate() {
                frames[last_idx] = Frame {
                    schema: &input.schema,
                    row,
                };
                let key: Vec<String> = group_exprs
                    .iter()
                    .map(|e| self.eval(e, &frames).map(|v| v.index_key()))
                    .collect::<Result<_>>()?;
                match group_index.get(&key) {
                    Some(&gi) => groups[gi].1.push(ri),
                    None => {
                        group_index.insert(key.clone(), groups.len());
                        groups.push((key, vec![ri]));
                    }
                }
            }
        }
        // No GROUP BY + aggregates over zero rows still yields one (empty) group.
        if group_exprs.is_empty() && groups.is_empty() {
            groups.push((Vec::new(), Vec::new()));
        }

        // Collect the aggregate call expressions present in the query.
        let agg_calls = collect_aggregates(select);

        let cols = self.projection_columns_grouped(select, &input.schema)?;
        let out_schema = RowSchema::new(
            cols.iter()
                .map(|c| FieldRef {
                    table: None,
                    name: c.name.clone(),
                    ty: c.ty.clone(),
                })
                .collect(),
        );

        let order_exprs: &[sqlparser::ast::OrderByExpr] = match order_by.map(|o| &o.kind) {
            Some(OrderByKind::Expressions(e)) => e,
            _ => &[],
        };
        // Phase 1: aggregates + HAVING per group, keeping the representative
        // row (for non-aggregated references, i.e. group key columns).
        let mut survivors: Vec<(Tuple, HashMap<String, SqlValue>)> = Vec::new();
        for (_key, members) in &groups {
            let group_rows: Vec<&Tuple> = members.iter().map(|&i| &input.rows[i]).collect();
            // Compute each aggregate over the group.
            let mut aggs: HashMap<String, SqlValue> = HashMap::new();
            for call in &agg_calls {
                let value = self.eval_aggregate(call, &group_rows, &input.schema, outer)?;
                aggs.insert(call.to_string(), value);
            }
            let rep: Tuple = group_rows
                .first()
                .map(|t| (*t).clone())
                .unwrap_or_else(|| vec![SqlValue::Null; input.schema.len()]);
            let mut frames: Vec<Frame> = outer
                .iter()
                .map(|f| Frame {
                    schema: f.schema,
                    row: f.row,
                })
                .collect();
            frames.push(Frame {
                schema: &input.schema,
                row: &rep,
            });
            if let Some(having) = &select.having
                && self.eval_agg(having, &frames, &aggs)?.truthy() != Some(true)
            {
                continue;
            }
            survivors.push((rep, aggs));
        }

        // Phase 2: window calls run over the surviving groups (PostgreSQL
        // evaluates them after GROUP BY/HAVING); the per-group window results
        // merge into the aggregate map for projection/ORDER BY lookup.
        let rep_rows: Vec<Tuple> = survivors.iter().map(|(rep, _)| rep.clone()).collect();
        let rep_aggs: Vec<HashMap<String, SqlValue>> =
            survivors.iter().map(|(_, a)| a.clone()).collect();
        if let Some(win_maps) = self.compute_window_maps(
            select,
            order_by,
            &input.schema,
            &rep_rows,
            Some(&rep_aggs),
            outer,
        )? {
            for ((_, aggs), win) in survivors.iter_mut().zip(win_maps) {
                aggs.extend(win);
            }
        }

        // Phase 3: (order keys, output tuple) per surviving group, for sorting.
        let mut out_rows: Vec<(Vec<SqlValue>, Tuple)> = Vec::new();
        // Build the outer-frame prefix once; reuse by updating the last slot per
        // group instead of allocating a new Vec<Frame> for every surviving group.
        let mut frames: Vec<Frame> = outer
            .iter()
            .map(|f| Frame {
                schema: f.schema,
                row: f.row,
            })
            .collect();
        if !survivors.is_empty() {
            frames.push(Frame {
                schema: &input.schema,
                row: &survivors[0].0,
            });
        }
        let last_idx = frames.len().saturating_sub(1);
        for (rep, aggs) in &survivors {
            frames[last_idx] = Frame {
                schema: &input.schema,
                row: rep,
            };
            // Projection with aggregates.
            let mut tuple = Vec::new();
            for item in &select.projection {
                match item {
                    SelectItem::UnnamedExpr(e)
                    | SelectItem::ExprWithAlias { expr: e, .. }
                    | SelectItem::ExprWithAliases { expr: e, .. } => {
                        tuple.push(self.eval_agg(e, &frames, aggs)?);
                    }
                    SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => {
                        return Err(SqlError::Syntax(
                            "cannot use * in an aggregate query without GROUP BY columns".into(),
                        ));
                    }
                }
            }
            // ORDER BY keys: positional / output alias / expression over the
            // group representative (aggregates and group columns resolvable).
            let mut keys = Vec::with_capacity(order_exprs.len());
            for ob in order_exprs {
                let key = if let Expr::Value(v) = &ob.expr {
                    match &v.value {
                        sqlparser::ast::Value::Number(n, _) => n
                            .parse::<usize>()
                            .ok()
                            .and_then(|p| tuple.get(p.wrapping_sub(1)).cloned())
                            .map(Ok)
                            .unwrap_or_else(|| self.eval_agg(&ob.expr, &frames, aggs)),
                        _ => self.eval_agg(&ob.expr, &frames, aggs),
                    }
                } else if let Expr::Identifier(ident) = &ob.expr {
                    match out_schema
                        .fields
                        .iter()
                        .position(|f| f.name == ident_name(ident))
                    {
                        Some(i) => Ok(tuple[i].clone()),
                        None => self.eval_agg(&ob.expr, &frames, aggs),
                    }
                } else {
                    self.eval_agg(&ob.expr, &frames, aggs)
                }?;
                keys.push(key);
            }
            out_rows.push((keys, tuple));
        }

        if !order_exprs.is_empty() {
            let directions: Vec<(bool, bool)> = order_exprs
                .iter()
                .map(|o| {
                    let asc = o.options.asc.unwrap_or(true);
                    (asc, o.options.nulls_first.unwrap_or(!asc))
                })
                .collect();
            out_rows.sort_by(|a, b| {
                for (i, (asc, nf)) in directions.iter().enumerate() {
                    let ord = compare_sort(&a.0[i], &b.0[i], *asc, *nf);
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
                Ordering::Equal
            });
        }
        Ok(RowSet {
            schema: out_schema,
            rows: out_rows.into_iter().map(|(_, t)| t).collect(),
        })
    }

    fn projection_columns_grouped(
        &self,
        select: &Select,
        input: &RowSchema,
    ) -> Result<Vec<OutCol>> {
        let mut cols = Vec::new();
        for item in &select.projection {
            match item {
                SelectItem::UnnamedExpr(e) => {
                    cols.push(OutCol {
                        name: default_col_name(e),
                        ty: self.infer_type_agg(e, input),
                    });
                }
                SelectItem::ExprWithAlias { expr, alias } => {
                    cols.push(OutCol {
                        name: ident_name(alias),
                        ty: self.infer_type_agg(expr, input),
                    });
                }
                SelectItem::ExprWithAliases { expr, aliases } => {
                    let name = aliases
                        .first()
                        .map(ident_name)
                        .unwrap_or_else(|| default_col_name(expr));
                    cols.push(OutCol {
                        name,
                        ty: self.infer_type_agg(expr, input),
                    });
                }
                _ => {
                    return Err(SqlError::Syntax(
                        "cannot use * in an aggregate query".into(),
                    ));
                }
            }
        }
        Ok(cols)
    }

    fn eval_aggregate(
        &self,
        call: &sqlparser::ast::Function,
        group_rows: &[&Tuple],
        input_schema: &RowSchema,
        outer: &[Frame],
    ) -> Result<SqlValue> {
        let name = call
            .name
            .0
            .last()
            .and_then(|p| p.as_ident())
            .map(ident_name)
            .unwrap_or_default();
        let (distinct, arg_expr, is_star) = aggregate_arg(call)?;

        // Gather the argument values across the group (skipping NULLs, like
        // SQL), honouring an attached `FILTER (WHERE ...)` clause.
        let mut values: Vec<SqlValue> = Vec::new();
        let mut count_all = 0usize;
        if !group_rows.is_empty() {
            // Build the outer-frame prefix once; reuse by updating the last slot
            // each iteration instead of allocating a new Vec<Frame> per group row.
            let mut frames: Vec<Frame> = outer
                .iter()
                .map(|f| Frame {
                    schema: f.schema,
                    row: f.row,
                })
                .collect();
            frames.push(Frame {
                schema: input_schema,
                row: group_rows[0],
            });
            let last_idx = frames.len() - 1;
            for row in group_rows {
                frames[last_idx] = Frame {
                    schema: input_schema,
                    row,
                };
                if let Some(filter) = &call.filter
                    && self.eval(filter, &frames)?.truthy() != Some(true)
                {
                    continue;
                }
                count_all += 1;
                if is_star {
                    continue;
                }
                let expr = arg_expr.as_ref().unwrap();
                let v = self.eval(expr, &frames)?;
                if !v.is_null() {
                    values.push(v);
                }
            }
        }
        if distinct {
            values = dedupe_values(values);
        }
        self.fold_aggregate(&name, call, is_star, values, count_all)
    }

    /// Fold already-gathered (non-NULL) argument values into an aggregate
    /// result. Shared by GROUP BY aggregation and window aggregates.
    pub(crate) fn fold_aggregate(
        &self,
        name: &str,
        call: &sqlparser::ast::Function,
        is_star: bool,
        values: Vec<SqlValue>,
        count_all: usize,
    ) -> Result<SqlValue> {
        use rust_decimal::Decimal;
        let out = match name {
            "count" => {
                if is_star {
                    SqlValue::Int8(count_all as i64)
                } else {
                    SqlValue::Int8(values.len() as i64)
                }
            }
            "sum" => {
                if values.is_empty() {
                    SqlValue::Null
                } else if values.iter().all(|v| v.type_of().is_integer()) {
                    SqlValue::Int8(values.iter().filter_map(SqlValue::as_i64).sum())
                } else if values
                    .iter()
                    .any(|v| matches!(v, SqlValue::Float4(_) | SqlValue::Float8(_)))
                {
                    SqlValue::Float8(values.iter().filter_map(SqlValue::as_f64).sum())
                } else {
                    let s: Decimal = values.iter().filter_map(SqlValue::as_decimal).sum();
                    SqlValue::Numeric(s)
                }
            }
            "avg" => {
                if values.is_empty() {
                    SqlValue::Null
                } else if values
                    .iter()
                    .any(|v| matches!(v, SqlValue::Float4(_) | SqlValue::Float8(_)))
                {
                    let s: f64 = values.iter().filter_map(SqlValue::as_f64).sum();
                    SqlValue::Float8(s / values.len() as f64)
                } else {
                    let s: Decimal = values.iter().filter_map(SqlValue::as_decimal).sum();
                    SqlValue::Numeric(s / Decimal::from(values.len() as i64))
                }
            }
            "min" => fold_extreme(&values, true),
            "max" => fold_extreme(&values, false),
            "bool_and" | "every" => {
                if values.is_empty() {
                    SqlValue::Null
                } else {
                    SqlValue::Bool(values.iter().all(|v| v.truthy() == Some(true)))
                }
            }
            "bool_or" => {
                if values.is_empty() {
                    SqlValue::Null
                } else {
                    SqlValue::Bool(values.iter().any(|v| v.truthy() == Some(true)))
                }
            }
            "string_agg" => {
                let sep = aggregate_second_arg(call)
                    .and_then(|e| self.eval(&e, &[]).ok())
                    .and_then(|v| v.to_text())
                    .unwrap_or_else(|| ",".to_string());
                SqlValue::Text(
                    values
                        .iter()
                        .map(|v| v.to_text().unwrap_or_default())
                        .collect::<Vec<_>>()
                        .join(&sep),
                )
            }
            "array_agg" => SqlValue::Array(values),
            other => {
                return Err(SqlError::FeatureNotSupported(format!(
                    "aggregate {other} not supported"
                )));
            }
        };
        Ok(out)
    }

    // ---- ORDER BY / LIMIT ----------------------------------------------

    fn apply_order_by(
        &self,
        rowset: &mut RowSet,
        order_by: &OrderBy,
        outer: &[Frame],
    ) -> Result<()> {
        let exprs = match &order_by.kind {
            OrderByKind::Expressions(exprs) => exprs,
            OrderByKind::All(_) => return Ok(()),
        };
        // Precompute sort keys for each row.
        let mut keyed: Vec<(Vec<SqlValue>, Tuple)> = Vec::with_capacity(rowset.rows.len());
        for row in &rowset.rows {
            let mut keys = Vec::with_capacity(exprs.len());
            for ob in exprs {
                let v = self.eval_order_key(&ob.expr, rowset, row, outer)?;
                keys.push(v);
            }
            keyed.push((keys, row.clone()));
        }
        let directions: Vec<(bool, bool)> = exprs
            .iter()
            .map(|ob| {
                let asc = ob.options.asc.unwrap_or(true);
                let nulls_first = ob.options.nulls_first.unwrap_or(!asc);
                (asc, nulls_first)
            })
            .collect();
        keyed.sort_by(|a, b| {
            for (i, (asc, nulls_first)) in directions.iter().enumerate() {
                let ord = compare_sort(&a.0[i], &b.0[i], *asc, *nulls_first);
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            Ordering::Equal
        });
        rowset.rows = keyed.into_iter().map(|(_, t)| t).collect();
        Ok(())
    }

    /// Evaluate an ORDER BY key, supporting references to output column aliases,
    /// 1-based output positions, and arbitrary expressions over the output row.
    fn eval_order_key(
        &self,
        expr: &Expr,
        rowset: &RowSet,
        row: &Tuple,
        outer: &[Frame],
    ) -> Result<SqlValue> {
        // ORDER BY <positional integer>.
        if let Expr::Value(v) = expr
            && let sqlparser::ast::Value::Number(n, _) = &v.value
            && let Ok(pos) = n.parse::<usize>()
            && pos >= 1
            && pos <= row.len()
        {
            return Ok(row[pos - 1].clone());
        }
        // ORDER BY <output alias>.
        if let Expr::Identifier(ident) = expr {
            let name = ident_name(ident);
            if let Some(i) = rowset.schema.fields.iter().position(|f| f.name == name) {
                return Ok(row[i].clone());
            }
        }
        // Otherwise evaluate against the output row schema.
        let mut frames: Vec<Frame> = outer
            .iter()
            .map(|f| Frame {
                schema: f.schema,
                row: f.row,
            })
            .collect();
        frames.push(Frame {
            schema: &rowset.schema,
            row,
        });
        self.eval(expr, &frames)
    }

    fn apply_limit(
        &self,
        rowset: &mut RowSet,
        limit: Option<&LimitClause>,
        outer: &[Frame],
    ) -> Result<()> {
        let (limit_expr, offset_expr) = match limit {
            None => (None, None),
            Some(LimitClause::LimitOffset { limit, offset, .. }) => {
                (limit.clone(), offset.as_ref().map(|o| o.value.clone()))
            }
            Some(LimitClause::OffsetCommaLimit { offset, limit }) => {
                (Some(limit.clone()), Some(offset.clone()))
            }
        };
        let offset = match offset_expr {
            Some(e) => self.eval(&e, outer)?.as_i64().unwrap_or(0).max(0) as usize,
            None => 0,
        };
        if offset > 0 {
            rowset.rows = rowset.rows.split_off(offset.min(rowset.rows.len()));
        }
        if let Some(e) = limit_expr {
            let lim = self.eval(&e, outer)?.as_i64().unwrap_or(0).max(0) as usize;
            rowset.rows.truncate(lim);
        }
        Ok(())
    }

    // ---- type inference for RowDescription -----------------------------

    pub(crate) fn infer_type(&self, expr: &Expr, input: &RowSchema) -> SqlType {
        match expr {
            Expr::Identifier(ident) => input
                .resolve(None, &ident_name(ident))
                .ok()
                .map(|i| input.fields[i].ty.clone())
                .unwrap_or(SqlType::Text),
            Expr::CompoundIdentifier(parts) => {
                let names: Vec<String> = parts.iter().map(ident_name).collect();
                let (t, c) = match names.as_slice() {
                    [c] => (None, c.clone()),
                    [.., t, c] => (Some(t.clone()), c.clone()),
                    _ => (None, String::new()),
                };
                input
                    .resolve(t.as_deref(), &c)
                    .ok()
                    .map(|i| input.fields[i].ty.clone())
                    .unwrap_or(SqlType::Text)
            }
            Expr::Value(v) => match &v.value {
                sqlparser::ast::Value::Number(n, _) => {
                    if n.contains(['.', 'e', 'E']) {
                        SqlType::Numeric {
                            precision: None,
                            scale: None,
                        }
                    } else {
                        SqlType::Integer
                    }
                }
                sqlparser::ast::Value::Boolean(_) => SqlType::Boolean,
                sqlparser::ast::Value::Null => SqlType::Text,
                _ => SqlType::Text,
            },
            Expr::Cast { data_type, .. } => {
                crate::sql::eval::parse_data_type(data_type).unwrap_or(SqlType::Text)
            }
            Expr::BinaryOp { op, left, right } => {
                use sqlparser::ast::BinaryOperator::*;
                match op {
                    Eq | NotEq | Gt | Lt | GtEq | LtEq | And | Or | Spaceship => SqlType::Boolean,
                    StringConcat => SqlType::Text,
                    Plus | Minus | Multiply | Divide | Modulo => {
                        let lt = self.infer_type(left, input);
                        let rt = self.infer_type(right, input);
                        if lt.is_integer() && rt.is_integer() {
                            SqlType::BigInt
                        } else if matches!(lt, SqlType::DoublePrecision | SqlType::Real)
                            || matches!(rt, SqlType::DoublePrecision | SqlType::Real)
                        {
                            SqlType::DoublePrecision
                        } else {
                            SqlType::Numeric {
                                precision: None,
                                scale: None,
                            }
                        }
                    }
                    _ => SqlType::Text,
                }
            }
            Expr::IsNull(_)
            | Expr::IsNotNull(_)
            | Expr::Between { .. }
            | Expr::InList { .. }
            | Expr::Like { .. }
            | Expr::ILike { .. }
            | Expr::Exists { .. }
            | Expr::IsTrue(_)
            | Expr::IsFalse(_) => SqlType::Boolean,
            Expr::Nested(e) => self.infer_type(e, input),
            Expr::Function(f) if f.over.is_some() => self.infer_window_type(f, input),
            Expr::Function(f) => self.infer_function_type(f),
            Expr::Case {
                conditions,
                else_result,
                ..
            } => conditions
                .first()
                .map(|w| self.infer_type(&w.result, input))
                .or_else(|| else_result.as_ref().map(|e| self.infer_type(e, input)))
                .unwrap_or(SqlType::Text),
            _ => SqlType::Text,
        }
    }

    /// Result type of a window call, for RowDescription purposes.
    fn infer_window_type(&self, f: &sqlparser::ast::Function, input: &RowSchema) -> SqlType {
        let name = f
            .name
            .0
            .last()
            .and_then(|p| p.as_ident())
            .map(ident_name)
            .unwrap_or_default();
        let first_arg_type = || {
            if let FunctionArguments::List(list) = &f.args
                && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(e))) = list.args.first()
            {
                return self.infer_type(e, input);
            }
            SqlType::Text
        };
        match name.as_str() {
            "row_number" | "rank" | "dense_rank" | "count" => SqlType::BigInt,
            "percent_rank" | "cume_dist" => SqlType::DoublePrecision,
            "ntile" => SqlType::Integer,
            "sum" | "avg" => SqlType::Numeric {
                precision: None,
                scale: None,
            },
            "bool_and" | "bool_or" | "every" => SqlType::Boolean,
            "string_agg" => SqlType::Text,
            "lag" | "lead" | "first_value" | "last_value" | "nth_value" | "min" | "max" => {
                first_arg_type()
            }
            _ => SqlType::Text,
        }
    }

    fn infer_type_agg(&self, expr: &Expr, input: &RowSchema) -> SqlType {
        if let Expr::Function(f) = expr {
            let name = f
                .name
                .0
                .last()
                .and_then(|p| p.as_ident())
                .map(ident_name)
                .unwrap_or_default();
            match name.as_str() {
                "count" => return SqlType::BigInt,
                "sum" | "avg" => {
                    return SqlType::Numeric {
                        precision: None,
                        scale: None,
                    };
                }
                "bool_and" | "bool_or" | "every" => return SqlType::Boolean,
                "string_agg" => return SqlType::Text,
                "min" | "max" => {
                    if let FunctionArguments::List(list) = &f.args
                        && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(e))) =
                            list.args.first()
                    {
                        return self.infer_type(e, input);
                    }
                }
                _ => {}
            }
        }
        self.infer_type(expr, input)
    }

    fn infer_function_type(&self, f: &sqlparser::ast::Function) -> SqlType {
        let name = f
            .name
            .0
            .last()
            .and_then(|p| p.as_ident())
            .map(ident_name)
            .unwrap_or_default();
        match name.as_str() {
            "now"
            | "current_timestamp"
            | "transaction_timestamp"
            | "statement_timestamp"
            | "clock_timestamp" => SqlType::Timestamptz,
            "current_date" => SqlType::Date,
            "current_time" => SqlType::Time,
            "count" => SqlType::BigInt,
            "length" | "char_length" | "character_length" | "octet_length" | "position"
            | "array_length" | "cardinality" => SqlType::Integer,
            "gen_random_uuid" | "uuid_generate_v4" => SqlType::Uuid,
            "upper" | "lower" | "trim" | "btrim" | "ltrim" | "rtrim" | "concat" | "concat_ws"
            | "substr" | "substring" | "replace" | "current_user" | "session_user"
            | "current_schema" | "current_database" | "version" | "format_type" => SqlType::Text,
            "abs" | "ceil" | "ceiling" | "floor" | "round" | "sum" | "avg" => SqlType::Numeric {
                precision: None,
                scale: None,
            },
            _ => SqlType::Text,
        }
    }

    // ---- WITH (CTE) materialization -------------------------------------

    /// Materialize a statement-level `WITH` clause into `self.cte`, in order.
    /// `WITH RECURSIVE` members that reference themselves iterate to a
    /// fixpoint; all other members materialize once, non-recursively.
    pub(crate) fn materialize_with(&mut self, with: &sqlparser::ast::With) -> Result<()> {
        let names: Vec<String> = with
            .cte_tables
            .iter()
            .map(|c| ident_name(&c.alias.name))
            .collect();
        for (i, cte) in with.cte_tables.iter().enumerate() {
            let name = &names[i];
            // A recursive WITH item may reference itself and items defined
            // before it; a reference to a later item is mutual recursion.
            if with.recursive
                && let Some(later) = names[i + 1..]
                    .iter()
                    .find(|n| n.as_str() != name && query_references(&cte.query, n))
            {
                return Err(SqlError::FeatureNotSupported(format!(
                    "mutual recursion between WITH items \"{name}\" and \"{later}\" is not \
                     implemented"
                )));
            }
            let rs = if with.recursive && query_references(&cte.query, name) {
                self.exec_recursive_cte(name, cte)?
            } else {
                self.exec_select_query(&cte.query, &[])?
            };
            let rs = label_cte(rs, name, &cte.alias.columns)?;
            self.cte.insert(name.clone(), rs);
        }
        Ok(())
    }

    /// Iterate a recursive CTE to its fixpoint. PostgreSQL semantics: the
    /// recursive term sees only the rows produced by the *previous* iteration
    /// (the working table), never the full accumulation; `UNION` (without ALL)
    /// dedups new rows against everything accumulated so far.
    fn exec_recursive_cte(&mut self, name: &str, cte: &sqlparser::ast::Cte) -> Result<RowSet> {
        let query = &cte.query;
        if query.order_by.is_some() {
            return Err(SqlError::FeatureNotSupported(
                "ORDER BY in a recursive query is not implemented".into(),
            ));
        }
        if query.limit_clause.is_some() {
            return Err(SqlError::FeatureNotSupported(
                "LIMIT/OFFSET in a recursive query is not implemented".into(),
            ));
        }
        let SetExpr::SetOperation {
            op: SetOperator::Union,
            set_quantifier,
            left,
            right,
        } = query.body.as_ref()
        else {
            return Err(SqlError::InvalidRecursion(format!(
                "recursive query \"{name}\" does not have the form non-recursive-term UNION \
                 [ALL] recursive-term"
            )));
        };
        if setexpr_references(left, name) {
            return Err(SqlError::InvalidRecursion(format!(
                "recursive reference to query \"{name}\" must not appear within its \
                 non-recursive term"
            )));
        }
        validate_recursive_term(right, name)?;
        let union_all = matches!(
            set_quantifier,
            sqlparser::ast::SetQuantifier::All | sqlparser::ast::SetQuantifier::AllByName
        );

        // The base (non-recursive) term fixes the CTE's column names and
        // types; recursive-term rows are coerced to those types (or error).
        let base = self.exec_set_expr(left, &[])?;
        let mut schema = label_cte(
            RowSet {
                schema: base.schema,
                rows: Vec::new(),
            },
            name,
            &cte.alias.columns,
        )?
        .schema;
        let mut types: Vec<SqlType> = schema.fields.iter().map(|f| f.ty.clone()).collect();
        // Static inference falls back to Text for expressions it cannot type;
        // trust the first base row's actual value types there instead, so a
        // numeric column is not spuriously coerced through text.
        if let Some(first) = base.rows.first() {
            for (i, ty) in types.iter_mut().enumerate() {
                if *ty == SqlType::Text
                    && let Some(v) = first.get(i)
                    && !v.is_null()
                    && v.type_of() != SqlType::Text
                {
                    *ty = v.type_of();
                }
            }
            for (f, ty) in schema.fields.iter_mut().zip(&types) {
                f.ty = ty.clone();
            }
        }

        let mut seen: std::collections::HashSet<Vec<String>> = std::collections::HashSet::new();
        let mut acc: Vec<Tuple> = Vec::new();
        let mut working: Vec<Tuple> = Vec::new();
        let absorb = |rows: Vec<Tuple>,
                      acc: &mut Vec<Tuple>,
                      working: &mut Vec<Tuple>,
                      seen: &mut std::collections::HashSet<Vec<String>>|
         -> Result<()> {
            for row in rows {
                let row = coerce_recursive_row(row, &types, name)?;
                if union_all || seen.insert(row.iter().map(|v| v.index_key()).collect()) {
                    acc.push(row.clone());
                    working.push(row);
                }
            }
            Ok(())
        };
        absorb(base.rows, &mut acc, &mut working, &mut seen)?;

        // Iteration guard: bounded by an iteration cap (session-settable via
        // `guardian.recursive_max_iterations`, default 100_000) and a fixed
        // 10M-row cap, so runaway recursion errors instead of hanging.
        let max_iterations: u64 = self
            .vars
            .borrow()
            .get("guardian.recursive_max_iterations")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(100_000);
        const MAX_ROWS: usize = 10_000_000;
        let mut iterations: u64 = 0;
        while !working.is_empty() {
            iterations += 1;
            if iterations > max_iterations {
                self.cte.remove(name);
                return Err(SqlError::StatementTooComplex(format!(
                    "recursive query \"{name}\" exceeded {max_iterations} iterations (set \
                     guardian.recursive_max_iterations to raise the limit)"
                )));
            }
            // Publish the working table under the CTE name so the recursive
            // term's self-reference resolves to it.
            self.cte.insert(
                name.to_string(),
                RowSet {
                    schema: schema.clone(),
                    rows: std::mem::take(&mut working),
                },
            );
            let out = match self.exec_set_expr(right, &[]) {
                Ok(out) => out,
                Err(e) => {
                    self.cte.remove(name);
                    return Err(e);
                }
            };
            if let Err(e) = absorb(out.rows, &mut acc, &mut working, &mut seen) {
                self.cte.remove(name);
                return Err(e);
            }
            if acc.len() > MAX_ROWS {
                self.cte.remove(name);
                return Err(SqlError::StatementTooComplex(format!(
                    "recursive query \"{name}\" produced more than {MAX_ROWS} rows"
                )));
            }
        }
        self.cte.remove(name);
        Ok(RowSet { schema, rows: acc })
    }
}

// ---------------------------------------------------------------------------
// Recursive-CTE helpers
// ---------------------------------------------------------------------------

/// Label a materialized CTE: every field belongs to the CTE name, renamed by
/// the optional column-alias list (`WITH c(a, b) AS ...`).
fn label_cte(
    mut rs: RowSet,
    name: &str,
    columns: &[sqlparser::ast::TableAliasColumnDef],
) -> Result<RowSet> {
    if !columns.is_empty() && columns.len() > rs.schema.fields.len() {
        return Err(SqlError::Syntax(format!(
            "table \"{name}\" has {} columns available but {} columns specified",
            rs.schema.fields.len(),
            columns.len()
        )));
    }
    for (i, f) in rs.schema.fields.iter_mut().enumerate() {
        f.table = Some(name.to_string());
        if let Some(c) = columns.get(i) {
            f.name = ident_name(&c.name);
        }
    }
    rs.schema.rebuild_lookup();
    Ok(rs)
}

/// Coerce a recursive-term row to the column types fixed by the base term.
fn coerce_recursive_row(row: Tuple, types: &[SqlType], name: &str) -> Result<Tuple> {
    if row.len() != types.len() {
        return Err(SqlError::Syntax(format!(
            "recursive query \"{name}\": each UNION query must have the same number of columns"
        )));
    }
    row.into_iter()
        .zip(types)
        .map(|(v, ty)| {
            if v.is_null() || v.type_of() == *ty {
                Ok(v)
            } else {
                v.cast(ty)
            }
        })
        .collect()
}

/// Structural guards on the recursive term, mirroring PostgreSQL's rules: the
/// self-reference must appear exactly once, directly in FROM (not inside a
/// subquery or the nullable side of an outer join), and the term must not
/// aggregate or dedup.
fn validate_recursive_term(term: &SetExpr, name: &str) -> Result<()> {
    let SetExpr::Select(sel) = term else {
        return Err(SqlError::InvalidRecursion(format!(
            "recursive query \"{name}\" does not have the form non-recursive-term UNION [ALL] \
             recursive-term"
        )));
    };
    if select_has_aggregate(sel) {
        return Err(SqlError::InvalidRecursion(
            "aggregate functions are not allowed in a recursive query's recursive term".into(),
        ));
    }
    if let GroupByExpr::Expressions(exprs, _) = &sel.group_by
        && !exprs.is_empty()
    {
        return Err(SqlError::FeatureNotSupported(
            "GROUP BY in a recursive query's recursive term is not implemented".into(),
        ));
    }
    if sel.distinct.is_some() {
        return Err(SqlError::FeatureNotSupported(
            "DISTINCT in a recursive query is not implemented".into(),
        ));
    }
    let outer_join_err = || {
        Err(SqlError::InvalidRecursion(format!(
            "recursive reference to query \"{name}\" must not appear within an outer join"
        )))
    };
    let mut count = 0usize;
    for twj in &sel.from {
        let mut chain_has = count_recursive_refs(&twj.relation, name, &mut count)?;
        for join in &twj.joins {
            let before = count;
            let right_has =
                count_recursive_refs(&join.relation, name, &mut count)? || count > before;
            use sqlparser::ast::JoinOperator;
            match &join.join_operator {
                JoinOperator::Left(_) | JoinOperator::LeftOuter(_) if right_has => {
                    return outer_join_err();
                }
                JoinOperator::Right(_) | JoinOperator::RightOuter(_) if chain_has => {
                    return outer_join_err();
                }
                JoinOperator::FullOuter(_) if chain_has || right_has => {
                    return outer_join_err();
                }
                _ => {}
            }
            chain_has = chain_has || right_has;
        }
    }
    // The self-reference must not hide inside expression subqueries either.
    if select_expr_subquery_references(sel, name) {
        return Err(SqlError::InvalidRecursion(format!(
            "recursive reference to query \"{name}\" must not appear within a subquery"
        )));
    }
    match count {
        1 => Ok(()),
        0 => Err(SqlError::InvalidRecursion(format!(
            "recursive query \"{name}\" does not have the form non-recursive-term UNION [ALL] \
             recursive-term"
        ))),
        _ => Err(SqlError::InvalidRecursion(format!(
            "recursive reference to query \"{name}\" must not appear more than once"
        ))),
    }
}

/// Count direct FROM references to the CTE `name` inside a table factor,
/// erroring if the reference occurs inside a derived subquery.
fn count_recursive_refs(tf: &TableFactor, name: &str, count: &mut usize) -> Result<bool> {
    match tf {
        TableFactor::Table { name: obj, .. } => {
            let parts = object_name_parts(obj);
            if parts.len() == 1 && parts[0] == name {
                *count += 1;
                return Ok(true);
            }
            Ok(false)
        }
        TableFactor::Derived { subquery, .. } => {
            if query_references(subquery, name) {
                return Err(SqlError::InvalidRecursion(format!(
                    "recursive reference to query \"{name}\" must not appear within a subquery"
                )));
            }
            Ok(false)
        }
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => {
            let mut has = count_recursive_refs(&table_with_joins.relation, name, count)?;
            for j in &table_with_joins.joins {
                has |= count_recursive_refs(&j.relation, name, count)?;
            }
            Ok(has)
        }
        _ => Ok(false),
    }
}

/// Does this query reference `name` as an unqualified table anywhere —
/// including derived tables and expression subqueries?
fn query_references(q: &Query, name: &str) -> bool {
    setexpr_references(&q.body, name)
}

fn setexpr_references(s: &SetExpr, name: &str) -> bool {
    match s {
        SetExpr::Select(sel) => select_references(sel, name),
        SetExpr::Query(q) => query_references(q, name),
        SetExpr::SetOperation { left, right, .. } => {
            setexpr_references(left, name) || setexpr_references(right, name)
        }
        SetExpr::Values(v) => v
            .rows
            .iter()
            .flat_map(|r| &r.content)
            .any(|e| expr_references(e, name)),
        _ => false,
    }
}

fn select_references(sel: &Select, name: &str) -> bool {
    let tf_refs = |tf: &TableFactor| -> bool {
        let mut n = 0usize;
        match count_recursive_refs(tf, name, &mut n) {
            Ok(has) => has || n > 0,
            // A reference hidden in a derived subquery is still a reference.
            Err(_) => true,
        }
    };
    let from = sel.from.iter().any(|twj| {
        tf_refs(&twj.relation)
            || twj.joins.iter().any(|j| {
                tf_refs(&j.relation) || join_on_expr(j).is_some_and(|e| expr_references(e, name))
            })
    });
    from || select_expr_subquery_references(sel, name)
}

/// Does any expression position of this SELECT contain a subquery that
/// references `name`?
fn select_expr_subquery_references(sel: &Select, name: &str) -> bool {
    let mut exprs: Vec<&Expr> = Vec::new();
    for item in &sel.projection {
        if let SelectItem::UnnamedExpr(e)
        | SelectItem::ExprWithAlias { expr: e, .. }
        | SelectItem::ExprWithAliases { expr: e, .. } = item
        {
            exprs.push(e);
        }
    }
    if let Some(e) = &sel.selection {
        exprs.push(e);
    }
    if let Some(e) = &sel.having {
        exprs.push(e);
    }
    if let GroupByExpr::Expressions(gexprs, _) = &sel.group_by {
        exprs.extend(gexprs.iter());
    }
    for twj in &sel.from {
        for j in &twj.joins {
            if let Some(e) = join_on_expr(j) {
                exprs.push(e);
            }
        }
    }
    exprs.into_iter().any(|e| expr_references(e, name))
}

fn join_on_expr(join: &Join) -> Option<&Expr> {
    use sqlparser::ast::JoinOperator;
    let constraint = match &join.join_operator {
        JoinOperator::Inner(c)
        | JoinOperator::Join(c)
        | JoinOperator::Left(c)
        | JoinOperator::LeftOuter(c)
        | JoinOperator::Right(c)
        | JoinOperator::RightOuter(c)
        | JoinOperator::FullOuter(c) => c,
        _ => return None,
    };
    match constraint {
        JoinConstraint::On(e) => Some(e),
        _ => None,
    }
}

/// Does this expression contain a subquery that references `name`?
fn expr_references(e: &Expr, name: &str) -> bool {
    let mut found = false;
    walk_expr(e, &mut |inner| {
        if let Expr::Subquery(q)
        | Expr::Exists { subquery: q, .. }
        | Expr::InSubquery { subquery: q, .. } = inner
            && query_references(q, name)
        {
            found = true;
        }
    });
    found
}

// ---------------------------------------------------------------------------
// Join helpers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum JoinKind {
    Inner,
    Left,
    Right,
    Full,
    Cross,
}

enum JoinPredicate {
    // `Expr` is large (~300 bytes); box it so this transient enum stays small.
    On(Box<Expr>),
    Using(Vec<String>),
    Natural,
    NaturalCols(Vec<String>),
}

/// Find an `column = <non-column-value>` predicate on a single-column-indexed
/// column, descending only through `AND` (never `OR`) and parentheses.
fn find_indexed_equality<'a>(
    expr: &'a Expr,
    loaded: &crate::sql::store::LoadedTable,
) -> Option<(String, &'a Expr)> {
    use sqlparser::ast::BinaryOperator;
    match expr {
        Expr::BinaryOp {
            left,
            op: BinaryOperator::And,
            right,
        } => find_indexed_equality(left, loaded).or_else(|| find_indexed_equality(right, loaded)),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Eq,
            right,
        } => {
            if let Some(col) = column_name(left)
                && is_single_indexed(loaded, &col)
                && !is_column_ref(right)
            {
                return Some((col, right));
            }
            if let Some(col) = column_name(right)
                && is_single_indexed(loaded, &col)
                && !is_column_ref(left)
            {
                return Some((col, left));
            }
            None
        }
        Expr::Nested(e) => find_indexed_equality(e, loaded),
        _ => None,
    }
}

/// If `expr` is a top-level `UNNEST(arg)` call, return its single argument.
fn unnest_arg(expr: &Expr) -> Option<&Expr> {
    if let Expr::Function(f) = expr {
        let name = f
            .name
            .0
            .last()
            .and_then(|p| p.as_ident())
            .map(ident_name)
            .unwrap_or_default();
        if name == "unnest"
            && let FunctionArguments::List(list) = &f.args
            && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(e))) = list.args.first()
        {
            return Some(e);
        }
    }
    None
}

fn is_unnest(expr: &Expr) -> bool {
    unnest_arg(expr).is_some()
}

fn column_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Identifier(i) => Some(ident_name(i)),
        Expr::CompoundIdentifier(parts) => parts.last().map(ident_name),
        _ => None,
    }
}

fn is_column_ref(e: &Expr) -> bool {
    matches!(e, Expr::Identifier(_) | Expr::CompoundIdentifier(_))
}

fn is_single_indexed(loaded: &crate::sql::store::LoadedTable, col: &str) -> bool {
    loaded
        .indexes
        .iter()
        .any(|i| i.meta.columns.len() == 1 && i.meta.columns[0] == col)
}

fn cross_join(left: RowSet, right: RowSet) -> RowSet {
    let schema = left.schema.concat(&right.schema);
    let mut rows = Vec::with_capacity(left.rows.len() * right.rows.len().max(1));
    for l in &left.rows {
        for r in &right.rows {
            let mut combined = l.clone();
            combined.extend(r.iter().cloned());
            rows.push(combined);
        }
    }
    RowSet { schema, rows }
}

/// Build a RowSet from a loaded table, labelling each field with `alias`. When
/// `filter` is given (SKIP LOCKED), only those row ids are included; rows in
/// `rls_hidden` (invisible under row-level security) are always excluded.
fn loaded_to_rowset(
    loaded: &crate::sql::store::LoadedTable,
    alias: &str,
    filter: Option<&std::collections::BTreeSet<String>>,
    rls_hidden: Option<&std::collections::BTreeSet<String>>,
) -> RowSet {
    let fields = loaded
        .meta
        .columns
        .iter()
        .map(|c| FieldRef {
            table: Some(alias.to_string()),
            name: c.name.clone(),
            ty: c.ty.clone(),
        })
        .collect();
    let schema = RowSchema::new(fields);
    let rows = loaded
        .rows
        .iter()
        .filter(|(rid, _)| filter.map(|f| f.contains(*rid)).unwrap_or(true))
        .filter(|(rid, _)| rls_hidden.map(|h| !h.contains(*rid)).unwrap_or(true))
        .map(|(_, values)| {
            loaded
                .meta
                .columns
                .iter()
                .map(|c| values.get(&c.name).cloned().unwrap_or(SqlValue::Null))
                .collect()
        })
        .collect();
    RowSet { schema, rows }
}

/// Relabel a RowSet's fields to a table alias (and optional column aliases).
fn relabel(
    mut rs: RowSet,
    alias: &str,
    table_alias: &Option<sqlparser::ast::TableAlias>,
) -> RowSet {
    let col_aliases: Vec<String> = table_alias
        .as_ref()
        .map(|a| a.columns.iter().map(|c| ident_name(&c.name)).collect())
        .unwrap_or_default();
    for (i, f) in rs.schema.fields.iter_mut().enumerate() {
        f.table = Some(alias.to_string());
        if let Some(name) = col_aliases.get(i) {
            f.name = name.clone();
        }
    }
    rs.schema.rebuild_lookup();
    rs
}

// ---------------------------------------------------------------------------
// Aggregate helpers
// ---------------------------------------------------------------------------

/// Does this expression contain a window function call (`... OVER ...`)?
pub(crate) fn expr_has_window(expr: &Expr) -> bool {
    let mut found = false;
    walk_expr(expr, &mut |e| {
        if let Expr::Function(f) = e
            && f.over.is_some()
        {
            found = true;
        }
    });
    found
}

pub(crate) fn select_has_aggregate(select: &Select) -> bool {
    let mut found = false;
    for item in &select.projection {
        if let SelectItem::UnnamedExpr(e)
        | SelectItem::ExprWithAlias { expr: e, .. }
        | SelectItem::ExprWithAliases { expr: e, .. } = item
            && expr_has_aggregate(e)
        {
            found = true;
        }
    }
    if let Some(h) = &select.having
        && expr_has_aggregate(h)
    {
        found = true;
    }
    found
}

fn expr_has_aggregate(expr: &Expr) -> bool {
    let mut found = false;
    walk_expr(expr, &mut |e| {
        // An aggregate with OVER is a window call, not a plain aggregate; its
        // arguments may still contain plain aggregates (walked separately).
        if let Expr::Function(f) = e
            && f.over.is_none()
        {
            let name = f
                .name
                .0
                .last()
                .and_then(|p| p.as_ident())
                .map(ident_name)
                .unwrap_or_default();
            if funcs::is_aggregate(&name) {
                found = true;
            }
        }
    });
    found
}

fn collect_aggregates(select: &Select) -> Vec<sqlparser::ast::Function> {
    let mut out = Vec::new();
    let mut push = |e: &Expr| {
        walk_expr(e, &mut |inner| {
            // Skip window calls; plain aggregates nested in their arguments
            // are still collected (the walk descends into function args).
            if let Expr::Function(f) = inner
                && f.over.is_none()
            {
                let name = f
                    .name
                    .0
                    .last()
                    .and_then(|p| p.as_ident())
                    .map(ident_name)
                    .unwrap_or_default();
                if funcs::is_aggregate(&name) {
                    out.push(f.clone());
                }
            }
        });
    };
    for item in &select.projection {
        if let SelectItem::UnnamedExpr(e)
        | SelectItem::ExprWithAlias { expr: e, .. }
        | SelectItem::ExprWithAliases { expr: e, .. } = item
        {
            push(e);
        }
    }
    if let Some(h) = &select.having {
        push(h);
    }
    out
}

/// Extract `(distinct, single_arg_expr, is_star)` from an aggregate call.
pub(crate) fn aggregate_arg(call: &sqlparser::ast::Function) -> Result<(bool, Option<Expr>, bool)> {
    match &call.args {
        FunctionArguments::List(list) => {
            let distinct = matches!(
                list.duplicate_treatment,
                Some(sqlparser::ast::DuplicateTreatment::Distinct)
            );
            match list.args.first() {
                None => Ok((distinct, None, false)),
                Some(FunctionArg::Unnamed(FunctionArgExpr::Wildcard)) => Ok((distinct, None, true)),
                Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(e))) => {
                    Ok((distinct, Some(e.clone()), false))
                }
                _ => Err(SqlError::FeatureNotSupported("aggregate argument".into())),
            }
        }
        FunctionArguments::None => Ok((false, None, true)),
        FunctionArguments::Subquery(_) => Err(SqlError::FeatureNotSupported(
            "aggregate over subquery argument".into(),
        )),
    }
}

fn aggregate_second_arg(call: &sqlparser::ast::Function) -> Option<Expr> {
    if let FunctionArguments::List(list) = &call.args
        && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(e))) = list.args.get(1)
    {
        return Some(e.clone());
    }
    None
}

fn fold_extreme(values: &[SqlValue], min: bool) -> SqlValue {
    let mut best: Option<&SqlValue> = None;
    for v in values {
        best = Some(match best {
            None => v,
            Some(cur) => match v.compare(cur) {
                Some(Ordering::Less) if min => v,
                Some(Ordering::Greater) if !min => v,
                _ => cur,
            },
        });
    }
    best.cloned().unwrap_or(SqlValue::Null)
}

// ---------------------------------------------------------------------------
// Misc helpers
// ---------------------------------------------------------------------------

fn dedupe(rows: Vec<Tuple>) -> Vec<Tuple> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        let key: Vec<String> = row.iter().map(|v| v.index_key()).collect();
        if seen.insert(key) {
            out.push(row);
        }
    }
    out
}

fn dedupe_values(values: Vec<SqlValue>) -> Vec<SqlValue> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for v in values {
        if seen.insert(v.index_key()) {
            out.push(v);
        }
    }
    out
}

pub(crate) fn compare_sort(a: &SqlValue, b: &SqlValue, asc: bool, nulls_first: bool) -> Ordering {
    match (a.is_null(), b.is_null()) {
        (true, true) => return Ordering::Equal,
        (true, false) => {
            return if nulls_first {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        (false, true) => {
            return if nulls_first {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
        (false, false) => {}
    }
    let ord = a.compare(b).unwrap_or(Ordering::Equal);
    if asc { ord } else { ord.reverse() }
}

pub(crate) fn default_col_name(expr: &Expr) -> String {
    match expr {
        Expr::Identifier(ident) => ident_name(ident),
        Expr::CompoundIdentifier(parts) => parts
            .last()
            .map(ident_name)
            .unwrap_or_else(|| "?column?".into()),
        Expr::Function(f) => f
            .name
            .0
            .last()
            .and_then(|p| p.as_ident())
            .map(ident_name)
            .unwrap_or_else(|| "?column?".into()),
        Expr::Cast { expr, .. } => default_col_name(expr),
        Expr::Nested(e) => default_col_name(e),
        _ => "?column?".to_string(),
    }
}

fn qualified_wildcard_table(kind: &sqlparser::ast::SelectItemQualifiedWildcardKind) -> String {
    match kind {
        sqlparser::ast::SelectItemQualifiedWildcardKind::ObjectName(name) => name
            .0
            .last()
            .and_then(|p| p.as_ident())
            .map(ident_name)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Recursively visit sub-expressions (shallow set sufficient for aggregate /
/// window / subquery detection in scalar trees; subqueries themselves are not
/// descended into — callers handle `Subquery`/`Exists`/`InSubquery` nodes).
pub(crate) fn walk_expr(expr: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(expr);
    match expr {
        Expr::BinaryOp { left, right, .. } => {
            walk_expr(left, f);
            walk_expr(right, f);
        }
        Expr::UnaryOp { expr, .. }
        | Expr::Nested(expr)
        | Expr::IsNull(expr)
        | Expr::IsNotNull(expr)
        | Expr::IsTrue(expr)
        | Expr::IsNotTrue(expr)
        | Expr::IsFalse(expr)
        | Expr::IsNotFalse(expr)
        | Expr::IsUnknown(expr)
        | Expr::IsNotUnknown(expr)
        | Expr::Collate { expr, .. }
        | Expr::InSubquery { expr, .. }
        | Expr::Cast { expr, .. } => walk_expr(expr, f),
        Expr::IsDistinctFrom(a, b) | Expr::IsNotDistinctFrom(a, b) => {
            walk_expr(a, f);
            walk_expr(b, f);
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            walk_expr(expr, f);
            walk_expr(low, f);
            walk_expr(high, f);
        }
        Expr::InList { expr, list, .. } => {
            walk_expr(expr, f);
            for e in list {
                walk_expr(e, f);
            }
        }
        Expr::Like { expr, pattern, .. }
        | Expr::ILike { expr, pattern, .. }
        | Expr::SimilarTo { expr, pattern, .. } => {
            walk_expr(expr, f);
            walk_expr(pattern, f);
        }
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            if let Some(o) = operand {
                walk_expr(o, f);
            }
            for w in conditions {
                walk_expr(&w.condition, f);
                walk_expr(&w.result, f);
            }
            if let Some(e) = else_result {
                walk_expr(e, f);
            }
        }
        Expr::Function(func) => {
            if let FunctionArguments::List(list) = &func.args {
                for arg in &list.args {
                    if let FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
                    | FunctionArg::Named {
                        arg: FunctionArgExpr::Expr(e),
                        ..
                    }
                    | FunctionArg::ExprNamed {
                        arg: FunctionArgExpr::Expr(e),
                        ..
                    } = arg
                    {
                        walk_expr(e, f);
                    }
                }
            }
        }
        _ => {}
    }
}

// Maintenance note 3: documents compatibility expectations without changing runtime behavior.

// Maintenance note 15: documents compatibility expectations without changing runtime behavior.

// Maintenance note: keeps SQL compatibility behavior explicit for future updates.

// Maintenance note: keeps SQL compatibility behavior explicit for future updates.

// SQL compatibility note 2: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.

// SQL compatibility note 18: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.

// SQL compatibility note 2: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.

// SQL compatibility note 18: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.
