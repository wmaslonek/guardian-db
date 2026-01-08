    // ╔════════════════════════════════════════════════════════════════════════════════╗
    // ║                          ⚠ OUTDATED DOCUMENTATION                             ║
    // ╚════════════════════════════════════════════════════════════════════════════════╝


# GuardianDBAccessController

## Overview

The `GuardianDBAccessController` is the most advanced access control implementation in GuardianDB. Unlike the `SimpleAccessController` (which keeps permissions in memory), this controller persists all permissions in a **distributed KeyValueStore**, offering:

- **Durable Persistence**: All permissions are stored in IPFS via KeyValueStore
- **P2P Synchronization**: Permissions are replicated between network nodes
- **Event System**: Emits events for auditing and change monitoring
- **Control Granularity**: Supports multiple roles with specific permissions

## Data Structure

### GuardianDBAccessController

```rust
pub struct GuardianDBAccessController {
    event_bus: EventBus,
    event_emitter: Arc<Mutex<Option<Emitter<EventUpdated>>>>,
    guardian_db: Arc<dyn GuardianDBKVStoreProvider<Error = GuardianError>>,
    kv_store: RwLock<Option<Arc<Mutex<Box<dyn KeyValueStore<Error = GuardianError>>>>>>,
    options: Box<dyn ManifestParams>,
    span: Span,
}
```

**Fields:**
- `event_bus` - EventBus for type-safe event communication
- `event_emitter` - Lazy-initialized emitter for `EventUpdated` events
- `guardian_db` - Provider to create/access KeyValueStores
- `kv_store` - Persistent store for permissions (with RwLock for concurrency)
- `options` - Manifest configuration parameters
- `span` - Tracing context for observability

### EventUpdated

```rust
#[derive(Debug, Clone)]
pub struct EventUpdated {
    pub controller_type: String,
    pub address: String,
    pub action: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}
```

Event emitted when permissions are modified (grant/revoke).

---

## Public API

### Constructor

#### `new()`

Creates a new instance of GuardianDBAccessController.

```rust
pub async fn new(
    guardian_db: Arc<dyn GuardianDBKVStoreProvider<Error = GuardianError>>,
    params: Box<dyn ManifestParams>,
) -> Result<Self>
```

**Parameters:**
- `guardian_db` - Provider to create KeyValueStore
- `params` - Configuration parameters including address and initial permissions

**Behavior:**
1. Creates or opens a KeyValueStore for the specified address
2. Initializes EventBus for event system
3. Applies initial "write" permissions from manifest
4. Sets up tracing span

**Example:**
```rust
use guardian_db::access_controller::{
    guardian::GuardianDBAccessController,
    manifest::CreateAccessControllerOptions,
};

let params = CreateAccessControllerOptions::new(
    cid::Cid::default(),
    false,
    "guardian".to_string(),
);

let controller = GuardianDBAccessController::new(
    guardian_db.clone(),
    Box::new(params),
).await?;
```

---

### Type and Address Query

#### `get_type()`

Returns the controller type.

```rust
pub fn get_type(&self) -> &'static str
```

**Returns:** `"GuardianDB"`

#### `address()`

Returns the address of the underlying KeyValueStore.

```rust
pub async fn address(&self) -> Option<Box<dyn Address>>
```

**Returns:**
- `Some(Box<dyn Address>)` - Store address
- `None` - Store not initialized

---

### Permission Management

#### `grant()`

Grants a permission (capability) to a specific key.

```rust
pub async fn grant(&self, capability: &str, key_id: &str) -> Result<()>
```

**Parameters:**
- `capability` - Permission name (e.g., "write", "admin", "read")
- `key_id` - Key identifier (DID, public key, or "*" for universal access)

**Behavior:**
1. Loads existing permissions for the capability
2. Adds `key_id` to the set (avoids duplicates with HashSet)
3. Serializes and persists to KeyValueStore
4. Emits `EventUpdated` event with "grant" action

**Example:**
```rust
// Grant write permission to a specific key
controller.grant("write", "did:key:z6Mkk...").await?;

// Grant admin permission to another key
controller.grant("admin", "did:key:z6Mkj...").await?;

// Universal access
controller.grant("write", "*").await?;
```

---

#### `revoke()`

Revokes a previously granted permission.

```rust
pub async fn revoke(&self, capability: &str, key_id: &str) -> Result<()>
```

**Parameters:**
- `capability` - Permission name
- `key_id` - Key identifier to be removed

**Behavior:**
1. Loads existing permissions
2. Removes `key_id` from the list
3. If the list becomes empty, removes the entry completely
4. Otherwise, persists the updated list
5. Emits `EventUpdated` event with "revoke" action

**Example:**
```rust
// Remove write permission
controller.revoke("write", "did:key:z6Mkk...").await?;

// Remove admin permission
controller.revoke("admin", "did:key:z6Mkj...").await?;
```

---

#### `get_authorized_by_role()`

Returns all keys authorized for a specific role.

```rust
pub async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>>
```

**Parameters:**
- `role` - Role name ("write", "admin", etc.)

**Returns:**
- `Vec<String>` - List of authorized key_ids (empty if role doesn't exist)

**Example:**
```rust
let writers = controller.get_authorized_by_role("write").await?;
println!("Authorized writers: {:?}", writers);

let admins = controller.get_authorized_by_role("admin").await?;
println!("Authorized admins: {:?}", admins);
```

---

### Entry Validation

#### `can_append()`

Checks if an entry (LogEntry) can be added to the log, validating identity and permissions.

```rust
pub async fn can_append(
    &self,
    entry: &dyn LogEntry,
    identity_provider: &dyn IdentityProvider,
    _additional_context: &dyn CanAppendAdditionalContext,
) -> Result<()>
```

**Parameters:**
- `entry` - Entry to be validated
- `identity_provider` - Provider for identity verification
- `_additional_context` - Additional context (currently unused)

**Returns:**
- `Ok(())` - Entry authorized and identity verified
- `Err(GuardianError)` - Entry rejected or invalid identity

**Authorization Logic:**
1. Loads "write" and "admin" permissions
2. Combines both into a single set
3. Extracts identity ID from the entry
4. Checks if ID is in the set OR if "*" is present
5. Validates identity signature using IdentityProvider
6. Returns Ok if authorized, Err otherwise

**Example:**
```rust
match controller.can_append(&entry, &identity_provider, &context).await {
    Ok(()) => {
        println!("Entry authorized - adding to log");
        log.add_entry(entry).await?;
    }
    Err(e) => {
        println!("Entry rejected: {}", e);
    }
}
```

---

### Persistence and Loading

#### `load()`

Loads an existing KeyValueStore for the access controller.

```rust
pub async fn load(&self, address: &str) -> Result<()>
```

**Parameters:**
- `address` - KeyValueStore address (will be suffixed with `/_access`)

**Behavior:**
1. Closes any existing store
2. Normalizes the address using `utils::ensure_address()`
3. Sets up access controller for the new store
4. Opens/creates the KeyValueStore
5. Replaces the internal store

**Example:**
```rust
// Load permissions from another store
controller.load("my-database").await?;

// Now the controller uses permissions persisted in "my-database/_access"
let writers = controller.get_authorized_by_role("write").await?;
```

---

#### `save()`

Saves the current state and returns the manifest parameters.

```rust
pub async fn save(&self) -> Result<Box<dyn ManifestParams>>
```

**Returns:**
- `Box<dyn ManifestParams>` - Manifest parameters including the store address

**Behavior:**
1. Gets address from internal KeyValueStore
2. Creates manifest parameters with type "GuardianDB"
3. Returns for serialization/storage in IPFS

**Note:** Permissions are automatically persisted to the KeyValueStore during `grant()` and `revoke()`. This method only returns metadata for the manifest system.

---

#### `close()`

Closes the KeyValueStore and releases resources.

```rust
pub async fn close(&self) -> Result<()>
```

**Behavior:**
1. Acquires write lock on kv_store
2. Removes the store from the struct (Option::take)
3. Calls `close()` on the KeyValueStore
4. Logs success or warning in case of error

**Example:**
```rust
// Close the controller at the end of use
controller.close().await?;
```

---

## Permission System

### Authorization Model

The GuardianDBAccessController uses a model based on **capabilities (roles)**:

```
Role         | Description
-------------|-----------------------------------------------------
"write"      | Allows adding entries to the log
"admin"      | Inherits "write" + permission management
"read"       | (Customizable) Allows reading
<custom>     | Any string can be used as a role
```

### Permission Inheritance

**Special Rule:**
- Users with `"write"` permission automatically receive `"admin"` when querying authorizations
- Implemented in `get_authorizations()`:

```rust
// If 'write' permission exists, grant the same keys for 'admin'
if let Some(write_keys) = authorizations_set.get("write").cloned() {
    let admin_keys = authorizations_set.entry("admin".to_string()).or_default();
    for key in write_keys.iter() {
        admin_keys.insert(key.clone());
    }
}
```

### Universal Access

The key `"*"` grants universal access:

```rust
controller.grant("write", "*").await?; // Any identity can write
```

---

## Persistence Architecture

### Structure in KeyValueStore

Permissions are stored as key-value pairs:

```
Key (capability)  | Value (JSON array of key_ids)
------------------|---------------------------------
"write"           | ["did:key:z6Mkk...", "did:key:z6Mkj..."]
"admin"           | ["did:key:z6Mkk..."]
"custom_role"     | ["*"]
```

### Serialization Format

```rust
// Permissions for "write"
let capabilities = vec![
    "did:key:z6Mkk...".to_string(),
    "did:key:z6Mkj...".to_string(),
];

let json_bytes = serde_json::to_vec(&capabilities)?;
// Result: [34,100,105,100,58,107,101,121,58,122,54,77,107,107,46,46,46,34,...]
```

### Replication

Since the KeyValueStore is distributed:
1. Permissions are synchronized via IPFS/libp2p
2. Multiple nodes can share the same access controller
3. Changes (grant/revoke) are automatically propagated

---

## Event System

### EventBus Integration

The controller uses the type-safe `EventBus` to emit events:

```rust
// Event structure
pub struct EventUpdated {
    pub controller_type: String,  // "guardian"
    pub address: String,           // Store address
    pub action: String,            // "grant:write:did:key:..." or "revoke:admin:..."
    pub timestamp: DateTime<Utc>,  // UTC timestamp
}
```

### Event Subscription

```rust
use guardian_db::access_controller::guardian::EventUpdated;

// Subscribe to update events
let mut receiver = controller.event_bus.subscribe::<EventUpdated>().await?;

// Asynchronous listener
tokio::spawn(async move {
    while let Ok(event) = receiver.recv().await {
        println!(
            "[{}] {} on {} at {}",
            event.controller_type,
            event.action,
            event.address,
            event.timestamp
        );
    }
});
```

### Event Use Cases

1. **Auditing**: Record all permission changes
2. **Monitoring**: Alert on suspicious grants/revocations
3. **UI Synchronization**: Update interfaces in real-time
4. **Logging**: Maintain access control history

---

## Concurrency and Thread Safety

### Locking Strategy

```rust
// kv_store uses RwLock to allow concurrent reads
kv_store: RwLock<Option<Arc<Mutex<Box<dyn KeyValueStore>>>>>
```

**Benefits:**
- **Read Lock** (`get_authorized_by_role`, `can_append`): Multiple simultaneous reads
- **Write Lock** (`load`, `close`): Exclusivity for destructive operations
- **Internal Mutex**: Protects KeyValueStore operations

### Safe Usage Pattern

```rust
// Read (multiple threads OK)
{
    let store_guard = self.kv_store.read().await;
    let store = store_guard.as_ref().ok_or(...)?;
    // Use the store
} // Lock released automatically

// Write (exclusive)
{
    let mut store_guard = self.kv_store.write().await;
    *store_guard = Some(new_store);
} // Lock released
```

---

## Tracing and Observability

### Instrumentation

All main operations are instrumented with `#[instrument]`:

```rust
#[instrument(skip(self), fields(capability = %capability, key_id = %key_id))]
pub async fn grant(&self, capability: &str, key_id: &str) -> Result<()>
```

### Log Levels

- **INFO**: Creation, initialization
- **DEBUG**: Successful operations, intermediate states
- **WARN**: Fallbacks, non-fatal errors
- **ERROR**: Critical failures

### Log Examples

```
DEBUG access_controller: Key-value store initialized address="my-db/_access"
DEBUG GuardianDB::ac: Event emitted successfully action="grant" capability="write" key_id="did:key:..."
WARN GuardianDB::ac: Failed to emit update event error="channel closed"
```

---

## AccessController Trait Implementation

The `GuardianDBAccessController` implements the `AccessController` trait:

```rust
#[async_trait::async_trait]
impl AccessController for GuardianDBAccessController {
    fn get_type(&self) -> &str;
    async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>>;
    async fn grant(&self, capability: &str, key_id: &str) -> Result<()>;
    async fn revoke(&self, capability: &str, key_id: &str) -> Result<()>;
    async fn load(&self, address: &str) -> Result<()>;
    async fn save(&self) -> Result<Box<dyn ManifestParams>>;
    async fn close(&self) -> Result<()>;
    async fn can_append(...) -> Result<()>;
}
```

This allows polymorphic usage with other controllers.

---

## Helper Types

### StringAddress

Internal wrapper to implement the `Address` trait:

```rust
#[derive(Debug, Clone)]
struct StringAddress(String);

impl Address for StringAddress {
    fn get_root(&self) -> cid::Cid { cid::Cid::default() }
    fn get_path(&self) -> &str { &self.0 }
    fn equals(&self, other: &dyn Address) -> bool { ... }
}
```

Used to return KeyValueStore addresses as `Box<dyn Address>`.

---

## Complete Examples

### Create and Configure Controller

```rust
use guardian_db::{
    GuardianDB,
    access_controller::{
        guardian::GuardianDBAccessController,
        manifest::CreateAccessControllerOptions,
    },
};

// Initialize GuardianDB
let ipfs = IrohClient::development().await?;
let db = GuardianDB::new(ipfs, None).await?;

// Create manifest parameters
let params = CreateAccessControllerOptions::new(
    cid::Cid::default(),
    false,
    "guardian".to_string(),
);

// Create the controller
let controller = GuardianDBAccessController::new(
    Arc::new(db.base_guardian_db()),
    Box::new(params),
).await?;

// Set up permissions
controller.grant("write", "did:key:z6Mkk...").await?;
controller.grant("admin", "did:key:z6Mkj...").await?;
```

---

### Validate Entries with Auditing

```rust
use guardian_db::access_controller::guardian::EventUpdated;

// Subscribe to events for auditing
let mut audit_log = controller.event_bus.subscribe::<EventUpdated>().await?;

tokio::spawn(async move {
    while let Ok(event) = audit_log.recv().await {
        println!("[AUDIT] {} - {}", event.timestamp, event.action);
    }
});

// Grant permission (will be audited)
controller.grant("write", "did:key:z6Mka...").await?;

// Validate entry
let result = controller.can_append(&entry, &identity_provider, &context).await;
match result {
    Ok(()) => println!("Entry authorized"),
    Err(e) => println!("Entry rejected: {}", e),
}

// Revoke permission (will be audited)
controller.revoke("write", "did:key:z6Mka...").await?;
```

---

### Permission Migration

```rust
// Load permissions from an old store
controller.load("legacy-database").await?;

// Copy permissions
let writers = controller.get_authorized_by_role("write").await?;
let admins = controller.get_authorized_by_role("admin").await?;

// Create new controller for new store
let new_controller = GuardianDBAccessController::new(
    guardian_db.clone(),
    Box::new(new_params),
).await?;

// Replicate permissions
for writer in writers {
    new_controller.grant("write", &writer).await?;
}
for admin in admins {
    new_controller.grant("admin", &admin).await?;
}

println!("Migration completed");
```

---

## Comparison with Other Controllers

### vs SimpleAccessController

| Aspect                | GuardianDBAccessController        | SimpleAccessController  |
|-----------------------|-----------------------------------|-------------------------|
| **Persistence**       | Distributed KeyValueStore (IPFS) | In memory (HashMap)     |
| **Replication**       | Automatic via P2P                 | Not supported           |
| **Events**            | Type-safe EventBus                | None                    |
| **Concurrency**       | RwLock + Mutex                    | Simple RwLock           |
| **Complexity**        | High (requires GuardianDB provider) | Low (standalone)      |
| **Recommended Use**   | Production, multi-node            | Development, testing    |

### vs IPFSAccessController

| Aspect                | GuardianDBAccessController  | IPFSAccessController    |
|-----------------------|-----------------------------|-------------------------|
| **Backend**           | GuardianDB KeyValueStore    | Direct IPFS             |
| **Status**            | Production (implemented)    | Planned (TODO)          |
| **Dependencies**      | GuardianDB + IPFS           | IPFS only               |

---

## Limitations and TODOs

### Current Limitations

1. **Additional Context Not Used**: The `_additional_context` parameter in `can_append()` is currently ignored

2. **Load Fallback**: If there's an error closing the old store, the error is silently ignored

3. **Default CID in Save**: Uses `cid::Cid::default()` when creating manifest - should generate real CID

### Planned TODOs

```rust
// TODO: Implement additional_context verification
pub async fn can_append(
    &self,
    entry: &dyn LogEntry,
    identity_provider: &dyn IdentityProvider,
    additional_context: &dyn CanAppendAdditionalContext, // ⬅ Use this
) -> Result<()>

// TODO: Handle error when closing old store
pub async fn load(&self, address: &str) -> Result<()> {
    // ...
    if let Some(_store) = store_guard.take() {
        // ⬅ Propagate error?
    }
}

// TODO: Generate real CID based on store address
pub async fn save(&self) -> Result<Box<dyn ManifestParams>> {
    // ...
    let cid = cid::Cid::default(); // ⬅ Calculate real CID
}
```

---

## See Also

- [ACCESS_CONTROLLER_TRAITS.md](./ACCESS_CONTROLLER_TRAITS.md) - AccessController trait
- [SIMPLE_ACCESS_CONTROLLER_IMPLEMENTATION.md](./SIMPLE_ACCESS_CONTROLLER_IMPLEMENTATION.md) - Simple controller
- [ACCESS_CONTROLLER_UTILS.md](./ACCESS_CONTROLLER_UTILS.md) - Utilities
- [ACCESS_CONTROLLER_MANIFEST.md](./ACCESS_CONTROLLER_MANIFEST.md) - Manifest system
