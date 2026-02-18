# guardian-db Storage Architecture - February 2026

guardian-db uses two storage layers with distinct purposes: **Sled** and **iroh-blobs (FsStore)**. They do not compete — each one has a clear role in the architecture.

---

## 1. Sled — Local Cache and Datastore

**Code location:** `src/cache/mod.rs` → `SledDatastore`

Sled is an embedded database (key-value) used as a local cache and operational datastore for the stores.

### Responsibilities

- Implements the `Datastore` trait (defined in `src/data_store.rs`), which provides CRUD operations: `get`, `put`, `has`, `delete`, `query`, `list_keys`
- Stores the **operational state** of stores: replication heads (`_remoteHeads`), sync state (`sync_state`), access data, etc.
- Used as the `cache` within `BaseStore` — every store (`KeyValueStore`, `EventLogStore`, `DocumentStore`) has an `Arc<dyn Datastore>` which defaults to a `SledDatastore`
- Supports prefix queries, pagination (offset/limit), and ordering (ascending/descending)

### Operation Modes

| Mode | Description |
|------|-------------|
| **Persistent** | Data written to disk at the specified path |
| **In-Memory** | Temporary data, lost on shutdown (path `:memory:` or `CacheMode::InMemory`) |

### Usage Example

```rust
// Saving heads to Sled cache
cache.put("_remoteHeads".as_bytes(), &heads_bytes).await?;

// During reset, flush Sled to disk
if let Some(sled_cache) = cache.as_any().downcast_ref::<SledDatastore>() {
    sled_cache.close(); // flush to disk
}

// Query with prefix and pagination
let query = QueryBuilder::new()
    .prefix("/users")
    .limit(10)
    .offset(0)
    .build();
let results = cache.query(&query).await?;
```

---

## 2. iroh-blobs (FsStore) — Content-Addressed P2P Storage

**Code location:** `src/p2p/network/core/blobs.rs` → `BlobStore` and `src/p2p/network/core/mod.rs` → `IrohBackend`

iroh-blobs is the **content-addressed storage** for data that needs to be **shared and replicated across peers**.

### Responsibilities

- Uses `FsStore` (filesystem-based) initialized in the `<data_dir>/iroh_store/` directory
- Stores **blobs** identified by BLAKE3 hashes — each document/data item is an immutable blob
- Provides operations: `add_document`, `get_document`, `has_document`, `delete_document`, `list_documents`
- Uses **tags** to protect blobs against garbage collection (format: `doc_<hash_hex>`)
- Shared with Iroh protocols for P2P communication

### Integrated Protocols

| Protocol | Function |
|----------|----------|
| **BlobsProtocol** | Efficient blob transfer between peers via QUIC |
| **Docs (iroh-docs)** | Distributed KV store that stores its values as blobs |
| **Gossip (iroh-gossip)** | Pub/sub protocol for message and head exchange between peers |

### Initialization Flow (`IrohBackend::initialize_node()`)

```
FsStore::load("iroh_store/")
    ├── BlobsProtocol::new(fs_store, endpoint)   → P2P blob transfer
    ├── Docs::persistent("iroh_docs/")            → distributed KV
    └── Gossip::spawn(endpoint)                   → pub/sub for head sync
```

### Usage Example

```rust
// Adding a document to the blob store
let hash = blob_store.add_document(data).await?;

// Retrieving by BLAKE3 hash
let data = blob_store.get_document(&hash).await?;

// Checking existence
let exists = blob_store.has_document(&hash).await?;
```

---

## 3. Architecture Overview

```
┌──────────────────────────────────────────────────────────┐
│                      BaseStore                            │
│   (KeyValueStore / EventLogStore / DocumentStore)         │
│                                                           │
│   ┌───────────────┐       ┌────────────────────────────┐ │
│   │  Sled Cache    │       │     IrohClient (P2P)       │ │
│   │  (local ops)   │       │                            │ │
│   │                │       │  ┌──────────────────────┐  │ │
│   │ • heads cache  │       │  │ iroh-blobs / FsStore │  │ │
│   │ • sync state   │       │  │ (content-addressed)  │  │ │
│   │ • query index  │       │  └──────────────────────┘  │ │
│   │ • local KV     │       │  ┌──────────────────────┐  │ │
│   └───────────────┘       │  │  Gossip Protocol     │  │ │
│                            │  │  (head exchange)     │  │ │
│   OpLog (in-memory)        │  └──────────────────────┘  │ │
│   • entries / operations   │  ┌──────────────────────┐  │ │
│   • CRDT merge (join)      │  │  iroh-docs (KV)      │  │ │
│                            │  │  (distributed docs)  │  │ │
│                            │  └──────────────────────┘  │ │
│                            └────────────────────────────┘ │
└──────────────────────────────────────────────────────────┘
```

---

## 4. Data Flows

### Write Flow

1. Operation arrives at `BaseStore` → creates `Entry` in `OpLog` (memory)
2. Heads are serialized and saved to the **Sled cache** (`_remoteHeads`)
3. Heads are published via **Gossip** to connected peers
4. Large data (documents/blobs) goes to **iroh-blobs/FsStore**

### Synchronization Flow (new peer joins)

1. Peer receives heads via **Gossip** (pub/sub)
2. `MessageMarshaler` deserializes the `MessageExchangeHeads`
3. `BaseStore::sync()` performs merge (CRDT join) on the local OpLog
4. Referenced blobs are transferred via **BlobsProtocol** (QUIC)
5. Local Sled cache is updated with the new state

---

## 5. Dependencies (Cargo.toml)

| Crate | Version | Role |
|-------|---------|------|
| `sled` | 0.34.7 | Embedded key-value database for local cache |
| `iroh` | 0.92.0 | P2P endpoint, discovery (Pkarr/DNS/mDNS), QUIC |
| `iroh-blobs` | 0.94.0 | Content-addressed storage (BLAKE3) |
| `iroh-gossip` | 0.92.0 | Pub/sub protocol for head exchange |
| `iroh-docs` | 0.92.0 | Distributed KV store over blobs |

---

## 6. Summary

| Component | Purpose | Scope |
|-----------|---------|-------|
| **Sled** | Fast local storage, operational cache, queries | Local to the node |
| **iroh-blobs** | Content-addressed storage for P2P replication | Distributed across peers |
| **Gossip** | Metadata (heads) exchange between peers | P2P communication |
| **iroh-docs** | Distributed KV store (uses blobs internally) | Distributed across peers |
| **OpLog** | In-memory operation log with CRDT merge | Local to the node (in-memory) |
