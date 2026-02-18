# Guardian-DB Benchmarks

## Current State

There is **1 benchmark** in `benches/iroh_gossipsub_performance.rs`, but it is marked as **⚠ DEPRECATED**.

It covers 3 groups using Criterion:

| Group | Benchmarks |
|-------|-----------|
| `benchmark_iroh_operations` | `iroh_add_content`, `iroh_cat_content`, `network_ping` |
| `benchmark_concurrent_operations` | `concurrent_add_operations` (10 simultaneous tasks) |
| `benchmark_cache_performance` | `cache_hit_performance` |

The deprecated benchmark needs to be replaced with new ones that reflect the current architecture (Postcard, ed25519, direct `EpidemicPubSub`, isolated cache).

---

## Required Benchmarks

### 1. Store Operations (Core CRUD)

- **KeyValueStore**: `put`, `get`, `delete`, `all` — throughput with varying payload sizes
- **EventLogStore**: `add_operation`, `get_by_hash`, log iteration
- **DocumentStore**: `put`, `delete`, queries with JSON filters, batch operations

### 2. Serialization (Postcard + BLAKE3)

- Serialization/deserialization of `Entry` with Postcard (recent migration from JSON)
- BLAKE3 hashing for entries of different sizes
- CBOR encoding/decoding (used in `IrohAccessController`)

### 3. Cryptography (ed25519)

- Signing and verification with `ed25519_dalek` (recent migration from secp256k1)
- Identity creation (`DefaultIdentificator`)
- Batch signature validation

### 4. P2P / Networking

- `IrohBackend`: add/cat content (update the deprecated benchmark)
- `EpidemicPubSub`: publish/subscribe throughput
- Replication between nodes (propagation latency)

### 5. Cache (Sled)

- Cache hit vs miss performance
- Read and write with directory isolation
- Eviction and LRU behavior

### 6. Access Control

- `SimpleAccessController`: `can_append` evaluation
- `GuardianDBAccessController`: grant/revoke + persistence (save/load)
- `IrohAccessController`: permission verification

### 7. Concurrency

- Concurrent operations on stores via `Arc<dyn Store>`
- Contention on `Arc<RwLock<T>>` from `BaseStore`
- Concurrent grants/revokes on the access controller

### 8. Reactive Synchronization

- `SyncObserver` event dispatch throughput
- `SyncProgress` tracking overhead
- `load()` and `sync()` with active vs inactive observer

---

## Suggested Structure

It is recommended to create separate files in `benches/` for each category:

```
benches/
├── store_benchmarks.rs          # Store Operations (CRUD)
├── serialization_benchmarks.rs  # Postcard, BLAKE3, CBOR
├── crypto_benchmarks.rs         # ed25519, identities
├── p2p_benchmarks.rs            # IrohBackend, EpidemicPubSub
├── cache_benchmarks.rs          # Sled cache performance
├── access_control_benchmarks.rs # Access controllers
├── concurrency_benchmarks.rs    # Arc<RwLock<T>>, simultaneous operations
└── sync_benchmarks.rs           # Reactive Synchronization
```

Each file must be registered in `Cargo.toml`:

```toml
[[bench]]
name = "store_benchmarks"
harness = false

[[bench]]
name = "serialization_benchmarks"
harness = false

# ... etc
```

## Running

```bash
# All benchmarks
cargo bench

# Specific benchmark
cargo bench --bench store_benchmarks

# With function filter
cargo bench --bench crypto_benchmarks -- ed25519_sign
```
