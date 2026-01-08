    // ╔════════════════════════════════════════════════════════════════════════════════╗
    // ║                          ⚠ OUTDATED DOCUMENTATION                             ║
    // ╚════════════════════════════════════════════════════════════════════════════════╝


# IrohAccessController

## Overview

The `IrohAccessController` is a lightweight, IPFS-native access control implementation that stores permissions directly in the IPFS network. Unlike `GuardianDBAccessController` (which uses a distributed KeyValueStore), this controller:

- **Stores permissions as IPFS objects**: Uses content-addressed storage via CIDs
- **Immutable manifests**: Each permission set is stored as an immutable CBOR document
- **Minimalist design**: Only supports "write" capability by default
- **Read-only in-memory cache**: Fast permission checks with RwLock-protected state

## Architecture

### Data Structures

#### IrohAccessController

```rust
pub struct IrohAccessController {
    ipfs: Arc<IrohClient>,
    state: RwLock<ControllerState>,
    span: Span,
}
```

**Fields:**
- `ipfs` - Shared IPFS client for reading/writing permission manifests
- `state` - In-memory cache of permissions protected by RwLock
- `span` - Tracing span for structured observability

#### ControllerState

```rust
struct ControllerState {
    write_access: Vec<String>,
}
```

Internal state holding the list of authorized key IDs for write operations.

#### CborWriteAccess

```rust
#[derive(Debug, Serialize, Deserialize)]
struct CborWriteAccess {
    #[serde(rename = "write")]
    write: String,  // JSON string containing Vec<String>
}
```

CBOR serialization wrapper for storing permissions in IPFS. The `write` field contains a JSON-encoded array of key IDs.

---

## Public API

### Constructor

#### `new()`

Creates a new IPFS access controller instance.

```rust
pub fn new(
    ipfs_client: Arc<IrohClient>,
    identity_id: String,
    mut params: CreateAccessControllerOptions,
) -> Result<Self>
```

**Parameters:**
- `ipfs_client` - Shared IPFS client for manifest operations
- `identity_id` - Identity ID to grant initial write access (if none specified)
- `params` - Configuration options including initial permissions

**Behavior:**
1. If no "write" access is specified, grants write to `identity_id`
2. Initializes in-memory state with permissions from params
3. Creates tracing span for observability

**Example:**
```rust
use guardian_db::{
    ipfs_core_api::IrohClient,
    access_controller::{
        ipfs::IrohAccessController,
        manifest::CreateAccessControllerOptions,
    },
};

let ipfs = IrohClient::development().await?;
let identity_id = "did:key:z6Mkk...".to_string();

let mut params = CreateAccessControllerOptions::default();
params.set_access("write".to_string(), vec![
    identity_id.clone(),
    "did:key:z6Mkj...".to_string(),
]);

let controller = IrohAccessController::new(
    Arc::new(ipfs),
    identity_id,
    params,
)?;
```

---

### Type and Address

#### `get_type()`

Returns the controller type identifier.

```rust
pub fn get_type(&self) -> &'static str
```

**Returns:** `"ipfs"`

#### `address()`

Returns the address of the controller.

```rust
pub fn address(&self) -> Option<Box<dyn Address>>
```

**Returns:** `None` - IPFS controllers are content-addressed by CID, not by traditional addresses

---

### Permission Management

#### `grant()`

Grants write permission to a key ID.

```rust
pub async fn grant(&self, capability: &str, key_id: &str) -> Result<()>
```

**Parameters:**
- `capability` - Must be `"write"` (other capabilities not supported)
- `key_id` - Identity key to authorize (or `"*"` for universal access)

**Behavior:**
1. Validates capability is "write"
2. Acquires write lock on state
3. Adds key_id to write_access if not already present
4. Logs result (grant or already exists)

**Important:** Changes are only in memory until `save()` is called!

**Example:**
```rust
// Grant write access to specific identity
controller.grant("write", "did:key:z6Mkk...").await?;

// Grant universal write access
controller.grant("write", "*").await?;

// Save to IPFS to persist changes
let manifest = controller.save().await?;
println!("Permissions saved at CID: {}", manifest.address());
```

---

#### `revoke()`

Revokes write permission from a key ID.

```rust
pub async fn revoke(&self, capability: &str, key_id: &str) -> Result<()>
```

**Parameters:**
- `capability` - Must be `"write"`
- `key_id` - Identity key to remove

**Behavior:**
1. Validates capability is "write"
2. Acquires write lock on state
3. Removes key_id from write_access list
4. Logs result (revoked or not found)

**Important:** Changes are only in memory until `save()` is called!

**Example:**
```rust
// Revoke write access
controller.revoke("write", "did:key:z6Mkk...").await?;

// Save changes to IPFS
let manifest = controller.save().await?;
```

---

#### `get_authorized_by_role()`

Returns all key IDs authorized for a given role.

```rust
pub async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>>
```

**Parameters:**
- `role` - Role name: `"write"`, `"read"`, or `"admin"`

**Returns:**
- `Vec<String>` - List of authorized key IDs
- Empty vector for unsupported roles

**Role Mapping:**
- `"write"` → Returns write_access list
- `"read"` → Returns write_access list (write implies read)
- `"admin"` → Returns write_access list (write implies admin)
- Other roles → Returns empty list

**Example:**
```rust
let writers = controller.get_authorized_by_role("write").await?;
println!("Authorized writers: {:?}", writers);

let admins = controller.get_authorized_by_role("admin").await?;
// Same as writers for IPFS controller
```

---

### Entry Validation

#### `can_append()`

Validates whether a log entry can be appended based on its identity.

```rust
pub async fn can_append(
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
- `Ok(())` - Entry authorized and identity verified
- `Err(GuardianError)` - Entry rejected or invalid identity

**Authorization Logic:**
1. Extract entry's identity ID
2. Check if ID exists in write_access OR if `"*"` is present
3. If authorized, verify identity signature
4. Return Ok on success, Err on failure

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

### IPFS Persistence

#### `save()`

Saves the current permission state to IPFS and returns the manifest CID.

```rust
pub async fn save(&self) -> Result<CreateAccessControllerOptions>
```

**Returns:**
- `CreateAccessControllerOptions` - Manifest params containing the IPFS CID

**Persistence Format:**
```
1. write_access Vec → JSON string → CborWriteAccess struct → CBOR bytes
2. CBOR bytes → IPFS.add() → CID
3. Return CreateAccessControllerOptions with CID
```

**Example:**
```rust
// Modify permissions
controller.grant("write", "did:key:z6Mkk...").await?;
controller.grant("write", "did:key:z6Mkj...").await?;

// Save to IPFS
let manifest = controller.save().await?;
let cid = manifest.address();
println!("Permissions saved at: {}", cid);

// Share CID with other nodes to load same permissions
```

---

#### `load()`

Loads permission state from an IPFS CID.

```rust
pub async fn load(&self, address: &str) -> Result<()>
```

**Parameters:**
- `address` - IPFS CID string of the manifest

**Loading Process:**
```
1. Parse CID from address string
2. Read manifest CBOR from IPFS
3. Deserialize Manifest structure
4. Extract permissions address from manifest
5. Read permissions CBOR from IPFS
6. Deserialize CborWriteAccess
7. Parse JSON to Vec<String>
8. Update internal state
```

**Technical Details:**
- Uses `spawn_blocking` for non-Send IPFS operations
- Two-stage loading: manifest → permissions
- Nested serialization: CBOR(JSON(Vec))

**Example:**
```rust
// Load permissions from previously saved CID
let cid = "bafyreib...";
controller.load(cid).await?;

// Check loaded permissions
let writers = controller.get_authorized_by_role("write").await?;
println!("Loaded {} writers", writers.len());
```

---

#### `close()`

Closes the controller and releases resources.

```rust
pub async fn close(&self) -> Result<()>
```

**Behavior:**
- No-op operation for IPFS controller
- Logs current state for debugging
- Always returns Ok

**Note:** IPFS-based controllers are stateless from a resource perspective. State is stored in IPFS, not held locally.

**Example:**
```rust
controller.close().await?;
```

---

## Permission Model

### Capabilities

The IrohAccessController only supports one capability:

| Capability | Description | Implementation |
|------------|-------------|----------------|
| `"write"` | Allow appending entries to log | Enforced in `can_append()` |

**Role Aliases:**
- `"admin"` maps to `"write"` permissions
- `"read"` maps to `"write"` permissions (write implies read)

### Universal Access

The special key `"*"` grants access to all identities:

```rust
controller.grant("write", "*").await?;
// Now ANY identity can append entries
```

### Capability Restriction

Attempting to use unsupported capabilities returns an error:

```rust
controller.grant("custom", "did:key:...").await?;
// Error: "IrohAccessController only supports 'write' capability, got 'custom'"
```

---

## IPFS Storage Format

### Manifest Structure

```rust
Manifest {
    type: "ipfs",
    params: {
        address: CID,  // Points to CborWriteAccess CBOR document
    }
}
```

### Permissions Document

```cbor
{
  "write": "[\"did:key:z6Mkk...\",\"did:key:z6Mkj...\",\"*\"]"
}
```

**Note:** The write field contains a JSON-encoded string, not a direct array. This is for compatibility with OrbitDB format.

### Storage Example

```rust
// 1. In-memory state
write_access: vec![
    "did:key:z6Mkk...".to_string(),
    "did:key:z6Mkj...".to_string(),
]

// 2. JSON encoding
let json_str = "[\"did:key:z6Mkk...\",\"did:key:z6Mkj...\"]";

// 3. CBOR wrapper
let cbor = CborWriteAccess { write: json_str };

// 4. Serialized CBOR bytes
let bytes = serde_cbor::to_vec(&cbor)?;

// 5. Store in IPFS
let response = ipfs.add_bytes(bytes).await?;
// Returns: bafyreib...
```

---

## Concurrency and Thread Safety

### Locking Strategy

```rust
state: RwLock<ControllerState>
```

**Benefits:**
- **Read operations** (`can_append`, `get_authorized_by_role`): Multiple concurrent reads
- **Write operations** (`grant`, `revoke`): Exclusive access during modification

### Lock Scope Example

```rust
// Read lock - allows concurrency
pub async fn can_append(&self, ...) -> Result<()> {
    let state = self.state.read().await;
    // Check permissions with read lock
    for allowed_key in state.write_access.iter() {
        // ...
    }
} // Read lock automatically released

// Write lock - exclusive
pub async fn grant(&self, ...) -> Result<()> {
    let mut state = self.state.write().await;
    state.write_access.push(key_id.to_string());
} // Write lock automatically released
```

### Blocking Operations

IPFS operations are non-Send, requiring `spawn_blocking`:

```rust
let response = tokio::task::spawn_blocking(move || {
    let rt = tokio::runtime::Handle::current();
    rt.block_on(async move {
        ipfs.add_bytes(cbor_bytes).await
    })
})
.await??;
```

This pattern:
1. Spawns a blocking task
2. Uses current runtime handle
3. Runs async IPFS code in blocking context
4. Unwraps nested Result with `??`

---

## Tracing and Observability

### Instrumentation

All public methods use `#[instrument]` for automatic tracing:

```rust
#[instrument(skip(self, entry, identity_provider, _additional_context))]
pub async fn can_append(...) -> Result<()>
```

### Target and Span

- **Target**: `ipfs_access_controller`
- **Span**: Created in constructor with `controller_type = "ipfs"`

### Log Levels

- **DEBUG**: Successful operations, state changes
- **WARN**: Warnings (currently none)
- **ERROR**: Fatal errors (propagated as Result::Err)

### Example Logs

```
DEBUG ipfs_access_controller: Permission granted successfully capability="write" key_id="did:key:z6Mkk..." total_keys=3
DEBUG ipfs_access_controller: Permission already exists capability="write" key_id="did:key:z6Mkk..."
DEBUG ipfs_access_controller: Permission revoked successfully capability="write" key_id="did:key:z6Mkj..." remaining_keys=2
DEBUG ipfs_access_controller: IPFS access controller saved cid="bafyreib..."
DEBUG ipfs_access_controller: Closing IPFS access controller write_access_count=2
```

---

## AccessController Trait Implementation

The `IrohAccessController` implements the `AccessController` trait:

```rust
#[async_trait]
impl AccessController for IrohAccessController {
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

This enables polymorphic usage with other controllers like `SimpleAccessController` and `GuardianDBAccessController`.

---

## Complete Examples

### Create and Configure Controller

```rust
use guardian_db::{
    ipfs_core_api::IrohClient,
    access_controller::{
        ipfs::IrohAccessController,
        manifest::CreateAccessControllerOptions,
    },
};
use std::sync::Arc;

// Initialize IPFS client
let ipfs = Arc::new(IrohClient::development().await?);

// Setup initial permissions
let identity_id = "did:key:z6MkkSample123".to_string();
let mut params = CreateAccessControllerOptions::default();
params.set_access("write".to_string(), vec![
    identity_id.clone(),
    "did:key:z6MkkOtherUser".to_string(),
]);

// Create controller
let controller = IrohAccessController::new(
    ipfs.clone(),
    identity_id,
    params,
)?;

println!("Controller type: {}", controller.get_type());
```

---

### Modify and Persist Permissions

```rust
// Grant additional permissions
controller.grant("write", "did:key:z6MkkNewUser").await?;
controller.grant("write", "did:key:z6MkkAnotherUser").await?;

// Check current permissions
let writers = controller.get_authorized_by_role("write").await?;
println!("Current writers: {}", writers.len());

// Save to IPFS
let manifest = controller.save().await?;
let cid = manifest.address().to_string();
println!("Permissions saved at CID: {}", cid);

// Share CID with other nodes...
```

---

### Load and Verify Permissions

```rust
// Create new controller instance
let controller2 = IrohAccessController::new(
    ipfs.clone(),
    "temporary".to_string(),
    CreateAccessControllerOptions::default(),
)?;

// Load permissions from CID
let saved_cid = "bafyreib...";
controller2.load(saved_cid).await?;

// Verify loaded permissions
let writers = controller2.get_authorized_by_role("write").await?;
println!("Loaded {} writers", writers.len());

for writer in writers {
    println!("  - {}", writer);
}
```

---

### Validate Log Entries

```rust
use guardian_db::ipfs_log::entry::Entry;

// Create a log entry (simplified)
let entry: Box<dyn LogEntry> = ...; // Your entry implementation
let identity_provider: Box<dyn IdentityProvider> = ...; // Your identity provider
let context: Box<dyn CanAppendAdditionalContext> = ...; // Context

// Validate entry
match controller.can_append(
    entry.as_ref(),
    identity_provider.as_ref(),
    context.as_ref()
).await {
    Ok(()) => {
        println!("Entry authorized - adding to log");
        // Add entry to log...
    }
    Err(e) => {
        println!("Entry rejected: {}", e);
        // Handle rejection...
    }
}
```

---

### Permission Update Workflow

```rust
// 1. Create controller with initial permissions
let mut initial_params = CreateAccessControllerOptions::default();
initial_params.set_access("write".to_string(), vec![
    "did:key:alice".to_string(),
]);

let controller = IrohAccessController::new(
    ipfs.clone(),
    "did:key:alice".to_string(),
    initial_params,
)?;

// 2. Save initial state
let v1_manifest = controller.save().await?;
let v1_cid = v1_manifest.address().to_string();
println!("v1 CID: {}", v1_cid);

// 3. Grant additional access
controller.grant("write", "did:key:bob").await?;

// 4. Save updated state (new CID!)
let v2_manifest = controller.save().await?;
let v2_cid = v2_manifest.address().to_string();
println!("v2 CID: {}", v2_cid);

// 5. Both versions are immutably stored in IPFS
// Can load either version at any time
controller.load(&v1_cid).await?; // Back to v1
controller.load(&v2_cid).await?; // Forward to v2
```

---

## Comparison with Other Controllers

### vs SimpleAccessController

| Aspect | IrohAccessController | SimpleAccessController |
|--------|---------------------|------------------------|
| **Storage** | IPFS (content-addressed) | In-memory (HashMap) |
| **Persistence** | Immutable IPFS objects | None (ephemeral) |
| **Sharing** | Share CID across nodes | Not shareable |
| **Versioning** | Implicit (each save = new CID) | Not supported |
| **Capabilities** | Only "write" | Multiple custom capabilities |
| **Complexity** | Medium (requires IPFS) | Low (standalone) |
| **Use Case** | IPFS-native apps | Quick tests, prototypes |

---

### vs GuardianDBAccessController

| Aspect | IrohAccessController | GuardianDBAccessController |
|--------|---------------------|----------------------------|
| **Storage** | Raw IPFS CBOR docs | GuardianDB KeyValueStore |
| **Mutability** | Immutable (new CID per update) | Mutable (same address) |
| **Events** | None | EventBus integration |
| **Replication** | Manual (share CIDs) | Automatic (P2P sync) |
| **Complexity** | Medium | High |
| **Use Case** | IPFS-first designs | GuardianDB ecosystem |

---

## Limitations and Considerations

### Current Limitations

1. **Single Capability**: Only "write" is supported
   - Cannot define custom roles like "moderator", "viewer", etc.
   - All roles ("admin", "read") map to "write"

2. **No Events**: Unlike GuardianDBAccessController, doesn't emit events
   - Cannot audit or monitor permission changes
   - No reactive updates

3. **Additional Context Unused**: The `_additional_context` parameter in `can_append()` is ignored

4. **Immutability Overhead**: Each permission update creates a new CID
   - Must share new CID with all nodes
   - Historical CIDs remain in IPFS (storage overhead)

### Design Considerations

**When to use IrohAccessController:**
- IPFS-native applications
- Need immutable permission history
- Content-addressed access control
- Simple write-only permissions sufficient

**When NOT to use:**
- Need complex role hierarchies
- Require permission change events
- Frequent permission updates
- Automatic permission replication needed

---

## Future Enhancements

### Potential TODOs

```rust
// TODO: Support custom capabilities beyond "write"
pub async fn grant(&self, capability: &str, key_id: &str) -> Result<()> {
    // Currently: capability must be "write"
    // Future: Support arbitrary capabilities
}

// TODO: Utilize additional_context in validation
pub async fn can_append(
    &self,
    entry: &dyn LogEntry,
    identity_provider: &dyn IdentityProvider,
    additional_context: &dyn CanAppendAdditionalContext, // ⬅ Use this
) -> Result<()>

// TODO: Add optional event emission
pub async fn grant(&self, capability: &str, key_id: &str) -> Result<()> {
    // ... grant logic ...
    // Emit event for monitoring?
}

// TODO: Support delta updates instead of full state rewrites
pub async fn save_delta(&self, changes: PermissionDelta) -> Result<Cid>
```

---

## Serialization Details

### Full Serialization Chain

```
Rust: Vec<String>
  ↓ serde_json::to_string
JSON: "[\"did:key:z6Mkk...\",\"*\"]"
  ↓ Wrap in CborWriteAccess
CBOR struct: { write: "..." }
  ↓ serde_cbor::to_vec
CBOR bytes: [0xa1, 0x65, 0x77, 0x72, 0x69, ...]
  ↓ ipfs.add_bytes
IPFS CID: bafyreib...
```

### Deserialization Chain

```
IPFS CID: bafyreib...
  ↓ ipfs.cat
CBOR bytes: [0xa1, 0x65, 0x77, 0x72, 0x69, ...]
  ↓ serde_cbor::from_slice
CBOR struct: CborWriteAccess { write: "..." }
  ↓ Extract .write field
JSON: "[\"did:key:z6Mkk...\",\"*\"]"
  ↓ serde_json::from_str
Rust: Vec<String>
```

### Why Nested Serialization?

This format maintains compatibility with OrbitDB's JavaScript implementation:
- JavaScript stores stringified JSON in CBOR
- Rust follows same pattern for interoperability
- Allows seamless migration from OrbitDB to GuardianDB

---

## Testing Patterns

### Unit Test Example

```rust
#[tokio::test]
async fn test_grant_and_revoke() {
    let ipfs = Arc::new(IrohClient::development().await.unwrap());
    let controller = IrohAccessController::new(
        ipfs,
        "did:key:alice".to_string(),
        CreateAccessControllerOptions::default(),
    ).unwrap();
    
    // Grant
    controller.grant("write", "did:key:bob").await.unwrap();
    let writers = controller.get_authorized_by_role("write").await.unwrap();
    assert!(writers.contains(&"did:key:bob".to_string()));
    
    // Revoke
    controller.revoke("write", "did:key:bob").await.unwrap();
    let writers = controller.get_authorized_by_role("write").await.unwrap();
    assert!(!writers.contains(&"did:key:bob".to_string()));
}
```

### Integration Test Example

```rust
#[tokio::test]
async fn test_persistence_roundtrip() {
    let ipfs = Arc::new(IrohClient::development().await.unwrap());
    
    // Create and configure controller
    let controller1 = IrohAccessController::new(
        ipfs.clone(),
        "did:key:alice".to_string(),
        CreateAccessControllerOptions::default(),
    ).unwrap();
    
    controller1.grant("write", "did:key:bob").await.unwrap();
    
    // Save to IPFS
    let manifest = controller1.save().await.unwrap();
    let cid = manifest.address().to_string();
    
    // Load in new controller
    let controller2 = IrohAccessController::new(
        ipfs,
        "temporary".to_string(),
        CreateAccessControllerOptions::default(),
    ).unwrap();
    
    controller2.load(&cid).await.unwrap();
    
    // Verify permissions match
    let writers1 = controller1.get_authorized_by_role("write").await.unwrap();
    let writers2 = controller2.get_authorized_by_role("write").await.unwrap();
    assert_eq!(writers1, writers2);
}
```

---

## See Also

- [ACCESS_CONTROLLER_TRAITS.md](./ACCESS_CONTROLLER_TRAITS.md) - AccessController trait definition
- [SIMPLE_ACCESS_CONTROLLER_IMPLEMENTATION.md](./SIMPLE_ACCESS_CONTROLLER_IMPLEMENTATION.md) - Simple in-memory controller
- [GUARDIAN_DB_ACCESS_CONTROLLER.md](./GUARDIAN_DB_ACCESS_CONTROLLER.md) - GuardianDB-backed controller
- [ACCESS_CONTROLLER_UTILS.md](./ACCESS_CONTROLLER_UTILS.md) - Utility functions
- [ACCESS_CONTROLLER_MANIFEST.md](./ACCESS_CONTROLLER_MANIFEST.md) - Manifest system documentation
