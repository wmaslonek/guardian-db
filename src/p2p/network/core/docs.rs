/// Wrapper around the iroh-docs protocol for integration with GuardianDB.
///
/// This module provides an abstraction layer over the iroh-docs protocol,
/// enabling distributed key-value storage with automatic synchronization
/// and Last-Write-Wins conflict resolution.
use crate::guardian::error::{GuardianError, Result};
use bytes::Bytes;
use iroh_docs::{AuthorId, NamespaceId, api::Doc, protocol::Docs, store::Query, sync::Entry};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

// Import IrohBackend to access Docs.
use super::IrohBackend;

/// iroh-docs client that manages documents and authors.
///
/// This wrapper simplifies using the iroh-docs protocol for KV stores,
/// encapsulating document creation, author management, and basic
/// read/write operations.
#[derive(Clone)]
pub struct WillowDocs {
    /// Instance of the Docs protocol.
    docs: Arc<Docs>,
    /// Default author used for all write operations.
    default_author: Option<AuthorId>,
}

impl WillowDocs {
    /// Creates a new iroh-docs client from the IrohBackend.
    ///
    /// # Arguments
    /// * `backend` - Reference to the IrohBackend that holds the configured Docs
    ///
    /// # Returns
    /// Ok(WillowDocs) with no default author configured, Err if Docs is not initialized
    pub async fn new(backend: Arc<IrohBackend>) -> Result<Self> {
        // Get Docs from the backend.
        let docs_lock_guard = backend.get_docs().await?;
        let docs_lock = docs_lock_guard.read().await;
        let docs = docs_lock
            .as_ref()
            .ok_or_else(|| GuardianError::Other("Docs not initialized in the backend".into()))?
            .clone();
        drop(docs_lock);

        Ok(Self {
            docs: Arc::new(docs),
            default_author: None,
        })
    }

    /// Initializes the default author for this client.
    ///
    /// Tries to use the system's default author. If none exists, creates a new one.
    /// This author will be used for all write operations.
    ///
    /// # Returns
    /// Ok(AuthorId) on success, Err if creating/getting the author fails
    pub async fn init_default_author(&mut self) -> Result<AuthorId> {
        match self.docs.author_default().await {
            Ok(author_id) => {
                info!("Using existing default author: {:?}", author_id);
                self.default_author = Some(author_id);
                Ok(author_id)
            }
            Err(_) => {
                // If no default author exists, create a new one.
                match self.docs.author_create().await {
                    Ok(author_id) => {
                        info!("Created new default author: {:?}", author_id);
                        // Try to set it as the default.
                        if let Err(e) = self.docs.author_set_default(author_id).await {
                            warn!("Failed to set default author: {:?}", e);
                        }
                        self.default_author = Some(author_id);
                        Ok(author_id)
                    }
                    Err(e) => {
                        error!("Failed to create author: {:?}", e);
                        Err(GuardianError::Storage(format!(
                            "Failed to create author: {:?}",
                            e
                        )))
                    }
                }
            }
        }
    }

    /// Returns the default author, initializing it if necessary.
    ///
    /// # Returns
    /// Ok(AuthorId) of the default author, Err if initialization fails
    pub async fn get_or_init_author(&mut self) -> Result<AuthorId> {
        if let Some(author) = self.default_author {
            Ok(author)
        } else {
            self.init_default_author().await
        }
    }

    /// Creates a new document (replica).
    ///
    /// # Returns
    /// Ok(Doc) - Handle to the created document
    /// Err(GuardianError) - If creating the document fails
    pub async fn create_doc(&self) -> Result<Doc> {
        match self.docs.create().await {
            Ok(doc) => {
                info!("Created new document: {:?}", doc.id());
                Ok(doc)
            }
            Err(e) => {
                error!("Failed to create document: {:?}", e);
                Err(GuardianError::Storage(format!(
                    "Failed to create document: {:?}",
                    e
                )))
            }
        }
    }

    /// Shares an existing document by generating a `DocTicket` (capability model).
    ///
    /// The ticket carries the namespace key (secret for writing, public for reading)
    /// and this node's addresses so the peer can connect and synchronize. Only whoever
    /// receives the ticket can import and synchronize the document — it is the
    /// replication access-control point.
    ///
    /// # Arguments
    /// * `doc` - The document to share
    /// * `write` - `true` grants write capability; `false` read-only
    pub async fn share_doc(&self, doc: &Doc, write: bool) -> Result<iroh_docs::DocTicket> {
        use iroh_docs::api::protocol::ShareMode;

        let mode = if write {
            ShareMode::Write
        } else {
            ShareMode::Read
        };

        // AddrInfoOptions::default() includes the address/relay information needed for dialing.
        doc.share(mode, Default::default()).await.map_err(|e| {
            GuardianError::Storage(format!("Failed to share document via ticket: {:?}", e))
        })
    }

    /// Imports a document from a `DocTicket`, joining the ticket's peers.
    ///
    /// This is the secure counterpart of [`share_doc`]: the node starts using the **same
    /// namespace** as the creator and begins synchronization (range-based + live) with the
    /// peers embedded in the ticket.
    pub async fn import_doc(&self, ticket: iroh_docs::DocTicket) -> Result<Doc> {
        match self.docs.import(ticket).await {
            Ok(doc) => {
                info!("Imported shared document via ticket: {:?}", doc.id());
                Ok(doc)
            }
            Err(e) => {
                error!("Failed to import document from ticket: {:?}", e);
                Err(GuardianError::Storage(format!(
                    "Failed to import document from ticket: {:?}",
                    e
                )))
            }
        }
    }

    /// Opens an existing document by its NamespaceId.
    ///
    /// # Arguments
    /// * `namespace_id` - ID of the namespace (document) to open
    ///
    /// # Returns
    /// Ok(Some(Doc)) if the document exists, Ok(None) if it does not, Err on error
    pub async fn open_doc(&self, namespace_id: NamespaceId) -> Result<Option<Doc>> {
        match self.docs.open(namespace_id).await {
            Ok(doc_option) => {
                if let Some(ref doc) = doc_option {
                    debug!("Opened existing document: {:?}", doc.id());
                }
                Ok(doc_option)
            }
            Err(e) => {
                error!("Failed to open document: {:?}", e);
                Err(GuardianError::Storage(format!(
                    "Failed to open document: {:?}",
                    e
                )))
            }
        }
    }

    /// Closes a document.
    ///
    /// # Arguments
    /// * `doc` - Reference to the document to close
    ///
    /// # Returns
    /// Ok(()) on success, Err on error
    pub async fn close_doc(&self, doc: &Doc) -> Result<()> {
        match doc.close().await {
            Ok(_) => {
                debug!("Closed document: {:?}", doc.id());
                Ok(())
            }
            Err(e) => {
                error!("Failed to close document: {:?}", e);
                Err(GuardianError::Storage(format!(
                    "Failed to close document: {:?}",
                    e
                )))
            }
        }
    }

    /// Removes a document permanently.
    ///
    /// # Arguments
    /// * `namespace_id` - ID of the namespace (document) to remove
    ///
    /// # Returns
    /// Ok(()) on success, Err on error
    pub async fn drop_doc(&self, namespace_id: NamespaceId) -> Result<()> {
        match self.docs.drop_doc(namespace_id).await {
            Ok(_) => {
                info!("Dropped document: {:?}", namespace_id);
                Ok(())
            }
            Err(e) => {
                error!("Failed to drop document: {:?}", e);
                Err(GuardianError::Storage(format!(
                    "Failed to drop document: {:?}",
                    e
                )))
            }
        }
    }

    /// Sets a value for a key in a document.
    ///
    /// # Arguments
    /// * `doc` - Reference to the document
    /// * `author_id` - Author ID for this operation
    /// * `key` - Key (will be converted to Bytes)
    /// * `value` - Value (will be converted to Bytes)
    ///
    /// # Returns
    /// Ok(Hash) - Hash of the stored content, Err on error
    pub async fn set_bytes(
        &self,
        doc: &Doc,
        author_id: AuthorId,
        key: impl Into<Bytes>,
        value: impl Into<Bytes>,
    ) -> Result<iroh_blobs::Hash> {
        match doc.set_bytes(author_id, key, value).await {
            Ok(hash) => {
                debug!("Set bytes in document {:?}: hash={:?}", doc.id(), hash);
                // Convert from iroh_docs::Hash (0.92.0) to iroh_blobs::Hash (0.94.0).
                // Both share the same hash structure (BLAKE3, 32 bytes), so the
                // conversion through the bytes is safe.
                let hash_bytes = hash.as_bytes();
                let result_hash = iroh_blobs::Hash::from_bytes(*hash_bytes);
                Ok(result_hash)
            }
            Err(e) => {
                error!("Failed to set bytes: {:?}", e);
                Err(GuardianError::Storage(format!(
                    "Failed to set bytes: {:?}",
                    e
                )))
            }
        }
    }

    /// Removes a key from a document.
    ///
    /// # Arguments
    /// * `doc` - Reference to the document
    /// * `author_id` - Author ID for this operation
    /// * `key` - Key to remove (prefix)
    ///
    /// # Returns
    /// Ok(usize) - Number of deleted entries, Err on error
    pub async fn del(
        &self,
        doc: &Doc,
        author_id: AuthorId,
        key: impl Into<Bytes>,
    ) -> Result<usize> {
        match doc.del(author_id, key).await {
            Ok(count) => {
                debug!("Deleted {} entries from document {:?}", count, doc.id());
                Ok(count)
            }
            Err(e) => {
                error!("Failed to delete: {:?}", e);
                Err(GuardianError::Storage(format!("Failed to delete: {:?}", e)))
            }
        }
    }

    /// Gets a single entry from a document.
    ///
    /// # Arguments
    /// * `doc` - Reference to the document
    /// * `query` - Query used to look up the entry
    ///
    /// # Returns
    /// Ok(Some(Entry)) if found, Ok(None) if not found, Err on error
    pub async fn get_one(&self, doc: &Doc, query: impl Into<Query>) -> Result<Option<Entry>> {
        match doc.get_one(query).await {
            Ok(entry_option) => {
                if let Some(ref entry) = entry_option {
                    debug!(
                        "Got entry from document {:?}: key={:?}",
                        doc.id(),
                        String::from_utf8_lossy(entry.key())
                    );
                }
                Ok(entry_option)
            }
            Err(e) => {
                error!("Failed to get entry: {:?}", e);
                Err(GuardianError::Storage(format!(
                    "Failed to get entry: {:?}",
                    e
                )))
            }
        }
    }

    /// Gets multiple entries from a document using a query.
    ///
    /// # Arguments
    /// * `doc` - Reference to the document
    /// * `query` - Query used to filter the entries
    ///
    /// # Returns
    /// Ok(Vec<Entry>) - List of matching entries, Err on error
    pub async fn get_many(&self, doc: &Doc, query: impl Into<Query>) -> Result<Vec<Entry>> {
        use futures::StreamExt;

        match doc.get_many(query).await {
            Ok(stream) => {
                let entries: Vec<Entry> = stream
                    .filter_map(|result| async move {
                        match result {
                            Ok(entry) => Some(entry),
                            Err(e) => {
                                warn!("Error reading entry from stream: {:?}", e);
                                None
                            }
                        }
                    })
                    .collect()
                    .await;

                debug!("Got {} entries from document {:?}", entries.len(), doc.id());
                Ok(entries)
            }
            Err(e) => {
                error!("Failed to get entries: {:?}", e);
                Err(GuardianError::Storage(format!(
                    "Failed to get entries: {:?}",
                    e
                )))
            }
        }
    }

    /// Returns the underlying Docs instance for advanced operations.
    pub fn docs(&self) -> &Arc<Docs> {
        &self.docs
    }

    /// Returns the default AuthorId (without initializing one if it does not exist).
    ///
    /// # Returns
    /// Ok(AuthorId) if a default author exists, Err otherwise
    pub async fn default_author_id(&self) -> Result<AuthorId> {
        if let Some(author) = self.default_author {
            Ok(author)
        } else {
            // Try to get the default author from docs.
            self.docs.author_default().await.map_err(|e| {
                GuardianError::Storage(format!("No default author configured: {:?}", e))
            })
        }
    }
}

#[cfg(test)]
mod tests {
    // Note: Full tests require a configured Iroh environment.
    // These are basic unit tests to verify the interface.

    #[tokio::test]
    async fn test_client_creation() {
        // Basic creation test - requires a mock Docs or skip.
        // This test serves as documentation of the expected API.
    }
}
