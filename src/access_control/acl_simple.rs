use crate::access_control::manifest::{CreateAccessControllerOptions, ManifestParams};
use crate::access_control::traits::AccessController;
use crate::address::Address;
use crate::guardian::error::{GuardianError, Result};
use crate::log::{access_control::LogEntry, identity_provider::IdentityProvider};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{Span, debug, info, instrument, warn};

/// Internal state of the SimpleAccessController: a map from capability to the
/// list of keys authorized for it.
struct SimpleAccessControllerState {
    allowed_keys: HashMap<String, Vec<String>>,
}

/// Main structure of the simple access controller.
/// Keeps a list of authorized keys in memory.
pub struct SimpleAccessController {
    state: Arc<RwLock<SimpleAccessControllerState>>,
    span: Span,
}

impl SimpleAccessController {
    /// Creates a new SimpleAccessController with optional initial configuration.
    #[instrument(skip(initial_keys))]
    pub fn new(initial_keys: Option<HashMap<String, Vec<String>>>) -> Self {
        let mut allowed_keys = initial_keys.unwrap_or_default();

        // Ensure at least the basic categories exist.
        allowed_keys.entry("read".to_string()).or_default();
        allowed_keys.entry("write".to_string()).or_default();
        allowed_keys.entry("admin".to_string()).or_default();

        info!(target: "simple_access_controller",
            categories = ?allowed_keys.keys().collect::<Vec<_>>(),
            total_permissions = allowed_keys.values().map(|v| v.len()).sum::<usize>(),
            "Created SimpleAccessController"
        );

        Self {
            state: Arc::new(RwLock::new(SimpleAccessControllerState { allowed_keys })),
            span: tracing::info_span!("simple_access_controller"),
        }
    }

    /// Creates a new SimpleAccessController with no initial keys.
    #[allow(dead_code)]
    pub fn new_simple() -> Self {
        Self::new(None)
    }

    /// Returns a reference to the tracing span used for instrumentation.
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Lists all keys of a capability.
    pub async fn list_keys(&self, capability: &str) -> Vec<String> {
        let state = self.state.read().await;
        state
            .allowed_keys
            .get(capability)
            .cloned()
            .unwrap_or_default()
    }

    /// Lists all available capabilities.
    #[allow(dead_code)]
    pub async fn list_capabilities(&self) -> Vec<String> {
        let state = self.state.read().await;
        state.allowed_keys.keys().cloned().collect()
    }

    /// Checks whether a key has a specific capability (matching the exact key
    /// or the universal `"*"` wildcard).
    #[allow(dead_code)]
    pub async fn has_capability(&self, capability: &str, key_id: &str) -> bool {
        let state = self.state.read().await;

        if let Some(keys) = state.allowed_keys.get(capability) {
            keys.contains(&"*".to_string()) || keys.contains(&key_id.to_string())
        } else {
            false
        }
    }

    /// Removes all keys of a capability.
    pub async fn clear_capability(&self, capability: &str) -> Result<()> {
        if capability.is_empty() {
            return Err(GuardianError::Store(
                "Capability cannot be empty".to_string(),
            ));
        }

        let mut state = self.state.write().await;

        if let Some(keys) = state.allowed_keys.get_mut(capability) {
            let count = keys.len();
            keys.clear();

            info!(target: "simple_access_controller",
                capability = %capability,
                removed_keys = count,
                "Capability cleared"
            );
        } else {
            warn!(target: "simple_access_controller",
                capability = %capability,
                "Capability not found for clearing"
            );
        }

        Ok(())
    }

    /// Gets statistics about the permissions (number of keys per capability).
    pub async fn get_stats(&self) -> HashMap<String, usize> {
        let state = self.state.read().await;
        state
            .allowed_keys
            .iter()
            .map(|(capability, keys)| (capability.clone(), keys.len()))
            .collect()
    }

    /// Checks whether a capability is empty (or absent).
    pub async fn is_capability_empty(&self, capability: &str) -> bool {
        let state = self.state.read().await;
        state
            .allowed_keys
            .get(capability)
            .map(|keys| keys.is_empty())
            .unwrap_or(true)
    }

    /// Counts the total number of permissions across all capabilities.
    pub async fn total_permissions(&self) -> usize {
        let state = self.state.read().await;
        state.allowed_keys.values().map(|keys| keys.len()).sum()
    }

    /// Exports all permissions to a HashMap.
    pub async fn export_permissions(&self) -> HashMap<String, Vec<String>> {
        let state = self.state.read().await;
        state.allowed_keys.clone()
    }

    /// Imports permissions from a HashMap (replaces all existing ones).
    pub async fn import_permissions(
        &self,
        permissions: HashMap<String, Vec<String>>,
    ) -> Result<()> {
        let mut state = self.state.write().await;

        info!(target: "simple_access_controller", "Importing permissions: capabilities_count={}, total_permissions={}",
            permissions.len(),
            permissions.values().map(|v| v.len()).sum::<usize>()
        );

        state.allowed_keys = permissions;
        Ok(())
    }

    /// Adds multiple keys to a capability at once, skipping empty or duplicate
    /// keys.
    pub async fn grant_multiple(&self, capability: &str, key_ids: Vec<&str>) -> Result<()> {
        let _entered = self.span.enter();

        if capability.is_empty() {
            return Err(GuardianError::Store(
                "Capability cannot be empty".to_string(),
            ));
        }

        let mut state = self.state.write().await;
        let keys = state
            .allowed_keys
            .entry(capability.to_string())
            .or_insert_with(Vec::new);

        let mut added_count = 0;
        for key_id in key_ids {
            if !key_id.is_empty() && !keys.contains(&key_id.to_string()) {
                keys.push(key_id.to_string());
                added_count += 1;
            }
        }

        let total_keys = keys.len();
        let capability_name = capability.to_string();

        info!(target: "simple_access_controller", "Multiple permissions granted: capability={}, added_keys={}, total_keys={}",
            capability_name, added_count, total_keys
        );

        Ok(())
    }

    /// Removes multiple keys from a capability at once. If no keys remain, the
    /// capability is removed entirely.
    pub async fn revoke_multiple(&self, capability: &str, key_ids: Vec<&str>) -> Result<()> {
        let _entered = self.span.enter();

        if capability.is_empty() {
            return Err(GuardianError::Store(
                "Capability cannot be empty".to_string(),
            ));
        }

        let mut state = self.state.write().await;

        if let Some(keys) = state.allowed_keys.get_mut(capability) {
            let initial_len = keys.len();

            for key_id in key_ids {
                keys.retain(|k| k != key_id);
            }

            let removed_count = initial_len - keys.len();
            let remaining_keys = keys.len();
            let capability_name = capability.to_string();
            let should_remove_capability = keys.is_empty();

            info!(target: "simple_access_controller", "Multiple permissions revoked: capability={}, removed_keys={}, remaining_keys={}",
                capability_name, removed_count, remaining_keys
            );

            // Remove the capability entirely if no keys remain.
            if should_remove_capability {
                state.allowed_keys.remove(capability);
                debug!(target: "simple_access_controller", "Capability removed completely: capability={}",
                    capability
                );
            }
        }

        Ok(())
    }

    /// Clones the permissions of one capability into another (overwriting the
    /// target). Returns an error if the source capability does not exist.
    pub async fn clone_capability(
        &self,
        source_capability: &str,
        target_capability: &str,
    ) -> Result<()> {
        if source_capability.is_empty() || target_capability.is_empty() {
            return Err(GuardianError::Store(
                "Source and target capabilities cannot be empty".to_string(),
            ));
        }

        let mut state = self.state.write().await;

        if let Some(source_keys) = state.allowed_keys.get(source_capability) {
            let cloned_keys = source_keys.clone();
            let keys_count = cloned_keys.len();

            state
                .allowed_keys
                .insert(target_capability.to_string(), cloned_keys);

            info!(target: "simple_access_controller", "Capability cloned: source_capability={}, target_capability={}, cloned_keys={}",
                source_capability, target_capability, keys_count
            );
        } else {
            return Err(GuardianError::Store(format!(
                "Source capability '{}' not found",
                source_capability
            )));
        }

        Ok(())
    }

    /// This controller has no address, since it is not persisted.
    pub fn address(&self) -> Option<Box<dyn Address>> {
        None
    }

    /// Alternative factory method that builds the controller from creation
    /// options.
    #[instrument(skip(params))]
    pub fn from_options(params: CreateAccessControllerOptions) -> Result<Self> {
        // Permissions are extracted directly from the creation parameters.
        let allowed_keys = params.get_all_access();
        Ok(Self {
            state: Arc::new(RwLock::new(SimpleAccessControllerState { allowed_keys })),
            span: tracing::info_span!("simple_access_controller", controller_type = "simple"),
        })
    }
}

#[async_trait]
impl AccessController for SimpleAccessController {
    /// Returns the controller type identifier.
    fn get_type(&self) -> &str {
        "simple"
    }

    /// Returns the keys authorized for the given role.
    async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>> {
        let _entered = self.span.enter();

        // Parameter validation.
        if role.is_empty() {
            return Err(GuardianError::Store("Role cannot be empty".to_string()));
        }

        let state = self.state.read().await;

        // Log the query.
        debug!(target: "simple_access_controller", "Getting authorized keys by role: role={}",
            role
        );

        let keys = state.allowed_keys.get(role).cloned().unwrap_or_default();

        debug!(target: "simple_access_controller", "Retrieved authorized keys: role={}, key_count={}",
            role, keys.len()
        );

        Ok(keys)
    }

    /// Grants a key the given capability, adding it only if not already present.
    async fn grant(&self, capability: &str, key_id: &str) -> Result<()> {
        let _entered = self.span.enter();

        // Parameter validation.
        if capability.is_empty() {
            return Err(GuardianError::Store(
                "Capability cannot be empty".to_string(),
            ));
        }
        if key_id.is_empty() {
            return Err(GuardianError::Store("Key ID cannot be empty".to_string()));
        }

        let mut state = self.state.write().await;

        // Log the operation.
        info!(target: "simple_access_controller", "Granting permission: capability={}, key_id={}",
            capability, key_id
        );

        // Add the key to the permission list for the specified capability.
        let entry = state
            .allowed_keys
            .entry(capability.to_string())
            .or_insert_with(Vec::new);

        // Check whether the key already exists to avoid duplicates.
        if !entry.contains(&key_id.to_string()) {
            entry.push(key_id.to_string());
            let total_keys = entry.len();
            let capability_name = capability.to_string();
            let key_id_name = key_id.to_string();

            debug!(target: "simple_access_controller", "Permission granted successfully: capability={}, key_id={}, total_keys={}",
                capability_name, key_id_name, total_keys
            );
        } else {
            debug!(target: "simple_access_controller", "Permission already exists: capability={}, key_id={}",
                capability, key_id
            );
        }

        Ok(())
    }

    /// Revokes a key's capability. If the capability has no keys left
    /// afterwards, it is removed entirely.
    async fn revoke(&self, capability: &str, key_id: &str) -> Result<()> {
        let _entered = self.span.enter();

        // Parameter validation.
        if capability.is_empty() {
            return Err(GuardianError::Store(
                "Capability cannot be empty".to_string(),
            ));
        }
        if key_id.is_empty() {
            return Err(GuardianError::Store("Key ID cannot be empty".to_string()));
        }

        let mut state = self.state.write().await;

        // Log the operation.
        info!(target: "simple_access_controller", "Revoking permission: capability={}, key_id={}",
            capability, key_id
        );

        // Remove the key from the permission list for the specified capability.
        if let Some(keys) = state.allowed_keys.get_mut(capability) {
            let initial_len = keys.len();
            keys.retain(|k| k != key_id);

            if keys.len() < initial_len {
                let remaining_keys = keys.len();
                let capability_name = capability.to_string();
                let key_id_name = key_id.to_string();
                let should_remove_capability = keys.is_empty();

                debug!(target: "simple_access_controller", "Permission revoked successfully: capability={}, key_id={}, remaining_keys={}",
                    capability_name, key_id_name, remaining_keys
                );

                // Remove the entry entirely if no keys remain.
                if should_remove_capability {
                    state.allowed_keys.remove(capability);
                    debug!(target: "simple_access_controller", "Capability removed completely: capability={}",
                        capability
                    );
                }
            } else {
                debug!(target: "simple_access_controller", "Permission not found for revocation: capability={}, key_id={}",
                    capability, key_id
                );
            }
        } else {
            debug!(target: "simple_access_controller", "Capability not found for revocation: capability={}",
                capability
            );
        }

        Ok(())
    }

    /// Loads the controller configuration from an address. This is a no-op for
    /// the simple controller since its state is held in memory.
    async fn load(&self, address: &str) -> Result<()> {
        // Parameter validation.
        if address.is_empty() {
            return Err(GuardianError::Store("Address cannot be empty".to_string()));
        }

        // Log the operation.
        info!(target: "simple_access_controller", "Loading access controller configuration: address={}",
            address
        );

        // For SimpleAccessController, load is a no-op since it is memory-based.
        // A more advanced implementation could load from a file or the network.
        debug!(target: "simple_access_controller", "Load operation completed (no-op for simple controller): address={}",
            address
        );

        Ok(())
    }

    /// Saves the current permissions into manifest options describing this
    /// controller.
    async fn save(&self) -> Result<Box<dyn ManifestParams>> {
        let state = self.state.read().await;

        // Log the operation.
        info!(target: "simple_access_controller", "Saving access controller configuration");

        // Build options with the current permissions.
        let mut options = CreateAccessControllerOptions::new_empty();
        options.set_type("simple".to_string());

        // Copy all current permissions into the manifest.
        for (capability, keys) in &state.allowed_keys {
            options.set_access(capability.clone(), keys.clone());
        }

        debug!(target: "simple_access_controller", "Save operation completed: capabilities_count={}",
            state.allowed_keys.len()
        );

        Ok(Box::new(options))
    }

    /// Closes the controller. This is a no-op for the simple controller since
    /// its state is held in memory.
    async fn close(&self) -> Result<()> {
        let state = self.state.read().await;

        // Log the close operation.
        info!(target: "simple_access_controller", "Closing simple access controller");

        // For SimpleAccessController, close is a no-op since it is memory-based.
        // A more advanced implementation could close connections or save state.
        debug!(target: "simple_access_controller", "Close operation completed: capabilities_count={}",
            state.allowed_keys.len()
        );

        Ok(())
    }

    /// Decides whether a log entry may be appended.
    ///
    /// The entry's identity must be authorized for `write` (or `admin`, since
    /// admins may write), either by an exact key match or the universal `"*"`
    /// wildcard. In every accepted case the identity signature is also verified.
    async fn can_append(
        &self,
        entry: &dyn LogEntry,
        identity_provider: &dyn IdentityProvider,
        _additional_context: &dyn crate::log::access_control::CanAppendAdditionalContext,
    ) -> Result<()> {
        let _entered = self.span.enter();
        let state = self.state.read().await;

        // Get the identity id of the entry.
        let entry_identity = entry.get_identity();
        let entry_id = entry_identity.id();

        debug!(target: "simple_access_controller", "Checking append permission: entry_id={}",
            entry_id
        );

        // Check the write-permission keys first.
        if let Some(write_keys) = state.allowed_keys.get("write") {
            // Check for a wildcard that allows any identity.
            if write_keys.contains(&"*".to_string()) {
                debug!(target: "simple_access_controller", "Wildcard permission found, verifying identity: entry_id={}",
                    entry_id
                );

                // Even so, verify the identity to ensure it is valid.
                if let Err(e) = identity_provider
                    .verify_identity(entry.get_identity())
                    .await
                {
                    warn!(target: "simple_access_controller", "Invalid identity signature for wildcard access: entry_id={}, error={}",
                        entry_id, e
                    );
                    return Err(GuardianError::Store(format!(
                        "Invalid identity signature: {}",
                        e
                    )));
                }

                debug!(target: "simple_access_controller", "Append permission granted (wildcard): entry_id={}",
                    entry_id
                );
                return Ok(());
            }

            // Check whether the entry id is in the list of keys authorized for writing.
            if write_keys.contains(&entry_id.to_string()) {
                // Verify the identity signature.
                if let Err(e) = identity_provider.verify_identity(entry_identity).await {
                    warn!(target: "simple_access_controller", "Invalid identity signature for authorized key: entry_id={}, error={}",
                        entry_id, e
                    );
                    return Err(GuardianError::Store(format!(
                        "Invalid identity signature for authorized key {}: {}",
                        entry_id, e
                    )));
                }

                debug!(target: "simple_access_controller", "Append permission granted (write key): entry_id={}",
                    entry_id
                );
                return Ok(());
            }
        }

        // Also check admin permissions (admins may write).
        if let Some(admin_keys) = state.allowed_keys.get("admin")
            && (admin_keys.contains(&"*".to_string()) || admin_keys.contains(&entry_id.to_string()))
        {
            // Verify the identity signature.
            if let Err(e) = identity_provider.verify_identity(entry_identity).await {
                warn!(target: "simple_access_controller", "Invalid identity signature for admin key: entry_id={}, error={}",
                    entry_id, e
                );
                return Err(GuardianError::Store(format!(
                    "Invalid identity signature for admin key {}: {}",
                    entry_id, e
                )));
            }

            debug!(target: "simple_access_controller", "Append permission granted (admin key): entry_id={}",
                entry_id
            );
            return Ok(());
        }

        warn!(target: "simple_access_controller", "Access denied for append operation: entry_id={}, available_write_keys={:?}, available_admin_keys={:?}",
            entry_id, state.allowed_keys.get("write"), state.allowed_keys.get("admin")
        );

        Err(GuardianError::Store(format!(
            "Access denied: identity {} not authorized for write operations",
            entry_id
        )))
    }
}
