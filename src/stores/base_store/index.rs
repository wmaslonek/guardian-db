use crate::guardian::error::GuardianError;
use crate::log::{Log, entry::Entry};
use crate::traits::StoreIndex;
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

/// BaseIndex is the base of an index for Log Stores.
/// It maintains a mapping of keys to values, processing operation-log entries
/// to keep the state up to date.
pub struct BaseIndex {
    /// Index ID, usually the store's public key.
    id: Vec<u8>,

    /// Internal map for the hash-based index.
    /// Stores values as bytes for flexibility, protected by an RwLock
    /// to allow safe concurrent access (multiple readers or a single writer).
    index: RwLock<HashMap<String, Vec<u8>>>,
}

/// Constructor for the `BaseIndex`. Creates a new instance with an empty HashMap
/// to store the key-value index.
pub fn new_base_index(
    public_key: Vec<u8>,
) -> Box<dyn StoreIndex<Error = GuardianError> + Send + Sync> {
    Box::new(BaseIndex {
        id: public_key,
        index: RwLock::new(HashMap::new()),
    })
}

/// `StoreIndex` trait implementation for `BaseIndex`.
impl StoreIndex for BaseIndex {
    /// Specifies that we will use GuardianError as the associated error type.
    type Error = GuardianError;

    /// Checks whether a key exists in the index.
    fn contains_key(&self, key: &str) -> Result<bool, Self::Error> {
        let index_lock = self
            .index
            .read()
            .map_err(|e| GuardianError::Store(format!("Failed to acquire read lock: {}", e)))?;

        Ok(index_lock.contains_key(key))
    }

    /// Returns a copy of the data for a specific key as bytes.
    fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
        let index_lock = self
            .index
            .read()
            .map_err(|e| GuardianError::Store(format!("Failed to acquire read lock: {}", e)))?;

        Ok(index_lock.get(key).cloned())
    }

    /// Returns all keys available in the index.
    fn keys(&self) -> Result<Vec<String>, Self::Error> {
        let index_lock = self
            .index
            .read()
            .map_err(|e| GuardianError::Store(format!("Failed to acquire read lock: {}", e)))?;

        Ok(index_lock.keys().cloned().collect())
    }

    /// Returns the number of entries in the index.
    fn len(&self) -> Result<usize, Self::Error> {
        let index_lock = self
            .index
            .read()
            .map_err(|e| GuardianError::Store(format!("Failed to acquire read lock: {}", e)))?;

        Ok(index_lock.len())
    }

    /// Checks whether the index is empty.
    fn is_empty(&self) -> Result<bool, Self::Error> {
        let index_lock = self
            .index
            .read()
            .map_err(|e| GuardianError::Store(format!("Failed to acquire read lock: {}", e)))?;

        Ok(index_lock.is_empty())
    }

    /// Updates the index by processing the operation-log entries.
    /// Implements the CRDT logic by processing PUT and DEL operations.
    fn update_index(&mut self, _log: &Log, entries: &[Entry]) -> Result<(), Self::Error> {
        // Set to track already-processed keys, ensuring that
        // only the most recent operation for each key is applied.
        let mut handled = HashSet::new();

        // Acquire a write lock to modify the index safely.
        let mut index = self
            .index
            .write()
            .map_err(|e| GuardianError::Store(format!("Failed to acquire write lock: {}", e)))?;

        // Iterate over the provided entries in reverse order (newest to oldest).
        // This ensures that only the most recent operation for each key is applied.
        for entry in entries.iter().rev() {
            // Parse the operation from the log entry.
            let operation = match crate::stores::operation::parse_operation(entry.clone()) {
                Ok(op) => op,
                Err(e) => {
                    // Log the error but continue processing other entries.
                    eprintln!("Warning: Error parsing operation: {}", e);
                    continue;
                }
            };

            // Get the operation's key.
            let key = match operation.key() {
                Some(k) if !k.is_empty() => k,
                _ => continue, // Ignore entries with a null or empty key.
            };

            // Avoid processing the same key multiple times.
            if handled.contains(key) {
                continue;
            }
            handled.insert(key.clone());

            // Apply the operation based on its type.
            match operation.op() {
                "PUT" => {
                    let value = operation.value();
                    if !value.is_empty() {
                        index.insert(key.clone(), value.to_vec());
                    }
                }
                "DEL" => {
                    index.remove(key);
                }
                _ => {
                    // Ignore unknown operations.
                    eprintln!("Warning: Unknown operation ignored: {}", operation.op());
                }
            }
        }

        Ok(())
    }

    /// Clears all data from the index.
    fn clear(&mut self) -> Result<(), Self::Error> {
        let mut index_lock = self
            .index
            .write()
            .map_err(|e| GuardianError::Store(format!("Failed to acquire write lock: {}", e)))?;

        index_lock.clear();
        Ok(())
    }
}

impl BaseIndex {
    /// Returns the index ID (public key).
    pub fn id(&self) -> &[u8] {
        &self.id
    }

    /// Returns a copy of the value associated with the key, if it exists.
    /// This is a convenience method that calls the trait's get_bytes().
    pub fn get_value(&self, key: &str) -> Result<Option<Vec<u8>>, GuardianError> {
        self.get_bytes(key)
    }
}
