//! The SELECT execution pipeline (synchronous, over pre-loaded tables).

use crate::error::{Result, SqlError};
use crate::exec::{Exec, Frame};
use crate::funcs;
use crate::names::{ident_name, object_name_parts, split_schema_table};
use crate::row::{FieldRef, RowSchema, RowSet, Tuple};
use guardian_relational::{SqlType, SqlValue};
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
    /// Execute a subquery (used by the evaluator), inheriting outer frames.
    pub fn exec_subquery(&self, query: &Query, outer: &[Frame]) -> Result<RowSet> {
        self.exec_select_query(query, outer)
    }

    /// Execute a full `Query` (a SELECT with optional WITH/ORDER BY/LIMIT).
    pub fn exec_select_query(&self, query: &Query, outer: &[Frame]) -> Result<RowSet> {
        // Nested WITH that was not pre-materialized at the statement top level is
        // not supported.
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
            SetExpr::Select(select) => self.exec_select(select, outer, query.order_by.as_ref())?,
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

    fn exec_set_expr(&self, body: &SetExpr, outer: &[Frame]) -> Result<RowSet> {
        match body {
            SetExpr::Select(select) => self.exec_select(select, outer, None),
            SetExpr::Query(q) => self.exec_select_query(q, outer),
            SetExpr::Values(values) => self.exec_values(values, outer),
            SetExpr::SetOperation { left, op, set_quantifier, right } => {
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
        Ok(RowSet { schema: RowSchema::new(fields), rows })
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
        let key = |t: &Tuple| -> Vec<String> {
            t.iter().map(|v| v.index_key()).collect()
        };
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
        Ok(RowSet { schema: left.schema, rows })
    }

    // ---- core single-block SELECT --------------------------------------

    fn exec_select(
        &self,
        select: &Select,
        outer: &[Frame],
        order_by: Option<&OrderBy>,
    ) -> Result<RowSet> {
        // Planner: prefer an index scan when a single base table is filtered by
        // an equality on an indexed column; otherwise fall back to a full scan.
        let from = match self.try_index_scan(select, outer)? {
            Some(rs) => rs,
            None => self.exec_from(&select.from, outer)?,
        };
        let filtered = self.apply_where(from, select.selection.as_ref(), outer)?;
        let group_exprs = match &select.group_by {
            GroupByExpr::Expressions(exprs, _) => exprs.clone(),
            GroupByExpr::All(_) => Vec::new(),
        };
        let has_aggregate = select_has_aggregate(select);
        let distinct = matches!(select.distinct, Some(Distinct::Distinct));

        if !group_exprs.is_empty() || has_aggregate {
            let mut rowset = self.exec_grouped(select, &filtered, &group_exprs, outer)?;
            if distinct {
                rowset.rows = dedupe(std::mem::take(&mut rowset.rows));
            }
            if let Some(ob) = order_by {
                self.apply_order_by(&mut rowset, ob, outer)?;
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
            .map(|c| FieldRef { table: Some(alias_name.clone()), name: c.name.clone(), ty: c.ty.clone() })
            .collect();
        let schema = RowSchema::new(fields);
        let rows = row_ids
            .iter()
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

    fn exec_from(&self, from: &[TableWithJoins], outer: &[Frame]) -> Result<RowSet> {
        if from.is_empty() {
            return Ok(RowSet { schema: RowSchema::default(), rows: vec![vec![]] });
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
            TableFactor::Table { name, alias, args: Some(args), .. } => {
                let arg_exprs: Vec<&Expr> = args
                    .args
                    .iter()
                    .filter_map(|a| match a {
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
                        | FunctionArg::Named { arg: FunctionArgExpr::Expr(e), .. } => Some(e),
                        _ => None,
                    })
                    .collect();
                self.exec_table_function(name, &arg_exprs, alias, outer)
            }
            TableFactor::Function { name, args, alias, .. } => {
                let arg_exprs: Vec<&Expr> = args
                    .iter()
                    .filter_map(|a| match a {
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
                        | FunctionArg::Named { arg: FunctionArgExpr::Expr(e), .. } => Some(e),
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
                if parts.len() == 1 {
                    if let Some(cte) = self.cte.get(&bare) {
                        return Ok(relabel(cte.clone(), &alias_name, alias));
                    }
                }
                // Catalog table?
                let (schema, tname) = split_schema_table(name);
                if let Some(q) = self.catalog.resolve_table_name(schema.as_deref(), &tname) {
                    if let Some(loaded) = self.tables.get(&q) {
                        return Ok(relabel(loaded_to_rowset(loaded, &alias_name), &alias_name, alias));
                    }
                    if let Some(view) = self.catalog.get_view(&q).cloned() {
                        return self.exec_view(&view, &alias_name, alias, outer);
                    }
                }
                // Catalog introspection view?
                if let Some(rs) = crate::catalog_views::view_rows(
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
            TableFactor::Derived { subquery, alias, .. } => {
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
        let fname = name
            .0
            .last()
            .and_then(|p| p.as_ident())
            .map(ident_name)
            .unwrap_or_default();
        if matches!(fname.as_str(), "generate_series" | "unnest" | "jsonb_array_elements" | "json_array_elements") {
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
        let field = FieldRef { table: Some(alias_name), name: col_name, ty: value.type_of() };
        Ok(RowSet { schema: RowSchema::new(vec![field]), rows: vec![vec![value]] })
    }

    fn exec_view(
        &self,
        view: &guardian_relational::catalog::View,
        alias_name: &str,
        alias: &Option<sqlparser::ast::TableAlias>,
        outer: &[Frame],
    ) -> Result<RowSet> {
        let stmts = crate::parser::parse_sql(&view.query)?;
        let query = match stmts.into_iter().next() {
            Some(sqlparser::ast::Statement::Query(q)) => q,
            _ => return Err(SqlError::Internal("view definition is not a query".into())),
        };
        let rs = self.exec_select_query(&query, outer)?;
        Ok(relabel(rs, alias_name, alias))
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
                )))
            }
        };
        if matches!(kind, JoinKind::Cross) {
            return Ok(cross_join(left, right));
        }
        let combined_schema = left.schema.concat(&right.schema);
        let predicate = constraint.and_then(|c| match c {
            JoinConstraint::On(e) => Some(JoinPredicate::On(e.clone())),
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
                if self.join_matches(&predicate, &left.schema, &right.schema, &combined_schema, &combined, outer)? {
                    rows.push(combined);
                    any = true;
                    right_matched[ri] = true;
                }
            }
            if !any && matches!(kind, JoinKind::Left | JoinKind::Full) {
                let mut combined = l.clone();
                combined.extend(std::iter::repeat(SqlValue::Null).take(right_width));
                rows.push(combined);
            }
        }
        if matches!(kind, JoinKind::Right | JoinKind::Full) {
            for (ri, matched) in right_matched.iter().enumerate() {
                if !matched {
                    let mut combined: Tuple =
                        std::iter::repeat(SqlValue::Null).take(left_width).collect();
                    combined.extend(right.rows[ri].iter().cloned());
                    rows.push(combined);
                }
            }
        }
        Ok(RowSet { schema: combined_schema, rows })
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
                let mut frames: Vec<Frame> = outer.iter().map(|f| Frame { schema: f.schema, row: f.row }).collect();
                frames.push(Frame { schema: combined_schema, row: combined });
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
        for row in &input.rows {
            let mut frames: Vec<Frame> = outer.iter().map(|f| Frame { schema: f.schema, row: f.row }).collect();
            frames.push(Frame { schema: &input.schema, row });
            if self.eval(predicate, &frames)?.truthy() == Some(true) {
                rows.push(row.clone());
            }
        }
        Ok(RowSet { schema: input.schema, rows })
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
                .map(|c| FieldRef { table: None, name: c.name.clone(), ty: c.ty.clone() })
                .collect(),
        );
        // Project each row, retaining the input row so ORDER BY can reference
        // input columns (which need not appear in the select list). A projection
        // containing a set-returning `UNNEST(...)` expands each input row into one
        // output row per array element (parallel UNNESTs expand in lockstep).
        let has_unnest = select
            .projection
            .iter()
            .any(|it| matches!(it, SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } | SelectItem::ExprWithAliases { expr: e, .. } if is_unnest(e)));
        let mut paired: Vec<(Tuple, Tuple)> = Vec::with_capacity(input.rows.len());
        for row in &input.rows {
            let mut frames: Vec<Frame> = outer.iter().map(|f| Frame { schema: f.schema, row: f.row }).collect();
            frames.push(Frame { schema: &input.schema, row });
            if has_unnest {
                for out in self.project_row_unnest(select, &frames)? {
                    paired.push((row.clone(), out));
                }
            } else {
                let out = self.project_row(select, &input.schema, &frames)?;
                paired.push((row.clone(), out));
            }
        }

        if let Some(OrderBy { kind: OrderByKind::Expressions(exprs), .. }) = order_by {
            let directions: Vec<(bool, bool)> = exprs
                .iter()
                .map(|o| {
                    let asc = o.options.asc.unwrap_or(true);
                    (asc, o.options.nulls_first.unwrap_or(!asc))
                })
                .collect();
            let mut keyed: Vec<(Vec<SqlValue>, (Tuple, Tuple))> = Vec::with_capacity(paired.len());
            for (inp, out) in paired {
                let mut keys = Vec::with_capacity(exprs.len());
                for o in exprs {
                    keys.push(self.order_key_paired(&o.expr, &out_schema, &out, &input.schema, &inp, outer)?);
                }
                keyed.push((keys, (inp, out)));
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
        Ok(RowSet { schema: out_schema, rows: out_rows })
    }

    /// Expand a projection containing `UNNEST(...)` into multiple output rows.
    fn project_row_unnest(&self, select: &Select, frames: &[Frame]) -> Result<Vec<Tuple>> {
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
                    ))
                }
            };
            if let Some(arg) = unnest_arg(expr) {
                let values = match self.eval(arg, frames)? {
                    SqlValue::Array(items) => items,
                    SqlValue::Null => Vec::new(),
                    single => vec![single],
                };
                max_len = max_len.max(values.len());
                cols.push(Col::Array(values));
            } else {
                cols.push(Col::Scalar(self.eval(expr, frames)?));
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
    /// the pre-projection input columns.
    fn order_key_paired(
        &self,
        expr: &Expr,
        out_schema: &RowSchema,
        out_row: &Tuple,
        in_schema: &RowSchema,
        in_row: &Tuple,
        outer: &[Frame],
    ) -> Result<SqlValue> {
        if let Expr::Value(v) = expr {
            if let sqlparser::ast::Value::Number(n, _) = &v.value {
                if let Ok(pos) = n.parse::<usize>() {
                    if pos >= 1 && pos <= out_row.len() {
                        return Ok(out_row[pos - 1].clone());
                    }
                }
            }
        }
        if let Expr::Identifier(ident) = expr {
            let name = ident_name(ident);
            if let Some(i) = out_schema.fields.iter().position(|f| f.name == name) {
                return Ok(out_row[i].clone());
            }
        }
        let mut frames: Vec<Frame> = outer.iter().map(|f| Frame { schema: f.schema, row: f.row }).collect();
        frames.push(Frame { schema: in_schema, row: in_row });
        self.eval(expr, &frames)
    }

    /// Expand projection into concrete output columns (names + types).
    fn projection_columns(&self, select: &Select, input: &RowSchema) -> Result<Vec<OutCol>> {
        let mut cols = Vec::new();
        for item in &select.projection {
            match item {
                SelectItem::Wildcard(_) => {
                    for f in &input.fields {
                        cols.push(OutCol { name: f.name.clone(), ty: f.ty.clone() });
                    }
                }
                SelectItem::QualifiedWildcard(kind, _) => {
                    let table = qualified_wildcard_table(kind);
                    for f in &input.fields {
                        if f.table.as_deref() == Some(table.as_str()) {
                            cols.push(OutCol { name: f.name.clone(), ty: f.ty.clone() });
                        }
                    }
                }
                SelectItem::UnnamedExpr(e) => {
                    cols.push(OutCol { name: default_col_name(e), ty: self.infer_type(e, input) });
                }
                SelectItem::ExprWithAlias { expr, alias } => {
                    cols.push(OutCol { name: ident_name(alias), ty: self.infer_type(expr, input) });
                }
                SelectItem::ExprWithAliases { expr, aliases } => {
                    let name = aliases.first().map(ident_name).unwrap_or_else(|| default_col_name(expr));
                    cols.push(OutCol { name, ty: self.infer_type(expr, input) });
                }
            }
        }
        Ok(cols)
    }

    fn project_row(&self, select: &Select, input: &RowSchema, frames: &[Frame]) -> Result<Tuple> {
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
                    tuple.push(self.eval(e, frames)?);
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
    ) -> Result<RowSet> {
        // Partition input rows into groups keyed by the group expressions.
        let mut groups: Vec<(Vec<String>, Vec<usize>)> = Vec::new();
        let mut group_index: HashMap<Vec<String>, usize> = HashMap::new();
        for (ri, row) in input.rows.iter().enumerate() {
            let mut frames: Vec<Frame> = outer.iter().map(|f| Frame { schema: f.schema, row: f.row }).collect();
            frames.push(Frame { schema: &input.schema, row });
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
        // No GROUP BY + aggregates over zero rows still yields one (empty) group.
        if group_exprs.is_empty() && groups.is_empty() {
            groups.push((Vec::new(), Vec::new()));
        }

        // Collect the aggregate call expressions present in the query.
        let agg_calls = collect_aggregates(select);

        let cols = self.projection_columns_grouped(select, &input.schema)?;
        let out_schema = RowSchema::new(
            cols.iter()
                .map(|c| FieldRef { table: None, name: c.name.clone(), ty: c.ty.clone() })
                .collect(),
        );

        let mut out_rows = Vec::new();
        for (_key, members) in &groups {
            let group_rows: Vec<&Tuple> = members.iter().map(|&i| &input.rows[i]).collect();
            // Compute each aggregate over the group.
            let mut aggs: HashMap<String, SqlValue> = HashMap::new();
            for call in &agg_calls {
                let value = self.eval_aggregate(call, &group_rows, &input.schema, outer)?;
                aggs.insert(call.to_string(), value);
            }
            // Representative row for non-aggregated references (group key columns).
            let rep: Tuple = group_rows
                .first()
                .map(|t| (*t).clone())
                .unwrap_or_else(|| vec![SqlValue::Null; input.schema.len()]);
            let mut frames: Vec<Frame> = outer.iter().map(|f| Frame { schema: f.schema, row: f.row }).collect();
            frames.push(Frame { schema: &input.schema, row: &rep });

            // HAVING.
            if let Some(having) = &select.having {
                if self.eval_agg(having, &frames, &aggs)?.truthy() != Some(true) {
                    continue;
                }
            }
            // Projection with aggregates.
            let mut tuple = Vec::new();
            for item in &select.projection {
                match item {
                    SelectItem::UnnamedExpr(e)
                    | SelectItem::ExprWithAlias { expr: e, .. }
                    | SelectItem::ExprWithAliases { expr: e, .. } => {
                        tuple.push(self.eval_agg(e, &frames, &aggs)?);
                    }
                    SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => {
                        return Err(SqlError::Syntax(
                            "cannot use * in an aggregate query without GROUP BY columns".into(),
                        ));
                    }
                }
            }
            out_rows.push(tuple);
        }
        Ok(RowSet { schema: out_schema, rows: out_rows })
    }

    fn projection_columns_grouped(&self, select: &Select, input: &RowSchema) -> Result<Vec<OutCol>> {
        let mut cols = Vec::new();
        for item in &select.projection {
            match item {
                SelectItem::UnnamedExpr(e) => {
                    cols.push(OutCol { name: default_col_name(e), ty: self.infer_type_agg(e, input) });
                }
                SelectItem::ExprWithAlias { expr, alias } => {
                    cols.push(OutCol { name: ident_name(alias), ty: self.infer_type_agg(expr, input) });
                }
                SelectItem::ExprWithAliases { expr, aliases } => {
                    let name = aliases.first().map(ident_name).unwrap_or_else(|| default_col_name(expr));
                    cols.push(OutCol { name, ty: self.infer_type_agg(expr, input) });
                }
                _ => {
                    return Err(SqlError::Syntax(
                        "cannot use * in an aggregate query".into(),
                    ))
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

        // Gather the argument values across the group (skipping NULLs, like SQL).
        let mut values: Vec<SqlValue> = Vec::new();
        let mut count_all = 0usize;
        for row in group_rows {
            count_all += 1;
            if is_star {
                continue;
            }
            let expr = arg_expr.as_ref().unwrap();
            let mut frames: Vec<Frame> =
                outer.iter().map(|f| Frame { schema: f.schema, row: f.row }).collect();
            frames.push(Frame { schema: input_schema, row });
            let v = self.eval(expr, &frames)?;
            if !v.is_null() {
                values.push(v);
            }
        }
        if distinct {
            values = dedupe_values(values);
        }

        use rust_decimal::Decimal;
        let out = match name.as_str() {
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
                } else if values.iter().any(|v| matches!(v, SqlValue::Float4(_) | SqlValue::Float8(_))) {
                    SqlValue::Float8(values.iter().filter_map(SqlValue::as_f64).sum())
                } else {
                    let s: Decimal = values.iter().filter_map(SqlValue::as_decimal).sum();
                    SqlValue::Numeric(s)
                }
            }
            "avg" => {
                if values.is_empty() {
                    SqlValue::Null
                } else if values.iter().any(|v| matches!(v, SqlValue::Float4(_) | SqlValue::Float8(_))) {
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
                )))
            }
        };
        Ok(out)
    }

    // ---- ORDER BY / LIMIT ----------------------------------------------

    fn apply_order_by(&self, rowset: &mut RowSet, order_by: &OrderBy, outer: &[Frame]) -> Result<()> {
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
        if let Expr::Value(v) = expr {
            if let sqlparser::ast::Value::Number(n, _) = &v.value {
                if let Ok(pos) = n.parse::<usize>() {
                    if pos >= 1 && pos <= row.len() {
                        return Ok(row[pos - 1].clone());
                    }
                }
            }
        }
        // ORDER BY <output alias>.
        if let Expr::Identifier(ident) = expr {
            let name = ident_name(ident);
            if let Some(i) = rowset.schema.fields.iter().position(|f| f.name == name) {
                return Ok(row[i].clone());
            }
        }
        // Otherwise evaluate against the output row schema.
        let mut frames: Vec<Frame> = outer.iter().map(|f| Frame { schema: f.schema, row: f.row }).collect();
        frames.push(Frame { schema: &rowset.schema, row });
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
                        SqlType::Numeric { precision: None, scale: None }
                    } else {
                        SqlType::Integer
                    }
                }
                sqlparser::ast::Value::Boolean(_) => SqlType::Boolean,
                sqlparser::ast::Value::Null => SqlType::Text,
                _ => SqlType::Text,
            },
            Expr::Cast { data_type, .. } => {
                crate::eval::parse_data_type(data_type).unwrap_or(SqlType::Text)
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
                            SqlType::Numeric { precision: None, scale: None }
                        }
                    }
                    _ => SqlType::Text,
                }
            }
            Expr::IsNull(_) | Expr::IsNotNull(_) | Expr::Between { .. } | Expr::InList { .. }
            | Expr::Like { .. } | Expr::ILike { .. } | Expr::Exists { .. }
            | Expr::IsTrue(_) | Expr::IsFalse(_) => SqlType::Boolean,
            Expr::Nested(e) => self.infer_type(e, input),
            Expr::Function(f) => self.infer_function_type(f),
            Expr::Case { conditions, else_result, .. } => conditions
                .first()
                .map(|w| self.infer_type(&w.result, input))
                .or_else(|| else_result.as_ref().map(|e| self.infer_type(e, input)))
                .unwrap_or(SqlType::Text),
            _ => SqlType::Text,
        }
    }

    fn infer_type_agg(&self, expr: &Expr, input: &RowSchema) -> SqlType {
        if let Expr::Function(f) = expr {
            let name = f.name.0.last().and_then(|p| p.as_ident()).map(ident_name).unwrap_or_default();
            match name.as_str() {
                "count" => return SqlType::BigInt,
                "sum" | "avg" => return SqlType::Numeric { precision: None, scale: None },
                "bool_and" | "bool_or" | "every" => return SqlType::Boolean,
                "string_agg" => return SqlType::Text,
                _ => {}
            }
        }
        self.infer_type(expr, input)
    }

    fn infer_function_type(&self, f: &sqlparser::ast::Function) -> SqlType {
        let name = f.name.0.last().and_then(|p| p.as_ident()).map(ident_name).unwrap_or_default();
        match name.as_str() {
            "now" | "current_timestamp" | "transaction_timestamp" | "statement_timestamp"
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
            "abs" | "ceil" | "ceiling" | "floor" | "round" | "sum" | "avg" => {
                SqlType::Numeric { precision: None, scale: None }
            }
            _ => SqlType::Text,
        }
    }
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
    On(Expr),
    Using(Vec<String>),
    Natural,
    NaturalCols(Vec<String>),
}

/// Find an `column = <non-column-value>` predicate on a single-column-indexed
/// column, descending only through `AND` (never `OR`) and parentheses.
fn find_indexed_equality<'a>(
    expr: &'a Expr,
    loaded: &crate::store::LoadedTable,
) -> Option<(String, &'a Expr)> {
    use sqlparser::ast::BinaryOperator;
    match expr {
        Expr::BinaryOp { left, op: BinaryOperator::And, right } => {
            find_indexed_equality(left, loaded).or_else(|| find_indexed_equality(right, loaded))
        }
        Expr::BinaryOp { left, op: BinaryOperator::Eq, right } => {
            if let Some(col) = column_name(left) {
                if is_single_indexed(loaded, &col) && !is_column_ref(right) {
                    return Some((col, right));
                }
            }
            if let Some(col) = column_name(right) {
                if is_single_indexed(loaded, &col) && !is_column_ref(left) {
                    return Some((col, left));
                }
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
        let name = f.name.0.last().and_then(|p| p.as_ident()).map(ident_name).unwrap_or_default();
        if name == "unnest" {
            if let FunctionArguments::List(list) = &f.args {
                if let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(e))) = list.args.first() {
                    return Some(e);
                }
            }
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

fn is_single_indexed(loaded: &crate::store::LoadedTable, col: &str) -> bool {
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

/// Build a RowSet from a loaded table, labelling each field with `alias`.
fn loaded_to_rowset(loaded: &crate::store::LoadedTable, alias: &str) -> RowSet {
    let fields = loaded
        .meta
        .columns
        .iter()
        .map(|c| FieldRef { table: Some(alias.to_string()), name: c.name.clone(), ty: c.ty.clone() })
        .collect();
    let schema = RowSchema::new(fields);
    let rows = loaded
        .rows
        .values()
        .map(|values| {
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
fn relabel(mut rs: RowSet, alias: &str, table_alias: &Option<sqlparser::ast::TableAlias>) -> RowSet {
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
    rs
}

// ---------------------------------------------------------------------------
// Aggregate helpers
// ---------------------------------------------------------------------------

fn select_has_aggregate(select: &Select) -> bool {
    let mut found = false;
    for item in &select.projection {
        if let SelectItem::UnnamedExpr(e)
        | SelectItem::ExprWithAlias { expr: e, .. }
        | SelectItem::ExprWithAliases { expr: e, .. } = item
        {
            if expr_has_aggregate(e) {
                found = true;
            }
        }
    }
    if let Some(h) = &select.having {
        if expr_has_aggregate(h) {
            found = true;
        }
    }
    found
}

fn expr_has_aggregate(expr: &Expr) -> bool {
    let mut found = false;
    walk_expr(expr, &mut |e| {
        if let Expr::Function(f) = e {
            let name = f.name.0.last().and_then(|p| p.as_ident()).map(ident_name).unwrap_or_default();
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
            if let Expr::Function(f) = inner {
                let name = f.name.0.last().and_then(|p| p.as_ident()).map(ident_name).unwrap_or_default();
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
fn aggregate_arg(call: &sqlparser::ast::Function) -> Result<(bool, Option<Expr>, bool)> {
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
    if let FunctionArguments::List(list) = &call.args {
        if let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(e))) = list.args.get(1) {
            return Some(e.clone());
        }
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

fn compare_sort(a: &SqlValue, b: &SqlValue, asc: bool, nulls_first: bool) -> Ordering {
    match (a.is_null(), b.is_null()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return if nulls_first { Ordering::Less } else { Ordering::Greater },
        (false, true) => return if nulls_first { Ordering::Greater } else { Ordering::Less },
        (false, false) => {}
    }
    let ord = a.compare(b).unwrap_or(Ordering::Equal);
    if asc {
        ord
    } else {
        ord.reverse()
    }
}

pub(crate) fn default_col_name(expr: &Expr) -> String {
    match expr {
        Expr::Identifier(ident) => ident_name(ident),
        Expr::CompoundIdentifier(parts) => {
            parts.last().map(ident_name).unwrap_or_else(|| "?column?".into())
        }
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

/// Recursively visit sub-expressions (shallow set sufficient for aggregate
/// detection: projection/having scalar trees).
fn walk_expr(expr: &Expr, f: &mut dyn FnMut(&Expr)) {
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
        | Expr::IsFalse(expr)
        | Expr::Cast { expr, .. } => walk_expr(expr, f),
        Expr::Case { operand, conditions, else_result, .. } => {
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
                    | FunctionArg::Named { arg: FunctionArgExpr::Expr(e), .. }
                    | FunctionArg::ExprNamed { arg: FunctionArgExpr::Expr(e), .. } = arg
                    {
                        walk_expr(e, f);
                    }
                }
            }
        }
        _ => {}
    }
}
