//! The storage boundary used by the relational engine.
//!
//! [`RelationalStorage`] is a thin document-collection interface. Each table maps
//! to one opaque "collection"; rows are JSON documents keyed by a stable row id.
//! The relational engine is responsible for all encoding/decoding and catalog
//! semantics, so a backend only needs to persist and retrieve JSON.
//!
//! Two implementations exist:
//!   * [`MemoryStorage`] — deterministic, in-process, used by tests and embedders
//!     that do not need replication.
//!   * The GuardianDB document-store adapter (in the `guardian-db` crate, behind
//!     the `sql` feature) — maps collections onto replicated iroh-docs documents.

use crate::relational::error::Result;
use async_trait::async_trait;
use serde_json::Value as Json;
use std::collections::BTreeMap;
use std::sync::RwLock;

/// Reserved collection name where the serialized catalog is stored.
pub const CATALOG_COLLECTION: &str = "__gdb_sql_catalog";

/// The persistence boundary for the relational engine.
#[async_trait]
pub trait RelationalStorage: Send + Sync {
    /// Return every live `(row_id, document)` pair in a collection.
    async fn scan(&self, collection: &str) -> Result<Vec<(String, Json)>>;

    /// Fetch a single row document by id.
    async fn get(&self, collection: &str, row_id: &str) -> Result<Option<Json>>;

    /// Insert or replace a row document.
    async fn put(&self, collection: &str, row_id: &str, doc: &Json) -> Result<()>;

    /// Remove a row document. Removing a missing row is not an error.
    async fn delete(&self, collection: &str, row_id: &str) -> Result<()>;

    /// Remove every row in a collection.
    async fn truncate(&self, collection: &str) -> Result<()>;

    /// Load the persisted catalog document, if one exists.
    async fn load_catalog(&self) -> Result<Option<Json>>;

    /// Persist the catalog document.
    async fn save_catalog(&self, catalog: &Json) -> Result<()>;

    /// A monotonic change counter for a collection, or `None` if the backend
    /// cannot cheaply report one.
    ///
    /// The engine reloads and decodes a whole table view per statement (its
    /// local-first "refresh then operate" model). When a backend can answer
    /// "has this collection changed?" in O(1), the engine caches the decoded
    /// view keyed by this counter and skips the rescan+decode+index-rebuild
    /// on a hit (see [`crate::sql::engine::Database`]).
    ///
    /// Contract: the returned value **must strictly increase** whenever any
    /// row in `collection` is added, changed, removed, or truncated —
    /// including changes that arrive through replication and become visible
    /// to [`scan`](Self::scan). Returning a value that fails to advance after
    /// a visible change makes the engine serve stale reads. The default
    /// implementation returns `None`, which disables caching (always
    /// correct, never stale) — a backend opts into caching by overriding it.
    fn generation(&self, collection: &str) -> Option<u64> {
        let _ = collection;
        None
    }
}

/// Deterministic in-memory storage.
#[derive(Debug, Default)]
pub struct MemoryStorage {
    collections: RwLock<BTreeMap<String, BTreeMap<String, Json>>>,
    catalog: RwLock<Option<Json>>,
    /// Per-collection monotonic change counters (see
    /// [`RelationalStorage::generation`]), bumped on every mutation.
    generations: RwLock<BTreeMap<String, u64>>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of live rows in a collection (test helper).
    pub fn count(&self, collection: &str) -> usize {
        self.collections
            .read()
            .unwrap()
            .get(collection)
            .map(|c| c.len())
            .unwrap_or(0)
    }

    /// Bump a collection's change counter (any mutation advances it).
    fn bump(&self, collection: &str) {
        *self
            .generations
            .write()
            .unwrap()
            .entry(collection.to_string())
            .or_insert(0) += 1;
    }
}

#[async_trait]
impl RelationalStorage for MemoryStorage {
    async fn scan(&self, collection: &str) -> Result<Vec<(String, Json)>> {
        Ok(self
            .collections
            .read()
            .unwrap()
            .get(collection)
            .map(|c| c.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default())
    }

    async fn get(&self, collection: &str, row_id: &str) -> Result<Option<Json>> {
        Ok(self
            .collections
            .read()
            .unwrap()
            .get(collection)
            .and_then(|c| c.get(row_id).cloned()))
    }

    async fn put(&self, collection: &str, row_id: &str, doc: &Json) -> Result<()> {
        self.collections
            .write()
            .unwrap()
            .entry(collection.to_string())
            .or_default()
            .insert(row_id.to_string(), doc.clone());
        self.bump(collection);
        Ok(())
    }

    async fn delete(&self, collection: &str, row_id: &str) -> Result<()> {
        if let Some(c) = self.collections.write().unwrap().get_mut(collection) {
            c.remove(row_id);
        }
        self.bump(collection);
        Ok(())
    }

    async fn truncate(&self, collection: &str) -> Result<()> {
        if let Some(c) = self.collections.write().unwrap().get_mut(collection) {
            c.clear();
        }
        self.bump(collection);
        Ok(())
    }

    async fn load_catalog(&self) -> Result<Option<Json>> {
        Ok(self.catalog.read().unwrap().clone())
    }

    async fn save_catalog(&self, catalog: &Json) -> Result<()> {
        *self.catalog.write().unwrap() = Some(catalog.clone());
        Ok(())
    }

    fn generation(&self, collection: &str) -> Option<u64> {
        Some(
            self.generations
                .read()
                .unwrap()
                .get(collection)
                .copied()
                .unwrap_or(0),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn memory_storage_crud() {
        let s = MemoryStorage::new();
        s.put("c", "1", &json!({"a": 1})).await.unwrap();
        s.put("c", "2", &json!({"a": 2})).await.unwrap();
        assert_eq!(s.scan("c").await.unwrap().len(), 2);
        assert_eq!(s.get("c", "1").await.unwrap(), Some(json!({"a": 1})));
        s.delete("c", "1").await.unwrap();
        assert_eq!(s.scan("c").await.unwrap().len(), 1);
        s.truncate("c").await.unwrap();
        assert_eq!(s.scan("c").await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn generation_advances_on_every_mutation() {
        let s = MemoryStorage::new();
        // Unknown collection reports a stable baseline.
        assert_eq!(s.generation("c"), Some(0));
        s.put("c", "1", &json!({"a": 1})).await.unwrap();
        let g1 = s.generation("c").unwrap();
        assert!(g1 > 0);
        // Every mutation strictly advances it.
        s.put("c", "2", &json!({"a": 2})).await.unwrap();
        let g2 = s.generation("c").unwrap();
        assert!(g2 > g1);
        s.delete("c", "1").await.unwrap();
        let g3 = s.generation("c").unwrap();
        assert!(g3 > g2);
        s.truncate("c").await.unwrap();
        assert!(s.generation("c").unwrap() > g3);
        // Independent collections keep independent counters.
        assert_eq!(s.generation("other"), Some(0));
    }

    #[tokio::test]
    async fn memory_storage_catalog() {
        let s = MemoryStorage::new();
        assert!(s.load_catalog().await.unwrap().is_none());
        s.save_catalog(&json!({"v": 1})).await.unwrap();
        assert_eq!(s.load_catalog().await.unwrap(), Some(json!({"v": 1})));
    }
}
