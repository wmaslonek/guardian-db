//! # guardian-relational
//!
//! The relational foundation for GuardianDB's PostgreSQL compatibility layer.
//!
//! This crate is deliberately free of any dependency on GuardianDB's networking /
//! iroh stack so that the relational core compiles and tests quickly and can be
//! reused by embedders. It provides:
//!
//! * [`SqlType`] — the PostgreSQL-compatible type system with OIDs.
//! * [`SqlValue`] — the runtime value model with canonical JSON encoding, wire
//!   text I/O, three-valued comparison and casts.
//! * [`Catalog`] — schemas, tables, columns, constraints, indexes, sequences and
//!   views, fully serializable for persistence and transaction snapshots.
//! * [`RelationalStorage`] — the document-collection persistence boundary, with an
//!   in-memory implementation ([`MemoryStorage`]).
//! * Secondary index structures with composite keys and uniqueness enforcement.
//! * [`RelError`] — error types carrying PostgreSQL SQLSTATE codes.

pub mod catalog;
pub mod error;
pub mod fts;
/// Engine-native HNSW ANN index for `vector` columns (RFC 0005 phase 1).
/// Enabled by the `vector-index` feature.
#[cfg(feature = "vector-index")]
pub mod hnsw;
pub mod index;
pub mod storage;
pub mod types;
pub mod value;

pub use catalog::{
    Catalog, CheckConstraint, Column, DropFunctionByName, EmbeddingRuleDef, FIRST_USER_OID,
    ForeignKey, FunctionArgDef, FunctionDef, FunctionLanguage, FunctionVolatility, Index, Policy,
    PolicyCmd, PrimaryKey, QualifiedName, ReferentialAction, Schema, Sequence, Table, TriggerDef,
    TriggerEventDef, TriggerLevel, TriggerTiming, UniqueConstraint, View,
};
pub use error::{RelError, Result};
pub use index::{SecondaryIndex, composite_key, ordered_key};
pub use storage::{CATALOG_COLLECTION, MemoryStorage, RelationalStorage};
pub use types::SqlType;
pub use value::SqlValue;
