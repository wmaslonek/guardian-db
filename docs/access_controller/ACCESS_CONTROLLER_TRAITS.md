# AccessController Trait

## Overview

The `AccessController` trait defines the core interface that all access control implementations in GuardianDB must implement. It provides a consistent API for:

- **Permission Management**: Grant and revoke capabilities
- **Authorization**: Validate log entries before appending
- **Persistence**: Save and load controller state
- **Query**: Inspect permissions and authorized keys
- **Resource Management**: Proper cleanup and lifecycle management

All access controller implementations (`SimpleAccessController`, `IpfsAccessController`, `GuardianDBAccessController`) implement this trait, enabling polymorphic usage and interchangeability.

---

## Trait Definition

```rust
#[async_trait]
pub trait AccessController: Send + Sync {
    fn get_type(&self) -> &str;
    async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>>;
    async fn grant(&self, capability: &str, key_id: &str) -> Result<()>;
    async fn revoke(&self, capability: &str, key_id: &str) -> Result<()>;
    async fn load(&self, address: &str) -> Result<()>;
    async fn save(&self) -> Result<Box<dyn ManifestParams>>;
    async fn close(&self) -> Result<()>;
    async fn can_append(
        &self,
        entry: &dyn LogEntry,
        identity_provider: &dyn IdentityProvider,
        additional_context: &dyn CanAppendAdditionalContext,
    ) -> Result<()>;
}
```

### Trait Bounds

- **`Send`**: Controllers can be transferred between threads
- **`Sync`**: Controllers can be shared between threads (via `Arc`)
- **`async_trait`**: All async methods work with trait objects

---

## Type Aliases

### LogEntry

```rust
pub type LogEntry = dyn access_controller::LogEntry;
```

Alias for log entries from the `ipfs_log` module. Represents an entry that can be validated and appended to a log.

### CanAppendAdditionalContext

```rust
pub type CanAppendAdditionalContext = dyn access_controller::CanAppendAdditionalContext;
```

Alias for additional context that can be provided during entry validation. Used for extensibility in validation logic.

### Option

```rust
pub type Option = Box<dyn FnOnce(&mut dyn AccessController)>;
```

Function type for applying configuration options to controllers. Used in factory methods.

---

## Method Reference

### `get_type()`

Returns the type identifier for the controller.

```rust
fn get_type(&self) -> &str
```

**Returns:**
- `&str` - Type identifier string

**Implementations:**
- `SimpleAccessController` → `"simple"`
- `IpfsAccessController` → `"ipfs"`
- `GuardianDBAccessController` → `"guardian"`

**Example:**
```rust
fn print_controller_type(controller: &dyn AccessController) {
    println!("Controller type: {}", controller.get_type());
}

let simple = SimpleAccessController::new_simple();
print_controller_type(&simple); // Output: Controller type: simple
```

**Use Cases:**
- Logging and debugging
- Type-specific behavior in generic code
- Serialization metadata
- Controller selection logic

---

### `get_authorized_by_role()`

Returns all key IDs authorized for a specific role/capability.

```rust
async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>>
```

**Parameters:**
- `role` - Role/capability name (e.g., "write", "admin", "read")

**Returns:**
- `Ok(Vec<String>)` - List of authorized key IDs (may be empty)
- `Err(GuardianError)` - Error during query (e.g., invalid role name)

**Example:**
```rust
async fn list_writers(controller: &dyn AccessController) -> Result<()> {
    let writers = controller.get_authorized_by_role("write").await?;
    println!("Authorized writers:");
    for writer in writers {
        println!("  - {}", writer);
    }
    Ok(())
}
```

**Implementation Notes:**
- Should return empty vector for non-existent roles (not an error)
- `"*"` (wildcard) should be included in result if present
- Should be efficient (read-only operation)

---

### `grant()`

Grants a capability/permission to a key ID.

```rust
async fn grant(&self, capability: &str, key_id: &str) -> Result<()>
```

**Parameters:**
- `capability` - Role/permission name (e.g., "write", "admin")
- `key_id` - Identity key to authorize (DID, public key, or `"*"`)

**Returns:**
- `Ok(())` - Permission granted successfully
- `Err(GuardianError)` - Error during grant (e.g., invalid parameters)

**Example:**
```rust
async fn setup_permissions(controller: &dyn AccessController) -> Result<()> {
    // Grant write to specific user
    controller.grant("write", "did:key:alice").await?;
    
    // Grant admin to another user
    controller.grant("admin", "did:key:bob").await?;
    
    // Grant universal read
    controller.grant("read", "*").await?;
    
    Ok(())
}
```

**Implementation Requirements:**
- Idempotent: granting same permission twice should succeed
- Avoid duplicates: don't add same key_id multiple times
- Thread-safe: concurrent grants should work correctly
- Should persist changes (if controller supports persistence)

**Special Values:**
- `key_id = "*"` → Grant to all identities (universal access)

---

### `revoke()`

Revokes a capability/permission from a key ID.

```rust
async fn revoke(&self, capability: &str, key_id: &str) -> Result<()>
```

**Parameters:**
- `capability` - Role/permission name
- `key_id` - Identity key to remove authorization from

**Returns:**
- `Ok(())` - Permission revoked successfully
- `Err(GuardianError)` - Error during revocation

**Example:**
```rust
async fn remove_user_access(
    controller: &dyn AccessController,
    user_id: &str,
) -> Result<()> {
    // Remove all permissions for user
    controller.revoke("write", user_id).await?;
    controller.revoke("admin", user_id).await?;
    controller.revoke("read", user_id).await?;
    
    println!("All permissions revoked for {}", user_id);
    Ok(())
}
```

**Implementation Requirements:**
- Idempotent: revoking non-existent permission should succeed
- Thread-safe: concurrent revokes should work correctly
- Should persist changes (if controller supports persistence)
- May remove capability entirely if no keys remain

**Special Behavior:**
- Revoking from non-existent capability → No-op, returns Ok
- Revoking non-existent key → No-op, returns Ok

---

### `load()`

Loads controller state from a stored address/location.

```rust
async fn load(&self, address: &str) -> Result<()>
```

**Parameters:**
- `address` - Location identifier (IPFS CID, file path, database key, etc.)

**Returns:**
- `Ok(())` - State loaded successfully
- `Err(GuardianError)` - Error during load (e.g., address not found)

**Example:**
```rust
async fn restore_controller(
    controller: &dyn AccessController,
    backup_cid: &str,
) -> Result<()> {
    println!("Restoring from {}", backup_cid);
    controller.load(backup_cid).await?;
    println!("Controller restored");
    Ok(())
}
```

**Implementation Strategies:**

**IpfsAccessController:**
```rust
// Loads from IPFS CID
controller.load("bafyreib...").await?;
```

**GuardianDBAccessController:**
```rust
// Loads from KeyValueStore address
controller.load("my-database/_access").await?;
```

**SimpleAccessController:**
```rust
// No-op (in-memory only)
controller.load("anything").await?; // Returns Ok, does nothing
```

**Thread Safety:**
- Must handle concurrent load operations
- Should replace current state atomically
- May close/release previous resources

---

### `save()`

Saves controller state and returns manifest parameters.

```rust
async fn save(&self) -> Result<Box<dyn ManifestParams>>
```

**Returns:**
- `Ok(Box<dyn ManifestParams>)` - Manifest containing saved state
- `Err(GuardianError)` - Error during save

**Example:**
```rust
async fn backup_controller(controller: &dyn AccessController) -> Result<String> {
    // Save state
    let manifest = controller.save().await?;
    
    // Extract CID/address
    let address = manifest.address().to_string();
    println!("Backed up to: {}", address);
    
    Ok(address)
}
```

**Implementation Strategies:**

**IpfsAccessController:**
```rust
// Serializes state to IPFS, returns CID in manifest
let manifest = controller.save().await?;
let cid = manifest.address(); // IPFS CID
```

**GuardianDBAccessController:**
```rust
// Returns manifest with KeyValueStore address
let manifest = controller.save().await?;
let address = manifest.address(); // Store address
```

**SimpleAccessController:**
```rust
// Creates manifest from current in-memory state
let manifest = controller.save().await?;
// Manifest can be serialized for external storage
```

**Use Cases:**
- Periodic backups
- State replication
- Version control
- Configuration export

---

### `close()`

Closes controller and releases resources.

```rust
async fn close(&self) -> Result<()>
```

**Returns:**
- `Ok(())` - Controller closed successfully
- `Err(GuardianError)` - Error during close

**Example:**
```rust
async fn cleanup_controller(controller: Box<dyn AccessController>) -> Result<()> {
    println!("Closing controller...");
    controller.close().await?;
    println!("Controller closed");
    Ok(())
}
```

**Implementation Requirements:**
- Release any held resources (file handles, connections, locks)
- Flush pending operations (if applicable)
- Safe to call multiple times (idempotent)
- Should not fail under normal circumstances

**Resource Types by Implementation:**

**IpfsAccessController:**
- No-op (no resources to release)

**GuardianDBAccessController:**
- Closes underlying KeyValueStore
- Releases network connections

**SimpleAccessController:**
- No-op (in-memory only)

**Best Practices:**
```rust
// Use RAII pattern
{
    let controller = create_controller().await?;
    // Use controller...
} // Automatically dropped, but explicit close is better

// Explicit cleanup
let controller = create_controller().await?;
// Use controller...
controller.close().await?; // ✅ Explicit
```

---

### `can_append()`

Validates whether a log entry can be appended based on permissions and identity.

```rust
async fn can_append(
    &self,
    entry: &dyn LogEntry,
    identity_provider: &dyn IdentityProvider,
    additional_context: &dyn CanAppendAdditionalContext,
) -> Result<()>
```

**Parameters:**
- `entry` - Log entry to validate
- `identity_provider` - Provider for verifying entry's identity signature
- `additional_context` - Additional validation context (extensibility point)

**Returns:**
- `Ok(())` - Entry is authorized and valid
- `Err(GuardianError)` - Entry is rejected (unauthorized or invalid)

**Example:**
```rust
async fn add_entry_safe(
    log: &mut Log,
    controller: &dyn AccessController,
    entry: Entry,
    identity_provider: &dyn IdentityProvider,
    context: &dyn CanAppendAdditionalContext,
) -> Result<()> {
    // Validate first
    controller.can_append(&entry, identity_provider, context).await?;
    
    // Only add if authorized
    log.append(entry).await?;
    
    Ok(())
}
```

**Validation Steps (Typical Implementation):**

1. **Extract Identity:**
   ```rust
   let entry_identity = entry.get_identity();
   let entry_id = entry_identity.id();
   ```

2. **Check Permissions:**
   ```rust
   let write_keys = self.get_authorized_by_role("write").await?;
   let is_authorized = write_keys.contains(&"*".to_string()) 
       || write_keys.contains(&entry_id.to_string());
   ```

3. **Verify Signature:**
   ```rust
   if is_authorized {
       identity_provider.verify_identity(entry_identity).await?;
       return Ok(());
   }
   ```

4. **Reject if Unauthorized:**
   ```rust
   Err(GuardianError::Store(format!(
       "Access denied: identity {} not authorized",
       entry_id
   )))
   ```

**Implementation Requirements:**
- **Security Critical**: This is the enforcement point for access control
- **Atomic**: Check and validate should be atomic
- **Fast**: Called for every log entry, should be efficient
- **Deterministic**: Same entry should always produce same result

**Common Authorization Patterns:**

```rust
// Pattern 1: Write or Admin
let write_keys = self.get_authorized_by_role("write").await?;
let admin_keys = self.get_authorized_by_role("admin").await?;
let authorized = write_keys.contains(&entry_id) || admin_keys.contains(&entry_id);

// Pattern 2: Wildcard support
let has_wildcard = write_keys.contains(&"*".to_string());
let has_specific = write_keys.contains(&entry_id.to_string());
let authorized = has_wildcard || has_specific;

// Pattern 3: Role hierarchy
let authorized = check_write(&entry_id).await?
    || check_admin(&entry_id).await?
    || check_owner(&entry_id).await?;
```

---

## Polymorphic Usage

### Dynamic Dispatch

```rust
use std::sync::Arc;

async fn work_with_any_controller(controller: Arc<dyn AccessController>) -> Result<()> {
    println!("Using {} controller", controller.get_type());
    
    // Grant permission
    controller.grant("write", "did:key:alice").await?;
    
    // Query permissions
    let writers = controller.get_authorized_by_role("write").await?;
    println!("Writers: {:?}", writers);
    
    // Save state
    let manifest = controller.save().await?;
    println!("Saved to: {}", manifest.address());
    
    Ok(())
}

// Works with any implementation
let simple: Arc<dyn AccessController> = Arc::new(SimpleAccessController::new_simple());
work_with_any_controller(simple).await?;

let ipfs: Arc<dyn AccessController> = Arc::new(ipfs_controller);
work_with_any_controller(ipfs).await?;
```

---

### Trait Objects in Structs

```rust
pub struct Database {
    access_controller: Arc<dyn AccessController>,
    // ... other fields
}

impl Database {
    pub fn new(controller: Arc<dyn AccessController>) -> Self {
        Self {
            access_controller: controller,
        }
    }
    
    pub async fn add_entry(&self, entry: Entry) -> Result<()> {
        // Use trait methods
        self.access_controller
            .can_append(&entry, &self.identity_provider, &context)
            .await?;
        
        // Add to log...
        Ok(())
    }
}

// Can use any controller implementation
let db1 = Database::new(Arc::new(SimpleAccessController::new_simple()));
let db2 = Database::new(Arc::new(ipfs_controller));
let db3 = Database::new(Arc::new(guardian_controller));
```

---

### Factory Pattern

```rust
async fn create_controller(
    controller_type: &str,
    params: CreateAccessControllerOptions,
) -> Result<Arc<dyn AccessController>> {
    match controller_type {
        "simple" => {
            let controller = SimpleAccessController::from_options(params)?;
            Ok(Arc::new(controller) as Arc<dyn AccessController>)
        }
        "ipfs" => {
            let controller = IpfsAccessController::new(
                ipfs_client,
                identity_id,
                params,
            )?;
            Ok(Arc::new(controller) as Arc<dyn AccessController>)
        }
        "guardian" => {
            let controller = GuardianDBAccessController::new(
                guardian_db,
                Box::new(params),
            ).await?;
            Ok(Arc::new(controller) as Arc<dyn AccessController>)
        }
        _ => Err(GuardianError::Store(format!(
            "Unknown controller type: {}",
            controller_type
        ))),
    }
}
```

---

## Implementation Guidelines

### Required Behavior

All implementations MUST:

1. **Thread Safety**
   - All methods must be safe for concurrent access
   - Use appropriate synchronization primitives (RwLock, Mutex, etc.)
   - Avoid deadlocks and race conditions

2. **Error Handling**
   - Return meaningful errors with context
   - Don't panic in normal operation
   - Use `GuardianError` variants appropriately

3. **Idempotency**
   - `grant()` with existing permission should succeed
   - `revoke()` of non-existent permission should succeed
   - `close()` should be safe to call multiple times

4. **Performance**
   - `get_authorized_by_role()` should be fast (frequent calls)
   - `can_append()` should be fast (called for every entry)
   - `grant()` and `revoke()` can be slower (less frequent)

5. **Security**
   - `can_append()` must verify identity signatures
   - Permission checks must be atomic
   - No time-of-check/time-of-use vulnerabilities

---

### Optional Behavior

Implementations MAY:

1. **Persistence**
   - `load()` and `save()` can be no-ops for in-memory controllers
   - Persistent controllers should implement robust save/load

2. **Event Emission**
   - Controllers can emit events on permission changes
   - Not required by trait, but useful for auditing

3. **Additional Context**
   - `additional_context` parameter currently unused by most implementations
   - Future extension point for custom validation logic

4. **Custom Capabilities**
   - Controllers can support arbitrary capability names
   - Or restrict to specific set (e.g., IpfsAccessController only supports "write")

---

## Testing Patterns

### Mock Controller

```rust
struct MockAccessController {
    allowed: Arc<RwLock<bool>>,
}

#[async_trait]
impl AccessController for MockAccessController {
    fn get_type(&self) -> &str { "mock" }
    
    async fn can_append(&self, ...) -> Result<()> {
        let allowed = *self.allowed.read().await;
        if allowed {
            Ok(())
        } else {
            Err(GuardianError::Store("Mocked rejection".into()))
        }
    }
    
    // ... other methods (no-ops or simple implementations)
}

// Usage in tests
#[tokio::test]
async fn test_with_mock() {
    let mock = Arc::new(MockAccessController {
        allowed: Arc::new(RwLock::new(true)),
    });
    
    // Test code using trait methods
    let result = mock.can_append(&entry, &provider, &context).await;
    assert!(result.is_ok());
    
    // Change mock behavior
    *mock.allowed.write().await = false;
    let result = mock.can_append(&entry, &provider, &context).await;
    assert!(result.is_err());
}
```

---

### Test Helper

```rust
async fn test_controller_interface(controller: Arc<dyn AccessController>) -> Result<()> {
    // Test get_type
    let controller_type = controller.get_type();
    assert!(!controller_type.is_empty());
    
    // Test grant
    controller.grant("write", "test_key").await?;
    
    // Test get_authorized_by_role
    let writers = controller.get_authorized_by_role("write").await?;
    assert!(writers.contains(&"test_key".to_string()));
    
    // Test revoke
    controller.revoke("write", "test_key").await?;
    let writers = controller.get_authorized_by_role("write").await?;
    assert!(!writers.contains(&"test_key".to_string()));
    
    // Test save/load
    let manifest = controller.save().await?;
    let address = manifest.address().to_string();
    controller.load(&address).await?;
    
    // Test close
    controller.close().await?;
    
    Ok(())
}

// Run test against all implementations
#[tokio::test]
async fn test_all_controllers() {
    let simple = Arc::new(SimpleAccessController::new_simple());
    test_controller_interface(simple).await.unwrap();
    
    let ipfs = Arc::new(create_ipfs_controller().await.unwrap());
    test_controller_interface(ipfs).await.unwrap();
    
    // etc...
}
```

---

## Best Practices

### 1. Use Arc for Shared Ownership

```rust
// Good: Use Arc for trait objects
let controller: Arc<dyn AccessController> = Arc::new(SimpleAccessController::new_simple());
let clone = controller.clone(); // Cheap clone

// Bad: Box doesn't support cloning
let controller: Box<dyn AccessController> = Box::new(SimpleAccessController::new_simple());
// Can't clone!
```

### 2. Handle Errors Gracefully

```rust
// Good: Specific error handling
match controller.grant("write", user_id).await {
    Ok(()) => println!("Permission granted"),
    Err(GuardianError::Store(msg)) => eprintln!("Grant failed: {}", msg),
    Err(e) => eprintln!("Unexpected error: {}", e),
}

// Bad: Ignore errors
let _ = controller.grant("write", user_id).await; // Silent failure!
```

### 3. Always Close Controllers

```rust
// Good: Explicit close
{
    let controller = create_controller().await?;
    // Use controller...
    controller.close().await?;
}

// Good: Drop guard pattern
struct ControllerGuard {
    controller: Arc<dyn AccessController>,
}

impl Drop for ControllerGuard {
    fn drop(&mut self) {
        // Note: Can't run async in Drop, so this is a limitation
        // Best to close explicitly before dropping
    }
}
```

### 4. Cache Expensive Queries

```rust
// Good: Cache frequently-accessed permissions
let write_permissions = controller.get_authorized_by_role("write").await?;
for entry in entries {
    if write_permissions.contains(&entry.identity_id()) {
        // Process entry
    }
}

// Bad: Query on every iteration
for entry in entries {
    let permissions = controller.get_authorized_by_role("write").await?; // Slow!
    if permissions.contains(&entry.identity_id()) {
        // Process entry
    }
}
```

### 5. Type-Specific Behavior When Needed

```rust
use std::any::Any;

fn optimize_for_simple(controller: &dyn AccessController) {
    // Downcast if needed for type-specific optimization
    if controller.get_type() == "simple" {
        // Use SimpleAccessController-specific methods if needed
        println!("Using fast path for simple controller");
    }
}
```

---

## Common Patterns

### Permission Checker

```rust
pub struct PermissionChecker {
    controller: Arc<dyn AccessController>,
}

impl PermissionChecker {
    pub fn new(controller: Arc<dyn AccessController>) -> Self {
        Self { controller }
    }
    
    pub async fn can_user_write(&self, user_id: &str) -> bool {
        match self.controller.get_authorized_by_role("write").await {
            Ok(writers) => writers.contains(&user_id.to_string()) 
                || writers.contains(&"*".to_string()),
            Err(_) => false,
        }
    }
    
    pub async fn is_admin(&self, user_id: &str) -> bool {
        match self.controller.get_authorized_by_role("admin").await {
            Ok(admins) => admins.contains(&user_id.to_string()),
            Err(_) => false,
        }
    }
}
```

---

### Access Control Middleware

```rust
pub struct AccessControlMiddleware {
    controller: Arc<dyn AccessController>,
}

impl AccessControlMiddleware {
    pub async fn validate_entry(
        &self,
        entry: &Entry,
        identity_provider: &dyn IdentityProvider,
    ) -> Result<()> {
        // Use empty context
        struct EmptyContext;
        impl CanAppendAdditionalContext for EmptyContext {}
        
        self.controller
            .can_append(entry, identity_provider, &EmptyContext)
            .await
    }
}
```

---

## See Also

- [SIMPLE_ACCESS_CONTROLLER_IMPLEMENTATION.md](./SIMPLE_ACCESS_CONTROLLER_IMPLEMENTATION.md) - Simple in-memory implementation
- [IPFS_ACCESS_CONTROLLER_IMPLEMENTATION.md](./IPFS_ACCESS_CONTROLLER_IMPLEMENTATION.md) - IPFS-backed implementation
- [GUARDIAN_DB_ACCESS_CONTROLLER.md](./GUARDIAN_DB_ACCESS_CONTROLLER.md) - GuardianDB implementation
- [ACCESS_CONTROLLER_UTILS.md](./ACCESS_CONTROLLER_UTILS.md) - Utility functions
- [ACCESS_CONTROLLER_MANIFEST.md](./ACCESS_CONTROLLER_MANIFEST.md) - Manifest system
