// High-level Guardian DB API client.
//
// This module provides a simplified interface for using Guardian DB, focusing on:
// - Factory methods for different environments
// - Convenient helper methods (add_bytes, get_channel_id)
// - Subsystem integration (docs, blobs, document stores)
// - Local PubSub (in-process only, not distributed over the network)
//
// For direct access to the backend's optimized features (cache, connection pool,
// performance monitor, etc.), use IrohBackend directly via backend().

use crate::guardian::error::{GuardianError, Result};
use crate::p2p::network::{config::ClientConfig, types::*};
use iroh::{EndpointId as NodeId, SecretKey};
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// High-level Guardian DB API client.
///
/// Provides:
/// - Convenient factory methods for different environments
/// - Useful helpers (add_bytes, get_channel_id)
/// - Subsystem integration (docs, blobs, document stores)
///
/// For low-level operations and access to optimizations (cache, connection pool,
/// performance monitor, key synchronizer), use `backend()` to obtain the IrohBackend.
/// For P2P communication, use EpidemicPubSub via backend.create_pubsub_interface().
#[derive(Clone)]
pub struct IrohClient {
    /// Optimized Iroh backend (accessed via backend() for advanced operations).
    backend: Arc<crate::p2p::network::core::IrohBackend>,
    /// Client configuration.
    config: ClientConfig,
    /// Node ID (Iroh NodeId).
    node_id: NodeId,
    /// Node secret key (Iroh SecretKey).
    secret_key: SecretKey,
    /// iroh-docs client for distributed KV stores (optional).
    docs_client: Arc<RwLock<Option<crate::p2p::network::core::docs::WillowDocs>>>,
    /// iroh-blobs client for content-addressed storage (optional).
    blobs_client: Arc<RwLock<Option<crate::p2p::network::core::blobs::BlobStore>>>,
}

impl IrohClient {
    /// Creates a new Guardian DB client instance.
    ///
    /// Initializes the optimized backend and prepares optional subsystems.
    pub async fn new(config: ClientConfig) -> Result<Self> {
        // Validate the configuration.
        config
            .validate()
            .map_err(|e| GuardianError::Other(format!("Invalid configuration: {}", e)))?;

        info!("Initializing Guardian DB client");

        // Create the optimized Iroh backend first.
        let backend = Arc::new(crate::p2p::network::core::IrohBackend::new(&config).await?);

        // Get the NodeId from the backend (it loads or generates the persistent key).
        let node_info = backend.id().await?;
        let node_id = node_info.id;

        // Get the secret_key from the backend to keep consistency.
        let secret_key = backend.secret_key().clone();

        info!("NodeId: {}", node_id);

        let client = Self {
            backend,
            config: config.clone(),
            node_id,
            secret_key,
            docs_client: Arc::new(RwLock::new(None)),
            blobs_client: Arc::new(RwLock::new(None)),
        };

        // Initialize iroh-blobs automatically using the backend's shared store.
        if config.data_store_path.is_some() {
            match client.init_blobs().await {
                Ok(_) => info!("✓ iroh-blobs initialized with shared store"),
                Err(e) => {
                    warn!("Warning: iroh-blobs not initialized: {}", e);
                    debug!("  Use init_blobs() manually if needed");
                }
            }
        }

        info!("✓ Guardian DB client initialized");
        Ok(client)
    }

    /// Creates an instance with the default configuration.
    pub async fn default() -> Result<Self> {
        Self::new(ClientConfig::default()).await
    }

    /// Creates an instance for development.
    pub async fn development() -> Result<Self> {
        Self::new(ClientConfig::development()).await
    }

    /// Creates an instance for production.
    pub async fn production() -> Result<Self> {
        Self::new(ClientConfig::production()).await
    }

    /// Creates an instance for testing.
    pub async fn testing() -> Result<Self> {
        Self::new(ClientConfig::testing()).await
    }

    /// Creates an instance using an existing Iroh backend.
    pub async fn new_with_backend(
        backend: Arc<crate::p2p::network::core::IrohBackend>,
    ) -> Result<Self> {
        let config = ClientConfig::default();

        // Generate a key and NodeId using Iroh.
        let secret_key = SecretKey::generate();
        let node_id = secret_key.public();

        Ok(Self {
            backend,
            config,
            node_id,
            secret_key,
            docs_client: Arc::new(RwLock::new(None)),
            blobs_client: Arc::new(RwLock::new(None)),
        })
    }

    // ==================== Backend Access ====================

    /// Returns a reference to the optimized Iroh backend.
    ///
    /// Use it for direct access to:
    /// - Intelligent cache: `backend.optimized_cache`
    /// - Connection pool: `backend.list_active_connections()`
    /// - Performance monitor: `backend.get_performance_metrics()`
    /// - Key synchronizer: `backend.sync_key_with_peers()`
    /// - Networking metrics: `backend.get_networking_metrics()`
    ///
    /// # Example
    /// ```no_run
    /// # use guardian_db::p2p::network::IrohClient;
    /// # use guardian_db::guardian::error::Result;
    /// # async fn example() -> Result<()> {
    /// let client = IrohClient::development().await?;
    ///
    /// // Direct access to the optimized backend.
    /// let backend = client.backend();
    /// let report = backend.generate_performance_report().await;
    /// println!("{}", report);
    /// # Ok(())
    /// # }
    /// ```
    pub fn backend(&self) -> &Arc<crate::p2p::network::core::IrohBackend> {
        &self.backend
    }

    /// Returns whether the node is online.
    pub async fn is_online(&self) -> bool {
        self.backend.is_online().await
    }

    // ==================== Helper Methods (Value-Added) ====================

    /// Helper: Adds Vec<u8> data to the backend.
    ///
    /// Converts Vec<u8> into AsyncRead automatically.
    ///
    /// # Example
    /// ```no_run
    /// # use guardian_db::p2p::network::IrohClient;
    /// # use guardian_db::guardian::error::Result;
    /// # async fn example() -> Result<()> {
    /// let client = IrohClient::development().await?;
    /// let data = b"Hello, Guardian!".to_vec();
    /// let response = client.add_bytes(data).await?;
    /// println!("Hash: {}", response.hash);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn add_bytes(&self, data: Vec<u8>) -> Result<AddResponse> {
        struct BytesReader {
            data: Vec<u8>,
            pos: usize,
        }

        impl tokio::io::AsyncRead for BytesReader {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                let remaining = self.data.len() - self.pos;
                let to_read = std::cmp::min(remaining, buf.remaining());

                if to_read == 0 {
                    return std::task::Poll::Ready(Ok(()));
                }

                buf.put_slice(&self.data[self.pos..self.pos + to_read]);
                self.pos += to_read;

                std::task::Poll::Ready(Ok(()))
            }
        }

        let reader = BytesReader { data, pos: 0 };
        let pinned_data = Pin::new(Box::new(reader));
        self.backend.add(pinned_data).await
    }

    /// Helper: Retrieves data from the backend and reads it into a Vec<u8>.
    ///
    /// # Example
    /// ```no_run
    /// # use guardian_db::p2p::network::IrohClient;
    /// # use guardian_db::guardian::error::Result;
    /// # async fn example() -> Result<()> {
    /// let client = IrohClient::development().await?;
    /// let hash = "..."; // content hash
    /// let data = client.cat_bytes(hash).await?;
    /// println!("Retrieved {} bytes", data.len());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn cat_bytes(&self, hash: &str) -> Result<Vec<u8>> {
        let mut reader = self.backend.cat(hash).await?;
        let mut data = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut data).await?;
        Ok(data)
    }

    /// Helper: Generates a unique ID for a peer-to-peer communication channel.
    ///
    /// Format: `/guardian-db/one-on-one-channel/1/{peer1}/{peer2}` (sorted)
    pub fn get_channel_id(&self, other_peer: &NodeId) -> String {
        let mut channel_id_peers = [self.node_id.to_string(), other_peer.to_string()];
        channel_id_peers.sort();
        format!(
            "/guardian-db/one-on-one-channel/1/{}",
            channel_id_peers.join("/")
        )
    }

    // ==================== Getters and Information ====================

    /// Returns the current configuration.
    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Returns the node ID (Iroh NodeId).
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Returns the node secret key (Iroh SecretKey).
    pub fn secret_key(&self) -> &SecretKey {
        &self.secret_key
    }

    /// Returns node information via the backend.
    pub async fn id(&self) -> Result<NodeInfo> {
        self.backend.id().await
    }

    /// Adds an EndpointAddr to the endpoint (useful for tests where discovery does not work).
    ///
    /// Iroh 1.0: the Endpoint::add_node_addr method was removed. The static address is
    /// registered via a MemoryLookup (formerly StaticProvider) added to the endpoint's
    /// address lookup services, from where resolve()/connect() can query it.
    pub async fn add_node_addr(&self, node_addr: iroh::EndpointAddr) -> Result<()> {
        let endpoint_arc = self.backend.get_endpoint().await?;
        let endpoint_lock = endpoint_arc.read().await;
        let endpoint = endpoint_lock
            .as_ref()
            .ok_or_else(|| GuardianError::Other("Endpoint not available".to_string()))?;

        let lookup = iroh::address_lookup::memory::MemoryLookup::new();
        lookup.add_endpoint_info(node_addr);
        endpoint
            .address_lookup()
            .map_err(|e| GuardianError::Other(format!("Address lookup unavailable: {}", e)))?
            .add(lookup);
        Ok(())
    }

    /// Connects to the peer via the gossip ALPN and registers the connection with Gossip.
    ///
    /// IMPORTANT: This function opens a QUIC connection with the gossip ALPN and passes it
    /// to Gossip.handle_connection() so it is kept alive. Without this, the connection
    /// would be closed on going out of scope, causing "closed by peer: 0".
    pub async fn connect_gossip(&self, node_id: NodeId) -> Result<()> {
        // Register the peer as known (a candidate for automatic DocTicket exchange).
        self.backend.note_known_peer(node_id).await;

        let endpoint_arc = self.backend.get_endpoint().await?;
        let endpoint_lock = endpoint_arc.read().await;
        let endpoint = endpoint_lock
            .as_ref()
            .ok_or_else(|| GuardianError::Other("Endpoint not available".to_string()))?;

        // Get the Gossip from the backend.
        let gossip_arc = self.backend.get_gossip().await?;
        let gossip_lock = gossip_arc.read().await;
        let gossip = gossip_lock
            .as_ref()
            .ok_or_else(|| GuardianError::Other("Gossip not initialized".to_string()))?
            .clone();
        drop(gossip_lock);

        // Open a QUIC connection with the gossip ALPN.
        // Use the library constant so this always matches the ALPN the Router accepts.
        let connection = endpoint
            .connect(node_id, iroh_gossip::ALPN)
            .await
            .map_err(|e| GuardianError::Other(format!("Failed to connect via gossip: {}", e)))?;

        // CRUCIAL: Pass the connection to Gossip so it keeps it alive.
        // Without this, the connection would be closed on going out of scope.
        gossip.handle_connection(connection).await.map_err(|e| {
            GuardianError::Other(format!("Failed to register connection with gossip: {}", e))
        })?;

        Ok(())
    }

    // ==================== Subsystem Integration ====================
    // iroh-docs methods.

    /// Initializes the iroh-docs client by obtaining Docs from the backend.
    ///
    /// # Returns
    /// Ok(()) if initialized successfully, Err on error
    pub async fn init_docs(&self) -> Result<()> {
        // Get the backend to pass to WillowDocs.
        let backend = self.backend.clone();

        let mut client = crate::p2p::network::core::docs::WillowDocs::new(backend).await?;

        // Initialize the default author.
        client.init_default_author().await?;

        // Store the client.
        let mut docs_guard = self.docs_client.write().await;
        *docs_guard = Some(client);

        info!("iroh-docs client initialized successfully");
        Ok(())
    }

    /// Returns a reference to the iroh-docs client if it is initialized.
    ///
    /// # Returns
    /// Some(WillowDocs) if initialized, None otherwise
    pub async fn docs_client(&self) -> Option<crate::p2p::network::core::docs::WillowDocs> {
        let guard = self.docs_client.read().await;
        (*guard).clone()
    }

    /// Returns whether the iroh-docs client is initialized.
    pub async fn has_docs_client(&self) -> bool {
        let guard = self.docs_client.read().await;
        guard.is_some()
    }

    // ==================== iroh-blobs methods ====================

    /// Initializes the iroh-blobs client using the backend's shared store.
    ///
    /// The iroh-blobs client now uses the IrohBackend's shared store,
    /// ensuring consistency and avoiding storage duplication.
    ///
    /// # Returns
    /// Ok(()) if initialized successfully, Err on error
    pub async fn init_blobs(&self) -> Result<()> {
        // Get the store from the backend.
        let store = self.backend.get_store_for_blobs().await?;

        // Get the endpoint for P2P blob download.
        let endpoint_arc = self.backend.get_endpoint().await?;
        let endpoint_lock = endpoint_arc.read().await;
        let endpoint = endpoint_lock.as_ref().cloned();
        drop(endpoint_lock);

        // Create the client with the shared store + P2P download.
        let client = if let Some(ep) = endpoint {
            info!("iroh-blobs client initialized with shared store + P2P download");
            crate::p2p::network::core::blobs::BlobStore::new_with_endpoint(store, ep)
        } else {
            info!("iroh-blobs client initialized with shared store (no P2P)");
            crate::p2p::network::core::blobs::BlobStore::new(store)
        };

        let mut blobs_guard = self.blobs_client.write().await;
        *blobs_guard = Some(client);

        Ok(())
    }

    /// Returns a reference to the iroh-blobs client if it is initialized.
    ///
    /// # Returns
    /// Some(BlobStore) if initialized, None otherwise
    pub async fn blobs_client(&self) -> Option<crate::p2p::network::core::blobs::BlobStore> {
        let guard = self.blobs_client.read().await;
        (*guard).clone()
    }

    /// Returns whether the iroh-blobs client is initialized.
    pub async fn has_blobs_client(&self) -> bool {
        let guard = self.blobs_client.read().await;
        guard.is_some()
    }

    // ==================== Document Store Factory ====================

    /// Creates a new GuardianDBDocumentStore instance.
    ///
    /// # Arguments
    /// * `identity` - Identity for operations on the store
    /// * `addr` - Store address
    /// * `options` - Store configuration options
    ///
    /// # Returns
    /// Ok(GuardianDBDocumentStore) on success
    ///
    /// # Example
    /// ```no_run
    /// use guardian_db::p2p::network::IrohClient;
    /// use guardian_db::p2p::network::config::ClientConfig;
    /// use guardian_db::log::identity::{DefaultIdentificator, Identificator};
    /// use guardian_db::traits::NewStoreOptions;
    /// use guardian_db::stores::document_store::default_store_opts_for_map;
    /// use guardian_db::guardian::error::Result;
    /// use guardian_db::address;
    /// use std::sync::Arc;
    ///
    /// # async fn example() -> Result<()> {
    /// let client = IrohClient::new(ClientConfig::development()).await?;
    ///
    /// // Create an identity.
    /// let mut identificator = DefaultIdentificator::new();
    /// let identity = Arc::new(identificator.create("user"));
    ///
    /// // Create an address from a string.
    /// let addr = Arc::new(address::parse("/guardiandb/zdpuAm...")?);
    ///
    /// // Configure options - use the helper function to create default options.
    /// let doc_opts = default_store_opts_for_map("_id");
    /// let options = NewStoreOptions {
    ///     store_specific_opts: Some(Box::new(doc_opts)),
    ///     ..Default::default()
    /// };
    ///
    /// let store = client.create_document_store(
    ///     identity,
    ///     addr,
    ///     options
    /// ).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn create_document_store(
        &self,
        identity: Arc<crate::log::identity::Identity>,
        addr: Arc<dyn crate::address::Address>,
        options: crate::traits::NewStoreOptions,
    ) -> Result<crate::stores::document_store::GuardianDBDocumentStore> {
        crate::stores::document_store::GuardianDBDocumentStore::new(
            Arc::new(self.clone()),
            identity,
            addr,
            options,
        )
        .await
    }

    /// Shuts down the client.
    pub async fn shutdown(&self) -> Result<()> {
        info!("Shutting down Guardian DB client");
        info!("Guardian DB client shut down successfully");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_client_creation() {
        let mut config = ClientConfig::development();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        config.data_store_path = Some(format!("./tmp/test_creation_{}", timestamp).into());
        let client = IrohClient::new(config).await;
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn test_client_online() {
        let mut config = ClientConfig::development();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        config.data_store_path = Some(format!("./tmp/test_online_{}", timestamp).into());
        let client = IrohClient::new(config).await.unwrap();
        assert!(client.is_online().await);
    }

    #[tokio::test]
    async fn test_blobs_client_initialization() {
        let mut config = ClientConfig::development();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let data_path = format!("./tmp/test_blobs_{}", timestamp);
        config.data_store_path = Some(data_path.into());
        let client = IrohClient::new(config).await.unwrap();

        // Check that blobs_client was initialized automatically.
        assert!(
            client.has_blobs_client().await,
            "blobs_client should be initialized automatically"
        );

        // Check that we can obtain a reference.
        let blobs = client.blobs_client().await;
        assert!(blobs.is_some(), "blobs_client() should return Some");
    }

    #[tokio::test]
    async fn test_blobs_client_manual_init() {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        let mut config = ClientConfig::development();
        let base_path = format!("./tmp/test_manual_blobs_{}", timestamp);
        config.data_store_path = Some(base_path.clone().into()); // The backend needs a data_path.

        let client = IrohClient::new(config).await.unwrap();

        // With data_store_path, blobs_client is already initialized automatically.
        assert!(
            client.has_blobs_client().await,
            "blobs_client should be initialized automatically when data_store_path is present"
        );

        // Re-initialization test (now uses the backend's store).
        let result = client.init_blobs().await;
        assert!(result.is_ok(), "Re-initialization should be allowed");
    }

    #[tokio::test]
    async fn test_add_bytes_helper() {
        let mut config = ClientConfig::development();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        config.data_store_path = Some(format!("./tmp/test_add_bytes_{}", timestamp).into());
        let client = IrohClient::new(config).await.unwrap();

        let test_data = b"Hello, Guardian!".to_vec();
        let response = client.add_bytes(test_data.clone()).await.unwrap();

        assert!(!response.hash.is_empty());
        assert_eq!(response.size_bytes().unwrap(), test_data.len());

        // Test cat_bytes helper
        let retrieved = client.cat_bytes(&response.hash).await.unwrap();
        assert_eq!(retrieved, test_data);
    }

    #[tokio::test]
    async fn test_backend_access() {
        let mut config = ClientConfig::development();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        config.data_store_path = Some(format!("./tmp/test_backend_{}", timestamp).into());
        let client = IrohClient::new(config).await.unwrap();

        // Test backend access
        let backend = client.backend();
        let report = backend.generate_performance_report().await;
        assert!(!report.is_empty());
    }

    #[tokio::test]
    async fn test_node_info() {
        let test_dir = format!("./tmp/iroh_test_info_{}", std::process::id());
        let mut config = ClientConfig::development();
        config.data_store_path = Some(test_dir.into());
        let client = IrohClient::new(config).await.unwrap();
        let info = client.id().await.unwrap();
        // The node_id should be consistent with the backend's NodeId.
        assert_eq!(info.id, client.node_id());
    }

    #[tokio::test]
    async fn test_get_channel_id() {
        let client = IrohClient::development().await.unwrap();
        let other_peer = SecretKey::generate().public();

        let channel_id = client.get_channel_id(&other_peer);
        assert!(channel_id.starts_with("/guardian-db/one-on-one-channel/1/"));

        // Should be deterministic.
        let channel_id2 = client.get_channel_id(&other_peer);
        assert_eq!(channel_id, channel_id2);
    }

    #[tokio::test]
    async fn test_error_handling() {
        let mut config = ClientConfig::development();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        config.data_store_path = Some(format!("./tmp/test_errors_{}", timestamp).into());
        let client = IrohClient::new(config).await.unwrap();

        // Test cat_bytes with non-existent hash
        let result = client.cat_bytes("invalid_hash").await;
        assert!(result.is_err());
    }
}
