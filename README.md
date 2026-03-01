<div align="center">
  <img src="docs/logotipo-guardiandb-new-outubro.png" alt="GuardianDB Logo" width="350"/>

[![Discord](https://img.shields.io/discord/1410233136846995519?label=chat&logo=discord&logoColor=white&style=flat-square&color=7289DA)](https://discord.gg/https://discord.gg/Ezzk8PnGR5)
![License](https://img.shields.io/badge/license-MIT-blue.svg)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
![Rust](https://img.shields.io/badge/rust-1.90.0+-orange.svg)
![Version](https://img.shields.io/badge/version-0.16.0-brightgreen.svg)
![Build Status](https://img.shields.io/badge/build-passing-green.svg)
![Tests](https://img.shields.io/badge/tests-847%20passed-green.svg)
![Examples](https://img.shields.io/badge/examples-16%20demos-blue.svg)
[![codecov](https://codecov.io/github/wmaslonek/guardian-db/branch/main/graph/badge.svg?token=AKOZE17VN8)](https://codecov.io/github/wmaslonek/guardian-db)

---

**GuardianDB: High-performance, local-first decentralized database built on Rust and Iroh**

</div>

GuardianDB is a decentralized database designed for applications that require transparent peer-to-peer synchronization, offline-first operation, and extreme performance. It leverages the power of the Iroh stack to offer a robust alternative to centralized cloud databases.

## Evolution: Beyond OrbitDB

<div align="left">
<img align="right" src="docs/guardian-github-2.png" height="96px"/>

GuardianDB's development began as a Rust implementation of OrbitDB. However, as we sought better performance on mobile and edge devices, the limitations of the legacy stack (IPFS, CIDs, generic libp2p DHTs) became evident.

*We moved away from IPFS completely.*

GuardianDB is no longer "OrbitDB in Rust". It is a distinct and modernized database. We removed the legacy OrbitDB architecture, IPFS, CIDs, and generic libp2p networking in favor of Iroh, a next-generation P2P protocol.

</div>

### What makes GuardianDB different now?

**No Global DHT:** We use direct and encrypted connections via Iroh's Magicsock.<br>
**No Garbage Collection pauses:** We replaced heavy JSON serialization with zero-copy binary formats.<br>
**Speed:** Exclusive use of BLAKE3 hashing and QUIC transport protocol.

## ⚡Powered by Iroh

GuardianDB is built on top of Iroh, leveraging state-of-the-art protocols to handle the heavy lifting of networking and data reconciliation. Understanding GuardianDB means understanding these core concepts:

**Magicsock & NAT Traversal:** Built on technology adapted from Tailscale, instead of traditional addressing, we use Iroh's Magicsock. It automatically handles NAT traversal, hole punching, and roaming. A device can switch from Wi-Fi to 5G without breaking the connection, creating a direct and encrypted QUIC tunnel between peers.

**Iroh Endpoint (Node):** The Endpoint binds to a single UDP socket and manages all peer connections. It utilizes QUIC's native stream multiplexing and ALPN (Application-Layer Protocol Negotiation) to route traffic. This architecture allows *Blobs* (data transfer), *Docs* (state sync), and *Gossip* (real-time signals) to share the same encrypted connection simultaneously.

**EndpointID ((NodeID)Ed25519):** Security is not an afterthought. Each peer is identified by a cryptographic public key. No PeerIDs or multiaddrs, identity is the address.

**Willow Protocol:** GuardianDB uses Iroh's Willow protocol for data synchronization. Willow enables efficient and bandwidth-economical synchronization by treating data as a three-dimensional space: Entry, Author and Namespace.

**Range-Based Reconciliation:** Instead of exchanging complete file lists to discover what's missing, Willow uses 3D range-based reconciliation. It identifies and transfers only the missing data differences between two peers in milliseconds, even with millions of records.

**Epidemic Broadcast Trees:** For real-time updates (like *"User is typing..."*), we use iroh-gossip. The hybrid Epidemic Broadcast Trees algorithm combines the robustness of random (epidemic) gossip with the efficiency of multicast trees, ensuring messages reach all peers with minimal latency and low bandwidth redundancy.

## Architecture Comparison

### Terminology Shift

If you're coming from IPFS/OrbitDB, here are the GuardianDB/Iroh concept differences:

| Concept | Legacy (IPFS / OrbitDB) | GuardianDB (Iroh) | Benefit |
|--------|-------------------------|-------------------|-----------|
| Identity | PeerID (Multihash) | NodeID (Ed25519 Key) | Faster verification, smaller keys (32 bytes) |
| Content ID | CID (SHA-256 + Codecs) | Hash (BLAKE3) | Extreme hashing performance, simplified references. |
| Network | Libp2p Swarm (TCP/WS) | Iroh Endpoint (QUIC) | Native encryption, lower latency, better mobile roaming. |
| Discovery | DHT Kademlia (Global) | Relay / Direct | Private and segmented discovery. No leakage to public DHTs. |
| Data Format | IPLD DAG (JSON) | Binary (Postcard / Bao) | Zero-copy serialization, storage reduction over 50%. |

### Stack Comparison: Libp2p/IPFS vs Iroh

| Feature | Libp2p + IPFS (OrbitDB) | Iroh (GuardianDB) |
|-------|---------------|-------------------|
| Philosophy | Modular "Lego" blocks. Extremely flexible, but complex to tune. | Vertical integration. Opinionated, optimized for performance and ease of use. |
| Transport | Negotiates transport (TCP, WS, QUIC, etc). | QUIC only. Optimized for unstable networks. |
| Storage | Blockstore | Blobs |
| Synchronization | Bitswap (block-by-block exchange). | Willow (range-based reconciliation). |

## Internal Architecture and Roadmap

GuardianDB orchestrates three main Iroh primitives to deliver a complete database experience:

**1. Iroh-Blobs:** The lowest layer. Stores raw data (images, logs, binary payloads) addressed by BLAKE3 hashes.<br>
**2. Iroh-Docs:** The mutable layer. Manages key-value storage and synchronization using Last-Write-Wins (LWW) conflict resolution.<br>
**3. Iroh-Gossip:** The ephemeral layer. Handles transient messages and signals between peers.

### ✅ Migration Complete: Pure Iroh
The migration to a fully Iroh-native architecture is complete:

**KV Store and Document Store:** Now run exclusively on Iroh-Docs. All CRDT logic lives at the protocol layer, enabling instant synchronization using Willow's range-based reconciliation.<br>
**Event Log Store (Chat/Feed):** Retains the original ipfs-log logic (DAG structure) for strict ordering, but the engine has been completely refactored:

**No IPFS:** Fully disconnected from the IPFS Blockstore.<br>
**No JSON:** Replaced with binary serialization (Postcard).<br>
**No CIDs:** All linking logic uses 32-byte BLAKE3 hashes.

This hybrid approach gives us the best of both worlds: Iroh's speed for key-value and document data, and the auditability of a causal log for transaction history.

<details>
<summary>
Current Architecture
</summary>
<br />

```
GuardianDB v0.16.0
├── Core Database
│   ├── GuardianDB (guardian/mod.rs)         # Main database API facade
│   ├── BaseGuardianDB (guardian/core.rs)    # Core implementation
│   ├── Error Handling (guardian/error.rs)   # Error types
│   └── Serializer (guardian/serializer.rs)  # Data serialization
│
├── Store Implementations (stores/)
│   ├── EventLogStore                   # Immutable append-only logs
│   ├── KeyValueStore                   # Distributed key-value pairs  
│   ├── DocumentStore                   # JSON documents with queries
│   ├── BaseStore                       # Common store functionality
│   └── Operations                      # Store operations & parsing
│
├── Networking Layer (p2p/)
│   ├── IrohClient (network/client.rs)  # Native Iroh Client
│   ├── Network Core (network/core/)    # IrohBackend
│   │   ├── BatchProcessor              # Batch operation handling
│   │   ├── Blobs                       # Blob storage management
│   │   ├── ConnectionPool              # Connection pooling
│   │   ├── Docs                        # Document replication (Willow Protocol)
│   │   ├── Gossip                      # Gossipsub protocol
│   │   ├── KeySynchronizer             # Key distribution
│   │   ├── NetworkingMetrics           # Network telemetry
│   │   └── OptimizedCache              # Performance caching
│   ├── Messaging (messaging/)
│   │   ├── DirectChannel               # Peer-to-peer messaging
│   │   └── OneOnOneChannel             # Direct peer communication
│   ├── EventBus                        # Type-safe event system
│   └── Config (network/config.rs)      # Network configuration
│
├── Access Control Layer (access_control/)
│   ├── GuardianDBAccessController (acl_guardian.rs)   # Signature-based ACL
│   ├── IrohAccessController (acl_iroh.rs)             # Iroh-based ACL
│   ├── SimpleAccessController (acl_simple.rs)         # Open access (dev)
│   ├── Manifest (manifest.rs)                         # ACL configuration
│   └── Traits (traits.rs)                             # ACL traits
│
├── CRDT Log System (log/)
│   ├── Entry (entry.rs)                            # CRDT log entries
│   ├── Identity (identity.rs)                      # Peer identity
│   ├── IdentityProvider (identity_provider.rs)     # Identity management
│   ├── LamportClock (lamport_clock.rs)             # Distributed time ordering
│   ├── AccessControl (access_control.rs)           # Log-level ACL
│   └── Traits (traits.rs)                          # Log traits
│
├── Data & Storage Layer
│   ├── Cache (cache/)
│   │   └── LevelDown (level_down.rs)             # Sled-based storage
│   ├── DataStore (data_store.rs)                 # Pluggable storage interface
│   ├── Keystore (keystore.rs)                    # Ed25519 cryptographic keys
│   ├── MessageMarshaler (message_marshaler.rs)   # Message Serialization
│   ├── DBManifest (db_manifest.rs)               # Database configuration
│   └── Address (address.rs)                      # Address resolution
│
├── Reactive Systems
│   ├── ReactiveSynchronizer (reactive_synchronizer.rs)   # State sync
│   └── Events (events.rs)                                # Event emission system
│
└── Core Traits & Types
    └── Traits (traits.rs)    # Common traits & types
```
</details>

### Current Implementation Status

**Networking & Discovery:**
<ul> ✔️ Native Iroh Embedded Node (Endpoint) with QUIC transport </ul>
<ul> ✔️ Iroh Blobs for content-addressed storage </ul>
<ul> ✔️ Iroh Gossip with Epidemic Broadcast Trees </ul>
<ul> ✔️ Iroh Docs for Willow Protocol document replication </ul>
<ul> ✔️ Optimized Connection Pool with circuit breaking and load balancing </ul>
<ul> ✔️ Real-time Networking Metrics Collector with performance tracking </ul>
<ul> ✔️ EpidemicPubSub for native distributed messaging </ul>
<ul> ✔️ Key Synchronizer for distributed key consistency </ul>

**Data Stores & Operations:**
<ul> ✔️ EventLogStore (immutable append-only logs) </ul>
<ul> ✔️ KeyValueStore (distributed key-value pairs) </ul>
<ul> ✔️ DocumentStore (JSON documents with queries) </ul>
<ul> ✔️ BaseStore (common functionality and CRDT operations) </ul>
<ul> ✔️ Multi-level caching with Sled backend </ul>
<ul> ✔️ Cryptographic access control with Ed25519 signatures </ul>

## Store Types

<details>
<summary>
Event Log Store
</summary>
<br />

```rust
use guardian_db::guardian::GuardianDB;
use guardian_db::guardian::core::NewGuardianDBOptions;
use guardian_db::traits::{CreateDBOptions, EventLogStore, Store};
use guardian_db::p2p::network::client::IrohClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create development Iroh Client and database
    let client = IrohClient::development().await?;

    // Create GuardianDB instance
    let options = NewGuardianDBOptions {
        directory: Some("./guardian_data".into()),
        ..Default::default()
    };
    let db = GuardianDB::new(client, Some(options)).await?;

    // Create an event log with options
    let log_options = CreateDBOptions {
        create: Some(true),
        store_type: Some("eventlog".to_string()),
        ..Default::default()
    };
    let log = db.log("my-event-log", Some(log_options)).await?;

    // Add events to the log (append-only, immutable)
    log.add("Hello, GuardianDB!".as_bytes().to_vec()).await?;
    log.add("This is a decentralized database".as_bytes().to_vec()).await?;
    log.add("Built with Rust and Iroh".as_bytes().to_vec()).await?;

    // List all operations in the log
    let operations = log.list(None).await?;
    println!("Total entries: {}", operations.len());

    // Iterate over operations
    for (i, op) in operations.iter().enumerate() {
        println!("Entry {}: {:?}", i + 1, String::from_utf8_lossy(op.value()));
    }

    Ok(())
}
```
</details>

<details>
<summary>
Key-Value Store
</summary>
<br />

```rust
use guardian_db::guardian::GuardianDB;
use guardian_db::guardian::core::NewGuardianDBOptions;
use guardian_db::traits::KeyValueStore;
use guardian_db::p2p::network::client::IrohClient;

#[tokio::main] 
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize Iroh Client and database
    let client = IrohClient::development().await?;
    
    let options = NewGuardianDBOptions {
        directory: Some("./guardian_data".into()),
        ..Default::default()
    };
    
    let db = GuardianDB::new(client, Some(options)).await?;

    // Create a key-value store with CRDT semantics
    let kv = db.key_value("my-kv-store", None).await?;

    // CRUD operations - all operations are automatically replicated
    kv.put("app_name", "GuardianDB".as_bytes().to_vec()).await?;
    kv.put("version", "0.16.0".as_bytes().to_vec()).await?;
    kv.put("language", "Rust".as_bytes().to_vec()).await?;

    // Get values - queries the local CRDT index
    if let Some(name_bytes) = kv.get("app_name").await? {
        let name = String::from_utf8(name_bytes)?;
        println!("App: {}", name);
    }

    if let Some(version_bytes) = kv.get("version").await? {
        let version = String::from_utf8(version_bytes)?;
        println!("Version: {}", version);
    }

    // List all key-value pairs
    let all_pairs = kv.all();
    println!("Total entries: {}", all_pairs.len());
    for (key, value) in all_pairs.iter() {
        let value_str = String::from_utf8_lossy(value);
        println!("  {}: {}", key, value_str);
    }

    // Delete a key - creates a DEL operation in the distributed log
    kv.delete("version").await?;
    println!("After deletion: {} keys remaining", kv.all().len());

    Ok(())
}
```
</details>

<details>
<summary>
Document Store
</summary>
<br />

```rust
use guardian_db::guardian::GuardianDB;
use guardian_db::guardian::core::NewGuardianDBOptions;
use guardian_db::traits::{CreateDBOptions, Document, AsyncDocumentFilter};
use guardian_db::p2p::network::client::IrohClient;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize Iroh Client and database
    let client = IrohClient::development().await?;
    
    let options = NewGuardianDBOptions {
        directory: Some("./guardian_data".into()),
        ..Default::default()
    };
    
    let db = GuardianDB::new(client, Some(options)).await?;

    // Create a document store with options
    let doc_options = CreateDBOptions {
        create: Some(true),
        store_type: Some("document".to_string()),
        ..Default::default()
    };
    
    let docs = db.docs("my-document-store", Some(doc_options)).await?;

    // Add JSON documents (requires _id field)
    let project_doc = json!({
        "_id": "guardian-db",
        "name": "GuardianDB", 
        "type": "database",
        "version": "0.16.0",
        "language": "Rust",
        "features": ["decentralized", "peer-to-peer", "CRDT", "Iroh"]
    });

    let network_doc = json!({
        "_id": "iroh-network",
        "name": "Iroh Network",
        "type": "networking",
        "version": "0.92.0", 
        "protocols": ["gossip", "docs", "blobs"]
    });

    // Store documents (wrap in Box for the Document type)
    docs.put(Box::new(project_doc)).await?;
    docs.put(Box::new(network_doc)).await?;

    // Query documents by type using async filter
    let filter: AsyncDocumentFilter = Box::pin(|doc: &Document| {
        let is_match = doc
            .downcast_ref::<serde_json::Value>()
            .and_then(|v| v.get("type"))
            .and_then(|v| v.as_str())
            == Some("database");
        Box::pin(async move {
            Ok(is_match) as Result<bool, Box<dyn std::error::Error + Send + Sync>>
        })
    });
    let database_docs = docs.query(filter).await?;

    println!("Found {} database documents", database_docs.len());
    
    // Get specific document by ID
    let guardian_docs = docs.get("guardian-db", None).await?;
    println!("GuardianDB doc: {:?}", guardian_docs);

    // Delete a document
    docs.delete("iroh-network").await?;

    // Put multiple documents in batch
    let batch_docs: Vec<Document> = vec![
        Box::new(json!({"_id": "doc1", "name": "Document 1"})),
        Box::new(json!({"_id": "doc2", "name": "Document 2"})),
    ];
    docs.put_batch(batch_docs).await?;

    Ok(())
}
```
</details>

<details>
<summary>
Native Iroh Backend with QUIC transport
</summary>
<br />

```rust
use guardian_db::p2p::network::client::IrohClient;
use guardian_db::p2p::network::config::ClientConfig;
use tokio::io::AsyncReadExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Quick development setup (Native Iroh with QUIC transport)
    let client = IrohClient::development().await?;
    println!("✓ Iroh Client initialized with native QUIC backend");

    // Advanced configuration with native Iroh
    let config = ClientConfig {
        enable_pubsub: true,                        // iroh-gossip support
        data_store_path: Some("./iroh_data".into()),
        port: 4001,                                 // Iroh endpoint port (0 = random)
        enable_discovery_n0: true,                  // Discovery via n0.computer (Pkarr/DNS)
        enable_discovery_mdns: true,                // Local mDNS discovery
        known_peers: vec![],                        // NodeIds of known peers
        network: Default::default(),                // Network tuning (timeout, buffer, etc.)
        storage: Default::default(),                // iroh-blobs storage config
        gossip: Default::default(),                 // iroh-gossip config
    };

    let client = IrohClient::new(config).await?;
    println!("✓ Advanced Iroh Client configured");

    // Add data using native Iroh backend (BLAKE3 hashing)
    let data = "Hello from GuardianDB! This is stored with Iroh.";
    let add_response = client.add_bytes(data.as_bytes().to_vec()).await?;

    println!("Added to Iroh: {}", add_response.hash);
    println!("Size: {} bytes", add_response.size);

    // Retrieve data from Iroh (with smart caching)
    let mut stream = client.backend().cat(&add_response.hash).await?;
    let mut buffer = Vec::new();
    stream.read_to_end(&mut buffer).await?;

    let retrieved_text = String::from_utf8(buffer)?;
    println!("Retrieved: {}", retrieved_text);

    // Pin the content (persistent tags prevent GC)
    client.backend().pin_add(&add_response.hash).await?;
    println!("Content pinned with persistent tag");

    // Get node info (Iroh NodeId)
    let node_info = client.id().await?;
    println!("Node ID: {}", node_info.id);
    println!("Protocol Version: {}", node_info.protocol_version);
    println!("Agent Version: {}", node_info.agent_version);

    Ok(())
}
```
</details>

## Configuration

<details>
<summary>
Basic Configuration
</summary>
<br />

```rust
use guardian_db::guardian::core::{GuardianDB, NewGuardianDBOptions};
use guardian_db::p2p::network::config::ClientConfig;
use tracing::{info, Level};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging/tracing
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    // Configure Iroh client with native backend
    let config = ClientConfig::default();
    info!("Iroh Client config prepared");
    
    // Configure GuardianDB
    let options = NewGuardianDBOptions {
        directory: Some("./guardian_data".into()),
        ..Default::default()
    };
    
    let db = GuardianDB::new(Some(config), Some(options)).await?;
    info!("GuardianDB initialized successfully");

    // Database is ready for use
    let stores_info = format!(
        "Available store types: EventLog, KeyValue, Document"
    );
    info!("{}", stores_info);
    
    Ok(())
}
```
</details>

<details>
<summary>
Advanced Configuration
</summary>
<br />

```rust
use guardian_db::guardian::core::{GuardianDB, NewGuardianDBOptions};
use guardian_db::p2p::network::config::{ClientConfig, NetworkConfig, StorageConfig, GossipConfig};
use tracing::{info, Level};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize comprehensive logging
    tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .init();

    // Production Iroh configuration with native backend
    let config = ClientConfig {
        enable_pubsub: true,                        // iroh-gossip support
        data_store_path: Some("./production_iroh".into()),
        port: 4001,                                 // Fixed port for production
        enable_discovery_n0: true,                  // Discovery via n0.computer (Pkarr/DNS)
        enable_discovery_mdns: true,                // Local mDNS discovery
        known_peers: vec![],                        // Add known NodeIds here
        
        // Network configuration
        network: NetworkConfig {
            connection_timeout: Duration::from_secs(60),
            max_peers_per_session: 1000,
            io_buffer_size: 128 * 1024,             // 128KB
            keepalive_interval: Duration::from_secs(120),
        },
        
        // Storage configuration (iroh-blobs)
        storage: StorageConfig {
            enable_memory_cache: true,
            max_cache_size: 1024 * 1024 * 1024,     // 1GB cache
            max_blob_size: 100 * 1024 * 1024,       // 100MB per blob
            enable_gc: true,
            gc_interval: Duration::from_secs(1800), // 30 minutes
        },
        
        // Gossip configuration (iroh-gossip)
        gossip: GossipConfig {
            max_message_size: 10 * 1024 * 1024,     // 10MB
            message_buffer_size: 10000,
            operation_timeout: Duration::from_secs(60),
            heartbeat_interval: Duration::from_millis(500),
            max_topics: 1000,
        },
    };

    info!("Production Iroh Client configured");

    // Advanced GuardianDB configuration
    let db_options = NewGuardianDBOptions {
        directory: Some("./production_guardian".into()),
        ..Default::default()
    };

    let db = GuardianDB::new(Some(config), Some(db_options)).await?;
    info!("GuardianDB configured for production use");

    // Database is ready for use with advanced configuration
    info!("Advanced configuration applied successfully");

    Ok(())
}
```
</details>

## Development

### Prerequisites

- Rust 1.90+ (edition 2024)
- Git

### Build

```bash
git clone https://github.com/wmaslonek/guardian-db.git
cd guardian-db
cargo build
```

<details>
<summary>
Tests
</summary>
<br />

```bash
# Run all tests (776 passing tests)
cargo test

# Run only unit tests (in src/)
cargo test --lib

# Run only integration tests (in tests/)
cargo test --test '*'

# Run specific test files
cargo test --test integration_lifecycle        # Lifecycle tests
cargo test --test integration_replication      # P2P replication tests
cargo test --test integration_access_control   # Access control tests
cargo test --test integration_persistence      # Persistence tests
cargo test --test test_determinism             # Determinism tests

# Run specific unit test modules
cargo test guardian_core_test               # Core GuardianDB tests
cargo test event_log_store_test             # Event log store tests
cargo test kv_store_test                    # Key-value store tests
cargo test document_store_test              # Document store tests
cargo test iroh_backend_test                # Iroh backend tests
cargo test connection_pool_test             # Connection pool tests
cargo test networking_metrics_test          # Networking metrics tests
cargo test access_control                   # Access control tests

# Run with detailed output
RUST_LOG=debug cargo test -- --nocapture

# Run with single thread (for P2P tests)
cargo test --test integration_replication -- --test-threads=1

# Run tests in release mode (faster)
cargo test --release
```
</details>

<details>
<summary>
Build Features
</summary>
<br />

```bash
# Build with all features (default)
cargo build

# Build optimized release version
cargo build --release

# Check code quality and formatting
cargo clippy                   # Comprehensive linting
cargo fmt                      # Code formatting
cargo check                    # Fast compilation check

# Build documentation
cargo doc --open               # Generate and open docs

# Development tools
cargo watch -x check           # Auto-rebuild on changes  
cargo audit                    # Security audit
```
</details>

## Community & Support

<div align="left">
<img align="right" src="docs/guardian-github-1.png" height="96px"/>

GuardianDB is an open-source project welcoming contributions and discussions from developers interested in decentralized systems, Iroh, and Rust programming.

If you are excited about the project, don't hesitate to join our
[Discord](https://discord.gg/Ezzk8PnGR5)! We try to be as welcoming as possible to everybody from
any background. You can ask your questions and share what you built with the community! 
Follow updates on [Twitter](https://x.com/willsearch_) and [LinkedIn](https://www.linkedin.com/company/willsearch/)!

**Get Involved:**
- **Issues**: Report bugs and request features on [GitHub Issues](https://github.com/wmaslonek/guardian-db/issues)
- **Discussions**: Technical discussions and Q&A on [GitHub Discussions](https://github.com/wmaslonek/guardian-db/discussions)  
- **Documentation**: Contribute to docs and examples
- **Code**: Submit PRs for bug fixes and new features

We welcome developers from all backgrounds and experience levels!

</div>

## Status

GuardianDB is currently in active development, and there will be breaking changes. While any resulting
issues are likely to be easy to fix, there are no guarantees at this stage.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution instructions.

## License

GuardianDB is dual-licensed under the terms of both the MIT license and the Apache License 2.0.

See [LICENSE-MIT](./LICENSE-MIT) and [LICENSE-APACHE](./LICENSE-APACHE) for details. Opening a pull
request is assumed to signal agreement with these licensing terms.

## Acknowledgments

- **[ipfs-log-rs](https://github.com/eqlabs/ipfs-log-rs)** - CRDT Log implementation foundation
- **[Iroh](https://github.com/n0-computer/iroh)** - QUIC-based P2P data synchronization

This project incorporates and builds upon code from [ipfs-log-rs](https://github.com/eqlabs/ipfs-log-rs),
licensed under the MIT License © EQLabs. Significant enhancements and optimizations have been added
for production use in decentralized applications.

## Useful Links

- **[Iroh Documentation](https://www.iroh.computer/)** - QUIC-based P2P data synchronization

---

**GuardianDB** - A secure, performant, and fully decentralized peer-to-peer database for the modern Web.
