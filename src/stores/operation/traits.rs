use crate::log::entry::Entry;

pub trait OpDoc {
    fn get_key(&self) -> &str;
    fn get_value(&self) -> &[u8];
}

/// Describes a CRDT operation.
pub trait Operation {
    /// Gets a key, if applicable (e.g. for key-value stores).
    fn get_key(&self) -> Option<&String>;

    /// Returns the operation name (e.g. "append", "put", "remove").
    fn get_operation(&self) -> &str;

    /// Returns the operation's payload (data).
    fn get_value(&self) -> &[u8];

    /// Gets the underlying Log Entry.
    fn get_entry(&self) -> &Entry;

    /// Gets the list of documents.
    fn get_docs(&self) -> Vec<Box<dyn OpDoc>>;

    /// Serializes the operation.
    fn marshal(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>>;
}
