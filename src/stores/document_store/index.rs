use crate::traits::{CreateDocumentDBOptions, StoreIndex};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[allow(dead_code)]
type Result<T> = std::result::Result<T, crate::guardian::error::GuardianError>;

/// DocumentIndex maintains an in-memory key-value index for the DocumentStore.
///
/// It works directly with iroh-docs Entry.
#[derive(Clone)]
pub struct DocumentIndex {
    // The main index, protected by an RwLock for safe concurrent access.
    // Maps: key (String) -> blob_hash (Vec<u8>)
    // Arc allows sharing the same index across multiple instances.
    index: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    // Store configuration options, shared via Arc.
    #[allow(dead_code)]
    opts: Arc<CreateDocumentDBOptions>,
}

impl DocumentIndex {
    /// Creates a new DocumentIndex instance.
    pub fn new(opts: Arc<CreateDocumentDBOptions>) -> Self {
        Self {
            index: Arc::new(RwLock::new(HashMap::new())),
            opts,
        }
    }

    /// Returns a copy of all keys present in the index.
    pub fn keys(&self) -> Vec<String> {
        // Acquire a read lock. The unwrap handles mutex "poisoning" cases.
        let index_lock = self
            .index
            .read()
            .expect("Failed to acquire read lock on document index");
        // Collect the map's keys. `.keys()` returns an iterator of &String,
        // so `.cloned()` creates new Strings from the references.
        index_lock.keys().cloned().collect()
    }

    /// Specific method to get a Vec<u8> from the index.
    /// Used internally by the DocumentStore.
    pub fn get_bytes(&self, key: &str) -> Option<Vec<u8>> {
        let index_lock = self
            .index
            .read()
            .expect("Failed to acquire read lock on document index");
        index_lock.get(key).cloned()
    }
}

// Implements the StoreIndex trait for DocumentIndex.
impl StoreIndex for DocumentIndex {
    type Error = crate::guardian::error::GuardianError;

    /// Checks whether a key exists in the index.
    fn contains_key(&self, key: &str) -> std::result::Result<bool, Self::Error> {
        let index_lock = self
            .index
            .read()
            .expect("Failed to acquire read lock on document index");
        Ok(index_lock.contains_key(key))
    }

    /// Returns a copy of the data for a specific key as bytes.
    fn get_bytes(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
        let index_lock = self
            .index
            .read()
            .expect("Failed to acquire read lock on document index");
        Ok(index_lock.get(key).cloned())
    }

    /// Returns all keys available in the index.
    fn keys(&self) -> std::result::Result<Vec<String>, Self::Error> {
        let index_lock = self
            .index
            .read()
            .expect("Failed to acquire read lock on document index");
        Ok(index_lock.keys().cloned().collect())
    }

    /// Returns the number of entries in the index.
    fn len(&self) -> std::result::Result<usize, Self::Error> {
        let index_lock = self
            .index
            .read()
            .expect("Failed to acquire read lock on document index");
        Ok(index_lock.len())
    }

    /// Checks whether the index is empty.
    fn is_empty(&self) -> std::result::Result<bool, Self::Error> {
        let index_lock = self
            .index
            .read()
            .expect("Failed to acquire read lock on document index");
        Ok(index_lock.is_empty())
    }

    /// Updates the index by processing the operation-log entries.
    ///
    /// Processes PUT, DEL and PUTALL operations to keep the index synchronized.
    fn update_index(
        &mut self,
        _log: &crate::log::Log,
        entries: &[crate::log::entry::Entry],
    ) -> std::result::Result<(), Self::Error> {
        let mut index = self
            .index
            .write()
            .expect("Failed to acquire write lock on document index");

        // Process each log entry.
        for entry in entries {
            // Try to deserialize the entry's payload to obtain the operation.
            match crate::stores::operation::parse_operation(entry.clone()) {
                Ok(operation) => {
                    match operation.op() {
                        "PUT" => {
                            // For PUT, add/update the key with the value.
                            if let Some(key) = operation.key()
                                && !operation.value().is_empty()
                            {
                                index.insert(key.clone(), operation.value().to_vec());
                            }
                        }
                        "DEL" => {
                            // For DEL, remove the key from the index.
                            if let Some(key) = operation.key() {
                                index.remove(key);
                            }
                        }
                        "PUTALL" => {
                            // For PUTALL, add all documents from the operation.
                            for doc in operation.docs() {
                                index.insert(doc.key().to_string(), doc.value().to_vec());
                            }
                        }
                        _ => {
                            // Ignore unknown operations.
                        }
                    }
                }
                Err(_) => {
                    // If deserialization fails, ignore this entry.
                    // This can happen if the entry is not a valid operation.
                    continue;
                }
            }
        }

        Ok(())
    }

    /// Clears all data from the index.
    fn clear(&mut self) -> std::result::Result<(), Self::Error> {
        let mut index = self
            .index
            .write()
            .expect("Failed to acquire write lock on document index");
        index.clear();
        Ok(())
    }
}

// === IROH-DOCS-SPECIFIC METHODS ===

impl DocumentIndex {
    /// Updates the index from iroh-docs entries.
    ///
    /// This method is used by the IrohDocsDocumentStore to synchronize
    /// the local index with the iroh-docs document state.
    ///
    /// # Arguments
    /// * `entries` - Vector of iroh-docs entries (key, hash_bytes)
    pub fn update_from_iroh_entries(
        &mut self,
        entries: Vec<(String, Vec<u8>)>,
    ) -> std::result::Result<(), crate::guardian::error::GuardianError> {
        let mut index = self
            .index
            .write()
            .expect("Failed to acquire write lock on document index");

        // Clear the current index.
        index.clear();

        // Update with the new entries.
        for (key, hash_bytes) in entries {
            if !hash_bytes.is_empty() {
                index.insert(key, hash_bytes);
            }
        }

        Ok(())
    }

    /// Adds or updates a single entry in the index.
    ///
    /// # Arguments
    /// * `key` - Document key
    /// * `hash_bytes` - Blob hash (32 bytes)
    pub fn put(
        &self,
        key: String,
        hash_bytes: Vec<u8>,
    ) -> std::result::Result<(), crate::guardian::error::GuardianError> {
        let mut index = self
            .index
            .write()
            .expect("Failed to acquire write lock on document index");

        index.insert(key, hash_bytes);
        Ok(())
    }

    /// Removes an entry from the index.
    ///
    /// # Arguments
    /// * `key` - Key of the document to remove
    pub fn remove(
        &self,
        key: &str,
    ) -> std::result::Result<Option<Vec<u8>>, crate::guardian::error::GuardianError> {
        let mut index = self
            .index
            .write()
            .expect("Failed to acquire write lock on document index");

        Ok(index.remove(key))
    }

    /// Returns index statistics.
    pub fn stats(&self) -> IndexStats {
        let index = self
            .index
            .read()
            .expect("Failed to acquire read lock on document index");

        let total_keys = index.len();
        let total_bytes: usize = index.values().map(|v| v.len()).sum();

        IndexStats {
            total_keys,
            total_bytes,
        }
    }
}

/// Index statistics.
#[derive(Debug, Clone)]
pub struct IndexStats {
    /// Total number of keys.
    pub total_keys: usize,
    /// Total bytes stored (hashes).
    pub total_bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_opts() -> Arc<CreateDocumentDBOptions> {
        Arc::new(CreateDocumentDBOptions {
            marshal: Arc::new(|doc| {
                serde_json::to_vec(doc)
                    .map_err(|e| crate::guardian::error::GuardianError::Other(e.to_string()))
            }),
            unmarshal: Arc::new(|bytes| {
                serde_json::from_slice(bytes)
                    .map_err(|e| crate::guardian::error::GuardianError::Other(e.to_string()))
            }),
            key_extractor: Arc::new(|doc| {
                Ok(doc
                    .get("_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string())
            }),
            item_factory: Arc::new(|| {
                serde_json::json!({
                    "_id": "",
                    "data": {}
                })
            }),
        })
    }

    #[test]
    fn test_new_index() {
        let opts = create_test_opts();
        let index = DocumentIndex::new(opts);
        assert!(index.is_empty().unwrap());
    }

    #[test]
    fn test_put_and_get() {
        let opts = create_test_opts();
        let index = DocumentIndex::new(opts);

        let hash = vec![1u8; 32];
        index.put("key1".to_string(), hash.clone()).unwrap();

        let retrieved = index.get_bytes("key1");
        assert_eq!(retrieved, Some(hash));
    }

    #[test]
    fn test_remove() {
        let opts = create_test_opts();
        let index = DocumentIndex::new(opts);

        let hash = vec![2u8; 32];
        index.put("key2".to_string(), hash.clone()).unwrap();

        let removed = index.remove("key2").unwrap();
        assert_eq!(removed, Some(hash.clone()));

        let retrieved = index.get_bytes("key2");
        assert_eq!(retrieved, None);
    }

    #[test]
    fn test_update_from_iroh_entries() {
        let opts = create_test_opts();
        let mut index = DocumentIndex::new(opts);

        let entries = vec![
            ("key1".to_string(), vec![1u8; 32]),
            ("key2".to_string(), vec![2u8; 32]),
            ("key3".to_string(), vec![3u8; 32]),
        ];

        index.update_from_iroh_entries(entries).unwrap();

        assert_eq!(index.len().unwrap(), 3);
        assert!(index.contains_key("key1").unwrap());
        assert!(index.contains_key("key2").unwrap());
        assert!(index.contains_key("key3").unwrap());
    }

    #[test]
    fn test_stats() {
        let opts = create_test_opts();
        let index = DocumentIndex::new(opts);

        index.put("key1".to_string(), vec![1u8; 32]).unwrap();
        index.put("key2".to_string(), vec![2u8; 32]).unwrap();

        let stats = index.stats();
        assert_eq!(stats.total_keys, 2);
        assert_eq!(stats.total_bytes, 64); // 2 * 32 bytes
    }

    #[test]
    fn test_clear() {
        let opts = create_test_opts();
        let mut index = DocumentIndex::new(opts);

        index.put("key1".to_string(), vec![1u8; 32]).unwrap();
        index.put("key2".to_string(), vec![2u8; 32]).unwrap();

        assert_eq!(index.len().unwrap(), 2);

        index.clear().unwrap();
        assert!(index.is_empty().unwrap());
    }
}
