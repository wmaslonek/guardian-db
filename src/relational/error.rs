//! Error types for the relational layer, each carrying a PostgreSQL SQLSTATE code.
//!
//! SQLSTATE codes follow the PostgreSQL error code catalogue so that wire-protocol
//! clients (psql, node-postgres, TypeORM) observe the same `code` field they would
//! against a real PostgreSQL server.

use thiserror::Error;

/// A relational-layer error. Every variant maps to a stable 5-character SQLSTATE.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RelError {
    #[error("relation \"{0}\" does not exist")]
    UndefinedTable(String),

    #[error("relation \"{0}\" already exists")]
    DuplicateTable(String),

    #[error("column \"{0}\" does not exist")]
    UndefinedColumn(String),

    #[error("column \"{0}\" of relation \"{1}\" already exists")]
    DuplicateColumn(String, String),

    #[error("schema \"{0}\" does not exist")]
    UndefinedSchema(String),

    #[error("schema \"{0}\" already exists")]
    DuplicateSchema(String),

    #[error("index \"{0}\" does not exist")]
    UndefinedIndex(String),

    #[error("relation \"{0}\" already exists")]
    DuplicateIndex(String),

    #[error("duplicate key value violates unique constraint \"{constraint}\"")]
    UniqueViolation {
        constraint: String,
        detail: String,
    },

    #[error("null value in column \"{column}\" of relation \"{table}\" violates not-null constraint")]
    NotNullViolation { column: String, table: String },

    #[error("insert or update on table \"{table}\" violates foreign key constraint \"{constraint}\"")]
    ForeignKeyViolation { table: String, constraint: String, detail: String },

    #[error("update or delete on table \"{table}\" violates foreign key constraint \"{constraint}\" on table \"{referencing}\"")]
    ForeignKeyViolationReferenced { table: String, constraint: String, referencing: String, detail: String },

    #[error("new row for relation \"{table}\" violates check constraint \"{constraint}\"")]
    CheckViolation { table: String, constraint: String },

    #[error("column \"{column}\" is of type {expected} but expression is of type {actual}")]
    DatatypeMismatch { column: String, expected: String, actual: String },

    #[error("invalid input syntax for type {ty}: \"{value}\"")]
    InvalidTextRepresentation { ty: String, value: String },

    #[error("value out of range for type {0}")]
    NumericValueOutOfRange(String),

    #[error("division by zero")]
    DivisionByZero,

    #[error("cannot cast type {from} to {to}")]
    CannotCoerce { from: String, to: String },

    #[error("type \"{0}\" does not exist")]
    UndefinedType(String),

    #[error("object \"{0}\" does not exist")]
    UndefinedObject(String),

    #[error("syntax error: {0}")]
    Syntax(String),

    #[error("{0}")]
    FeatureNotSupported(String),

    #[error("invalid parameter: {0}")]
    InvalidParameter(String),

    #[error("constraint \"{0}\" is invalid")]
    InvalidConstraint(String),

    #[error("deadlock detected")]
    DeadlockDetected { detail: String },

    #[error("could not obtain lock on {0}")]
    LockNotAvailable(String),

    #[error("current transaction is aborted, commands ignored until end of transaction block")]
    InFailedTransaction,

    #[error("storage error: {0}")]
    Storage(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl RelError {
    /// The 5-character SQLSTATE code for this error.
    pub fn sqlstate(&self) -> &'static str {
        match self {
            RelError::UndefinedTable(_) => "42P01",
            RelError::DuplicateTable(_) => "42P07",
            RelError::UndefinedColumn(_) => "42703",
            RelError::DuplicateColumn(_, _) => "42701",
            RelError::UndefinedSchema(_) => "3F000",
            RelError::DuplicateSchema(_) => "42P06",
            RelError::UndefinedIndex(_) => "42704",
            RelError::DuplicateIndex(_) => "42P07",
            RelError::UniqueViolation { .. } => "23505",
            RelError::NotNullViolation { .. } => "23502",
            RelError::ForeignKeyViolation { .. } => "23503",
            RelError::ForeignKeyViolationReferenced { .. } => "23503",
            RelError::CheckViolation { .. } => "23514",
            RelError::DatatypeMismatch { .. } => "42804",
            RelError::InvalidTextRepresentation { .. } => "22P02",
            RelError::NumericValueOutOfRange(_) => "22003",
            RelError::DivisionByZero => "22012",
            RelError::CannotCoerce { .. } => "42846",
            RelError::UndefinedType(_) => "42704",
            RelError::UndefinedObject(_) => "42704",
            RelError::Syntax(_) => "42601",
            RelError::FeatureNotSupported(_) => "0A000",
            RelError::InvalidParameter(_) => "22023",
            RelError::InvalidConstraint(_) => "42P10",
            RelError::DeadlockDetected { .. } => "40P01",
            RelError::LockNotAvailable(_) => "55P03",
            RelError::InFailedTransaction => "25P02",
            RelError::Storage(_) => "58030",
            RelError::Internal(_) => "XX000",
        }
    }

    /// The PostgreSQL severity for this error (always `ERROR` here).
    pub fn severity(&self) -> &'static str {
        "ERROR"
    }
}

pub type Result<T> = std::result::Result<T, RelError>;
