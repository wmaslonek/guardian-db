use crate::access_control::manifest::CreateAccessControllerOptions;
use crate::access_control::manifest::ManifestParams;
use crate::address::Address;
use crate::guardian::error::{GuardianError, Result};
use crate::log::{access_control, identity_provider::IdentityProvider};
use crate::p2p::{Emitter, EventBus};
use crate::traits::{CreateDBOptions, GuardianDBKVStoreProvider, KeyValueStore};
use iroh_blobs::Hash;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{Span, debug, instrument, warn};

// Type alias to simplify the complex key-value store type used throughout this module.
type KVStoreType =
    RwLock<Option<Arc<tokio::sync::Mutex<Box<dyn KeyValueStore<Error = GuardianError>>>>>>;

// Simple string wrapper that implements `Address` so plain strings can be
// returned where an `Address` trait object is expected.
#[derive(Debug, Clone)]
struct StringAddress(String);

impl std::fmt::Display for StringAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Address for StringAddress {
    fn get_root(&self) -> Hash {
        Hash::from_bytes([0u8; 32]) // Default (zero) hash for string-based addresses.
    }

    fn get_path(&self) -> &str {
        &self.0
    }

    fn equals(&self, other: &dyn Address) -> bool {
        format!("{}", self) == format!("{}", other)
    }
}

/// Event emitted whenever the access controller's permissions change
/// (for example after a `grant` or `revoke`).
#[derive(Debug, Clone)]
pub struct EventUpdated {
    pub controller_type: String,
    pub address: String,
    pub action: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl EventUpdated {
    /// Builds a new `EventUpdated`, stamping it with the current UTC time.
    pub fn new(controller_type: String, address: String, action: String) -> Self {
        Self {
            controller_type,
            address,
            action,
            timestamp: chrono::Utc::now(),
        }
    }
}

/// Access controller backed by a GuardianDB key-value store.
///
/// Permissions are stored as a map from capability/role (e.g. `"write"`,
/// `"admin"`) to a list of authorized identity keys, persisted in the
/// underlying key-value store.
pub struct GuardianDBAccessController {
    /// EventBus used to emit access controller events.
    event_bus: EventBus,

    /// Type-safe emitter for update events (with interior mutability so it can
    /// be initialized lazily on first use).
    event_emitter: Arc<tokio::sync::Mutex<Option<Emitter<EventUpdated>>>>,

    /// Shared reference to the main GuardianDB instance.
    guardian_db: Arc<dyn GuardianDBKVStoreProvider<Error = GuardianError>>,

    /// The key-value store holding the permissions. Wrapped in `RwLock` and
    /// `Option` because it can be replaced dynamically by the `load` method.
    kv_store: KVStoreType,

    /// Mutex serializing grant/revoke operations to avoid race conditions.
    write_mutex: tokio::sync::Mutex<()>,

    /// Manifest options.
    options: Box<dyn ManifestParams>,

    /// Span for structured tracing context.
    span: Span,
}

impl GuardianDBAccessController {
    /// Returns a reference to the span used for tracing context.
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Returns the concrete controller type identifier.
    pub fn get_type(&self) -> &'static str {
        "GuardianDB"
    }

    /// Returns the address of the underlying key-value store, or `None` if no
    /// store is currently loaded.
    pub async fn address(&self) -> Option<Box<dyn Address>> {
        let store_guard = self.kv_store.read().await;
        // Return the kv_store's address if a store exists.
        if let Some(store_arc) = store_guard.as_ref() {
            let store = store_arc.lock().await;
            let addr = store.address();
            let addr_string = format!("{}", addr);
            Some(Box::new(StringAddress(addr_string)) as Box<dyn Address>)
        } else {
            None
        }
    }

    /// Returns the list of identity keys authorized for the given role,
    /// or an empty list if the role has no authorizations.
    pub async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>> {
        let authorizations = self.get_authorizations().await?;

        // Return the keys for the role, or an empty list if the role is absent.
        Ok(authorizations.get(role).cloned().unwrap_or_default())
    }

    /// Reads and aggregates all persisted authorizations from the store into a
    /// `role -> keys` map. As a special rule, every key with `write` access is
    /// also granted `admin` access.
    async fn get_authorizations(&self) -> Result<HashMap<String, Vec<String>>> {
        let mut authorizations_set: HashMap<String, HashSet<String>> = HashMap::new();

        let store_guard = self.kv_store.read().await;
        let store = match store_guard.as_ref() {
            Some(s) => s,
            // No store means no authorizations.
            None => return Ok(HashMap::new()),
        };

        // Use store.all() to retrieve every persisted authorization entry.
        let store_lock = store.lock().await;
        let all_data = store_lock.all();

        for (role, key_bytes) in all_data {
            let authorized_keys: Vec<String> =
                crate::guardian::serializer::deserialize(&key_bytes)?;

            let entry = authorizations_set.entry(role).or_default();
            for key in authorized_keys {
                entry.insert(key);
            }
        }

        // If the 'write' permission exists, grant the same keys to 'admin'.
        if let Some(write_keys) = authorizations_set.get("write").cloned() {
            let admin_keys = authorizations_set.entry("admin".to_string()).or_default();
            for key in write_keys.iter() {
                admin_keys.insert(key.clone());
            }
        }

        // Convert the HashSet values to Vec<String> in the final map.
        let authorizations_list = authorizations_set
            .into_iter()
            .map(|(permission, keys)| (permission, keys.into_iter().collect()))
            .collect();

        Ok(authorizations_list)
    }

    /// Decides whether a log entry may be appended.
    ///
    /// Access is granted if the entry's identity id (or the universal `"*"`
    /// key) appears in the combined `write` + `admin` authorization sets; in
    /// that case the identity is also cryptographically verified. Otherwise an
    /// "unauthorized" error is returned.
    #[instrument(skip(self, entry, identity_provider, _additional_context))]
    pub async fn can_append(
        &self,
        entry: &dyn access_control::LogEntry,
        identity_provider: &dyn IdentityProvider,
        _additional_context: &dyn access_control::CanAppendAdditionalContext,
    ) -> Result<()> {
        let write_access = self.get_authorized_by_role("write").await?;
        let admin_access = self.get_authorized_by_role("admin").await?;

        let access: HashSet<String> = write_access
            .into_iter()
            .chain(admin_access.into_iter())
            .collect();

        let entry_id = entry.get_identity().id();

        // Check whether the universal key ("*") or the entry's specific id is present.
        if access.contains(entry_id) || access.contains("*") {
            identity_provider
                .verify_identity(entry.get_identity())
                .await?;
            return Ok(());
        }

        Err(GuardianError::Store("Not authorized".to_string()))
    }

    /// Grants `key_id` the given `capability`, persisting the change and
    /// emitting an update event. Concurrent grants/revokes are serialized.
    #[allow(dead_code)]
    #[instrument(skip(self), fields(capability = %capability, key_id = %key_id))]
    pub async fn grant(&self, capability: &str, key_id: &str) -> Result<()> {
        // Serialize grant operations to avoid race conditions.
        let _guard = self.write_mutex.lock().await;

        {
            let store_guard = self.kv_store.read().await;
            let store_arc = store_guard
                .as_ref()
                .ok_or_else(|| GuardianError::Store("kv_store not initialized".to_string()))?;

            // Use a HashSet to automatically deduplicate keys.
            let mut capabilities: HashSet<String> = self
                .get_authorized_by_role(capability)
                .await?
                .into_iter()
                .collect();

            capabilities.insert(key_id.to_string());

            let capabilities_vec: Vec<String> = capabilities.into_iter().collect();

            let capabilities_bytes = crate::guardian::serializer::serialize(&capabilities_vec)?;

            // Persist the permissions in the store.
            let store = store_arc.lock().await;
            store
                .put(capability, capabilities_bytes)
                .await
                .map_err(|e| GuardianError::Store(format!("Error saving to store: {}", e)))?;
        }

        // Then emit an update event via the EventBus.
        self.on_update("grant", capability, key_id).await;

        Ok(())
    }

    /// Revokes `key_id`'s `capability`. The updated permission list is
    /// persisted, or the capability entry is deleted entirely if no keys
    /// remain. Emits an update event. Concurrent grants/revokes are serialized.
    #[allow(dead_code)]
    #[instrument(skip(self), fields(capability = %capability, key_id = %key_id))]
    pub async fn revoke(&self, capability: &str, key_id: &str) -> Result<()> {
        // Serialize revoke operations to avoid race conditions.
        let _guard = self.write_mutex.lock().await;

        {
            let store_guard = self.kv_store.read().await;
            let store_arc = store_guard
                .as_ref()
                .ok_or_else(|| GuardianError::Store("kv_store not initialized".to_string()))?;

            let mut capabilities: Vec<String> = self.get_authorized_by_role(capability).await?;

            // Remove the key if it is present.
            capabilities.retain(|id| id != key_id);

            let store = store_arc.lock().await;
            if !capabilities.is_empty() {
                let capabilities_bytes = crate::guardian::serializer::serialize(&capabilities)?;

                // Persist the remaining permissions in the store.
                store
                    .put(capability, capabilities_bytes)
                    .await
                    .map_err(|e| {
                        GuardianError::Store(format!("Error persisting permissions: {}", e))
                    })?;
            } else {
                // Remove the entry entirely when no permissions remain.
                store.delete(capability).await.map_err(|e| {
                    GuardianError::Store(format!("Error removing permissions: {}", e))
                })?;
            }
        }

        // Then emit an update event via the EventBus.
        self.on_update("revoke", capability, key_id).await;

        Ok(())
    }

    /// Loads (or reloads) the permissions store from the given address,
    /// closing any currently loaded store first. The admin access from the
    /// manifest options is used as the new store's write access, falling back
    /// to universal access (`"*"`) when none is configured.
    #[instrument(skip(self), fields(address = %address))]
    pub async fn load(&self, address: &str) -> Result<()> {
        let mut store_guard = self.kv_store.write().await;
        // Close any existing store before loading a new one.
        if let Some(_store) = store_guard.take() {
            // Ignore close errors for now.
        }

        let write_access = self.options.get_access("admin");
        let write_access = match write_access {
            Some(access) if !access.is_empty() => access,
            _ => {
                // No configured access: fall back to universal access.
                vec!["*".to_string()]
            }
        };

        let db_address = crate::access_control::ensure_address(address);

        let mut store_options = CreateDBOptions::default();
        // Configure the access controller for the store.
        let iroh_ac_params = CreateAccessControllerOptions::new_simple("iroh".to_string(), {
            let mut access = HashMap::new();
            access.insert("write".to_string(), write_access);
            access
        });
        store_options.access_controller = Some(Box::new(iroh_ac_params));

        // Open the key-value store at the resolved address.
        let store = self
            .guardian_db
            .key_value(&db_address, &mut store_options)
            .await
            .map_err(|e| GuardianError::Store(format!("Error opening key-value store: {}", e)))?;

        // Store the newly opened store.
        *store_guard = Some(Arc::new(tokio::sync::Mutex::new(store)));

        Ok(())
    }

    /// Produces the manifest parameters describing this access controller,
    /// used to persist a reference to it.
    #[instrument(skip(self))]
    pub async fn save(&self) -> Result<Box<dyn ManifestParams>> {
        let store_guard = self.kv_store.read().await;
        let store_arc = store_guard
            .as_ref()
            .ok_or_else(|| GuardianError::Store("kv_store not initialized".to_string()))?;

        let store = store_arc.lock().await;
        let addr = store.address();
        let addr_string = format!("{}", addr);

        debug!(target: "access_controller", address = %addr_string, "Save executed for the store");

        // Build the manifest based on the store address.
        // Uses a default (zero) Hash.
        let hash = iroh_blobs::Hash::from([0u8; 32]);

        // Build a 'GuardianDB' manifest from the Hash.
        let params = CreateAccessControllerOptions::new(hash, false, "GuardianDB".to_string());
        Ok(Box::new(params))
    }

    /// Closes the underlying store (if loaded) and clears the reference to it.
    #[instrument(skip(self))]
    pub async fn close(&self) -> Result<()> {
        let mut store_guard = self.kv_store.write().await;
        if let Some(store_arc) = store_guard.take() {
            // Close the store via the Store trait's close() method.
            let store = store_arc.lock().await;
            match store.close().await {
                Ok(_) => debug!(target: "access_controller", "Store closed successfully"),
                Err(e) => warn!(target: "access_controller", error = %e, "Error closing the store"),
            }
        }
        Ok(())
    }

    /// Emits an `EventUpdated` describing a permission change, lazily
    /// initializing the event emitter on first use.
    async fn on_update(&self, action: &str, capability: &str, key_id: &str) {
        let mut emitter_guard = self.event_emitter.lock().await;

        // Initialize the emitter if it does not exist yet.
        if emitter_guard.is_none() {
            match self.event_bus.emitter::<EventUpdated>().await {
                Ok(emitter) => {
                    *emitter_guard = Some(emitter);
                }
                Err(e) => {
                    warn!(target: "GuardianDB::ac", error = %e, "Failed to initialize event emitter");
                    return;
                }
            }
        }

        // Emit the event via the EventBus.
        if let Some(emitter) = emitter_guard.as_ref() {
            let address = self
                .address()
                .await
                .map(|addr| format!("{}", addr))
                .unwrap_or_else(|| "unknown".to_string());

            let event = EventUpdated::new(
                "guardian".to_string(),
                address,
                format!("{}:{}:{}", action, capability, key_id),
            );

            if let Err(e) = emitter.emit(event) {
                warn!(target: "GuardianDB::ac", error = %e, "Failed to emit update event");
            } else {
                debug!(target: "GuardianDB::ac", action = %action, capability = %capability, key_id = %key_id, "Event emitted successfully");
            }
        }
    }

    /// Creates a new access controller, opening its backing key-value store and
    /// granting the initial `write` access keys from the manifest params.
    ///
    /// When the params carry no name, a unique timestamp-based name is generated
    /// to avoid collisions (the hash is intentionally not used as a name since it
    /// looks like an address).
    #[instrument(skip(guardian_db, params))]
    pub async fn new(
        guardian_db: Arc<dyn GuardianDBKVStoreProvider<Error = GuardianError>>,
        params: Box<dyn crate::access_control::manifest::ManifestParams>,
    ) -> std::result::Result<Self, GuardianError> {
        let kv_provider = guardian_db;
        // Use the provided name if present, otherwise generate a unique name.
        // Avoid using the hash as a database name since it looks like an address.
        let addr_str = if !params.get_name().is_empty() {
            params.get_name().to_string()
        } else {
            // Use a unique timestamp-based name to avoid collisions.
            format!(
                "test-ac-{}",
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
            )
        };

        let mut opts = CreateDBOptions {
            create: Some(true),
            overwrite: Some(params.skip_manifest()),
            access_controller: None,
            store_type: Some("keyvalue".to_string()),
            event_bus: Some(EventBus::new()),
            ..Default::default()
        };

        let kv_store = kv_provider
            .key_value(&addr_str, &mut opts)
            .await
            .map_err(|e| {
                GuardianError::Store(format!("Error initializing key-value store: {}", e))
            })?;

        debug!(target: "access_controller", address = %addr_str, "Key-value store initialized");

        // Use our own EventBus to emit type-safe events.
        let event_bus = EventBus::new();
        let _write_access = params.get_access("write");
        let controller = Self {
            event_bus,
            event_emitter: Arc::new(tokio::sync::Mutex::new(None)), // Initialized lazily when needed.
            guardian_db: kv_provider,
            kv_store: RwLock::new(Some(Arc::new(tokio::sync::Mutex::new(kv_store)))), // Initialize with the store.
            write_mutex: tokio::sync::Mutex::new(()),
            options: params,
            // Create a span for tracing context.
            span: tracing::info_span!("guardian_access_controller", address = %addr_str),
        };

        // Grant the initial permissions if any are configured.
        let write_access = controller.options.get_access("write");
        if let Some(access_keys) = write_access {
            for key in access_keys {
                controller.grant("write", &key).await?;
            }
        }

        Ok(controller)
    }
}

// AccessController trait implementation for GuardianDBAccessController.
// Each method simply delegates to the inherent method of the same name.
#[async_trait::async_trait]
impl crate::access_control::traits::AccessController for GuardianDBAccessController {
    fn get_type(&self) -> &str {
        "guardian"
    }

    async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>> {
        self.get_authorized_by_role(role).await
    }

    async fn grant(&self, capability: &str, key_id: &str) -> Result<()> {
        self.grant(capability, key_id).await
    }

    async fn revoke(&self, capability: &str, key_id: &str) -> Result<()> {
        self.revoke(capability, key_id).await
    }

    async fn load(&self, address: &str) -> Result<()> {
        self.load(address).await
    }

    async fn save(&self) -> Result<Box<dyn crate::access_control::manifest::ManifestParams>> {
        self.save().await
    }

    async fn close(&self) -> Result<()> {
        self.close().await
    }

    async fn can_append(
        &self,
        entry: &dyn crate::log::access_control::LogEntry,
        identity_provider: &dyn crate::log::identity_provider::IdentityProvider,
        additional_context: &dyn crate::log::access_control::CanAppendAdditionalContext,
    ) -> Result<()> {
        self.can_append(entry, identity_provider, additional_context)
            .await
    }
}
