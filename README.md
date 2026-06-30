<div align="center">
  <img src="docs/logotipo-guardiandb-new-outubro.png" alt="GuardianDB Logo" width="350"/>

[![Discord](https://img.shields.io/discord/1410233136846995519?label=chat&logo=discord&logoColor=white&style=flat-square&color=7289DA)](https://discord.gg/Ezzk8PnGR5)
![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)
![Rust](https://img.shields.io/badge/rust-1.95.0+-orange.svg)
![Version](https://img.shields.io/badge/version-0.17.1-brightgreen.svg)
[![codecov](https://codecov.io/github/wmaslonek/guardian-db/branch/main/graph/badge.svg?token=AKOZE17VN8)](https://codecov.io/github/wmaslonek/guardian-db)

---

**High-performance, local-first decentralized database built on Rust and Iroh**

</div>

GuardianDB is a decentralized, local-first database for apps that need peer-to-peer
synchronization, offline-first operation, and high performance. Every node keeps a full
local replica: reads and writes are local (no server round-trip), and changes converge
across peers automatically over [Iroh](https://www.iroh.computer/).

It began as a Rust port of OrbitDB but is no longer "OrbitDB in Rust". The legacy
IPFS/CID/libp2p stack has been removed in favor of Iroh's QUIC transport, BLAKE3 hashing,
and Willow range-based set reconciliation.

## Why Iroh

- **Direct, encrypted connections** — Iroh's Magicsock handles NAT traversal, hole
  punching, and roaming (Wi-Fi ⇄ 5G without dropping). No global DHT.
- **QUIC transport** — one encrypted UDP socket multiplexes blobs (data), docs (state),
  and gossip (signals) per peer.
- **Identity is the address** — each peer is an Ed25519 public key (`NodeId`, 32 bytes).
- **Range-Based Set Reconciliation (Willow)** — peers transfer only the diff between them, not
  full record lists, syncing millions of records in milliseconds.
- **Gossip for real-time signals** — iroh-gossip's epidemic broadcast trees fan out
  ephemeral messages with low latency and redundancy.

### Coming from IPFS/OrbitDB

| Concept | Legacy (IPFS / OrbitDB) | GuardianDB (Iroh) |
|---------|-------------------------|-------------------|
| Identity | PeerID (Multihash) | EndpointID (Ed25519, 32 bytes) |
| Content ID | CID (SHA-256 + codecs) | Hash (BLAKE3) |
| Network | libp2p swarm (TCP/WS) | Iroh Endpoint (QUIC) |
| Discovery | Kademlia DHT (global) | Pkarr/DNS + mDNS, direct |
| Data format | IPLD DAG (JSON) | Binary (Postcard) |
| Sync | Bitswap (block-by-block) | Willow (range-based) |

## How the stores map to Iroh

- **KeyValueStore / DocumentStore** run on **Iroh-Docs** (Last-Write-Wins CRDT), syncing via
  Willow range-based reconciliation.
- **EventLogStore** keeps a causal DAG (ipfs-log lineage) for strict ordering and
  auditability — but with no IPFS, no JSON, and 32-byte BLAKE3 links instead of CIDs.

<details>
<summary>Source layout</summary>
<br />

```
guardian-db/
├── guardian/            # GuardianDB facade (mod.rs) + core impl (core.rs)
├── stores/              # EventLogStore, KeyValueStore, DocumentStore, BaseStore
├── p2p/
│   ├── network/client.rs    # IrohClient
│   ├── network/core/        # IrohBackend: blobs, docs, gossip, connection_pool, metrics, cache
│   ├── network/config.rs    # ClientConfig / NetworkConfig / StorageConfig / GossipConfig
│   └── messaging/           # DirectChannel, OneOnOneChannel
├── access_control/      # Guardian (signature), Iroh, and Simple (open) controllers
├── log/                 # CRDT log: Entry, Identity, LamportClock, ACL
├── cache/, data_store.rs, keystore.rs, db_manifest.rs, address.rs
└── odm/                 # Optional ODM layer (feature = "odm")
```
</details>

## Quick Start

```toml
[dependencies]
guardian-db = "0.17"
tokio = { version = "1", features = ["full"] }
```

```rust
use guardian_db::guardian::GuardianDB;
use guardian_db::guardian::core::NewGuardianDBOptions;
use guardian_db::p2p::network::client::IrohClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Start a local Iroh node and open a database persisted under ./guardian_data.
    let client = IrohClient::development().await?;
    let db = GuardianDB::new(client, Some(NewGuardianDBOptions {
        directory: Some("./guardian_data".into()),
        ..Default::default()
    })).await?;

    // Open (or create) a key-value store. Writes replicate to peers automatically.
    let kv = db.key_value("settings", None).await?;
    kv.put("theme", b"dark".to_vec()).await?;

    if let Some(v) = kv.get("theme").await? {
        println!("theme = {}", String::from_utf8_lossy(&v));
    }
    Ok(())
}
```

`GuardianDB::new` takes an `IrohClient` plus optional `NewGuardianDBOptions`. Use
`IrohClient::development()` for local work, or `IrohClient::new(config)` with a tuned
`ClientConfig` (see [Configuration](#configuration)) for production.

## Scaling

A common question: *how does a local-first P2P database scale?* GuardianDB has no central
server, so you don't scale a bottleneck — you add peers and tune how they connect and sync.

**Reads & writes are local.** Each node answers queries from its own replica with no network
round-trip, so read throughput scales linearly with the number of nodes. Writes are applied
locally and propagated asynchronously.

**Sync cost is proportional to the diff, not the dataset.** Willow range-based
reconciliation exchanges only what two peers are missing, so steady-state sync stays cheap
even as total data grows. BLAKE3 + QUIC keep hashing and transport fast.

**Discovery & connectivity** (in `ClientConfig`):
- `enable_discovery_mdns` — find peers on the same LAN automatically.
- `enable_discovery_n0` — global discovery via Pkarr/DNS (n0.computer), for peers across
  the internet.
- `known_peers` / `config.add_known_peer(node_id)` — bootstrap against specific nodes.
- `db.connect_to_peer(node_id).await?` — force a direct connection and sync with one peer.

Share a node's identity so others can reach it:

```rust
let client = IrohClient::development().await?;
let node_id = client.id().await?.id;   // an Ed25519 NodeId — share this with peers
let db = GuardianDB::new(client, None).await?;
```

**Tuning knobs that matter as you grow** (all in `ClientConfig`):

| Concern | Field | Default → Production |
|---------|-------|----------------------|
| Concurrent peers | `network.max_peers_per_session` | 100 → 1000 |
| Connection timeout / keepalive | `network.connection_timeout`, `network.keepalive_interval` | 30s / 60s → 60s / 120s |
| Blob cache | `storage.max_cache_size` | 100 MB → 1 GB |
| Largest object | `storage.max_blob_size` | 10 MB → 100 MB |
| Gossip throughput | `gossip.message_buffer_size`, `gossip.max_topics` | 1000 / 100 → 10000 / 1000 |

The connection pool does circuit breaking and load balancing across these connections
automatically.

**Topology guidance:**
- *Small groups (a handful of peers):* let mDNS / n0 discovery form a full mesh — no extra
  setup.
- *Larger or internet-spanning deployments:* run one or more **always-on "super peers"** with
  `enable_discovery_n0 = true` and a fixed `port` as stable rendezvous/sync points, and list
  them in every client's `known_peers`.
- *Many independent workloads:* partition by database name and gossip topic so unrelated
  peers don't sync data they don't need.

Start from a preset and adjust: `ClientConfig::production()` already raises peer limits,
cache sizes, and gossip buffers for you.

## Optional ODM and Collection API

GuardianDB now includes an optional TypeORM/Mongoose-inspired ODM layer for applications that want higher-level modeling primitives without giving up local-first replication. The ODM sits above `DocumentStore`, so documents still synchronize through Iroh Docs/Willow while application code can use schemas, collections, indexes, and CRUD helpers.

Enable the Rust ODM explicitly:

```toml
[dependencies]
guardian-db = { version = "0.17", features = ["odm"] }
```

### Rust model definitions

```rust
use guardian_db::odm::Model;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Model)]
#[model(collection = "employees", timestamps)]
struct Employee {
    #[primary_key]
    ssn: String,
    #[unique]
    email: String,
    #[index]
    department: String,
    name: String,
    hourly_pay: String,
    created_at: Option<String>,
    updated_at: Option<String>,
}

let employees = db.model_collection::<Employee>().await?;
employees.insert_one(Employee {
    ssn: "562-48-5384".into(),
    email: "elon@example.com".into(),
    department: "engineering".into(),
    name: "Elon".into(),
    hourly_pay: "$15".into(),
    created_at: None,
    updated_at: None,
}).await?;
```

Supported derive attributes include `#[primary_key]`, `#[unique]`, `#[index]`, `#[model(collection = "...")]`, `#[model(timestamps)]`, `#[model(flexible)]`, and schema versions. Runtime schemas are available through `ModelSchema`, `FieldDefinition`, and `Collection::new` for dynamic collections.

### JavaScript/TypeScript SDK shape

The TypeScript SDK in `packages/guardiandb-odm` exposes the collection API requested in the ODM RFC. The included process-local transport is for SDK development and tests; production Node/WASM/mobile bindings should implement `GuardianTransport` against the Rust/Iroh backend.

```javascript
import GuardianDB from "@guardiandb/odm";
import Iroh from "iroh";

const iroh = await Iroh.create();
const guardiandb = await GuardianDB.init("DatabaseName", iroh, {
  path: "./.guardiandb",
});

console.log(await GuardianDB.listDatabases());

const collection = await guardiandb.initCollection("employees", {
  primaryKey: "ssn",
  unique: ["ssn"],
  indexes: ["department"],
  timestamps: true,
});

await collection.insertOne({
  name: "Elon",
  ssn: "562-48-5384",
  department: "engineering",
  hourly_pay: "$15",
});

const employee = await collection.findOne({ ssn: "562-48-5384" });
const updatedEmployee = await collection.update(
  { ssn: "562-48-5384" },
  { $set: { hourly_pay: "$100" } }
);
```

Collections support `insertOne`, batch `insert`, `findOne`, `find`, `findById`, and first-match `update` with MongoDB-style operators such as `$set`, `$unset`, and `$inc`.

### Consistency boundary

ODM writes validate field types, primary keys, uniqueness, required/nullability rules, immutable primary keys, and strict schemas before persistence. Primary, unique, and secondary indexes are rebuilt from the current local document view and used for equality-query narrowing.

The implemented guarantees are intentionally local to a collection instance. GuardianDB remains decentralized and eventually convergent; disconnected peers can still create conflicting unique values that must be reconciled after replication. `TransactionContext` and `ConsistencyLevel` reserve the API shape for future replicated transaction coordination, while unsupported replicated transactions are rejected explicitly instead of implying global ACID semantics.

See [`docs/odm.md`](docs/odm.md) for the full design notes and caveats.

## PostgreSQL Compatibility (TypeORM, psql, node-postgres)

GuardianDB now ships a **PostgreSQL-compatible relational layer** on top of its
document model. Standard PostgreSQL clients — `psql`, node-postgres, **TypeORM**
(`type: "postgres"`), DBeaver — connect over the PostgreSQL wire protocol and
run ordinary SQL (DDL, DML, joins, aggregates, transactions, migrations), with
no GuardianDB-specific client code. 

**NOTE Locking and other more advanced concepts in Postgres are to be supported**

```bash
cargo run --features pgwire --bin guardian-pgwire        # PostgreSQL gateway on 127.0.0.1:15432
psql 'postgres://guardian:guardian@127.0.0.1:15432/app'
```

```ts
import { DataSource } from "typeorm";
const ds = new DataSource({
  type: "postgres", host: "127.0.0.1", port: 15432,
  username: "guardian", password: "guardian", database: "app",
  synchronize: true, entities: [User, Post, Org],
});
await ds.initialize();   // schema sync, migrations, repositories, QueryBuilder, transactions
```

This lives inside the `guardian-db` crate as feature-gated modules:
`relational` (types, catalog, storage trait), `sql`
(parser/planner/executor, `information_schema`/`pg_catalog`), and `pgwire`
(wire-protocol server). Enable the **`sql`** feature for the embedded engine —
which maps relational storage onto a replicated GuardianDB document store,
preserving the local-first / P2P model — or the **`pgwire`** feature (which
implies `sql`) to also build the `guardian-pgwire` server binary.

- Full guide & compatibility matrix: [`docs/postgres-compat.md`](docs/postgres-compat.md)
- Example TypeORM app: [`examples/postgres-typeorm`](examples/postgres-typeorm)
- Native driver: [`packages/guardiandb-postgres-typeorm`](packages/guardiandb-postgres-typeorm)
- Conformance tests: [`tests/postgres-compat`](tests/postgres-compat)

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
    kv.put("version", "0.17.0".as_bytes().to_vec()).await?;
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
        "version": "0.17.0",
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

The simplest path is a preset on `IrohClient`. Each preset tunes networking, storage, and
gossip together:

| Preset | Discovery | Storage | Use for |
|--------|-----------|---------|---------|
| `IrohClient::development()` | mDNS only | persistent, GC off | local dev |
| `IrohClient::production()` | mDNS + n0 (global) | 1 GB cache, GC on | deployments |
| `IrohClient::new(ClientConfig::testing())` | none | in-memory | tests |
| `IrohClient::new(ClientConfig::offline())` | none | persistent | no networking |

For full control, build a `ClientConfig` and pass it to `IrohClient::new`:

```rust
use guardian_db::guardian::GuardianDB;
use guardian_db::guardian::core::NewGuardianDBOptions;
use guardian_db::p2p::network::client::IrohClient;
use guardian_db::p2p::network::config::{ClientConfig, NetworkConfig, StorageConfig, GossipConfig};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ClientConfig {
        enable_pubsub: true,                        // iroh-gossip
        data_store_path: Some("./iroh_data".into()),
        port: 4001,                                 // 0 = random port
        enable_discovery_n0: true,                  // global discovery (Pkarr/DNS)
        enable_discovery_mdns: true,                // local discovery
        known_peers: vec![],                        // bootstrap NodeIds
        network: NetworkConfig {
            connection_timeout: Duration::from_secs(60),
            max_peers_per_session: 1000,
            io_buffer_size: 128 * 1024,
            keepalive_interval: Duration::from_secs(120),
        },
        storage: StorageConfig {
            enable_memory_cache: true,
            max_cache_size: 1024 * 1024 * 1024,     // 1 GB
            max_blob_size: 100 * 1024 * 1024,       // 100 MB per blob
            enable_gc: true,
            gc_interval: Duration::from_secs(1800),
        },
        gossip: GossipConfig {
            max_message_size: 10 * 1024 * 1024,
            message_buffer_size: 10000,
            operation_timeout: Duration::from_secs(60),
            heartbeat_interval: Duration::from_millis(500),
            max_topics: 1000,
        },
    };

    let client = IrohClient::new(config).await?;
    let db = GuardianDB::new(client, Some(NewGuardianDBOptions {
        directory: Some("./guardian_data".into()),
        ..Default::default()
    })).await?;

    let _ = db; // open stores via db.log() / db.key_value() / db.docs()
    Ok(())
}
```

## Development

**Prerequisites:** Rust 1.95+ (edition 2024) and Git.

```bash
git clone https://github.com/wmaslonek/guardian-db.git
cd guardian-db
cargo build            # build the library
cargo test             # run the test suite
cargo run --example p2p_chat_tui   # try a P2P demo (see examples/)
```

```bash
# Quality
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all
cargo doc --no-deps --open

# Optional ODM layer (feature-gated)
cargo test --features odm odm

# Benchmark features like queries, read operations, concurrency
cargo test --features odm --test odm_benchmark_reliability
cargo bench --features odm --bench odm_benchmark

# Check the TypeScript ODM SDK
cd packages/guardiandb-odm
npm test
cd ../..

# To test above MongoDB's 16 MiB BSON document limit
set GUARDIANDB_ODM_LARGE_DOC_MB 

# Benchmark features with Typescript SSDK
cd packages/guardiandb-odm
npm run bench -- --mode=runAll --docs=10000 --batch-size=1000 --queries=2500 --updates=2500
npm run bench -- --mode=large --large-mb=17


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

## Community & Support

<div align="left">
<img align="right" src="docs/guardian-github-1.png" height="96px"/>

GuardianDB is open source and welcomes contributions from anyone interested in
decentralized systems, Iroh, and Rust. Join the
[Discord](https://discord.gg/Ezzk8PnGR5) to ask questions and share what you build, and
follow updates on [Twitter](https://x.com/willsearch_) and
[LinkedIn](https://www.linkedin.com/company/willsearch/).

- **Issues** — bugs and feature requests: [GitHub Issues](https://github.com/wmaslonek/guardian-db/issues)
- **Discussions** — Q&A and design: [GitHub Discussions](https://github.com/wmaslonek/guardian-db/discussions)
- **Code** — see [CONTRIBUTING.md](CONTRIBUTING.md)

</div>

## Status

GuardianDB is in active development and there will be breaking changes. Resulting issues
are usually easy to fix, but there are no stability guarantees at this stage.

## License

Dual-licensed under the [MIT](./LICENSE-MIT) and [Apache 2.0](./LICENSE-APACHE) licenses.
Opening a pull request is assumed to signal agreement with these terms.

## Acknowledgments

- **[Iroh](https://github.com/n0-computer/iroh)** — QUIC-based P2P data synchronization.
- **[ipfs-log-rs](https://github.com/eqlabs/ipfs-log-rs)** — CRDT log foundation, MIT
  © EQLabs. GuardianDB builds on it with significant enhancements for decentralized apps.

---

**GuardianDB** - A secure, performant, and fully decentralized peer-to-peer database for the modern Web.
