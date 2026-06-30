use crate::guardian::error::GuardianError;
use crate::odm::error::{OdmError, Result};
use crate::traits::{Document, DocumentStore};
use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Storage boundary used by the ODM. Implementations can target GuardianDB's
/// decentralized DocumentStore or a deterministic in-memory store for tests.
#[async_trait]
pub trait CollectionStorage: Send + Sync {
    /// Loads every persisted document into memory.
    async fn load_all(&self) -> Result<Vec<Value>>;
    /// Persists a single document under the given id.
    async fn write_one(&self, id: &str, document: &Value) -> Result<()>;
    /// Persists multiple documents in one batch.
    async fn write_many(&self, documents: &[(String, Value)]) -> Result<()>;
}

/// `CollectionStorage` backed by GuardianDB's decentralized `DocumentStore`.
pub struct DocumentStoreStorage {
    store: Arc<dyn DocumentStore<Error = GuardianError>>,
}

impl DocumentStoreStorage {
    /// Wraps a GuardianDB `DocumentStore` as ODM collection storage.
    pub fn new(store: Arc<dyn DocumentStore<Error = GuardianError>>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl CollectionStorage for DocumentStoreStorage {
    async fn load_all(&self) -> Result<Vec<Value>> {
        let index = self.store.index();
        let keys = index.keys().map_err(OdmError::Guardian)?;
        let mut documents = Vec::with_capacity(keys.len());
        for key in keys {
            let Some(bytes) = index.get_bytes(&key).map_err(OdmError::Guardian)? else {
                continue;
            };
            let mut document: Value = serde_json::from_slice(&bytes)?;
            if let Some(object) = document.as_object_mut() {
                object
                    .entry("_id".to_string())
                    .or_insert_with(|| Value::String(key));
            }
            documents.push(document);
        }
        Ok(documents)
    }

    async fn write_one(&self, id: &str, document: &Value) -> Result<()> {
        let document = storage_document(id, document)?;
        self.store.put(Box::new(document)).await?;
        Ok(())
    }

    async fn write_many(&self, documents: &[(String, Value)]) -> Result<()> {
        let values: Result<Vec<Document>> = documents
            .iter()
            .map(|(id, document)| {
                storage_document(id, document).map(|value| Box::new(value) as Document)
            })
            .collect();
        self.store.put_all(values?).await?;
        Ok(())
    }
}

/// Clones a document and ensures it carries an `_id` field set to `id`, so the
/// storage layer always has a stable key. Errors if the document is not a JSON object.
fn storage_document(id: &str, document: &Value) -> Result<Value> {
    let mut document = document.clone();
    let object = document
        .as_object_mut()
        .ok_or_else(|| OdmError::Validation {
            field: "$document".to_string(),
            message: "document must be a JSON object".to_string(),
        })?;
    object
        .entry("_id".to_string())
        .or_insert_with(|| Value::String(id.to_string()));
    Ok(document)
}

/// Deterministic storage used by ODM unit tests and embedders that do not need
/// replication. Batch writes are committed by swapping a fully prepared map.
#[derive(Debug, Default)]
pub struct MemoryStorage {
    documents: RwLock<BTreeMap<String, Value>>,
}

impl MemoryStorage {
    /// Creates an empty in-memory storage.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a clone of the current document map for inspection in tests.
    pub fn snapshot(&self) -> BTreeMap<String, Value> {
        self.documents.read().clone()
    }
}

#[async_trait]
impl CollectionStorage for MemoryStorage {
    async fn load_all(&self) -> Result<Vec<Value>> {
        Ok(self.documents.read().values().cloned().collect())
    }

    async fn write_one(&self, id: &str, document: &Value) -> Result<()> {
        self.documents
            .write()
            .insert(id.to_string(), storage_document(id, document)?);
        Ok(())
    }

    async fn write_many(&self, documents: &[(String, Value)]) -> Result<()> {
        let mut next = self.documents.read().clone();
        for (id, document) in documents {
            next.insert(id.clone(), storage_document(id, document)?);
        }
        *self.documents.write() = next;
        Ok(())
    }
}
