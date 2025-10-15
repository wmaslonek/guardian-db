# Access Controller Manifest System

## Overview

The manifest system provides a standardized way to store, retrieve, and validate access controller configurations in GuardianDB. It enables:

- **IPFS Persistence**: Store controller configurations as immutable CBOR documents
- **Type Safety**: Strongly-typed parameters with validation
- **Interoperability**: Consistent format across different controller types
- **Versioning**: Content-addressed storage enables version history
- **Flexibility**: Support for multiple controller types and custom configurations

## Core Concepts

### Manifest

A manifest is a metadata document that describes an access controller's type and configuration. It contains:
- **Type**: Controller type identifier (e.g., "ipfs", "GuardianDB", "simple")
- **Parameters**: Configuration options including address, permissions, and settings

### Content Addressing

Manifests are stored in IPFS using content-addressed storage:
1. Configuration → CBOR serialization → IPFS CID
2. CID uniquely identifies the configuration
3. Same configuration always produces same CID (deterministic)
4. Configurations are immutable by design

---

## Data Structures

### Manifest

```rust
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Manifest {
    #[serde(rename = "type")]
    pub get_type: String,
    
    #[serde(rename = "params")]
    pub params: CreateAccessControllerOptions,
}
```

**Fields:**
- `get_type` - Controller type: `"ipfs"`, `"GuardianDB"`, or `"simple"`
- `params` - Configuration parameters for the controller

**JSON Example:**
```json
{
  "type": "ipfs",
  "params": {
    "skip_manifest": false,
    "address": "bafyreib...",
    "type": "ipfs",
    "name": "my-controller",
    "access": {
      "write": ["did:key:z6Mkk...", "*"]
    }
  }
}
```

---

### CreateAccessControllerOptions

```rust
#[derive(Debug, Clone)]
pub struct CreateAccessControllerOptions {
    pub skip_manifest: bool,
    pub address: Cid,
    pub get_type: String,
    pub name: String,
    access: Arc<Mutex<HashMap<String, Vec<String>>>>,
}
```

**Fields:**
- `skip_manifest` - If true, skip IPFS manifest creation (use in-memory only)
- `address` - CID pointing to the controller's data or manifest
- `get_type` - Controller type identifier
- `name` - Human-readable controller name
- `access` - Permission map: role → list of authorized key IDs (thread-safe)

**Thread Safety:**
The `access` field uses `Arc<Mutex<HashMap>>` to allow:
- Safe concurrent access from multiple threads
- Cloning without duplicating the underlying data
- Interior mutability through the trait interface

---

### ManifestParams Trait

Defines the interface for accessing and modifying manifest parameters.

```rust
pub trait ManifestParams: Send + Sync {
    fn skip_manifest(&self) -> bool;
    fn address(&self) -> Cid;
    fn set_address(&mut self, addr: Cid);
    fn get_type(&self) -> &str;
    fn set_type(&mut self, t: String);
    fn get_name(&self) -> &str;
    fn set_name(&mut self, name: String);
    fn set_access(&mut self, role: String, allowed: Vec<String>);
    fn get_access(&self, role: &str) -> Option<Vec<String>>;
    fn get_all_access(&self) -> HashMap<String, Vec<String>>;
    fn as_any(&self) -> &dyn std::any::Any;
}
```

This trait enables:
- Polymorphic access to parameters
- Type-safe downcasting via `as_any()`
- Consistent interface across controller implementations

---

## API Reference

### Constructors

#### `new()`

Creates options with specific address and type.

```rust
pub fn new(address: Cid, skip_manifest: bool, manifest_type: String) -> Self
```

**Example:**
```rust
let cid = Cid::try_from("bafyreib...")?;
let options = CreateAccessControllerOptions::new(
    cid,
    false,
    "ipfs".to_string(),
);
```

---

#### `new_empty()`

Creates default/empty options.

```rust
pub fn new_empty() -> Self
```

**Example:**
```rust
let options = CreateAccessControllerOptions::new_empty();
// All fields set to defaults: empty CID, empty type, etc.
```

---

#### `new_simple()`

Creates options for simple access controller with permissions.

```rust
pub fn new_simple(manifest_type: String, access: HashMap<String, Vec<String>>) -> Self
```

**Parameters:**
- `manifest_type` - Controller type
- `access` - Initial permission map

**Example:**
```rust
let mut access = HashMap::new();
access.insert("write".to_string(), vec![
    "did:key:z6Mkk...".to_string(),
    "*".to_string(),
]);

let options = CreateAccessControllerOptions::new_simple(
    "simple".to_string(),
    access,
);
```

**Note:** Sets `skip_manifest = true` automatically.

---

#### `from_params()`

Clones existing options.

```rust
pub fn from_params(params: &CreateAccessControllerOptions) -> Self
```

**Example:**
```rust
let original = CreateAccessControllerOptions::new_empty();
let copy = CreateAccessControllerOptions::from_params(&original);
```

---

### IPFS Operations

#### `create()`

Creates a manifest and stores it in IPFS, returning its CID.

```rust
pub async fn create(
    ipfs: Arc<IpfsClient>,
    controller_type: String,
    params: &CreateAccessControllerOptions,
) -> Result<Cid>
```

**Parameters:**
- `ipfs` - IPFS client for storage
- `controller_type` - Type of the controller
- `params` - Configuration parameters

**Returns:**
- `Ok(Cid)` - CID of the stored manifest
- `Err(GuardianError)` - Error during creation or storage

**Behavior:**
1. If `params.skip_manifest()` is true, returns `params.address()` directly
2. Validates controller_type is not empty
3. Creates Manifest structure
4. Serializes to CBOR
5. Stores in IPFS
6. Returns CID

**Example:**
```rust
use guardian_db::ipfs_core_api::IpfsClient;

let ipfs = Arc::new(IpfsClient::development().await?);
let mut params = CreateAccessControllerOptions::new_empty();
params.set_type("ipfs".to_string());
params.set_name("my-ac".to_string());

let cid = create(ipfs, "ipfs".to_string(), &params).await?;
println!("Manifest stored at: {}", cid);
```

---

#### `resolve()`

Loads a manifest from IPFS using its CID.

```rust
pub async fn resolve(
    ipfs: Arc<IpfsClient>,
    manifest_address: &str,
    params: &CreateAccessControllerOptions,
) -> Result<Manifest>
```

**Parameters:**
- `ipfs` - IPFS client for retrieval
- `manifest_address` - CID or `/ipfs/{CID}` path
- `params` - Parameters for fallback if skip_manifest is true

**Returns:**
- `Ok(Manifest)` - Loaded and validated manifest
- `Err(GuardianError)` - Error during retrieval or validation

**Behavior:**
1. If `params.skip_manifest()` is true, constructs manifest from params
2. Strips `/ipfs/` prefix if present
3. Parses CID
4. Fetches data from IPFS
5. Deserializes CBOR to Manifest
6. Validates manifest type is not empty
7. Returns manifest

**Example:**
```rust
let manifest_cid = "bafyreib...";
let params = CreateAccessControllerOptions::new_empty();

let manifest = resolve(
    ipfs.clone(),
    manifest_cid,
    &params,
).await?;

println!("Controller type: {}", manifest.get_type);
println!("Permissions: {:?}", manifest.params.get_all_access());
```

---

### Validation

#### `create_manifest_with_validation()`

Creates and validates a manifest locally (without IPFS).

```rust
pub fn create_manifest_with_validation(
    controller_type: String,
    params: CreateAccessControllerOptions,
) -> Result<Manifest>
```

**Parameters:**
- `controller_type` - Type of the controller
- `params` - Configuration parameters

**Returns:**
- `Ok(Manifest)` - Valid manifest
- `Err(GuardianError)` - Validation error

**Validations:**
1. Controller type not empty
2. Controller type ≤ 255 characters
3. Controller type is one of: `"ipfs"`, `"GuardianDB"`, `"simple"`

**Example:**
```rust
let params = CreateAccessControllerOptions::new_simple(
    "ipfs".to_string(),
    HashMap::new(),
);

let manifest = create_manifest_with_validation(
    "ipfs".to_string(),
    params,
)?;

// Manifest is valid and ready to use
```

**Error Cases:**
```rust
// Error: Empty type
let result = create_manifest_with_validation("".to_string(), params);
// Err: "Controller type cannot be empty"

// Error: Invalid type
let result = create_manifest_with_validation("custom".to_string(), params);
// Err: "Unknown controller type: custom"

// Error: Too long
let long_type = "x".repeat(256);
let result = create_manifest_with_validation(long_type, params);
// Err: "Controller type too long (max 255 characters)"
```

---

## ManifestParams Trait Methods

### Getters

```rust
// Skip manifest flag
fn skip_manifest(&self) -> bool;

// CID address
fn address(&self) -> Cid;

// Controller type
fn get_type(&self) -> &str;

// Controller name
fn get_name(&self) -> &str;

// Get permissions for specific role
fn get_access(&self, role: &str) -> Option<Vec<String>>;

// Get all permissions
fn get_all_access(&self) -> HashMap<String, Vec<String>>;

// Downcast support
fn as_any(&self) -> &dyn std::any::Any;
```

### Setters

```rust
// Set CID address
fn set_address(&mut self, addr: Cid);

// Set controller type
fn set_type(&mut self, t: String);

// Set controller name
fn set_name(&mut self, name: String);

// Set permissions for role
fn set_access(&mut self, role: String, allowed: Vec<String>);
```

### Usage Example

```rust
let mut params = CreateAccessControllerOptions::new_empty();

// Set basic info
params.set_type("ipfs".to_string());
params.set_name("production-ac".to_string());

// Configure permissions
params.set_access("write".to_string(), vec![
    "did:key:alice".to_string(),
    "did:key:bob".to_string(),
]);

params.set_access("admin".to_string(), vec![
    "did:key:alice".to_string(),
]);

// Read back
println!("Type: {}", params.get_type());
println!("Writers: {:?}", params.get_access("write"));
println!("All access: {:?}", params.get_all_access());
```

---

## Serialization

### Custom Implementation

`CreateAccessControllerOptions` implements custom `Serialize` and `Deserialize` to handle the thread-safe `Arc<Mutex<HashMap>>` field.

**Serialization:**
1. Acquires mutex lock
2. Serializes access map if non-empty
3. Releases lock automatically

**Deserialization:**
1. Deserializes fields from CBOR/JSON
2. Wraps access map in Arc<Mutex<>>
3. Returns fully initialized struct

### Supported Formats

- **JSON**: Human-readable, debugging
- **CBOR**: Compact binary, IPFS storage
- **Native CID**: CID type serializes/deserializes natively

### Examples

#### JSON Serialization

```rust
let mut access = HashMap::new();
access.insert("write".to_string(), vec!["*".to_string()]);

let options = CreateAccessControllerOptions::new_simple(
    "simple".to_string(),
    access,
);

// Serialize to JSON
let json = serde_json::to_string_pretty(&options)?;
println!("{}", json);

// Output:
// {
//   "skip_manifest": true,
//   "address": "bafybeib...",
//   "type": "simple",
//   "name": "",
//   "access": {
//     "write": ["*"]
//   }
// }
```

#### CBOR Serialization

```rust
// Serialize to CBOR
let cbor_bytes = serde_cbor::to_vec(&options)?;
println!("CBOR size: {} bytes", cbor_bytes.len());

// Deserialize from CBOR
let decoded: CreateAccessControllerOptions = 
    serde_cbor::from_slice(&cbor_bytes)?;

assert_eq!(decoded.get_type(), "simple");
```

---

## Workflow Examples

### Complete Manifest Lifecycle

```rust
use guardian_db::{
    ipfs_core_api::IpfsClient,
    access_controller::manifest::*,
};
use std::sync::Arc;

// 1. Initialize IPFS
let ipfs = Arc::new(IpfsClient::development().await?);

// 2. Create configuration
let mut params = CreateAccessControllerOptions::new_empty();
params.set_type("ipfs".to_string());
params.set_name("auth-controller".to_string());
params.set_access("write".to_string(), vec![
    "did:key:alice".to_string(),
    "did:key:bob".to_string(),
]);

// 3. Validate locally
let manifest = create_manifest_with_validation(
    "ipfs".to_string(),
    params.clone(),
)?;
println!("Manifest valid: {}", manifest.get_type);

// 4. Store in IPFS
let cid = create(
    ipfs.clone(),
    "ipfs".to_string(),
    &params,
).await?;
println!("Manifest CID: {}", cid);

// 5. Load from IPFS
let loaded = resolve(
    ipfs.clone(),
    &cid.to_string(),
    &CreateAccessControllerOptions::new_empty(),
).await?;

// 6. Verify loaded data
assert_eq!(loaded.get_type, "ipfs");
assert_eq!(loaded.params.get_name(), "auth-controller");
```

---

### Skip Manifest Mode

For testing or temporary controllers:

```rust
let mut params = CreateAccessControllerOptions::new_empty();
params.skip_manifest = true; // Skip IPFS storage
params.set_type("simple".to_string());

// create() will return params.address() directly
let cid = create(ipfs, "simple".to_string(), &params).await?;
assert_eq!(cid, params.address());

// resolve() will construct manifest from params
let manifest = resolve(ipfs, "", &params).await?;
assert_eq!(manifest.get_type, "simple");
```

---

### Update Permissions

Manifests are immutable, so updates create new CIDs:

```rust
// Initial version
let mut v1_params = CreateAccessControllerOptions::new_simple(
    "ipfs".to_string(),
    {
        let mut access = HashMap::new();
        access.insert("write".to_string(), vec![
            "did:key:alice".to_string(),
        ]);
        access
    },
);

let v1_cid = create(ipfs.clone(), "ipfs".to_string(), &v1_params).await?;
println!("V1: {}", v1_cid);

// Add permission (creates new manifest)
v1_params.set_access("write".to_string(), vec![
    "did:key:alice".to_string(),
    "did:key:bob".to_string(), // Added
]);

let v2_cid = create(ipfs.clone(), "ipfs".to_string(), &v1_params).await?;
println!("V2: {}", v2_cid);

// Both versions exist in IPFS
assert_ne!(v1_cid, v2_cid);
```

---

### Controller Type Migration

```rust
// Start with simple controller
let mut params = CreateAccessControllerOptions::new_simple(
    "simple".to_string(),
    HashMap::new(),
);

let simple_cid = create(ipfs.clone(), "simple".to_string(), &params).await?;

// Migrate to IPFS controller
params.set_type("ipfs".to_string());
let ipfs_cid = create(ipfs.clone(), "ipfs".to_string(), &params).await?;

// Both manifests preserved
let simple_manifest = resolve(ipfs.clone(), &simple_cid.to_string(), &params).await?;
assert_eq!(simple_manifest.get_type, "simple");

let ipfs_manifest = resolve(ipfs.clone(), &ipfs_cid.to_string(), &params).await?;
assert_eq!(ipfs_manifest.get_type, "ipfs");
```

---

## Error Handling

### Common Errors

| Error | Cause | Solution |
|-------|-------|----------|
| "Controller type cannot be empty" | Empty type string | Provide valid type |
| "Invalid CID format" | Malformed CID string | Validate CID before use |
| "Failed to store manifest in IPFS" | IPFS connection issue | Check IPFS client |
| "Retrieved manifest data is empty" | CID not found | Verify CID exists |
| "Unknown controller type" | Invalid type in validation | Use known types |
| "Manifest type cannot be empty" | Corrupted manifest | Re-create manifest |

### Error Handling Patterns

```rust
// Pattern 1: Validation before creation
match create_manifest_with_validation("ipfs".to_string(), params.clone()) {
    Ok(manifest) => {
        let cid = create(ipfs, "ipfs".to_string(), &params).await?;
        println!("Created: {}", cid);
    }
    Err(e) => {
        eprintln!("Invalid manifest: {}", e);
        return Err(e);
    }
}

// Pattern 2: Graceful fallback
let manifest = match resolve(ipfs.clone(), cid_str, &params).await {
    Ok(m) => m,
    Err(_) => {
        warn!("Failed to load manifest, using defaults");
        Manifest {
            get_type: "simple".to_string(),
            params: CreateAccessControllerOptions::new_empty(),
        }
    }
};

// Pattern 3: Detailed error context
create(ipfs, controller_type, &params)
    .await
    .map_err(|e| {
        error!("Manifest creation failed: {}", e);
        GuardianError::Store(format!("Cannot create AC manifest: {}", e))
    })?;
```

---

## Best Practices

### 1. Always Validate

```rust
// Good: Validate before storing
let manifest = create_manifest_with_validation(type_str, params)?;
let cid = create(ipfs, type_str, &manifest.params).await?;

// Bad: Skip validation
let cid = create(ipfs, user_input, &params).await?; // May fail
```

### 2. Use Type Safety

```rust
// Good: Use trait for polymorphism
fn process_manifest(params: &dyn ManifestParams) {
    println!("Type: {}", params.get_type());
}

// Bad: Assume concrete type
fn process_manifest(params: &CreateAccessControllerOptions) {
    // Tightly coupled
}
```

### 3. Handle Concurrent Access

```rust
// Good: Proper mutex usage
{
    let access_guard = params.access.lock().unwrap();
    let write_perms = access_guard.get("write").cloned();
} // Lock released

// Bad: Holding lock too long
let access_guard = params.access.lock().unwrap();
do_expensive_operation(); // Lock held unnecessarily
```

### 4. Version Manifests

```rust
// Good: Track versions
let mut versions = Vec::new();
versions.push(create(ipfs.clone(), "ipfs".to_string(), &params).await?);

params.set_access("admin".to_string(), new_admins);
versions.push(create(ipfs.clone(), "ipfs".to_string(), &params).await?);

// Can rollback to any version
let previous = resolve(ipfs, &versions[0].to_string(), &params).await?;
```

### 5. Document Controller Types

```rust
// Good: Define constants
const CONTROLLER_TYPE_SIMPLE: &str = "simple";
const CONTROLLER_TYPE_IPFS: &str = "ipfs";
const CONTROLLER_TYPE_GUARDIAN: &str = "GuardianDB";

let params = CreateAccessControllerOptions::new_simple(
    CONTROLLER_TYPE_SIMPLE.to_string(),
    access_map,
);

// Bad: Magic strings
let params = CreateAccessControllerOptions::new_simple(
    "sImPLe".to_string(), // Typo-prone
    access_map,
);
```

---

## Testing

### Unit Tests

The module includes tests for:
- CID native serialization/deserialization
- JSON and CBOR format support
- Custom Serialize/Deserialize implementation

```rust
#[test]
fn test_cid_serialization_deserialization() {
    let mut access = HashMap::new();
    access.insert("write".to_string(), vec!["test_key".to_string()]);

    let options = CreateAccessControllerOptions {
        skip_manifest: false,
        address: Cid::default(),
        get_type: "test".to_string(),
        name: "test_controller".to_string(),
        access: Arc::new(Mutex::new(access)),
    };

    // JSON round-trip
    let json = serde_json::to_string(&options).unwrap();
    let deserialized: CreateAccessControllerOptions = 
        serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.address, Cid::default());

    // CBOR round-trip
    let cbor = serde_cbor::to_vec(&options).unwrap();
    let cbor_deserialized: CreateAccessControllerOptions = 
        serde_cbor::from_slice(&cbor).unwrap();
    assert_eq!(cbor_deserialized.address, Cid::default());
}
```

### Integration Test Pattern

```rust
#[tokio::test]
async fn test_manifest_lifecycle() {
    let ipfs = Arc::new(IpfsClient::development().await.unwrap());
    
    // Create
    let mut params = CreateAccessControllerOptions::new_empty();
    params.set_type("ipfs".to_string());
    let cid = create(ipfs.clone(), "ipfs".to_string(), &params)
        .await
        .unwrap();
    
    // Resolve
    let manifest = resolve(ipfs.clone(), &cid.to_string(), &params)
        .await
        .unwrap();
    
    // Verify
    assert_eq!(manifest.get_type, "ipfs");
    assert_eq!(manifest.params.get_type(), "ipfs");
}
```

---

## Performance Considerations

### Mutex Contention

The `Arc<Mutex<HashMap>>` in `access` field can cause contention:

```rust
// Contention: Multiple threads accessing permissions
for _ in 0..1000 {
    tokio::spawn({
        let params = params.clone();
        async move {
            let writers = params.get_access("write"); // Lock acquired
        }
    });
}
```

**Solution**: Cache frequently-accessed permissions:
```rust
// Cache permissions
let cached_writers = params.get_access("write").unwrap_or_default();
for _ in 0..1000 {
    tokio::spawn({
        let writers = cached_writers.clone();
        async move {
            // Use cached copy, no mutex
        }
    });
}
```

### IPFS Latency

IPFS operations are network-dependent:

```rust
// Sequential: Slow for multiple manifests
for cid in cids {
    let manifest = resolve(ipfs.clone(), &cid, &params).await?;
}

// Parallel: Much faster
let futures: Vec<_> = cids.iter()
    .map(|cid| resolve(ipfs.clone(), cid, &params))
    .collect();
let manifests = futures::future::try_join_all(futures).await?;
```

### Serialization Overhead

CBOR is more efficient than JSON for storage:

```rust
let json_size = serde_json::to_vec(&params)?.len();
let cbor_size = serde_cbor::to_vec(&params)?.len();
println!("JSON: {} bytes, CBOR: {} bytes", json_size, cbor_size);
// Typical: CBOR is 20-40% smaller
```

---

## See Also

- [ACCESS_CONTROLLER_TRAITS.md](./ACCESS_CONTROLLER_TRAITS.md) - AccessController trait
- [ACCESS_CONTROLLER_UTILS.md](./ACCESS_CONTROLLER_UTILS.md) - Utility functions
- [IPFS_ACCESS_CONTROLLER_IMPLEMENTATION.md](./IPFS_ACCESS_CONTROLLER_IMPLEMENTATION.md) - IPFS controller
- [GUARDIAN_DB_ACCESS_CONTROLLER.md](./GUARDIAN_DB_ACCESS_CONTROLLER.md) - GuardianDB controller
- [SIMPLE_ACCESS_CONTROLLER_IMPLEMENTATION.md](./SIMPLE_ACCESS_CONTROLLER_IMPLEMENTATION.md) - Simple controller
