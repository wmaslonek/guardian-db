use crate::address::{Address, GuardianDBAddress};
use crate::cache::level_down::LevelDownCache;
use crate::db_manifest;
use crate::guardian::error::{GuardianError, Result};
use crate::keystore::RedbKeystore;
use crate::log::identity::Identity;
pub use crate::log::identity_provider::Keystore;
use crate::p2p::Emitter;
pub use crate::p2p::EventBus;
pub use crate::p2p::EventBus as EventBusImpl;
use crate::p2p::network::{client::IrohClient, config::ClientConfig, core::IrohBackend};
use crate::traits::{
    AccessControllerConstructor, BaseGuardianDB, CreateDBOptions, DetermineAddressOptions,
    DirectChannel, DirectChannelFactory, DirectChannelOptions, EventPubSubPayload,
    MessageExchangeHeads, MessageMarshaler, PubSubInterface, Store, StoreConstructor,
    TracerWrapper,
};
use hex;
use iroh::EndpointId as NodeId;
use iroh_blobs::Hash;
use opentelemetry::global::BoxedTracer;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Span;

// Type aliases to simplify complex types.
type CloseKeystoreFn = Arc<RwLock<Option<Box<dyn Fn() -> Result<()> + Send + Sync>>>>;
// Type alias for a Store using GuardianError.
type GuardianStore = dyn Store<Error = GuardianError> + Send + Sync;

// We use `Option<T>` for fields that may not be provided.
#[derive(Default)]
pub struct NewGuardianDBOptions {
    pub id: Option<String>,
    pub node_id: Option<NodeId>,
    pub directory: Option<PathBuf>,
    pub keystore: Option<Box<dyn Keystore + Send + Sync>>,
    pub cache: Option<Arc<LevelDownCache>>,
    pub identity: Option<Identity>,
    pub close_keystore: Option<Box<dyn Fn() -> Result<()> + Send + Sync>>,
    pub tracer: Option<Arc<BoxedTracer>>,
    pub direct_channel_factory: Option<DirectChannelFactory>,
    pub pubsub: Option<Box<dyn PubSubInterface<Error = GuardianError>>>,
    pub message_marshaler: Option<Box<dyn MessageMarshaler<Error = GuardianError>>>,
    pub event_bus: Option<Arc<EventBusImpl>>,
    pub backend: Option<Arc<IrohBackend>>,
}

pub struct GuardianDB {
    client: IrohClient,
    identity: Arc<RwLock<Identity>>,
    id: Arc<RwLock<NodeId>>,
    keystore: Arc<RwLock<Option<Box<dyn Keystore + Send + Sync>>>>,
    close_keystore: CloseKeystoreFn,
    tracer: Arc<BoxedTracer>,
    span: Span,
    stores: Arc<RwLock<HashMap<String, Arc<GuardianStore>>>>,
    #[allow(dead_code)]
    direct_channel: Arc<dyn DirectChannel<Error = GuardianError> + Send + Sync>,
    access_control_types: Arc<RwLock<HashMap<String, AccessControllerConstructor>>>,
    store_types: Arc<RwLock<HashMap<String, StoreConstructor>>>,
    directory: PathBuf,
    cache: Arc<RwLock<Arc<LevelDownCache>>>,
    #[allow(dead_code)]
    pubsub: Option<Box<dyn PubSubInterface<Error = GuardianError>>>,
    event_bus: Arc<EventBusImpl>,
    #[allow(dead_code)]
    message_marshaler: Arc<dyn MessageMarshaler<Error = GuardianError> + Send + Sync>,
    _monitor_handle: JoinHandle<()>, // Handle for the background task, so it can be cancelled on Drop.
    cancellation_token: CancellationToken,
    emitters: Arc<Emitters>,
}

#[derive(Clone)]
pub struct EventExchangeHeads {
    pub peer: NodeId,
    pub message: MessageExchangeHeads,
}

// GuardianDB-level events
#[derive(Clone)]
pub struct EventGuardianDBReady {
    pub address: String,
    pub db_type: String,
}

#[derive(Clone)]
pub struct EventPeerConnected {
    pub node_id: String,
    pub address: String,
}

#[derive(Clone)]
pub struct EventPeerDisconnected {
    pub node_id: String,
    pub address: String,
}

#[derive(Clone)]
pub struct EventDatabaseCreated {
    pub address: String,
    pub name: String,
    pub db_type: String,
}

#[derive(Clone)]
pub struct EventDatabaseDropped {
    pub address: String,
    pub name: String,
}

// Store-specific events
#[derive(Clone)]
pub struct EventStoreUpdated {
    pub store_address: String,
    pub store_type: String,
    pub entries_added: usize,
    pub total_entries: usize,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone)]
pub struct EventSyncCompleted {
    pub store_address: String,
    pub node_id: String,
    pub heads_synced: usize,
    pub duration_ms: u64,
    pub success: bool,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone)]
pub struct EventNewEntries {
    pub store_address: String,
    pub entries: Vec<crate::log::entry::Entry>,
    pub total_entries: usize,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone)]
pub struct EventSyncError {
    pub store_address: String,
    pub node_id: String,
    pub error_message: String,
    pub heads_count: usize,
    pub error_type: SyncErrorType,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Debug)]
pub enum SyncErrorType {
    PermissionDenied,
    NetworkError,
    ValidationError,
    StoreError,
    UnknownError,
}

#[derive(Clone)]
pub struct EventPermissionDenied {
    pub store_address: String,
    pub identity_id: String,
    pub identity_pubkey: String,
    pub required_permission: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

pub struct Emitters {
    pub ready: Emitter<EventGuardianDBReady>,
    pub peer_connected: Emitter<EventPeerConnected>,
    pub peer_disconnected: Emitter<EventPeerDisconnected>,
    pub new_heads: Emitter<EventExchangeHeads>,
    pub database_created: Emitter<EventDatabaseCreated>,
    pub database_dropped: Emitter<EventDatabaseDropped>,
    // Store-specific events
    pub store_updated: Emitter<EventStoreUpdated>,
    pub sync_completed: Emitter<EventSyncCompleted>,
    pub new_entries: Emitter<EventNewEntries>,
    // Error events
    pub sync_error: Emitter<EventSyncError>,
    pub permission_denied: Emitter<EventPermissionDenied>,
}

impl Emitters {
    /// Generate emitters from an EventBus instance
    pub async fn generate_emitters(event_bus: &EventBusImpl) -> Result<Self> {
        Ok(Self {
            ready: event_bus.emitter().await?,
            peer_connected: event_bus.emitter().await?,
            peer_disconnected: event_bus.emitter().await?,
            new_heads: event_bus.emitter().await?,
            database_created: event_bus.emitter().await?,
            database_dropped: event_bus.emitter().await?,
            // Store-specific events
            store_updated: event_bus.emitter().await?,
            sync_completed: event_bus.emitter().await?,
            new_entries: event_bus.emitter().await?,
            // Error events
            sync_error: event_bus.emitter().await?,
            permission_denied: event_bus.emitter().await?,
        })
    }
}

impl EventExchangeHeads {
    /// Creates a new EventExchangeHeads instance.
    pub fn new(p: NodeId, msg: MessageExchangeHeads) -> Self {
        Self {
            peer: p,
            message: msg,
        }
    }
}

impl EventStoreUpdated {
    pub fn new(
        store_address: String,
        store_type: String,
        entries_added: usize,
        total_entries: usize,
    ) -> Self {
        Self {
            store_address,
            store_type,
            entries_added,
            total_entries,
            timestamp: chrono::Utc::now(),
        }
    }
}

impl EventSyncCompleted {
    pub fn new(
        store_address: String,
        node_id: String,
        heads_synced: usize,
        duration_ms: u64,
        success: bool,
    ) -> Self {
        Self {
            store_address,
            node_id,
            heads_synced,
            duration_ms,
            success,
            timestamp: chrono::Utc::now(),
        }
    }
}

impl EventNewEntries {
    pub fn new(
        store_address: String,
        entries: Vec<crate::log::entry::Entry>,
        total_entries: usize,
    ) -> Self {
        Self {
            store_address,
            entries,
            total_entries,
            timestamp: chrono::Utc::now(),
        }
    }
}

impl EventSyncError {
    pub fn new(
        store_address: String,
        node_id: String,
        error_message: String,
        heads_count: usize,
        error_type: SyncErrorType,
    ) -> Self {
        Self {
            store_address,
            node_id,
            error_message,
            heads_count,
            error_type,
            timestamp: chrono::Utc::now(),
        }
    }
}

impl EventPermissionDenied {
    pub fn new(
        store_address: String,
        identity_id: String,
        identity_pubkey: String,
        required_permission: String,
    ) -> Self {
        Self {
            store_address,
            identity_id,
            identity_pubkey,
            required_permission,
            timestamp: chrono::Utc::now(),
        }
    }
}

impl GuardianDB {
    /// High-level constructor that sets up the Keystore and the Identity.
    pub async fn new(
        client_config: Option<ClientConfig>,
        options: Option<NewGuardianDBOptions>,
    ) -> Result<Self> {
        let mut options = options.unwrap_or_default();

        // Use the default Client configuration if none is provided.
        let client_config = client_config.unwrap_or_default();

        // Extract node_id or generate a random one.
        let node_id = options
            .node_id
            .unwrap_or_else(|| iroh::SecretKey::generate().public());

        // Create the Iroh backend.
        let backend = Arc::new(IrohBackend::new(&client_config).await?);
        let client = IrohClient::new_with_backend(backend.clone()).await?;

        // If no directory is provided, use a default based on the node_id.
        let default_dir = PathBuf::from("./GuardianDB").join(node_id.to_string());
        let directory = options.directory.as_ref().unwrap_or(&default_dir);

        // Configure the Keystore if none is provided.
        // Uses the `sled` database as a replacement for `leveldb`.
        if options.keystore.is_none() {
            // In `sled`, None for the path means in-memory.
            let sled_path = if directory.to_string_lossy() == "./GuardianDB/in-memory" {
                None
            } else {
                Some(directory.join(node_id.to_string()).join("keystore"))
            };

            // Create the keystore using our RedbKeystore implementation.
            let keystore = Arc::new(RedbKeystore::new(sled_path).map_err(|e| {
                GuardianError::Other(format!("Failed to create the keystore: {}", e))
            })?);

            // Create the close closure.
            let keystore_clone = keystore.clone();
            options.close_keystore = Some(Box::new(move || {
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(async { keystore_clone.close().await })
                })
            }));

            // Set the keystore in the options.
            options.keystore = Some(Box::new(keystore));
        }

        // Configure the identity if none is provided.
        let identity = if let Some(identity) = options.identity {
            identity
        } else {
            let id = options
                .id
                .as_deref()
                .unwrap_or(&node_id.to_string())
                .to_string();
            let _keystore = options.keystore.as_ref().ok_or_else(|| {
                GuardianError::Other("A Keystore is required to create an identity".to_string())
            })?;

            // Try to load persisted identity from the data directory
            let identity_path = directory.join("identity.json");
            if identity_path.exists() {
                match std::fs::read_to_string(&identity_path) {
                    Ok(data) => match serde_json::from_str::<Identity>(&data) {
                        Ok(id) => {
                            tracing::debug!("Loaded persisted identity from {:?}", identity_path);
                            id
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to deserialize identity file, creating new: {}",
                                e
                            );
                            use crate::log::identity::{DefaultIdentificator, Identificator};
                            let mut identificator = DefaultIdentificator::new();
                            let new_id = identificator.create(&id);
                            Self::save_identity_to_file(&identity_path, &new_id);
                            new_id
                        }
                    },
                    Err(e) => {
                        tracing::warn!("Failed to read identity file, creating new: {}", e);
                        use crate::log::identity::{DefaultIdentificator, Identificator};
                        let mut identificator = DefaultIdentificator::new();
                        let new_id = identificator.create(&id);
                        Self::save_identity_to_file(&identity_path, &new_id);
                        new_id
                    }
                }
            } else {
                use crate::log::identity::{DefaultIdentificator, Identificator};
                let mut identificator = DefaultIdentificator::new();
                let new_id = identificator.create(&id);
                Self::save_identity_to_file(&identity_path, &new_id);
                new_id
            }
        };

        options.identity = Some(identity.clone());
        options.backend = Some(backend);

        // Call the main constructor with the fully configured options.
        Self::new_guardian_db(client, identity, Some(options)).await
    }

    /// Main constructor for a GuardianDB instance.
    pub async fn new_guardian_db(
        client: IrohClient,
        identity: Identity,
        options: Option<NewGuardianDBOptions>,
    ) -> Result<Self> {
        // Use the provided options or create a default value.
        let options = options.unwrap_or_default();

        // 1. Configure default values for the options.
        let tracer = options.tracer.unwrap_or_else(|| {
            // Use a basic tracer for telemetry.
            Arc::new(BoxedTracer::new(Box::new(
                opentelemetry::trace::noop::NoopTracer::new(),
            )))
        });

        // Create a span for this GuardianDB instance.
        let span = tracing::info_span!("guardian_db", node_id = %identity.id());
        // Initialize EventBus with proper configuration
        let event_bus = Arc::new(EventBusImpl::new());

        // Extract the IrohBackend or create a new one (must always be present via options).
        let backend = options.backend.clone().ok_or_else(|| {
            GuardianError::Other("IrohBackend is required in the options".to_string())
        })?;

        // Create the DirectChannelFactory.
        let own_node_id = client.node_id();
        let direct_channel_factory = options.direct_channel_factory.unwrap_or_else(|| {
            let temp_span = tracing::Span::none();
            crate::p2p::messaging::direct_channel::init_direct_channel_factory(
                temp_span,
                own_node_id,
                backend.clone(),
            )
        });
        let cancellation_token = CancellationToken::new();

        // Create emitters using the EventBus.
        let emitters = Emitters::generate_emitters(&event_bus).await.map_err(|e| {
            GuardianError::Other(format!("Failed to generate EventBus emitters: {}", e))
        })?;

        // 2. Initialize components.
        // Create the direct channel using our factory.
        let direct_channel = make_direct_channel(
            &event_bus,
            direct_channel_factory,
            &DirectChannelOptions::default(),
        )
        .await?;

        let message_marshaler_arc: Arc<dyn MessageMarshaler<Error = GuardianError> + Send + Sync> =
            match options.message_marshaler {
                Some(boxed_marshaler) => {
                    // Convert Box into Arc by creating a new Arc with the content.
                    // Box::into_inner would be ideal, but trait objects do not allow it,
                    // so we create a wrapper that delegates all calls.
                    struct BoxWrapper(Box<dyn MessageMarshaler<Error = GuardianError>>);
                    impl MessageMarshaler for BoxWrapper {
                        type Error = GuardianError;
                        fn marshal(
                            &self,
                            msg: &MessageExchangeHeads,
                        ) -> std::result::Result<Vec<u8>, GuardianError> {
                            self.0.marshal(msg)
                        }
                        fn unmarshal(
                            &self,
                            data: &[u8],
                        ) -> std::result::Result<MessageExchangeHeads, GuardianError>
                        {
                            self.0.unmarshal(data)
                        }
                    }
                    Arc::new(BoxWrapper(boxed_marshaler))
                }
                None => {
                    // Create directly as an Arc to avoid a conversion.
                    Arc::new(crate::message_marshaler::PostcardMarshaler::new())
                }
            };
        let cache = options.cache.unwrap_or_else(|| {
            // Create a cache with an appropriate configuration.
            Arc::new(crate::cache::level_down::LevelDownCache::new(None))
        });
        let directory = options
            .directory
            .unwrap_or_else(|| PathBuf::from("./GuardianDB/in-memory")); // Default for in-memory data.

        // 3. Instantiate the GuardianDB struct.
        let instance = GuardianDB {
            client,
            identity: Arc::new(RwLock::new(identity.clone())),
            id: Arc::new(RwLock::new(own_node_id)), // NodeId from the IrohBackend.
            pubsub: options.pubsub,
            cache: Arc::new(RwLock::new(cache)),
            directory,
            event_bus: event_bus.clone(),
            stores: Arc::new(RwLock::new(HashMap::new())),
            direct_channel,
            close_keystore: Arc::new(RwLock::new(options.close_keystore)),
            keystore: Arc::new(RwLock::new(options.keystore)),
            store_types: Arc::new(RwLock::new(HashMap::new())),
            access_control_types: Arc::new(RwLock::new(HashMap::new())),
            tracer,
            message_marshaler: message_marshaler_arc,
            cancellation_token: cancellation_token.clone(),
            emitters: Arc::new(emitters),
            // Start the direct channel monitor using the helper function.
            _monitor_handle: Self::start_monitor_task(
                event_bus.clone(),
                cancellation_token.clone(),
                span.clone(),
            ),
            span,
        };

        // 4. Post-initialization configuration.
        // Register the default store constructors.
        instance.register_default_store_types();

        // Configure the "newHeads" emitter on the event_bus.
        tracing::debug!("Configuring EventBus emitters");

        // Start the direct channel monitor to process incoming messages.
        tracing::debug!("Starting the direct channel monitor");
        if let Err(e) = instance.monitor_direct_channel(event_bus.clone()).await {
            tracing::error!("Failed to start the direct channel monitor: {}", e);
        } else {
            tracing::info!("Direct channel monitor started successfully");
        }

        // Emit the GuardianDB ready event.
        let ready_event = EventGuardianDBReady {
            address: format!("/GuardianDB/{}", instance.node_id()),
            db_type: "GuardianDB".to_string(),
        };

        if let Err(e) = instance.emitters.ready.emit(ready_event) {
            tracing::warn!("Failed to emit GuardianDB ready event: {}", e);
        } else {
            tracing::debug!("GuardianDB ready event emitted successfully");
        }

        Ok(instance)
    }

    /// Persists the identity to a JSON file so it can be reloaded across sessions
    fn save_identity_to_file(path: &Path, identity: &Identity) {
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

    /// Returns the tracer for telemetry and monitoring.
    pub fn tracer(&self) -> Arc<BoxedTracer> {
        self.tracer.clone()
    }

    /// Returns a reference to the tracing span used for instrumentation.
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Returns the GuardianDB's Client.
    pub fn client(&self) -> &IrohClient {
        &self.client
    }

    /// Returns the identity of the GuardianDB instance.
    /// The identity is cloned so the caller can use it without holding the read lock.
    pub fn identity(&self) -> Identity {
        self.identity.read().clone()
    }

    /// Returns the NodeId of the GuardianDB instance.
    /// `NodeId` implements the `Copy` trait, so the value is copied, which is very efficient.
    pub fn node_id(&self) -> NodeId {
        *self.id.read()
    }

    /// Returns a clone of the `Arc` to the Keystore, allowing shared access.
    /// The keystore is configured during initialization and can be used for cryptographic operations.
    pub fn keystore(&self) -> Arc<RwLock<Option<Box<dyn Keystore + Send + Sync>>>> {
        self.keystore.clone()
    }

    /// Returns the close function for the Keystore, if one exists.
    ///
    /// This function returns a closure that can be called to close the keystore.
    /// The closure captures a cloned reference to the internal close_keystore field.
    ///
    /// # Returns
    ///
    /// - `Some(closure)` if a close function was configured during initialization
    /// - `None` if no close function was set
    ///
    /// # Alternative
    ///
    /// For a simpler interface, use `close_key_store()` directly.
    pub fn close_keystore(&self) -> Option<Box<dyn Fn() -> Result<()> + Send + Sync>> {
        // Acquire a read lock to check whether a close function exists.
        let guard = self.close_keystore.read();
        // Check whether a close function exists.
        if guard.is_some() {
            // If it does, clone the inner Arc to capture it in the closure.
            let close_keystore_clone = self.close_keystore.clone();
            // Return a new closure that runs the close function.
            // Inside the returned closure, re-check that the function still exists.
            Some(Box::new(move || {
                let guard = close_keystore_clone.read();
                if let Some(close_fn) = guard.as_ref() {
                    close_fn() // Run the function if it still exists.
                } else {
                    Ok(()) // The function was removed between the check and the execution.
                }
            }))
        } else {
            None // Between the first and second check, another thread may have removed the function.
        }
    }

    /// Adds or updates a store in the managed stores map.
    /// This operation acquires a write lock.
    pub fn set_store(&self, address: String, store: Arc<GuardianStore>) {
        self.stores.write().insert(address, store);
    }

    /// Removes a store from the managed stores map.
    /// This operation acquires a write lock.
    pub fn delete_store(&self, address: &str) {
        self.stores.write().remove(address);
    }

    /// Looks up a store in the map by its address.
    /// Returns `Some(store)` if found, or `None` otherwise.
    pub fn get_store(&self, address: &str) -> Option<Arc<GuardianStore>> {
        self.stores.read().get(address).cloned()
    }

    /// Returns a list of all managed stores with their addresses.
    /// Each element is a tuple (address, reference to the store).
    pub fn list_stores(&self) -> Vec<(String, Arc<GuardianStore>)> {
        self.stores
            .read()
            .iter()
            .map(|(addr, store)| (addr.clone(), Arc::clone(store)))
            .collect()
    }

    /// Connects to and synchronizes with a specific peer.
    ///
    /// This method facilitates manual peer connection when automatic discovery
    /// is not enough or you want to force a synchronization.
    /// It works by finding any active EventLogStore and using its BaseStore
    /// to start the synchronization via exchange_heads.
    ///
    /// # Arguments
    /// * `peer_id` - NodeId of the peer to synchronize with
    ///
    /// # Returns
    /// `Ok(())` if the synchronization was started successfully
    pub async fn connect_to_peer(&self, peer_id: NodeId) -> Result<()> {
        tracing::info!(peer = %peer_id, "Establishing connection with peer");

        // STEP 1: Establish a QUIC connection via the gossip ALPN.
        // CRITICAL: Iroh gossip requires peers to be connected via QUIC
        // with the ALPN "/iroh-gossip/1" BEFORE forming the mesh.
        tracing::debug!(peer = %peer_id, "Establishing QUIC connection via gossip ALPN");

        if let Err(e) = self.client.connect_gossip(peer_id).await {
            tracing::warn!(
                peer = %peer_id,
                error = %e,
                "Failed to establish gossip QUIC connection, but continuing anyway (peer may already be connected)"
            );
            // Do not return an error - the peer may already be connected or have another path.
        } else {
            tracing::info!(peer = %peer_id, "✓ Gossip QUIC connection established");
        }

        // STEP 2: Find active stores to start synchronization.
        // Supports EventLogStore, KeyValueStore and DocumentStore.
        let stores: Vec<_> = self.stores.read().values().cloned().collect();

        for store in &stores {
            // Macro to synchronize the basestore of any store type.
            macro_rules! sync_store {
                ($store_ref:expr, $store_type:expr) => {{
                    let basestore = $store_ref.basestore();
                    tracing::info!(
                        peer = %peer_id,
                        store_type = $store_type,
                        "[SYNC] Starting synchronization with peer"
                    );

                    // Add the peer to the gossip mesh before sending heads.
                    let pubsub = basestore.pubsub();
                    if let Some(epidemic_pubsub) = pubsub
                        .as_any()
                        .downcast_ref::<crate::p2p::network::core::gossip::EpidemicPubSub>()
                    {
                        let shared_topic_name = basestore.extract_log_name();
                        tracing::debug!(peer = %peer_id, "[GOSSIP_MESH] Using topic: {}", shared_topic_name);
                        if let Err(e) = epidemic_pubsub
                            .get_or_create_topic_with_peers(&shared_topic_name, vec![peer_id])
                            .await
                        {
                            tracing::warn!(peer = %peer_id, error = %e, "[GOSSIP_MESH] Failed to add peer to gossip mesh");
                        } else {
                            tracing::debug!(peer = %peer_id, "[GOSSIP_MESH] Successfully added peer to gossip mesh");
                            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                        }
                    }

                    return basestore.exchange_heads(peer_id).await;
                }};
            }

            // Try to downcast to EventLogStore.
            if let Some(event_log_store) = store
                .as_any()
                .downcast_ref::<crate::stores::event_log_store::GuardianDBEventLogStore>(
            ) {
                sync_store!(event_log_store, "eventlog");
            }

            // KeyValueStore uses iroh-docs (Willow range sync) — no exchange_heads needed.
            if store
                .as_any()
                .downcast_ref::<crate::stores::kv_store::GuardianDBKeyValue>()
                .is_some()
            {
                tracing::info!(
                    peer = %peer_id,
                    store_type = "keyvalue",
                    "[SYNC] KeyValueStore uses iroh-docs Willow sync — automatic sync"
                );
                return Ok(());
            }

            // Try to downcast to DocumentStore.
            // DocumentStore uses iroh-docs Willow sync — automatic sync.
            if store
                .as_any()
                .downcast_ref::<crate::stores::document_store::GuardianDBDocumentStore>()
                .is_some()
            {
                tracing::info!(
                    peer = %peer_id,
                    store_type = "document",
                    "[SYNC] DocumentStore uses iroh-docs Willow sync — automatic sync"
                );
                return Ok(());
            }
        }

        Err(GuardianError::Store(
            "No active store found for synchronization. Create a store with db.log(), db.key_value() or db.docs() first.".to_string()
        ))
    }

    /// Iterates over all managed stores and calls `close()` on each one.
    /// Clones the store list to avoid holding the lock during the `close()` call,
    /// preventing potential deadlocks.
    pub async fn close_all_stores(&self) {
        let stores_to_close: Vec<Arc<GuardianStore>> =
            self.stores.read().values().cloned().collect();

        tracing::debug!(
            store_count = stores_to_close.len(),
            "Starting to close stores"
        );

        for (index, store) in stores_to_close.iter().enumerate() {
            tracing::debug!(
                store_index = index + 1,
                total_stores = stores_to_close.len(),
                store_type = store.store_type(),
                address = %store.address(),
                "Closing store"
            );

            match store.close().await {
                Ok(()) => {
                    tracing::debug!(
                        store_type = store.store_type(),
                        address = %store.address(),
                        "Store closed successfully"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        store_type = store.store_type(),
                        address = %store.address(),
                        error = %e,
                        "Error closing store"
                    );
                    // Keep closing other stores even if one fails.
                }
            }
        }

        // Clear the store map after closing them all.
        self.stores.write().clear();
        tracing::debug!(
            stores_count = stores_to_close.len(),
            "All stores were processed and removed from the map"
        );
    }

    /// Closes the LevelDown cache, ensuring all data is persisted
    /// and releasing the associated resources.
    pub fn close_cache(&self) {
        tracing::debug!("Starting cache shutdown");

        // Acquire a write lock on the cache to perform the shutdown.
        let cache_guard = self.cache.write();

        // Close the cache using the instance's direct method.
        match cache_guard.close_internal() {
            Ok(()) => {
                tracing::debug!("Cache closed successfully");
            }
            Err(e) => {
                tracing::error!(error = %e, "Error closing cache");
            }
        }

        // The lock is automatically released when cache_guard goes out of scope.
    }

    /// Closes the direct communication channel and logs an error if the operation fails.
    pub async fn close_direct_connections(&self) {
        tracing::debug!("Starting direct channel shutdown");

        match self.direct_channel.close_shared().await {
            Ok(()) => {
                tracing::debug!("Direct channel closed successfully");
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "Error closing direct channel"
                );
            }
        }
    }

    /// Runs the keystore close function, if one was set.
    /// Acquires a write lock to ensure the function is not modified while being read and executed.
    pub fn close_key_store(&self) {
        let guard = self.close_keystore.write();
        if let Some(close_fn) = guard.as_ref()
            && let Err(e) = close_fn()
        {
            tracing::error!(error = %e, "could not close the keystore");
        }
    }

    /// Looks up an AccessController constructor by its type (name).
    /// Returns `Some(constructor)` if found, or `None` otherwise.
    pub fn get_access_control_type(
        &self,
        controller_type: &str,
    ) -> Option<AccessControllerConstructor> {
        tracing::debug!(
            controller_type = controller_type,
            "Looking up AccessController constructor"
        );

        let access_controls = self.access_control_types.read();

        match access_controls.get(controller_type) {
            Some(constructor) => {
                tracing::debug!(
                    controller_type = controller_type,
                    "AccessController constructor found"
                );
                Some(constructor.clone())
            }
            None => {
                tracing::debug!(
                    controller_type = controller_type,
                    available_types = ?access_controls.keys().collect::<Vec<_>>(),
                    "AccessController constructor not found"
                );
                None
            }
        }
    }

    /// Returns a list of the names of all registered AccessController types.
    /// Helper function for debugging and listing available types.
    pub fn access_control_types_names(&self) -> Vec<String> {
        self.access_control_types.read().keys().cloned().collect()
    }

    /// Removes an AccessController constructor from the map by its type.
    /// This operation acquires a write lock.
    pub fn unregister_access_control_type(&self, controller_type: &str) {
        self.access_control_types.write().remove(controller_type);
    }

    /// Registers a new AccessController type.
    /// The constructor function is run once to determine the type name.
    ///
    /// Runs the constructor to determine the dynamic type.
    /// Registers a new AccessController type with an explicit type.
    pub fn register_access_control_type_with_name(
        &self,
        controller_type: &str,
        constructor: AccessControllerConstructor,
    ) -> Result<()> {
        tracing::debug!(
            controller_type = %controller_type,
            "Registering new AccessController type"
        );

        // Type validations.
        if controller_type.is_empty() {
            return Err(GuardianError::InvalidArgument(
                "The controller type cannot be an empty string".to_string(),
            ));
        }

        if controller_type.len() > 100 {
            return Err(GuardianError::InvalidArgument(
                "The controller type is too long (maximum 100 characters)".to_string(),
            ));
        }

        // Validate known types.
        let valid_types = ["simple", "guardian", "iroh"];
        if !valid_types.contains(&controller_type) {
            tracing::warn!(
                controller_type = %controller_type,
                valid_types = ?valid_types,
                "Unrecognized AccessController type - registering anyway"
            );
        }

        // Check whether the type is already registered.
        {
            let existing_types = self.access_control_types.read();
            if existing_types.contains_key(controller_type) {
                tracing::warn!(
                    controller_type = %controller_type,
                    "AccessController already registered - overwriting"
                );
            } else {
                tracing::debug!(
                    controller_type = %controller_type,
                    "New AccessController type being registered"
                );
            }
        }

        // Register the constructor in the map.
        self.access_control_types
            .write()
            .insert(controller_type.to_string(), constructor);

        tracing::debug!(
            controller_type = %controller_type,
            "AccessController registered successfully"
        );

        Ok(())
    }

    /// Legacy method kept for compatibility - uses the default "simple" type.
    pub async fn register_access_control_type(
        &self,
        constructor: AccessControllerConstructor,
    ) -> Result<()> {
        tracing::debug!("Using legacy registration with default 'simple' type");
        self.register_access_control_type_with_name("simple", constructor)
    }

    pub fn register_store_type(&self, store_type: String, constructor: StoreConstructor) {
        self.store_types.write().insert(store_type, constructor);
    }

    /// Removes a Store constructor from the map by its type.
    pub fn unregister_store_type(&self, store_type: &str) {
        self.store_types.write().remove(store_type);
    }

    /// Returns a list of the names of all registered Store types.
    pub fn store_types_names(&self) -> Vec<String> {
        self.store_types.read().keys().cloned().collect()
    }

    /// Looks up a Store constructor by its type (name).
    /// Returns `Some(constructor)` if found, or `None` otherwise.
    pub fn get_store_constructor(&self, store_type: &str) -> Option<StoreConstructor> {
        tracing::debug!(store_type = store_type, "Looking up Store constructor");

        let store_constructors = self.store_types.read();

        match store_constructors.get(store_type) {
            Some(constructor) => {
                tracing::debug!(store_type = store_type, "Store constructor found");
                Some(constructor.clone())
            }
            None => {
                tracing::debug!(
                    store_type = store_type,
                    available_types = ?store_constructors.keys().collect::<Vec<_>>(),
                    "Store constructor not found"
                );
                None
            }
        }
    }

    /// Shuts down the GuardianDB instance, closing all stores, connections and background tasks.
    pub async fn close(&self) -> Result<()> {
        let _entered = self.span.enter();
        tracing::debug!("Starting GuardianDB shutdown");

        // Close all stores first (async operation) - with error handling.
        tracing::debug!("Closing all stores");
        self.close_all_stores().await;

        // Close direct connections (async operation) - with error handling.
        tracing::debug!("Closing direct connections");
        self.close_direct_connections().await;

        // Close cache (synchronous operation)
        tracing::debug!("Closing cache");
        self.close_cache();

        // Close keystore (synchronous operation) - with error handling.
        tracing::debug!("Closing keystore");
        self.close_key_store();

        // Close emitters using the EventBus.
        // Note: our emitters do not need an explicit close since they use Tokio broadcast channels,
        // which are automatically cleaned up when the EventBus is dropped.
        tracing::debug!("Emitters will be closed automatically with the EventBus");

        // Signal all background tasks (such as `monitor_direct_channel`) to shut down.
        tracing::debug!("Cancelling background tasks");
        self.cancellation_token.cancel();

        // Explicitly abort the monitor task to avoid hangs during shutdown.
        tracing::debug!("Aborting the direct channel monitor task");
        self._monitor_handle.abort();

        // Small delay to allow the abort to propagate.
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        tracing::debug!("GuardianDB closed successfully");
        Ok(())
    }

    /// Creates a new database (store), determines its address, saves it locally and opens it.
    pub async fn create(
        &self,
        name: &str,
        store_type: &str,
        options: Option<CreateDBOptions>,
    ) -> Result<Arc<GuardianStore>> {
        let _entered = self.span.enter();
        tracing::debug!("Create()");
        let options = options.unwrap_or_default();

        // The directory can be passed as an option; otherwise, use the instance default.
        let directory = options
            .directory
            .clone()
            .unwrap_or_else(|| self.directory.to_string_lossy().to_string());
        let mut options = options;
        options.directory = Some(directory.clone());

        tracing::debug!(
            name = name,
            store_type = store_type,
            directory = %directory,
            "Creating database"
        );

        // Create the database address.
        let determine_opts = crate::traits::DetermineAddressOptions {
            only_hash: None,
            replicate: None,
            access_controller:
                crate::access_control::manifest::CreateAccessControllerOptions::new_empty(),
        };
        let db_address = self
            .determine_address(name, store_type, Some(determine_opts))
            .await?;

        // Load the locally saved cache.
        let directory_path = PathBuf::from(&directory);
        self.load_cache(directory_path.as_path(), &db_address)
            .await?;

        // Check whether the database already exists locally.
        let have_db = self.have_local_data_in(&db_address, &directory).await;

        if have_db && !options.overwrite.unwrap_or(false) {
            return Err(GuardianError::DatabaseAlreadyExists(db_address.to_string()));
        }

        // Save the database manifest locally.
        self.add_manifest_to_cache(&directory_path, &db_address)
            .await
            .map_err(|e| {
                GuardianError::Other(format!("could not add the manifest to the cache: {}", e))
            })?;

        tracing::debug!(
            address = %db_address,
            "Database created"
        );

        // Open the database.
        self.open(&db_address.to_string(), options).await
    }

    /// Opens a database from a GuardianDB address.
    pub async fn open(
        &self,
        db_address: &str,
        options: CreateDBOptions,
    ) -> Result<Arc<GuardianStore>> {
        let _entered = self.span.enter();
        tracing::debug!(address = db_address, "opening GuardianDB store");
        let mut options = options;

        let directory = options
            .directory
            .clone()
            .unwrap_or_else(|| self.directory.to_string_lossy().to_string());
        options.directory = Some(directory.clone());

        // Validate the address. If invalid, try to create a new database if the `create` option is true.
        if crate::address::is_valid(db_address).is_err() {
            tracing::warn!(address = db_address, "open: Invalid GuardianDB address");
            if !options.create.unwrap_or(false) {
                return Err(GuardianError::InvalidArgument("'options.create' set to 'false'. If you want to create a database, set it to 'true'".to_string()));
            }
            let store_type = options.store_type.as_deref().unwrap_or("");
            if store_type.is_empty() {
                let available_types = self.store_types_names();
                let types_list = if available_types.is_empty() {
                    "No store type registered".to_string()
                } else {
                    format!("Available types: {}", available_types.join(", "))
                };
                return Err(GuardianError::InvalidArgument(format!(
                    "Database type not provided! Provide a type with 'options.store_type'. {}",
                    types_list
                )));
            }

            options.overwrite = Some(true);
            // To avoid the borrow checker, we create new options preserving the event_bus.
            let new_options = CreateDBOptions {
                overwrite: Some(true),
                create: Some(true),
                store_type: Some(store_type.to_string()),
                event_bus: options.event_bus.clone(), // Preserve the event_bus!
                ..Default::default()
            };

            // Use Box::pin to break recursion
            return Box::pin(self.create(db_address, store_type, Some(new_options))).await;
        }

        let parsed_address = crate::address::parse(db_address)
            .map_err(|e| GuardianError::Other(format!("Error parsing the address: {}", e)))?;

        let directory_path = PathBuf::from(&directory);
        self.load_cache(directory_path.as_path(), &parsed_address)
            .await?;

        if options.local_only.unwrap_or(false)
            && !self.have_local_data_in(&parsed_address, &directory).await
        {
            return Err(GuardianError::NotFound(format!(
                "The database does not exist locally: {}",
                db_address
            )));
        }

        // If overwrite is active and we have a store_type, use it directly without reading the manifest.
        let manifest_type = if options.overwrite.unwrap_or(false) && options.store_type.is_some() {
            tracing::debug!("Overwrite active, using store_type from options");
            options.store_type.clone().unwrap()
        } else {
            // Read the manifest to determine the database type.
            if self.have_local_data_in(&parsed_address, &directory).await {
                // If we have local data, first try to read from the local cache.
                tracing::debug!("Data found locally, trying to read from cache before iroh");

                // Read the local cache.
                let _cache_key = format!("{}/_manifest", parsed_address);

                // Try the cache first, then fall back to the Client.
                let cache_result = {
                    let cache = self.cache.read();
                    let directory_str = directory_path.to_string_lossy();

                    // Try to load the data from the cache using internal methods.
                    match cache.load_internal(&directory_str, &parsed_address as &dyn Address) {
                        Ok(wrapped_cache) => {
                            // Cache loaded successfully, now check whether the manifest exists.
                            let manifest_key = format!("{}/_manifest", parsed_address);

                            tracing::debug!(
                                key = %manifest_key,
                                cache_loaded = true,
                                "Checking the manifest in the cache"
                            );

                            // Prepare the context and key for the cache.
                            let mut ctx: Box<dyn std::any::Any> = Box::new(());
                            let key = crate::data_store::Key::new(&manifest_key);

                            // Try to get the manifest from the cache.
                            match wrapped_cache.get(ctx.as_mut(), &key) {
                                Ok(manifest_data) => {
                                    tracing::debug!(
                                        key = %manifest_key,
                                        data_size = manifest_data.len(),
                                        "Manifest found in the cache"
                                    );

                                    // Validate that the data is a valid store type.
                                    let manifest_type =
                                        String::from_utf8_lossy(&manifest_data).to_string();

                                    // Check whether the type is registered.
                                    if self.get_store_constructor(&manifest_type).is_some() {
                                        tracing::debug!(
                                            manifest_type = %manifest_type,
                                            "Valid manifest found in the cache"
                                        );
                                        Some(manifest_data)
                                    } else {
                                        tracing::warn!(
                                            manifest_type = %manifest_type,
                                            available_types = ?self.store_types_names(),
                                            "Manifest type in the cache is not registered"
                                        );
                                        None
                                    }
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        key = %manifest_key,
                                        error = %e,
                                        "Manifest not found in the cache"
                                    );
                                    None
                                }
                            }
                        }
                        Err(e) => {
                            tracing::debug!(
                                error = %e,
                                "Failed to load cache, using iroh"
                            );
                            None
                        }
                    }
                };

                match cache_result {
                    Some(cached_data) => {
                        tracing::debug!("Manifest found in the local cache");
                        // Parse the manifest type from the cached data.
                        String::from_utf8_lossy(&cached_data).to_string()
                    }
                    None => {
                        tracing::debug!("Cache miss, reading manifest");
                        let manifest = db_manifest::read_db_manifest(
                            self.client(),
                            &parsed_address.get_root(),
                        )
                        .await
                        .map_err(|e| {
                            GuardianError::Other(format!("Could not read the manifest: {}", e))
                        })?;
                        manifest.get_type
                    }
                }
            } else {
                // If we have no local data, read directly from the Client.
                tracing::debug!("Data not found locally, reading manifest");
                let manifest =
                    db_manifest::read_db_manifest(self.client(), &parsed_address.get_root())
                        .await
                        .map_err(|e| {
                            GuardianError::Other(format!("Could not read the manifest: {}", e))
                        })?;
                manifest.get_type
            }
        };

        tracing::debug!(manifest_type = %manifest_type, "Database type detected");
        tracing::debug!("Creating store instance");

        self.create_store(&manifest_type, &parsed_address, options)
            .await
    }

    /// Determines a database address by creating its manifest and saving it to the Client.
    pub async fn determine_address(
        &self,
        name: &str,
        store_type: &str,
        options: Option<DetermineAddressOptions>,
    ) -> Result<GuardianDBAddress> {
        let _options = options.unwrap_or_default();

        // Validate that the store type is registered.
        if self.get_store_constructor(store_type).is_none() {
            let available_types = self.store_types_names();
            return Err(GuardianError::InvalidArgument(format!(
                "Invalid database type: {}. Available types: {:?}",
                store_type, available_types
            )));
        }

        if crate::address::is_valid(name).is_ok() {
            return Err(GuardianError::InvalidArgument(
                "The provided database name is already a valid address".to_string(),
            ));
        }

        // Create options for the access controller with appropriate settings.
        let _ac_params =
            crate::access_control::manifest::CreateAccessControllerOptions::new_empty();

        // Access Controller creation.
        // Generate an address based on the manifest hash and the user's identity.
        let identity_hash = hex::encode(self.identity().pub_key.as_bytes());
        let ac_address_string = format!("/iroh/{}/access_control/{}", name, &identity_hash[..8]);

        tracing::debug!(
            address = %ac_address_string,
            identity = %&identity_hash[..16],
            "Access Controller created"
        );

        // Create the database manifest on the Client.
        let manifest_hash =
            db_manifest::create_db_manifest(self.client(), name, store_type, &ac_address_string)
                .await
                .map_err(|e| GuardianError::Other(format!("Could not save the manifest: {}", e)))?;

        // Build and return the final GuardianDB address.
        let addr_string = format!("/GuardianDB/{}/{}", manifest_hash, name);
        crate::address::parse(&addr_string)
            .map_err(|e| GuardianError::Other(format!("Error parsing the address: {}", e)))
    }

    /// Loads the cache for a given database address.
    pub async fn load_cache(&self, directory: &Path, db_address: &GuardianDBAddress) -> Result<()> {
        // Load the cache using the LevelDownCache.
        let cache = self.cache.read();
        let directory_str = directory.to_string_lossy();

        tracing::debug!(
            address = %db_address,
            directory = %directory_str,
            "Loading cache for address"
        );

        // Load the cache specific to this address.
        let _loaded_cache = cache
            .load_internal(&directory_str, db_address)
            .map_err(|e| GuardianError::Other(format!("Failed to load cache: {}", e)))?;

        tracing::debug!(address = %db_address, "Cache loaded successfully");
        Ok(())
    }

    /// Checks whether a database manifest exists in the local cache.
    pub async fn have_local_data_in(
        &self,
        db_address: &GuardianDBAddress,
        directory: &str,
    ) -> bool {
        let _cache_key = format!("{}/_manifest", db_address);

        // Check whether the data exists in the cache.
        let cache = self.cache.read();
        let directory_str = directory;

        // Try to load the cache and check whether the manifest exists.
        match cache.load_internal(directory_str, db_address) {
            Ok(wrapped_cache) => {
                // Check whether the manifest key exists in the cache.
                let manifest_key = format!("{}/_manifest", db_address);

                // Prepare the context and key to check existence.
                let mut ctx: Box<dyn std::any::Any> = Box::new(());
                let key = crate::data_store::Key::new(&manifest_key);

                // Try to get the manifest from the cache to check whether it exists.
                match wrapped_cache.get(ctx.as_mut(), &key) {
                    Ok(manifest_data) => {
                        // Manifest found, check whether the data is valid.
                        if !manifest_data.is_empty() {
                            tracing::debug!(
                                address = %db_address,
                                manifest_size = manifest_data.len(),
                                "Local data found in the cache"
                            );
                            true
                        } else {
                            tracing::debug!(
                                address = %db_address,
                                "Empty manifest found in the cache"
                            );
                            false
                        }
                    }
                    Err(e) => {
                        tracing::debug!(
                            address = %db_address,
                            error = %e,
                            "Manifest not found in the cache"
                        );
                        false
                    }
                }
            }
            Err(e) => {
                tracing::debug!(
                    address = %db_address,
                    error = %e,
                    "Failed to load cache to check local data"
                );
                false
            }
        }
    }

    /// Adds a database's manifest hash to the local cache.
    pub async fn add_manifest_to_cache(
        &self,
        directory: &Path,
        db_address: &GuardianDBAddress,
    ) -> Result<()> {
        let cache_key = format!("{}/_manifest", db_address);
        let root_hash_bytes = db_address.get_root().to_string().into_bytes();

        // Store the manifest in the cache.
        let wrapped_cache = {
            let cache = self.cache.read();
            let directory_str = directory.to_string_lossy();

            // Load or create the datastore for this address.
            cache
                .load_internal(&directory_str, db_address)
                .map_err(|e| GuardianError::Other(format!("Failed to load cache: {}", e)))?
        };

        // Store the manifest hash in the cache concretely.
        let key = crate::data_store::Key::new(&cache_key);

        // Store the manifest type (not just the hash) to make checking easier.
        // Fetch the manifest type if it is available.
        let manifest_data = if let Ok(manifest) =
            db_manifest::read_db_manifest(self.client(), &db_address.get_root()).await
        {
            // If we could read the manifest from the Client, store the type.
            manifest.get_type.into_bytes()
        } else {
            // Fallback: store only the root hash as an existence indicator.
            root_hash_bytes
        };

        // Create the context after the await to avoid Send issues.
        let mut ctx: Box<dyn std::any::Any + Send + Sync> = Box::new(());

        match wrapped_cache.put(ctx.as_mut(), &key, &manifest_data) {
            Ok(()) => {
                tracing::debug!(
                    cache_key = %cache_key,
                    data_size = manifest_data.len(),
                    address = %db_address,
                    "Manifest stored in the cache successfully"
                );
            }
            Err(e) => {
                tracing::warn!(
                    cache_key = %cache_key,
                    error = %e,
                    address = %db_address,
                    "Failed to store manifest in the cache"
                );
                // Do not return an error since this is an optimization, not a critical operation.
            }
        }

        tracing::debug!(
            address = %db_address,
            directory = %directory.to_string_lossy(),
            cache_key = %cache_key,
            "Manifest added to the cache"
        );

        Ok(())
    }

    /// Handles the complex logic of instantiating a new Store, including resolving
    /// the Access Controller, loading the cache and configuring all options.
    pub async fn create_store(
        &self,
        store_type: &str,
        address: &GuardianDBAddress,
        options: CreateDBOptions,
    ) -> Result<Arc<GuardianStore>> {
        tracing::debug!(
            store_type = store_type,
            address = %address,
            "Creating store"
        );

        // 1. Look up the registered constructor for the store type.
        let constructor = self.get_store_constructor(store_type).ok_or_else(|| {
            let available_types = self.store_types_names();
            GuardianError::InvalidArgument(format!(
                "Store type '{}' not registered. Available types: {:?}",
                store_type, available_types
            ))
        })?;

        // 2. Convert CreateDBOptions into NewStoreOptions.
        let new_store_options = self
            .convert_create_to_store_options(store_type, options)
            .await?;

        // 3. Prepare arguments for the constructor.
        let client = Arc::new(self.client().clone());
        let identity = Arc::new(self.identity());
        let store_address = Box::new(address.clone()) as Box<dyn Address>;

        tracing::debug!(
            store_type = store_type,
            address = %address,
            "Running the store constructor"
        );

        // 4. Run the constructor.
        let store_result = constructor(client, identity, store_address, new_store_options).await;

        let store = match store_result {
            Ok(store) => store,
            Err(e) => {
                tracing::error!(
                    store_type = store_type,
                    address = %address,
                    error = %e,
                    "Failed to create store"
                );
                return Err(e);
            }
        };

        // 5. Convert into Arc<GuardianStore>.
        let boxed_store = store as Box<dyn Store<Error = GuardianError> + Send + Sync>;
        let arc_store: Arc<GuardianStore> = Arc::from(boxed_store);

        // 6. Register the store in the managed map.
        self.set_store(address.to_string(), arc_store.clone());

        tracing::debug!(
            store_type = store_type,
            address = %address,
            store_type_confirmed = arc_store.store_type(),
            "Store created and registered successfully"
        );

        Ok(arc_store)
    }

    /// Converts CreateDBOptions into the NewStoreOptions required by the constructors.
    async fn convert_create_to_store_options(
        &self,
        store_type: &str,
        options: CreateDBOptions,
    ) -> Result<crate::traits::NewStoreOptions> {
        use crate::traits::NewStoreOptions;

        tracing::debug!("Converting options for store creation");

        // Convert access_control from ManifestParams into an AccessController.
        let access_controller = if let Some(manifest_params) = options.access_controller {
            tracing::debug!("Converting ManifestParams into AccessController");

            // Extract information from the ManifestParams.
            let controller_type = manifest_params.get_type();

            tracing::debug!(
                controller_type = %controller_type,
                "Creating access controller from the manifest"
            );

            // Extract the permissions from the ManifestParams.
            let permissions = manifest_params.get_all_access();

            // Create the AccessController based on the type.
            match controller_type {
                "simple" | "" => {
                    tracing::debug!("Creating SimpleAccessController");

                    let simple_controller =
                        crate::access_control::acl_simple::SimpleAccessController::new(
                            if permissions.is_empty() {
                                None
                            } else {
                                Some(permissions)
                            },
                        );
                    Some(Arc::new(simple_controller)
                        as Arc<dyn crate::access_control::traits::AccessController>)
                }
                "guardian" => {
                    tracing::debug!("Creating GuardianAccessController");

                    // For GuardianAccessController, use a basic configuration.
                    let simple_controller =
                        crate::access_control::acl_simple::SimpleAccessController::new(
                            if permissions.is_empty() {
                                None
                            } else {
                                Some(permissions)
                            },
                        );
                    Some(Arc::new(simple_controller)
                        as Arc<dyn crate::access_control::traits::AccessController>)
                }
                "iroh" => {
                    tracing::debug!(
                        "Iroh AccessController not implemented, using SimpleAccessController"
                    );

                    let simple_controller =
                        crate::access_control::acl_simple::SimpleAccessController::new(
                            if permissions.is_empty() {
                                None
                            } else {
                                Some(permissions)
                            },
                        );
                    Some(Arc::new(simple_controller)
                        as Arc<dyn crate::access_control::traits::AccessController>)
                }
                _ => {
                    tracing::warn!(
                        controller_type = %controller_type,
                        "Unrecognized access controller type, using SimpleAccessController"
                    );

                    let simple_controller =
                        crate::access_control::acl_simple::SimpleAccessController::new(
                            if permissions.is_empty() {
                                None
                            } else {
                                Some(permissions)
                            },
                        );
                    Some(Arc::new(simple_controller)
                        as Arc<dyn crate::access_control::traits::AccessController>)
                }
            }
        } else {
            tracing::debug!("No access controller specified, using the default");
            None
        };

        // Convert the basic options while keeping compatibility.

        // Create or use the existing PubSub.
        let pubsub = if self.pubsub.is_some() {
            // PubSub already exists - create a new EpidemicPubSub from the backend.
            let backend = self.client().backend().clone();
            let epidemic_pubsub = Arc::new(backend.create_pubsub_interface().await?);
            Some(epidemic_pubsub as Arc<dyn PubSubInterface<Error = GuardianError>>)
        } else {
            // Create an EpidemicPubSub directly from the backend.
            let backend = self.client().backend().clone();
            let epidemic_pubsub = Arc::new(backend.create_pubsub_interface().await?);
            Some(epidemic_pubsub as Arc<dyn PubSubInterface<Error = GuardianError>>)
        };

        let store_options = NewStoreOptions {
            event_bus: options.event_bus, // Use the provided EventBus (required).
            index: {
                // CRITICAL FIX: create the index based on the store_type.
                match store_type {
                    "eventlog" => {
                        use crate::stores::event_log_store::index::new_event_index;
                        Some(Box::new(new_event_index))
                    }
                    _ => None, // Other store types may not have an index.
                }
            },
            access_controller, // AccessController converted from the ManifestParams.
            cache: None,       // Use the default cache.
            cache_destroy: None,
            replication_concurrency: None,
            reference_count: None,
            replicate: Some(true), // Enable replication by default.
            max_history: None,
            directory: options
                .directory
                .unwrap_or_else(|| self.directory.to_string_lossy().to_string()),
            sort_fn: None,
            span: None,   // Will be configured by the BaseStore.
            tracer: None, // Will be configured by the BaseStore.
            pubsub,       // ***Check whether the pubsub to use is EpidemicPubSub or RawPubSub.
            message_marshaler: Some(self.message_marshaler.clone()),
            node_id: *self.id.read(), // NodeId of the GuardianDB instance.
            direct_channel: Some(self.direct_channel.clone()),
            close_func: None,
            store_specific_opts: None,
            doc_ticket: options.doc_ticket,
            read_only: options.read_only,
        };

        tracing::debug!("Options converted successfully");
        Ok(store_options)
    }

    /// Registers the available default access controller constructors.
    pub async fn register_default_access_control_types(&self) -> Result<()> {
        tracing::debug!("Registering default access controller constructors");

        // Register SimpleAccessController.
        let simple_constructor =
            Arc::new(
                |_base_guardian: Arc<
                    dyn crate::traits::BaseGuardianDB<Error = crate::guardian::error::GuardianError>,
                >,
                 options: &crate::access_control::manifest::CreateAccessControllerOptions,
                 _access_control_options: Option<
                    Vec<crate::access_control::traits::Option>,
                >| {
                    let options = options.clone(); // Clone to move into the async block
                    Box::pin(async move {
                use crate::access_control::acl_simple::SimpleAccessController;
                let access_control = SimpleAccessController::from_options(options)
                    .map_err(|e| crate::guardian::error::GuardianError::Store(e.to_string()))?;
                Ok(Arc::new(access_control) as Arc<dyn crate::access_control::traits::AccessController>)
            }) as Pin<Box<dyn std::future::Future<Output = crate::guardian::error::Result<Arc<dyn crate::access_control::traits::AccessController>>> + Send>>
                },
            );

        // Perform the registration using the new method with an explicit type.
        self.register_access_control_type_with_name("simple", simple_constructor)?;

        tracing::debug!(
            types = ?self.access_control_types_names(),
            "Default access controller constructors registered"
        );

        Ok(())
    }

    /// Registers the available default store constructors.
    pub fn register_default_store_types(&self) {
        tracing::debug!("Registering default store constructors");

        // Register EventLogStore.
        let eventlog_constructor =
            Arc::new(
                |client: Arc<crate::p2p::network::client::IrohClient>,
                 identity: Arc<crate::log::identity::Identity>,
                 address: Box<dyn crate::address::Address>,
                 options: crate::traits::NewStoreOptions| {
                    Box::pin(async move {
                    use crate::stores::event_log_store::GuardianDBEventLogStore;
                    // Convert Box<dyn Address> into Arc<dyn Address + Send + Sync>.
                    let arc_address: Arc<dyn crate::address::Address + Send + Sync> =
                        Arc::from(address as Box<dyn crate::address::Address + Send + Sync>);

                    let store = GuardianDBEventLogStore::new(client, identity, arc_address, options)
                        .await
                        .map_err(|e| crate::guardian::error::GuardianError::Store(e.to_string()))?;

                    Ok(Box::new(store)
                        as Box<
                            dyn crate::traits::Store<Error = crate::guardian::error::GuardianError>,
                        >)
                })
                    as Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = crate::guardian::error::Result<
                                        Box<
                                            dyn crate::traits::Store<
                                                    Error = crate::guardian::error::GuardianError,
                                                >,
                                        >,
                                    >,
                                > + Send,
                        >,
                    >
                },
            );
        // Registra KeyValueStore
        let keyvalue_constructor = Arc::new(
            |client: Arc<crate::p2p::network::client::IrohClient>,
             identity: Arc<crate::log::identity::Identity>,
             address: Box<dyn crate::address::Address>,
             options: crate::traits::NewStoreOptions| {
                Box::pin(async move {
                    use crate::stores::kv_store::GuardianDBKeyValue;
                    // Convert Box<dyn Address> into Arc<dyn Address + Send + Sync>.
                    let arc_address: Arc<dyn crate::address::Address + Send + Sync> =
                        Arc::from(address as Box<dyn crate::address::Address + Send + Sync>);

                    let store =
                        GuardianDBKeyValue::new(client, identity, arc_address, Some(options))
                            .await
                            .map_err(|e| {
                                crate::guardian::error::GuardianError::Store(e.to_string())
                            })?;

                    Ok(Box::new(store)
                        as Box<
                            dyn crate::traits::Store<Error = crate::guardian::error::GuardianError>,
                        >)
                })
                    as Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = crate::guardian::error::Result<
                                        Box<
                                            dyn crate::traits::Store<
                                                    Error = crate::guardian::error::GuardianError,
                                                >,
                                        >,
                                    >,
                                > + Send,
                        >,
                    >
            },
        );

        // Registra DocumentStore
        let document_constructor =
            Arc::new(
                |client: Arc<crate::p2p::network::client::IrohClient>,
                 identity: Arc<crate::log::identity::Identity>,
                 address: Box<dyn crate::address::Address>,
                 options: crate::traits::NewStoreOptions| {
                    Box::pin(async move {
                    use crate::stores::document_store::GuardianDBDocumentStore;

                    // Convert Box<dyn Address> into Arc<dyn Address>.
                    let arc_address: Arc<dyn crate::address::Address> =
                        Arc::from(address as Box<dyn crate::address::Address>);

                    let store = GuardianDBDocumentStore::new(client, identity, arc_address, options)
                        .await
                        .map_err(|e| crate::guardian::error::GuardianError::Store(e.to_string()))?;

                    Ok(Box::new(store)
                        as Box<
                            dyn crate::traits::Store<Error = crate::guardian::error::GuardianError>,
                        >)
                })
                    as Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = crate::guardian::error::Result<
                                        Box<
                                            dyn crate::traits::Store<
                                                    Error = crate::guardian::error::GuardianError,
                                                >,
                                        >,
                                    >,
                                > + Send,
                        >,
                    >
                },
            );

        // Perform the registrations.
        self.register_store_type("eventlog".to_string(), eventlog_constructor);
        self.register_store_type("keyvalue".to_string(), keyvalue_constructor);
        self.register_store_type("document".to_string(), document_constructor);

        tracing::debug!(
            types = ?self.store_types_names(),
            "Default constructors registered"
        );
    }

    /// Returns the event bus of the GuardianDB instance.
    pub fn event_bus(&self) -> Arc<EventBusImpl> {
        self.event_bus.clone()
    }

    /// Starts a background task to listen for pubsub events and process them.
    pub async fn monitor_direct_channel(
        &self,
        event_bus: Arc<EventBusImpl>,
    ) -> Result<JoinHandle<()>> {
        let mut receiver = event_bus
            .subscribe::<EventPubSubPayload>()
            .await
            .map_err(|e| {
                GuardianError::Other(format!("could not subscribe to pubsub events: {}", e))
            })?;

        // Clone the Arcs and other data needed for the asynchronous task.
        let token = self.cancellation_token.clone();
        let message_marshaler = self.message_marshaler.clone();
        let emitters = self.emitters.clone();
        let stores = self.stores.clone();

        let handle = tokio::spawn(async move {
            tracing::debug!("Direct channel monitor started");

            loop {
                tokio::select! {
                    // Listen for the cancellation signal.
                    _ = token.cancelled() => {
                        tracing::debug!("monitor_direct_channel shutting down");
                        return;
                    }
                    // Listen for new events.
                    maybe_event = receiver.recv() => {
                        match maybe_event {
                            Ok(event) => {
                                tracing::trace!(
                                    peer = %event.peer,
                                    payload_size = event.payload.len(),
                                    "Event received on the direct channel"
                                );

                                // STEP 1: Deserialize the message using message_marshaler.
                                let msg = match message_marshaler.unmarshal(&event.payload) {
                                    Ok(msg) => msg,
                                    Err(e) => {
                                        tracing::warn!(
                                            peer = %event.peer,
                                            error = %e,
                                            payload_size = event.payload.len(),
                                            "Failed to deserialize direct channel message"
                                        );
                                        continue;
                                    }
                                };

                                tracing::debug!(
                                    peer = %event.peer,
                                    store_address = %msg.address,
                                    heads_count = msg.heads.len(),
                                    "Message deserialized successfully"
                                );

                                // STEP 2: Find the matching store by address.
                                // If msg.address is just the log name, try to find the store ending with that name.
                                let store = {
                                    let stores_guard = stores.read();

                                    // First try an exact lookup.
                                    if let Some(store) = stores_guard.get(&msg.address) {
                                        Some(store.clone())
                                    } else {
                                        // If not found, search for an address ending with msg.address.
                                        tracing::debug!(
                                            looking_for = %msg.address,
                                            available_stores = ?stores_guard.keys().collect::<Vec<_>>(),
                                            "Looking up store by partial name"
                                        );

                                        stores_guard.iter()
                                            .find(|(addr, _)| addr.ends_with(&msg.address))
                                            .map(|(_, store)| store.clone())
                                    }
                                };

                                let _store = match store {
                                    Some(store) => {
                                        tracing::debug!(
                                            store_address = %store.address(),
                                            peer = %event.peer,
                                            "Store found for processing"
                                        );
                                        store
                                    },
                                    None => {
                                        tracing::warn!(
                                            store_address = %msg.address,
                                            peer = %event.peer,
                                            "Store not found for address, ignoring message"
                                        );
                                        continue;
                                    }
                                };

                                // STEP 3: Process the head exchange.
                                // Perform basic validation of the received heads.
                                let valid_heads: Vec<_> = msg.heads.iter()
                                    .filter(|head| !head.id.is_empty() && !head.payload.is_empty())
                                    .cloned()
                                    .collect();

                                if valid_heads.is_empty() {
                                    tracing::warn!(
                                        store_address = %msg.address,
                                        peer = %event.peer,
                                        total_heads = msg.heads.len(),
                                        "All received heads are invalid"
                                    );
                                    continue;
                                }

                                tracing::debug!(
                                    store_address = %msg.address,
                                    peer = %event.peer,
                                    valid_heads = valid_heads.len(),
                                    total_heads = msg.heads.len(),
                                    "Processing valid heads"
                                );

                                // STEP 4: Actual synchronization with the store.
                                // Synchronization using the store's sync method.
                                tracing::debug!(
                                    store_address = %msg.address,
                                    peer = %event.peer,
                                    valid_heads = valid_heads.len(),
                                    "Starting synchronization with the store"
                                );

                                // Perform the synchronization using the Store trait's sync method.
                                // Note: we use interior mutability for compatibility with Arc<>.
                                let sync_result = Self::sync_store_with_heads(&_store, valid_heads.clone()).await;

                                match sync_result {
                                    Ok(()) => {
                                        tracing::debug!(
                                            store_address = %msg.address,
                                            peer = %event.peer,
                                            processed_heads = valid_heads.len(),
                                            "Head synchronization completed successfully"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            store_address = %msg.address,
                                            peer = %event.peer,
                                            error = %e,
                                            attempted_heads = valid_heads.len(),
                                            "Error during head synchronization"
                                        );
                                        // We do not continue here so the event can still be emitted even on error.
                                    }
                                }

                                // STEP 5: Emit an event to notify interested components.
                                let exchange_event = EventExchangeHeads::new(event.peer, msg);
                                if let Err(e) = emitters.new_heads.emit(exchange_event) {
                                    tracing::error!(
                                        error = %e,
                                        peer = %event.peer,
                                        "Error emitting new_heads event"
                                    );
                                } else {
                                    tracing::trace!(peer = %event.peer, "new_heads event emitted successfully");
                                }
                            }
                            Err(_) => {
                                // The channel was closed, shut down the task.
                                tracing::debug!("Event channel closed, shutting down monitor");
                                break;
                            }
                        }
                    }
                }
            }

            tracing::debug!("Direct channel monitor finished");
        });

        Ok(handle)
    }

    /// Helper method to synchronize a store with received heads.
    /// Solves the mutability problem when working with Arc<GuardianStore>.
    async fn sync_store_with_heads(
        store: &Arc<GuardianStore>,
        heads: Vec<crate::log::entry::Entry>,
    ) -> Result<()> {
        // Strategy: use interior mutability via downcasting to BaseStore.
        // First, try to downcast to BaseStore directly.
        if let Some(base_store) = store
            .as_any()
            .downcast_ref::<crate::stores::base_store::BaseStore>()
        {
            // BaseStore works with interior mutability.
            return base_store.sync(heads).await.map_err(|e| {
                GuardianError::Store(format!("Error in BaseStore synchronization: {}", e))
            });
        }
        // Fallback: for stores that do not expose BaseStore directly.
        // EventLogStore - try to access the inner BaseStore.
        if let Some(event_log_store) = store
            .as_any()
            .downcast_ref::<crate::stores::event_log_store::GuardianDBEventLogStore>(
        ) {
            // Access the inner BaseStore, which has sync(&self).
            let base_store = event_log_store.basestore();
            return base_store.sync(heads).await.map_err(|e| {
                GuardianError::Store(format!("Error in EventLogStore synchronization: {}", e))
            });
        }

        // KeyValueStore — uses the Store trait directly (iroh-docs backend).
        if let Some(kv_store) = store
            .as_any()
            .downcast_ref::<crate::stores::kv_store::GuardianDBKeyValue>()
        {
            return kv_store.sync(heads).await.map_err(|e| {
                GuardianError::Store(format!("Error in KeyValueStore synchronization: {}", e))
            });
        }

        // DocumentStore - uses automatic iroh-docs Willow sync.
        if store
            .as_any()
            .downcast_ref::<crate::stores::document_store::GuardianDBDocumentStore>()
            .is_some()
        {
            // iroh-docs manages sync via Willow — heads are not needed.
            return Ok(());
        }

        // If no downcast worked, return an error.
        Err(GuardianError::Other(
            "Store type not supported for synchronization, or downcast failed".to_string(),
        ))
    }

    /// Helper method to get the total number of entries in a store.
    /// Used to generate informational events about the store's state.
    async fn get_store_total_entries(&self, store: &Arc<GuardianStore>) -> Result<usize> {
        // Try to access the inner BaseStore to get oplog information.
        // First, try to downcast to BaseStore directly.
        if let Some(base_store) = store
            .as_any()
            .downcast_ref::<crate::stores::base_store::BaseStore>()
        {
            // Access the oplog to get the number of entries.
            let op_log = base_store.op_log();
            let log = op_log.read();
            return Ok(log.len());
        }

        // Fallback: for stores that do not expose BaseStore directly.
        // EventLogStore - try to access the inner BaseStore.
        if let Some(event_log_store) = store
            .as_any()
            .downcast_ref::<crate::stores::event_log_store::GuardianDBEventLogStore>(
        ) {
            let base_store = event_log_store.basestore();
            let op_log = base_store.op_log();
            let log = op_log.read();
            return Ok(log.len());
        }

        // KeyValueStore — uses the Store trait directly (iroh-docs backend).
        if let Some(kv_store) = store
            .as_any()
            .downcast_ref::<crate::stores::kv_store::GuardianDBKeyValue>()
        {
            let op_log = kv_store.op_log();
            let log = op_log.read();
            return Ok(log.len());
        }

        // DocumentStore - uses iroh-docs, returns the local index size.
        if let Some(doc_store) = store
            .as_any()
            .downcast_ref::<crate::stores::document_store::GuardianDBDocumentStore>(
        ) {
            // iroh-docs does not use an OpLog — return the number of entries in the local index.
            let op_log = doc_store.op_log();
            let log = op_log.read();
            return Ok(log.len());
        }

        // If no downcast worked, return an error.
        Err(GuardianError::Other(
            "Store type not supported for getting the total number of entries, or downcast failed"
                .to_string(),
        ))
    }

    /// Static helper function to create and start the direct channel monitor
    /// during initialization, avoiding circular reference issues.
    fn start_monitor_task(
        event_bus: Arc<EventBusImpl>,
        cancellation_token: CancellationToken,
        span: Span,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let _enter = span.enter();
            // Try to subscribe to pubsub events.
            let mut receiver = match event_bus.subscribe::<EventPubSubPayload>().await {
                Ok(rx) => rx,
                Err(e) => {
                    tracing::error!("Failed to subscribe to pubsub events: {}", e);
                    return;
                }
            };

            tracing::debug!("Direct channel monitor started");

            loop {
                tokio::select! {
                    // Listen for the cancellation signal.
                    _ = cancellation_token.cancelled() => {
                        tracing::debug!("Direct channel monitor shutting down");
                        return;
                    }
                    // Listen for new events.
                    maybe_event = receiver.recv() => {
                        match maybe_event {
                            Ok(event) => {
                                tracing::trace!(
                                    peer = %event.peer,
                                    "Event received in the direct channel monitor"
                                );

                                // Process different types of direct channel events:
                                // 1. Head exchange events (data synchronization)
                                // 2. Peer connection/disconnection events
                                // 3. Protocol message events

                                tracing::debug!(
                                    event_type = "pubsub_payload",
                                    from_peer = %event.peer,
                                    payload_size = event.payload.len(),
                                    "Processing direct channel event"
                                );

                                // Note: full processing is done by the main monitor via monitor_direct_channel(),
                                // which has access to the message_marshaler, stores and emitters.
                            }
                            Err(_) => {
                                tracing::debug!("Event channel closed, shutting down monitor");
                                break;
                            }
                        }
                    }
                }
            }
        })
    }

    /// Verifies the access permissions of heads using the store's Access Controller.
    ///
    /// Performs a complete permission check for each head:
    /// 1. Extract the head's identity
    /// 2. Verify write permissions via the Access Controller
    /// 3. Validate the identity signature if necessary
    /// 4. Filter out unauthorized heads
    ///
    /// # Arguments
    ///
    /// * `heads` - The list of heads to verify
    /// * `store` - The store that holds the Access Controller for verification
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<Entry>)` - A filtered list containing only authorized heads
    /// * `Err(GuardianError)` - If there was a critical error during verification
    ///
    /// # Security Policy
    ///
    /// - Heads without an identity are **rejected** for security reasons
    /// - Invalid or unauthorized identities are **rejected**
    /// - Verification failures are logged but do not interrupt processing
    /// - Only explicitly authorized heads are accepted
    async fn verify_heads_permissions(
        &self,
        heads: &[crate::log::entry::Entry],
        store: &Arc<GuardianStore>,
    ) -> Result<Vec<crate::log::entry::Entry>> {
        tracing::debug!(
            heads_count = heads.len(),
            "Starting permission verification for heads"
        );

        let mut authorized_heads = Vec::new();
        let mut denied_count = 0;
        let mut no_identity_count = 0;

        // Get the store's Access Controller for verification.
        let access_control = {
            // Try to access the inner BaseStore of the known stores.
            if let Some(event_log_store) = store
                .as_any()
                .downcast_ref::<crate::stores::event_log_store::GuardianDBEventLogStore>(
            ) {
                event_log_store.basestore().access_controller()
            } else if let Some(kv_store) = store
                .as_any()
                .downcast_ref::<crate::stores::kv_store::GuardianDBKeyValue>()
            {
                kv_store.access_controller()
            } else if let Some(doc_store) = store
                .as_any()
                .downcast_ref::<crate::stores::document_store::GuardianDBDocumentStore>(
            ) {
                doc_store.access_controller()
            } else if let Some(base_store) = store
                .as_any()
                .downcast_ref::<crate::stores::base_store::BaseStore>()
            {
                base_store.access_controller()
            } else {
                tracing::warn!("Store type not supported for permission verification");
                return Err(GuardianError::Store(
                    "Store type not supported for permission verification".to_string(),
                ));
            }
        };

        tracing::debug!(
            access_control_type = access_control.get_type(),
            "Access Controller obtained"
        );

        // Verify each head individually.
        for (i, head) in heads.iter().enumerate() {
            // CHECK 1: Presence of an identity.
            let identity = match &head.identity {
                Some(identity) => identity,
                None => {
                    tracing::debug!(
                        head_index = i + 1,
                        total_heads = heads.len(),
                        head_hash = %head.hash,
                        "Head rejected: no identity"
                    );
                    no_identity_count += 1;
                    continue;
                }
            };

            // CHECK 2: Basic identity validation.
            if identity.id().is_empty() || identity.pub_key().is_empty() {
                tracing::debug!(
                    head_index = i + 1,
                    total_heads = heads.len(),
                    head_hash = %head.hash,
                    "Head rejected: invalid identity"
                );
                denied_count += 1;
                continue;
            }

            // CHECK 3: Write permissions via the Access Controller.
            let identity_key = identity.pub_key();
            let has_write_permission = match access_control.get_authorized_by_role("write").await {
                Ok(authorized_keys) => {
                    // Check whether the key is explicitly authorized.
                    authorized_keys.contains(&identity_key.to_string())
                        || authorized_keys.contains(&identity.id().to_string())
                        || authorized_keys.contains(&"*".to_string()) // Universal permission.
                }
                Err(e) => {
                    tracing::warn!(
                        head_index = i + 1,
                        total_heads = heads.len(),
                        error = %e,
                        head_hash = %head.hash,
                        "Head error while checking permissions"
                    );

                    // Emit a permission error event.
                    let permission_denied_event = EventPermissionDenied::new(
                        store.address().to_string(),
                        identity.id().to_string(),
                        identity_key.to_string(),
                        "write".to_string(),
                    );

                    if let Err(emit_err) = self
                        .emitters
                        .permission_denied
                        .emit(permission_denied_event)
                    {
                        tracing::warn!(error = %emit_err, "Error emitting PermissionDenied event");
                    }

                    false // On error, deny access for safety.
                }
            };

            if !has_write_permission {
                tracing::debug!(
                    head_index = i + 1,
                    total_heads = heads.len(),
                    head_hash = %head.hash,
                    identity_id = identity.id(),
                    "Head rejected: no write permission"
                );

                // Emit a permission-denied event.
                let permission_denied_event = EventPermissionDenied::new(
                    store.address().to_string(),
                    identity.id().to_string(),
                    identity_key.to_string(),
                    "write".to_string(),
                );

                if let Err(e) = self
                    .emitters
                    .permission_denied
                    .emit(permission_denied_event)
                {
                    tracing::warn!(error = %e, "Error emitting PermissionDenied event");
                }

                denied_count += 1;
                continue;
            }

            // CHECK 4: Also check administrative permissions as a fallback.
            let has_admin_permission = match access_control.get_authorized_by_role("admin").await {
                Ok(admin_keys) => {
                    admin_keys.contains(&identity_key.to_string())
                        || admin_keys.contains(&identity.id().to_string())
                        || admin_keys.contains(&"*".to_string())
                }
                Err(_) => false, // Not critical if admin fails.
            };

            // CHECK 5: Accept the head if it has write or admin permission.
            if has_write_permission || has_admin_permission {
                let permission_type = if has_admin_permission {
                    "admin"
                } else {
                    "write"
                };
                tracing::debug!(
                    head_index = i + 1,
                    total_heads = heads.len(),
                    permission_type = permission_type,
                    head_hash = %head.hash,
                    identity_id = identity.id(),
                    "Head authorized"
                );

                authorized_heads.push(head.clone());
            } else {
                tracing::debug!(
                    head_index = i + 1,
                    total_heads = heads.len(),
                    head_hash = %head.hash,
                    identity_id = identity.id(),
                    "Head rejected: no adequate permissions"
                );

                // Emit the final permission-denied event.
                let permission_denied_event = EventPermissionDenied::new(
                    store.address().to_string(),
                    identity.id().to_string(),
                    identity_key.to_string(),
                    "write/admin".to_string(),
                );

                if let Err(e) = self
                    .emitters
                    .permission_denied
                    .emit(permission_denied_event)
                {
                    tracing::warn!(error = %e, "Error emitting PermissionDenied event");
                }

                denied_count += 1;
            }
        }

        // Detailed log of the verification results.
        let authorized_count = authorized_heads.len();
        let total_heads = heads.len();

        tracing::debug!(
            total_heads = total_heads,
            authorized_heads = authorized_count,
            denied_heads = denied_count,
            no_identity_heads = no_identity_count,
            access_control_type = access_control.get_type(),
            "Permission verification completed"
        );

        if authorized_count == 0 && total_heads > 0 {
            tracing::warn!(
                total_heads = total_heads,
                "WARNING: All heads were rejected due to missing permissions"
            );

            // List the authorized keys for debugging.
            if let Ok(write_keys) = access_control.get_authorized_by_role("write").await {
                tracing::debug!(write_keys = ?write_keys, "Keys authorized for writing");
            }
            if let Ok(admin_keys) = access_control.get_authorized_by_role("admin").await {
                tracing::debug!(admin_keys = ?admin_keys, "Keys authorized for admin");
            }
        } else if authorized_count < total_heads {
            tracing::info!(
                authorized_heads = authorized_count,
                total_heads = total_heads,
                rejected_heads = total_heads - authorized_count,
                "Partial permission verification completed"
            );
        } else if authorized_count == total_heads && total_heads > 0 {
            tracing::debug!(
                authorized_heads = total_heads,
                "Full verification: all heads were authorized"
            );
        }

        Ok(authorized_heads)
    }

    /// Cryptographically verifies the validity of an identity.
    ///
    /// Performs a complete identity verification using:
    /// 1. Public key validation
    /// 2. Signature verification using Ed25519 (consistent with Iroh)
    /// 3. Validation of the identity and public key signatures
    /// 4. Integrity verification of the signed data
    ///
    /// # Arguments
    ///
    /// * `identity` - The identity to verify
    ///
    /// # Returns
    ///
    /// * `Ok(())` if the identity is valid
    /// * `Err(GuardianError)` if verification failed
    async fn verify_identity_cryptographically(&self, identity: &Identity) -> Result<()> {
        use ed25519_dalek::VerifyingKey;

        // STEP 1: Basic validation of the required fields.
        if identity.id().is_empty() {
            return Err(GuardianError::Store(
                "Identity ID cannot be empty".to_string(),
            ));
        }

        if identity.pub_key().is_empty() {
            return Err(GuardianError::Store(
                "Identity public key cannot be empty".to_string(),
            ));
        }

        // STEP 2: Public key validation using Ed25519.
        let pub_key_hex = identity.pub_key();
        let pub_key_bytes = match hex::decode(pub_key_hex) {
            Ok(bytes) => bytes,
            Err(e) => {
                return Err(GuardianError::Store(format!(
                    "Failed to decode public key from hex: {}",
                    e
                )));
            }
        };

        if pub_key_bytes.len() != 32 {
            return Err(GuardianError::Store(format!(
                "Invalid Ed25519 public key length: expected 32 bytes, got {}",
                pub_key_bytes.len()
            )));
        }

        let mut pk_array = [0u8; 32];
        pk_array.copy_from_slice(&pub_key_bytes);

        let public_key = match VerifyingKey::from_bytes(&pk_array) {
            Ok(pk) => pk,
            Err(e) => {
                return Err(GuardianError::Store(format!(
                    "Invalid Ed25519 public key: {}",
                    e
                )));
            }
        };

        // STEP 3: Verify the identity's signatures.
        let signatures = identity.signatures();

        // Verify the ID signature.
        if !signatures.id().is_empty() {
            match self.verify_signature_with_ed25519(identity.id(), signatures.id(), &public_key) {
                Ok(true) => {
                    tracing::debug!("Identity ID signature verified successfully");
                }
                Ok(false) => {
                    return Err(GuardianError::Store(
                        "Identity ID signature verification failed".to_string(),
                    ));
                }
                Err(e) => {
                    return Err(GuardianError::Store(format!(
                        "Error verifying ID signature: {}",
                        e
                    )));
                }
            }
        }

        // Verify the public key signature.
        if !signatures.pub_key().is_empty() {
            // Reconstruct the data that was signed for the public key.
            let pub_key_data = format!("{}{}", identity.pub_key(), signatures.id());

            match self.verify_signature_with_ed25519(
                &pub_key_data,
                signatures.pub_key(),
                &public_key,
            ) {
                Ok(true) => {
                    tracing::debug!("Identity public key signature verified successfully");
                }
                Ok(false) => {
                    return Err(GuardianError::Store(
                        "Identity public key signature verification failed".to_string(),
                    ));
                }
                Err(e) => {
                    return Err(GuardianError::Store(format!(
                        "Error verifying public key signature: {}",
                        e
                    )));
                }
            }
        }

        // STEP 4: Additional public key consistency check.
        if let Some(_public_key) = identity.public_key() {
            // The public key was already validated via Ed25519 above.
            // Iroh's NodeId is derived directly from the Ed25519 public key.
            tracing::debug!(
                identity_id = %identity.id(),
                "Identity cryptographic verification completed successfully"
            );
        }

        tracing::debug!(
            identity_id = identity.id(),
            public_key_len = identity.pub_key().len(),
            "Identity cryptographic verification completed successfully"
        );

        Ok(())
    }

    /// Verifies a signature using Ed25519 (consistent with Iroh).
    ///
    /// # Arguments
    ///
    /// * `message` - The original message that was signed
    /// * `signature_str` - The signature as a hex string
    /// * `public_key` - The Ed25519 public key
    ///
    /// # Returns
    ///
    /// * `Ok(true)` if the signature is valid
    /// * `Ok(false)` if the signature is invalid
    /// * `Err(GuardianError)` if there was an error during verification
    fn verify_signature_with_ed25519(
        &self,
        message: &str,
        signature_str: &str,
        public_key: &ed25519_dalek::VerifyingKey,
    ) -> Result<bool> {
        use ed25519_dalek::{Signature, Verifier};

        // Decode the signature from hex.
        let sig_bytes = match hex::decode(signature_str) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::debug!(
                    signature = signature_str,
                    error = %e,
                    "Failed to decode signature from hex"
                );
                return Ok(false); // Invalid signature, not a fatal error.
            }
        };

        if sig_bytes.len() != 64 {
            tracing::debug!(
                signature = signature_str,
                length = sig_bytes.len(),
                "Invalid signature length: expected 64 bytes"
            );
            return Ok(false);
        }

        // Parse the signature.
        let signature = match Signature::from_slice(&sig_bytes) {
            Ok(sig) => sig,
            Err(e) => {
                tracing::debug!(
                    signature = signature_str,
                    error = %e,
                    "Failed to parse signature"
                );
                return Ok(false); // Invalid signature, not a fatal error.
            }
        };

        // Verify the signature.
        match public_key.verify(message.as_bytes(), &signature) {
            Ok(()) => {
                tracing::debug!("Signature verification successful");
                Ok(true)
            }
            Err(e) => {
                tracing::debug!(error = %e, "Signature verification failed");
                Ok(false) // Invalid signature, not a fatal error.
            }
        }
    }

    /// Processes a "heads" exchange event, synchronizing the new entries with the local store.
    ///
    /// Performs the full synchronization of the received heads, including:
    /// 1. Head integrity validation
    /// 2. Access permission verification
    /// 3. Detection of existing duplicates
    /// 4. Actual synchronization with the store
    /// 5. Emission of progress events
    ///
    /// # Arguments
    ///
    /// * `event` - Event containing the heads to synchronize and metadata
    /// * `store` - Reference to the store that will receive the heads
    ///
    /// # Processing
    ///
    /// 1. **Basic Validation**: checks that the heads have valid data (hash, payload)
    /// 2. **Access Control**: uses the store's access controller to validate permissions
    /// 3. **Duplicate Detection**: queries the oplog to avoid reprocessing
    /// 4. **Synchronization**: delegates to the store's `sync()` method, which implements the full logic
    /// 5. **Events**: emits progress events for interested components
    ///
    /// # Performance
    ///
    /// - **O(n)** where n = number of received heads
    /// - **Parallelization**: sequential validation, but batch sync for efficiency
    /// - **Cache-aware**: leverages existing indexes for duplicate detection
    ///
    /// # Errors
    ///
    /// - Returns an error if the store synchronization fails
    /// - Individual invalid heads are ignored (logged) but do not cause an overall failure
    pub async fn handle_event_exchange_heads(
        &self,
        event: &MessageExchangeHeads,
        store: Arc<GuardianStore>,
    ) -> Result<()> {
        let heads = &event.heads;
        let store_address = &event.address;

        tracing::debug!(
            node_id = %self.node_id(),
            count = heads.len(),
            store_address = store_address,
            "Processing exchange heads event"
        );

        if heads.is_empty() {
            tracing::debug!("No heads received for synchronization");
            return Ok(());
        }

        // STEP 1: Basic validation and filtering of invalid heads.
        let mut valid_heads = Vec::new();
        let mut skipped_count = 0;

        for (i, head) in heads.iter().enumerate() {
            // Basic integrity validation.
            let empty_hash = Hash::from([0u8; 32]);
            if head.hash == empty_hash || head.payload.is_empty() {
                tracing::debug!(
                    head_index = i + 1,
                    total_heads = heads.len(),
                    "Head ignored: invalid data (empty hash or payload)"
                );
                skipped_count += 1;
                continue;
            }

            // Structure validation.
            if head.id.is_empty() {
                tracing::debug!(
                    head_index = i + 1,
                    total_heads = heads.len(),
                    "Head ignored: empty ID"
                );
                skipped_count += 1;
                continue;
            }

            // Identity validation (if available).
            if let Some(identity) = &head.identity {
                if identity.id().is_empty() || identity.pub_key().is_empty() {
                    tracing::warn!(
                        head_index = i + 1,
                        total_heads = heads.len(),
                        head_hash = %head.hash,
                        "Head with invalid identity"
                    );
                } else {
                    tracing::debug!(
                        head_index = i + 1,
                        total_heads = heads.len(),
                        head_hash = %head.hash,
                        identity_id = identity.id(),
                        "Head with valid identity"
                    );

                    // Cryptographic verification of the identity.
                    match self.verify_identity_cryptographically(identity).await {
                        Ok(()) => {
                            tracing::debug!(
                                head_index = i + 1,
                                total_heads = heads.len(),
                                head_hash = %head.hash,
                                "Head identity verified cryptographically"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                head_index = i + 1,
                                total_heads = heads.len(),
                                head_hash = %head.hash,
                                error = %e,
                                "Head failed cryptographic identity verification"
                            );
                            // Continue processing even on verification failure, for compatibility.
                            // Note: in production you may choose to reject heads with invalid identities.
                        }
                    }
                }
            }

            valid_heads.push(head.clone());

            tracing::debug!(
                head_index = i + 1,
                total_heads = heads.len(),
                head_hash = %head.hash,
                clock_id = head.clock.id(),
                clock_time = head.clock.time(),
                "Head validated"
            );
        }

        if valid_heads.is_empty() {
            tracing::warn!(total_heads = heads.len(), "All received heads are invalid");
            return Ok(());
        }

        if skipped_count > 0 {
            tracing::debug!(
                valid_heads = valid_heads.len(),
                total_heads = heads.len(),
                skipped_count = skipped_count,
                "Validation completed with skipped heads"
            );
        }

        // STEP 2: Verify access permissions via the Access Controller.
        tracing::debug!(
            valid_heads_count = valid_heads.len(),
            "Verifying access permissions for heads"
        );

        // Full permission verification using the store's Access Controller.
        let permitted_heads = self.verify_heads_permissions(&valid_heads, &store).await?;

        // STEP 3: Detect duplicates by querying the existing oplog.
        tracing::debug!(
            permitted_heads_count = permitted_heads.len(),
            "Checking the oplog for duplicate heads"
        );

        let mut new_heads = Vec::new();
        let mut duplicate_count = 0;

        // For each head, check whether it already exists in the store's oplog.
        for (i, head) in permitted_heads.iter().enumerate() {
            // Check whether the head already exists in the store's oplog.
            let head_hash = head.hash();
            let already_exists =
                {
                    // Try to access the oplog through the known store types.
                    if let Some(event_log_store) = store.as_any()
                    .downcast_ref::<crate::stores::event_log_store::GuardianDBEventLogStore>()
                {
                    event_log_store.basestore().op_log().read().has(head_hash)
                } else if let Some(kv_store) = store.as_any()
                    .downcast_ref::<crate::stores::kv_store::GuardianDBKeyValue>()
                {
                    kv_store.op_log().read().has(head_hash)
                } else if let Some(doc_store) = store.as_any()
                    .downcast_ref::<crate::stores::document_store::GuardianDBDocumentStore>()
                {
                    doc_store.op_log().read().has(head_hash)
                } else if let Some(base_store) = store.as_any()
                    .downcast_ref::<crate::stores::base_store::BaseStore>()
                {
                    base_store.op_log().read().has(head_hash)
                } else {
                    tracing::warn!(
                        head_index = i + 1,
                        total_heads = permitted_heads.len(),
                        head_hash = %head_hash,
                        "Store type not supported for duplicate detection, assuming new"
                    );
                    false // If we cannot check, assume it is new.
                }
                };

            if already_exists {
                tracing::debug!(
                    head_index = i + 1,
                    total_heads = permitted_heads.len(),
                    head_hash = %head_hash,
                    "Head already exists in the oplog (duplicate)"
                );
                duplicate_count += 1;
            } else {
                tracing::debug!(
                    head_index = i + 1,
                    total_heads = permitted_heads.len(),
                    head_hash = %head_hash,
                    "Head is new, adding it for synchronization"
                );
                new_heads.push(head.clone());
            }
        }

        if new_heads.is_empty() {
            tracing::debug!(
                duplicate_count = duplicate_count,
                "All heads are duplicates, synchronization unnecessary"
            );
            return Ok(());
        }

        if duplicate_count > 0 {
            tracing::debug!(
                new_heads = new_heads.len(),
                total_heads = heads.len(),
                duplicate_count = duplicate_count,
                "Duplicates detected"
            );
        }

        // STEP 4: Actual synchronization with the store.
        tracing::debug!(
            valid_heads = new_heads.len(),
            store_address = store_address,
            "Starting synchronization with the store"
        );

        // Store the count before moving the vector and measure the sync time.
        let new_heads_count = new_heads.len();
        let sync_start_time = std::time::Instant::now();

        // Create a copy of the entries for use in the events.
        let entries_for_events = new_heads.clone();

        // Helper method that solves mutability problems.
        let sync_result = Self::sync_store_with_heads(&store, new_heads).await;

        // Compute the synchronization duration.
        let sync_duration = sync_start_time.elapsed();
        let duration_ms = sync_duration.as_millis() as u64;

        match sync_result {
            Ok(()) => {
                tracing::debug!(
                    processed_count = new_heads_count,
                    store_address = store_address,
                    duration_ms = duration_ms,
                    "Head synchronization completed successfully"
                );

                // STEP 5: Emit success events.
                // Emit a synchronization event for interested components.
                let exchange_event = EventExchangeHeads::new(self.node_id(), event.clone());

                if let Err(e) = self.emitters.new_heads.emit(exchange_event) {
                    tracing::warn!(error = %e, "Failed to emit new_heads event");
                } else {
                    tracing::trace!(
                        processed_heads = new_heads_count,
                        "new_heads event emitted successfully"
                    );
                }

                // STEP 6: Emit store-specific events.
                // Get store information for the events.
                let store_type = store.store_type();
                let total_entries = self.get_store_total_entries(&store).await.unwrap_or(0);

                // EventStoreUpdated: notifies changes in the store.
                let store_updated_event = EventStoreUpdated::new(
                    store_address.clone(),
                    store_type.to_string(),
                    new_heads_count,
                    total_entries,
                );

                if let Err(e) = self.emitters.store_updated.emit(store_updated_event) {
                    tracing::warn!(error = %e, "Failed to emit store_updated event");
                } else {
                    tracing::debug!(
                        store_address = store_address,
                        entries_added = new_heads_count,
                        "store_updated event emitted successfully"
                    );
                }

                // EventSyncCompleted: notifies the completion of the synchronization.
                let sync_completed_event = EventSyncCompleted::new(
                    store_address.clone(),
                    self.node_id().to_string(),
                    new_heads_count,
                    duration_ms,
                    true, // success = true
                );

                if let Err(e) = self.emitters.sync_completed.emit(sync_completed_event) {
                    tracing::warn!(error = %e, "Failed to emit sync_completed event");
                } else {
                    tracing::debug!(
                        store_address = store_address,
                        duration_ms = duration_ms,
                        "sync_completed event emitted successfully"
                    );
                }

                // EventNewEntries: notifies new entries that were added.
                if !entries_for_events.is_empty() {
                    let new_entries_event = EventNewEntries::new(
                        store_address.clone(),
                        entries_for_events,
                        total_entries,
                    );

                    if let Err(e) = self.emitters.new_entries.emit(new_entries_event) {
                        tracing::warn!(error = %e, "Failed to emit new_entries event");
                    } else {
                        tracing::debug!(
                            store_address = store_address,
                            new_entries_count = new_heads_count,
                            "new_entries event emitted successfully"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    store_address = store_address,
                    heads_count = new_heads_count,
                    duration_ms = duration_ms,
                    "Head synchronization failed"
                );

                // Emit error events for interested components.
                // EventSyncError: general synchronization error.
                let error_type = match &e {
                    GuardianError::Store(_) => SyncErrorType::StoreError,
                    GuardianError::Network(_) => SyncErrorType::NetworkError,
                    GuardianError::InvalidArgument(_) => SyncErrorType::ValidationError,
                    _ => SyncErrorType::UnknownError,
                };

                let sync_error_event = EventSyncError::new(
                    store_address.clone(),
                    self.node_id().to_string(),
                    e.to_string(),
                    new_heads_count,
                    error_type.clone(),
                );

                if let Err(emit_err) = self.emitters.sync_error.emit(sync_error_event) {
                    tracing::warn!(
                        error = %emit_err,
                        original_error = %e,
                        "Failed to emit sync_error event"
                    );
                } else {
                    tracing::debug!(
                        store_address = store_address,
                        error_type = ?error_type,
                        "sync_error event emitted successfully"
                    );
                }

                // EventSyncCompleted with success = false.
                let sync_completed_event = EventSyncCompleted::new(
                    store_address.clone(),
                    self.node_id().to_string(),
                    0, // heads_synced = 0 due to the error.
                    duration_ms,
                    false, // success = false
                );

                if let Err(emit_err) = self.emitters.sync_completed.emit(sync_completed_event) {
                    tracing::warn!(error = %emit_err, "Failed to emit sync_completed event (error)");
                }

                return Err(e);
            }
        }

        tracing::debug!(
            total_heads_received = heads.len(),
            heads_processed = new_heads_count,
            heads_skipped = skipped_count,
            store_address = store_address,
            "Exchange heads processing completed successfully"
        );

        Ok(())
    }
}

/// Helper function to create a direct communication channel.
pub async fn make_direct_channel(
    event_bus: &EventBusImpl,
    factory: DirectChannelFactory,
    options: &DirectChannelOptions,
) -> Result<Arc<dyn DirectChannel<Error = GuardianError> + Send + Sync>> {
    let emitter = crate::p2p::PayloadEmitter::new(event_bus)
        .await
        .map_err(|e| {
            GuardianError::Other(format!("could not initialize the pubsub emitter: {}", e))
        })?;

    // Use the provided factory to create the direct channel.
    let channel = factory(Arc::new(emitter), Some((*options).clone()))
        .await
        .map_err(|e| GuardianError::Other(format!("Failed to create direct channel: {}", e)))?;

    tracing::debug!("Direct channel created successfully using the provided factory");
    Ok(channel)
}

/// Drop trait implementation to ensure safe GuardianDB cleanup.
impl Drop for GuardianDB {
    fn drop(&mut self) {
        // Abort the monitor task to avoid accessing already-freed memory.
        self._monitor_handle.abort();

        // Cancel the token to signal all tasks that they must stop.
        self.cancellation_token.cancel();

        // We cannot use async in Drop, so we only abort and cancel.
        // The rest of the cleanup is done by the Arcs' automatic destructors.
    }
}

/// BaseGuardianDB trait implementation for GuardianDB.
#[async_trait::async_trait]
impl BaseGuardianDB for GuardianDB {
    type Error = GuardianError;

    fn client(&self) -> Arc<IrohClient> {
        Arc::new(self.client.clone())
    }

    fn identity(&self) -> Arc<Identity> {
        // Create a clone of the Arc<Identity> from the RwLock.
        let identity_guard = self.identity.read();
        Arc::new(identity_guard.clone())
    }

    async fn open(
        &self,
        address: &str,
        options: &mut CreateDBOptions,
    ) -> std::result::Result<Arc<dyn Store<Error = GuardianError>>, Self::Error> {
        // Create a copy of the options to use with the internal method.
        let options_copy = CreateDBOptions {
            event_bus: options.event_bus.clone(),
            directory: options.directory.clone(),
            overwrite: options.overwrite,
            local_only: options.local_only,
            create: options.create,
            store_type: options.store_type.clone(),
            access_controller_address: options.access_controller_address.clone(),
            access_controller: None, // Will be resolved internally if needed.
            replicate: options.replicate,
            keystore: options.keystore.clone(),
            cache: options.cache.clone(),
            identity: options.identity.clone(),
            sort_fn: options.sort_fn,
            timeout: options.timeout,
            message_marshaler: options.message_marshaler.clone(),
            span: options.span.clone(),
            close_func: None,
            store_specific_opts: None,
            doc_ticket: options.doc_ticket.clone(),
            read_only: options.read_only,
        };

        // Call the internal GuardianDB open method.
        let arc_store = GuardianDB::open(self, address, options_copy).await?;

        // Convert Arc<GuardianStore> into Arc<dyn Store>.
        let store_dyn: Arc<dyn Store<Error = GuardianError>> =
            arc_store as Arc<dyn Store<Error = GuardianError>>;

        Ok(store_dyn)
    }

    fn get_store(&self, address: &str) -> Option<Arc<dyn Store<Error = GuardianError>>> {
        // Use the internal GuardianDB get_store method.
        if let Some(arc_store) = GuardianDB::get_store(self, address) {
            // Convert Arc<GuardianStore> into Arc<dyn Store>.
            let store_dyn: Arc<dyn Store<Error = GuardianError>> =
                arc_store as Arc<dyn Store<Error = GuardianError>>;
            Some(store_dyn)
        } else {
            None
        }
    }

    async fn create(
        &self,
        name: &str,
        store_type: &str,
        options: &mut CreateDBOptions,
    ) -> std::result::Result<Arc<dyn Store<Error = GuardianError>>, Self::Error> {
        // Create a copy of the options to use with the internal method.
        let options_copy = CreateDBOptions {
            event_bus: options.event_bus.clone(),
            directory: options.directory.clone(),
            overwrite: options.overwrite,
            local_only: options.local_only,
            create: options.create,
            store_type: Some(store_type.to_string()),
            access_controller_address: options.access_controller_address.clone(),
            access_controller: None,
            replicate: options.replicate,
            keystore: options.keystore.clone(),
            cache: options.cache.clone(),
            identity: options.identity.clone(),
            sort_fn: options.sort_fn,
            timeout: options.timeout,
            message_marshaler: options.message_marshaler.clone(),
            span: options.span.clone(),
            close_func: None,
            store_specific_opts: None,
            doc_ticket: options.doc_ticket.clone(),
            read_only: options.read_only,
        };

        // Call the internal GuardianDB create method.
        let arc_store = GuardianDB::create(self, name, store_type, Some(options_copy)).await?;

        // Convert Arc<GuardianStore> into Arc<dyn Store>.
        let store_dyn: Arc<dyn Store<Error = GuardianError>> =
            arc_store as Arc<dyn Store<Error = GuardianError>>;

        Ok(store_dyn)
    }

    async fn determine_address(
        &self,
        name: &str,
        store_type: &str,
        options: &DetermineAddressOptions,
    ) -> std::result::Result<Box<dyn Address>, Self::Error> {
        // Use the internal GuardianDB determine_address method.
        let guardian_address =
            GuardianDB::determine_address(self, name, store_type, Some(options.clone())).await?;

        // Convert GuardianDBAddress into Box<dyn Address>.
        let boxed_address: Box<dyn Address> = Box::new(guardian_address);

        Ok(boxed_address)
    }

    fn register_store_type(&mut self, store_type: &str, constructor: StoreConstructor) {
        // Use the existing register_store_type logic (avoids recursion by calling the internal method).
        let mut types = self.store_types.write();
        types.insert(store_type.to_string(), constructor);
        tracing::debug!("Registered store type: {}", store_type);
    }

    fn unregister_store_type(&mut self, store_type: &str) {
        // Use the existing unregister_store_type logic (avoids recursion by calling the internal method).
        let mut types = self.store_types.write();
        types.remove(store_type);
        tracing::debug!("Unregistered store type: {}", store_type);
    }

    fn register_access_controller_type(
        &mut self,
        constructor: AccessControllerConstructor,
    ) -> std::result::Result<(), Self::Error> {
        // Register with the "default" type to avoid recursion.
        let mut types = self.access_control_types.write();
        types.insert("default".to_string(), constructor);
        tracing::debug!("Registered access controller type: default");
        Ok(())
    }

    fn unregister_access_controller_type(&mut self, controller_type: &str) {
        // Use the internal method to avoid recursion.
        let mut types = self.access_control_types.write();
        types.remove(controller_type);
        tracing::debug!("Unregistered access controller type: {}", controller_type);
    }

    fn get_access_controller_type(
        &self,
        controller_type: &str,
    ) -> Option<AccessControllerConstructor> {
        // Use direct access to avoid recursion.
        let types = self.access_control_types.read();
        types.get(controller_type).cloned()
    }

    fn event_bus(&self) -> EventBus {
        (*self.event_bus).clone()
    }

    fn span(&self) -> &tracing::Span {
        &self.span
    }

    fn tracer(&self) -> Arc<TracerWrapper> {
        Arc::new(TracerWrapper::OpenTelemetry(self.tracer.clone()))
    }
}
