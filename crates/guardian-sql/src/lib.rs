//! # guardian-sql
//!
//! A PostgreSQL-dialect SQL engine for GuardianDB: parser (via `sqlparser`),
//! planner/executor, expression evaluator, DDL/DML, transactions, and catalog
//! introspection views — all on top of the storage-agnostic
//! [`guardian_relational::RelationalStorage`] boundary.
//!
//! The public surface is [`Database`] and [`Session`]. A `Session` parses SQL,
//! loads the tables a statement touches into memory, executes synchronously, and
//! commits the resulting mutations back to storage.

mod catalog_views;
mod conv;
mod ddl;
mod dml;
mod eval;
mod exec;
mod funcs;
pub mod lock;
mod names;
mod result;
mod row;
mod select;
mod store;

pub mod engine;
pub mod error;
pub mod parser;

pub use engine::{Database, Prepared, Session};
pub use error::{Result as SqlResult, SqlError};
pub use parser::parse_sql;
pub use result::{ExecResult, OutField};

// Re-exports from the relational core for convenience.
pub use guardian_relational::{
    Catalog, MemoryStorage, RelError, RelationalStorage, SqlType, SqlValue,
};

#[cfg(test)]
mod tests;
