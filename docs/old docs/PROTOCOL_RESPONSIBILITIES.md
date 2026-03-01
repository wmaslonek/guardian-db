# Protocol Responsibilities & Migration Guide — February 2026

This document consolidates the architectural decisions around the three Iroh protocols used in
guardian-db (`iroh-gossip`, `iroh-blobs`, `iroh-docs`) and defines the correct responsibility
boundary for each one. It also covers the KV store migration path to `WillowDocs` and the
correct approach for file transfers in the P2P chat.

---

## 1. Protocol Responsibility Matrix

| Protocol | Correct Responsibility | Current State |
|----------|------------------------|---------------|
| **iroh-gossip** | Ephemeral metadata: CRDT heads exchange, typing indicators, presence | ✅ Correct — gossip carries only heads and ~80-byte file hash references |
| **iroh-blobs** | Content-addressed storage + P2P file transfer (QUIC, streaming) | ✅ Wired into chat file transfers via `BlobStore` |
| **iroh-docs** | Mutable distributed KV with Last-Write-Wins (Willow sync) | ✅ Connected to `GuardianDBKeyValue` and `GuardianDBDocumentStore` |

---

## 2. iroh-gossip — What It Should and Should Not Do

### ✅ Keep (correct uses)

- **Head exchange** between peers (`exchange_heads()` in `src/stores/base_store/mod.rs`)  
  Gossip is ideal: small payloads (~100–500 bytes), broadcast semantics, no delivery guarantee needed (CRDT is self-healing).
- **Ephemeral signals**: "user is typing", online/offline presence, group membership events.
- **New peer notifications**: signalling that a peer joined a topic so `on_new_peer_joined()` can trigger head sync.

### ❌ Remove (incorrect uses) — RESOLVED

- **File transfers via the chat** — Previously `p2p_chat_tui.rs`:
  1. Read the entire file from disk
  2. Base64-encoded it (~33% size inflation)
  3. Embedded it inside a `FileAttachment` struct
  4. Serialized the struct into a `ChatMessage`
  5. Added the `ChatMessage` as an `Entry` into the `EventLogStore` OpLog
  6. OpLog entry was broadcast to **all** peers via gossip

  **Now fixed**: File transfers use `iroh-blobs` via `BlobStore`. Only a ~80-byte
  `file://<hash>|<name>|<size>` reference is stored in the EventLog. Actual file bytes
  are stored/retrieved via `BlobStore.add_document()` / `get_document()`.
  `ChatMessage` serialization migrated from JSON to postcard.

---

## 3. iroh-blobs — The Correct File Transfer Backend

`iroh-blobs` is already fully implemented in `src/p2p/network/core/blobs.rs` as `BlobStore`.
The protocol is registered in the QUIC router at node initialization (`IrohBackend::initialize_node()`).

### Correct file transfer flow

```
Sender side:
  1. Read file from disk
  2. blob_store.add_document(file_bytes).await  →  returns BLAKE3 Hash
  3. Create ChatMessage { message_type: "file", content: "<hash>|<filename>|<size>" }
     (NO file bytes — only the hash reference, ~80 bytes)
  4. EventLogStore::add(chat_message_bytes)
     (gossip distributes only the metadata entry)

Receiver side:
  1. Receive ChatMessage via gossip head sync
  2. Parse hash from content field
  3. blob_store.get_document(&hash).await  →  streams file bytes via QUIC
     (BlobsProtocol handles P2P transfer, range requests, integrity verification)
  4. Save to received_files/
```

### Benefits over the current base64-in-gossip approach

| Property | Current (base64 in gossip) | Correct (iroh-blobs) |
|----------|---------------------------|----------------------|
| Size limit | 5 MB (artificial) | No limit (streaming) |
| Payload per peer | Full file for every peer | Only downloaded by peers who request it |
| Deduplication | None | BLAKE3 hash = same file stored once |
| Integrity check | None | Bao encoding verifies every chunk |
| OpLog bloat | Full file bytes in every entry | ~80-byte hash reference only |
| Resume support | No | Yes (iroh-blobs range requests) |

### Migration in `p2p_chat_tui.rs` — COMPLETED

The chat now uses:
- `BlobStore.add_document(Bytes)` to store file bytes, returning a BLAKE3 hash
- `FileReference { blob_hash, file_name, file_size }` as the metadata struct
- `ChatMessage.content = "file://<hash>|<name>|<size>"` (~80 bytes)
- `BlobStore.get_document(&hash)` to retrieve file bytes on the receiver side
- `ChatMessage::to_bytes()` / `from_bytes()` use `postcard` instead of `serde_json`
- No file size limit (streaming via iroh-blobs)
- Content deduplication via BLAKE3 hash (same file stored once)
- OpLog entries contain only the hash reference, not the file bytes

---

## 4. iroh-blobs — Called Directly or Through a Store?

### Answer: directly via `BlobStore`, accessed through `IrohClient`

`iroh-blobs` does **not** fit the guardian-db store model (EventLogStore / KVStore /
DocumentStore) and should **not** be wrapped in a new "FileStore" abstraction. Here is why:

### Why iroh-blobs is architecturally different from the Stores

| Property | guardian-db Stores | iroh-blobs |
|----------|--------------------|------------|
| Identity per write | Yes — `Identity` signs every `Entry` | No — the hash IS the identity |
| Access control per write | Yes — `AccessController.can_append()` | Implicit — you need the hash to fetch; who has the hash is controlled by the EventLog |
| Conflict resolution | CRDT merge (EventLog) or LWW (KVStore) | None needed — blobs are immutable |
| Sync protocol | Gossip heads / Willow | BlobsProtocol (QUIC, range requests, Bao verification) |
| History / audit | Preserved in OpLog | Not applicable — bytes are static |
| Mutability | Append-only or LWW | Immutable — same hash = same content, forever |

Adding a "FileStore" wrapper around `BlobStore` would just add indirection with zero benefit.
The CRDT/OpLog machinery would be meaningless for immutable, content-addressed bytes.

### The correct layered pattern for chat file transfers

```
┌─────────────────────────────────────────────────────────────┐
│  Layer 1 — Who can see the reference?                       │
│  EventLogStore (OpLog + gossip)                             │
│  ChatMessage { content: "file://<hash>|<name>|<size>" }     │
│  Access control: AccessController on the EventLog           │
└────────────────────────────┬────────────────────────────────┘
                             │ hash extracted from ChatMessage
┌────────────────────────────▼────────────────────────────────┐
│  Layer 2 — How to transfer the bytes?                       │
│  BlobStore (iroh-blobs) — called directly                   │
│  iroh_client.blobs_client().get_document(&hash).await       │
│  Transport: BlobsProtocol over QUIC (verified, streaming)   │
└─────────────────────────────────────────────────────────────┘
```

- **Layer 1 (EventLog)** controls authorization: only peers who can read the chat log
  receive the hash. Without the hash there is no way to fetch the file.
- **Layer 2 (BlobStore)** is pure mechanics: given a hash, retrieve or store bytes.
  No CRDT, no head exchange, no identity per write needed.

### Access path in code

`BlobStore` is accessed through `IrohClient`, which is already available everywhere a
`BaseStore` (and thus every Store type) is alive:

```rust
// Upload (sender)
let blob_store = iroh_client.blobs_client().await
    .ok_or("blobs not initialized")?;
let hash = blob_store.add_document(Bytes::from(file_bytes)).await?;

// Reference stored in EventLog (metadata only — ~80 bytes)
let reference = format!("file://{}|{}|{}", hash, file_name, file_size);
chat_event_log.add(reference.into_bytes()).await?;

// Download (receiver — triggered when ChatMessage is received from EventLog)
let hash = parse_hash_from_message(&msg)?;
let file_bytes = blob_store.get_document(&hash).await?;
fs::write(output_path, file_bytes)?;
```

### What is missing from BlobStore for production use

The current `BlobStore` API is complete for basic transfers. What could be added
**without creating a new Store abstraction**:

| Feature | Approach |
|---------|----------|
| Upload progress | Add `add_document_with_progress(data, tx: Sender<u64>)` to `BlobStore` |
| Download progress | iroh-blobs supports range requests; add streaming variant to `BlobStore` |
| Transfer registry | Keep a `Vec<FileTransfer>` in the app layer (already done in `p2p_chat_tui.rs`) |
| Size validation | Validate before calling `add_document()` — app layer responsibility |
| GC protection | Already handled by tags in `BlobStore::add_document()` |

None of these require a new guardian-db Store. They are either improvements to `BlobStore`
itself or responsibilities of the application layer.

### Why not route through a "FileStore" wrapping BaseStore?

If a `FileStore` were built on top of `BaseStore` (like `EventLogStore`), it would:
- Create an OpLog entry for every file upload — bloating the CRDT log with large binary entries
- Use gossip to broadcast file hashes to all peers — iroh-blobs already handles P2P transfer more efficiently via QUIC
- Add `AccessController.can_append()` on every write — unnecessary since the hash in EventLog already acts as the capability token
- Require CRDT merge logic for content that is by definition immutable and never conflicts

The separation of concerns is clear: **authorization** goes through the Store layer (EventLog
controls who gets the hash); **data transfer** goes directly through `BlobStore`.

---

## 5. iroh-docs (WillowDocs) — Where It Belongs

`iroh-docs` uses Willow range-based reconciliation for mutable distributed key-value storage
with Last-Write-Wins conflict resolution. This makes it the right protocol for **state that
changes over time**, as opposed to the EventLogStore's append-only CRDT log.

### Correct use cases for iroh-docs in the chat context

| Data | Why iroh-docs? |
|------|----------------|
| User profile (username, avatar) | Mutable; only current value matters; user edits overwrite |
| Contacts list | Mutable; additions/deletions; LWW is correct semantics |
| Group metadata (name, member list) | Mutable; small KV entries; sync across devices |
| Shared file catalog (hash → metadata index) | KV reference map; LWW for each key |

### What iroh-docs is NOT for

| Data | Why NOT iroh-docs? |
|------|--------------------|
| Chat message history | Append-only; full history must be preserved; CRDT merge is correct |
| File binary content | Use `iroh-blobs` for actual bytes |
| Ephemeral signals | Use `iroh-gossip` |

### The access path: via KV Store, not directly

The chat and application layer should **always** access iroh-docs through
`GuardianDBKeyValue`, not by calling `WillowDocs` directly.

```
❌ Wrong:  ChatApp → WillowDocs (iroh-docs) directly
✅ Correct: ChatApp → GuardianDBKeyValue → (today) BaseStore+OpLog
                                         → (after migration) WillowDocs internally
```

**Reasons to go through the KV store abstraction:**
1. `AccessController` enforcement — all writes are checked for permissions
2. `EventBus` events — UI can react to data changes reactively
3. Cryptographic identity — `Identity` is mapped to `AuthorId` consistently
4. API stability — when the internal backend changes, the chat code doesn't change
5. Migration transparency — the `put(key, value)` / `get(key)` API is identical before and after migration

---

## 6. KV Store Migration to WillowDocs

### Why the current KV store uses OpLog (and why that's wrong for KV)

`GuardianDBKeyValue` currently wraps `BaseStore`, which uses the same ipfs-log-style
append-only OpLog as `EventLogStore`. Every `put("username", "alice")` creates a new
immutable `Entry` in the log. The "current" value is derived by replaying the entire
log from the beginning and applying operations in order (via `sync_index_with_log()`).

This is **semantically wrong** for mutable KV:

| Concern | OpLog (current) | iroh-docs (target) |
|---------|-----------------|-------------------|
| PUT semantics | Appends a new entry to a grow-only log | Overwrites the key's value |
| Read performance | Requires index rebuild from full log replay | Direct KV lookup |
| Network sync | Gossip broadcasts entire OpLog heads | Willow reconciles only changed ranges |
| Conflict resolution | CRDT merge (correct for logs, overkill for KV) | Last-Write-Wins timestamp (correct for KV) |
| Disk usage over time | Grows forever (all historical writes kept) | Only current values stored |

### What must be disconnected

To connect `WillowDocs` to the KV store, the following `BaseStore` components must be
**removed or bypassed** for the KV store:

| Component | Action | Reason |
|-----------|--------|--------|
| `OpLog` (in-memory CRDT log) | **Remove** | iroh-docs manages its own state |
| Gossip pub/sub for heads | **Remove** | Willow sync replaces gossip-based head exchange |
| `KeyValueIndex` (HashMap rebuilt from log replay) | **Remove** | iroh-docs is the authoritative KV state |
| `Sled cache` | **Keep (optional)** | Can serve as local read cache |
| `AccessController` | **Keep** | Must validate writes before they reach iroh-docs |
| `EventBus` | **Keep** | Reactive events for UI / observers |
| `Identity` → `AuthorId` mapping | **Keep** | iroh-docs needs an `AuthorId` per node |

### What must NOT be disconnected in EventLogStore

The EventLogStore keeps the full OpLog + gossip architecture. Chat message history is
append-only by design — every message is a permanent immutable entry. CRDT merge
ensures eventual consistency even when peers rejoin with divergent histories.

```
EventLogStore → OpLog + gossip heads  ✅  (history = never overwritten)
KeyValueStore → iroh-docs WillowDocs  ✅  (profile = only current value matters)
DocumentStore → iroh-docs WillowDocs  ✅  (document = editable, only latest version)
```

### Proposed migration path

**Phase 1 — Parallel implementation (non-breaking)**

Create `WillowDocsKeyValue` as a new struct that directly holds `WillowDocs`
and implements the `KeyValueStore` trait without extending `BaseStore`:

```rust
pub struct WillowDocsKeyValue {
    docs: WillowDocs,           // iroh-docs backend
    doc_handle: Arc<Doc>,       // The iroh-docs document for this namespace
    author_id: AuthorId,        // Mapped from Identity
    access_controller: Arc<dyn AccessController>,
    event_bus: Arc<EventBus>,
    address: Arc<dyn Address + Send + Sync>,
}

impl KeyValueStore for WillowDocsKeyValue {
    async fn put(&self, key: &str, value: Vec<u8>) -> Result<Operation> {
        // access_controller check
        // docs.set_bytes(&doc_handle, author_id, key, value).await
        // event_bus.emit(EventWrite { ... })
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        // docs.get_one(&doc_handle, Query::key_exact(key)).await
    }
    // ...
}
```

**Phase 2 — Wire into GuardianDB factory**

```rust
// In guardian/core.rs
pub async fn keyvalue(&self, name: &str, options: Option<CreateDBOptions>) -> Result<...> {
    if options.use_willow_docs {
        WillowDocsKeyValue::new(self.iroh_client.clone(), name, options).await
    } else {
        GuardianDBKeyValue::new(self.iroh_client.clone(), ...).await  // existing path
    }
}
```

**Phase 3 — Cut over and deprecate**

Once `WillowDocsKeyValue` is validated in production, the `BaseStore`-backed
`GuardianDBKeyValue` can be deprecated and eventually removed.

---

## 7. Store Type vs Protocol Summary

```
┌────────────────────────────────────────────────────────────────────┐
│              Guardian-DB Store ↔ Protocol Mapping                  │
├──────────────────────┬─────────────────────────────────────────────┤
│ EventLogStore        │ OpLog (ipfs-log CRDT) + iroh-gossip (heads) │
│                      │ + iroh-blobs (large payload references)     │
├──────────────────────┼─────────────────────────────────────────────┤
│ KeyValueStore        │ ✅ iroh-docs WillowDocs (LWW KV)            │
├──────────────────────┼─────────────────────────────────────────────┤
│ DocumentStore        │ ✅ iroh-docs WillowDocs (LWW KV)            │
├──────────────────────┼─────────────────────────────────────────────┤
│ File transfers       │ ✅ iroh-blobs (hash ref in EventLog)        │
│ (chat)               │                                             │
└──────────────────────┴─────────────────────────────────────────────┘
```

---

## 8. References

| File | Relevance |
|------|-----------|
| `src/stores/event_log_store/mod.rs` | EventLogStore implementation using BaseStore + OpLog |
| `src/stores/kv_store/mod.rs` | Current KV store implementation (BaseStore-backed) |
| `src/stores/base_store/mod.rs` | BaseStore core: OpLog, gossip heads, Sled cache |
| `src/p2p/network/core/gossip.rs` | `EpidemicPubSub` — iroh-gossip wrapper |
| `src/p2p/network/core/blobs.rs` | `BlobStore` — iroh-blobs wrapper |
| `src/p2p/network/core/docs.rs` | `WillowDocs` — iroh-docs wrapper |
| `src/p2p/network/client.rs` | `IrohClient` — exposes `docs_client`, `blobs_client` |
| `examples/p2p_chat_tui.rs` | Chat demo — current file transfer via base64 in gossip |
| `docs/STORAGE_ARCHITECTURE.md` | Storage layer overview (Sled + iroh-blobs) |
