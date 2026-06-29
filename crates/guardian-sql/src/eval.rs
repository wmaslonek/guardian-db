//! Expression evaluation with SQL three-valued logic.
//!
//! Evaluation is synchronous and operates against a stack of name-resolution
//! [`Frame`]s (innermost last). Correlated subqueries push the outer frames so an
//! inner query can reference outer columns.

use crate::error::{Result, SqlError};
use crate::exec::{Exec, Frame};
use crate::funcs;
use crate::names::ident_name;
use guardian_relational::{SqlType, SqlValue};
use sqlparser::ast::{
    BinaryOperator, DateTimeField, Expr, FunctionArg, FunctionArgExpr, FunctionArguments,
    UnaryOperator, Value,
};
use std::collections::HashMap;
use std::cmp::Ordering;

impl Exec {
    /// Evaluate an expression that must not contain aggregate calls.
    pub fn eval(&self, expr: &Expr, frames: &[Frame]) -> Result<SqlValue> {
        self.eval_inner(expr, frames, None)
    }

    /// Evaluate an expression that may reference precomputed aggregate results
    /// (keyed by the aggregate's normalized SQL text).
    pub fn eval_agg(
        &self,
        expr: &Expr,
        frames: &[Frame],
        aggs: &HashMap<String, SqlValue>,
    ) -> Result<SqlValue> {
        self.eval_inner(expr, frames, Some(aggs))
    }

    fn eval_inner(
        &self,
        expr: &Expr,
        frames: &[Frame],
        aggs: Option<&HashMap<String, SqlValue>>,
    ) -> Result<SqlValue> {
        match expr {
            Expr::Identifier(ident) => self.resolve_or_special(frames, None, &ident_name(ident)),
            Expr::CompoundIdentifier(parts) => {
                let names: Vec<String> = parts.iter().map(ident_name).collect();
                let (table, col) = match names.as_slice() {
                    [col] => (None, col.clone()),
                    [.., table, col] => (Some(table.clone()), col.clone()),
                    _ => return Err(SqlError::Syntax("empty identifier".into())),
                };
                self.resolve_or_special(frames, table.as_deref(), &col)
            }
            Expr::Value(vws) => match &vws.value {
                Value::Placeholder(p) => self.param(p),
                v => crate::conv::literal_to_value(v),
            },
            Expr::TypedString(ts) => {
                let ty = parse_data_type(&ts.data_type)?;
                let raw = string_of_value(&ts.value.value)?;
                SqlValue::from_text(&raw, &ty)
            }
            Expr::Nested(e) => self.eval_inner(e, frames, aggs),
            Expr::UnaryOp { op, expr } => {
                let v = self.eval_inner(expr, frames, aggs)?;
                self.unary(op, v)
            }
            Expr::BinaryOp { left, op, right } => self.binary(left, op, right, frames, aggs),
            Expr::IsNull(e) => Ok(SqlValue::Bool(self.eval_inner(e, frames, aggs)?.is_null())),
            Expr::IsNotNull(e) => Ok(SqlValue::Bool(!self.eval_inner(e, frames, aggs)?.is_null())),
            Expr::IsTrue(e) => Ok(SqlValue::Bool(self.eval_inner(e, frames, aggs)?.truthy() == Some(true))),
            Expr::IsNotTrue(e) => Ok(SqlValue::Bool(self.eval_inner(e, frames, aggs)?.truthy() != Some(true))),
            Expr::IsFalse(e) => Ok(SqlValue::Bool(self.eval_inner(e, frames, aggs)?.truthy() == Some(false))),
            Expr::IsNotFalse(e) => Ok(SqlValue::Bool(self.eval_inner(e, frames, aggs)?.truthy() != Some(false))),
            Expr::IsUnknown(e) => Ok(SqlValue::Bool(self.eval_inner(e, frames, aggs)?.truthy().is_none())),
            Expr::IsNotUnknown(e) => Ok(SqlValue::Bool(self.eval_inner(e, frames, aggs)?.truthy().is_some())),
            Expr::IsDistinctFrom(a, b) => {
                let x = self.eval_inner(a, frames, aggs)?;
                let y = self.eval_inner(b, frames, aggs)?;
                Ok(SqlValue::Bool(!values_not_distinct(&x, &y)))
            }
            Expr::IsNotDistinctFrom(a, b) => {
                let x = self.eval_inner(a, frames, aggs)?;
                let y = self.eval_inner(b, frames, aggs)?;
                Ok(SqlValue::Bool(values_not_distinct(&x, &y)))
            }
            Expr::Between { expr, negated, low, high } => {
                self.eval_between(expr, *negated, low, high, frames, aggs)
            }
            Expr::InList { expr, list, negated } => {
                self.eval_in_list(expr, list, *negated, frames, aggs)
            }
            Expr::InSubquery { expr, subquery, negated } => {
                self.eval_in_subquery(expr, subquery, *negated, frames)
            }
            Expr::Like { negated, expr, pattern, escape_char, .. } => {
                self.eval_like(expr, pattern, escape_char.as_ref(), *negated, false, frames, aggs)
            }
            Expr::ILike { negated, expr, pattern, escape_char, .. } => {
                self.eval_like(expr, pattern, escape_char.as_ref(), *negated, true, frames, aggs)
            }
            Expr::Case { operand, conditions, else_result, .. } => {
                self.eval_case(operand.as_deref(), conditions, else_result.as_deref(), frames, aggs)
            }
            Expr::Cast { expr, data_type, .. } => {
                let v = self.eval_inner(expr, frames, aggs)?;
                let ty = parse_data_type(data_type)?;
                v.cast(&ty)
            }
            Expr::Exists { subquery, negated } => {
                let rows = self.exec_subquery(subquery, frames)?;
                let exists = !rows.rows.is_empty();
                Ok(SqlValue::Bool(exists != *negated))
            }
            Expr::Subquery(q) => {
                let rows = self.exec_subquery(q, frames)?;
                if rows.rows.is_empty() {
                    return Ok(SqlValue::Null);
                }
                if rows.rows.len() > 1 {
                    return Err(SqlError::Internal(
                        "more than one row returned by a subquery used as an expression".into(),
                    ));
                }
                Ok(rows.rows[0].first().cloned().unwrap_or(SqlValue::Null))
            }
            Expr::Function(func) => self.eval_function(func, frames, aggs),
            Expr::Tuple(items) => {
                // A standalone row constructor; represent as an array for scalar use.
                let mut out = Vec::with_capacity(items.len());
                for it in items {
                    out.push(self.eval_inner(it, frames, aggs)?);
                }
                Ok(SqlValue::Array(out))
            }
            Expr::Array(arr) => {
                let mut out = Vec::with_capacity(arr.elem.len());
                for it in &arr.elem {
                    out.push(self.eval_inner(it, frames, aggs)?);
                }
                Ok(SqlValue::Array(out))
            }
            Expr::Substring { expr, substring_from, substring_for, .. } => {
                let mut args = vec![self.eval_inner(expr, frames, aggs)?];
                if let Some(f) = substring_from {
                    args.push(self.eval_inner(f, frames, aggs)?);
                }
                if let Some(f) = substring_for {
                    args.push(self.eval_inner(f, frames, aggs)?);
                }
                funcs::call_scalar(self, "substring", args)
            }
            Expr::Position { expr, r#in } => {
                let needle = self.eval_inner(expr, frames, aggs)?;
                let hay = self.eval_inner(r#in, frames, aggs)?;
                if needle.is_null() || hay.is_null() {
                    return Ok(SqlValue::Null);
                }
                let h = hay.to_text().unwrap_or_default();
                let n = needle.to_text().unwrap_or_default();
                let pos = h.find(&n).map(|b| h[..b].chars().count() + 1).unwrap_or(0);
                Ok(SqlValue::Int4(pos as i32))
            }
            Expr::Trim { expr, trim_what, .. } => {
                let mut args = vec![self.eval_inner(expr, frames, aggs)?];
                if let Some(w) = trim_what {
                    args.push(self.eval_inner(w, frames, aggs)?);
                }
                funcs::call_scalar(self, "btrim", args)
            }
            Expr::Extract { field, expr, .. } => {
                let v = self.eval_inner(expr, frames, aggs)?;
                eval_extract(field, &v)
            }
            Expr::Collate { expr, .. } => self.eval_inner(expr, frames, aggs),
            Expr::Prefixed { value, .. } => self.eval_inner(value, frames, aggs),
            other => Err(SqlError::FeatureNotSupported(format!(
                "expression not supported: {other}"
            ))),
        }
    }

    fn resolve_or_special(
        &self,
        frames: &[Frame],
        table: Option<&str>,
        col: &str,
    ) -> Result<SqlValue> {
        // Try column resolution first (innermost frame outward).
        let mut last_err: Option<SqlError> = None;
        for frame in frames.iter().rev() {
            match frame.schema.resolve(table, col) {
                Ok(i) => return Ok(frame.row[i].clone()),
                Err(e @ SqlError::Syntax(_)) => return Err(e),
                Err(e) => last_err = Some(e),
            }
        }
        // Niladic functions / keywords usable without parentheses.
        if table.is_none() {
            match col {
                "current_timestamp" | "current_date" | "current_time" | "current_user"
                | "session_user" | "user" | "current_schema" | "current_catalog"
                | "current_database" | "localtimestamp" | "localtime" => {
                    return funcs::call_scalar(self, col, Vec::new());
                }
                "true" => return Ok(SqlValue::Bool(true)),
                "false" => return Ok(SqlValue::Bool(false)),
                "null" => return Ok(SqlValue::Null),
                _ => {}
            }
        }
        Err(last_err.unwrap_or_else(|| SqlError::UndefinedColumn(col.to_string())))
    }

    fn unary(&self, op: &UnaryOperator, v: SqlValue) -> Result<SqlValue> {
        if v.is_null() {
            return Ok(SqlValue::Null);
        }
        match op {
            UnaryOperator::Plus => Ok(v),
            UnaryOperator::Minus => match v {
                SqlValue::Int2(n) => Ok(SqlValue::Int2(-n)),
                SqlValue::Int4(n) => Ok(SqlValue::Int4(-n)),
                SqlValue::Int8(n) => Ok(SqlValue::Int8(-n)),
                SqlValue::Float4(n) => Ok(SqlValue::Float4(-n)),
                SqlValue::Float8(n) => Ok(SqlValue::Float8(-n)),
                SqlValue::Numeric(d) => Ok(SqlValue::Numeric(-d)),
                other => Err(SqlError::CannotCoerce {
                    from: other.type_of().name(),
                    to: "numeric".into(),
                }),
            },
            UnaryOperator::Not => match v.truthy() {
                Some(b) => Ok(SqlValue::Bool(!b)),
                None => Ok(SqlValue::Null),
            },
            other => Err(SqlError::FeatureNotSupported(format!(
                "unary operator {other} not supported"
            ))),
        }
    }

    fn binary(
        &self,
        left: &Expr,
        op: &BinaryOperator,
        right: &Expr,
        frames: &[Frame],
        aggs: Option<&HashMap<String, SqlValue>>,
    ) -> Result<SqlValue> {
        use BinaryOperator::*;
        // Short-circuiting logical operators.
        match op {
            And => {
                let l = self.eval_inner(left, frames, aggs)?.truthy();
                if l == Some(false) {
                    return Ok(SqlValue::Bool(false));
                }
                let r = self.eval_inner(right, frames, aggs)?.truthy();
                if r == Some(false) {
                    return Ok(SqlValue::Bool(false));
                }
                return Ok(match (l, r) {
                    (Some(true), Some(true)) => SqlValue::Bool(true),
                    _ => SqlValue::Null,
                });
            }
            Or => {
                let l = self.eval_inner(left, frames, aggs)?.truthy();
                if l == Some(true) {
                    return Ok(SqlValue::Bool(true));
                }
                let r = self.eval_inner(right, frames, aggs)?.truthy();
                if r == Some(true) {
                    return Ok(SqlValue::Bool(true));
                }
                return Ok(match (l, r) {
                    (Some(false), Some(false)) => SqlValue::Bool(false),
                    _ => SqlValue::Null,
                });
            }
            _ => {}
        }

        let a = self.eval_inner(left, frames, aggs)?;
        let b = self.eval_inner(right, frames, aggs)?;
        match op {
            Plus | Minus | Multiply | Divide | Modulo | PGExp => arith(&a, op, &b),
            StringConcat => {
                if a.is_null() || b.is_null() {
                    Ok(SqlValue::Null)
                } else {
                    Ok(SqlValue::Text(format!(
                        "{}{}",
                        a.to_text().unwrap_or_default(),
                        b.to_text().unwrap_or_default()
                    )))
                }
            }
            Eq | NotEq | Gt | Lt | GtEq | LtEq | Spaceship => Ok(compare_op(&a, op, &b)),
            BitwiseAnd | BitwiseOr | BitwiseXor => bitwise(&a, op, &b),
            other => Err(SqlError::FeatureNotSupported(format!(
                "binary operator {other} not supported"
            ))),
        }
    }

    fn eval_between(
        &self,
        expr: &Expr,
        negated: bool,
        low: &Expr,
        high: &Expr,
        frames: &[Frame],
        aggs: Option<&HashMap<String, SqlValue>>,
    ) -> Result<SqlValue> {
        let v = self.eval_inner(expr, frames, aggs)?;
        let lo = self.eval_inner(low, frames, aggs)?;
        let hi = self.eval_inner(high, frames, aggs)?;
        if v.is_null() || lo.is_null() || hi.is_null() {
            return Ok(SqlValue::Null);
        }
        let (v1, lo) = coerce_pair(v.clone(), lo);
        let (v2, hi) = coerce_pair(v, hi);
        let ge = matches!(v1.compare(&lo), Some(Ordering::Greater | Ordering::Equal));
        let le = matches!(v2.compare(&hi), Some(Ordering::Less | Ordering::Equal));
        let within = ge && le;
        Ok(SqlValue::Bool(within != negated))
    }

    fn eval_in_list(
        &self,
        expr: &Expr,
        list: &[Expr],
        negated: bool,
        frames: &[Frame],
        aggs: Option<&HashMap<String, SqlValue>>,
    ) -> Result<SqlValue> {
        let v = self.eval_inner(expr, frames, aggs)?;
        if v.is_null() {
            return Ok(SqlValue::Null);
        }
        let mut saw_null = false;
        for item in list {
            let iv = self.eval_inner(item, frames, aggs)?;
            if iv.is_null() {
                saw_null = true;
                continue;
            }
            let (a, b) = coerce_pair(v.clone(), iv);
            if a.sql_eq(&b) == Some(true) {
                return Ok(SqlValue::Bool(!negated));
            }
        }
        if saw_null {
            Ok(SqlValue::Null)
        } else {
            Ok(SqlValue::Bool(negated))
        }
    }

    fn eval_in_subquery(
        &self,
        expr: &Expr,
        subquery: &sqlparser::ast::Query,
        negated: bool,
        frames: &[Frame],
    ) -> Result<SqlValue> {
        let v = self.eval(expr, frames)?;
        if v.is_null() {
            return Ok(SqlValue::Null);
        }
        let rows = self.exec_subquery(subquery, frames)?;
        let mut saw_null = false;
        for row in &rows.rows {
            let candidate = row.first().cloned().unwrap_or(SqlValue::Null);
            if candidate.is_null() {
                saw_null = true;
                continue;
            }
            let (a, b) = coerce_pair(v.clone(), candidate);
            if a.sql_eq(&b) == Some(true) {
                return Ok(SqlValue::Bool(!negated));
            }
        }
        if saw_null {
            Ok(SqlValue::Null)
        } else {
            Ok(SqlValue::Bool(negated))
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_like(
        &self,
        expr: &Expr,
        pattern: &Expr,
        escape: Option<&sqlparser::ast::ValueWithSpan>,
        negated: bool,
        case_insensitive: bool,
        frames: &[Frame],
        aggs: Option<&HashMap<String, SqlValue>>,
    ) -> Result<SqlValue> {
        let v = self.eval_inner(expr, frames, aggs)?;
        let p = self.eval_inner(pattern, frames, aggs)?;
        if v.is_null() || p.is_null() {
            return Ok(SqlValue::Null);
        }
        let escape_char = escape
            .and_then(|e| string_of_value(&e.value).ok())
            .and_then(|s| s.chars().next());
        let matched = funcs::like_match(
            &v.to_text().unwrap_or_default(),
            &p.to_text().unwrap_or_default(),
            case_insensitive,
            escape_char,
        );
        Ok(SqlValue::Bool(matched != negated))
    }

    fn eval_case(
        &self,
        operand: Option<&Expr>,
        conditions: &[sqlparser::ast::CaseWhen],
        else_result: Option<&Expr>,
        frames: &[Frame],
        aggs: Option<&HashMap<String, SqlValue>>,
    ) -> Result<SqlValue> {
        let operand_val = match operand {
            Some(e) => Some(self.eval_inner(e, frames, aggs)?),
            None => None,
        };
        for when in conditions {
            let matched = match &operand_val {
                Some(ov) => {
                    let cond = self.eval_inner(&when.condition, frames, aggs)?;
                    let (a, b) = coerce_pair(ov.clone(), cond);
                    a.sql_eq(&b) == Some(true)
                }
                None => self.eval_inner(&when.condition, frames, aggs)?.truthy() == Some(true),
            };
            if matched {
                return self.eval_inner(&when.result, frames, aggs);
            }
        }
        match else_result {
            Some(e) => self.eval_inner(e, frames, aggs),
            None => Ok(SqlValue::Null),
        }
    }

    fn eval_function(
        &self,
        func: &sqlparser::ast::Function,
        frames: &[Frame],
        aggs: Option<&HashMap<String, SqlValue>>,
    ) -> Result<SqlValue> {
        let name = func
            .name
            .0
            .last()
            .and_then(|p| p.as_ident())
            .map(ident_name)
            .unwrap_or_default();
        if funcs::is_aggregate(&name) {
            if let Some(map) = aggs {
                let key = func.to_string();
                return map
                    .get(&key)
                    .cloned()
                    .ok_or_else(|| SqlError::Internal(format!("aggregate {key} not precomputed")));
            }
            return Err(SqlError::Syntax(format!(
                "aggregate function {name} is not allowed here"
            )));
        }
        let mut args = Vec::new();
        if let FunctionArguments::List(list) = &func.args {
            for arg in &list.args {
                match arg {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
                    | FunctionArg::Named { arg: FunctionArgExpr::Expr(e), .. }
                    | FunctionArg::ExprNamed { arg: FunctionArgExpr::Expr(e), .. } => {
                        args.push(self.eval_inner(e, frames, aggs)?);
                    }
                    FunctionArg::Unnamed(FunctionArgExpr::Wildcard) => {
                        args.push(SqlValue::Int4(1));
                    }
                    _ => {
                        return Err(SqlError::FeatureNotSupported(
                            "wildcard function argument not supported".into(),
                        ))
                    }
                }
            }
        }
        funcs::call_scalar(self, &name, args)
    }
}

// ---------------------------------------------------------------------------
// Free helpers.
// ---------------------------------------------------------------------------

/// Parse a sqlparser `DataType` into a [`SqlType`] via its textual form.
pub fn parse_data_type(dt: &sqlparser::ast::DataType) -> Result<SqlType> {
    let text = dt.to_string();
    if let Some(ty) = SqlType::is_serial_name(&text) {
        return Ok(ty);
    }
    SqlType::parse(&text)
}

fn string_of_value(v: &Value) -> Result<String> {
    match crate::conv::literal_to_value(v)? {
        SqlValue::Text(s) => Ok(s),
        other => Ok(other.to_text().unwrap_or_default()),
    }
}

/// Coerce a string literal against a typed counterpart so comparisons behave
/// like PostgreSQL (`n > '9'` compares numerically, not lexically).
fn coerce_pair(a: SqlValue, b: SqlValue) -> (SqlValue, SqlValue) {
    fn coerce_one(text_side: &SqlValue, typed: &SqlValue) -> Option<SqlValue> {
        let ty = typed.type_of();
        if matches!(ty, SqlType::Text | SqlType::Unknown) {
            return None;
        }
        text_side.cast(&ty).ok()
    }
    match (&a, &b) {
        (SqlValue::Text(_), other) if !matches!(other, SqlValue::Text(_) | SqlValue::Null) => {
            if let Some(c) = coerce_one(&a, &b) {
                return (c, b);
            }
            (a, b)
        }
        (other, SqlValue::Text(_)) if !matches!(other, SqlValue::Text(_) | SqlValue::Null) => {
            if let Some(c) = coerce_one(&b, &a) {
                return (a, c);
            }
            (a, b)
        }
        _ => (a, b),
    }
}

fn compare_op(a: &SqlValue, op: &BinaryOperator, b: &SqlValue) -> SqlValue {
    use BinaryOperator::*;
    if a.is_null() || b.is_null() {
        return SqlValue::Null;
    }
    let (a, b) = coerce_pair(a.clone(), b.clone());
    match a.compare(&b) {
        None => SqlValue::Null,
        Some(ord) => {
            let result = match op {
                Eq | Spaceship => ord == Ordering::Equal,
                NotEq => ord != Ordering::Equal,
                Gt => ord == Ordering::Greater,
                Lt => ord == Ordering::Less,
                GtEq => ord != Ordering::Less,
                LtEq => ord != Ordering::Greater,
                _ => return SqlValue::Null,
            };
            SqlValue::Bool(result)
        }
    }
}

fn values_not_distinct(a: &SqlValue, b: &SqlValue) -> bool {
    match (a.is_null(), b.is_null()) {
        (true, true) => true,
        (false, false) => a.sql_eq(b) == Some(true),
        _ => false,
    }
}

fn arith(a: &SqlValue, op: &BinaryOperator, b: &SqlValue) -> Result<SqlValue> {
    use BinaryOperator::*;
    if a.is_null() || b.is_null() {
        return Ok(SqlValue::Null);
    }
    let a_int = a.type_of().is_integer();
    let b_int = b.type_of().is_integer();
    let a_float = matches!(a, SqlValue::Float4(_) | SqlValue::Float8(_));
    let b_float = matches!(b, SqlValue::Float4(_) | SqlValue::Float8(_));

    // Integer arithmetic (truncating division/modulo).
    if a_int && b_int {
        let x = a.as_i64().unwrap() as i128;
        let y = b.as_i64().unwrap() as i128;
        let r: i128 = match op {
            Plus => x + y,
            Minus => x - y,
            Multiply => x * y,
            Divide => {
                if y == 0 {
                    return Err(SqlError::DivisionByZero);
                }
                x / y
            }
            Modulo => {
                if y == 0 {
                    return Err(SqlError::DivisionByZero);
                }
                x % y
            }
            _ => return Err(SqlError::FeatureNotSupported("operator".into())),
        };
        return fit_int(r);
    }

    // Floating-point arithmetic.
    if a_float || b_float {
        let x = a.as_f64().ok_or_else(|| coerce_err(a))?;
        let y = b.as_f64().ok_or_else(|| coerce_err(b))?;
        let r = match op {
            Plus => x + y,
            Minus => x - y,
            Multiply => x * y,
            Divide => {
                if y == 0.0 {
                    return Err(SqlError::DivisionByZero);
                }
                x / y
            }
            Modulo => x % y,
            PGExp => x.powf(y),
            _ => return Err(SqlError::FeatureNotSupported("operator".into())),
        };
        return Ok(SqlValue::Float8(r));
    }

    // Decimal arithmetic.
    let x = a.as_decimal().ok_or_else(|| coerce_err(a))?;
    let y = b.as_decimal().ok_or_else(|| coerce_err(b))?;
    use rust_decimal::prelude::*;
    let r = match op {
        Plus => x + y,
        Minus => x - y,
        Multiply => x * y,
        Divide => {
            if y.is_zero() {
                return Err(SqlError::DivisionByZero);
            }
            x / y
        }
        Modulo => {
            if y.is_zero() {
                return Err(SqlError::DivisionByZero);
            }
            x % y
        }
        PGExp => {
            let xf = x.to_f64().unwrap_or(0.0);
            let yf = y.to_f64().unwrap_or(0.0);
            return Ok(SqlValue::Float8(xf.powf(yf)));
        }
        _ => return Err(SqlError::FeatureNotSupported("operator".into())),
    };
    Ok(SqlValue::Numeric(r))
}

fn fit_int(r: i128) -> Result<SqlValue> {
    if r >= i32::MIN as i128 && r <= i32::MAX as i128 {
        Ok(SqlValue::Int4(r as i32))
    } else if r >= i64::MIN as i128 && r <= i64::MAX as i128 {
        Ok(SqlValue::Int8(r as i64))
    } else {
        Err(SqlError::NumericValueOutOfRange("bigint".into()))
    }
}

fn bitwise(a: &SqlValue, op: &BinaryOperator, b: &SqlValue) -> Result<SqlValue> {
    use BinaryOperator::*;
    if a.is_null() || b.is_null() {
        return Ok(SqlValue::Null);
    }
    let x = a.as_i64().ok_or_else(|| coerce_err(a))?;
    let y = b.as_i64().ok_or_else(|| coerce_err(b))?;
    let r = match op {
        BitwiseAnd => x & y,
        BitwiseOr => x | y,
        BitwiseXor => x ^ y,
        _ => return Err(SqlError::FeatureNotSupported("operator".into())),
    };
    fit_int(r as i128)
}

fn coerce_err(v: &SqlValue) -> SqlError {
    SqlError::CannotCoerce {
        from: v.type_of().name(),
        to: "numeric".into(),
    }
}

fn eval_extract(field: &DateTimeField, v: &SqlValue) -> Result<SqlValue> {
    use chrono::{Datelike, Timelike};
    if v.is_null() {
        return Ok(SqlValue::Null);
    }
    let part = field.to_string().to_lowercase();
    let (date, time) = match v {
        SqlValue::Date(d) => (Some(*d), None),
        SqlValue::Timestamp(ts) => (Some(ts.date()), Some(ts.time())),
        SqlValue::Timestamptz(ts) => (Some(ts.naive_utc().date()), Some(ts.naive_utc().time())),
        SqlValue::Time(t) => (None, Some(*t)),
        _ => return Err(SqlError::FeatureNotSupported("EXTRACT from non-temporal".into())),
    };
    use rust_decimal::Decimal;
    let n = match part.as_str() {
        "year" => date.map(|d| d.year() as i64),
        "month" => date.map(|d| d.month() as i64),
        "day" => date.map(|d| d.day() as i64),
        "hour" => time.map(|t| t.hour() as i64),
        "minute" => time.map(|t| t.minute() as i64),
        "second" => time.map(|t| t.second() as i64),
        "dow" => date.map(|d| d.weekday().num_days_from_sunday() as i64),
        "doy" => date.map(|d| d.ordinal() as i64),
        "quarter" => date.map(|d| ((d.month() - 1) / 3 + 1) as i64),
        "week" => date.map(|d| d.iso_week().week() as i64),
        _ => return Err(SqlError::FeatureNotSupported(format!("EXTRACT field {part}"))),
    };
    Ok(n.map(|x| SqlValue::Numeric(Decimal::from(x))).unwrap_or(SqlValue::Null))
}
