//! SQL parsing using sqlparser's PostgreSQL dialect.

use crate::error::{parse_error, Result};
use sqlparser::ast::Statement;
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

/// Parse a SQL string (possibly containing multiple `;`-separated statements).
pub fn parse_sql(sql: &str) -> Result<Vec<Statement>> {
    Parser::parse_sql(&PostgreSqlDialect {}, sql).map_err(parse_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_statements() {
        let stmts = parse_sql("SELECT 1; SELECT 2").unwrap();
        assert_eq!(stmts.len(), 2);
    }

    #[test]
    fn parse_error_is_syntax() {
        let err = parse_sql("SELEKT 1").unwrap_err();
        assert_eq!(err.sqlstate(), "42601");
    }
}
