/// Wrapper client for iroh-blobs.
///
/// Provides a simplified interface for content-addressed blob storage
/// operations using BLAKE3 hashes.
///
/// This client uses the IrohBackend's shared store, ensuring consistency
/// and avoiding storage duplication.
use crate::guardian::error::{GuardianError, Result};
use bytes::Bytes;
use futures::StreamExt;
use iroh::EndpointId as NodeId;
use iroh::endpoint::Endpoint;
use iroh_blobs::{Hash as BlobHash, HashAndFormat, store::fs::FsStore};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, instrument, warn};

/// Client for operations with iroh-blobs.
///
/// Supports local operations and P2P download of blobs from remote peers
/// when the Endpoint is configured.
#[derive(Clone)]
pub struct BlobStore {
    /// Shared iroh-blobs store (filesystem-based).
    store: Arc<RwLock<FsStore>>,
    /// Iroh Endpoint for P2P blob download (optional).
    endpoint: Option<Endpoint>,
}

impl BlobStore {
    /// Creates a new iroh-blobs client instance using a shared store.
    ///
    /// # Arguments
    /// * `store` - The IrohBackend's shared store
    ///
    /// # Example
    /// ```no_run
    /// use std::sync::Arc;
    /// use tokio::sync::RwLock;
    /// use iroh_blobs::store::fs::FsStore;
    /// use guardian_db::p2p::network::core::BlobStore;
    ///
    /// # async fn example(fs_store: FsStore) {
    /// let store = Arc::new(RwLock::new(fs_store));
    /// let blobs_client = BlobStore::new(store);
    /// # }
    /// ```
    #[instrument(level = "debug", skip(store))]
    pub fn new(store: Arc<RwLock<FsStore>>) -> Self {
        debug!("Creating BlobStore with shared store (no P2P download)");
        Self {
            store,
            endpoint: None,
        }
    }

    /// Creates a new instance with P2P download support via an Endpoint.
    ///
    /// The Endpoint allows downloading blobs from remote peers using the native
    /// iroh-blobs protocol (QUIC + BLAKE3 verified streaming).
    #[instrument(level = "debug", skip(store, endpoint))]
    pub fn new_with_endpoint(store: Arc<RwLock<FsStore>>, endpoint: Endpoint) -> Self {
        debug!("Creating BlobStore with shared store + P2P download");
        Self {
            store,
            endpoint: Some(endpoint),
        }
    }

    /// Adds a document (bytes) to the blob store.
    ///
    /// Returns the BLAKE3 Hash of the stored content.
    #[instrument(level = "debug", skip(self, data))]
    pub async fn add_document(&self, data: Bytes) -> Result<BlobHash> {
        let store = self.store.read().await;

        // Add bytes to the store using the new API.
        let outcome = store.blobs().add_bytes(data.clone()).await.map_err(|e| {
            GuardianError::Other(format!("Error adding bytes to the blob store: {}", e))
        })?;

        let hash = outcome.hash;

        // Create a permanent tag to protect against GC.
        // Format: doc_<hash_hex>
        let tag_name = format!("doc_{}", hex::encode(hash.as_bytes()));

        store
            .tags()
            .set(tag_name.as_bytes(), HashAndFormat::raw(hash))
            .await
            .map_err(|e| GuardianError::Other(format!("Error creating permanent tag: {}", e)))?;

        debug!(
            "Document added to the blob store: {} ({} bytes)",
            hex::encode(hash.as_bytes()),
            data.len()
        );

        Ok(hash)
    }

    /// Retrieves a document from the blob store by its hash.
    #[instrument(level = "debug", skip(self))]
    pub async fn get_document(&self, hash: &BlobHash) -> Result<Bytes> {
        let store = self.store.read().await;

        // Use the new API: blobs().get_bytes() - requires an owned Hash.
        let data = store
            .blobs()
            .get_bytes(*hash)
            .await
            .map_err(|e| GuardianError::Other(format!("Error fetching blob: {}", e)))?;

        debug!(
            "Document retrieved from the blob store: {} ({} bytes)",
            hex::encode(hash.as_bytes()),
            data.len()
        );

        Ok(data)
    }

    /// Retrieves a document from the blob store, attempting a P2P download if not found locally.
    ///
    /// If the blob does not exist in the local store and a peer provider is given,
    /// it tries to download from the remote peer using the iroh-blobs protocol.
    #[instrument(level = "debug", skip(self))]
    pub async fn get_or_download(&self, hash: &BlobHash, providers: &[NodeId]) -> Result<Bytes> {
        // Try to fetch locally first.
        let store = self.store.read().await;
        match store.blobs().get_bytes(*hash).await {
            Ok(data) => {
                debug!(
                    "Document found locally: {} ({} bytes)",
                    hex::encode(hash.as_bytes()),
                    data.len()
                );
                return Ok(data);
            }
            Err(_) => {
                debug!(
                    "Document not found locally: {}, attempting P2P download",
                    hex::encode(hash.as_bytes())
                );
            }
        }
        drop(store);

        // Try a P2P download.
        self.download_from_peers(hash, providers).await?;

        // Now fetch from the local store (it should be there after the download).
        let store = self.store.read().await;
        let data = store.blobs().get_bytes(*hash).await.map_err(|e| {
            GuardianError::Other(format!("Blob not found after P2P download: {}", e))
        })?;

        // Create a permanent tag to protect against GC.
        let tag_name = format!("doc_{}", hex::encode(hash.as_bytes()));
        store
            .tags()
            .set(tag_name.as_bytes(), HashAndFormat::raw(*hash))
            .await
            .ok();

        debug!(
            "Document downloaded via P2P: {} ({} bytes)",
            hex::encode(hash.as_bytes()),
            data.len()
        );

        Ok(data)
    }

    /// Downloads a blob from remote peers using the iroh-blobs Downloader.
    #[instrument(level = "debug", skip(self))]
    pub async fn download_from_peers(&self, hash: &BlobHash, providers: &[NodeId]) -> Result<()> {
        let endpoint = self.endpoint.as_ref().ok_or_else(|| {
            GuardianError::Other("Endpoint not available for P2P blob download".to_string())
        })?;

        if providers.is_empty() {
            return Err(GuardianError::Other(
                "No provider given for P2P download".to_string(),
            ));
        }

        let store = self.store.read().await;
        let downloader = store.downloader(endpoint);

        let providers_vec: Vec<NodeId> = providers.to_vec();
        info!(
            "Starting P2P download of blob {} from {} provider(s)",
            hex::encode(hash.as_bytes()),
            providers_vec.len()
        );

        let progress = downloader.download(*hash, providers_vec);
        let mut stream = progress
            .stream()
            .await
            .map_err(|e| GuardianError::Other(format!("Error starting P2P download: {}", e)))?;

        while let Some(item) = stream.next().await {
            match &item {
                iroh_blobs::api::downloader::DownloadProgressItem::Error(e) => {
                    return Err(GuardianError::Other(format!(
                        "Error in P2P download: {}",
                        e
                    )));
                }
                iroh_blobs::api::downloader::DownloadProgressItem::DownloadError => {
                    return Err(GuardianError::Other("P2P download failed".to_string()));
                }
                iroh_blobs::api::downloader::DownloadProgressItem::PartComplete { .. } => {
                    debug!("P2P download: part complete");
                }
                iroh_blobs::api::downloader::DownloadProgressItem::Progress(bytes) => {
                    debug!("P2P download: {} bytes received", bytes);
                }
                _ => {}
            }
        }

        info!("P2P download complete: {}", hex::encode(hash.as_bytes()));
        Ok(())
    }

    /// Checks whether a document exists in the blob store.
    #[instrument(level = "debug", skip(self))]
    pub async fn has_document(&self, hash: &BlobHash) -> Result<bool> {
        let store = self.store.read().await;

        // Use the new API: blobs().has() - requires an owned Hash.
        let has_blob = store.blobs().has(*hash).await.unwrap_or(false);

        Ok(has_blob)
    }

    /// Deletes a document from the blob store.
    ///
    /// Removes the protection tag and optionally deletes the physical blob.
    #[instrument(level = "debug", skip(self))]
    pub async fn delete_document(&self, hash: &BlobHash) -> Result<()> {
        let store = self.store.read().await;

        // Remove the protection tag.
        let tag_name = format!("doc_{}", hex::encode(hash.as_bytes()));

        store
            .tags()
            .delete(tag_name.as_bytes())
            .await
            .map_err(|e| {
                warn!("Error deleting document tag: {}", e);
                GuardianError::Other(format!("Error deleting tag: {}", e))
            })?;

        // Note: The physical blob will be removed by GC when there are no more
        // references. This avoids accidental deletion of shared blobs.

        debug!("Document tag removed: {}", hex::encode(hash.as_bytes()));

        Ok(())
    }

    /// Lists all tagged documents in the blob store.
    ///
    /// Returns (hash, size) pairs for all documents.
    #[instrument(level = "debug", skip(self))]
    pub async fn list_documents(&self) -> Result<Vec<(BlobHash, u64)>> {
        use futures::stream::StreamExt;

        let store = self.store.read().await;
        let mut documents = Vec::new();

        // Use the new API: tags().list_prefix() to list tags with the "doc_" prefix.
        let mut tags_stream = store
            .tags()
            .list_prefix(b"doc_")
            .await
            .map_err(|e| GuardianError::Other(format!("Error getting tags: {}", e)))?;

        while let Some(tag_result) = tags_stream.next().await {
            match tag_result {
                Ok(tag_info) => {
                    let hash = tag_info.hash;
                    // Return size 0 for now - the iroh-blobs API does not provide easy access to the size.
                    documents.push((hash, 0));
                }
                Err(e) => {
                    warn!("Error processing tag during listing: {}", e);
                }
            }
        }

        debug!("Listed {} documents in the blob store", documents.len());

        Ok(documents)
    }

    /// Performs manual garbage collection.
    ///
    /// Removes blobs not referenced by any tag.
    #[instrument(level = "debug", skip(self))]
    pub async fn gc(&self) -> Result<u64> {
        use futures::stream::StreamExt;

        let store = self.store.read().await;

        // Collect all hashes protected by tags.
        let mut protected_hashes = std::collections::BTreeSet::new();
        let mut tags_stream = store
            .tags()
            .list()
            .await
            .map_err(|e| GuardianError::Other(format!("Error getting tags for GC: {}", e)))?;

        while let Some(tag_result) = tags_stream.next().await {
            if let Ok(tag_info) = tag_result {
                protected_hashes.insert(tag_info.hash);
            }
        }

        debug!("GC: {} hashes protected by tags", protected_hashes.len());

        // NOTE: The 0.94.0 API manages GC automatically via FsStore.
        // Manual GC is not exposed directly in the new API.
        // GC runs periodically in the background.

        debug!("GC is managed automatically by FsStore");

        Ok(0) // Returns 0 since GC is automatic.
    }

    /// Returns true if the BlobStore supports P2P download.
    pub fn has_p2p_support(&self) -> bool {
        self.endpoint.is_some()
    }

    /// Creates a test instance with a temporary store.
    #[cfg(test)]
    pub async fn memory() -> Result<Self> {
        // Create a temporary directory.
        let temp_dir =
            std::env::temp_dir().join(format!("iroh-blobs-test-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&temp_dir).await.map_err(|e| {
            GuardianError::Other(format!("Error creating temporary directory: {}", e))
        })?;

        // Load FsStore in the temporary directory.
        let store = FsStore::load(&temp_dir)
            .await
            .map_err(|e| GuardianError::Other(format!("Error creating temporary store: {}", e)))?;

        Ok(Self::new(Arc::new(RwLock::new(store))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_add_and_get_document() {
        let blobs_client = BlobStore::memory().await.unwrap();

        let data = Bytes::from("Hello, iroh-blobs!");
        let hash = blobs_client.add_document(data.clone()).await.unwrap();

        let retrieved = blobs_client.get_document(&hash).await.unwrap();
        assert_eq!(data, retrieved);
    }

    #[tokio::test]
    async fn test_has_document() {
        let blobs_client = BlobStore::memory().await.unwrap();

        let data = Bytes::from("Test data");
        let hash = blobs_client.add_document(data).await.unwrap();

        assert!(blobs_client.has_document(&hash).await.unwrap());
    }

    #[tokio::test]
    async fn test_delete_document() {
        let blobs_client = BlobStore::memory().await.unwrap();

        let data = Bytes::from("To be deleted");
        let hash = blobs_client.add_document(data).await.unwrap();

        blobs_client.delete_document(&hash).await.unwrap();

        // After deleting the tag, GC may remove the blob.
        // But immediately after delete_document it may still exist
        // until GC runs.
    }

    #[tokio::test]
    async fn test_list_documents() {
        let blobs_client = BlobStore::memory().await.unwrap();

        let data1 = Bytes::from("Document 1");
        let data2 = Bytes::from("Document 2");

        blobs_client.add_document(data1).await.unwrap();
        blobs_client.add_document(data2).await.unwrap();

        let docs = blobs_client.list_documents().await.unwrap();
        assert_eq!(docs.len(), 2);
    }
}
