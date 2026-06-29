//! Conversions from sqlparser literals to [`SqlValue`].

use crate::error::{Result, SqlError};
use guardian_relational::SqlValue;
use rust_decimal::Decimal;
use sqlparser::ast::Value;
use std::str::FromStr;

/// Convert a SQL literal into a [`SqlValue`]. String literals become `Text`
/// (PostgreSQL's untyped string literal), to be coerced by surrounding context.
pub fn literal_to_value(value: &Value) -> Result<SqlValue> {
    let out = match value {
        Value::Number(s, _) => parse_number(s)?,
        Value::SingleQuotedString(s)
        | Value::TripleSingleQuotedString(s)
        | Value::TripleDoubleQuotedString(s)
        | Value::EscapedStringLiteral(s)
        | Value::UnicodeStringLiteral(s)
        | Value::NationalStringLiteral(s)
        | Value::SingleQuotedRawStringLiteral(s)
        | Value::DoubleQuotedRawStringLiteral(s) => SqlValue::Text(s.clone()),
        Value::DollarQuotedString(d) => SqlValue::Text(d.value.clone()),
        Value::Boolean(b) => SqlValue::Bool(*b),
        Value::Null => SqlValue::Null,
        Value::HexStringLiteral(s) => {
            // PostgreSQL X'...' is a bit string; expose as text of the hex digits.
            SqlValue::Text(s.clone())
        }
        Value::Placeholder(p) => {
            return Err(SqlError::Internal(format!(
                "unbound parameter placeholder {p}"
            )));
        }
        other => SqlValue::Text(other.to_string()),
    };
    Ok(out)
}

/// Parse a numeric literal following PostgreSQL's literal typing rules.
pub fn parse_number(s: &str) -> Result<SqlValue> {
    let is_integral = !s.contains(['.', 'e', 'E']);
    if is_integral {
        if let Ok(i) = s.parse::<i32>() {
            return Ok(SqlValue::Int4(i));
        }
        if let Ok(i) = s.parse::<i64>() {
            return Ok(SqlValue::Int8(i));
        }
    }
    if let Ok(d) = Decimal::from_str(s) {
        return Ok(SqlValue::Numeric(d));
    }
    s.parse::<f64>()
        .map(SqlValue::Float8)
        .map_err(|_| SqlError::InvalidTextRepresentation {
            ty: "numeric".into(),
            value: s.to_string(),
        })
}
