# DocumentStore Migration Guide — February 2026

This document describes the migration strategy for `GuardianDBDocumentStore` from the
current `BaseStore + OpLog` architecture to `iroh-docs (WillowDocs)`, including the role
of `iroh-blobs` and when each protocol should be used.

---

## 1. Current Architecture

`GuardianDBDocumentStore` currently wraps `BaseStore`, which uses an ipfs-log-style
append-only OpLog. Every `put(document)` creates a new immutable `Entry` in the log.
The "current" state is derived by replaying the entire log and applying PUT/DEL/PUTALL
operations in order via `DocumentIndex::update_index()`.

```
GuardianDBDocumentStore
  ├── BaseStore
  │     ├── OpLog (in-memory CRDT log)
  │     ├── Sled Cache (local operational state)
  │     └── IrohClient (P2P: gossip + blobs)
  ├── DocumentIndex (HashMap<String, Vec<u8>> rebuilt from log replay)
  └── CreateDocumentDBOptions (marshal/unmarshal/key_extractor)
```

### Problems with the current approach

| Concern | Current (OpLog) | Target (iroh-docs) |
|---------|-----------------|---------------------|
| PUT semantics | Appends a new entry to a grow-only log | Overwrites the key's value (LWW) |
| Read performance | Requires index rebuild from full log replay | Direct KV lookup |
| Network sync | Gossip broadcasts entire OpLog heads | Willow reconciles only changed ranges |
| Conflict resolution | CRDT merge (correct for logs, overkill for documents) | Last-Write-Wins (correct for mutable documents) |
| Disk usage over time | Grows forever (all historical writes kept) | Only current values stored |
| Index consistency | In-memory HashMap can drift from log | iroh-docs IS the authoritative state |

---

## 2. iroh-docs vs iroh-blobs — Role of Each Protocol

### How iroh-docs works internally

When you call `doc.set_bytes(author, key, value)`, iroh-docs internally:

1. Stores the **value bytes** as a blob in iroh-blobs (content-addressed, BLAKE3 hash)
2. Records in the Willow document the entry `(author, key, hash, timestamp)`
3. Willow sync exchanges **metadata only** (author, key, hash, timestamp)
4. iroh-blobs handles the actual **byte transfer** via BlobsProtocol over QUIC

**iroh-blobs is already involved implicitly** — you don't need to call it manually for
normal document operations.

### When to use each protocol

| Scenario | iroh-docs (WillowDocs) | iroh-blobs (BlobStore) direct |
|----------|------------------------|-------------------------------|
| JSON documents (~KB) | **Sufficient** — `set_bytes()` handles everything | Not needed |
| Small-medium documents (~10-100 KB) | **Sufficient** — Willow manages sync efficiently | Not needed |
| Large binary payloads (~MB+) | Works but suboptimal — Willow sync overhead | **Preferred** — streaming QUIC, range requests |
| Binary attachments (images, PDFs) | Not ideal — LWW on entire entry | **Correct** — hash as reference, bytes via blobs |
| Content deduplication | Limited to the namespace | **Native** — same hash = same blob globally |
| Resume/partial download | Not supported | **Supported** — iroh-blobs range requests |

### Decision matrix for the DocumentStore

```
Is the document a normal JSON object (< ~100 KB)?
  └─ YES → iroh-docs alone (blobs implicit)
       doc.set_bytes(author, key, serialized_json)

Does the document contain or reference large binary data?
  └─ YES → iroh-docs + iroh-blobs direct (hash reference pattern)
       blob_store.add_document(binary_data) → hash
       doc.set_bytes(author, key, json_with_hash_reference)
```

---

## 3. Target Architecture

### For standard JSON documents (majority of use cases)

iroh-docs alone is sufficient. The iroh-blobs layer is used internally by iroh-docs
and does not need to be called directly.

```
GuardianDBDocumentStore (migrated)
  │
  ├─ WillowDocs (iroh-docs) as KV backend
  │     • put(key, doc_json) → doc.set_bytes(author, key, serialized_json)
  │     • get(key) → doc.get_one(Query::key_exact(key)) → read_to_bytes()
  │     • delete(key) → doc.del(author, key)
  │     • query(filter) → doc.get_many(Query::all()) + filter in app layer
  │     • Automatic sync via Willow range reconciliation
  │
  └─ iroh-blobs (used internally by iroh-docs, NOT called directly)
```

### For documents with large binary attachments (optional future extension)

```
GuardianDBDocumentStore (with large attachment support)
  │
  ├─ WillowDocs (iroh-docs) — stores document metadata + hash references
  │     • doc.set_bytes(author, "doc_123", json_with_blob_refs)
  │     • JSON: { "_id": "doc_123", "name": "report", "attachment_hash": "<blake3>" }
  │
  └─ BlobStore (iroh-blobs) — stores large binary payloads directly
        • blob_store.add_document(pdf_bytes) → hash
        • blob_store.get_document(&hash) → pdf_bytes
        • Hash stored as reference in the iroh-docs entry
```

---

## 4. The `log` Module — What Gets Disconnected

The `src/log/` module implements the ipfs-log CRDT. The DocumentStore currently depends on it
in two layers: **directly** in its own files, and **indirectly** through `BaseStore`.

### Direct dependencies in `document_store/mod.rs` and `document_store/index.rs`

| Usage | File | Fate after migration |
|-------|------|----------------------|
| `use crate::log::identity::Identity` | `mod.rs` line 4 | **Keep** — `Identity` is still needed to map to `AuthorId` |
| `use crate::stores::operation::{self, Operation}` | `mod.rs` line 9 | **Remove** — no more OpLog operations |
| `Operation::new(...)` in `put()`, `delete()` | `mod.rs` | **Remove** — replaced by `set_bytes()` / `del()` |
| `operation::parse_operation(entry)` return value | `mod.rs` | **Remove** — no Entry to parse; method returns hash directly |
| `Operation::new_with_documents(...)` in `put_all()` | `mod.rs` | **Remove** — replaced by loop of `set_bytes()` |
| `crate::log::entry::Entry` in `Store` trait methods | `mod.rs` lines 84, 89, 123–124 | **Keep as stub** — `Store` trait requires them; implement as no-ops |
| `crate::log::Log` in `op_log()` | `mod.rs` line 100 | **Keep as stub** — `Store` trait requires it; return an empty log |
| `_log: &crate::log::Log` in `update_index()` | `index.rs` line 108 | **Remove** — entire `index.rs` is deleted |
| `entries: &[crate::log::entry::Entry]` in `update_index()` | `index.rs` line 109 | **Remove** — entire `index.rs` is deleted |
| `crate::stores::operation::parse_operation` in `update_index()` | `index.rs` line 119 | **Remove** — entire `index.rs` is deleted |

### Indirect dependencies via `BaseStore` (also removed)

`BaseStore` owns the `OpLog` (`Arc<parking_lot::RwLock<Log>>`) and uses essentially
the entire `log` module:

| `BaseStore` component | Uses from `log` | Fate |
|-----------------------|-----------------|------|
| `OpLog` (CRDT log) | `Log`, `LogOptions`, `Entry`, `LamportClock` | **Removed** — `BaseStore` itself is removed from DocumentStore |
| `add_operation()` | `Entry`, identity provider chain | **Removed** |
| `update_index()` / `sync_index_with_log()` | `Log`, `Entry` | **Removed** |
| Gossip head exchange | Serialized `Entry` heads | **Removed** |

### What `log` the module keeps serving (in other stores)

The `log` module is **not deleted**. It remains fully in use by:

- **`EventLogStore`** — append-only log semantics are correct there; OpLog + gossip stays
- **`BaseStore`** — still used by `EventLogStore`
- **`operation/`** module — still used by `EventLogStore` and `KeyValueStore` (until KV migration)
- **`Identity`** type itself — still imported by the migrated DocumentStore to create `AuthorId`

### The `Store` trait boundary — why some stubs are necessary

The `Store` trait in `src/traits.rs` exposes several methods that return `log` types:

```rust
// In trait Store:
async fn sync(&self, heads: Vec<Entry>) -> Result<(), Self::Error>;
async fn load_more_from(&self, amount: u64, entries: Vec<Entry>);
fn op_log(&self) -> Arc<RwLock<Log>>;
async fn add_operation(&self, op: Operation, ...) -> Result<Entry, Self::Error>;
```

These exist because `EventLogStore` needs them. After the DocumentStore migration, the
`GuardianDBDocumentStore` will implement them as **no-ops or empty returns**:

```rust
// Stubs in migrated GuardianDBDocumentStore — these are meaningless for iroh-docs
// but required by the trait boundary
async fn sync(&self, _heads: Vec<Entry>) -> Result<(), Self::Error> {
    Ok(()) // Willow handles sync automatically — no manual head exchange needed
}
fn op_log(&self) -> Arc<RwLock<Log>> {
    Arc::new(RwLock::new(Log::empty())) // DocumentStore has no OpLog after migration
}
async fn add_operation(&self, _op: Operation, ...) -> Result<Entry, Self::Error> {
    Err(GuardianError::Other("DocumentStore does not use OpLog operations".into()))
}
```

If the heterogeneous `Store` trait becomes a maintenance burden after both DocumentStore
and KVStore are migrated, a future refactor could split it into `EventLogStore`-specific
methods vs shared methods — but that is **out of scope** for this migration.

---

## 5. Components to Disconnect from BaseStore

The following `BaseStore` components must be **removed or bypassed** for the migrated
DocumentStore:

| Component | Action | Reason |
|-----------|--------|--------|
| `OpLog` (in-memory CRDT log) | **Remove** | iroh-docs manages its own state via Willow |
| Gossip pub/sub for heads | **Remove** | Willow sync replaces gossip-based head exchange |
| `DocumentIndex` (HashMap from log replay) | **Remove** | iroh-docs is the authoritative KV state |
| `update_index()` / `sync_index_with_log()` | **Remove** | No log to replay — iroh-docs is the source of truth |
| `Sled cache` | **Keep (optional)** | Can serve as local read cache for hot keys |
| `AccessController` | **Keep** | Must validate writes before they reach iroh-docs |
| `EventBus` | **Keep** | Reactive events for UI / observers |
| `Identity` → `AuthorId` mapping | **Keep** | iroh-docs needs an `AuthorId` per node |
| `CreateDocumentDBOptions` | **Keep (adapted)** | marshal/unmarshal/key_extractor still needed |

---

## 5. Migrated Struct — `GuardianDBDocumentStore`

The existing struct in `src/stores/document_store/mod.rs` is updated in-place.
`BaseStore`, `DocumentIndex`, and all OpLog-related fields are replaced by the
iroh-docs backend. The public API (`put`, `get`, `delete`, `query`, `put_all`) remains
unchanged — only the internal implementation changes.

```rust
pub struct GuardianDBDocumentStore {
    /// iroh-docs backend (Willow protocol) — replaces BaseStore
    willow_docs: WillowDocs,
    /// The iroh-docs document for this store's namespace — replaces DocumentIndex
    doc_handle: Arc<Doc>,
    /// Author ID mapped from Identity
    author_id: AuthorId,
    /// Access control enforcement — kept from current implementation
    access_controller: Arc<dyn AccessController>,
    /// Reactive event emission — kept from current implementation
    event_bus: Arc<EventBus>,
    /// Store address (db name + type) — kept from current implementation
    address: Arc<dyn Address + Send + Sync>,
    /// Document serialization options (marshal, unmarshal, key_extractor)
    doc_opts: CreateDocumentDBOptions,
    /// Optional: BlobStore for large binary attachments
    blob_store: Option<Arc<BlobStore>>,
    /// Tracing span
    span: Span,
}
```

**Fields removed from the current struct:**
- `base_store: Arc<BaseStore>` → replaced by `willow_docs` + individual fields
- `doc_index: Arc<DocumentIndex>` → replaced by iroh-docs queries
- `cached_address: Arc<dyn Address>` → now stored directly as `address`

---

## 6. API Mapping — Current vs Migrated

### `put(document)`

**Current:**
```rust
// 1. Extract key and serialize
let key = (self.doc_opts.key_extractor)(&document)?;
let data = (self.doc_opts.marshal)(&document)?;
// 2. Create OpLog operation
let op = Operation::new(Some(key), "PUT", Some(data));
// 3. Add to OpLog (creates Entry, broadcasts via gossip)
let entry = self.base_store.add_operation(op, None).await?;
// 4. Update in-memory index
self.base_store.update_index()?;
self.doc_index.put(key, data)?;
```

**Migrated:**
```rust
// 1. Access control check
self.access_controller.can_append(&identity, &self.address)?;
// 2. Extract key and serialize
let key = (self.doc_opts.key_extractor)(&document)?;
let data = (self.doc_opts.marshal)(&document)?;
// 3. Write directly to iroh-docs (Willow handles sync)
let hash = self.willow_docs.set_bytes(&self.doc_handle, self.author_id, &key, data).await?;
// 4. Emit event for reactive listeners
self.event_bus.emit(StoreEvent::Write { key, hash });
```

### `get(key)`

**Current:**
```rust
// 1. Search in-memory DocumentIndex (HashMap)
for index_key in doc_index.keys() {
    if matches(index_key, key) {
        let value_bytes = doc_index.get_bytes(&index_key);
        let doc = serde_json::from_slice(&value_bytes)?;
        documents.push(doc);
    }
}
```

**Migrated:**
```rust
// 1. Query iroh-docs directly (no in-memory index needed)
let query = if opts.partial_matches {
    Query::key_prefix(key)
} else {
    Query::key_exact(key)
};
let entries = self.willow_docs.get_many(&self.doc_handle, query).await?;
// 2. Read blob content for each entry
for entry in entries {
    let bytes = self.doc_handle.read_to_bytes(&entry).await?;
    let doc = (self.doc_opts.unmarshal)(&bytes)?;
    documents.push(doc);
}
```

### `delete(key)`

**Current:**
```rust
// 1. Check existence in DocumentIndex
if doc_index.get_bytes(document_id).is_none() {
    return Err(NotFound);
}
// 2. Create DEL operation in OpLog
let op = Operation::new(Some(key), "DEL", None);
let entry = self.base_store.add_operation(op, None).await?;
// 3. Update in-memory index
self.doc_index.remove(document_id)?;
```

**Migrated:**
```rust
// 1. Check existence in iroh-docs
let existing = self.willow_docs.get_one(&self.doc_handle, Query::key_exact(key)).await?;
if existing.is_none() {
    return Err(NotFound);
}
// 2. Delete directly from iroh-docs (Willow handles sync)
let count = self.willow_docs.del(&self.doc_handle, self.author_id, key).await?;
// 3. Emit event
self.event_bus.emit(StoreEvent::Delete { key });
```

### `query(filter)`

**Current:**
```rust
// Iterate over in-memory DocumentIndex, deserialize each, apply filter
for index_key in doc_index.keys() {
    let doc_bytes = doc_index.get_bytes(&index_key);
    let doc = serde_json::from_slice(&doc_bytes)?;
    if filter(&doc)? {
        results.push(doc);
    }
}
```

**Migrated:**
```rust
// Get all entries from iroh-docs, deserialize each, apply filter
let entries = self.willow_docs.get_many(&self.doc_handle, Query::all()).await?;
for entry in entries {
    let bytes = self.doc_handle.read_to_bytes(&entry).await?;
    let doc = (self.doc_opts.unmarshal)(&bytes)?;
    if filter(&doc)? {
        results.push(doc);
    }
}
```

### `put_all(documents)`

**Current:**
```rust
// Creates a single PUTALL operation with all documents bundled
let op = Operation::new_with_documents(None, "PUTALL", to_add);
let entry = self.base_store.add_operation(op, None).await?;
```

**Migrated:**
```rust
// Each document is a separate set_bytes call (iroh-docs has no batch API)
// This is correct because each key is independent in the Willow model
for (key, data) in to_add {
    self.willow_docs.set_bytes(&self.doc_handle, self.author_id, &key, data).await?;
}
self.event_bus.emit(StoreEvent::BatchWrite { count: to_add.len() });
```

---

## 7. Synchronization — Before vs After

### Current sync flow

```
Peer A writes document
  → OpLog.append(Entry with PUT operation)
  → Gossip broadcasts OpLog heads to all peers
  → Peer B receives heads via gossip
  → Peer B merges heads (CRDT join) into local OpLog
  → Peer B replays entire OpLog to rebuild DocumentIndex
  → Peer B now has the updated document in its in-memory HashMap
```

### Migrated sync flow

```
Peer A writes document
  → doc.set_bytes(author, key, value) stores blob + metadata in iroh-docs
  → Willow protocol detects new entry
  → Willow range reconciliation syncs only the changed key(s) to Peer B
  → iroh-blobs transfers the value bytes via QUIC (if not already present)
  → Peer B's iroh-docs now has the entry
  → Peer B reads directly from iroh-docs (no index rebuild needed)
```

**Key improvement:** No full log replay, no gossip head exchange, no in-memory index
rebuild. Willow syncs only what changed, and iroh-docs is always the source of truth.

---

## 8. Migration Phases

The migration is done **in-place** inside `src/stores/document_store/mod.rs` and
`src/stores/document_store/index.rs`. No new files or parallel structs are created.

### Phase 1 — Replace Struct Fields

Edit `GuardianDBDocumentStore` directly:

1. Remove `base_store: Arc<BaseStore>` and `doc_index: Arc<DocumentIndex>`
2. Add `willow_docs: WillowDocs`, `doc_handle: Arc<Doc>`, `author_id: AuthorId`
3. Move `access_controller`, `event_bus`, `address` from BaseStore delegation to direct fields
4. Update `new()` to initialize `WillowDocs` and create/open the iroh-docs document
   instead of calling `BaseStore::new()`

```rust
// new() — before
let base_store = BaseStore::new(client, identity, addr, Some(options)).await?;

// new() — after
let mut willow_docs = WillowDocs::new(backend.clone()).await?;
let author_id = willow_docs.get_or_init_author().await?;
let doc_handle = willow_docs.open_doc(namespace_id).await?
    .unwrap_or(willow_docs.create_doc().await?);
```

### Phase 2 — Rewrite Method Bodies

Update each method body in-place (see Section 6 for the before/after mapping):

- `put()` — remove `add_operation` + `update_index`, replace with `set_bytes`
- `get()` — remove HashMap iteration, replace with `get_many` + `read_to_bytes`
- `delete()` — remove OpLog DEL operation, replace with `del`
- `query()` — remove DocumentIndex iteration, replace with `get_many` + filter
- `put_all()` — remove PUTALL operation, replace with loop of `set_bytes`

### Phase 3 — Remove Dead Code

Once all methods are migrated and tests pass:

- Delete `src/stores/document_store/index.rs` (DocumentIndex no longer needed)
- Remove `pub mod index;` from `mod.rs`
- Remove `Operation`, `parse_operation`, and OpLog-related imports from `mod.rs`
- Keep `BaseStore` + OpLog untouched for `EventLogStore`

---

## 9. What About the Existing DocumentIndex?

The current `DocumentIndex` (`src/stores/document_store/index.rs`) maintains an in-memory
`HashMap<String, Vec<u8>>` that is rebuilt by replaying OpLog entries.

After migration, this index is **no longer needed** because:
- iroh-docs IS the index — `Query::key_exact()` and `Query::key_prefix()` provide direct lookup
- No log replay is required — Willow maintains consistent state automatically
- The `StoreIndex` trait implementation can delegate directly to iroh-docs queries

The `DocumentIndex` struct and its `StoreIndex` implementation can be removed once the
migration is complete.

---

## 10. iroh-blobs Direct Usage — When and How

For the standard DocumentStore migration, **iroh-blobs is NOT called directly**. It is
used internally by iroh-docs.

If a future requirement adds support for documents with large binary attachments, the
pattern would be:

```rust
impl WillowDocsDocumentStore {
    /// Stores a document with a large binary attachment
    pub async fn put_with_attachment(
        &self,
        document: Document,
        attachment: Bytes,
    ) -> Result<Operation> {
        let blob_store = self.blob_store.as_ref()
            .ok_or(GuardianError::Other("BlobStore not configured"))?;

        // 1. Store binary attachment in iroh-blobs
        let attachment_hash = blob_store.add_document(attachment).await?;

        // 2. Add hash reference to document metadata
        let mut doc = document;
        doc["_attachment_hash"] = serde_json::json!(hex::encode(attachment_hash.as_bytes()));

        // 3. Store document (with hash reference) in iroh-docs
        let key = (self.doc_opts.key_extractor)(&doc)?;
        let data = (self.doc_opts.marshal)(&doc)?;
        self.willow_docs.set_bytes(&self.doc_handle, self.author_id, &key, data).await?;

        Ok(...)
    }

    /// Retrieves a document and its binary attachment
    pub async fn get_with_attachment(
        &self,
        key: &str,
    ) -> Result<(Document, Option<Bytes>)> {
        // 1. Get document from iroh-docs
        let entry = self.willow_docs.get_one(&self.doc_handle, Query::key_exact(key)).await?;
        let bytes = self.doc_handle.read_to_bytes(&entry).await?;
        let doc: Document = (self.doc_opts.unmarshal)(&bytes)?;

        // 2. If document has attachment hash, fetch from iroh-blobs
        let attachment = if let Some(hash_hex) = doc.get("_attachment_hash").and_then(|v| v.as_str()) {
            let blob_store = self.blob_store.as_ref()
                .ok_or(GuardianError::Other("BlobStore not configured"))?;
            let hash_bytes = hex::decode(hash_hex)?;
            let hash = iroh_blobs::Hash::from_bytes(hash_bytes.try_into()?);
            Some(blob_store.get_document(&hash).await?)
        } else {
            None
        };

        Ok((doc, attachment))
    }
}
```

This pattern keeps the separation of concerns:
- **iroh-docs** stores document metadata and small JSON payloads
- **iroh-blobs** stores large binary content, referenced by hash
- The DocumentStore API abstracts both behind a unified interface

---

## 11. Store Type vs Protocol — Final Mapping

```
┌────────────────────────┬──────────────────────────────────────────────────┐
│ Store Type             │ Protocol Backend                                 │
├────────────────────────┼──────────────────────────────────────────────────┤
│ EventLogStore          │ OpLog (ipfs-log CRDT) + iroh-gossip (heads)     │
│                        │ + iroh-blobs (large payload references)         │
├────────────────────────┼──────────────────────────────────────────────────┤
│ KeyValueStore          │ TARGET: iroh-docs WillowDocs (LWW KV)           │
├────────────────────────┼──────────────────────────────────────────────────┤
│ DocumentStore          │ TARGET: iroh-docs WillowDocs (LWW KV)           │
│                        │ + iroh-blobs direct (only for large attachments)│
├────────────────────────┼──────────────────────────────────────────────────┤
│ File transfers (chat)  │ TARGET: iroh-blobs (hash ref in EventLog)       │
└────────────────────────┴──────────────────────────────────────────────────┘
```

---

## 12. References

| File | Relevance |
|------|-----------|
| `src/stores/document_store/mod.rs` | Current DocumentStore implementation (BaseStore-backed) |
| `src/stores/document_store/index.rs` | DocumentIndex (in-memory HashMap, to be removed) |
| `src/stores/base_store/mod.rs` | BaseStore core: OpLog, gossip heads, Sled cache |
| `src/p2p/network/core/docs.rs` | `WillowDocs` — iroh-docs wrapper (migration target) |
| `src/p2p/network/core/blobs.rs` | `BlobStore` — iroh-blobs wrapper |
| `src/p2p/network/client.rs` | `IrohClient` — exposes `docs_client`, `blobs_client` |
| `docs/PROTOCOL_RESPONSIBILITIES.md` | Protocol responsibility matrix and KV migration guide |
| `docs/STORAGE_ARCHITECTURE.md` | Storage layer overview (Sled + iroh-blobs) |
