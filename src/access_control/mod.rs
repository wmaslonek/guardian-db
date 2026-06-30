use crate::access_control::manifest::{CreateAccessControllerOptions, ManifestParams};
use crate::access_control::{
    acl_simple::SimpleAccessController, traits::AccessController,
    traits::Option as AccessControllerOption,
};
use crate::guardian::error::{GuardianError, Result};
use crate::traits::BaseGuardianDB;
use iroh_blobs::Hash;
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

pub mod acl_guardian;
pub mod acl_iroh;
pub mod acl_simple;
pub mod manifest;
pub mod traits;

/// Creates a new access controller and returns the Hash of its manifest.
///
/// # Arguments
/// * `db` - The BaseGuardianDB instance
/// * `controller_type` - The controller type ("simple", "guardian", "iroh")
/// * `params` - The controller's configuration parameters
/// * `options` - Additional options for creation
///
/// # Returns
/// * `Ok(Hash)` - Hash of the created manifest
/// * `Err(GuardianError)` - Error during creation
#[instrument(skip(db, params, _options), fields(controller_type = %controller_type))]
pub async fn create(
    db: Arc<dyn BaseGuardianDB<Error = GuardianError>>,
    controller_type: &str,
    params: CreateAccessControllerOptions,
    _options: AccessControllerOption,
) -> Result<Hash> {
    info!(target: "access_control_utils", controller_type = %controller_type, "Creating access controller");

    // Validate the controller type.
    let controller_type_normalized = controller_type.to_lowercase();
    match controller_type_normalized.as_str() {
        "simple" | "guardian" | "iroh" => {}
        _ => {
            warn!(target: "access_control_utils", controller_type = %controller_type, "Unknown access controller type");
            return Err(GuardianError::Store(format!(
                "Unknown access controller type: {}",
                controller_type
            )));
        }
    }

    // Create the controller based on its type.
    let controller = create_controller(
        &controller_type_normalized,
        params.clone(),
        Some(db.client().as_ref()),
        Some(db.clone()),
    )
    .await?;

    // Save the controller and obtain the manifest.
    let _manifest_params = controller.save().await?;

    // Ensure the address ends with "/_access".
    let access_address = ensure_address(&controller_type_normalized);

    debug!(target: "access_control_utils",
        controller_type = %controller_type,
        address = %access_address,
        "Access controller created successfully"
    );
    let client = db.client();

    // Create the manifest.
    let manifest_hash = crate::access_control::manifest::create(
        client,
        controller_type_normalized,
        &params,
    )
    .await
    .map_err(|e| {
        error!(target: "access_control_utils", error = %e, "Failed to create manifest in iroh");
        GuardianError::Store(format!(
            "Failed to create access controller manifest: {}",
            e
        ))
    })?;

    info!(target: "access_control_utils",
        hash = %hex::encode(manifest_hash.as_bytes()),
        controller_type = %controller_type,
        address = %access_address,
        "Access controller manifest created in iroh"
    );

    Ok(manifest_hash)
}

/// Resolves an access controller using its manifest address.
///
/// # Arguments
/// * `db` - The BaseGuardianDB instance
/// * `manifest_address` - The controller's manifest address
/// * `params` - Configuration parameters
/// * `options` - Additional options for resolution
///
/// # Returns
/// * `Ok(Arc<dyn AccessController>)` - The resolved access controller
/// * `Err(GuardianError)` - Error during resolution
#[instrument(skip(db, params, _options), fields(manifest_address = %manifest_address))]
pub async fn resolve(
    db: Arc<dyn BaseGuardianDB<Error = GuardianError>>,
    manifest_address: &str,
    params: &CreateAccessControllerOptions,
    _options: AccessControllerOption,
) -> Result<Arc<dyn AccessController>> {
    info!(target: "access_control_utils", manifest_address = %manifest_address, "Resolving access controller");

    // Ensure the address ends with "/_access".
    let access_address = ensure_address(manifest_address);

    // Validate the address.
    if access_address.is_empty() {
        return Err(GuardianError::Store(
            "Manifest address cannot be empty".to_string(),
        ));
    }

    debug!(target: "access_control_utils", address = %access_address, "Loading access controller manifest");

    // Get the client used to load the manifest.
    let client = db.client();

    // Try to load the manifest via the client.
    let manifest_result =
        crate::access_control::manifest::resolve(client, &access_address, params).await;

    let controller_type = match manifest_result {
        Ok(manifest) => {
            debug!(target: "access_control_utils",
                controller_type = %manifest.get_type,
                address = %access_address,
                "Loaded controller type from manifest"
            );
            manifest.get_type
        }
        Err(e) => {
            warn!(target: "access_control_utils",
                error = %e,
                address = %access_address,
                "Failed to load manifest, falling back to inference"
            );
            // Fallback: infer the type as before if loading from Iroh fails.
            infer_controller_type(&access_address, params)
        }
    };

    debug!(target: "access_control_utils",
        controller_type = %controller_type,
        address = %access_address,
        "Controller type determined"
    );

    // Create the controller based on the resolved or inferred type.
    let controller = create_controller(
        &controller_type,
        params.clone(),
        Some(db.client().as_ref()),
        Some(db.clone()),
    )
    .await?;

    // Load the controller state using the address.
    if let Err(e) = controller.load(&access_address).await {
        warn!(target: "access_control_utils",
            error = %e,
            address = %access_address,
            "Failed to load controller state, using defaults"
        );
    }

    info!(target: "access_control_utils",
        controller_type = %controller_type,
        address = %access_address,
        "Access controller resolved successfully"
    );

    Ok(controller)
}

/// Ensures an access controller address ends with "/_access".
/// If the suffix is not present, it is appended.
///
/// # Arguments
/// * `address` - The address to validate/fix
///
/// # Returns
/// * `String` - The address with a guaranteed "/_access" suffix
pub fn ensure_address(address: &str) -> String {
    // Trim surrounding whitespace.
    let address = address.trim();
    // If the address is empty, return just "_access".
    if address.is_empty() {
        return "_access".to_string();
    }
    // Check the last segment.
    // `split('/').next_back()` is more efficient than last() for a DoubleEndedIterator.
    // E.g. "foo/bar/_access".split('/').next_back() -> Some("_access")
    // E.g. "foo/bar/_access/".split('/').next_back() -> Some("")
    if address.split('/').next_back() == Some("_access") {
        return address.to_string();
    }
    // Handle the presence or absence of a trailing slash.
    if address.ends_with('/') {
        format!("{}{}", address, "_access")
    } else {
        format!("{}/{}", address, "_access")
    }
}

/// Helper function to create a controller based on its type.
///
/// # Arguments
/// * `controller_type` - The controller type ("simple", "guardian", "iroh")
/// * `params` - Configuration parameters
/// * `client` - Iroh client (optional, required for the "iroh" type)
/// * `guardian_db` - GuardianDB instance (optional, required for the "guardian" type)
///
/// # Returns
/// * `Ok(Arc<dyn AccessController>)` - The created controller
/// * `Err(GuardianError)` - Error during creation
#[instrument(skip(params, client, guardian_db))]
async fn create_controller(
    controller_type: &str,
    params: CreateAccessControllerOptions,
    client: Option<&crate::p2p::network::client::IrohClient>,
    guardian_db: Option<Arc<dyn BaseGuardianDB<Error = GuardianError>>>,
) -> Result<Arc<dyn AccessController>> {
    debug!(target: "access_control_utils", controller_type = %controller_type, "Creating access controller instance");

    match controller_type {
        "simple" => {
            let initial_keys = if params.get_all_access().is_empty() {
                // If no permissions are defined, create default permissions.
                let mut default_permissions = std::collections::HashMap::new();
                default_permissions.insert("write".to_string(), vec!["*".to_string()]);
                Some(default_permissions)
            } else {
                Some(params.get_all_access())
            };
            let controller = SimpleAccessController::new(initial_keys);
            Ok(Arc::new(controller) as Arc<dyn AccessController>)
        }
        "iroh" => {
            debug!(target: "access_control_utils", "Creating irohAccessController");

            // Check that the client was provided.
            let client = client.ok_or_else(|| {
                GuardianError::Store("Iroh client is required for IrohAccessController".to_string())
            })?;

            // Determine identity_id from the parameters or use a default.
            let identity_id = if let Some(write_keys) = params.get_access("write") {
                if !write_keys.is_empty() {
                    write_keys[0].clone()
                } else {
                    "*".to_string()
                }
            } else {
                "*".to_string()
            };

            debug!(target: "access_control_utils",
                identity_id = %identity_id,
                "Creating irohAccessController with identity"
            );

            // Create the IrohAccessController.
            let controller = crate::access_control::acl_iroh::IrohAccessController::new(
                Arc::new(client.clone()),
                identity_id,
                params,
            ).map_err(|e| {
                error!(target: "access_control_utils", error = %e, "Failed to create irohAccessController");
                GuardianError::Store(format!("Failed to create irohAccessController: {}", e))
            })?;

            info!(target: "access_control_utils", "irohAccessController created successfully");
            Ok(Arc::new(controller) as Arc<dyn AccessController>)
        }
        "guardian" => {
            debug!(target: "access_control_utils", "Creating GuardianDBAccessController");

            // Check that the GuardianDB instance was provided.
            let guardian_db_instance = guardian_db.ok_or_else(|| {
                GuardianError::Store(
                    "GuardianDB instance is required for GuardianDBAccessController".to_string(),
                )
            })?;

            // Create an adapter that implements GuardianDBKVStoreProvider.
            let kv_provider = GuardianDBAdapter::new(guardian_db_instance);

            debug!(target: "access_control_utils", "Creating GuardianDBAccessController with adapter");

            // Create the GuardianDBAccessController.
            let controller = crate::access_control::acl_guardian::GuardianDBAccessController::new(
                Arc::new(kv_provider),
                Box::new(params),
            ).await.map_err(|e| {
                error!(target: "access_control_utils", error = %e, "Failed to create GuardianDBAccessController");
                GuardianError::Store(format!("Failed to create GuardianDBAccessController: {}", e))
            })?;

            info!(target: "access_control_utils", "GuardianDBAccessController created successfully");
            Ok(Arc::new(controller) as Arc<dyn AccessController>)
        }
        _ => {
            error!(target: "access_control_utils", controller_type = %controller_type, "Unsupported access controller type");
            Err(GuardianError::Store(format!(
                "Unsupported access controller type: {}",
                controller_type
            )))
        }
    }
}

/// Helper function to infer the controller type based on the address/parameters.
///
/// # Arguments
/// * `address` - The manifest address
/// * `params` - Configuration parameters
///
/// # Returns
/// * `String` - The inferred controller type
pub fn infer_controller_type(address: &str, params: &CreateAccessControllerOptions) -> String {
    // Check for an explicit type in the parameters.
    let explicit_type = params.get_type();
    if !explicit_type.is_empty() {
        return explicit_type.to_string();
    }
    // Infer based on the address.
    if address.contains("/guardian/") || address.contains("guardian_") {
        return "guardian".to_string();
    }
    if address.contains("/iroh/") || address.contains("iroh_") {
        return "iroh".to_string();
    }
    // Default to SimpleAccessController.
    "simple".to_string()
}

/// Validates an access controller address.
///
/// # Arguments
/// * `address` - The address to validate
///
/// # Returns
/// * `Ok(())` - Valid address
/// * `Err(GuardianError)` - Invalid address
pub fn validate_address(address: &str) -> Result<()> {
    if address.trim().is_empty() {
        return Err(GuardianError::Store("Address cannot be empty".to_string()));
    }
    // Check for invalid characters.
    if address.contains("..") || address.contains("//") {
        return Err(GuardianError::Store(
            "Address contains invalid path components".to_string(),
        ));
    }
    // Check the maximum length.
    if address.len() > 1000 {
        return Err(GuardianError::Store(
            "Address is too long (max 1000 characters)".to_string(),
        ));
    }

    Ok(())
}

/// Lists the available access controller types.
///
/// # Returns
/// * `Vec<String>` - List of available types
pub fn list_available_types() -> Vec<String> {
    vec![
        "simple".to_string(),
        "guardian".to_string(),
        "iroh".to_string(),
    ]
}

/// Checks whether a controller type is supported.
///
/// # Arguments
/// * `controller_type` - The type to check
///
/// # Returns
/// * `bool` - true if supported, false otherwise
pub fn is_supported_type(controller_type: &str) -> bool {
    list_available_types().contains(&controller_type.to_lowercase())
}

/// Adapter that allows using a BaseGuardianDB where a GuardianDBKVStoreProvider
/// is expected.
pub struct GuardianDBAdapter {
    base_db: Arc<dyn BaseGuardianDB<Error = GuardianError>>,
}

impl GuardianDBAdapter {
    /// Wraps a BaseGuardianDB instance in the adapter.
    pub fn new(base_db: Arc<dyn BaseGuardianDB<Error = GuardianError>>) -> Self {
        Self { base_db }
    }
}

#[async_trait::async_trait]
impl crate::traits::GuardianDBKVStoreProvider for GuardianDBAdapter {
    type Error = GuardianError;

    async fn key_value(
        &self,
        address: &str,
        options: &mut crate::traits::CreateDBOptions,
    ) -> std::result::Result<
        Box<dyn crate::traits::KeyValueStore<Error = GuardianError>>,
        Self::Error,
    > {
        // Use BaseGuardianDB's create method to create a KeyValueStore.
        let store = self.base_db.create(address, "keyvalue", options).await?;

        // Convert it into a KeyValueStore using a wrapper.
        Ok(Box::new(KeyValueStoreAdapter::new(store)))
    }
}

/// Adapter that converts a generic Store into a specific KeyValueStore.
pub struct KeyValueStoreAdapter {
    store: Arc<dyn crate::traits::Store<Error = GuardianError>>,
}

impl KeyValueStoreAdapter {
    /// Wraps a generic Store in the adapter.
    pub fn new(store: Arc<dyn crate::traits::Store<Error = GuardianError>>) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl crate::traits::Store for KeyValueStoreAdapter {
    type Error = GuardianError;

    fn address(&self) -> &dyn crate::address::Address {
        self.store.address()
    }

    fn store_type(&self) -> &str {
        self.store.store_type()
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        // Close using interior mutability.
        // Signal the close through the event bus.
        let event_bus = self.store.event_bus();

        // Build the close event.
        let close_event = serde_json::json!({
            "event": "store_closed",
            "address": self.store.address().to_string(),
            "timestamp": chrono::Utc::now().to_rfc3339()
        });

        // Emit the close event (not critical if it fails).
        if let Ok(emitter) = event_bus.emitter::<serde_json::Value>().await {
            let _ = emitter.emit(close_event);
        }

        // Log the close.
        tracing::info!("Store adapter closed: {}", self.store.address());
        Ok(())
    }

    async fn drop(&self) -> std::result::Result<(), Self::Error> {
        // Drop with resource cleanup.
        // First close normally.
        self.close().await?;

        // Perform additional drop-specific cleanup.
        let op_log = self.store.op_log();

        // Force a log flush if possible (using try_write to avoid deadlock).
        if let Some(log_guard) = op_log.try_write() {
            // Ensure all pending operations are persisted.
            // (The log manages its own persistence, we just signal it.)
            drop(log_guard);
        }

        tracing::debug!("Store adapter dropped: {}", self.store.address());
        Ok(())
    }

    fn events(&self) -> &dyn crate::events::EmitterInterface {
        // events() is deprecated.
        unimplemented!("events() is deprecated, use event_bus() instead")
    }

    fn index(&self) -> Box<dyn crate::traits::StoreIndex<Error = Self::Error> + Send + Sync> {
        self.store.index()
    }

    fn cache(&self) -> Arc<dyn crate::data_store::Datastore> {
        self.store.cache()
    }

    async fn load(&self, amount: usize) -> std::result::Result<(), Self::Error> {
        // Implementation of load using the store's client.
        let client = self.store.client();
        let op_log = self.store.op_log();

        // Load entries from Iroh up to the specified limit.
        let mut loaded_count = 0;

        // Get the current heads of the log.
        let heads = {
            let log_guard = op_log.read();
            log_guard.heads().clone()
        };

        // For each head, load entries from Iroh.
        for head_entry in heads {
            if loaded_count >= amount {
                break;
            }

            // Try to load the entry from Iroh using cat_bytes with the hash string.
            let head_hash = head_entry.hash();
            let head_hash_str = hex::encode(head_hash.as_bytes());
            if let Ok(data) = client.cat_bytes(&head_hash_str).await {
                // Process the loaded data using postcard.
                if let Ok(entry) =
                    crate::guardian::serializer::deserialize::<crate::log::entry::Entry>(&data)
                {
                    // Add the entry to the log if it does not exist yet.
                    let entry_hash = entry.hash();
                    {
                        let mut log_guard = op_log.write();
                        if !log_guard.has(entry_hash) {
                            // Serialize the entry to add it to the log.
                            let entry_bytes =
                                crate::guardian::serializer::serialize(&entry).unwrap_or_default();
                            log_guard.append(&String::from_utf8_lossy(&entry_bytes), None);
                            loaded_count += 1;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn sync(
        &self,
        heads: Vec<crate::log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        // Implementation of sync with the provided heads.
        let op_log = self.store.op_log();
        let client = self.store.client();

        // For each provided head, sync the entries.
        for head_entry in heads {
            // Check whether we already have this entry.
            {
                let log_guard = op_log.read();
                if log_guard.has(head_entry.hash()) {
                    continue; // We already have this entry.
                }
            }

            // Load the entry and its dependencies from Iroh.
            let mut entries_to_add = Vec::new();
            let mut queue = vec![head_entry.clone()];

            while let Some(entry) = queue.pop() {
                entries_to_add.push(entry.clone());

                // Load parent entries (next).
                for next_hash in &entry.next {
                    // Use the hash string directly with cat_bytes.
                    let next_hash_str = hex::encode(next_hash.as_bytes());
                    if let Ok(data) = client.cat_bytes(&next_hash_str).await
                        && let Ok(parent_entry) = crate::guardian::serializer::deserialize::<
                            crate::log::entry::Entry,
                        >(&data)
                    {
                        let log_guard = op_log.read();
                        if !log_guard.has(parent_entry.hash()) {
                            drop(log_guard);
                            queue.push(parent_entry);
                        }
                    }
                }
            }

            // Add all entries to the log in reverse order.
            {
                let mut log_guard = op_log.write();
                for entry in entries_to_add.iter().rev() {
                    if !log_guard.has(entry.hash()) {
                        let entry_bytes =
                            crate::guardian::serializer::serialize(entry).unwrap_or_default();
                        log_guard.append(&String::from_utf8_lossy(&entry_bytes), None);
                    }
                }
            }
        }

        Ok(())
    }

    async fn load_more_from(&self, amount: u64, entries: Vec<crate::log::entry::Entry>) {
        // Implementation of load_more_from starting from the provided entries.
        let op_log = self.store.op_log();
        let client = self.store.client();
        let mut loaded_count = 0u64;

        for entry in entries {
            if loaded_count >= amount {
                break;
            }

            // Load previous entries (next) recursively.
            for next_hash in &entry.next {
                if loaded_count >= amount {
                    break;
                }

                // Use the hash string directly with cat_bytes.
                let next_hash_str = hex::encode(next_hash.as_bytes());
                if let Ok(data) = client.cat_bytes(&next_hash_str).await
                    && let Ok(parent_entry) =
                        crate::guardian::serializer::deserialize::<crate::log::entry::Entry>(&data)
                {
                    // Check whether we already have this entry.
                    let should_add = {
                        let log_guard = op_log.read();
                        !log_guard.has(parent_entry.hash())
                    };

                    if should_add {
                        // Add the entry to the log using try_write.
                        if let Some(mut log_guard) = op_log.try_write() {
                            let entry_bytes = crate::guardian::serializer::serialize(&parent_entry)
                                .unwrap_or_default();
                            log_guard.append(&String::from_utf8_lossy(&entry_bytes), None);
                            loaded_count += 1;
                        }
                    }
                }
            }
        }
    }

    async fn load_from_snapshot(&self) -> std::result::Result<(), Self::Error> {
        // Implementation of load_from_snapshot.
        let client = self.store.client();
        let op_log = self.store.op_log();
        let store_address = self.store.address();
        let snapshot_path = format!("{}/snapshot", store_address);

        // Try to load the snapshot from Iroh using cat_bytes.
        if let Ok(snapshot_data) = client.cat_bytes(&snapshot_path).await
            && let Ok(snapshot) = crate::guardian::serializer::deserialize::<
                Vec<crate::log::entry::Entry>,
            >(&snapshot_data)
        {
            // Load all entries from the snapshot.
            let mut log_guard = op_log.write();
            for entry in &snapshot {
                if !log_guard.has(entry.hash()) {
                    let entry_bytes =
                        crate::guardian::serializer::serialize(entry).unwrap_or_default();
                    log_guard.append(&String::from_utf8_lossy(&entry_bytes), None);
                }
            }

            drop(log_guard);

            // Log the successful load.
            tracing::info!(
                "Successfully loaded {} entries from snapshot",
                snapshot.len()
            );

            return Ok(());
        }

        // If there is no snapshot, return Ok (it is not an error).
        Ok(())
    }

    fn op_log(&self) -> Arc<parking_lot::RwLock<crate::log::Log>> {
        self.store.op_log()
    }

    fn client(&self) -> Arc<crate::p2p::network::client::IrohClient> {
        self.store.client()
    }

    fn db_name(&self) -> &str {
        self.store.db_name()
    }

    fn identity(&self) -> &crate::log::identity::Identity {
        self.store.identity()
    }

    fn access_controller(&self) -> &dyn crate::access_control::traits::AccessController {
        self.store.access_controller()
    }

    async fn add_operation(
        &self,
        op: crate::stores::operation::Operation,
        on_progress_callback: Option<crate::traits::ProgressCallback>,
    ) -> std::result::Result<crate::log::entry::Entry, Self::Error> {
        // Implementation of add_operation.
        // Since we hold Arc<Store>, we use interior mutability through the oplog.
        let op_log = self.store.op_log();
        let identity = self.store.identity();
        let client = self.store.client();

        // Serialize the operation using postcard.
        let payload = crate::guardian::serializer::serialize(&op)
            .map_err(|e| GuardianError::Store(format!("Failed to serialize operation: {}", e)))?;

        // Get the current heads.
        let heads = {
            let log_guard = op_log.read();
            log_guard.heads()
        };

        let store_id = self.store.db_name();
        let next_hashes: Vec<crate::log::entry::EntryOrHash> = heads
            .iter()
            .map(|entry| crate::log::entry::EntryOrHash::Entry(entry.as_ref()))
            .collect();

        let entry = crate::log::entry::Entry::new(
            identity.clone(),
            store_id,
            &payload,
            &next_hashes,
            None, // clock
        );

        // Store the entry in Iroh using add_bytes.
        let entry_data = crate::guardian::serializer::serialize(&entry)
            .map_err(|e| GuardianError::Store(format!("Failed to serialize entry: {}", e)))?;

        let _add_response = client
            .add_bytes(entry_data.clone())
            .await
            .map_err(|e| GuardianError::Store(format!("Failed to store entry: {}", e)))?;

        // Add the entry to the log using the proper append.
        {
            let mut log_guard = op_log.write();
            let entry_str = String::from_utf8_lossy(&entry_data).to_string();
            log_guard.append(&entry_str, None);
        }

        // Call the progress callback if provided.
        if let Some(callback) = on_progress_callback {
            // Send the entry through the channel.
            if (callback.send(entry.clone()).await).is_err() {
                // If it fails, just warn.
                tracing::warn!("Failed to send progress callback");
            }
        }

        Ok(entry)
    }

    fn span(&self) -> Arc<tracing::Span> {
        self.store.span()
    }

    fn tracer(&self) -> Arc<crate::traits::TracerWrapper> {
        self.store.tracer()
    }

    fn event_bus(&self) -> Arc<crate::p2p::EventBus> {
        self.store.event_bus()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait::async_trait]
impl crate::traits::KeyValueStore for KeyValueStoreAdapter {
    async fn put(
        &self,
        key: &str,
        value: Vec<u8>,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Put operation.
        let operation = crate::stores::operation::Operation::new(
            Some(key.to_string()),
            "PUT".to_string(),
            Some(value),
        );

        // Since we hold Arc<Store>, we use interior mutability through the oplog.
        // The operation is persisted directly in the store's log.
        let op_log = self.store.op_log();

        // Serialize the operation with postcard and convert it to a lossy string.
        let operation_bytes = crate::guardian::serializer::serialize(&operation)
            .map_err(|e| GuardianError::Store(format!("Failed to serialize operation: {}", e)))?;
        let entry_data = String::from_utf8_lossy(&operation_bytes).to_string();

        {
            let mut log_guard = op_log.write();
            log_guard.append(&entry_data, None);
        }

        Ok(operation)
    }

    async fn get(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
        // Search the store's oplog for entries containing the key.
        let op_log = self.store.op_log();
        let log_guard = op_log.read();

        // Look for the most recent entry with the key.
        for entry in log_guard.values().into_iter().rev() {
            let operation_data = entry.payload();
            // Deserialize the operation using postcard.
            if let Ok(operation) = crate::guardian::serializer::deserialize::<
                crate::stores::operation::Operation,
            >(operation_data)
                && let Some(op_key) = &operation.key
            {
                if op_key == key && operation.op == "PUT" {
                    return Ok(Some(operation.value));
                } else if op_key == key && operation.op == "DELETE" {
                    return Ok(None);
                }
            }
        }

        Ok(None)
    }

    async fn delete(
        &self,
        key: &str,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Delete operation.
        let operation = crate::stores::operation::Operation::new(
            Some(key.to_string()),
            "DELETE".to_string(),
            None,
        );

        // Since we hold Arc<Store>, we use interior mutability through the oplog.
        // The operation is persisted directly in the store's log.
        let op_log = self.store.op_log();

        // Serialize the operation with postcard and convert it to a lossy string.
        let operation_bytes = crate::guardian::serializer::serialize(&operation)
            .map_err(|e| GuardianError::Store(format!("Failed to serialize operation: {}", e)))?;
        let entry_data = String::from_utf8_lossy(&operation_bytes).to_string();

        {
            let mut log_guard = op_log.write();
            log_guard.append(&entry_data, None);
        }

        Ok(operation)
    }

    fn all(&self) -> std::collections::HashMap<String, Vec<u8>> {
        // Build a HashMap with all key-value pairs from the store.
        let mut result = std::collections::HashMap::new();
        let op_log = self.store.op_log();
        let log_guard = op_log.read();

        // Process all entries in the oplog.
        for entry in log_guard.values() {
            let operation_data = entry.payload();
            // Deserialize the operation using postcard.
            if let Ok(operation) = crate::guardian::serializer::deserialize::<
                crate::stores::operation::Operation,
            >(operation_data)
                && let Some(key) = &operation.key
            {
                match operation.op.as_str() {
                    "PUT" => {
                        result.insert(key.clone(), operation.value);
                    }
                    "DELETE" => {
                        result.remove(key);
                    }
                    _ => {} // Ignore other operations.
                }
            }
        }

        result
    }

    async fn share_ticket(&self) -> std::result::Result<String, Self::Error> {
        // This adapter is based on the oplog/access-control, not on iroh-docs.
        Err(GuardianError::Store(
            "share_ticket is not supported by KeyValueStoreAdapter".to_string(),
        ))
    }
}
