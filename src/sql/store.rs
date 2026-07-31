//! Materialized table views, row (de)serialization, index maintenance and the
//! mutation set produced by a statement.
//!
//! Each statement loads the tables it touches into [`LoadedTable`]s (rows decoded
//! using catalog column types, indexes built from the live rows). Mutations are
//! accumulated as [`Mutation`]s and flushed to storage on commit. This mirrors
//! GuardianDB's local-first "refresh, then operate" model: the in-memory view is
//! a synchronous mirror of the document store, exactly like the existing
//! DocumentStore index.

use crate::relational::catalog::{Index, Table};
use crate::relational::{SecondaryIndex, SqlValue, composite_key, ordered_key};
use crate::sql::error::{Result, SqlError};
use serde_json::{Map, Value as Json};
use std::collections::BTreeMap;

pub const F_ID: &str = "_id";
pub const F_SCHEMA: &str = "__schema";
pub const F_TABLE: &str = "__table";
pub const F_VERSION: &str = "__version";
pub const F_DELETED: &str = "__deleted";

/// A decoded row: column name -> value (internal/system columns excluded).
pub type RowValues = BTreeMap<String, SqlValue>;

/// A loaded, materialized table view with live indexes.
#[derive(Clone)]
pub struct LoadedTable {
    pub meta: Table,
    /// row id -> decoded column values
    pub rows: BTreeMap<String, RowValues>,
    /// monotonically increasing version per row id (for `__version`)
    pub versions: BTreeMap<String, i64>,
    pub indexes: Vec<LoadedIndex>,
}

#[derive(Clone)]
pub struct LoadedIndex {
    pub meta: Index,
    /// ordered_key -> set of row ids
    pub data: SecondaryIndex,
}

impl LoadedTable {
    /// Build a loaded table from raw `(row_id, document)` pairs plus the index
    /// definitions for the table.
    pub fn build(meta: Table, docs: Vec<(String, Json)>, index_defs: Vec<Index>) -> Result<Self> {
        let mut rows = BTreeMap::new();
        let mut versions = BTreeMap::new();
        for (_rid, doc) in docs {
            // `build` already owns the documents, so decode by move to skip a
            // per-column `Json` clone.
            if let Some((id, values, version)) = decode_row_owned(&meta, doc)? {
                versions.insert(id.clone(), version);
                rows.insert(id, values);
            }
        }
        let mut table = LoadedTable {
            meta,
            rows,
            versions,
            indexes: index_defs
                .into_iter()
                // ANN (hnsw) indexes are not ordered-btree structures: they
                // live in the engine-level `AnnRuntime` (RFC 0005), never as
                // a per-statement `SecondaryIndex`. Materializing them here
                // would both waste the build and wrongly offer them to
                // equality-lookup planning.
                .filter(|m| m.method != "hnsw")
                .map(|m| LoadedIndex {
                    data: SecondaryIndex::new(m.unique),
                    meta: m,
                })
                .collect(),
        };
        table.rebuild_indexes();
        Ok(table)
    }

    pub fn rebuild_indexes(&mut self) {
        for idx in &mut self.indexes {
            idx.data.clear();
        }
        // Single pass over rows: update all indexes per row, no intermediate Vec.
        for (rid, values) in &self.rows {
            for idx in self.indexes.iter_mut() {
                let key = ordered_key(&index_values(&idx.meta, values));
                idx.data.insert(key, rid.clone());
            }
        }
    }

    /// Check PK/unique indexes for a candidate row. `exclude` is the row id being
    /// updated (so it does not conflict with itself).
    pub fn check_unique(&self, values: &RowValues, exclude: Option<&str>) -> Result<()> {
        for idx in &self.indexes {
            if !idx.meta.unique {
                continue;
            }
            let key_vals = index_values(&idx.meta, values);
            // PostgreSQL treats NULLs as distinct in unique indexes.
            if composite_key(&key_vals).is_none() {
                continue;
            }
            let key = ordered_key(&key_vals);
            for rid in idx.data.get(&key) {
                if Some(rid.as_str()) != exclude {
                    return Err(SqlError::UniqueViolation {
                        constraint: idx.meta.name.clone(),
                        detail: describe_key(&idx.meta.columns, &key_vals),
                    });
                }
            }
        }
        Ok(())
    }

    /// Insert a new row into the in-memory view and maintain indexes.
    pub fn apply_insert(&mut self, row_id: String, values: RowValues) {
        for idx in &mut self.indexes {
            let key = ordered_key(&index_values(&idx.meta, &values));
            idx.data.insert(key, row_id.clone());
        }
        let v = self.versions.get(&row_id).copied().unwrap_or(0) + 1;
        self.versions.insert(row_id.clone(), v);
        self.rows.insert(row_id, values);
    }

    /// Replace an existing row and maintain indexes.
    pub fn apply_update(&mut self, row_id: &str, values: RowValues) {
        if let Some(old) = self.rows.get(row_id) {
            for idx in &mut self.indexes {
                let key = ordered_key(&index_values(&idx.meta, old));
                idx.data.remove(&key, row_id);
            }
        }
        for idx in &mut self.indexes {
            let key = ordered_key(&index_values(&idx.meta, &values));
            idx.data.insert(key, row_id.to_string());
        }
        let v = self.versions.get(row_id).copied().unwrap_or(0) + 1;
        self.versions.insert(row_id.to_string(), v);
        self.rows.insert(row_id.to_string(), values);
    }

    /// Remove a row and maintain indexes.
    pub fn apply_delete(&mut self, row_id: &str) {
        if let Some(old) = self.rows.remove(row_id) {
            for idx in &mut self.indexes {
                let key = ordered_key(&index_values(&idx.meta, &old));
                idx.data.remove(&key, row_id);
            }
        }
    }

    /// Candidate row ids for an equality lookup on an index, or `None` if no
    /// usable index exists (forcing a full scan).
    pub fn index_lookup_eq(&self, column: &str, value: &SqlValue) -> Option<Vec<String>> {
        for idx in &self.indexes {
            if idx.meta.columns.len() == 1 && idx.meta.columns[0] == column {
                if value.is_null() {
                    return Some(Vec::new());
                }
                let key = ordered_key(std::slice::from_ref(value));
                return Some(idx.data.get(&key).iter().cloned().collect());
            }
        }
        None
    }

    pub fn version_of(&self, row_id: &str) -> i64 {
        self.versions.get(row_id).copied().unwrap_or(1)
    }
}

/// Extract the ordered key column values for an index from a row.
pub fn index_values(index: &Index, values: &RowValues) -> Vec<SqlValue> {
    index
        .columns
        .iter()
        .map(|c| values.get(c).cloned().unwrap_or(SqlValue::Null))
        .collect()
}

fn describe_key(columns: &[String], values: &[SqlValue]) -> String {
    let cols = columns.join(", ");
    let vals: Vec<String> = values
        .iter()
        .map(|v| v.to_text().unwrap_or_else(|| "null".into()))
        .collect();
    format!("Key ({cols})=({}) already exists.", vals.join(", "))
}

/// Decode a stored document into `(row_id, values, version)` consuming the owned
/// document. Column values are extracted with [`Map::remove`] so no per-column
/// `Json` clone is required. Returns `None` for tombstoned rows.
pub fn decode_row_owned(table: &Table, doc: Json) -> Result<Option<(String, RowValues, i64)>> {
    let mut obj = match doc {
        Json::Object(o) => o,
        _ => return Err(SqlError::Storage("row document is not an object".into())),
    };
    if obj.get(F_DELETED).and_then(Json::as_bool).unwrap_or(false) {
        return Ok(None);
    }
    let id = obj
        .get(F_ID)
        .and_then(Json::as_str)
        .ok_or_else(|| SqlError::Storage("row document missing _id".into()))?
        .to_owned();
    let version = obj.get(F_VERSION).and_then(Json::as_i64).unwrap_or(1);
    let mut values = BTreeMap::new();
    for col in &table.columns {
        let raw = obj.remove(&col.name).unwrap_or(Json::Null);
        let value = SqlValue::decode_json(&raw, &col.ty)?;
        values.insert(col.name.clone(), value);
    }
    Ok(Some((id, values, version)))
}

/// Encode a row's column values into a stored document.
pub fn encode_row(table: &Table, row_id: &str, values: &RowValues, version: i64) -> Json {
    let mut obj = Map::new();
    obj.insert(F_ID.into(), Json::String(row_id.to_string()));
    obj.insert(F_SCHEMA.into(), Json::String(table.schema.clone()));
    obj.insert(F_TABLE.into(), Json::String(table.name.clone()));
    obj.insert(F_VERSION.into(), Json::from(version));
    obj.insert(F_DELETED.into(), Json::Bool(false));
    for col in &table.columns {
        // Encode directly from a reference — avoids a `SqlValue` clone per column.
        let v = values
            .get(&col.name)
            .map_or(Json::Null, SqlValue::encode_json);
        obj.insert(col.name.clone(), v);
    }
    Json::Object(obj)
}

/// Derive a stable row id from a row's primary-key values, or `None` if the
/// table has no primary key (caller generates a UUID).
pub fn derive_row_id(table: &Table, values: &RowValues) -> Option<String> {
    let pk = table.pk_columns();
    if pk.is_empty() {
        return None;
    }
    let parts: Vec<SqlValue> = pk
        .iter()
        .map(|c| values.get(c).cloned().unwrap_or(SqlValue::Null))
        .collect();
    if parts.iter().any(|v| v.is_null()) {
        return None;
    }
    Some(
        parts
            .iter()
            .map(|v| v.index_key())
            .collect::<Vec<_>>()
            .join("\u{1f}"),
    )
}

/// A single pending storage mutation produced by a statement.
#[derive(Clone, Debug)]
pub enum Mutation {
    Put {
        collection: String,
        row_id: String,
        doc: Json,
    },
    Delete {
        collection: String,
        row_id: String,
    },
    Truncate {
        collection: String,
    },
}
