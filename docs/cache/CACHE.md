# Cache Module Documentation

## Overview

The cache module provides a high-performance caching layer for GuardianDB using [Sled](https://github.com/spacejam/sled) as the backend storage engine. It supports both persistent disk-based caching and in-memory temporary caching with automatic cleanup capabilities.

## Architecture

```
┌─────────────────┐
│   Cache Trait   │  (Interface)
└────────┬────────┘
         │
         │ implements
         ▼
┌─────────────────┐
│   SledCache     │  (Cache Manager)
└────────┬────────┘
         │
         │ manages
         ▼
┌─────────────────┐
│ SledDatastore   │  (Storage Implementation)
└─────────────────┘
```

## Core Components

### 1. Cache Trait

The `Cache` trait defines the core interface for cache implementations.

#### Methods

- **`new(path: &str, opts: Option<Options>) -> NewCacheResult`**
  - Creates a new cache instance at the specified path
  - Returns a `Datastore` and cleanup function
  - Default implementation uses `SledCache`

- **`load(&self, directory: &str, db_address: &dyn Address) -> Result<DatastoreBox>`**
  - Loads or creates a cache for a specific database address
  - Returns a boxed Datastore implementation

- **`close(&mut self) -> Result<()>`**
  - Closes the cache and all associated datastores
  - Flushes pending writes to disk

- **`destroy(&self, directory: &str, db_address: &dyn Address) -> Result<()>`**
  - Removes all cached data for a specific database
  - Deletes cache files from disk

### 2. SledCache

Main cache implementation using Sled as the storage backend.

#### Features

- **Multiple Cache Management**: Maintains a hashmap of active caches indexed by cache key
- **Thread-Safe**: Uses `Arc<Mutex<HashMap>>` for concurrent access
- **Automatic Key Generation**: Creates unique cache keys based on directory and database address
- **Lazy Loading**: Caches are created on-demand when first accessed

#### Configuration

Caches are configured using the `Options` struct with the following parameters:

```rust
pub struct Options {
    pub span: Option<Span>,              // Tracing span for structured logging
    pub max_cache_size: Option<u64>,     // Maximum cache size in bytes (default: 100MB)
    pub cache_mode: CacheMode,           // Cache operation mode
}
```

#### Cache Modes

- **`CacheMode::Persistent`**: Data persists on disk between restarts
- **`CacheMode::InMemory`**: Temporary cache stored in memory only
- **`CacheMode::Auto`**: Automatically detects mode based on directory (`:memory:` triggers in-memory mode)

### 3. SledDatastore

Implementation of the `Datastore` trait using Sled for key-value storage.

#### Key Operations

All operations are asynchronous and instrumented with tracing:

- **`get(&self, key: &[u8]) -> Result<Option<Vec<u8>>>`**
  - Retrieves a value by key
  - Returns `None` if key doesn't exist
  - Logs cache hits/misses

- **`put(&self, key: &[u8], value: &[u8]) -> Result<()>`**
  - Stores a key-value pair
  - Overwrites existing values

- **`has(&self, key: &[u8]) -> Result<bool>`**
  - Checks if a key exists without retrieving the value

- **`delete(&self, key: &[u8]) -> Result<()>`**
  - Removes a key-value pair from the cache

- **`query(&self, query: &Query) -> Result<Results>`**
  - Advanced query with prefix filtering, pagination, and ordering
  - Supports:
    - Prefix-based scanning
    - Limit and offset for pagination
    - Ascending/descending order

- **`list_keys(&self, prefix: &[u8]) -> Result<Vec<Key>>`**
  - Lists all keys matching a given prefix
  - Efficient for key enumeration

## Usage Examples

### Creating a Cache

```rust
use guardian_db::cache::{create_default_cache, Options, CacheMode};

// Default cache (persistent, 100MB)
let cache = create_default_cache();

// Custom cache with options
let cache = create_cache(Options {
    span: Some(tracing::info_span!("my_cache")),
    max_cache_size: Some(500 * 1024 * 1024), // 500MB
    cache_mode: CacheMode::Persistent,
});

// In-memory cache for testing
let cache = create_memory_cache();
```

### Using the Cache Interface

```rust
use guardian_db::cache::Cache;

// Create cache instance
let (datastore, cleanup) = Cache::new("/path/to/cache", None)?;

// Use the datastore
datastore.put(b"key", b"value").await?;
let value = datastore.get(b"key").await?;

// Cleanup when done
cleanup()?;
```

### Loading Database-Specific Cache

```rust
let mut cache = create_default_cache();

// Load cache for specific database
let datastore = cache.load("/cache/root", &db_address)?;

// Perform operations
datastore.put(b"user:123", b"user_data").await?;
let data = datastore.get(b"user:123").await?;

// Destroy specific database cache
cache.destroy("/cache/root", &db_address)?;

// Close all caches
cache.close()?;
```

### Querying Data

```rust
use guardian_db::data_store::{Query, Order, Key};

let datastore = SledDatastore::new(":memory:", Options::default())?;

// Insert data
datastore.put(b"/users/alice", b"alice_data").await?;
datastore.put(b"/users/bob", b"bob_data").await?;

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

## Performance Characteristics

### Sled Backend

- **Write Performance**: O(log n) for insertions
- **Read Performance**: O(log n) for point lookups
- **Scan Performance**: Efficient prefix scans using B-tree structure
- **Memory Usage**: Configurable cache capacity (default: 100MB)

### Optimization Tips

1. **Batch Operations**: Use queries instead of multiple individual gets
2. **Prefix Design**: Structure keys hierarchically for efficient prefix scans
3. **Cache Size**: Increase `max_cache_size` for workloads with large working sets
4. **In-Memory Mode**: Use for temporary data or testing to avoid disk I/O

## Observability

All operations are instrumented with structured logging using the `tracing` crate:

```rust
// Enable logging
tracing_subscriber::fmt::init();

// Operations will log:
// - Cache hits/misses
// - Operation latencies
// - Error conditions
// - Cache lifecycle events (creation, closure, destruction)
```

### Log Levels

- `info`: Cache lifecycle events (create, load, close, destroy)
- `debug`: Individual operations (get, put, delete, query)
- `warn`: Non-fatal errors (failed cleanup, close errors)
- `error`: Fatal errors (storage failures, corruption)

## Error Handling

All operations return `Result<T>` with `GuardianError`:

```rust
match datastore.get(b"key").await {
    Ok(Some(value)) => println!("Found: {:?}", value),
    Ok(None) => println!("Not found"),
    Err(GuardianError::Store(msg)) => eprintln!("Storage error: {}", msg),
    Err(e) => eprintln!("Other error: {}", e),
}
```

## Testing

The module includes comprehensive tests:

```bash
# Run all cache tests
cargo test --package guardian-db cache::

# Run specific test
cargo test test_sled_datastore_basic_operations
```

### Test Coverage

- ✓ Basic CRUD operations (put, get, delete, has)
- ✓ Query functionality (prefix, limit, offset, ordering)
- ✓ Key listing with prefixes
- ✓ Cache mode detection
- ✓ In-memory vs persistent behavior

## Thread Safety

- **SledCache**: Thread-safe, can be shared across threads using `Arc`
- **SledDatastore**: Implements `Send + Sync`, safe for concurrent access
- **Sled Backend**: Lock-free data structures for high concurrency

## Cleanup and Resource Management

### Automatic Cleanup

When creating a cache with `Cache::new()`, a cleanup function is returned:

```rust
let (datastore, cleanup) = Cache::new("/tmp/cache", None)?;
// ... use datastore ...
cleanup()?; // Removes cache directory
```

### Manual Cleanup

```rust
// Close all caches (flushes to disk)
cache.close()?;

// Destroy specific database cache (removes from disk)
cache.destroy("/cache/root", &db_address)?;
```

### RAII Pattern

For automatic cleanup on drop:

```rust
struct CacheGuard {
    datastore: DatastoreBox,
    cleanup: CleanupFn,
}

impl Drop for CacheGuard {
    fn drop(&mut self) {
        // Take cleanup function and execute it
        let cleanup = std::mem::replace(&mut self.cleanup, Box::new(|| Ok(())));
        let _ = cleanup();
    }
}
```

## Best Practices

1. **Close Properly**: Always call `close()` before application shutdown to ensure data integrity
2. **Handle Errors**: Cache operations can fail; handle errors appropriately
3. **Use Spans**: Provide tracing spans for better observability in production
4. **Prefix Design**: Use hierarchical key structures (e.g., `/users/{id}`, `/config/{key}`)
5. **Cache Sizing**: Monitor memory usage and adjust `max_cache_size` based on workload
6. **Testing**: Use `create_memory_cache()` for unit tests to avoid disk I/O
7. **Cleanup**: Use the cleanup function or `destroy()` to remove temporary caches

## API Reference

### Factory Functions

```rust
// Create cache with custom options
pub fn create_cache(opts: Options) -> SledCache

// Create cache with default settings
pub fn create_default_cache() -> SledCache

// Create in-memory cache for testing
pub fn create_memory_cache() -> SledCache
```

### Type Aliases

```rust
type DatastoreBox = Box<dyn Datastore + Send + Sync>;
type CleanupFn = Box<dyn FnOnce() -> Result<()> + Send + Sync>;
type NewCacheResult = Result<(DatastoreBox, CleanupFn)>;
```

## Dependencies

- **sled**: Embedded database engine
- **tracing**: Structured logging and diagnostics
- **async-trait**: Async trait support
- **tokio**: Async runtime (for tests)

## Related Modules

- `data_store`: Defines the `Datastore` trait and query types
- `address`: Database addressing system
- `error`: Error types and result handling

## License

Part of the GuardianDB project.
