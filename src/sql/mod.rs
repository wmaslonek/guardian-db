//! # PostgreSQL-compatible SQL on top of GuardianDB
//!
//! A PostgreSQL-dialect SQL engine for GuardianDB: parser (via `sqlparser`),
//! planner/executor, expression evaluator, DDL/DML, transactions, and catalog
//! introspection views — all on top of the storage-agnostic
//! [`crate::relational::RelationalStorage`] boundary.
//!
//! The engine's public surface is [`Database`] and [`Session`]. A `Session`
//! parses SQL, loads the tables a statement touches into memory, executes
//! synchronously, and commits the resulting mutations back to storage.
//!
//! The [`guardian_storage`] submodule bridges that storage boundary onto a
//! GuardianDB [`DocumentStore`](crate::traits::DocumentStore), so relational
//! tables replicate as ordinary GuardianDB documents. Use [`open_sql`] /
//! [`open_sql_with`] to obtain a [`Database`] backed by a live GuardianDB node.

/// Engine-level ANN (HNSW) index runtime — RFC 0005 phase 1. Enabled by the
/// `vector-index` feature.
#[cfg(feature = "vector-index")]
pub mod ann;
mod catalog_views;
mod conv;
mod ddl;
mod dml;
mod eval;
mod exec;
pub mod ext;
mod fk;
mod fts;
mod funcs;
pub mod lock;
mod names;
mod result;
mod rls;
mod row;
mod select;
mod store;
mod trigger;
mod udf;
mod window;

pub mod engine;
pub mod error;
pub mod parser;

/// GuardianDB-backed [`RelationalStorage`] bridge (the local-first, replicated
/// storage path). The engine itself is storage-agnostic; this is the glue onto
/// a GuardianDB [`DocumentStore`](crate::traits::DocumentStore).
mod guardian_storage;

pub use engine::{ChangeEvent, ChangeOp, Database, Prepared, Session};
pub use error::{Result as SqlResult, SqlError};
pub use guardian_storage::{Consistency, GuardianRelationalStorage, open_sql, open_sql_with};
pub use parser::parse_sql;
pub use result::{ExecResult, OutField};
pub use rls::{role_bypasses_rls, role_bypasses_rls_on};

// Re-exports from the relational core for convenience.
pub use crate::relational::{
    Catalog, MemoryStorage, RelError, RelationalStorage, SqlType, SqlValue,
};

#[cfg(test)]
mod tests;

// SQL compatibility note 9: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.

// SQL compatibility note 9: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.
