# Storage Architecture Analysis — guardian-db

> **Audience:** guardian-db developers  
> **Status:** Technical review — February 2026  
> **Pending decision:** Migration from Sled to redb

---

## Overview

guardian-db currently uses three persistence layers:

| Layer | Technology | Purpose |
|-------|-----------|---------|
| Local Cache/Datastore | **Sled** 0.34.7 | OpLog persistence, queries, operational state |
| P2P Storage | **iroh-blobs** (FsStore) | Content-addressed blobs, replication via QUIC |
| Local Backup (demo) | **JSON files** | Message history, contacts, profile (chat demo only) |

---

## What Is Working Well

- **Sled as local cache** — a fast embedded KV store for local operations (queries, OpLog state, replication heads) is the right design decision.
- **iroh-blobs for P2P replication** — content-addressed storage with BLAKE3 hashes and QUIC transfer is the ideal choice for distributed synchronization.
- **Gossip for heads exchange** — efficient for propagating lightweight metadata between peers without overhead.
- **Local vs. distributed separation** — the conceptual split between "local data" and "replicable data" is solid.

---

## Identified Problems

### 1. Triple Data Redundancy

The chat demo stores messages in 3 places simultaneously (Sled + iroh-blobs + JSON). This creates concrete risks:

- **Inconsistency** — messages may exist in one layer but not another.
- **Cleanup code** — `cleanup_mixed_messages()` exists in the demo to fix messages "leaked" between DM and group logs, a direct symptom of the redundancy.
- **Complexity** — every read operation needs to merge and deduplicate across sources.

**Example of the problem in code:**
```rust
// get_dm_messages() needs to manually merge 2 sources + deduplicate
let store_messages = log.list(None).await?;          // Sled via EventLogStore
let local = load_messages_from_file(&username, &log); // JSON backup

// Manual merge + deduplication by (timestamp, from_node_id, content)
for sm in &store_messages {
    let already_exists = local.iter().any(|lm| /* ... */);
    if !already_exists { local.push(sm.clone()); }
}
```

### 2. ⚠️ Sled Is Abandoned — Migration Required

> **ACTION REQUIRED:** Migrate from Sled to **redb**.

`sled 0.34.7` is the last version published in 2021. This poses risks to the project:

- No bug fixes or security vulnerability patches
- No support for new Rust compiler versions
- No guarantee of future compatibility with the ecosystem

### 3. Duplication Between Sled and iroh-blobs

The iroh-blobs `FsStore` already persists data to disk. Having Sled as *another* persistence layer for the **same data** creates unnecessary duplication. Ideally, redb should store only **indexes and metadata**, not copies of the data.

---

## Migration Plan: Sled → redb

### Why redb?

| Criterion | Sled 0.34.7 | **redb** | sqlite (rusqlite) |
|-----------|-------------|----------|-------------------|
| Active maintenance | ❌ Abandoned | ✅ Active | ✅ Active |
| Native language | Rust | **Pure Rust** | C (via FFI) |
| ACID | Partial | ✅ Full | ✅ Full |
| External dependencies | None | **None** | libsqlite3 |
| API | KV bytes | **Typed KV** | SQL |
| Performance (embedded) | Good | **Good** | Good |
| Binary size | Small | **Small** | Medium |
| `no_std` support | No | **Partial** | No |
| Crash safety | Partial | **Full** | Full |

> **redb** is the recommended choice for being 100% Rust, ACID compliant, actively maintained, and with an API similar to Sled (key-value), making migration easier.

### Migration Phases

#### Phase 1 — Adapt the `Datastore` Trait

The `Datastore` trait in `src/data_store.rs` already abstracts storage operations. Create a new `RedbDatastore` implementation that satisfies the same trait:

```rust
// New: src/cache/redb_store.rs
pub struct RedbDatastore {
    db: redb::Database,
    table_name: String,
}

impl Datastore for RedbDatastore {
    async fn get(&self, key: &[u8]) -> DbResult<Option<Vec<u8>>> { /* ... */ }
    async fn put(&self, key: &[u8], value: &[u8]) -> DbResult<()> { /* ... */ }
    async fn has(&self, key: &[u8]) -> DbResult<bool> { /* ... */ }
    async fn delete(&self, key: &[u8]) -> DbResult<()> { /* ... */ }
    async fn query(&self, query: &Query) -> DbResult<Results> { /* ... */ }
    async fn list_keys(&self, prefix: &[u8]) -> DbResult<Vec<Key>> { /* ... */ }
}
```

#### Phase 2 — Replace Sled References

Affected files:

| File | Change |
|------|--------|
| `src/cache/mod.rs` | Replace `SledDatastore` with `RedbDatastore` |
| `src/stores/base_store/mod.rs` | Remove downcasts to `SledDatastore` (~3 occurrences) |
| `Cargo.toml` | Replace `sled = "0.34.7"` with `redb = "2.x"` |

#### Phase 3 — Eliminate JSON Backup (in chat demo)

After validating that redb + iroh-blobs restore data reliably:

1. Remove `load_messages_from_file()` / `save_messages_to_file()` functions
2. Remove manual merge in `get_dm_messages()` / `get_group_messages()`
3. Remove `cleanup_mixed_messages()`
4. Rely on `log.load()` as the sole way to restore state

#### Phase 4 — Testing and Validation

- [ ] Existing integration tests pass with `RedbDatastore`
- [ ] Crash recovery test (kill -9 during write)

---

## Target Architecture (Post-Migration)

```
┌─────────────────────────────────────────────────────────┐
│                      BaseStore                           │
│                                                          │
│  ┌─────────────────────┐   ┌──────────────────────────┐ │
│  │  redb                │   │  iroh-blobs (FsStore)    │ │
│  │  (indexes +          │   │  (P2P source of truth)   │ │
│  │   metadata           │   │                          │ │
│  │   heads cache        │   │  Gossip → heads exchange │ │
│  │   sync state)        │   │  Blobs  → BLAKE3 data    │ │
│  │                      │   │  Docs   → distributed KV │ │
│  └─────────────────────┘   └──────────────────────────┘ │
│                                                          │
│  OpLog (memory) ← restored from redb on startup         │
│                                                          │
│  ❌ No JSON files       ❌ No data duplication           │
└─────────────────────────────────────────────────────────┘
```

### Clear Responsibilities:

| Component | Stores | Does Not Store |
|-----------|--------|----------------|
| **redb** | Indexes, metadata, heads cache, sync state | Replicable data (blobs) |
| **iroh-blobs** | Content-addressed data, immutable blobs | Local metadata |
| **OpLog** | In-memory state (restored from redb) | Nothing directly on disk |

---

## References

- [redb — crates.io](https://crates.io/crates/redb)
- [redb — GitHub](https://github.com/cberner/redb)
- [Sled status (archived)](https://github.com/spacejam/sled)
- [Storage Architecture](./STORAGE_ARCHITECTURE.md)
