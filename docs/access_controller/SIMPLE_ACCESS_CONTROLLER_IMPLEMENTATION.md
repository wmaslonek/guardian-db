    // ╔════════════════════════════════════════════════════════════════════════════════╗
    // ║                          ⚠ OUTDATED DOCUMENTATION                             ║
    // ╚════════════════════════════════════════════════════════════════════════════════╝


# SimpleAccessController

## Overview

The `SimpleAccessController` is the most straightforward and lightweight access control implementation in GuardianDB. It provides:

- **In-Memory Storage**: All permissions stored in RAM using HashMap
- **No Persistence**: Permissions lost when controller is dropped (unless explicitly saved)
- **Fast Performance**: No IPFS or disk I/O, pure memory operations
- **Full Featured**: Complete support for multiple capabilities/roles
- **Development-Friendly**: Perfect for testing, prototyping, and development

## When to Use

### Ideal For:
- **Development and Testing**: Fast iteration without IPFS setup
- **Prototyping**: Quick proof-of-concepts
- **Single-Node Applications**: No need for distributed permissions
- **Temporary Access Control**: Short-lived processes
- **Unit Tests**: Deterministic, no external dependencies

### Not Suitable For:
- **Production Multi-Node Systems**: Permissions not replicated
- **Long-Running Services**: State lost on restart
- **Distributed Applications**: No P2P synchronization
- **Audit Requirements**: No persistence or event logs

---

## Architecture

### Data Structures

#### SimpleAccessController

```rust
pub struct SimpleAccessController {
    state: Arc<RwLock<SimpleAccessControllerState>>,
    span: Span,
}
```

**Fields:**
- `state` - Thread-safe, shared state container
- `span` - Tracing span for structured observability

#### SimpleAccessControllerState

```rust
struct SimpleAccessControllerState {
    allowed_keys: HashMap<String, Vec<String>>,
}
```

**Internal state:**
- `allowed_keys` - Map of capability → list of authorized key IDs
- Example: `{"write": ["did:key:alice", "*"], "admin": ["did:key:bob"]}`

### Thread Safety Model

```
Arc<RwLock<State>>
 │     │      │
 │     │      └─ State: HashMap of permissions
 │     └─ RwLock: Multiple readers OR single writer
 └─ Arc: Shared ownership, cheap cloning
```

**Concurrency Benefits:**
- Multiple threads can read permissions simultaneously
- Write operations (grant/revoke) are exclusive
- No data races or corruption possible

---

## API Reference

### Constructors

#### `new()`

Creates a new controller with optional initial permissions.

```rust
pub fn new(initial_keys: Option<HashMap<String, Vec<String>>>) -> Self
```

**Parameters:**
- `initial_keys` - Optional initial permission map

**Behavior:**
- Ensures basic categories exist: `"read"`, `"write"`, `"admin"`
- Creates empty vectors if not provided
- Logs creation with category and permission counts

**Example:**
```rust
use std::collections::HashMap;

// Empty controller (with basic categories)
let controller = SimpleAccessController::new(None);

// Controller with initial permissions
let mut permissions = HashMap::new();
permissions.insert("write".to_string(), vec![
    "did:key:alice".to_string(),
    "did:key:bob".to_string(),
]);
permissions.insert("admin".to_string(), vec![
    "did:key:alice".to_string(),
]);

let controller = SimpleAccessController::new(Some(permissions));
```

---

#### `new_simple()`

Creates an empty controller (convenience method).

```rust
pub fn new_simple() -> Self
```

Equivalent to `new(None)`.

**Example:**
```rust
let controller = SimpleAccessController::new_simple();
// Has empty "read", "write", "admin" categories
```

---

#### `from_options()`

Creates controller from manifest parameters.

```rust
pub fn from_options(params: CreateAccessControllerOptions) -> Result<Self>
```

**Parameters:**
- `params` - Manifest options containing permissions

**Example:**
```rust
use guardian_db::access_controller::manifest::CreateAccessControllerOptions;

let mut params = CreateAccessControllerOptions::new_empty();
params.set_access("write".to_string(), vec![
    "did:key:alice".to_string(),
]);

let controller = SimpleAccessController::from_options(params)?;
```

---

### Permission Management

#### `grant()`

Grants a capability to a key ID.

```rust
async fn grant(&self, capability: &str, key_id: &str) -> Result<()>
```

**Parameters:**
- `capability` - Role/permission name (e.g., "write", "admin", "custom")
- `key_id` - Identity key to authorize (or `"*"` for universal)

**Behavior:**
1. Validates capability and key_id are not empty
2. Acquires write lock
3. Creates capability entry if doesn't exist
4. Adds key_id if not already present (prevents duplicates)
5. Logs operation

**Example:**
```rust
// Grant write permission
controller.grant("write", "did:key:alice").await?;

// Grant custom capability
controller.grant("moderator", "did:key:bob").await?;

// Universal access
controller.grant("read", "*").await?;
```

---

#### `revoke()`

Revokes a capability from a key ID.

```rust
async fn revoke(&self, capability: &str, key_id: &str) -> Result<()>
```

**Parameters:**
- `capability` - Role/permission name
- `key_id` - Identity key to remove

**Behavior:**
1. Validates parameters
2. Acquires write lock
3. Removes key_id from list
4. If capability becomes empty, removes it entirely
5. Logs operation

**Example:**
```rust
// Revoke write permission
controller.revoke("write", "did:key:alice").await?;

// Revoke from non-existent capability (no-op, no error)
controller.revoke("nonexistent", "did:key:bob").await?; // OK
```

---

#### `grant_multiple()`

Grants a capability to multiple keys at once (bulk operation).

```rust
pub async fn grant_multiple(&self, capability: &str, key_ids: Vec<&str>) -> Result<()>
```

**Parameters:**
- `capability` - Role/permission name
- `key_ids` - Vector of key IDs to authorize

**Behavior:**
- Single write lock for all operations (atomic)
- Skips empty strings
- Prevents duplicates
- Logs count of added keys

**Example:**
```rust
controller.grant_multiple("write", vec![
    "did:key:alice",
    "did:key:bob",
    "did:key:charlie",
]).await?;

// More efficient than 3 separate grant() calls
```

---

#### `revoke_multiple()`

Revokes a capability from multiple keys at once.

```rust
pub async fn revoke_multiple(&self, capability: &str, key_ids: Vec<&str>) -> Result<()>
```

**Parameters:**
- `capability` - Role/permission name
- `key_ids` - Vector of key IDs to remove

**Behavior:**
- Single write lock for all operations
- Removes capability if becomes empty
- Logs count of removed keys

**Example:**
```rust
controller.revoke_multiple("write", vec![
    "did:key:alice",
    "did:key:bob",
]).await?;
```

---

### Query Operations

#### `get_authorized_by_role()`

Returns all key IDs authorized for a role.

```rust
async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>>
```

**Parameters:**
- `role` - Role/capability name

**Returns:**
- `Vec<String>` - List of authorized key IDs (empty if role doesn't exist)

**Example:**
```rust
let writers = controller.get_authorized_by_role("write").await?;
println!("Writers: {:?}", writers);

let admins = controller.get_authorized_by_role("admin").await?;
println!("Admins: {:?}", admins);
```

---

#### `list_keys()`

Lists all keys for a specific capability (same as `get_authorized_by_role` but without error handling).

```rust
pub async fn list_keys(&self, capability: &str) -> Vec<String>
```

**Example:**
```rust
let write_keys = controller.list_keys("write").await;
// Never fails, returns empty vec if capability doesn't exist
```

---

#### `list_capabilities()`

Lists all available capabilities/roles.

```rust
pub async fn list_capabilities(&self) -> Vec<String>
```

**Returns:**
- `Vec<String>` - All capability names

**Example:**
```rust
let capabilities = controller.list_capabilities().await;
println!("Available capabilities: {:?}", capabilities);
// Output: ["read", "write", "admin", "moderator"]
```

---

#### `has_capability()`

Checks if a key has a specific capability.

```rust
pub async fn has_capability(&self, capability: &str, key_id: &str) -> bool
```

**Parameters:**
- `capability` - Role/permission name
- `key_id` - Identity key to check

**Returns:**
- `true` if key has capability (or `"*"` exists)
- `false` otherwise

**Example:**
```rust
if controller.has_capability("write", "did:key:alice").await {
    println!("Alice can write");
} else {
    println!("Alice cannot write");
}
```

---

### Statistics and Management

#### `get_stats()`

Returns permission count for each capability.

```rust
pub async fn get_stats(&self) -> HashMap<String, usize>
```

**Returns:**
- `HashMap<String, usize>` - Capability → count of authorized keys

**Example:**
```rust
let stats = controller.get_stats().await;
for (capability, count) in stats {
    println!("{}: {} keys", capability, count);
}
// Output:
// write: 3 keys
// admin: 1 keys
// read: 5 keys
```

---

#### `total_permissions()`

Counts total permissions across all capabilities.

```rust
pub async fn total_permissions(&self) -> usize
```

**Example:**
```rust
let total = controller.total_permissions().await;
println!("Total permissions: {}", total);
```

---

#### `is_capability_empty()`

Checks if a capability has zero keys.

```rust
pub async fn is_capability_empty(&self, capability: &str) -> bool
```

**Returns:**
- `true` if capability doesn't exist or has no keys
- `false` if capability has at least one key

**Example:**
```rust
if controller.is_capability_empty("moderator").await {
    println!("No moderators assigned yet");
}
```

---

#### `clear_capability()`

Removes all keys from a capability.

```rust
pub async fn clear_capability(&self, capability: &str) -> Result<()>
```

**Parameters:**
- `capability` - Role/permission name to clear

**Behavior:**
- Empties the key list but keeps the capability entry
- Logs count of removed keys

**Example:**
```rust
controller.clear_capability("write").await?;
// All write permissions removed

let writers = controller.list_keys("write").await;
assert!(writers.is_empty());
```

---

### Import/Export

#### `export_permissions()`

Exports all permissions to a HashMap.

```rust
pub async fn export_permissions(&self) -> HashMap<String, Vec<String>>
```

**Returns:**
- `HashMap` - Complete copy of all permissions

**Example:**
```rust
let backup = controller.export_permissions().await;
// Save to file, database, etc.
```

---

#### `import_permissions()`

Imports permissions from a HashMap (replaces all existing).

```rust
pub async fn import_permissions(
    &self,
    permissions: HashMap<String, Vec<String>>,
) -> Result<()>
```

**Parameters:**
- `permissions` - New permission map (replaces current state)

**Behavior:**
- Completely replaces current permissions
- Logs count of capabilities and permissions
- Atomic operation (single write lock)

**Example:**
```rust
// Backup
let backup = controller.export_permissions().await;

// Make changes...
controller.grant("write", "did:key:eve").await?;

// Restore from backup
controller.import_permissions(backup).await?;
// Eve's permission is now gone
```

---

#### `clone_capability()`

Clones permissions from one capability to another.

```rust
pub async fn clone_capability(
    &self,
    source_capability: &str,
    target_capability: &str,
) -> Result<()>
```

**Parameters:**
- `source_capability` - Capability to copy from
- `target_capability` - Capability to copy to

**Behavior:**
- Creates target capability if doesn't exist
- Overwrites target if it exists
- Returns error if source doesn't exist

**Example:**
```rust
// Copy write permissions to moderator role
controller.clone_capability("write", "moderator").await?;

let writers = controller.list_keys("write").await;
let moderators = controller.list_keys("moderator").await;
assert_eq!(writers, moderators);
```

---

### Entry Validation

#### `can_append()`

Validates whether a log entry can be appended.

```rust
async fn can_append(
    &self,
    entry: &dyn LogEntry,
    identity_provider: &dyn IdentityProvider,
    _additional_context: &dyn CanAppendAdditionalContext,
) -> Result<()>
```

**Parameters:**
- `entry` - Log entry to validate
- `identity_provider` - Provider for identity verification
- `_additional_context` - Additional context (currently unused)

**Returns:**
- `Ok(())` - Entry authorized
- `Err(GuardianError)` - Entry rejected

**Authorization Logic:**
1. Extract entry's identity ID
2. Check "write" keys: if `"*"` OR ID matches → verify signature
3. Check "admin" keys: if `"*"` OR ID matches → verify signature
4. If neither matches → reject

**Example:**
```rust
match controller.can_append(&entry, &identity_provider, &context).await {
    Ok(()) => {
        println!("Entry authorized");
        log.add_entry(entry).await?;
    }
    Err(e) => {
        println!("Entry rejected: {}", e);
    }
}
```

---

### Persistence (No-Op)

#### `save()`

Creates manifest params from current state.

```rust
async fn save(&self) -> Result<Box<dyn ManifestParams>>
```

**Returns:**
- `Box<dyn ManifestParams>` - Manifest containing current permissions

**Note:** This doesn't persist to disk/IPFS, just creates a manifest object that could be serialized.

**Example:**
```rust
let manifest = controller.save().await?;
// Could serialize manifest to JSON/CBOR for storage
let json = serde_json::to_string(&manifest)?;
std::fs::write("permissions.json", json)?;
```

---

#### `load()`

No-op for SimpleAccessController.

```rust
async fn load(&self, address: &str) -> Result<()>
```

**Behavior:**
- Validates address is not empty
- Logs operation
- Returns Ok (does nothing)

**Note:** For real persistence, use `GuardianDBAccessController` or `IpfsAccessController`.

---

#### `close()`

No-op for SimpleAccessController.

```rust
async fn close(&self) -> Result<()>
```

**Behavior:**
- Logs capabilities count
- Returns Ok (does nothing)

---

### Utility Methods

#### `span()`

Returns the tracing span for this controller.

```rust
pub fn span(&self) -> &Span
```

Used for manual span entry in custom code.

#### `address()`

Returns the address of the controller.

```rust
pub fn address(&self) -> Option<Box<dyn Address>>
```

**Returns:** `None` (SimpleAccessController is not addressed)

---

## Permission Model

### Capabilities

SimpleAccessController supports any string as a capability name:

| Capability | Default Behavior | Common Use |
|------------|------------------|------------|
| `"write"` | Checked in `can_append()` | Allow adding entries |
| `"admin"` | Checked in `can_append()` | Administrative access (implies write) |
| `"read"` | Not enforced by default | Custom read restrictions |
| `"moderator"` | Custom | Any custom role |
| `"*"` | N/A | Not a capability, but a wildcard key |

### Authorization Flow

```
can_append(entry)
    │
    ├─ Get entry identity ID
    │
    ├─ Check "write" capability
    │   ├─ Has "*"? → Verify signature → OK
    │   └─ Has ID? → Verify signature → OK
    │
    ├─ Check "admin" capability
    │   ├─ Has "*"? → Verify signature → OK
    │   └─ Has ID? → Verify signature → OK
    │
    └─ No match → REJECT
```

### Universal Access

The key `"*"` grants universal access:

```rust
controller.grant("write", "*").await?;
// Now ANY identity can write (signature still verified)

controller.grant("read", "*").await?;
// Custom code can check this for universal read
```

---

## Concurrency Patterns

### Read Operations (Concurrent)

Multiple threads can read simultaneously:

```rust
// Thread 1
let writers1 = controller.get_authorized_by_role("write").await?;

// Thread 2 (concurrent)
let admins = controller.get_authorized_by_role("admin").await?;

// Thread 3 (concurrent)
let can_write = controller.has_capability("write", "did:key:alice").await;
```

### Write Operations (Exclusive)

Write operations are serialized:

```rust
// Thread 1
controller.grant("write", "alice").await?; // Acquires write lock

// Thread 2 (waits for Thread 1)
controller.grant("admin", "bob").await?; // Waits, then acquires write lock
```

### Mixed Operations

Reads wait for writes:

```rust
// Thread 1
controller.grant("write", "alice").await?; // Write lock held

// Thread 2 (waits)
let writers = controller.get_authorized_by_role("write").await?; // Waits for write lock
```

### Bulk Operations (Atomic)

Bulk methods are atomic:

```rust
// Single transaction
controller.grant_multiple("write", vec![
    "alice",
    "bob",
    "charlie",
]).await?;

// Another thread sees either:
// - None of the keys (before operation)
// - All of the keys (after operation)
// Never a partial state
```

---

## Complete Examples

### Basic Setup and Usage

```rust
use guardian_db::access_controller::simple::SimpleAccessController;
use std::collections::HashMap;

// Create controller
let controller = SimpleAccessController::new_simple();

// Grant permissions
controller.grant("write", "did:key:alice").await?;
controller.grant("write", "did:key:bob").await?;
controller.grant("admin", "did:key:alice").await?;

// Query
let writers = controller.get_authorized_by_role("write").await?;
println!("Writers: {:?}", writers);

let admins = controller.get_authorized_by_role("admin").await?;
println!("Admins: {:?}", admins);
```

---

### Role Management

```rust
// Create roles
controller.grant("moderator", "did:key:alice").await?;
controller.grant("moderator", "did:key:bob").await?;
controller.grant("viewer", "did:key:charlie").await?;

// Check roles
if controller.has_capability("moderator", "did:key:alice").await {
    println!("Alice is a moderator");
}

// List all roles
let roles = controller.list_capabilities().await;
println!("Available roles: {:?}", roles);

// Get role members
let moderators = controller.list_keys("moderator").await;
println!("Moderators: {:?}", moderators);
```

---

### Bulk Operations

```rust
// Bulk grant
let new_users = vec![
    "did:key:user1",
    "did:key:user2",
    "did:key:user3",
];
controller.grant_multiple("write", new_users).await?;

// Bulk revoke
let banned_users = vec!["did:key:user1", "did:key:user3"];
controller.revoke_multiple("write", banned_users).await?;
```

---

### Backup and Restore

```rust
// Backup
let backup = controller.export_permissions().await;
println!("Backed up {} capabilities", backup.len());

// Make changes
controller.grant("write", "did:key:eve").await?;
controller.clear_capability("admin").await?;

// Restore
controller.import_permissions(backup).await?;
println!("Restored from backup");
```

---

### Statistics and Monitoring

```rust
// Get statistics
let stats = controller.get_stats().await;
for (capability, count) in &stats {
    println!("{}: {} users", capability, count);
}

let total = controller.total_permissions().await;
println!("Total permissions: {}", total);

// Check if roles need assignment
if controller.is_capability_empty("moderator").await {
    println!("WARNING: No moderators assigned!");
}
```

---

### Entry Validation

```rust
use guardian_db::ipfs_log::entry::Entry;

// Setup controller
controller.grant("write", "did:key:alice").await?;

// Validate entry
let entry: Box<dyn LogEntry> = ...; // Your entry
let identity_provider: Box<dyn IdentityProvider> = ...; // Your provider
let context: Box<dyn CanAppendAdditionalContext> = ...; // Context

match controller.can_append(
    entry.as_ref(),
    identity_provider.as_ref(),
    context.as_ref()
).await {
    Ok(()) => {
        println!("Entry authorized");
        // Add to log...
    }
    Err(e) => {
        println!("Entry rejected: {}", e);
        // Handle rejection...
    }
}
```

---

### Custom Roles and Workflows

```rust
// Create custom role hierarchy
controller.grant("owner", "did:key:alice").await?;

// Owners can do everything
controller.clone_capability("owner", "admin").await?;
controller.clone_capability("owner", "write").await?;
controller.clone_capability("owner", "read").await?;

// Moderators inherit write
controller.grant("moderator", "did:key:bob").await?;
controller.clone_capability("write", "moderator").await?;

// Check custom capability
async fn check_custom_permission(
    controller: &SimpleAccessController,
    user_id: &str,
    action: &str,
) -> bool {
    controller.has_capability(action, user_id).await
}

if check_custom_permission(&controller, "did:key:alice", "owner").await {
    println!("User has owner privileges");
}
```

---

## Comparison with Other Controllers

### vs GuardianDBAccessController

| Aspect | SimpleAccessController | GuardianDBAccessController |
|--------|------------------------|----------------------------|
| **Storage** | In-memory HashMap | Distributed KeyValueStore |
| **Persistence** | None (ephemeral) | Automatic (IPFS) |
| **Performance** | Fastest (no I/O) | Slower (network I/O) |
| **Replication** | None | Automatic P2P sync |
| **Events** | None | EventBus integration |
| **Setup** | Trivial | Complex (needs DB) |
| **Use Case** | Dev/test/single-node | Production/multi-node |

---

### vs IpfsAccessController

| Aspect | SimpleAccessController | IpfsAccessController |
|--------|------------------------|----------------------|
| **Storage** | In-memory | IPFS CBOR documents |
| **Capabilities** | Unlimited custom | Only "write" |
| **Versioning** | None | Immutable CIDs |
| **Sharing** | Not shareable | Share CID |
| **Complexity** | Minimal | Medium |
| **Updates** | Instant | Create new CID |
| **Use Case** | Quick prototypes | IPFS-native apps |

---

## Best Practices

### 1. Initialize with Default Permissions

```rust
// Good: Set up basic permissions
let mut initial = HashMap::new();
initial.insert("write".to_string(), vec!["did:key:admin".to_string()]);
let controller = SimpleAccessController::new(Some(initial));

// Bad: Empty controller might deny all access
let controller = SimpleAccessController::new_simple();
// Forgot to grant any permissions!
```

### 2. Use Bulk Operations

```rust
// Good: Efficient bulk grant
controller.grant_multiple("write", vec!["alice", "bob", "charlie"]).await?;

// Bad: Multiple individual operations
for user in &["alice", "bob", "charlie"] {
    controller.grant("write", user).await?; // 3 write locks!
}
```

### 3. Export Permissions for Persistence

```rust
// Good: Backup before shutdown
let permissions = controller.export_permissions().await;
let json = serde_json::to_string(&permissions)?;
std::fs::write("permissions_backup.json", json)?;

// On restart
let json = std::fs::read_to_string("permissions_backup.json")?;
let permissions: HashMap<String, Vec<String>> = serde_json::from_str(&json)?;
controller.import_permissions(permissions).await?;
```

### 4. Check Before Grant/Revoke

```rust
// Good: Check first to avoid duplicate operations
if !controller.has_capability("write", "alice").await {
    controller.grant("write", "alice").await?;
}

// Bad: Always grant (no-op if exists, but unnecessary lock)
controller.grant("write", "alice").await?; // Might already have it
```

### 5. Use Statistics for Monitoring

```rust
// Good: Monitor permission health
let stats = controller.get_stats().await;
if stats.get("admin").copied().unwrap_or(0) == 0 {
    warn!("No administrators assigned!");
}

// Good: Log statistics periodically
tokio::spawn(async move {
    loop {
        tokio::time::sleep(Duration::from_secs(300)).await;
        let total = controller.total_permissions().await;
        info!("Current permissions: {}", total);
    }
});
```

---

## Tracing and Observability

### Instrumentation

All operations use structured logging with the `simple_access_controller` target:

**Log Levels:**
- **INFO**: High-level operations (grant, revoke, create)
- **DEBUG**: Detailed operations (permission checks, lookups)
- **WARN**: Warnings (invalid signatures, denied access)

### Example Logs

```
INFO simple_access_controller: Created SimpleAccessController categories=["read", "write", "admin"] total_permissions=0
INFO simple_access_controller: Granting permission: capability="write", key_id="did:key:alice"
DEBUG simple_access_controller: Permission granted successfully: capability="write" key_id="did:key:alice" total_keys=1
DEBUG simple_access_controller: Checking append permission: entry_id="did:key:alice"
DEBUG simple_access_controller: Append permission granted (write key): entry_id="did:key:alice"
WARN simple_access_controller: Access denied for append operation: entry_id="did:key:eve" available_write_keys=["did:key:alice"]
```

### Span Context

Use the controller's span for custom instrumentation:

```rust
let _entered = controller.span().enter();
// Your code here will be traced under the controller's span
```

---

## Testing

### Unit Test Pattern

```rust
#[tokio::test]
async fn test_grant_and_revoke() {
    let controller = SimpleAccessController::new_simple();
    
    // Grant
    controller.grant("write", "alice").await.unwrap();
    assert!(controller.has_capability("write", "alice").await);
    
    // Revoke
    controller.revoke("write", "alice").await.unwrap();
    assert!(!controller.has_capability("write", "alice").await);
}
```

### Integration Test Pattern

```rust
#[tokio::test]
async fn test_entry_validation() {
    let controller = SimpleAccessController::new_simple();
    controller.grant("write", test_identity_id()).await.unwrap();
    
    let entry = create_test_entry();
    let identity_provider = create_test_identity_provider();
    let context = create_test_context();
    
    let result = controller.can_append(&entry, &identity_provider, &context).await;
    assert!(result.is_ok());
}
```

---

## Limitations

1. **No Persistence**: All data lost on drop
2. **No Events**: Cannot audit or monitor changes
3. **No Replication**: Single-node only
4. **No Versioning**: Cannot rollback permissions
5. **Memory-Bound**: All permissions must fit in RAM

---

## See Also

- [ACCESS_CONTROLLER_TRAITS.md](./ACCESS_CONTROLLER_TRAITS.md) - AccessController trait
- [GUARDIAN_DB_ACCESS_CONTROLLER.md](./GUARDIAN_DB_ACCESS_CONTROLLER.md) - Persistent controller
- [IPFS_ACCESS_CONTROLLER_IMPLEMENTATION.md](./IPFS_ACCESS_CONTROLLER_IMPLEMENTATION.md) - IPFS controller
- [ACCESS_CONTROLLER_UTILS.md](./ACCESS_CONTROLLER_UTILS.md) - Utility functions
- [ACCESS_CONTROLLER_MANIFEST.md](./ACCESS_CONTROLLER_MANIFEST.md) - Manifest system
