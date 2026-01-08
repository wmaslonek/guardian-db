    // ╔════════════════════════════════════════════════════════════════════════════════╗
    // ║                          ⚠ OUTDATED DOCUMENTATION                             ║
    // ╚════════════════════════════════════════════════════════════════════════════════╝


# Key Synchronizer - Cryptographic Key Synchronization Documentation

## Overview

The Key Synchronizer provides robust cryptographic key synchronization between peers in Guardian DB. It ensures cryptographic consistency, prevents replay attacks, handles version conflicts, and maintains a trusted peer network with signature-based message verification.

## Table of Contents

- [Architecture](#architecture)
- [Core Components](#core-components)
- [Security Features](#security-features)
- [Synchronization Protocol](#synchronization-protocol)
- [Message Format](#message-format)
- [API Reference](#api-reference)
- [Conflict Resolution](#conflict-resolution)
- [Statistics and Monitoring](#statistics-and-monitoring)
- [Usage Examples](#usage-examples)
- [Best Practices](#best-practices)
- [Security Considerations](#security-considerations)

## Architecture

### High-Level Architecture

```
┌──────────────────────────────────────────────────────────┐
│                 KeySynchronizer                          │
├──────────────────────────────────────────────────────────┤
│  ┌────────────────────────────────────────────────┐     │
│  │         Local Keystore (SledKeystore)          │     │
│  │         - Main node keypair                    │     │
│  │         - Synchronized keys                    │     │
│  └────────────────────────────────────────────────┘     │
│  ┌────────────────────────────────────────────────┐     │
│  │    Synchronized Keys (HashMap)                 │     │
│  │    key_id -> KeyMetadata                       │     │
│  └────────────────────────────────────────────────┘     │
│  ┌────────────────────────────────────────────────┐     │
│  │    Sync Status (HashMap)                       │     │
│  │    key_id -> KeySyncStatus                     │     │
│  └────────────────────────────────────────────────┘     │
│  ┌────────────────────────────────────────────────┐     │
│  │    Trusted Peers (HashMap)                     │     │
│  │    node_id -> VerifyingKey                     │     │
│  └────────────────────────────────────────────────┘     │
│  ┌────────────────────────────────────────────────┐     │
│  │    Message Cache (HashMap)                     │     │
│  │    message_id -> timestamp (Replay Prevention) │     │
│  └────────────────────────────────────────────────┘     │
│  ┌────────────────────────────────────────────────┐     │
│  │    Sync Queue (VecDeque)                       │     │
│  │    Pending synchronization messages            │     │
│  └────────────────────────────────────────────────┘     │
└──────────────────────────────────────────────────────────┘
```

### Synchronization Flow

```
Local Key Change
    ↓
sync_key(key_id, operation)
    ↓
Get Key Metadata
    ↓
Create Sync Message
    ├─ Generate message_id (UUID)
    ├─ Add timestamp
    ├─ Include metadata
    ├─ Add key data (if needed)
    └─ Sign message
    ↓
Enqueue Sync Message
    ↓
[Distributed to Peers]
    ↓
Peer Receives Message
    ↓
handle_sync_message()
    ├─ Check message age (< 5 min)
    ├─ Check for duplicates
    ├─ Verify signature
    ├─ Process operation
    └─ Update local keystore
    ↓
Update Statistics
```

## Core Components

### 1. KeySynchronizer

Main synchronization structure:

```rust
pub struct KeySynchronizer {
    /// Local configuration
    config: ClientConfig,
    
    /// Local keystore (SledKeystore)
    local_keystore: Arc<SledKeystore>,
    
    /// Node's main keypair
    node_keypair: LibP2PKeypair,
    
    /// Node's PeerID
    node_id: PeerId,
    
    /// Synchronized keys metadata
    synchronized_keys: Arc<RwLock<HashMap<String, KeyMetadata>>>,
    
    /// Sync status per key
    sync_status: Arc<RwLock<HashMap<String, KeySyncStatus>>>,
    
    /// Synchronization queue
    sync_queue: Arc<Mutex<VecDeque<SyncQueueEntry>>>,
    
    /// Message cache (prevents replay attacks)
    message_cache: Arc<RwLock<HashMap<Uuid, SystemTime>>>,
    
    /// Synchronization statistics
    statistics: Arc<RwLock<SyncStatistics>>,
    
    /// Trusted peers (authorized for sync)
    trusted_peers: Arc<RwLock<HashMap<PeerId, VerifyingKey>>>,
}
```

### 2. KeyMetadata

Metadata for synchronized keys:

```rust
pub struct KeyMetadata {
    /// Unique key ID
    pub key_id: String,
    
    /// Key version (conflict control)
    pub version: u64,
    
    /// Last modified timestamp
    pub last_modified: DateTime<Utc>,
    
    /// Creator PeerID
    pub creator: PeerId,
    
    /// Metadata signature
    pub signature: Vec<u8>,
    
    /// Cryptographic algorithm
    pub crypto_algorithm: String,
    
    /// Public key hash (Blake3)
    pub public_key_hash: Vec<u8>,
}
```

### 3. SyncMessage

Synchronization message structure:

```rust
pub struct SyncMessage {
    /// Unique message ID (UUID)
    pub message_id: Uuid,
    
    /// Protocol version
    pub protocol_version: u32,
    
    /// Message timestamp
    pub timestamp: SystemTime,
    
    /// Sender PeerID
    pub sender: PeerId,
    
    /// Sync operation type
    pub operation: SyncOperation,
    
    /// Key metadata
    pub metadata: KeyMetadata,
    
    /// Key data (encrypted)
    pub key_data: Option<Vec<u8>>,
    
    /// Message signature (Ed25519)
    pub message_signature: Vec<u8>,
}
```

## Security Features

### 1. Replay Attack Prevention

Messages are validated by age:

```rust
const MAX_MESSAGE_AGE: Duration = Duration::from_secs(300);  // 5 minutes

fn is_message_too_old(&self, message: &SyncMessage) -> Result<bool> {
    let now = SystemTime::now();
    let age = now.duration_since(message.timestamp)?;
    
    Ok(age > MAX_MESSAGE_AGE)
}
```

**Protection:**
- Rejects messages older than 5 minutes
- Prevents replay of old messages
- Requires synchronized clocks (±5 min tolerance)

### 2. Duplicate Detection

Message cache prevents processing duplicates:

```rust
async fn is_message_duplicate(&self, message: &SyncMessage) -> Result<bool> {
    let cache = self.message_cache.read().await;
    Ok(cache.contains_key(&message.message_id))
}

async fn cache_processed_message(&self, message: &SyncMessage) {
    let mut cache = self.message_cache.write().await;
    cache.insert(message.message_id, SystemTime::now());
    
    // Clean old messages
    let cutoff = SystemTime::now() - MAX_MESSAGE_AGE;
    cache.retain(|_, timestamp| *timestamp > cutoff);
}
```

**Features:**
- UUID-based message identification
- Automatic cache cleanup (5-minute TTL)
- Fast duplicate detection (HashMap lookup)

### 3. Message Signing

All messages are signed with Ed25519:

```rust
async fn sign_sync_message(&self, mut message: SyncMessage) -> Result<SyncMessage> {
    // Serialize message without signature
    let mut message_copy = message.clone();
    message_copy.message_signature.clear();
    
    let message_bytes = bincode::serde::encode_to_vec(
        &message_copy,
        bincode::config::standard()
    )?;
    
    // Sign with node keypair
    if let Ok(ed25519_keypair) = self.node_keypair.clone().try_into_ed25519() {
        let secret_bytes = ed25519_keypair.secret().as_ref().to_vec();
        let signing_key = SigningKey::try_from(&secret_bytes[..32])?;
        let signature = signing_key.sign(&message_bytes);
        
        message.message_signature = signature.to_bytes().to_vec();
    }
    
    Ok(message)
}
```

### 4. Signature Verification

Only trusted peers' messages are accepted:

```rust
async fn verify_message_signature(&self, message: &SyncMessage) -> Result<()> {
    // Get sender's public key from trusted peers
    let trusted_peers = self.trusted_peers.read().await;
    let verifying_key = trusted_peers.get(&message.sender)
        .ok_or_else(|| GuardianError::Other(
            format!("Untrusted peer: {}", message.sender)
        ))?;
    
    // Reconstruct message without signature
    let mut message_copy = message.clone();
    message_copy.message_signature.clear();
    
    let message_bytes = bincode::serde::encode_to_vec(
        &message_copy,
        bincode::config::standard()
    )?;
    
    // Verify signature
    let signature = Signature::from_slice(&message.message_signature)?;
    
    verifying_key.verify(&message_bytes, &signature)
        .map_err(|e| GuardianError::Other(
            format!("Signature verification failed: {}", e)
        ))?;
    
    Ok(())
}
```

**Security Guarantees:**
- Only messages from trusted peers are processed
- Ed25519 cryptographic signature verification
- Prevents message tampering
- Ensures message authenticity

## Synchronization Protocol

### Operation Types

```rust
pub enum SyncOperation {
    /// Create new key
    Create,
    
    /// Update existing key
    Update,
    
    /// Delete key
    Delete,
    
    /// Synchronize metadata only
    MetadataSync,
}
```

### Sync Status

```rust
pub enum KeySyncStatus {
    /// Key is synchronized
    Synchronized,
    
    /// Sync in progress
    Synchronizing,
    
    /// Sync pending
    Pending,
    
    /// Sync failed
    Failed(String),
    
    /// Conflict detected (requires manual resolution)
    Conflict(String),
}
```

## Message Format

### Protocol Constants

```rust
/// Sync protocol version
const SYNC_PROTOCOL_VERSION: u32 = 1;

/// Maximum message age (prevents replay)
const MAX_MESSAGE_AGE: Duration = Duration::from_secs(300);

/// Maximum sync queue size
const MAX_SYNC_QUEUE_SIZE: usize = 1000;
```

### Message Structure

```
┌───────────────────────────────────────┐
│         SyncMessage                   │
├───────────────────────────────────────┤
│ message_id: UUID                      │
│ protocol_version: u32                 │
│ timestamp: SystemTime                 │
│ sender: PeerId                        │
│ operation: SyncOperation              │
│ metadata: KeyMetadata                 │
│   ├─ key_id: String                   │
│   ├─ version: u64                     │
│   ├─ last_modified: DateTime          │
│   ├─ creator: PeerId                  │
│   ├─ signature: Vec<u8>               │
│   ├─ crypto_algorithm: String         │
│   └─ public_key_hash: Vec<u8>         │
│ key_data: Option<Vec<u8>>             │
│ message_signature: Vec<u8> (Ed25519)  │
└───────────────────────────────────────┘
```

## API Reference

### Creating a Key Synchronizer

```rust
pub async fn new(config: &ClientConfig) -> Result<Self>
```

**Parameters:**
- `config`: Client configuration with data store path

**Returns:**
- `KeySynchronizer` instance

**Example:**
```rust
let config = ClientConfig {
    data_store_path: Some(PathBuf::from("/data/guardian")),
    ..Default::default()
};

let synchronizer = KeySynchronizer::new(&config).await?;
```

### Getting PeerID

```rust
pub fn node_id(&self) -> PeerId
```

**Returns:**
- Node's PeerID

### Getting Keypair

```rust
pub fn keypair(&self) -> &LibP2PKeypair
```

**Returns:**
- Reference to node's keypair

### Managing Trusted Peers

#### Add Trusted Peer

```rust
pub async fn add_trusted_peer(&self, node_id: PeerId, public_key: VerifyingKey) -> Result<()>
```

**Parameters:**
- `node_id`: Peer to trust
- `public_key`: Peer's Ed25519 public key

**Example:**
```rust
let node_id: PeerId = "12D3KooW...".parse()?;
let public_key: VerifyingKey = /* get from peer */;

synchronizer.add_trusted_peer(node_id, public_key).await?;
```

#### Remove Trusted Peer

```rust
pub async fn remove_trusted_peer(&self, node_id: &PeerId) -> Result<bool>
```

**Returns:**
- `bool`: true if peer was removed

### Key Synchronization

#### Sync a Key

```rust
pub async fn sync_key(&self, key_id: &str, operation: SyncOperation) -> Result<()>
```

**Parameters:**
- `key_id`: Key identifier
- `operation`: Type of sync operation

**Example:**
```rust
// Sync new key
synchronizer.sync_key("my_key", SyncOperation::Create).await?;

// Update existing key
synchronizer.sync_key("my_key", SyncOperation::Update).await?;

// Delete key
synchronizer.sync_key("my_key", SyncOperation::Delete).await?;
```

#### Handle Sync Message

```rust
pub async fn handle_sync_message(&self, message: SyncMessage) -> Result<()>
```

**Parameters:**
- `message`: Synchronization message from peer

**Process:**
1. Check message age (< 5 minutes)
2. Check for duplicates
3. Verify signature
4. Process operation
5. Update local keystore
6. Cache message
7. Update statistics

### Sync Status

#### Get Key Sync Status

```rust
pub async fn get_key_sync_status(&self, key_id: &str) -> Option<KeySyncStatus>
```

**Returns:**
- `Option<KeySyncStatus>`: Current sync status

**Example:**
```rust
let status = synchronizer.get_key_sync_status("my_key").await;
match status {
    Some(KeySyncStatus::Synchronized) => println!("Key is synced"),
    Some(KeySyncStatus::Conflict(reason)) => println!("Conflict: {}", reason),
    Some(KeySyncStatus::Failed(error)) => println!("Failed: {}", error),
    _ => println!("Unknown status"),
}
```

#### List Synchronized Keys

```rust
pub async fn list_synchronized_keys(&self) -> Vec<String>
```

**Returns:**
- `Vec<String>`: List of synchronized key IDs

### Full Synchronization

#### Force Full Sync

```rust
pub async fn force_full_sync(&self) -> Result<()>
```

**Description:** Forces complete synchronization of all keys with peers.

**Example:**
```rust
// Sync all local keys
synchronizer.force_full_sync().await?;
```

### Statistics

#### Get Statistics

```rust
pub async fn get_statistics(&self) -> SyncStatistics
```

**Returns:**
- `SyncStatistics`: Current sync statistics

### Export/Import

#### Export Sync Config

```rust
pub async fn export_sync_config(&self) -> Result<Vec<u8>>
```

**Returns:**
- `Vec<u8>`: Serialized sync configuration

**Example:**
```rust
let config_bytes = synchronizer.export_sync_config().await?;
std::fs::write("sync_config.bin", config_bytes)?;
```

## Conflict Resolution

### Version-Based Conflict Detection

```rust
async fn handle_key_update(&self, message: &SyncMessage) -> Result<()> {
    let key_id = &message.metadata.key_id;
    
    // Get local metadata
    let local_metadata = self.get_key_metadata(key_id).await?;
    
    // Check version
    if local_metadata.version > message.metadata.version {
        // Conflict: local version is newer
        self.update_sync_status(
            key_id,
            KeySyncStatus::Conflict(format!(
                "Local: v{}, Remote: v{}",
                local_metadata.version,
                message.metadata.version
            ))
        ).await;
        
        // Update conflict statistics
        let mut stats = self.statistics.write().await;
        stats.conflicts_detected += 1;
        
        return Ok(());
    }
    
    // No conflict: apply update
    // ...
}
```

**Conflict Resolution Strategy:**
- **Higher version wins**: Newer version takes precedence
- **Manual resolution**: Conflicts are flagged for review
- **Status tracking**: Conflicts marked in sync_status
- **Statistics**: Conflict count tracked

### Conflict Types

1. **Version Conflict**: Local version > remote version
2. **Concurrent Update**: Same version, different data
3. **Delete vs Update**: Deletion conflicts with update

## Statistics and Monitoring

### SyncStatistics

```rust
pub struct SyncStatistics {
    /// Total messages synced
    pub messages_synced: u64,
    
    /// Pending messages in queue
    pub pending_messages: u64,
    
    /// Conflicts detected
    pub conflicts_detected: u64,
    
    /// Conflicts resolved
    pub conflicts_resolved: u64,
    
    /// Sync success rate (%)
    pub success_rate: f64,
    
    /// Average sync latency (ms)
    pub avg_sync_latency_ms: f64,
    
    /// Active peers in sync
    pub active_peers: u32,
}
```

### Statistics Calculation

```rust
async fn update_statistics(&self) {
    let mut stats = self.statistics.write().await;
    stats.messages_synced += 1;
    
    // Update pending count
    let queue = self.sync_queue.lock().await;
    stats.pending_messages = queue.len() as u64;
    
    // Update active peers
    let trusted_peers = self.trusted_peers.read().await;
    stats.active_peers = trusted_peers.len() as u32;
    
    // Calculate success rate
    let sync_status = self.sync_status.read().await;
    let total_keys = sync_status.len() as u64;
    let synchronized_keys = sync_status.values()
        .filter(|status| matches!(status, KeySyncStatus::Synchronized))
        .count() as u64;
    
    stats.success_rate = if total_keys > 0 {
        (synchronized_keys as f64 / total_keys as f64) * 100.0
    } else {
        100.0
    };
}
```

## Usage Examples

### Basic Setup

```rust
use guardian_db::ipfs_core_api::backends::key_synchronizer::*;
use guardian_db::ipfs_core_api::config::ClientConfig;

// Create configuration
let config = ClientConfig {
    data_store_path: Some(PathBuf::from("/data/guardian")),
    ..Default::default()
};

// Initialize synchronizer
let synchronizer = KeySynchronizer::new(&config).await?;
println!("PeerID: {}", synchronizer.node_id());
```

### Adding Trusted Peers

```rust
// Add trusted peer
let node_id: PeerId = "12D3KooW...".parse()?;
let public_key = get_peer_public_key(node_id).await?;

synchronizer.add_trusted_peer(node_id, public_key).await?;

// Verify trusted
let trusted = synchronizer.trusted_peers.read().await;
assert!(trusted.contains_key(&node_id));
```

### Synchronizing Keys

```rust
// Create new key locally
let key_id = "user_signing_key";
let keypair = LibP2PKeypair::generate_ed25519();
synchronizer.local_keystore.put_keypair(key_id, &keypair).await?;

// Sync with peers
synchronizer.sync_key(key_id, SyncOperation::Create).await?;

// Check sync status
let status = synchronizer.get_key_sync_status(key_id).await;
match status {
    Some(KeySyncStatus::Synchronizing) => println!("Sync in progress"),
    Some(KeySyncStatus::Synchronized) => println!("Sync complete"),
    _ => println!("Sync pending"),
}
```

### Receiving Sync Messages

```rust
// When message received from peer
let sync_message: SyncMessage = receive_from_network().await?;

// Handle the message
match synchronizer.handle_sync_message(sync_message).await {
    Ok(()) => println!("Message processed successfully"),
    Err(e) => eprintln!("Failed to process message: {}", e),
}
```

### Monitoring Statistics

```rust
// Get statistics
let stats = synchronizer.get_statistics().await;

println!("Sync Statistics:");
println!("  Messages synced: {}", stats.messages_synced);
println!("  Pending: {}", stats.pending_messages);
println!("  Success rate: {:.1}%", stats.success_rate);
println!("  Conflicts: {}", stats.conflicts_detected);
println!("  Active peers: {}", stats.active_peers);
```

### Full Synchronization

```rust
// Perform full sync of all keys
synchronizer.force_full_sync().await?;

// This will sync all keys in local keystore
let synced_keys = synchronizer.list_synchronized_keys().await;
println!("Synchronized {} keys", synced_keys.len());
```

### Export/Import Configuration

```rust
// Export sync configuration
let config_bytes = synchronizer.export_sync_config().await?;
std::fs::write("sync_backup.bin", config_bytes)?;

// Can be used for:
// - Backup
// - Migration
// - Debugging
```

## Message Processing

### Create Operation

```rust
async fn handle_key_create(&self, message: &SyncMessage) -> Result<()> {
    let key_id = &message.metadata.key_id;
    
    // Check if key already exists
    if self.local_keystore.has(key_id).await? {
        // Check versions for conflicts
        let local_metadata = self.get_key_metadata(key_id).await?;
        if local_metadata.version >= message.metadata.version {
            return Ok(());  // Local version is same or newer
        }
    }
    
    // Create/update key
    if let Some(key_data) = &message.key_data {
        self.local_keystore.put(key_id, key_data).await?;
    }
    
    // Update metadata
    let mut synchronized_keys = self.synchronized_keys.write().await;
    synchronized_keys.insert(key_id.clone(), message.metadata.clone());
    
    self.update_sync_status(key_id, KeySyncStatus::Synchronized).await;
    
    Ok(())
}
```

### Update Operation

```rust
async fn handle_key_update(&self, message: &SyncMessage) -> Result<()> {
    let key_id = &message.metadata.key_id;
    
    // Verify key exists
    if !self.local_keystore.has(key_id).await? {
        return Err(GuardianError::Other(
            format!("Key not found: {}", key_id)
        ));
    }
    
    // Check version for conflicts
    let local_metadata = self.get_key_metadata(key_id).await?;
    if local_metadata.version > message.metadata.version {
        // Conflict detected
        self.update_sync_status(
            key_id,
            KeySyncStatus::Conflict(format!(
                "Local: v{}, Remote: v{}",
                local_metadata.version,
                message.metadata.version
            ))
        ).await;
        return Ok(());
    }
    
    // Apply update
    if let Some(key_data) = &message.key_data {
        self.local_keystore.put(key_id, key_data).await?;
    }
    
    // Update metadata
    let mut synchronized_keys = self.synchronized_keys.write().await;
    synchronized_keys.insert(key_id.clone(), message.metadata.clone());
    
    self.update_sync_status(key_id, KeySyncStatus::Synchronized).await;
    
    Ok(())
}
```

### Delete Operation

```rust
async fn handle_key_delete(&self, message: &SyncMessage) -> Result<()> {
    let key_id = &message.metadata.key_id;
    
    // Delete local key
    self.local_keystore.delete(key_id).await?;
    
    // Remove metadata
    let mut synchronized_keys = self.synchronized_keys.write().await;
    synchronized_keys.remove(key_id);
    
    let mut sync_status = self.sync_status.write().await;
    sync_status.remove(key_id);
    
    Ok(())
}
```

### Metadata Sync Operation

```rust
async fn handle_metadata_sync(&self, message: &SyncMessage) -> Result<()> {
    let key_id = &message.metadata.key_id;
    
    // Update metadata only (not key data)
    let mut synchronized_keys = self.synchronized_keys.write().await;
    synchronized_keys.insert(key_id.clone(), message.metadata.clone());
    
    Ok(())
}
```

## Keypair Management

### Load or Generate Keypair

```rust
async fn load_or_generate_keypair(keystore: &SledKeystore) -> Result<LibP2PKeypair> {
    const MAIN_KEYPAIR_KEY: &str = "main_node_keypair";
    
    // Try to load existing keypair
    if let Some(keypair) = keystore.get_keypair(MAIN_KEYPAIR_KEY).await? {
        debug!("Loaded existing main keypair");
        return Ok(keypair);
    }
    
    // Generate new keypair
    let keypair = LibP2PKeypair::generate_ed25519();
    keystore.put_keypair(MAIN_KEYPAIR_KEY, &keypair).await?;
    
    info!("Generated and saved new main keypair");
    Ok(keypair)
}
```

**Features:**
- Persistent storage in SledKeystore
- Automatic generation on first run
- Ed25519 algorithm
- Consistent PeerID derivation

## Best Practices

### 1. Verify Before Trusting

```rust
// Always verify peer's public key before adding to trusted list
let peer_pubkey = get_verified_public_key(node_id).await?;

// Only add if verified through secure channel
synchronizer.add_trusted_peer(node_id, peer_pubkey).await?;
```

### 2. Monitor Conflict Rate

```rust
// Regularly check for conflicts
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    
    loop {
        interval.tick().await;
        let stats = synchronizer.get_statistics().await;
        
        if stats.conflicts_detected > 0 {
            warn!("Conflicts detected: {}", stats.conflicts_detected);
            
            // Investigate conflicted keys
            for key_id in synchronizer.list_synchronized_keys().await {
                if let Some(KeySyncStatus::Conflict(reason)) = 
                    synchronizer.get_key_sync_status(&key_id).await {
                    error!("Key {} has conflict: {}", key_id, reason);
                }
            }
        }
    }
});
```

### 3. Handle Sync Failures

```rust
// Check for failed syncs
let keys = synchronizer.list_synchronized_keys().await;
for key_id in keys {
    if let Some(KeySyncStatus::Failed(error)) = 
        synchronizer.get_key_sync_status(&key_id).await {
        warn!("Key {} sync failed: {}", key_id, error);
        
        // Retry sync
        synchronizer.sync_key(&key_id, SyncOperation::Update).await?;
    }
}
```

### 4. Regular Full Syncs

```rust
// Periodic full sync to ensure consistency
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(3600));  // 1 hour
    
    loop {
        interval.tick().await;
        if let Err(e) = synchronizer.force_full_sync().await {
            error!("Full sync failed: {}", e);
        }
    }
});
```

## Security Considerations

### 1. Trusted Peer Management

**Only trust peers that you control or have verified:**

```rust
// ✓ Good: Verified public key
let verified_pubkey = exchange_keys_securely(node_id).await?;
synchronizer.add_trusted_peer(node_id, verified_pubkey).await?;

// ✗ Bad: Trusting unknown peer
synchronizer.add_trusted_peer(unknown_peer, unverified_key).await?;
```

### 2. Message Age Validation

**Always enabled - no configuration needed:**

- Maximum age: 5 minutes
- Automatic rejection of old messages
- Requires reasonably synchronized clocks

### 3. Signature Verification

**Automatic for all messages:**

- Ed25519 signature algorithm
- Full message integrity check
- Prevents tampering
- Ensures authenticity

### 4. Replay Attack Prevention

**Automatic through message cache:**

- UUID-based message identification
- 5-minute cache retention
- Automatic duplicate rejection

## Troubleshooting

### Sync Failures

**Problem:** Keys not synchronizing

```
Solutions:
1. Check if peer is in trusted_peers list
2. Verify network connectivity to peers
3. Check sync_status for specific keys
4. Review statistics for error patterns
5. Check logs for signature verification failures
```

### Version Conflicts

**Problem:** High conflict rate

```
Solutions:
1. Ensure synchronized clocks across peers
2. Implement conflict resolution strategy
3. Use metadata sync to update versions
4. Review key update patterns
5. Consider versioning scheme changes
```

### Message Rejection

**Problem:** Messages being rejected

```
Solutions:
1. Check message timestamps (must be < 5 min old)
2. Verify sender is in trusted_peers
3. Check signature verification
4. Ensure protocol versions match
5. Review message format
```

### Low Success Rate

**Problem:** `success_rate < 80%`

```
Solutions:
1. Check network reliability
2. Review trusted peers configuration
3. Monitor for conflicts
4. Check queue size (may be full)
5. Increase MAX_SYNC_QUEUE_SIZE if needed
```

## Testing

### Unit Tests

```rust
#[tokio::test]
async fn test_key_synchronizer_creation() {
    let temp_dir = TempDir::new("test_sync").unwrap();
    let config = ClientConfig {
        data_store_path: Some(temp_dir.path().to_path_buf()),
        ..Default::default()
    };
    
    let synchronizer = KeySynchronizer::new(&config).await.unwrap();
    assert!(!synchronizer.node_id().to_string().is_empty());
}

#[tokio::test]
async fn test_sync_message_creation_and_verification() {
    let synchronizer = create_test_synchronizer().await;
    
    // Create metadata
    let metadata = KeyMetadata {
        key_id: "test_key".to_string(),
        version: 1,
        last_modified: Utc::now(),
        creator: synchronizer.node_id(),
        signature: Vec::new(),
        crypto_algorithm: "Ed25519".to_string(),
        public_key_hash: vec![1, 2, 3, 4],
    };
    
    // Create message
    let message = synchronizer.create_sync_message(
        SyncOperation::Create,
        metadata,
        Some(b"test_data".to_vec())
    ).await.unwrap();
    
    // Verify structure
    assert_eq!(message.protocol_version, SYNC_PROTOCOL_VERSION);
    assert_eq!(message.sender, synchronizer.node_id());
    assert!(!message.message_signature.is_empty());
}
```

### Integration Tests

```rust
#[tokio::test]
async fn test_key_sync_between_peers() {
    // Create two synchronizers
    let sync1 = create_test_synchronizer().await;
    let sync2 = create_test_synchronizer().await;
    
    // Add each other as trusted peers
    sync1.add_trusted_peer(sync2.node_id(), get_public_key(&sync2)).await.unwrap();
    sync2.add_trusted_peer(sync1.node_id(), get_public_key(&sync1)).await.unwrap();
    
    // Create key on sync1
    let key_id = "shared_key";
    sync1.local_keystore.put(key_id, b"shared_data").await.unwrap();
    
    // Sync to sync2
    sync1.sync_key(key_id, SyncOperation::Create).await.unwrap();
    
    // Simulate network: get message from queue and send to sync2
    let message = get_message_from_queue(&sync1).await;
    sync2.handle_sync_message(message).await.unwrap();
    
    // Verify sync
    assert!(sync2.local_keystore.has(key_id).await.unwrap());
}
```

## Changelog

See the main Guardian DB changelog for version history.

## Future Enhancements

### Planned Features

- Automatic conflict resolution strategies
- Peer reputation system
- Key rotation support
- Batch synchronization
- Compression for key_data
- Encryption for key_data in transit
- Sync priority levels
- Selective sync (filter by key patterns)

## License

This module is part of GuardianDB and follows the same licensing terms.

## Contributing

Contributions are welcome! Please follow the project's contribution guidelines.

## References

- [Iroh Backend Documentation](IROH-BACKEND.md)
- [Hybrid Backend Documentation](HYBRID-BACKEND.md)
- [Ed25519 Signature Scheme](https://ed25519.cr.yp.to/)
- [Replay Attack Prevention](https://en.wikipedia.org/wiki/Replay_attack)
- [Version Vector Conflict Resolution](https://en.wikipedia.org/wiki/Version_vector)
