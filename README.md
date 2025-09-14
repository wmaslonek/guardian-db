# GuardianDB: The Rust Implementation of OrbitDB

<p align="left">
  <img src="docs/guardian-db-logo.png" alt="GuardianDB Logo" width="300" height="300"/>
</p>

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Rust](https://img.shields.io/badge/rust-1.89.0+-orange.svg)
![Version](https://img.shields.io/badge/version-0.9.13-brightgreen.svg)
![Build Status](https://img.shields.io/badge/build-passing-green.svg)
![Tests](https://img.shields.io/badge/tests-77passed-green.svg)

## 💬 Join Our Community
Join our Discord to collaborate: [Join Discord](https://discord.gg/Ezzk8PnGR5)

## 🔥 What is GuardianDB?
GuardianDB is a decentralized, peer-to-peer database built on top of IPFS. It allows applications to store and share data without relying on centralized servers, using CRDTs (Conflict-free Replicated Data Types) to synchronize data and prevent conflicts. Think of GuardianDB as having a "MongoDB" or "CouchDB", but without a central server, running on IPFS, where each participant keeps a copy and shares changes.

## 🎯 Overview

**GuardianDB** is a complete rewrite of OrbitDB in Rust, designed to overcome the limitations of previous implementations while providing superior performance, safety, and functionality.

### 🚀 Why GuardianDB is Superior?

**Compared to Original OrbitDB (JavaScript):**
- **🔒 Memory Safety**: Zero memory vulnerabilities thanks to Rust's ownership system
- **⚡ Superior Performance**: Elimination of V8 overhead and garbage collector
- **🛡️ Type Safety**: Compile-time error prevention vs JavaScript runtime errors
- **📦 Native Binary**: Standalone executables without Node.js runtime dependencies
- **🔄 True Concurrency**: Fearless concurrency with Tokio async runtime

**Compared to OrbitDB (Go):**
- **🎯 Zero-Cost Abstractions**: High-level abstractions without performance overhead
- **🔐 Ownership System**: Deterministic memory management vs Go's garbage collector
- **⚙️ LLVM Optimization**: Compilation to highly optimized machine code
- **🧵 Safe Concurrency**: Thread-safe by design vs potential goroutine data races
- **📊 Predictable Performance**: No GC pauses, consistent latency

### 💎 Core Rust Advantages

- **🔒 Type Safety**: Compile-time guarantees prevent runtime errors
- **⚡ Performance**: Zero-cost abstractions and LLVM optimizations
- **🛡️ Memory Safety**: Ownership system prevents leaks and use-after-free
- **🌐 Decentralization**: Peer-to-peer architecture with no single points of failure
- **📦 Native IPFS**: Integrated IPFS Core API implementation
- **🔄 Event-Driven**: Reactive, type-safe event system with Tokio
- **⚙️ Zero Runtime**: Standalone binaries without VM or interpreter requirements

## 🏗️ Architecture

```
GuardianDB
├── Core (guardian.rs)                # Main database interface
├── Base Guardian (base_guardian.rs)  # Core database implementation
├── Store Types
│   ├── Event Log Store              # Immutable append-only logs
│   ├── Key-Value Store              # Distributed key-value storage
│   └── Document Store               # JSON document storage with queries
├── IPFS Core API
│   ├── Client (client.rs)           # Native IPFS client implementation
│   ├── Config (config.rs)           # IPFS configuration management
│   └── Compat (compat.rs)          # Compatibility layer
├── PubSub System
│   ├── Direct Channel              # Peer-to-peer direct communication
│   ├── Raw PubSub                  # Low-level publish/subscribe
│   └── Event PubSub                # Event-driven messaging
├── Access Control
│   ├── Guardian AC                 # Custom access control system
│   ├── IPFS AC                     # IPFS signature-based control
│   └── Simple AC                   # Open access control
├── Event System
│   ├── Event Bus (events.rs)       # Type-safe event system
│   └── Replicator                  # Automatic data synchronization
├── Data Storage
│   ├── Cache System                # Multi-level caching (Sled)
│   ├── Keystore                    # Cryptographic key management
│   └── Datastore                   # Pluggable storage backends
└── IPFS Log (ipfs_log/)
    ├── Entry                       # Individual log entries
    ├── Identity                    # Peer identity management
    └── Lamport Clock               # Logical time synchronization
```

## 🚀 Features

### Store Types

#### Event Log Store
```rust
use guardian_db::{GuardianDB, CreateDBOptions};
use guardian_db::ipfs_core_api::IpfsClient;

// Create IPFS client
let ipfs = IpfsClient::development().await?;

// Create GuardianDB instance
let db = GuardianDB::new(ipfs, None).await?;

// Create an event log
let log = db.log("my-log", None).await?;

// Add events
log.add(b"Hello, World!").await?;
log.add(b"Second event").await?;

// Iterate over events
for entry in log.iterator(None).await? {
    println!("Hash: {}, Data: {:?}", entry.hash(), entry.payload());
}
```

#### Key-Value Store
```rust
// Create a key-value store
let kv = db.key_value("my-store", None).await?;

// CRUD operations
kv.put("name", b"Guardian DB").await?;
kv.put("version", b"0.9.2").await?;

let value = kv.get("name").await?;
println!("Name: {:?}", value);

// Delete
kv.del("version").await?;

// List all keys
let keys = kv.keys().await?;
for key in keys {
    println!("Key: {}", key);
}
```

#### Document Store
```rust
use serde_json::json;

// Create a document store
let docs = db.docs("my-docs", None).await?;

// Add documents
let doc = json!({
    "name": "Guardian DB",
    "type": "database",
    "version": "0.9.2",
    "features": ["decentralized", "peer-to-peer", "rust"]
});

docs.put(doc).await?;

// Query documents
let results = docs.query(|doc| {
    doc["type"] == "database"
}).await?;

println!("Found {} documents", results.len());
```

### Event System

```rust
use guardian_db::events::EventBus;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DatabaseEvent {
    action: String,
    data: Vec<u8>,
}

// Create event bus
let event_bus = EventBus::new();

// Create emitter
let emitter = event_bus.emitter::<DatabaseEvent>().await?;

// Subscribe to events
let mut receiver = event_bus.subscribe::<DatabaseEvent>().await?;

// Spawn listener task
tokio::spawn(async move {
    while let Ok(event) = receiver.recv().await {
        println!("Event received: {:?}", event);
    }
});

// Emit event
emitter.emit(DatabaseEvent {
    action: "created".to_string(),
    data: b"new database".to_vec(),
})?;
```

### IPFS Core API

```rust
use guardian_db::ipfs_core_api::{IpfsClient, ClientConfig};
use std::io::Cursor;

// Development configuration (for testing)
let client = IpfsClient::development().await?;

// Custom configuration
let config = ClientConfig {
    enable_pubsub: true,
    enable_swarm: true,
    data_store_path: Some("./ipfs_data".into()),
    listening_addrs: vec![
        "/ip4/127.0.0.1/tcp/0".to_string(),
    ],
    bootstrap_peers: vec![],
    enable_mdns: false,
    enable_kad: false,
    ..Default::default()
};

let ipfs = IpfsClient::new(config).await?;

// Add data to IPFS
let data = "Hello, IPFS!".as_bytes();
let cursor = Cursor::new(data.to_vec());
let response = ipfs.add(cursor).await?;

println!("Added to IPFS: {}", response.hash);

// Retrieve data from IPFS
let mut stream = ipfs.cat(&response.hash).await?;
let mut buffer = Vec::new();

use tokio::io::AsyncReadExt;
stream.read_to_end(&mut buffer).await?;

println!("Retrieved: {}", String::from_utf8(buffer)?);
```

## 📦 Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
guardian-db = "0.9.2"
tokio = { version = "1.47", features = ["full"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
```

## 🔧 Configuration

### Basic Configuration

```rust
use guardian_db::{GuardianDB, NewGuardianDBOptions};
use guardian_db::ipfs_core_api::IpfsClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configure IPFS client
    let ipfs = IpfsClient::development().await?;
    
    // Configure Guardian DB
    let options = NewGuardianDBOptions {
        directory: Some("./GuardianDB".to_string()),
        ..Default::default()
    };
    
    let db = GuardianDB::new(ipfs, Some(options)).await?;
    
    Ok(())
}
```

### Advanced Configuration

```rust
use guardian_db::{
    GuardianDB, NewGuardianDBOptions,
    ipfs_core_api::{IpfsClient, ClientConfig},
    access_controller::AccessControllerType,
};

// Advanced IPFS configuration
let ipfs_config = ClientConfig {
    enable_pubsub: true,
    enable_swarm: true,
    enable_mdns: true,
    enable_kad: true,
    data_store_path: Some("./ipfs_data".into()),
    listening_addrs: vec![
        "/ip4/0.0.0.0/tcp/4001".to_string(),
        "/ip6/::/tcp/4001".to_string(),
    ],
    bootstrap_peers: vec![
        "/dnsaddr/bootstrap.libp2p.io/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN".to_string(),
    ],
    ..Default::default()
};

let ipfs = IpfsClient::new(ipfs_config).await?;

// GuardianDB configuration
let db_options = NewGuardianDBOptions {
    directory: Some("./guardian_data".to_string()),
    cache_size: Some(1000),
    ..Default::default()
};

let db = GuardianDB::new(ipfs, Some(db_options)).await?;
```

## 🧪 Examples

See the `examples/` folder for complete examples:

- **`ipfs_core_api_demo.rs`** - IPFS Core API usage and testing
- **`event_bus_usage.rs`** - Event system and publish/subscribe
- **`direct_channel_transport_demo.rs`** - Direct peer-to-peer communication

Run an example:

```bash
cargo run --example ipfs_core_api_demo
cargo run --example event_bus_usage
cargo run --example direct_channel_transport_demo
```

## 🛠️ Development

### Prerequisites

- Rust 1.70+ (edition 2024)
- Git

### Build

```bash
git clone https://github.com/wmaslonek/guardian-db.git
cd guardian-db
cargo build
```

### Tests

```bash
# Run all unit tests (66 tests)
cargo test --lib

# Run all tests including integration tests requiring IPFS
cargo test -- --include-ignored

# Run specific test module
cargo test events::tests
cargo test ipfs_core_api::client::tests

# Run with debug output
RUST_LOG=debug cargo test
```

### Features

```bash
# Build with native IPFS support (default)
cargo build --features native-ipfs

# Build without default features
cargo build --no-default-features

# Check code quality
cargo clippy
cargo fmt
```

## 📚 Documentation

- **[API Documentation](docs/)** - Complete API documentation
- **[Event Bus Implementation](docs/event_bus_implementation.md)** - Event system architecture
- **[IPFS Core API](docs/IPFS_CORE_API_README.md)** - Native IPFS client documentation
- **[Direct Channel Improvements](docs/DIRECT_CHANNEL_IMPROVEMENTS.md)** - P2P communication
- **[Cache System](docs/CACHE_FIXES.md)** - Multi-level caching architecture
- **[Datastore Improvements](docs/DATASTORE_IMPROVEMENTS.md)** - Storage backend enhancements

### Generating Documentation

```bash
cargo doc --open
```

## 🔧 Project Status

### ✅ Implemented

- ✅ Core GuardianDB with async/await
- ✅ Event Log Store (append-only logs)
- ✅ Key-Value Store (distributed KV storage)  
- ✅ Document Store (JSON documents)
- ✅ Event Bus System (type-safe events)
- ✅ IPFS Core API (native implementation)
- ✅ Access Controllers (Guardian, IPFS, Simple)
- ✅ PubSub System (Direct channels, Raw pubsub)
- ✅ Cache System (Sled)
- ✅ Keystore (cryptographic key management)
- ✅ IPFS Log (CRDTs with Lamport clocks)
- ✅ Replicator (automatic synchronization)
- ✅ Error handling and type safety
- ✅ Comprehensive test suite (66 tests passing)

### 🚧 In Development

- 🚧 Advanced Document Store queries and indexing
- 🚧 Performance optimizations and benchmarks
- 🚧 Enhanced replication strategies
- 🚧 WebAssembly support
- 🚧 Network transport improvements

### 📋 Planned

- 📋 Sharding support for large datasets
- 📋 Automatic compaction and garbage collection
- 📋 GraphQL query interface
- 📋 Language bindings (Python, JavaScript, Go)
- 📋 Distributed consensus algorithms
- 📋 Web-based management interface

## 🤝 Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution instructions.

### Quick Steps

1. Fork the project
2. Create a branch (`git checkout -b feature/new-feature`)
3. Commit your changes (`git commit -am 'Add new feature'`)
4. Push to the branch (`git push origin feature/new-feature`)
5. Open a Pull Request

## 📄 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## 🙏 Acknowledgments

- **[OrbitDB](https://github.com/orbitdb/orbit-db)** - Inspiration and reference
- **[go-orbit-db](https://github.com/berty/go-orbit-db)** - Inspiration and reference
- **[ipfs-log-rs](https://github.com/eqlabs/ipfs-log-rs)** - IPFS logs implementation
- **[rust-ipfs](https://github.com/rs-ipfs/rust-ipfs)** - Native IPFS client
- **Rust Community** - Amazing tools and libraries

This project incorporates code from [ipfs-log-rs](https://github.com/eqlabs/ipfs-log-rs),
licensed under the MIT License © EQLabs.

## 🔗 Useful Links

- **[Original OrbitDB](https://orbitdb.org/)**
- **[OrbitDB Golang](https://berty.tech/docs/go-orbit-db/)**
- **[IPFS](https://ipfs.io/)**
- **[libp2p](https://libp2p.io/)**
- **[Rust](https://www.rust-lang.org/)**

## 📊 Statistics

- **Language**: 100% Rust (Edition 2024)
- **Files**: 71 Rust source files
- **Lines of code**: ~26,700+ lines
- **Dependencies**: Minimal and carefully selected
- **Test coverage**: 66 tests passing, 27 integration tests
- **Performance**: Zero-cost abstractions with LLVM optimization
- **Memory safety**: 100% safe Rust code
- **Concurrency**: Fearless concurrency with Tokio

---

**GuardianDB** - A secure, performant, and fully decentralized peer-to-peer database for the modern Web.
