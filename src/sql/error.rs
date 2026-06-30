//! Error handling. We reuse [`crate::relational::RelError`] (which carries
//! SQLSTATE codes) as the engine error type and add a parser-error mapping.

pub use crate::relational::RelError as SqlError;
pub type Result<T> = std::result::Result<T, SqlError>;

/// Map a sqlparser error into a SQLSTATE-tagged syntax error.
pub fn parse_error(e: sqlparser::parser::ParserError) -> SqlError {
    SqlError::Syntax(e.to_string())
}

/// Shorthand for an unsupported-feature error.
pub fn unsupported(what: impl Into<String>) -> SqlError {
    SqlError::FeatureNotSupported(what.into())
}
