use crate::guardian::error::GuardianError;
use crate::log::{Log, entry::Entry};
use crate::traits::StoreIndex;

pub struct NoopIndex;

/// This is a "factory" function or constructor that creates a new instance
/// of the NoopIndex.
pub fn new_noop_index(
    _public_key: &[u8],
) -> Box<dyn StoreIndex<Error = GuardianError> + Send + Sync> {
    Box::new(NoopIndex)
}

/// `StoreIndex` trait implementation for `NoopIndex`.
/// This is where the "empty" logic is defined.
impl StoreIndex for NoopIndex {
    /// We use GuardianError as the associated error type.
    /// GuardianError implements std::error::Error.
    type Error = GuardianError;

    /// Checks whether a key exists in the index.
    fn contains_key(&self, _key: &str) -> std::result::Result<bool, Self::Error> {
        Ok(false)
    }

    /// Returns a copy of the data for a specific key as bytes.
    fn get_bytes(&self, _key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
        Ok(None)
    }

    /// Returns all keys available in the index.
    fn keys(&self) -> std::result::Result<Vec<String>, Self::Error> {
        Ok(Vec::new())
    }

    /// Returns the number of entries in the index.
    fn len(&self) -> std::result::Result<usize, Self::Error> {
        Ok(0)
    }

    /// Checks whether the index is empty.
    fn is_empty(&self) -> std::result::Result<bool, Self::Error> {
        Ok(true)
    }

    /// The function does nothing and always returns `Ok(())`.
    fn update_index(
        &mut self,
        _oplog: &Log,
        _entries: &[Entry],
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    /// Clears all data from the index.
    fn clear(&mut self) -> std::result::Result<(), Self::Error> {
        Ok(())
    }
}
