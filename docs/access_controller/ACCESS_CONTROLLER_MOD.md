    // ╔════════════════════════════════════════════════════════════════════════════════╗
    // ║                          ⚠ OUTDATED DOCUMENTATION                             ║
    // ╚════════════════════════════════════════════════════════════════════════════════╝


# Access Controller Mod

## Overview

The `access_controller::utils` module provides utilities for creating, resolving, and managing access controllers in GuardianDB. This module acts as an abstraction layer between the public API and specific access controller implementations.

## Main Functions

### `create()`

Creates a new access controller and returns the CID of its manifest in IPFS.

```rust
pub async fn create(
    db: Arc<dyn BaseGuardianDB<Error = GuardianError>>,
    controller_type: &str,
    params: CreateAccessControllerOptions,
    options: AccessControllerOption,
) -> Result<Cid>
```

**Parameters:**
- `db` - BaseGuardianDB instance for IPFS access
- `controller_type` - Controller type: `"simple"`, `"guardian"`, or `"ipfs"`
- `params` - Controller configuration options
- `options` - Additional options (reserved for future use)

**Returns:**
- `Ok(Cid)` - CID of the manifest created and stored in IPFS
- `Err(GuardianError)` - Error during creation or validation

**Behavior:**
1. Validates the controller type
2. Creates instance of the appropriate controller
3. Saves the controller state
4. Creates manifest in IPFS
5. Returns the manifest CID

**Example:**
```rust
let params = CreateAccessControllerOptions::default()
    .with_write(vec!["*".to_string()]);
    
let manifest_cid = create(
    db.clone(),
    "simple",
    params,
    AccessControllerOption::default(),
).await?;

println!("Manifest CID: {}", manifest_cid);
```

---

### `resolve()`

Resolves an existing access controller using its manifest address.

```rust
pub async fn resolve(
    db: Arc<dyn BaseGuardianDB<Error = GuardianError>>,
    manifest_address: &str,
    params: &CreateAccessControllerOptions,
    options: AccessControllerOption,
) -> Result<Arc<dyn AccessController>>
```

**Parameters:**
- `db` - BaseGuardianDB instance
- `manifest_address` - Manifest address (automatically suffixed with `/_access`)
- `params` - Configuration parameters
- `options` - Additional options

**Returns:**
- `Ok(Arc<dyn AccessController>)` - Access controller loaded and ready for use
- `Err(GuardianError)` - Error during resolution or loading

**Behavior:**
1. Normalizes the address (adds `/_access` if necessary)
2. Attempts to load manifest from IPFS
3. If it fails, infers the type based on address or parameters
4. Creates controller instance
5. Loads controller state (with fallback to defaults)

**Example:**
```rust
let controller = resolve(
    db.clone(),
    "my-database",
    &params,
    AccessControllerOption::default(),
).await?;

// Check permissions
let can_write = controller.can_append(&entry, &identity_provider).await?;
```

---

### `ensure_address()`

Ensures that an access controller address ends with the `/_access` suffix.

```rust
pub fn ensure_address(address: &str) -> String
```

**Parameters:**
- `address` - Address to be normalized

**Returns:**
- `String` - Address with guaranteed `/_access` suffix

**Behavior:**
- Removes whitespace
- Adds `/_access` if not present
- Respects existing trailing slashes in the address

**Examples:**
```rust
assert_eq!(ensure_address("my-database"), "my-database/_access");
assert_eq!(ensure_address("my-database/"), "my-database/_access");
assert_eq!(ensure_address("my-database/_access"), "my-database/_access");
assert_eq!(ensure_address(""), "_access");
```

---

### `validate_address()`

Validates an access controller address for invalid characters and format.

```rust
pub fn validate_address(address: &str) -> Result<()>
```

**Parameters:**
- `address` - Address to be validated

**Returns:**
- `Ok(())` - Valid address
- `Err(GuardianError)` - Invalid address with error details

**Validations:**
- Cannot be empty (after trim)
- Cannot contain `..` (path traversal)
- Cannot contain `//` (double slashes)
- Must be at most 1000 characters

**Example:**
```rust
validate_address("my-database")?; // OK
validate_address("")?; // Error: Address cannot be empty
validate_address("foo/../bar")?; // Error: Invalid path components
validate_address("a".repeat(1001).as_str())?; // Error: Address too long
```

---

### `list_available_types()`

Lists all available access controller types in GuardianDB.

```rust
pub fn list_available_types() -> Vec<String>
```

**Returns:**
- `Vec<String>` - List with `["simple", "guardian", "ipfs"]`

**Example:**
```rust
let types = list_available_types();
println!("Available controllers: {:?}", types);
// Output: Available controllers: ["simple", "guardian", "ipfs"]
```

---

### `is_supported_type()`

Checks if a controller type is supported.

```rust
pub fn is_supported_type(controller_type: &str) -> bool
```

**Parameters:**
- `controller_type` - Type to be checked (case-insensitive)

**Returns:**
- `bool` - `true` if the type is supported, `false` otherwise

**Example:**
```rust
assert!(is_supported_type("simple"));
assert!(is_supported_type("SIMPLE")); // case-insensitive
assert!(is_supported_type("guardian"));
assert!(is_supported_type("ipfs"));
assert!(!is_supported_type("unknown"));
```

---

## Internal Functions

### `create_controller()`

Creates a controller instance based on the specified type. This function is internal and not part of the public API.

**Behavior:**
- `"simple"` - Creates `SimpleAccessController` with default or provided permissions
- `"guardian"` - Currently uses fallback to `SimpleAccessController` (TODO: complete implementation)
- `"ipfs"` - Currently uses fallback to `SimpleAccessController` (TODO: complete implementation)

**Default Permissions:**
When no permissions are provided:
- `simple`: `write: ["*"]` (open access for writing)
- `guardian` and `ipfs`: `write: ["*"]`, `admin: ["*"]` (full access)

---

### `infer_controller_type()`

Infers the controller type based on address or parameters when the manifest cannot be loaded from IPFS.

**Inference Logic:**
1. Checks if there's an explicit type in the parameters
2. Searches for patterns in the address:
   - `"/guardian/"` or `"guardian_"` → `"guardian"`
   - `"/ipfs/"` or `"ipfs_"` → `"ipfs"`
3. Default: `"simple"`

---

## IPFS Integration

The utils module integrates with IPFS through the `manifest` module for:

1. **Persistence**: Manifests are stored in IPFS as immutable documents
2. **Resolution**: Loads manifests using CIDs to retrieve configuration
3. **Fallback**: If IPFS fails, uses inference based on conventions

---

## Logging and Observability

All main functions use the tracing `#[instrument]` macro for complete observability:

- **Target**: `access_controller_utils`
- **Levels**: `info`, `debug`, `warn`, `error`
- **Context**: Includes `controller_type`, `address`, `cid`

**Log Example:**
```
INFO access_controller_utils: Creating access controller controller_type="simple"
DEBUG access_controller_utils: Access controller created successfully controller_type="simple" address="my-db/_access"
INFO access_controller_utils: Access controller manifest created in IPFS cid="bafyreib..." controller_type="simple"
```

---

## Error Handling

The module uses `GuardianError::Store` to report errors:

- Unknown controller type
- Failure to create manifest in IPFS
- Empty or invalid address
- Unsupported controller type

**Fallback Strategies:**
1. If IPFS manifest fails → infer type from address
2. If loading state fails → use defaults with warning
3. If type not specified → use "simple"

---

## Implementation Notes

### Known TODOs

1. **IpfsAccessController**: Currently uses fallback to `SimpleAccessController`. Requires implementation with full IPFS client integration.

2. **GuardianDBAccessController**: Currently uses fallback to `SimpleAccessController`. Requires resolution of complex trait bounds.

### Address Conventions

- All controller addresses must end with `/_access`
- Empty addresses are treated as `"_access"`
- Trailing slashes are handled correctly

### Thread Safety

All functions are thread-safe:
- Use of `Arc` for sharing
- `Send + Sync` traits required
- Safe async runtime (Tokio)

---

## Advanced Examples

### Create controller with custom permissions

```rust
use std::collections::HashMap;

let mut permissions = HashMap::new();
permissions.insert("write".to_string(), vec![
    "did:key:z6Mkk...".to_string(),
    "did:key:z6Mkj...".to_string(),
]);
permissions.insert("admin".to_string(), vec![
    "did:key:z6Mkk...".to_string(),
]);

let params = CreateAccessControllerOptions::new()
    .with_access(permissions);

let cid = create(db.clone(), "simple", params, None).await?;
```

### Resolve controller and check permissions

```rust
let controller = resolve(
    db.clone(),
    "my-store/_access",
    &params,
    None,
).await?;

let entry = LogEntry::new(...);
let can_append = controller.can_append(&entry, &identity).await?;

if can_append {
    println!("Entry authorized");
} else {
    println!("Entry rejected");
}
```

### Validation and type listing

```rust
// Validate user input
let user_type = "Simple"; // case-insensitive
if is_supported_type(user_type) {
    create(db, user_type, params, None).await?;
} else {
    println!("Available types: {:?}", list_available_types());
}
```

---

## See Also

- [ACCESS_CONTROLLER_TRAITS.md](./ACCESS_CONTROLLER_TRAITS.md) - Traits and interfaces
- [SIMPLE_ACCESS_CONTROLLER_IMPLEMENTATION.md](./SIMPLE_ACCESS_CONTROLLER_IMPLEMENTATION.md) - Simple controller
- [GUARDIAN_DB_ACCESS_CONTROLLER.md](./GUARDIAN_DB_ACCESS_CONTROLLER.md) - Guardian controller
- [IPFS_ACCESS_CONTROLLER_IMPLEMENTATION.md](./IPFS_ACCESS_CONTROLLER_IMPLEMENTATION.md) - IPFS controller
- [ACCESS_CONTROLLER_MANIFEST.md](./ACCESS_CONTROLLER_MANIFEST.md) - Manifest system
