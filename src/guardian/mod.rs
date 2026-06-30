use crate::guardian::core::{GuardianDB as BaseGuardianDB, NewGuardianDBOptions};
use crate::guardian::error::{GuardianError, Result};
use crate::p2p::network::client::IrohClient;
use crate::traits::{
    AsyncDocumentFilter, BaseGuardianDB as BaseGuardianDBTrait, CreateDBOptions, Document,
    DocumentStore, EventLogStore, GuardianDBKVStoreProvider, KeyValueStore, ProgressCallback,
    Store,
};
use parking_lot::RwLock;
#[cfg(feature = "odm")]
use std::collections::BTreeSet;
use std::sync::Arc;

pub mod core;
pub mod error;
pub mod serializer;

pub struct GuardianDB {
    base: BaseGuardianDB,
    #[cfg(feature = "odm")]
    collection_names: Arc<RwLock<BTreeSet<String>>>,
}

impl GuardianDB {
    /// Creates a new GuardianDB instance.
    pub async fn new(client: IrohClient, options: Option<NewGuardianDBOptions>) -> Result<Self> {
        use crate::log::identity::{DefaultIdentificator, Identificator};

        // Determine the data directory for persisting the identity
        let directory = options
            .as_ref()
            .and_then(|o| o.directory.clone())
            .unwrap_or_else(|| std::path::PathBuf::from("./GuardianDB"));

        let identity_path = directory.join("identity.json");

        // Try to load a previously saved identity, otherwise create and persist a new one
        let identity = if identity_path.exists() {
            match std::fs::read_to_string(&identity_path) {
                Ok(data) => match serde_json::from_str::<crate::log::identity::Identity>(&data) {
                    Ok(id) => {
                        tracing::debug!("Loaded persisted identity from {:?}", identity_path);
                        id
                    }
                    Err(e) => {
                        tracing::warn!("Failed to deserialize identity file, creating new: {}", e);
                        let mut identificator = DefaultIdentificator::new();
                        let id = identificator.create(&client.node_id().to_string());
                        Self::save_identity(&identity_path, &id);
                        id
                    }
                },
                Err(e) => {
                    tracing::warn!("Failed to read identity file, creating new: {}", e);
                    let mut identificator = DefaultIdentificator::new();
                    let id = identificator.create(&client.node_id().to_string());
                    Self::save_identity(&identity_path, &id);
                    id
                }
            }
        } else {
            let mut identificator = DefaultIdentificator::new();
            let id = identificator.create(&client.node_id().to_string());
            Self::save_identity(&identity_path, &id);
            id
        };

        let base = BaseGuardianDB::new_guardian_db(client, identity, options).await?;
        Ok(GuardianDB {
            base,
            #[cfg(feature = "odm")]
            collection_names: Arc::new(RwLock::new(BTreeSet::new())),
        })
    }

    /// Persists the identity to a JSON file so it can be reloaded across sessions
    fn save_identity(path: &std::path::Path, identity: &crate::log::identity::Identity) {
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::warn!("Failed to create identity directory: {}", e);
            return;
        }
        match serde_json::to_string_pretty(identity) {
            Ok(data) => {
                if let Err(e) = std::fs::write(path, data) {
                    tracing::warn!("Failed to write identity file: {}", e);
                } else {
                    tracing::debug!("Identity persisted to {:?}", path);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to serialize identity: {}", e);
            }
        }
    }

    /// Creates an EventLogStore.
    pub async fn log(
        &self,
        address: &str,
        options: Option<CreateDBOptions>,
    ) -> Result<Arc<dyn EventLogStore<Error = GuardianError>>> {
        let mut opts = options.unwrap_or_default();
        opts.create = Some(true);
        opts.store_type = Some("eventlog".to_string());

        // Pass the GuardianDB's event_bus into the options.
        if opts.event_bus.is_none() {
            opts.event_bus = Some((*self.base.event_bus()).clone());
        }

        tracing::debug!(
            address = address,
            "GuardianDB::log - Creating EventLogStore"
        );

        // Use create() directly for simple names (without a valid hash).
        // If it already exists, open it via the full address to preserve history.
        let store = match self
            .base
            .create(address, "eventlog", Some(opts.clone()))
            .await
        {
            Ok(store) => store,
            Err(GuardianError::DatabaseAlreadyExists(existing_addr)) => {
                tracing::debug!(address = address, full_addr = %existing_addr, "EventLogStore already exists, opening existing one");
                self.base.open(&existing_addr, opts).await?
            }
            Err(e) => return Err(e),
        };

        tracing::debug!(
            address = address,
            store_type = store.store_type(),
            has_index = store.index().supports_entry_queries(),
            "EventLogStore created"
        );

        // Check that the returned store is of the correct type.
        if store.store_type() == "eventlog" {
            // Create a simple wrapper that implements EventLogStore.
            Ok(Arc::new(EventLogStoreWrapper::new(store)))
        } else {
            Err(GuardianError::Store(format!(
                "Incorrect store type. Expected: eventlog, found: {}",
                store.store_type()
            )))
        }
    }

    /// Creates a KeyValueStore.
    pub async fn key_value(
        &self,
        address: &str,
        options: Option<CreateDBOptions>,
    ) -> Result<Arc<dyn KeyValueStore<Error = GuardianError>>> {
        let mut opts = options.unwrap_or_default();
        opts.create = Some(true);
        opts.store_type = Some("keyvalue".to_string());

        // Pass the GuardianDB's event_bus into the options.
        if opts.event_bus.is_none() {
            opts.event_bus = Some((*self.base.event_bus()).clone());
        }

        // Use create() directly for simple names.
        // If it already exists, open it via the full address to preserve the data.
        let store = match self
            .base
            .create(address, "keyvalue", Some(opts.clone()))
            .await
        {
            Ok(store) => store,
            Err(GuardianError::DatabaseAlreadyExists(existing_addr)) => {
                tracing::debug!(address = address, full_addr = %existing_addr, "KeyValueStore already exists, opening existing one");
                self.base.open(&existing_addr, opts).await?
            }
            Err(e) => return Err(e),
        };

        // For KeyValueStore, we create a generic wrapper.
        if store.store_type() == "keyvalue" {
            Ok(Arc::new(KeyValueStoreWrapper::new(store)))
        } else {
            Err(GuardianError::Store(format!(
                "Incorrect store type. Expected: keyvalue, found: {}",
                store.store_type()
            )))
        }
    }

    /// Creates a DocumentStore.
    pub async fn docs(
        &self,
        address: &str,
        options: Option<CreateDBOptions>,
    ) -> Result<Arc<dyn DocumentStore<Error = GuardianError>>> {
        let mut opts = options.unwrap_or_default();
        opts.create = Some(true);
        opts.store_type = Some("document".to_string());

        // Pass the GuardianDB's event_bus into the options.
        if opts.event_bus.is_none() {
            opts.event_bus = Some((*self.base.event_bus()).clone());
        }

        // Use create() directly for simple names. Like log() and key_value(),
        // initCollection should be idempotent and reopen a store that already
        // exists.
        let open_options = opts.clone();
        let store = match self.base.create(address, "document", Some(opts)).await {
            Ok(store) => store,
            Err(GuardianError::DatabaseAlreadyExists(existing_addr)) => {
                tracing::debug!(
                    address = address,
                    full_addr = %existing_addr,
                    "DocumentStore already exists, opening existing one"
                );
                self.base.open(&existing_addr, open_options).await?
            }
            Err(error) => return Err(error),
        };

        // Check that the returned store is of the correct type.
        if store.store_type() == "document" {
            #[cfg(feature = "odm")]
            self.collection_names.write().insert(address.to_string());
            // Create a wrapper that implements DocumentStore.
            Ok(Arc::new(DocumentStoreWrapper::new(store)))
        } else {
            Err(GuardianError::Store(format!(
                "Incorrect store type. Expected: document, found: {}",
                store.store_type()
            )))
        }
    }

    /// Initializes a schemaless ODM collection backed by a DocumentStore.
    #[cfg(feature = "odm")]
    pub async fn init_collection(&self, name: &str) -> crate::odm::Result<crate::odm::Collection> {
        let store = self.docs(name, None).await?;
        let storage = Arc::new(crate::odm::DocumentStoreStorage::new(store));
        crate::odm::Collection::schemaless(name, storage).await
    }

    /// Initializes an ODM collection with an explicit runtime schema.
    #[cfg(feature = "odm")]
    pub async fn init_collection_with_schema(
        &self,
        name: &str,
        schema: crate::odm::ModelSchema,
    ) -> crate::odm::Result<crate::odm::Collection> {
        let store = self.docs(name, None).await?;
        let storage = Arc::new(crate::odm::DocumentStoreStorage::new(store));
        crate::odm::Collection::new(name, schema, storage).await
    }

    /// Initializes a strongly typed collection generated by `#[derive(Model)]`.
    #[cfg(feature = "odm")]
    pub async fn model_collection<M: crate::odm::Model>(
        &self,
    ) -> crate::odm::Result<crate::odm::TypedCollection<M>> {
        let schema = M::schema();
        let store = self.docs(schema.collection(), None).await?;
        let storage = Arc::new(crate::odm::DocumentStoreStorage::new(store));
        crate::odm::TypedCollection::new(storage).await
    }

    /// Returns collection names initialized by this GuardianDB instance.
    #[cfg(feature = "odm")]
    pub fn list_collections(&self) -> Vec<String> {
        self.collection_names.read().iter().cloned().collect()
    }

    /// Direct access to the BaseGuardianDB for advanced functionality.
    pub fn base(&self) -> &BaseGuardianDB {
        &self.base
    }

    /// Returns a list of all currently open/managed stores.
    /// Each element is a tuple (address, reference to the store).
    pub fn list_stores(
        &self,
    ) -> Vec<(String, Arc<dyn Store<Error = GuardianError> + Send + Sync>)> {
        self.base.list_stores()
    }

    /// Convenience method to register an access controller type with an explicit name.
    pub fn register_access_control_type_with_name(
        &self,
        controller_type: &str,
        constructor: crate::traits::AccessControllerConstructor,
    ) -> Result<()> {
        self.base
            .register_access_control_type_with_name(controller_type, constructor)
    }

    /// Convenience method to register an access controller with the default type.
    pub async fn register_access_control_type(
        &self,
        constructor: crate::traits::AccessControllerConstructor,
    ) -> Result<()> {
        self.base.register_access_control_type(constructor).await
    }

    /// Convenience method to get an access controller constructor.
    pub fn get_access_control_type(
        &self,
        controller_type: &str,
    ) -> Option<crate::traits::AccessControllerConstructor> {
        self.base.get_access_control_type(controller_type)
    }

    /// Convenience method to list access controller type names.
    pub fn access_control_types_names(&self) -> Vec<String> {
        self.base.access_control_types_names()
    }

    /// Convenience method to register the default access controllers.
    pub async fn register_default_access_control_types(&self) -> Result<()> {
        self.base.register_default_access_control_types().await
    }

    /// Connects to and synchronizes with a specific peer.
    ///
    /// This method facilitates manual peer connection when automatic discovery
    /// is not enough or you want to force a synchronization.
    ///
    /// # Arguments
    /// * `peer_id` - NodeId of the peer to synchronize with
    ///
    /// # Returns
    /// `Ok(())` if the synchronization was started successfully
    pub async fn connect_to_peer(&self, peer_id: iroh::EndpointId) -> Result<()> {
        self.base.connect_to_peer(peer_id).await
    }
}

/// Wrapper that adapts a generic Store to EventLogStore.
///
/// IMPLEMENTED SOLUTION: the &mut self limitation was solved by downcasting to
/// BaseStore, which implements add_operation(&self) in a thread-safe way using
/// an internal Arc<RwLock<T>>.
pub struct EventLogStoreWrapper {
    store: Arc<dyn Store<Error = GuardianError> + Send + Sync>,
}

impl EventLogStoreWrapper {
    fn new(store: Arc<dyn Store<Error = GuardianError> + Send + Sync>) -> Self {
        Self { store }
    }

    /// Returns a reference to the inner store.
    pub fn inner_store(&self) -> &Arc<dyn Store<Error = GuardianError> + Send + Sync> {
        &self.store
    }

    /// Tries to access the underlying BaseStore via downcast.
    ///
    /// This method is useful for advanced operations such as manual peer sync.
    /// Returns `None` if the inner store is not of the expected type.
    pub fn try_get_basestore(&self) -> Option<&crate::stores::base_store::BaseStore> {
        // Try to downcast to GuardianDBEventLogStore.
        if let Some(event_log_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::event_log_store::GuardianDBEventLogStore>()
        {
            return Some(event_log_store.basestore());
        }
        None
    }

    /// Connects to and synchronizes with a specific peer.
    ///
    /// This method facilitates manual peer connection when automatic discovery
    /// is not enough or you want to force a synchronization.
    ///
    /// # Arguments
    /// * `peer_id` - NodeId of the peer to synchronize with
    ///
    /// # Returns
    /// `Ok(())` if the synchronization was started successfully
    pub async fn connect_to_peer(&self, peer_id: iroh::EndpointId) -> Result<()> {
        if let Some(base_store) = self.try_get_basestore() {
            base_store.exchange_heads(peer_id).await
        } else {
            Err(GuardianError::Store(
                "Could not access BaseStore for synchronization".to_string(),
            ))
        }
    }

    /// Optimized query using the store's index.
    fn query_from_index(
        &self,
        options: &crate::traits::StreamOptions,
    ) -> Result<Vec<crate::log::entry::Entry>> {
        let index = self.store.index();

        // Simple query by amount (the most common case).
        let is_simple_amount_query = options.gt.is_none()
            && options.gte.is_none()
            && options.lt.is_none()
            && options.lte.is_none();

        if is_simple_amount_query {
            let amount = match options.amount {
                Some(a) if a > 0 => a as usize,
                Some(-1) | None => {
                    // -1 or None means "all entries".
                    match index.len() {
                        Ok(len) => len,
                        Err(_) => return self.query_from_oplog(options), // Fallback
                    }
                }
                _ => 0,
            };

            // Use the index's optimized method if available.
            if let Some(entries) = index.get_last_entries(amount) {
                return Ok(entries);
            }
        }

        // Query by a specific Hash.
        if let Some(hash) = options.gte.as_ref()
            && options.amount == Some(1)
            && options.gt.is_none()
            && options.lt.is_none()
            && options.lte.is_none()
        {
            if let Some(entry) = index.get_entry_by_hash(hash) {
                return Ok(vec![entry]);
            } else {
                return Ok(Vec::new()); // Hash not found.
            }
        }

        // For more complex queries, use the fallback.
        self.query_from_oplog(options)
    }

    /// Fallback: direct oplog scan when the index does not support the query.
    fn query_from_oplog(
        &self,
        options: &crate::traits::StreamOptions,
    ) -> Result<Vec<crate::log::entry::Entry>> {
        let oplog = self.store.op_log();
        let oplog_guard = oplog.read();

        // Collect all entries from the oplog.
        let mut all_entries: Vec<_> = oplog_guard
            .values()
            .iter()
            .map(|arc_entry| arc_entry.as_ref().clone())
            .collect();

        // Sort chronologically (oldest first - insertion order).
        all_entries.sort_by_key(|b| b.clock().time());

        // Apply Hash filters if specified.
        let mut filtered_entries = all_entries;

        // gte filter (greater than or equal).
        if let Some(hash) = &options.gte {
            if let Some(start_idx) = filtered_entries.iter().position(|e| e.hash() == hash) {
                filtered_entries = filtered_entries.into_iter().skip(start_idx).collect();
            } else {
                return Ok(Vec::new()); // Hash not found.
            }
        }

        // gt filter (greater than).
        if let Some(hash) = &options.gt {
            if let Some(start_idx) = filtered_entries.iter().position(|e| e.hash() == hash) {
                filtered_entries = filtered_entries.into_iter().skip(start_idx + 1).collect();
            } else {
                return Ok(Vec::new()); // Hash not found.
            }
        }

        // lte filter (less than or equal).
        if let Some(hash) = &options.lte {
            if let Some(end_idx) = filtered_entries.iter().position(|e| e.hash() == hash) {
                filtered_entries = filtered_entries.into_iter().take(end_idx + 1).collect();
            } else {
                return Ok(Vec::new()); // Hash not found.
            }
        }

        // lt filter (less than).
        if let Some(hash) = &options.lt {
            if let Some(end_idx) = filtered_entries.iter().position(|e| e.hash() == hash) {
                filtered_entries = filtered_entries.into_iter().take(end_idx).collect();
            } else {
                return Ok(Vec::new()); // Hash not found.
            }
        }

        // Apply the amount limit.
        let amount = match options.amount {
            Some(a) if a > 0 => a as usize,
            Some(-1) | None => filtered_entries.len(), // -1 or None = all.
            _ => 0,
        };

        filtered_entries.truncate(amount);
        Ok(filtered_entries)
    }
}

#[async_trait::async_trait]
impl Store for EventLogStoreWrapper {
    type Error = GuardianError;

    #[allow(deprecated)]
    fn events(&self) -> &dyn crate::events::EmitterInterface {
        self.store.events()
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        self.store.close().await
    }

    fn address(&self) -> &dyn crate::address::Address {
        self.store.address()
    }

    fn index(&self) -> Box<dyn crate::traits::StoreIndex<Error = GuardianError> + Send + Sync> {
        self.store.index()
    }

    fn store_type(&self) -> &str {
        self.store.store_type()
    }

    fn cache(&self) -> Arc<dyn crate::data_store::Datastore> {
        self.store.cache()
    }

    async fn drop(&self) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    async fn load(&self, amount: usize) -> std::result::Result<(), Self::Error> {
        self.store.load(amount).await
    }

    async fn sync(
        &self,
        heads: Vec<crate::log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        self.store.sync(heads).await
    }

    async fn load_more_from(&self, _amount: u64, entries: Vec<crate::log::entry::Entry>) {
        self.store.load_more_from(_amount, entries).await
    }

    async fn load_from_snapshot(&self) -> std::result::Result<(), Self::Error> {
        self.store.load_from_snapshot().await
    }

    fn op_log(&self) -> Arc<RwLock<crate::log::Log>> {
        self.store.op_log()
    }

    fn client(&self) -> Arc<IrohClient> {
        unimplemented!("Adaptation between iroh client types pending")
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
        on_progress_callback: Option<ProgressCallback>,
    ) -> std::result::Result<crate::log::entry::Entry, Self::Error> {
        // Delegate directly to the inner store through the Store trait.
        self.store.add_operation(op, on_progress_callback).await
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
impl EventLogStore for EventLogStoreWrapper {
    async fn add(
        &self,
        data: Vec<u8>,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Create an ADD operation and add it to the store.
        let operation =
            crate::stores::operation::Operation::new(None, "ADD".to_string(), Some(data));

        let _entry = self.add_operation(operation.clone(), None).await?;

        // Return the operation that was successfully added.
        // ***In a more sophisticated implementation we could re-parse the entry
        // to ensure consistency, but for this case the original operation suffices.
        Ok(operation)
    }

    async fn get(
        &self,
        hash: &iroh_blobs::Hash,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Look up a specific operation by Hash.

        // First, try to use the index for an optimized lookup.
        if let Some(entry) = self.store.index().get_entry_by_hash(hash) {
            // Convert the Entry into an Operation using parse_operation.
            let operation = crate::stores::operation::parse_operation(entry)
                .map_err(|e| GuardianError::Store(format!("Failed to parse entry: {}", e)))?;
            return Ok(operation);
        }

        // Fallback: scan the oplog directly.
        let oplog = self.store.op_log();
        let oplog_guard = oplog.read();

        // Linear scan of the oplog by Hash.
        for arc_entry in oplog_guard.values() {
            if arc_entry.hash() == hash {
                // Convert Entry into Operation.
                let entry = arc_entry.as_ref().clone();
                let operation = crate::stores::operation::parse_operation(entry)
                    .map_err(|e| GuardianError::Store(format!("Failed to parse entry: {}", e)))?;
                return Ok(operation);
            }
        }

        // Hash not found.
        Err(GuardianError::Store(format!(
            "Operation not found for Hash: {}",
            hex::encode(hash.as_bytes())
        )))
    }

    async fn list(
        &self,
        options: Option<crate::traits::StreamOptions>,
    ) -> std::result::Result<Vec<crate::stores::operation::Operation>, Self::Error> {
        // List operations with optional filters.
        let options = options.unwrap_or_default();

        // Try the optimized index first.
        let entries = if self.store.index().supports_entry_queries() {
            // Optimized query using the index.
            self.query_from_index(&options)?
        } else {
            // Fallback: scan the oplog.
            self.query_from_oplog(&options)?
        };

        // Convert all entries into operations.
        let mut operations = Vec::with_capacity(entries.len());
        for entry in entries {
            match crate::stores::operation::parse_operation(entry) {
                Ok(operation) => operations.push(operation),
                Err(e) => {
                    // Log the error but keep processing other entries.
                    eprintln!("Warning: Failed to parse entry: {}", e);
                }
            }
        }

        Ok(operations)
    }
}

/// Wrapper that adapts a generic Store to KeyValueStore.
struct KeyValueStoreWrapper {
    store: Arc<dyn Store<Error = GuardianError> + Send + Sync>,
}

impl KeyValueStoreWrapper {
    fn new(store: Arc<dyn Store<Error = GuardianError> + Send + Sync>) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl Store for KeyValueStoreWrapper {
    type Error = GuardianError;

    #[allow(deprecated)]
    fn events(&self) -> &dyn crate::events::EmitterInterface {
        self.store.events()
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        self.store.close().await
    }

    fn address(&self) -> &dyn crate::address::Address {
        self.store.address()
    }

    fn index(&self) -> Box<dyn crate::traits::StoreIndex<Error = GuardianError> + Send + Sync> {
        self.store.index()
    }

    fn store_type(&self) -> &str {
        self.store.store_type()
    }

    fn cache(&self) -> Arc<dyn crate::data_store::Datastore> {
        self.store.cache()
    }

    async fn drop(&self) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    async fn load(&self, amount: usize) -> std::result::Result<(), Self::Error> {
        self.store.load(amount).await
    }

    async fn sync(
        &self,
        heads: Vec<crate::log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        self.store.sync(heads).await
    }

    async fn load_more_from(&self, _amount: u64, entries: Vec<crate::log::entry::Entry>) {
        self.store.load_more_from(_amount, entries).await
    }

    async fn load_from_snapshot(&self) -> std::result::Result<(), Self::Error> {
        self.store.load_from_snapshot().await
    }

    fn op_log(&self) -> Arc<RwLock<crate::log::Log>> {
        self.store.op_log()
    }

    fn client(&self) -> Arc<IrohClient> {
        unimplemented!("Adaptation between iroh client types pending")
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
        on_progress_callback: Option<ProgressCallback>,
    ) -> std::result::Result<crate::log::entry::Entry, Self::Error> {
        self.store.add_operation(op, on_progress_callback).await
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
impl KeyValueStore for KeyValueStoreWrapper {
    async fn get(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
        // Look up a value by key in the KeyValue store.

        // First, try the index (more efficient).
        let index = self.store.index();
        if let Ok(Some(bytes)) = index.get_bytes(key) {
            return Ok(Some(bytes));
        }

        // Fallback: scan the oplog for PUT operations with the specified key.
        let oplog = self.store.op_log();
        let oplog_guard = oplog.read();

        // Look for the most recent PUT operation with the specified key.
        let mut latest_value: Option<Vec<u8>> = None;
        let mut latest_time = 0;

        for arc_entry in oplog_guard.values() {
            let entry = arc_entry.as_ref().clone();

            // Convert Entry into Operation.
            if let Ok(operation) = crate::stores::operation::parse_operation(entry.clone()) {
                // Check whether it is an operation relevant to the key.
                if let Some(op_key) = operation.key()
                    && op_key == key
                {
                    let entry_time = entry.clock().time();

                    let op_str = operation.op();
                    if op_str == "PUT" {
                        // If it is more recent than the previous operation.
                        if entry_time > latest_time {
                            latest_time = entry_time;
                            latest_value = Some(operation.value().to_vec());
                        }
                    } else if op_str == "DEL" {
                        // If it is more recent than the previous operation and is a deletion.
                        if entry_time > latest_time {
                            latest_time = entry_time;
                            latest_value = None; // The value was deleted.
                        }
                    }
                }
            }
        }

        Ok(latest_value)
    }

    async fn put(
        &self,
        key: &str,
        value: Vec<u8>,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Create a PUT operation with key and value.
        let operation = crate::stores::operation::Operation::new(
            Some(key.to_string()),
            "PUT".to_string(),
            Some(value),
        );

        self.add_operation(operation.clone(), None).await?;
        Ok(operation)
    }

    async fn delete(
        &self,
        key: &str,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Create a DEL operation with the key.
        let operation = crate::stores::operation::Operation::new(
            Some(key.to_string()),
            "DEL".to_string(),
            None,
        );

        self.add_operation(operation.clone(), None).await?;
        Ok(operation)
    }

    fn all(&self) -> std::collections::HashMap<String, Vec<u8>> {
        let mut result = std::collections::HashMap::new();

        // First, try to collect from the index (more efficient if up to date).
        let index = self.store.index();
        if let Ok(keys) = index.keys() {
            for key in keys {
                if let Ok(Some(bytes)) = index.get_bytes(&key) {
                    result.insert(key, bytes);
                }
            }
        }

        // If the index returned no data, or as a complement, process the oplog.
        // This ensures we have the most up-to-date data, including unindexed operations.
        if result.is_empty() {
            let oplog = self.store.op_log();
            let oplog_guard = oplog.read();

            // Map key -> (timestamp, operation, value).
            let mut key_operations: std::collections::HashMap<
                String,
                (u64, String, Option<Vec<u8>>),
            > = std::collections::HashMap::new();

            // Collect all relevant operations.
            for arc_entry in oplog_guard.values() {
                let entry = arc_entry.as_ref().clone();

                // Convert Entry into Operation.
                if let Ok(operation) = crate::stores::operation::parse_operation(entry.clone())
                    && let Some(op_key) = operation.key()
                {
                    let timestamp = entry.clock().time();
                    let op_type = operation.op().to_string();
                    let value = if !operation.value().is_empty() {
                        Some(operation.value().to_vec())
                    } else {
                        None
                    };

                    // Update if it is more recent or did not exist before.
                    let key_clone = op_key.clone();
                    if let Some((existing_time, _, _)) = key_operations.get(&key_clone) {
                        if timestamp > *existing_time {
                            key_operations.insert(key_clone, (timestamp, op_type, value));
                        }
                    } else {
                        key_operations.insert(key_clone, (timestamp, op_type, value));
                    }
                }
            }

            // Process the final operations for each key.
            for (key, (_timestamp, op_type, value)) in key_operations {
                let op_str = op_type.as_str();
                if op_str == "PUT" {
                    if let Some(val) = value {
                        result.insert(key, val);
                    }
                } else if op_str == "DEL" {
                    // Remove from the list if it was deleted.
                    result.remove(&key);
                } else {
                    // For other operations, add if it has a value.
                    if let Some(val) = value {
                        result.insert(key, val);
                    }
                }
            }
        }

        result
    }

    async fn share_ticket(&self) -> std::result::Result<String, Self::Error> {
        // Delegate to the underlying GuardianDBKeyValue (which holds the iroh-docs doc).
        self.store
            .as_any()
            .downcast_ref::<crate::stores::kv_store::GuardianDBKeyValue>()
            .ok_or_else(|| {
                GuardianError::Store(
                    "share_ticket: underlying store is not a GuardianDBKeyValue".to_string(),
                )
            })?
            .share_ticket()
            .await
    }
}

/// Wrapper that adapts a generic Store to DocumentStore.
struct DocumentStoreWrapper {
    store: Arc<dyn Store<Error = GuardianError> + Send + Sync>,
}

impl DocumentStoreWrapper {
    fn new(store: Arc<dyn Store<Error = GuardianError> + Send + Sync>) -> Self {
        Self { store }
    }

    /// Searches the index for documents matching a key.
    fn search_documents_by_key(
        &self,
        key: &str,
        opts: &crate::traits::DocumentStoreGetOptions,
    ) -> Result<Vec<Document>> {
        let index = self.store.index();

        // Prepare the search key according to the options.
        let mut key_for_search = key.to_string();
        let has_multiple_terms = key.contains(' ');

        if has_multiple_terms {
            key_for_search = key_for_search.replace('.', " ");
        }
        if opts.case_insensitive {
            key_for_search = key_for_search.to_lowercase();
        }

        let mut documents = Vec::new();

        // Get all keys from the index.
        let all_keys = index.keys().unwrap_or_default();

        for index_key in all_keys {
            let mut index_key_for_search = index_key.clone();

            // Normalize the index key for the search.
            if opts.case_insensitive {
                index_key_for_search = index_key_for_search.to_lowercase();
            }

            // Check whether the key matches the search criteria.
            let matches = if opts.partial_matches {
                index_key_for_search.contains(&key_for_search)
            } else {
                index_key_for_search == key_for_search
            };

            if matches {
                // Look up the value in the index.
                if let Ok(Some(doc_bytes)) = index.get_bytes(&index_key) {
                    // Deserialize the document.
                    match serde_json::from_slice::<serde_json::Value>(&doc_bytes) {
                        Ok(json_value) => {
                            let doc: Document = Box::new(json_value);
                            documents.push(doc);
                        }
                        Err(e) => {
                            eprintln!(
                                "Warning: Failed to deserialize document for key '{}': {}",
                                index_key, e
                            );
                        }
                    }
                } else {
                    eprintln!(
                        "Warning: key '{}' found but without a corresponding value",
                        index_key
                    );
                }
            }
        }

        Ok(documents)
    }

    /// Searches for documents using operations from the oplog.
    fn search_documents_from_oplog(
        &self,
        key: &str,
        opts: &crate::traits::DocumentStoreGetOptions,
    ) -> Result<Vec<Document>> {
        let oplog = self.store.op_log();
        let oplog_guard = oplog.read();

        let mut documents = Vec::new();
        let mut processed_keys = std::collections::HashSet::new();

        // Collect all oplog entries into a vector to iterate in reverse order.
        let entries: Vec<Arc<crate::log::entry::Entry>> =
            oplog_guard.values().into_iter().collect();

        // Iterate in reverse order (newest to oldest) to ensure only the most
        // recent operation for each key is considered.
        for arc_entry in entries.iter().rev() {
            let entry: crate::log::entry::Entry = (**arc_entry).clone();

            // Convert Entry into Operation.
            if let Ok(operation) = crate::stores::operation::parse_operation(entry) {
                // Check whether the operation is relevant to documents.
                if let Some(op_key) = operation.key() {
                    // Avoid processing the same key multiple times.
                    if processed_keys.contains(op_key) {
                        continue;
                    }

                    let mut op_key_search = op_key.clone();
                    let mut key_search = key.to_string();

                    if opts.case_insensitive {
                        op_key_search = op_key_search.to_lowercase();
                        key_search = key_search.to_lowercase();
                    }

                    let matches = if opts.partial_matches {
                        op_key_search.contains(&key_search)
                    } else {
                        op_key_search == key_search
                    };

                    if matches {
                        processed_keys.insert(op_key.clone());

                        // If the most recent operation is DEL, skip this document.
                        if operation.op() == "DEL" {
                            continue;
                        }

                        // If it is PUT and has a value, add the document.
                        if operation.op() == "PUT" && !operation.value().is_empty() {
                            // Try to deserialize the value as a document.
                            match serde_json::from_slice::<serde_json::Value>(operation.value()) {
                                Ok(json_value) => {
                                    let doc: Document = Box::new(json_value);
                                    documents.push(doc);
                                }
                                Err(_) => {
                                    // If it cannot be deserialized as JSON, build a simple document.
                                    let simple_doc = serde_json::json!({
                                        "key": op_key,
                                        "value": String::from_utf8_lossy(operation.value()),
                                        "op_type": operation.op()
                                    });
                                    let doc: Document = Box::new(simple_doc);
                                    documents.push(doc);
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(documents)
    }

    /// Collects all documents from the index for queries.
    fn get_all_documents_from_index(&self) -> Result<Vec<Document>> {
        let index = self.store.index();
        let mut documents = Vec::new();

        let all_keys = index.keys().unwrap_or_default();

        eprintln!(
            "DEBUG: get_all_documents_from_index - Total keys in the index: {}",
            all_keys.len()
        );

        for key in all_keys {
            eprintln!(
                "DEBUG: get_all_documents_from_index - Processing key: {}",
                key
            );
            if let Ok(Some(doc_bytes)) = index.get_bytes(&key) {
                eprintln!(
                    "DEBUG: get_all_documents_from_index - Bytes retrieved for key '{}': {} bytes",
                    key,
                    doc_bytes.len()
                );
                match serde_json::from_slice::<serde_json::Value>(&doc_bytes) {
                    Ok(json_value) => {
                        eprintln!(
                            "DEBUG: get_all_documents_from_index - Document deserialized successfully: {:?}",
                            json_value
                        );
                        let doc: Document = Box::new(json_value);
                        documents.push(doc);
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: Failed to deserialize document for key '{}': {}",
                            key, e
                        );
                    }
                }
            } else {
                eprintln!(
                    "DEBUG: get_all_documents_from_index - No bytes found for key: {}",
                    key
                );
            }
        }

        eprintln!(
            "DEBUG: get_all_documents_from_index - Total documents collected: {}",
            documents.len()
        );
        Ok(documents)
    }
}

#[async_trait::async_trait]
impl Store for DocumentStoreWrapper {
    type Error = GuardianError;

    #[allow(deprecated)]
    fn events(&self) -> &dyn crate::events::EmitterInterface {
        self.store.events()
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        self.store.close().await
    }

    fn address(&self) -> &dyn crate::address::Address {
        self.store.address()
    }

    fn index(&self) -> Box<dyn crate::traits::StoreIndex<Error = GuardianError> + Send + Sync> {
        self.store.index()
    }

    fn store_type(&self) -> &str {
        self.store.store_type()
    }

    fn cache(&self) -> Arc<dyn crate::data_store::Datastore> {
        self.store.cache()
    }

    async fn drop(&self) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    async fn load(&self, amount: usize) -> std::result::Result<(), Self::Error> {
        self.store.load(amount).await
    }

    async fn sync(
        &self,
        heads: Vec<crate::log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        self.store.sync(heads).await
    }

    async fn load_more_from(&self, _amount: u64, entries: Vec<crate::log::entry::Entry>) {
        self.store.load_more_from(_amount, entries).await
    }

    async fn load_from_snapshot(&self) -> std::result::Result<(), Self::Error> {
        self.store.load_from_snapshot().await
    }

    fn op_log(&self) -> Arc<RwLock<crate::log::Log>> {
        self.store.op_log()
    }

    fn client(&self) -> Arc<IrohClient> {
        unimplemented!("Adaptation between iroh client types pending")
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
        on_progress_callback: Option<ProgressCallback>,
    ) -> std::result::Result<crate::log::entry::Entry, Self::Error> {
        // Delegate directly to the inner store through the Store trait.
        self.store.add_operation(op, on_progress_callback).await
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
impl DocumentStore for DocumentStoreWrapper {
    async fn put(
        &self,
        document: Document,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Extract the document key (try _id first, then id as a fallback).
        let key = if let Some(json_val) = document.downcast_ref::<serde_json::Value>() {
            json_val
                .get("_id")
                .or_else(|| json_val.get("id"))
                .or_else(|| json_val.get("key"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        } else {
            None
        };

        // Serialize the document to bytes.
        let data = if let Some(json_val) = document.downcast_ref::<serde_json::Value>() {
            serde_json::to_vec(json_val).map_err(|e| {
                GuardianError::Store(format!("Failed to serialize JSON document: {}", e))
            })?
        } else if let Some(bytes) = document.downcast_ref::<Vec<u8>>() {
            bytes.clone()
        } else {
            // For other types, use a generic serialization.
            format!("{:?}", document).into_bytes()
        };

        // Create a PUT operation for the document with the extracted key.
        let operation =
            crate::stores::operation::Operation::new(key, "PUT".to_string(), Some(data));

        self.add_operation(operation.clone(), None).await?;
        Ok(operation)
    }

    async fn delete(
        &self,
        key: &str,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Create a DEL operation for the document.
        let operation = crate::stores::operation::Operation::new(
            Some(key.to_string()),
            "DEL".to_string(),
            None,
        );

        self.add_operation(operation.clone(), None).await?;
        Ok(operation)
    }

    async fn put_batch(
        &self,
        values: Vec<Document>,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        if values.is_empty() {
            return Err(GuardianError::InvalidArgument(
                "Nothing to add to the store".to_string(),
            ));
        }

        let mut last_operation = None;
        for document in values {
            let op = self.put(document).await?;
            last_operation = Some(op);
        }

        Ok(last_operation.unwrap())
    }

    async fn put_all(
        &self,
        values: Vec<Document>,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        if values.is_empty() {
            return Err(GuardianError::InvalidArgument(
                "Nothing to add to the store".to_string(),
            ));
        }

        let mut last_operation = None;
        for document in values {
            let op = self.put(document).await?;
            last_operation = Some(op);
        }

        Ok(last_operation.unwrap())
    }

    async fn get(
        &self,
        key: &str,
        opts: Option<crate::traits::DocumentStoreGetOptions>,
    ) -> std::result::Result<Vec<Document>, Self::Error> {
        // Look up documents by key with advanced options.

        let opts = opts.unwrap_or_default();

        // Try the index first (more efficient).
        let documents_from_index = self.search_documents_by_key(key, &opts)?;

        if !documents_from_index.is_empty() {
            return Ok(documents_from_index);
        }

        // Fallback: scan the oplog if the index returns no results.
        // This can happen if the index has not been populated yet or is stale.
        let documents_from_oplog = self.search_documents_from_oplog(key, &opts)?;

        Ok(documents_from_oplog)
    }

    async fn query(
        &self,
        filter: AsyncDocumentFilter,
    ) -> std::result::Result<Vec<Document>, Self::Error> {
        // Query with a customizable asynchronous filter.

        // Get all available documents.
        let all_documents = self.get_all_documents_from_index()?;

        let mut filtered_documents = Vec::new();

        // Apply the asynchronous filter to each document.
        for document in all_documents {
            // Call the asynchronous filter.
            let filter_future = filter(&document);

            match filter_future.await {
                Ok(true) => {
                    // The document passed the filter.
                    filtered_documents.push(document);
                }
                Ok(false) => {
                    // The document did not pass the filter, continue.
                    continue;
                }
                Err(e) => {
                    // Filter error - we log it but keep processing.
                    eprintln!("Warning: Error applying filter to the document: {}", e);
                    continue;
                }
            }
        }

        Ok(filtered_documents)
    }

    async fn share_ticket(&self) -> std::result::Result<String, Self::Error> {
        // Delegate to the underlying GuardianDBDocumentStore (which holds the iroh-docs doc).
        self.store
            .as_any()
            .downcast_ref::<crate::stores::document_store::GuardianDBDocumentStore>()
            .ok_or_else(|| {
                GuardianError::Store(
                    "share_ticket: underlying store is not a GuardianDBDocumentStore".to_string(),
                )
            })?
            .share_ticket()
            .await
    }
}

#[async_trait::async_trait]
impl BaseGuardianDBTrait for GuardianDB {
    type Error = GuardianError;

    async fn open(
        &self,
        address: &str,
        options: &mut CreateDBOptions,
    ) -> std::result::Result<Arc<dyn Store<Error = GuardianError>>, Self::Error> {
        let opts = options.clone();
        let result = self.base.open(address, opts).await?;
        // Convert Send+Sync to non-Send+Sync
        Ok(result as Arc<dyn Store<Error = GuardianError>>)
    }

    async fn determine_address(
        &self,
        name: &str,
        store_type: &str,
        options: &crate::traits::DetermineAddressOptions,
    ) -> std::result::Result<Box<dyn crate::address::Address>, Self::Error> {
        let opts = Some(options.clone());
        let result = self.base.determine_address(name, store_type, opts).await?;
        Ok(Box::new(result))
    }

    fn client(&self) -> Arc<crate::p2p::network::client::IrohClient> {
        Arc::new(self.base.client().clone())
    }

    fn identity(&self) -> Arc<crate::log::identity::Identity> {
        Arc::new(self.base.identity().clone())
    }

    fn get_store(&self, address: &str) -> Option<Arc<dyn Store<Error = GuardianError>>> {
        self.base
            .get_store(address)
            .map(|store| store as Arc<dyn Store<Error = GuardianError>>)
    }

    async fn create(
        &self,
        name: &str,
        store_type: &str,
        options: &mut CreateDBOptions,
    ) -> std::result::Result<Arc<dyn Store<Error = GuardianError>>, Self::Error> {
        let opts = Some(options.clone());
        let result = self.base.create(name, store_type, opts).await?;
        Ok(result as Arc<dyn Store<Error = GuardianError>>)
    }

    fn register_store_type(
        &mut self,
        store_type: &str,
        constructor: crate::traits::StoreConstructor,
    ) {
        // BaseGuardianDB already uses Arc<RwLock<>> internally for store_types,
        // so it is thread-safe and does not need &mut self.
        self.base
            .register_store_type(store_type.to_string(), constructor);
    }

    fn unregister_store_type(&mut self, store_type: &str) {
        // BaseGuardianDB already uses Arc<RwLock<>> internally for store_types.
        self.base.unregister_store_type(store_type);
    }

    fn register_access_controller_type(
        &mut self,
        constructor: crate::traits::AccessControllerConstructor,
    ) -> std::result::Result<(), Self::Error> {
        // BaseGuardianDB already uses Arc<RwLock<>> internally for access_control_types,
        // so it is thread-safe. We use the legacy method that registers with the "simple" type.
        self.base
            .register_access_control_type_with_name("simple", constructor)
    }

    fn unregister_access_controller_type(&mut self, controller_type: &str) {
        // BaseGuardianDB already uses Arc<RwLock<>> internally for access_control_types.
        self.base.unregister_access_control_type(controller_type);
    }

    fn get_access_controller_type(
        &self,
        controller_type: &str,
    ) -> Option<crate::traits::AccessControllerConstructor> {
        self.base.get_access_controller_type(controller_type)
    }

    fn event_bus(&self) -> crate::p2p::EventBus {
        (*self.base.event_bus()).clone()
    }

    fn span(&self) -> &tracing::Span {
        self.base.span()
    }

    fn tracer(&self) -> Arc<crate::traits::TracerWrapper> {
        // Convert BoxedTracer into TracerWrapper.
        let boxed_tracer = self.base.tracer();
        Arc::new(crate::traits::TracerWrapper::new_opentelemetry(
            boxed_tracer,
        ))
    }
}

#[async_trait::async_trait]
impl GuardianDBKVStoreProvider for GuardianDB {
    type Error = GuardianError;

    async fn key_value(
        &self,
        address: &str,
        options: &mut CreateDBOptions,
    ) -> std::result::Result<Box<dyn KeyValueStore<Error = GuardianError>>, Self::Error> {
        // Use the already-implemented wrapper method that returns an Arc.
        let opts_clone = options.clone();
        let arc_store = self.key_value(address, Some(opts_clone)).await?;

        // Convert the Arc into a Box using a wrapper.
        Ok(Box::new(KeyValueStoreBoxWrapper::new(arc_store)))
    }
}

/// Wrapper to convert Arc<dyn KeyValueStore> into Box<dyn KeyValueStore>.
pub struct KeyValueStoreBoxWrapper {
    inner: Arc<dyn KeyValueStore<Error = GuardianError>>,
}

impl KeyValueStoreBoxWrapper {
    pub fn new(inner: Arc<dyn KeyValueStore<Error = GuardianError>>) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl Store for KeyValueStoreBoxWrapper {
    type Error = GuardianError;

    fn address(&self) -> &dyn crate::address::Address {
        self.inner.address()
    }

    fn store_type(&self) -> &str {
        self.inner.store_type()
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        // Delegate to the inner store using close().
        self.inner.close().await
    }

    async fn drop(&self) -> std::result::Result<(), Self::Error> {
        self.inner.close().await
    }

    fn events(&self) -> &dyn crate::events::EmitterInterface {
        // events() is deprecated.
        unimplemented!("events() is deprecated, use event_bus() instead")
    }

    fn index(&self) -> Box<dyn crate::traits::StoreIndex<Error = Self::Error> + Send + Sync> {
        self.inner.index()
    }

    fn cache(&self) -> Arc<dyn crate::data_store::Datastore> {
        self.inner.cache()
    }

    async fn load(&self, amount: usize) -> std::result::Result<(), Self::Error> {
        // Delegate directly to the inner store.
        self.inner.load(amount).await
    }

    async fn sync(
        &self,
        heads: Vec<crate::log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        // Delegate directly to the inner store.
        self.inner.sync(heads).await
    }

    async fn load_more_from(&self, _amount: u64, entries: Vec<crate::log::entry::Entry>) {
        // Delegate directly to the inner store.
        self.inner.load_more_from(_amount, entries).await
    }

    async fn load_from_snapshot(&self) -> std::result::Result<(), Self::Error> {
        // Delegate directly to the inner store.
        self.inner.load_from_snapshot().await
    }

    fn op_log(&self) -> Arc<parking_lot::RwLock<crate::log::Log>> {
        self.inner.op_log()
    }

    fn client(&self) -> Arc<crate::p2p::network::client::IrohClient> {
        self.inner.client()
    }

    fn db_name(&self) -> &str {
        self.inner.db_name()
    }

    fn identity(&self) -> &crate::log::identity::Identity {
        self.inner.identity()
    }

    fn access_controller(&self) -> &dyn crate::access_control::traits::AccessController {
        self.inner.access_controller()
    }

    async fn add_operation(
        &self,
        op: crate::stores::operation::Operation,
        on_progress_callback: Option<crate::traits::ProgressCallback>,
    ) -> std::result::Result<crate::log::entry::Entry, Self::Error> {
        // Delegate directly to the inner store.
        self.inner.add_operation(op, on_progress_callback).await
    }

    fn span(&self) -> Arc<tracing::Span> {
        self.inner.span()
    }

    fn tracer(&self) -> Arc<crate::traits::TracerWrapper> {
        self.inner.tracer()
    }

    fn event_bus(&self) -> Arc<crate::p2p::EventBus> {
        self.inner.event_bus()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait::async_trait]
impl KeyValueStore for KeyValueStoreBoxWrapper {
    async fn put(
        &self,
        key: &str,
        value: Vec<u8>,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Delegate to the inner KeyValueStore, which already implements put.
        self.inner.put(key, value).await
    }

    async fn get(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
        self.inner.get(key).await
    }

    async fn delete(
        &self,
        key: &str,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Delegate to the inner KeyValueStore, which already implements delete.
        self.inner.delete(key).await
    }

    fn all(&self) -> std::collections::HashMap<String, Vec<u8>> {
        self.inner.all()
    }

    async fn share_ticket(&self) -> std::result::Result<String, Self::Error> {
        self.inner.share_ticket().await
    }
}
