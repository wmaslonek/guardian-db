# LevelDown Cache Module Documentation

## Overview

The `level_down` module provides a LevelDB-compatible cache implementation for GuardianDB, using Sled as the underlying storage engine. This module offers an alternative cache interface that follows LevelDB/LevelDOWN conventions, making it easier to integrate with systems expecting LevelDB-style APIs.

## Architecture

```
┌──────────────────┐
│ LevelDownCache   │  (Cache Manager)
└────────┬─────────┘
         │
         │ manages
         ▼
┌──────────────────┐
│  WrappedCache    │  (LevelDB-style Interface)
└────────┬─────────┘
         │
         │ wraps
         ▼
┌──────────────────┐
│DatastoreWrapper  │  (Async Adapter)
└──────────────────┘
```

## Core Components

### 1. WrappedCache

Low-level cache implementation with a LevelDB-compatible synchronous API.

#### Features

- **Synchronous API**: Traditional blocking operations for compatibility
- **Reference Counting**: Uses `Arc` for shared ownership across threads
- **Weak References**: Maintains weak reference to parent manager for automatic cleanup
- **Close Protection**: Mutex-protected closed state prevents double-close errors

#### Methods

##### `get(ctx, key) -> Result<Vec<u8>>`
Retrieves a value by key. Returns error if key doesn't exist.

```rust
let mut ctx = ();
let value = cache.get(&mut ctx, &key)?;
```

##### `has(ctx, key) -> Result<bool>`
Checks if a key exists without retrieving the value.

```rust
let exists = cache.has(&mut ctx, &key)?;
```

##### `get_size(ctx, key) -> Result<usize>`
Returns the byte size of a stored value.

```rust
let size = cache.get_size(&mut ctx, &key)?;
```

##### `put(ctx, key, value) -> Result<()>`
Stores a key-value pair, overwriting if exists.

```rust
cache.put(&mut ctx, &key, &value)?;
```

##### `delete(ctx, key) -> Result<()>`
Removes a key-value pair from the cache.

```rust
cache.delete(&mut ctx, &key)?;
```

##### `sync(ctx, key) -> Result<()>`
Flushes pending writes to disk (persistence).

```rust
cache.sync(&mut ctx, &key)?;
```

##### `query(ctx, query) -> Result<Results>`
Advanced query with prefix filtering, pagination, and ordering.

```rust
let query = Query {
    prefix: Some(Key::new("/users")),
    limit: Some(10),
    order: Order::Asc,
    offset: Some(0),
};
let results = cache.query(&mut ctx, &query)?;
```

##### `close() -> Result<()>`
Closes the cache, flushes data, and removes from manager registry.

```rust
cache.close()?;
```

### 2. DatastoreWrapper

Async adapter that wraps `WrappedCache` to implement the `Datastore` trait.

#### Purpose

- **API Translation**: Converts between async `Datastore` trait and sync `WrappedCache` methods
- **Error Mapping**: Translates errors to `GuardianError` format
- **Key Conversion**: Handles conversion between byte slices and `Key` objects

#### Implementation

```rust
#[async_trait::async_trait]
impl Datastore for DatastoreWrapper {
    async fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>>;
    async fn put(&self, key: &[u8], value: &[u8]) -> Result<()>;
    async fn has(&self, key: &[u8]) -> Result<bool>;
    async fn delete(&self, key: &[u8]) -> Result<()>;
    async fn query(&self, query: &Query) -> Result<Results>;
    async fn list_keys(&self, prefix: &[u8]) -> Result<Vec<Key>>;
}
```

### 3. LevelDownCache

High-level cache manager that implements the `Cache` trait.

#### Responsibilities

- **Cache Registry**: Maintains a thread-safe hashmap of active caches
- **Lazy Loading**: Creates caches on-demand when first accessed
- **Path Management**: Generates unique cache paths based on directory and database address
- **Lifecycle Management**: Handles cache creation, closure, and destruction

#### Configuration

```rust
pub struct LevelDownCache {
    span: Span,                                          // Tracing context
    caches: Arc<Mutex<HashMap<String, Arc<WrappedCache>>>>,  // Active caches
}
```

## Constants

### `IN_MEMORY_DIRECTORY`

```rust
pub const IN_MEMORY_DIRECTORY: &str = ":memory:";
```

Special directory name that triggers in-memory cache mode. When used, no disk I/O occurs and data is stored in RAM only.

## Usage Examples

### Creating a LevelDown Cache

```rust
use guardian_db::cache::level_down::LevelDownCache;
use guardian_db::cache::cache::Options;

// Create with default options
let cache = LevelDownCache::new(None);

// Create with custom options
let opts = Options {
    span: Some(tracing::info_span!("my_cache")),
    max_cache_size: Some(200 * 1024 * 1024), // 200MB
    ..Default::default()
};
let cache = LevelDownCache::new(Some(&opts));
```

### Loading a Cache Instance

```rust
use guardian_db::cache::Cache;

let cache = LevelDownCache::new(None);

// Load cache for specific database
let datastore = cache.load("/var/cache/guardian", &db_address)?;

// Use the datastore (async operations)
datastore.put(b"user:123", b"user_data").await?;
let value = datastore.get(b"user:123").await?;
```

### In-Memory Cache

```rust
use guardian_db::cache::level_down::IN_MEMORY_DIRECTORY;

let cache = LevelDownCache::new(None);

// Load in-memory cache (no disk I/O)
let datastore = cache.load(IN_MEMORY_DIRECTORY, &db_address)?;

datastore.put(b"temp_data", b"value").await?;
```

### Direct WrappedCache Usage

```rust
// Load internal wrapped cache
let wrapped = cache.load_internal("/cache/dir", &db_address)?;

// Synchronous operations with context
let mut ctx = ();
wrapped.put(&mut ctx, &Key::new("key"), b"value")?;
let value = wrapped.get(&mut ctx, &Key::new("key"))?;
```

### Querying Data

```rust
use guardian_db::data_store::{Query, Order, Key};

let datastore = cache.load(IN_MEMORY_DIRECTORY, &db_address)?;

// Insert test data
datastore.put(b"/users/alice", b"alice_data").await?;
datastore.put(b"/users/bob", b"bob_data").await?;
datastore.put(b"/users/charlie", b"charlie_data").await?;

// Query with prefix and pagination
let query = Query {
    prefix: Some(Key::new("/users")),
    limit: Some(10),
    order: Order::Asc,
    offset: Some(0),
};

let results = datastore.query(&query).await?;
for item in results {
    println!("Key: {}, Value: {:?}", item.key.as_str(), item.value);
}
```

### Listing Keys

```rust
// List all keys with prefix
let keys = datastore.list_keys(b"/users").await?;

for key in keys {
    println!("Found key: {}", key.as_str());
}
```

### Cache Lifecycle Management

```rust
let mut cache = LevelDownCache::new(None);

// Load and use cache
let datastore = cache.load("/cache/dir", &db_address)?;
datastore.put(b"key", b"value").await?;

// Destroy specific cache
cache.destroy("/cache/dir", &db_address)?;

// Close all caches
cache.close()?;
```

## Key Generation

The module uses a deterministic key generation algorithm for cache paths:

```rust
fn datastore_key(directory: &str, db_address: &dyn Address) -> String {
    let db_path = PathBuf::from(db_address.get_root().to_string())
        .join(db_address.get_path());
    PathBuf::from(directory)
        .join(db_path)
        .to_string_lossy()
        .into_owned()
}
```

**Example:**
- Directory: `/var/cache/guardian`
- DB Root: `bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi`
- DB Path: `users/profile`
- Result: `/var/cache/guardian/bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi/users/profile`

## Performance Characteristics

### Sled Backend

- **Write Performance**: O(log n) insertions
- **Read Performance**: O(log n) point lookups
- **Scan Performance**: Efficient prefix scans via B-tree
- **Concurrency**: Lock-free data structures

### Context Parameter

The `ctx: &mut dyn Any` parameter in `WrappedCache` methods is reserved for future extensibility and is currently unused. It allows passing arbitrary context data to operations without breaking API compatibility.

## Error Handling

The module uses two error handling patterns:

### WrappedCache Errors

```rust
std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>
```

Generic boxed errors for maximum compatibility with LevelDB conventions.

### Datastore Errors

```rust
Result<T>  // Guardian's Result type (Result<T, GuardianError>)
```

Strongly-typed errors using `GuardianError` enum.

### Error Mapping

```rust
// Mapping from generic to typed errors
self.cache
    .get(&mut ctx, &key_obj)
    .map_err(|e| GuardianError::Other(format!("Cache get error: {}", e)))
```

## Thread Safety

All components are thread-safe:

- **LevelDownCache**: Uses `Arc<Mutex<HashMap>>` for cache registry
- **WrappedCache**: Implements `Send + Sync`, uses `Weak` references
- **DatastoreWrapper**: Thread-safe wrapper around `Arc<WrappedCache>`
- **Sled Backend**: Lock-free concurrent data structures

## Memory Management

### Reference Counting

```
LevelDownCache (Arc)
    ↓ (strong)
HashMap<String, Arc<WrappedCache>>
    ↓ (strong)
WrappedCache
    ↓ (weak)
HashMap (parent reference)
```

### Automatic Cleanup

When `WrappedCache::close()` is called:
1. Upgrades weak reference to parent HashMap
2. Removes itself from the registry
3. Flushes data to disk
4. Sets closed flag to prevent double-close

## Observability

All operations are instrumented with the `tracing` crate:

### Instrumented Methods

- `get`, `put`, `delete`, `sync`: Debug level
- `load`, `close`, `destroy`: Info level
- Query operations: Debug level with detailed parameters

### Example Logs

```
DEBUG opening cache db: path=/cache/root/bafybe.../users
DEBUG Cache put success: key=user:123
INFO  Cache closed: count=5 caches
```

## Testing

The module includes comprehensive tests:

```bash
# Run all level_down tests
cargo test --package guardian-db level_down::

# Run specific test
cargo test test_datastore_wrapper_basic_operations
```

### Test Coverage

- ✓ Basic CRUD operations (put, get, delete, has)
- ✓ Query with prefix, limit, offset, ordering
- ✓ Key listing with prefixes
- ✓ Cache key generation
- ✓ Cache lifecycle (load, destroy, close)
- ✓ In-memory vs persistent mode

### Mock Address

Tests use a `MockAddress` implementation for testing without real IPFS CIDs:

```rust
struct MockAddress {
    root: cid::Cid,
    path: String,
}
```

## Comparison: LevelDownCache vs SledCache

| Feature | LevelDownCache | SledCache |
|---------|----------------|-----------|
| **API Style** | Synchronous + Async Wrapper | Fully Async |
| **Context Parameter** | `&mut dyn Any` | None |
| **Error Type** | Boxed + Typed | Typed (`GuardianError`) |
| **Compatibility** | LevelDB-style | Native Rust |
| **Use Case** | Legacy integration | Modern async code |
| **Performance** | Slight overhead (wrapper) | Direct |

## Best Practices

1. **Use IN_MEMORY_DIRECTORY for Tests**: Avoid disk I/O in unit tests
2. **Close Explicitly**: Always call `close()` before dropping cache instances
3. **Handle Errors**: Cache operations can fail; handle errors appropriately
4. **Trace Context**: Provide spans for better debugging in production
5. **Prefix Keys**: Use hierarchical key structures (e.g., `/users/123`, `/config/db`)
6. **Async Wrapper**: Use `DatastoreWrapper` for async code, direct `WrappedCache` for sync
7. **Resource Management**: Destroy caches when no longer needed to free resources

## Advanced Patterns

### Custom Context

```rust
// Future extension: custom context
struct CacheContext {
    transaction_id: u64,
    user_id: String,
}

let mut ctx: Box<dyn Any> = Box::new(CacheContext { ... });
cache.get(&mut *ctx, &key)?;
```

### Batch Operations

```rust
// Efficient batch writes
let batch = vec![
    (b"key1", b"value1"),
    (b"key2", b"value2"),
    (b"key3", b"value3"),
];

for (key, value) in batch {
    datastore.put(key, value).await?;
}

// Sync once at the end
let wrapped = cache.load_internal("/cache", &db_address)?;
let mut ctx = ();
wrapped.sync(&mut ctx, &Key::new("dummy"))?;
```

### Cache Size Monitoring

```rust
// Get size of stored values
let wrapped = cache.load_internal("/cache", &db_address)?;
let mut ctx = ();

let mut total_size = 0;
for key in keys {
    if let Ok(size) = wrapped.get_size(&mut ctx, &key) {
        total_size += size;
    }
}
println!("Total cache size: {} bytes", total_size);
```

## API Reference

### Public Functions

```rust
// Helper for generating cache keys
fn datastore_key(directory: &str, db_address: &dyn Address) -> String
```

### Public Types

```rust
// Main cache manager
pub struct LevelDownCache { /* ... */ }

// Low-level cache instance
pub struct WrappedCache { /* ... */ }

// Async adapter
pub struct DatastoreWrapper { /* ... */ }

// Special in-memory directory constant
pub const IN_MEMORY_DIRECTORY: &str = ":memory:";
```

## Dependencies

- **sled**: Embedded database engine
- **tracing**: Structured logging framework
- **async-trait**: Async trait support
- **tokio**: Async runtime (tests)

## Related Modules

- `cache::cache`: Modern async cache implementation (SledCache)
- `data_store`: Datastore trait and query types
- `address`: Database addressing system
- `error`: Error types and result handling

## Migration Guide

### From LevelDB to LevelDownCache

```rust
// Before (LevelDB)
let db = leveldb::Database::open("/path/to/db", options)?;
db.put(key, value)?;

// After (LevelDownCache)
let cache = LevelDownCache::new(None);
let wrapped = cache.load_internal("/path/to/db", &address)?;
let mut ctx = ();
wrapped.put(&mut ctx, &Key::new(key), value)?;
```

### From LevelDownCache to SledCache

```rust
// Before (LevelDownCache)
let cache = LevelDownCache::new(None);
let datastore = cache.load("/cache", &address)?;

// After (SledCache)
use guardian_db::cache::cache::{create_default_cache, Cache};
let cache = create_default_cache();
let datastore = cache.load("/cache", &address)?;
// Same async API for datastore operations
```

## License

Part of the GuardianDB project.
