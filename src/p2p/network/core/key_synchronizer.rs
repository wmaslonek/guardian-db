/// Key synchronization system for Guardian DB.
///
/// Robust key synchronization between peers, ensuring cryptographic consistency
/// and preventing replay attacks.
use crate::guardian::error::{GuardianError, Result};
use crate::keystore::RedbKeystore;
use crate::log::identity_provider::Keystore;
use crate::p2p::network::config::ClientConfig;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use iroh::EndpointId as NodeId;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Synchronization protocol version.
const SYNC_PROTOCOL_VERSION: u32 = 1;

/// Maximum age for accepting messages (prevents replay attacks).
const MAX_MESSAGE_AGE: Duration = Duration::from_secs(300); // 5 minutes

/// Maximum number of synchronization retries (reserved for future use).
#[allow(dead_code)]
const MAX_SYNC_RETRIES: u8 = 3;

/// Maximum size of the synchronization queue.
const MAX_SYNC_QUEUE_SIZE: usize = 1000;

/// Synchronization status of a key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeySyncStatus {
    /// The key is synchronized.
    Synchronized,
    /// The key is being synchronized.
    Synchronizing,
    /// Synchronization pending.
    Pending,
    /// Synchronization error.
    Failed(String),
    /// Conflict detected (manual resolution required).
    Conflict(String),
}

/// Type of synchronization operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncOperation {
    /// Create a new key.
    Create,
    /// Update an existing key.
    Update,
    /// Delete a key.
    Delete,
    /// Synchronize metadata.
    MetadataSync,
}

/// Metadata of a synchronized key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyMetadata {
    /// Unique key ID.
    pub key_id: String,
    /// Key version (for conflict control).
    pub version: u64,
    /// Timestamp of the last modification.
    pub last_modified: DateTime<Utc>,
    /// NodeID that created the key.
    pub creator: NodeId,
    /// Signature of the metadata.
    pub signature: Vec<u8>,
    /// Encryption algorithm used.
    pub crypto_algorithm: String,
    /// Hash of the public key.
    pub public_key_hash: Vec<u8>,
}

/// Synchronization message between peers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncMessage {
    /// Unique message ID.
    pub message_id: Uuid,
    /// Protocol version.
    pub protocol_version: u32,
    /// Message timestamp.
    pub timestamp: SystemTime,
    /// NodeID of the sender.
    pub sender: NodeId,
    /// Operation type.
    pub operation: SyncOperation,
    /// Key metadata.
    pub metadata: KeyMetadata,
    /// Key data (encrypted).
    pub key_data: Option<Vec<u8>>,
    /// Signature of the complete message.
    pub message_signature: Vec<u8>,
}

/// Entry in the synchronization queue (reserved for future use).
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct SyncQueueEntry {
    /// Message to be synchronized.
    #[allow(dead_code)]
    message: SyncMessage,
    /// Number of attempts.
    #[allow(dead_code)]
    retry_count: u8,
    /// Next attempt.
    #[allow(dead_code)]
    next_retry: SystemTime,
    /// Peers that should receive the message.
    #[allow(dead_code)]
    target_peers: Vec<NodeId>,
}

/// Synchronization statistics.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SyncStatistics {
    /// Total messages synchronized.
    pub messages_synced: u64,
    /// Messages pending in the queue.
    pub pending_messages: u64,
    /// Conflicts detected.
    pub conflicts_detected: u64,
    /// Conflicts resolved.
    pub conflicts_resolved: u64,
    /// Synchronization success rate.
    pub success_rate: f64,
    /// Average synchronization latency (ms).
    pub avg_sync_latency_ms: f64,
    /// Active peers in synchronization.
    pub active_peers: u32,
}

/// Main key synchronization system.
pub struct KeySynchronizer {
    /// System configuration (reserved for future use).
    #[allow(dead_code)]
    client_config: ClientConfig,
    /// Local keystore.
    local_keystore: Arc<RedbKeystore>,
    /// The node's main keypair (Ed25519).
    node_signing_key: SigningKey,
    /// The node's NodeID.
    node_id: NodeId,
    /// Mapping of synchronized keys.
    synchronized_keys: Arc<RwLock<HashMap<String, KeyMetadata>>>,
    /// Synchronization status per key.
    sync_status: Arc<RwLock<HashMap<String, KeySyncStatus>>>,
    /// Synchronization queue.
    sync_queue: Arc<Mutex<VecDeque<SyncQueueEntry>>>,
    /// Cache of recent messages (prevents replay).
    message_cache: Arc<RwLock<HashMap<Uuid, SystemTime>>>,
    /// Synchronization statistics.
    statistics: Arc<RwLock<SyncStatistics>>,
    /// Trusted keys (authorized peers).
    trusted_peers: Arc<RwLock<HashMap<NodeId, VerifyingKey>>>,
}

impl KeySynchronizer {
    /// Creates a new key synchronizer instance.
    pub async fn new(client_config: &ClientConfig) -> Result<Self> {
        let keystore_path = client_config
            .data_store_path
            .as_ref()
            .map(|p| p.join("keystore"))
            .unwrap_or_else(|| std::env::temp_dir().join("guardian_keystore"));

        let local_keystore = Arc::new(RedbKeystore::new(Some(keystore_path))?);

        // Load or generate the main keypair.
        let node_signing_key = Self::load_or_generate_keypair(&local_keystore).await?;
        // Iroh 1.0: PublicKey (EndpointId) no longer implements From<ed25519_dalek::VerifyingKey>.
        // We convert via raw bytes (both are 32-byte Ed25519 keys).
        let node_id = NodeId::from_bytes(node_signing_key.verifying_key().as_bytes())
            .map_err(|e| GuardianError::Other(format!("Invalid public key: {}", e)))?;

        info!("Initializing key synchronizer for NodeID: {}", node_id);

        Ok(Self {
            client_config: client_config.clone(),
            local_keystore,
            node_signing_key,
            node_id,
            synchronized_keys: Arc::new(RwLock::new(HashMap::new())),
            sync_status: Arc::new(RwLock::new(HashMap::new())),
            sync_queue: Arc::new(Mutex::new(VecDeque::new())),
            message_cache: Arc::new(RwLock::new(HashMap::new())),
            statistics: Arc::new(RwLock::new(SyncStatistics::default())),
            trusted_peers: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Returns the node's NodeID.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Returns the node's signing key.
    pub fn signing_key(&self) -> &SigningKey {
        &self.node_signing_key
    }

    /// Loads or generates the main keypair.
    async fn load_or_generate_keypair(keystore: &RedbKeystore) -> Result<SigningKey> {
        const MAIN_KEYPAIR_KEY: &str = "main_node_keypair";

        // Try to load an existing keypair.
        if let Some(data) = keystore.get(MAIN_KEYPAIR_KEY).await?
            && data.len() == 32
        {
            debug!("Loading existing main keypair");
            return SigningKey::try_from(&data[..32])
                .map_err(|e| GuardianError::Other(format!("Error loading keypair: {}", e)));
        }

        // Generate a new keypair.
        let mut secret_bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut secret_bytes);
        let signing_key = SigningKey::from_bytes(&secret_bytes);
        keystore
            .put(MAIN_KEYPAIR_KEY, signing_key.as_bytes())
            .await?;

        info!("New main keypair generated and saved");
        Ok(signing_key)
    }

    /// Adds a trusted peer for synchronization.
    pub async fn add_trusted_peer(&self, node_id: NodeId, public_key: VerifyingKey) -> Result<()> {
        let mut trusted = self.trusted_peers.write().await;
        trusted.insert(node_id, public_key);
        info!("Trusted peer added: {}", node_id);
        Ok(())
    }

    /// Removes a trusted peer.
    pub async fn remove_trusted_peer(&self, node_id: &NodeId) -> Result<bool> {
        let mut trusted = self.trusted_peers.write().await;
        let removed = trusted.remove(node_id).is_some();
        if removed {
            info!("Peer removed from the trusted list: {}", node_id);
        }
        Ok(removed)
    }

    /// Synchronizes a specific key with peers.
    pub async fn sync_key(&self, key_id: &str, operation: SyncOperation) -> Result<()> {
        debug!(
            "Starting synchronization of key: {} (operation: {:?})",
            key_id, operation
        );

        // Get the key metadata.
        let metadata = self.get_key_metadata(key_id).await?;

        // Create the synchronization message.
        let message = self.create_sync_message(operation, metadata, None).await?;

        // Add it to the synchronization queue.
        self.enqueue_sync_message(message).await?;

        // Update the status.
        self.update_sync_status(key_id, KeySyncStatus::Synchronizing)
            .await;

        Ok(())
    }

    /// Processes a received synchronization message.
    pub async fn handle_sync_message(&self, message: SyncMessage) -> Result<()> {
        // Check the message age (prevent replay attacks).
        if self.is_message_too_old(&message)? {
            warn!(
                "Synchronization message rejected (too old): {:?}",
                message.message_id
            );
            return Err(GuardianError::Other("Message too old".to_string()));
        }

        // Check whether we have already processed this message.
        if self.is_message_duplicate(&message).await? {
            debug!("Duplicate message ignored: {:?}", message.message_id);
            return Ok(());
        }

        // Verify the message signature.
        self.verify_message_signature(&message).await?;

        // Process the operation.
        match message.operation {
            SyncOperation::Create => self.handle_key_create(&message).await?,
            SyncOperation::Update => self.handle_key_update(&message).await?,
            SyncOperation::Delete => self.handle_key_delete(&message).await?,
            SyncOperation::MetadataSync => self.handle_metadata_sync(&message).await?,
        }

        // Add it to the cache of processed messages.
        self.cache_processed_message(&message).await;

        // Update the statistics.
        self.update_statistics().await;

        Ok(())
    }

    /// Gets the metadata of a key.
    async fn get_key_metadata(&self, key_id: &str) -> Result<KeyMetadata> {
        let synchronized_keys = self.synchronized_keys.read().await;

        if let Some(metadata) = synchronized_keys.get(key_id) {
            return Ok(metadata.clone());
        }

        // Key not found, create initial metadata.
        let key_data = self
            .local_keystore
            .get(key_id)
            .await?
            .ok_or_else(|| GuardianError::Other(format!("Key not found: {}", key_id)))?;

        let public_key_hash = blake3::hash(&key_data).as_bytes().to_vec();

        let metadata = KeyMetadata {
            key_id: key_id.to_string(),
            version: 1,
            last_modified: Utc::now(),
            creator: self.node_id,
            signature: Vec::new(), // Will be filled in later.
            crypto_algorithm: "Ed25519".to_string(),
            public_key_hash,
        };

        Ok(metadata)
    }

    /// Creates a synchronization message.
    async fn create_sync_message(
        &self,
        operation: SyncOperation,
        metadata: KeyMetadata,
        key_data: Option<Vec<u8>>,
    ) -> Result<SyncMessage> {
        let message = SyncMessage {
            message_id: Uuid::new_v4(),
            protocol_version: SYNC_PROTOCOL_VERSION,
            timestamp: SystemTime::now(),
            sender: self.node_id,
            operation,
            metadata,
            key_data,
            message_signature: Vec::new(), // Will be filled in later.
        };

        // Sign the message.
        let signed_message = self.sign_sync_message(message).await?;

        Ok(signed_message)
    }

    /// Signs a synchronization message.
    async fn sign_sync_message(&self, mut message: SyncMessage) -> Result<SyncMessage> {
        // Serialize the message without the signature.
        let mut message_copy = message.clone();
        message_copy.message_signature.clear();

        let message_bytes = postcard::to_allocvec(&message_copy)
            .map_err(|e| GuardianError::Other(format!("Error serializing message: {}", e)))?;

        // Sign with the node's keypair.
        let signature = self
            .node_signing_key
            .sign(&message_bytes)
            .to_bytes()
            .to_vec();

        message.message_signature = signature;
        Ok(message)
    }

    /// Verifies a message signature.
    async fn verify_message_signature(&self, message: &SyncMessage) -> Result<()> {
        // Get the sender's public key.
        let trusted_peers = self.trusted_peers.read().await;
        let verifying_key = trusted_peers
            .get(&message.sender)
            .ok_or_else(|| GuardianError::Other(format!("Untrusted peer: {}", message.sender)))?;

        // Reconstruct the message without the signature.
        let mut message_copy = message.clone();
        message_copy.message_signature.clear();

        let message_bytes = postcard::to_allocvec(&message_copy)
            .map_err(|e| GuardianError::Other(format!("Error serializing message: {}", e)))?;

        // Verify the signature.
        let signature = Signature::from_slice(&message.message_signature)
            .map_err(|e| GuardianError::Other(format!("Invalid signature: {}", e)))?;

        verifying_key
            .verify(&message_bytes, &signature)
            .map_err(|e| GuardianError::Other(format!("Signature verification failed: {}", e)))?;

        Ok(())
    }

    /// Checks whether a message is too old.
    fn is_message_too_old(&self, message: &SyncMessage) -> Result<bool> {
        let now = SystemTime::now();
        let age = now
            .duration_since(message.timestamp)
            .map_err(|_| GuardianError::Other("Invalid timestamp".to_string()))?;

        Ok(age > MAX_MESSAGE_AGE)
    }

    /// Checks whether a message is a duplicate.
    async fn is_message_duplicate(&self, message: &SyncMessage) -> Result<bool> {
        let cache = self.message_cache.read().await;
        Ok(cache.contains_key(&message.message_id))
    }

    /// Adds a message to the synchronization queue.
    async fn enqueue_sync_message(&self, message: SyncMessage) -> Result<()> {
        let mut queue = self.sync_queue.lock().await;

        // Check that the queue is not full.
        if queue.len() >= MAX_SYNC_QUEUE_SIZE {
            // Remove the oldest message.
            queue.pop_front();
            warn!("Synchronization queue full, removing the oldest message");
        }

        let entry = SyncQueueEntry {
            message,
            retry_count: 0,
            next_retry: SystemTime::now(),
            target_peers: Vec::new(), // Will be filled based on connected peers.
        };

        queue.push_back(entry);
        debug!("Message added to the synchronization queue");

        Ok(())
    }

    /// Processes a key creation.
    async fn handle_key_create(&self, message: &SyncMessage) -> Result<()> {
        let key_id = &message.metadata.key_id;

        // Check whether the key already exists.
        if self.local_keystore.has(key_id).await? {
            // Check the versions to detect conflicts.
            let local_metadata = self.get_key_metadata(key_id).await?;
            if local_metadata.version >= message.metadata.version {
                debug!(
                    "Key already exists with an equal or higher version: {}",
                    key_id
                );
                return Ok(());
            }
        }

        // Create/update the key.
        if let Some(key_data) = &message.key_data {
            self.local_keystore.put(key_id, key_data).await?;
        }

        // Update the metadata.
        let mut synchronized_keys = self.synchronized_keys.write().await;
        synchronized_keys.insert(key_id.clone(), message.metadata.clone());

        self.update_sync_status(key_id, KeySyncStatus::Synchronized)
            .await;

        info!("Key created via synchronization: {}", key_id);
        Ok(())
    }

    /// Processes a key update.
    async fn handle_key_update(&self, message: &SyncMessage) -> Result<()> {
        let key_id = &message.metadata.key_id;

        // Check whether the key exists.
        if !self.local_keystore.has(key_id).await? {
            warn!("Attempt to update a non-existent key: {}", key_id);
            return Err(GuardianError::Other(format!("Key not found: {}", key_id)));
        }

        // Check the version to detect conflicts.
        let local_metadata = self.get_key_metadata(key_id).await?;
        if local_metadata.version > message.metadata.version {
            warn!("Version conflict detected for key: {}", key_id);
            self.update_sync_status(
                key_id,
                KeySyncStatus::Conflict(format!(
                    "Local: v{}, Remote: v{}",
                    local_metadata.version, message.metadata.version
                )),
            )
            .await;
            return Ok(());
        }

        // Update the key.
        if let Some(key_data) = &message.key_data {
            self.local_keystore.put(key_id, key_data).await?;
        }

        // Update the metadata.
        let mut synchronized_keys = self.synchronized_keys.write().await;
        synchronized_keys.insert(key_id.clone(), message.metadata.clone());

        self.update_sync_status(key_id, KeySyncStatus::Synchronized)
            .await;

        info!("Key updated via synchronization: {}", key_id);
        Ok(())
    }

    /// Processes a key deletion.
    async fn handle_key_delete(&self, message: &SyncMessage) -> Result<()> {
        let key_id = &message.metadata.key_id;

        // Delete the local key.
        self.local_keystore.delete(key_id).await?;

        // Remove the metadata.
        let mut synchronized_keys = self.synchronized_keys.write().await;
        synchronized_keys.remove(key_id);

        let mut sync_status = self.sync_status.write().await;
        sync_status.remove(key_id);

        info!("Key deleted via synchronization: {}", key_id);
        Ok(())
    }

    /// Processes a metadata synchronization.
    async fn handle_metadata_sync(&self, message: &SyncMessage) -> Result<()> {
        let key_id = &message.metadata.key_id;

        // Update only the metadata (not the key data).
        let mut synchronized_keys = self.synchronized_keys.write().await;
        synchronized_keys.insert(key_id.clone(), message.metadata.clone());

        debug!("Metadata synchronized for key: {}", key_id);
        Ok(())
    }

    /// Adds a processed message to the cache.
    async fn cache_processed_message(&self, message: &SyncMessage) {
        let mut cache = self.message_cache.write().await;
        cache.insert(message.message_id, SystemTime::now());

        // Clear old messages from the cache.
        let cutoff = SystemTime::now() - MAX_MESSAGE_AGE;
        cache.retain(|_, timestamp| *timestamp > cutoff);
    }

    /// Updates the synchronization status of a key.
    async fn update_sync_status(&self, key_id: &str, status: KeySyncStatus) {
        let mut sync_status = self.sync_status.write().await;
        sync_status.insert(key_id.to_string(), status);
    }

    /// Updates the synchronization statistics.
    async fn update_statistics(&self) {
        let mut stats = self.statistics.write().await;
        stats.messages_synced += 1;

        let queue = self.sync_queue.lock().await;
        stats.pending_messages = queue.len() as u64;

        let trusted_peers = self.trusted_peers.read().await;
        stats.active_peers = trusted_peers.len() as u32;

        // Compute the success rate.
        let sync_status = self.sync_status.read().await;
        let total_keys = sync_status.len() as u64;
        let synchronized_keys = sync_status
            .values()
            .filter(|status| matches!(status, KeySyncStatus::Synchronized))
            .count() as u64;

        stats.success_rate = if total_keys > 0 {
            (synchronized_keys as f64 / total_keys as f64) * 100.0
        } else {
            100.0
        };
    }

    /// Returns the synchronization statistics.
    pub async fn get_statistics(&self) -> SyncStatistics {
        self.statistics.read().await.clone()
    }

    /// Returns the synchronization status of a key.
    pub async fn get_key_sync_status(&self, key_id: &str) -> Option<KeySyncStatus> {
        let sync_status = self.sync_status.read().await;
        sync_status.get(key_id).cloned()
    }

    /// Lists all synchronized keys.
    pub async fn list_synchronized_keys(&self) -> Vec<String> {
        let synchronized_keys = self.synchronized_keys.read().await;
        synchronized_keys.keys().cloned().collect()
    }

    /// Lists all trusted peers.
    pub async fn list_trusted_peers(&self) -> Vec<NodeId> {
        let trusted = self.trusted_peers.read().await;
        trusted.keys().copied().collect()
    }

    /// Forces a full synchronization with peers.
    pub async fn force_full_sync(&self) -> Result<()> {
        info!("Starting forced full synchronization");

        let keys = self.local_keystore.list_keys().await?;
        for key_id in keys {
            self.sync_key(&key_id, SyncOperation::MetadataSync).await?;
        }

        info!(
            "Forced full synchronization started for {} keys",
            self.synchronized_keys.read().await.len()
        );
        Ok(())
    }

    /// Exports the synchronization configuration.
    pub async fn export_sync_config(&self) -> Result<Vec<u8>> {
        let sync_config = SyncExportConfig {
            node_id: self.node_id,
            trusted_peers: self.trusted_peers.read().await.clone(),
            synchronized_keys: self.synchronized_keys.read().await.clone(),
            statistics: self.statistics.read().await.clone(),
        };

        postcard::to_allocvec(&sync_config)
            .map_err(|e| GuardianError::Other(format!("Error exporting configuration: {}", e)))
    }
}

/// Configuration for export.
#[derive(Debug, Serialize, Deserialize)]
struct SyncExportConfig {
    node_id: NodeId,
    trusted_peers: HashMap<NodeId, VerifyingKey>,
    synchronized_keys: HashMap<String, KeyMetadata>,
    statistics: SyncStatistics,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_key_synchronizer_creation() {
        let temp_dir = TempDir::new().unwrap();
        let client_config = ClientConfig {
            data_store_path: Some(temp_dir.path().to_path_buf()),
            ..Default::default()
        };

        let synchronizer = KeySynchronizer::new(&client_config).await.unwrap();
        assert!(!synchronizer.node_id().to_string().is_empty());
    }

    #[tokio::test]
    async fn test_sync_message_creation_and_verification() {
        let temp_dir = TempDir::new().unwrap();
        let client_config = ClientConfig {
            data_store_path: Some(temp_dir.path().to_path_buf()),
            ..Default::default()
        };

        let synchronizer = KeySynchronizer::new(&client_config).await.unwrap();

        // Create test metadata.
        let metadata = KeyMetadata {
            key_id: "test_key".to_string(),
            version: 1,
            last_modified: Utc::now(),
            creator: synchronizer.node_id(),
            signature: Vec::new(),
            crypto_algorithm: "Ed25519".to_string(),
            public_key_hash: vec![1, 2, 3, 4],
        };

        // Create the message.
        let message = synchronizer
            .create_sync_message(SyncOperation::Create, metadata, Some(b"test_data".to_vec()))
            .await
            .unwrap();

        // Check the message structure.
        assert_eq!(message.protocol_version, SYNC_PROTOCOL_VERSION);
        assert_eq!(message.sender, synchronizer.node_id());
        assert_eq!(message.operation, SyncOperation::Create);
        assert!(!message.message_signature.is_empty());
    }

    fn config_at(temp: &TempDir) -> ClientConfig {
        ClientConfig {
            data_store_path: Some(temp.path().to_path_buf()),
            ..Default::default()
        }
    }

    // ─── Keypair persistence ─────────────────────────────────────────────────

    #[tokio::test]
    async fn keypair_persists_across_reload() {
        let temp_dir = TempDir::new().unwrap();
        let config = config_at(&temp_dir);

        let first = KeySynchronizer::new(&config).await.unwrap();
        let id_first = first.node_id();
        drop(first);

        // Reopening with the SAME data_store_path should load the same keypair.
        let second = KeySynchronizer::new(&config).await.unwrap();
        assert_eq!(
            id_first,
            second.node_id(),
            "the keypair should be persisted and reloaded"
        );
    }

    #[tokio::test]
    async fn distinct_data_paths_yield_distinct_keys() {
        let a = TempDir::new().unwrap();
        let b = TempDir::new().unwrap();
        let sync_a = KeySynchronizer::new(&config_at(&a)).await.unwrap();
        let sync_b = KeySynchronizer::new(&config_at(&b)).await.unwrap();
        assert_ne!(sync_a.node_id(), sync_b.node_id());
    }

    // ─── Trusted peers ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn add_list_remove_trusted_peer() {
        let temp_dir = TempDir::new().unwrap();
        let sync = KeySynchronizer::new(&config_at(&temp_dir)).await.unwrap();

        let peer = iroh::SecretKey::generate().public();
        let vkey = sync.signing_key().verifying_key();

        assert!(sync.list_trusted_peers().await.is_empty());

        sync.add_trusted_peer(peer, vkey).await.unwrap();
        let peers = sync.list_trusted_peers().await;
        assert_eq!(peers.len(), 1);
        assert!(peers.contains(&peer));

        // Removal returns true and empties the list.
        assert!(sync.remove_trusted_peer(&peer).await.unwrap());
        assert!(sync.list_trusted_peers().await.is_empty());
    }

    #[tokio::test]
    async fn remove_absent_trusted_peer_returns_false() {
        let temp_dir = TempDir::new().unwrap();
        let sync = KeySynchronizer::new(&config_at(&temp_dir)).await.unwrap();
        let absent = iroh::SecretKey::generate().public();
        assert!(!sync.remove_trusted_peer(&absent).await.unwrap());
    }

    #[tokio::test]
    async fn add_trusted_peer_is_idempotent_on_node_id() {
        let temp_dir = TempDir::new().unwrap();
        let sync = KeySynchronizer::new(&config_at(&temp_dir)).await.unwrap();
        let peer = iroh::SecretKey::generate().public();
        let vkey = sync.signing_key().verifying_key();

        sync.add_trusted_peer(peer, vkey).await.unwrap();
        sync.add_trusted_peer(peer, vkey).await.unwrap(); // re-add the same key
        assert_eq!(sync.list_trusted_peers().await.len(), 1);
    }

    // ─── State / export ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn fresh_synchronizer_has_empty_state() {
        let temp_dir = TempDir::new().unwrap();
        let sync = KeySynchronizer::new(&config_at(&temp_dir)).await.unwrap();
        assert!(sync.list_synchronized_keys().await.is_empty());
        assert!(sync.list_trusted_peers().await.is_empty());
    }

    #[tokio::test]
    async fn export_sync_config_produces_decodable_bytes() {
        let temp_dir = TempDir::new().unwrap();
        let sync = KeySynchronizer::new(&config_at(&temp_dir)).await.unwrap();
        let peer = iroh::SecretKey::generate().public();
        sync.add_trusted_peer(peer, sync.signing_key().verifying_key())
            .await
            .unwrap();

        let bytes = sync.export_sync_config().await.unwrap();
        assert!(!bytes.is_empty());
        // Should be deserializable back into SyncExportConfig (postcard).
        let decoded: SyncExportConfig = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.node_id, sync.node_id());
        assert!(decoded.trusted_peers.contains_key(&peer));
    }
}
