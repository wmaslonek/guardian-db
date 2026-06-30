use crate::access_control::{acl_simple::SimpleAccessController, traits::AccessController};
use crate::address::Address;
use crate::data_store::Datastore;
use crate::events::{EmitterInterface, EventEmitter};
use crate::guardian::error::{GuardianError, Result};
use crate::log::access_control::{CanAppendAdditionalContext, LogEntry};
use crate::log::identity_provider::{GuardianDBIdentityProvider, IdentityProvider};
use crate::log::{Log, LogOptions, entry::Entry, identity::Identity};
use crate::p2p::network::client::IrohClient;
use crate::p2p::{Emitter, EventBus};
use crate::stores::events::{
    EventLoad, EventLoadProgress, EventReady, EventReplicate, EventReplicateProgress,
    EventReplicated, EventWrite,
};
use crate::stores::operation::Operation;
use crate::traits::{
    DirectChannel, MessageExchangeHeads, MessageMarshaler, NewStoreOptions, PubSubInterface,
    PubSubTopic, Store, StoreIndex, TracerWrapper,
};
use iroh::EndpointId as NodeId;
use iroh_blobs::Hash;
use opentelemetry::trace::{TracerProvider, noop::NoopTracerProvider};
use parking_lot::{MappedRwLockReadGuard, Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::select;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{Span, debug, error, info, instrument, warn};

pub mod index;
pub mod noop_index;
pub mod utils;

pub struct LogAndIndex {
    pub oplog: Arc<RwLock<Log>>,
    /// Active store index - protected independently for flexible access.
    pub active_index: Arc<RwLock<Option<Box<dyn StoreIndex<Error = GuardianError> + Send + Sync>>>>,
}

impl LogAndIndex {
    /// Creates a new instance with independent thread-safe protections.
    pub fn new(
        oplog: Log,
        index: Option<Box<dyn StoreIndex<Error = GuardianError> + Send + Sync>>,
    ) -> Self {
        Self {
            oplog: Arc::new(RwLock::new(oplog)),
            active_index: Arc::new(RwLock::new(index)),
        }
    }

    /// Thread-safe access to the oplog without lifetime limitations.
    pub fn with_oplog<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Log) -> R,
    {
        let guard = self.oplog.read();
        f(&guard)
    }

    /// Thread-safe access to the oplog for modifications.
    pub fn with_oplog_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut Log) -> R,
    {
        let mut guard = self.oplog.write();
        f(&mut guard)
    }

    /// Thread-safe access to the active index.
    pub fn with_index<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&dyn StoreIndex<Error = GuardianError>) -> R,
    {
        let guard = self.active_index.read();
        guard.as_ref().map(|index| f(index.as_ref()))
    }

    /// Thread-safe access to the active index for modifications.
    pub fn with_index_mut<F, R>(&self, f: F) -> Result<Option<R>>
    where
        F: FnOnce(&mut dyn StoreIndex<Error = GuardianError>) -> Result<R>,
    {
        let mut guard = self.active_index.write();
        match guard.as_mut() {
            Some(index) => Ok(Some(f(index.as_mut())?)),
            None => Ok(None),
        }
    }

    /// Updates the index with the oplog entries in a thread-safe way.
    pub fn update_index_safe(&self) -> Result<usize> {
        // First, collect the oplog entries.
        let entries: Vec<Entry> = self.with_oplog(|oplog| {
            oplog
                .values()
                .into_iter()
                .map(|arc_entry| (*arc_entry).clone())
                .collect()
        });

        // Update the index with the collected entries.
        match self.with_index_mut(|index| {
            // Create a temporary reference to the oplog for the update.
            let oplog_guard = self.oplog.read();
            index.update_index(&oplog_guard, &entries)
        })? {
            Some(_result) => Ok(entries.len()),
            None => Ok(0), // No active index.
        }
    }

    /// Checks whether an active index exists.
    pub fn has_active_index(&self) -> bool {
        let guard = self.active_index.read();
        guard.is_some()
    }

    /// Returns an Arc reference to the oplog for compatibility with the Store trait.
    pub fn op_log_arc(&self) -> Arc<RwLock<Log>> {
        self.oplog.clone()
    }
}

pub struct Emitters {
    evt_write: Emitter<EventWrite>,
    evt_ready: Emitter<EventReady>,
    #[allow(dead_code)]
    evt_replicate_progress: Emitter<EventReplicateProgress>,
    evt_load: Emitter<EventLoad>,
    evt_load_progress: Emitter<EventLoadProgress>,
    evt_replicated: Emitter<EventReplicated>,
    #[allow(dead_code)]
    evt_replicate: Emitter<EventReplicate>,
}
#[allow(dead_code)]
struct CanAppendContextImpl {
    log: Log,
}

impl CanAppendAdditionalContext for CanAppendContextImpl {
    fn get_log_entries(&self) -> Vec<Box<dyn LogEntry>> {
        // Get all log entries and convert them into LogEntry.
        self.log
            .values()
            .into_iter()
            .map(|arc_entry| {
                // Create a LogEntry based on Entry.
                #[derive(Clone)]
                struct EntryLogEntry {
                    entry: Entry,
                }

                impl LogEntry for EntryLogEntry {
                    fn get_payload(&self) -> &[u8] {
                        self.entry.payload()
                    }

                    fn get_identity(&self) -> &Identity {
                        self.entry.get_identity()
                    }
                }

                let entry: Entry = (*arc_entry).clone();
                Box::new(EntryLogEntry { entry }) as Box<dyn LogEntry>
            })
            .collect()
    }
}

// Alternative implementation that uses a snapshot of the entries
// to avoid borrowing issues with the log.
struct CanAppendContextSnapshot {
    entries: Vec<Box<dyn LogEntry>>,
}

impl CanAppendAdditionalContext for CanAppendContextSnapshot {
    fn get_log_entries(&self) -> Vec<Box<dyn LogEntry>> {
        // Create new instances of the entries instead of cloning the boxes.
        self.entries
            .iter()
            .map(|entry_box| {
                // For each entry, create a new clonable EntryLogEntry.
                #[derive(Clone)]
                struct ClonableEntryLogEntry {
                    payload: Vec<u8>,
                    identity: Identity,
                }

                impl LogEntry for ClonableEntryLogEntry {
                    fn get_payload(&self) -> &[u8] {
                        &self.payload
                    }

                    fn get_identity(&self) -> &Identity {
                        &self.identity
                    }
                }

                let payload = entry_box.get_payload().to_vec();
                let identity = entry_box.get_identity().clone();

                Box::new(ClonableEntryLogEntry { payload, identity }) as Box<dyn LogEntry>
            })
            .collect()
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct StoreSnapshot {
    pub id: String,
    pub heads: Vec<Entry>,
    pub size: usize,
    #[serde(rename = "type")]
    pub store_type: String,
}

/// Retry statistics for monitoring P2P communication.
#[derive(Debug, Clone, Default)]
pub struct RetryMetrics {
    pub total_connection_attempts: u64,
    pub failed_connection_attempts: u64,
    pub total_send_attempts: u64,
    pub failed_send_attempts: u64,
    pub successful_retries: u64,
    pub failed_after_all_retries: u64,
    pub peer_exchange_attempts: u64,
    pub peer_exchange_successes: u64,
    pub peer_exchange_failures: u64,
    pub peer_exchange_final_failures: u64,
    pub peer_exchange_timeouts: u64,
    pub peer_exchange_cancellations: u64,
}

impl RetryMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_connection_attempt(&mut self, success: bool) {
        self.total_connection_attempts += 1;
        if !success {
            self.failed_connection_attempts += 1;
        }
    }

    pub fn record_send_attempt(&mut self, success: bool) {
        self.total_send_attempts += 1;
        if !success {
            self.failed_send_attempts += 1;
        }
    }

    pub fn record_successful_retry(&mut self) {
        self.successful_retries += 1;
    }

    pub fn record_final_failure(&mut self) {
        self.failed_after_all_retries += 1;
    }

    // NEW METHODS: For peer exchange.
    pub fn record_peer_exchange_attempt(&mut self) {
        self.peer_exchange_attempts += 1;
    }

    pub fn record_peer_exchange_success(&mut self) {
        self.peer_exchange_successes += 1;
    }

    pub fn record_peer_exchange_failure(&mut self) {
        self.peer_exchange_failures += 1;
    }

    pub fn record_peer_exchange_final_failure(&mut self) {
        self.peer_exchange_final_failures += 1;
    }

    pub fn record_peer_exchange_timeout(&mut self) {
        self.peer_exchange_timeouts += 1;
    }

    pub fn record_peer_exchange_cancellation(&mut self) {
        self.peer_exchange_cancellations += 1;
    }

    /// Records when a peer disconnects.
    pub fn record_peer_disconnection(&mut self) {
        // Can be used for peer churn statistics.
        // For now, this is a placeholder.
        // It can be expanded to include disconnection-specific metrics.
    }

    /// Computes the overall connection success rate.
    pub fn connection_success_rate(&self) -> f64 {
        if self.total_connection_attempts == 0 {
            return 0.0;
        }
        let successful = self.total_connection_attempts - self.failed_connection_attempts;
        (successful as f64 / self.total_connection_attempts as f64) * 100.0
    }

    /// Computes the overall send success rate.
    pub fn send_success_rate(&self) -> f64 {
        if self.total_send_attempts == 0 {
            return 0.0;
        }
        let successful = self.total_send_attempts - self.failed_send_attempts;
        (successful as f64 / self.total_send_attempts as f64) * 100.0
    }

    /// Computes the peer exchange success rate.
    pub fn peer_exchange_success_rate(&self) -> f64 {
        if self.peer_exchange_attempts == 0 {
            return 0.0;
        }
        (self.peer_exchange_successes as f64 / self.peer_exchange_attempts as f64) * 100.0
    }

    pub fn record_failed_after_retries(&mut self) {
        self.failed_after_all_retries += 1;
    }
}

/// This struct is the core of any store (e.g. kvstore, feed) in GuardianDB.
/// It manages the operation log (OpLog), the internal state (index),
/// replication with other peers, the cache, and the store's lifecycle.
pub struct BaseStore {
    // --- Identifiers and Essential Configuration ---
    id: String,
    node_id: NodeId,
    identity: Arc<Identity>,
    address: Arc<dyn Address + Send + Sync>,
    db_name: String,
    #[allow(dead_code)]
    directory: String,
    reference_count: usize,
    sort_fn: SortFn,

    // --- Main Components and External APIs ---
    client: Arc<IrohClient>,
    access_controller: Arc<dyn AccessController>,
    identity_provider: Arc<dyn IdentityProvider>,

    // --- Internal State ---
    cache: Arc<dyn Datastore>,
    log_and_index: LogAndIndex,

    // --- Replication Components ---
    pubsub: Arc<dyn PubSubInterface<Error = GuardianError> + Send + Sync>,
    message_marshaler: Arc<dyn MessageMarshaler<Error = GuardianError> + Send + Sync>,
    direct_channel:
        Arc<tokio::sync::Mutex<Arc<dyn DirectChannel<Error = GuardianError> + Send + Sync>>>,
    topic:
        Arc<tokio::sync::Mutex<Option<Arc<dyn PubSubTopic<Error = GuardianError> + Send + Sync>>>>,

    // --- Event System and Observability ---
    event_bus: Arc<EventBus>,
    emitter_interface: Arc<dyn EmitterInterface + Send + Sync>, // For compatibility with the Store trait.
    emitters: Emitters,
    span: Span,
    tracer: Arc<TracerWrapper>,
    sync_observer: Arc<crate::reactive_synchronizer::SyncObserver>,

    // --- Retry Metrics for P2P Communication ---
    retry_metrics: Arc<Mutex<RetryMetrics>>,

    // --- Lifecycle Management ---
    cancellation_token: CancellationToken,
    tasks: Mutex<JoinSet<()>>, // Adds a JoinSet to manage background tasks.
}

// We define a "type alias" for the cache to make the signature
// of the `cache()` function cleaner and more readable.
pub type CacheRef = Arc<dyn Datastore>;

// Type alias for the "guard" that points to the `index` field inside the lock.
pub type IndexGuard<'a> =
    MappedRwLockReadGuard<'a, dyn StoreIndex<Error = GuardianError> + Send + Sync>;

pub type IndexBuilder =
    Arc<dyn Fn(&[u8]) -> Box<dyn StoreIndex<Error = GuardianError> + Send + Sync>>;

// `sortFn` is a sorting function.
pub type SortFn = fn(&Entry, &Entry) -> std::cmp::Ordering;
fn default_sort_fn(a: &Entry, b: &Entry) -> std::cmp::Ordering {
    // First compare by clock time
    let time_cmp = a.clock().time().cmp(&b.clock().time());
    if time_cmp != std::cmp::Ordering::Equal {
        return time_cmp;
    }
    // If times are equal, compare by clock ID (identity)
    let id_cmp = a.clock().id().cmp(b.clock().id());
    if id_cmp != std::cmp::Ordering::Equal {
        return id_cmp;
    }
    // If clock IDs are also equal, use entry hash as final tiebreaker
    // This guarantees a total order even for entries with identical clocks
    a.hash().as_bytes().cmp(b.hash().as_bytes())
}

impl BaseStore {
    /// Creates a sled-based cache with optimized settings.
    ///
    /// This function creates a cache system using LevelDownCache (based on sled).
    /// The cache is used to:
    /// - Store local and remote heads
    /// - Cache frequently accessed entries
    /// - Keep synchronization and replication state
    /// - Optimize log query performance
    fn create_cache(address: &dyn Address, cache_dir: &str) -> Result<Arc<dyn Datastore>> {
        use crate::cache::level_down::LevelDownCache;
        use crate::cache::{Cache, CacheMode, Options};

        debug!(
            "Creating cache for address: {} in directory: {}",
            address.to_string().as_str(),
            cache_dir
        );

        // Optimized settings for the cache.
        let cache_options = Options {
            // Span for structured logging.
            span: None,
            // 100MB of cache is adequate for most use cases.
            max_cache_size: Some(100 * 1024 * 1024), // 100MB
            // Auto-detects whether to use a persistent or in-memory cache based on the environment.
            cache_mode: CacheMode::Auto,
        };

        // Create the cache manager using the Cache trait interface.
        let cache_manager = LevelDownCache::new(Some(&cache_options));

        // Prepare the address for use with the cache.
        // The address is converted to a string and re-parsed to ensure a consistent format.
        let address_string = address.to_string();

        // Use the parse function from the address module.
        let parsed_address = crate::address::parse(&address_string)
            .map_err(|e| GuardianError::Store(format!("Failed to parse address: {}", e)))?;

        // Load the cache using the configured directory.
        let boxed_datastore = cache_manager
            .load(cache_dir, &parsed_address)
            .map_err(|e| GuardianError::Store(format!("Failed to create cache: {}", e)))?;

        // Convert Box<dyn Datastore + Send + Sync> to Arc<dyn Datastore> safely,
        // using an explicit wrapper to avoid trait-object issues.
        struct DatastoreWrapper {
            inner: Box<dyn Datastore + Send + Sync>,
        }

        #[async_trait::async_trait]
        impl Datastore for DatastoreWrapper {
            async fn get(&self, key: &[u8]) -> crate::guardian::error::Result<Option<Vec<u8>>> {
                self.inner.get(key).await
            }

            async fn put(&self, key: &[u8], value: &[u8]) -> crate::guardian::error::Result<()> {
                self.inner.put(key, value).await
            }

            async fn has(&self, key: &[u8]) -> crate::guardian::error::Result<bool> {
                self.inner.has(key).await
            }

            async fn delete(&self, key: &[u8]) -> crate::guardian::error::Result<()> {
                self.inner.delete(key).await
            }

            async fn query(
                &self,
                query: &crate::data_store::Query,
            ) -> crate::guardian::error::Result<crate::data_store::Results> {
                self.inner.query(query).await
            }

            async fn list_keys(
                &self,
                prefix: &[u8],
            ) -> crate::guardian::error::Result<Vec<crate::data_store::Key>> {
                self.inner.list_keys(prefix).await
            }

            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }

        let arc_datastore: Arc<dyn Datastore> = Arc::new(DatastoreWrapper {
            inner: boxed_datastore,
        });

        info!(
            "Cache created successfully for address: {} with LevelDownCache, max_size: 100MB, mode: Auto",
            address_string.as_str()
        );

        debug!(
            "Cache configuration details - directory: {}, address_root: {}, address_path: {}",
            cache_dir,
            parsed_address.get_root(),
            parsed_address.get_path()
        );

        Ok(arc_datastore)
    }

    /// Helper method to create an access context based on the current log.
    fn create_append_context(&self) -> impl CanAppendAdditionalContext {
        // Create a snapshot of the current log entries to use as context.
        let entries = self.log_and_index.with_oplog(|oplog| {
            oplog
                .values()
                .into_iter()
                .map(|arc_entry| {
                    #[derive(Clone)]
                    struct EntryLogEntry {
                        entry: Entry,
                    }

                    impl LogEntry for EntryLogEntry {
                        fn get_payload(&self) -> &[u8] {
                            self.entry.payload()
                        }

                        fn get_identity(&self) -> &Identity {
                            self.entry.get_identity()
                        }
                    }

                    let entry = (*arc_entry).clone();
                    Box::new(EntryLogEntry { entry }) as Box<dyn LogEntry>
                })
                .collect()
        });

        CanAppendContextSnapshot { entries }
    }

    /// Returns the database (store) name.
    pub fn db_name(&self) -> &str {
        &self.db_name
    }

    /// Returns the GuardianDB Client.
    pub fn client(&self) -> Arc<IrohClient> {
        self.client.clone()
    }

    /// Returns an immutable reference to the store's identity.
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Returns a thread-safe reference to the store's OpLog.
    /// Uses the new architecture for safe access without lifetime limitations.
    pub fn op_log(&self) -> Arc<RwLock<Log>> {
        self.log_and_index.oplog.clone()
    }

    /// Helper method to get access to the oplog with a closure
    pub fn with_oplog<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Log) -> R,
    {
        self.log_and_index.with_oplog(f)
    }

    /// Helper method to get mutable access to the oplog with a closure
    pub fn with_oplog_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut Log) -> R,
    {
        self.log_and_index.with_oplog_mut(f)
    }

    /// Returns a reference to the store's access controller.
    /// The AccessController is responsible for validating read and write permissions,
    /// managing authorized keys and controlling access to the operation log.
    ///
    /// # AccessController features
    /// - Permission validation for write operations (`can_append`)
    /// - Management of authorized keys by role/capability
    /// - Identity-based access control
    /// - Persistence of access configurations
    ///
    /// # Use in Guardian-DB
    /// This controller is used mainly during:
    /// - Entry validation in the `sync()` method
    /// - Permission checks in `add_operation()`
    /// - Access control during replication
    ///
    /// # Returns
    /// An immutable reference to the store's active AccessController
    pub fn access_controller(&self) -> &dyn AccessController {
        self.access_controller.as_ref()
    }

    /// Helper methods for working with the AccessController.
    /// Checks whether an identity has permission to write to the store.
    pub async fn can_write(&self, identity: &Identity) -> bool {
        // Use the AccessController to check write permissions.
        match self.access_controller.get_authorized_by_role("write").await {
            Ok(authorized_keys) => {
                // Check whether the identity's public key is authorized.
                let identity_key = identity.pub_key();
                authorized_keys.contains(&identity_key.to_string())
                    || authorized_keys.contains(&"*".to_string()) // Universal permission.
            }
            Err(e) => {
                warn!("Failed to check write permissions: {}", e);
                false
            }
        }
    }

    /// Checks whether an identity has permission to read from the store.
    pub async fn can_read(&self, identity: &Identity) -> bool {
        match self.access_controller.get_authorized_by_role("read").await {
            Ok(authorized_keys) => {
                let identity_key = identity.pub_key();
                authorized_keys.contains(&identity_key.to_string()) ||
                authorized_keys.contains(&"*".to_string()) ||
                // If there are no specific read restrictions, allow reading if writing is allowed.
                (authorized_keys.is_empty() && self.can_write(identity).await)
            }
            Err(e) => {
                warn!("Failed to check read permissions: {}", e);
                false
            }
        }
    }

    /// Grants write permission to a specific key.
    pub async fn grant_write_access(&self, key_id: &str) -> Result<()> {
        debug!("Granting write access to key: {}", key_id);

        self.access_controller
            .grant("write", key_id)
            .await
            .map_err(|e| {
                warn!("Failed to grant write access to {}: {}", key_id, e);
                GuardianError::Store(format!("Failed to grant write access: {}", e))
            })?;

        debug!("Write access granted successfully to: {}", key_id);
        Ok(())
    }

    /// Removes write permission from a specific key.
    pub async fn revoke_write_access(&self, key_id: &str) -> Result<()> {
        debug!("Revoking write access from key: {}", key_id);

        self.access_controller
            .revoke("write", key_id)
            .await
            .map_err(|e| {
                warn!("Failed to revoke write access from {}: {}", key_id, e);
                GuardianError::Store(format!("Failed to revoke write access: {}", e))
            })?;

        debug!("Write access revoked successfully from: {}", key_id);
        Ok(())
    }

    /// Lists all keys with write permission.
    pub async fn list_write_keys(&self) -> Result<Vec<String>> {
        self.access_controller
            .get_authorized_by_role("write")
            .await
            .map_err(|e| GuardianError::Store(format!("Failed to list write keys: {}", e)))
    }

    /// Lists all keys with read permission.
    pub async fn list_read_keys(&self) -> Result<Vec<String>> {
        self.access_controller
            .get_authorized_by_role("read")
            .await
            .map_err(|e| GuardianError::Store(format!("Failed to list read keys: {}", e)))
    }

    /// Returns the AccessController type (simple, guardian, iroh, etc.).
    pub fn access_controller_type(&self) -> &str {
        self.access_controller.get_type()
    }

    /// Saves the current AccessController configuration.
    pub async fn save_access_controller(&self) -> Result<()> {
        debug!("Saving access controller configuration");

        match self.access_controller.save().await {
            Ok(_manifest) => {
                debug!("Access controller configuration saved successfully");
                Ok(())
            }
            Err(e) => {
                warn!("Failed to save access controller: {}", e);
                Err(GuardianError::Store(format!(
                    "Failed to save access controller: {}",
                    e
                )))
            }
        }
    }

    /// Returns a reference to the store's IdentityProvider.
    pub fn identity_provider(&self) -> &dyn IdentityProvider {
        self.identity_provider.as_ref()
    }

    /// ***For now, let's simplify by returning a direct reference.
    pub fn cache(&self) -> Arc<dyn Datastore> {
        self.cache.clone()
    }

    /// Returns access to the store's PubSub.
    pub fn pubsub(&self) -> Arc<dyn PubSubInterface<Error = GuardianError> + Send + Sync> {
        self.pubsub.clone()
    }

    /// Returns the store ID.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns a shared reference to the span. Used to allow
    /// multiple parts of the code to share the same tracing context.
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Returns a shared reference to the OpenTelemetry tracer.
    pub fn tracer(&self) -> Arc<TracerWrapper> {
        self.tracer.clone()
    }

    /// Returns a reference to the reactive synchronization observer.
    ///
    /// Allows external components to observe the progress of synchronization
    /// and replication operations in real time.
    pub fn sync_observer(&self) -> Arc<crate::reactive_synchronizer::SyncObserver> {
        self.sync_observer.clone()
    }

    /// Returns the retry metrics for monitoring P2P communication.
    ///
    /// Provides access to retry statistics including connection attempts,
    /// data sends, successes, and failures after all attempts.
    pub fn retry_metrics(&self) -> RetryMetrics {
        self.retry_metrics.lock().clone()
    }

    /// Logs the current retry metrics for monitoring.
    ///
    /// Includes detailed peer exchange and P2P communication metrics.
    pub fn log_retry_metrics(&self) {
        if let Some(metrics) = self.retry_metrics.try_lock() {
            debug!(
                "P2P Retry Metrics Summary:\n\
                 Connections: {}/{} ({:.1}% success)\n\
                 Sends: {}/{} ({:.1}% success)\n\
                 Peer Exchanges: {}/{} ({:.1}% success)\n\
                 Successful retries: {}\n\
                 Failed after all retries: {}\n\
                 Peer exchange timeouts: {}\n\
                 Peer exchange cancellations: {}",
                metrics.total_connection_attempts - metrics.failed_connection_attempts,
                metrics.total_connection_attempts,
                metrics.connection_success_rate(),
                metrics.total_send_attempts - metrics.failed_send_attempts,
                metrics.total_send_attempts,
                metrics.send_success_rate(),
                metrics.peer_exchange_successes,
                metrics.peer_exchange_attempts,
                metrics.peer_exchange_success_rate(),
                metrics.successful_retries,
                metrics.failed_after_all_retries,
                metrics.peer_exchange_timeouts,
                metrics.peer_exchange_cancellations
            );
        }
    }

    /// Returns a reference to the EmitterInterface for compatibility with the Store trait.
    pub fn events(&self) -> &dyn EmitterInterface {
        self.emitter_interface.as_ref()
    }

    /// Equivalent of a drop/close method.
    pub fn drop(&self) -> Result<()> {
        self.cancellation_token.cancel();
        Ok(())
    }

    /// Returns a shared reference to the event bus,
    /// allowing different parts of the system to subscribe to and emit events.
    pub fn event_bus(&self) -> Arc<EventBus> {
        self.event_bus.clone()
    }

    /// Returns a reference to the store's address.
    pub fn address(&self) -> Arc<dyn Address + Send + Sync> {
        self.address.clone()
    }

    /// Returns access to the store's active index.
    pub fn store_index(
        &self,
    ) -> Arc<RwLock<Option<Box<dyn StoreIndex<Error = GuardianError> + Send + Sync>>>> {
        self.log_and_index.active_index.clone()
    }

    /// Runs an operation with the active index if available.
    pub fn with_index<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&dyn StoreIndex<Error = GuardianError>) -> R,
    {
        self.log_and_index.with_index(f)
    }

    /// Runs a mutable operation with the active index if available.
    pub fn with_index_mut<F, R>(&self, f: F) -> Result<Option<R>>
    where
        F: FnOnce(&mut dyn StoreIndex<Error = GuardianError>) -> Result<R>,
    {
        self.log_and_index.with_index_mut(f)
    }

    /// Helper method to check whether there is an active index.
    pub fn has_active_index(&self) -> bool {
        self.log_and_index.has_active_index()
    }

    /// ***Returns a static string type (`&'static str`).
    pub fn store_type(&self) -> &'static str {
        "store"
    }

    /// Checks whether the cancellation token has been activated. This is a
    /// thread-safe, non-blocking operation.
    pub fn is_closed(&self) -> bool {
        self.cancellation_token.is_cancelled()
    }

    /// Performs the complete cleanup of the store's resources.
    #[instrument(level = "debug", skip(self))]
    pub async fn close(&self) -> Result<()> {
        if self.is_closed() {
            debug!("Store already closed, skipping close operation");
            return Ok(());
        }

        debug!("Starting BaseStore close operation");

        // Activate the token to signal the shutdown to all parts of the system.
        self.cancellation_token.cancel();
        debug!("Cancellation token activated - signaling shutdown to all components");

        // Abort all background tasks and wait for them to finish.
        debug!("Shutting down background tasks");
        {
            let mut joinset_guard = self.tasks.lock();
            joinset_guard.abort_all(); // Abort all tasks immediately.
        }

        // Wait a reasonable amount of time for the tasks to finish.
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        debug!("Background tasks shutdown completed");

        // Properly close all event emitters.
        debug!("Closing event emitters");

        // To close emitters correctly, we need to use the appropriate methods.
        // The emitters are part of the event_bus, so we disconnect the listeners.

        // For the EventWrite emitter - close the subscription if it exists.
        if let Err(e) = self.emitters.evt_write.close().await {
            warn!("Failed to close EventWrite emitter: {}", e);
        } else {
            debug!("EventWrite emitter closed successfully");
        }

        // For the EventReady emitter.
        if let Err(e) = self.emitters.evt_ready.close().await {
            warn!("Failed to close EventReady emitter: {}", e);
        } else {
            debug!("EventReady emitter closed successfully");
        }

        // For the EventReplicated emitter.
        if let Err(e) = self.emitters.evt_replicated.close().await {
            warn!("Failed to close EventReplicated emitter: {}", e);
        } else {
            debug!("EventReplicated emitter closed successfully");
        }

        // Close the cache properly if it is a type that supports closing.
        debug!("Closing cache");

        // For RedbDatastore caches, call the flush() method to compact and clean up.
        if let Some(redb_cache) = self
            .cache()
            .as_any()
            .downcast_ref::<crate::cache::RedbDatastore>()
        {
            if let Err(e) = redb_cache.flush() {
                warn!("Failed to flush RedbDatastore cache: {}", e);
            } else {
                debug!("RedbDatastore cache flushed successfully");
            }
        } else if let Some(handle) = self
            .cache()
            .as_any()
            .downcast_ref::<crate::cache::RedbDatastoreHandle>()
        {
            if let Err(e) = handle.flush() {
                warn!("Failed to flush RedbDatastoreHandle cache: {}", e);
            } else {
                debug!("RedbDatastoreHandle cache flushed successfully");
            }
        } else {
            debug!("Cache type doesn't require explicit closing - relying on Arc drop");
        }

        // Close network connections if needed.
        debug!("Closing network connections");

        // Close the direct channel if it exists.
        {
            let _channel_guard = self.direct_channel.lock().await;
            // Since Arc<dyn DirectChannel> does not allow mutability,
            // we just log the close attempt.
            debug!("Direct channel cleanup initiated - relying on Drop trait");
        }

        // Notify other components about the closing.
        debug!("Emitting store close event");

        // Emit a close event so that other components can react.
        let close_event = crate::stores::events::EventReady::new(
            self.address.clone(),
            vec![], // Empty heads indicating closing.
        );

        if let Err(e) = self.emitters.evt_ready.emit(close_event) {
            warn!("Failed to emit store close event: {}", e);
        } else {
            debug!("Store close event emitted successfully");
        }

        // Final resource cleanup.
        debug!("Performing final resource cleanup");

        // Force the release of any remaining locks.
        // This happens implicitly when the Arcs are dropped, but we can be explicit.

        debug!("BaseStore close operation completed successfully");

        Ok(())
    }

    /// Resets the store to its initial state, clearing the log, the index and the cache.
    #[instrument(level = "debug", skip(self))]
    pub async fn reset(&mut self) -> Result<()> {
        debug!("Starting BaseStore reset operation");

        // First close the store to stop all operations.
        self.close()
            .await
            .map_err(|e| GuardianError::Store(format!("unable to close store: {}", e)))?;

        // Clear the oplog by creating a new empty log.
        debug!("Clearing oplog - creating new empty log");

        // Create a new empty log using the same store settings.
        use crate::log::{AdHocAccess, LogOptions};

        let adhoc_access = AdHocAccess;
        let log_options = LogOptions {
            id: Some(&self.id),
            access: adhoc_access,
            entries: &[],
            heads: &[],
            clock: None,
            sort_fn: Some(Box::new(self.sort_fn)),
        };

        // Use the store's Client to create the empty log.
        let new_empty_log = Log::new(self.client.clone(), (*self.identity).clone(), log_options);

        // Replace the current log with the empty log using the thread-safe method.
        let _old_length = self.log_and_index.with_oplog_mut(|oplog| {
            // To fully reset the log, we replace its internal structure.
            // This effectively clears all entries, heads and log state.
            let old_length = oplog.len();
            *oplog = new_empty_log; // Use the created empty log.
            debug!("Log reset from {} entries to 0", old_length);
            old_length
        });

        debug!("Oplog successfully cleared");

        // Clear the index if it exists using the trait's clear() method.
        match self.log_and_index.with_index_mut(|index| {
            debug!("Clearing store index");

            // Call the StoreIndex trait's clear() method.
            match index.clear() {
                Ok(()) => {
                    debug!("Index successfully cleared");
                    Ok(())
                }
                Err(e) => {
                    warn!("Failed to clear index: {:?}", e);
                    Err(GuardianError::Store(format!(
                        "Failed to clear index: {:?}",
                        e
                    )))
                }
            }
        }) {
            Ok(Some(result)) => result,
            Ok(None) => {
                debug!("No active index to clear");
            }
            Err(e) => {
                warn!("Error accessing index for clearing: {:?}", e);
                return Err(GuardianError::Store(format!(
                    "Error accessing index: {:?}",
                    e
                )));
            }
        }

        // Clear the cache completely.
        debug!("Clearing all cache data");

        let cache = self.cache();

        // List of all known cache keys that should be cleared.
        let cache_keys = [
            "_localHeads",
            "_remoteHeads",
            "_allEntries",
            "queue",
            "snapshot",
            "replication_progress",
            "peers_status",
            "sync_state",
        ];

        let mut cache_errors = Vec::new();
        let mut cleared_count = 0;

        for key in &cache_keys {
            match cache.delete(key.as_bytes()).await {
                Ok(()) => {
                    cleared_count += 1;
                    debug!("Successfully cleared cache key: {}", key);
                }
                Err(e) => {
                    warn!("Failed to clear cache key '{}': {}", key, e);
                    cache_errors.push(format!("{}: {}", key, e));
                }
            }
        }

        // For caches that support a full flush, force persistence.
        if let Some(redb_cache) = cache.as_any().downcast_ref::<crate::cache::RedbDatastore>() {
            if let Err(e) = redb_cache.flush() {
                warn!("Failed to flush cache during reset: {}", e);
            } else {
                debug!("Cache successfully flushed during reset");
            }
        } else if let Some(handle) = cache
            .as_any()
            .downcast_ref::<crate::cache::RedbDatastoreHandle>()
        {
            if let Err(e) = handle.flush() {
                warn!("Failed to flush cache handle during reset: {}", e);
            } else {
                debug!("Cache handle successfully flushed during reset");
            }
        }

        debug!(
            "Cache clearing completed: {} keys cleared, {} errors",
            cleared_count,
            cache_errors.len()
        );

        // Reset the retry metrics.
        {
            let mut metrics = self.retry_metrics.lock();
            *metrics = crate::stores::base_store::RetryMetrics::new();
        }

        debug!("Retry metrics reset");

        // Emit a reset event.
        let reset_event = crate::stores::events::EventReset {
            address: self.address.clone(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };

        // Log the reset event for debugging.
        debug!(
            "Reset event created for address: {} at timestamp: {}",
            reset_event.address.to_string(),
            reset_event.timestamp
        );

        if let Err(e) = self
            .emitters
            .evt_ready
            .emit(crate::stores::events::EventReady::new(
                self.address.clone(),
                Vec::new(), // Empty heads after reset.
            ))
        {
            warn!("Failed to emit reset completion event: {}", e);
        } else {
            debug!("Reset completion event emitted successfully");
        }

        // If there were cache errors but the reset succeeded overall, log a warning.
        if !cache_errors.is_empty() {
            warn!(
                "BaseStore reset completed with cache warnings: {:?}",
                cache_errors
            );
        } else {
            debug!("BaseStore reset completed successfully - all data cleared");
        }

        Ok(())
    }

    /// This constructor is `async` because it needs to interact with the network (to obtain the NodeId)
    /// and starts background tasks. It returns an `Arc<Self>` to allow safe sharing
    /// of the `store` with the tasks it creates itself.
    #[instrument(level = "debug", skip(client, identity, address, options))]
    pub async fn new(
        client: Arc<IrohClient>,
        identity: Arc<Identity>,
        address: Arc<dyn Address + Send + Sync>,
        options: Option<NewStoreOptions>,
    ) -> Result<Arc<Self>> {
        let mut opts = options.unwrap_or_else(|| NewStoreOptions {
            event_bus: None,
            index: None,
            access_controller: None,
            cache: None,
            cache_destroy: None,
            replication_concurrency: None,
            reference_count: None,
            replicate: None,
            max_history: None,
            directory: String::new(),
            sort_fn: None,
            span: None,
            tracer: None,
            pubsub: None,
            message_marshaler: None,
            node_id: iroh::SecretKey::generate().public(),
            direct_channel: None,
            close_func: None,
            store_specific_opts: None,
            doc_ticket: None,
            read_only: None,
        });
        let cancellation_token = CancellationToken::new();

        // --- 1. Defining Defaults ---
        let span = tracing::info_span!("base_store", address = %address.to_string());
        let event_bus = opts
            .event_bus
            .take()
            .ok_or_else(|| GuardianError::Store("EventBus is a required option".to_string()))?;
        let _access_controller = match opts.access_controller.take() {
            Some(ac) => ac,
            None => {
                // If no access controller was provided, create a default SimpleAccessController.
                use std::collections::HashMap;

                let mut default_access = HashMap::new();
                default_access.insert("write".to_string(), vec!["*".to_string()]);

                Arc::new(SimpleAccessController::new(Some(default_access)))
                    as Arc<dyn AccessController>
            }
        };

        // Create an IdentityProvider based on the store's identity.
        let identity_provider =
            Arc::new(GuardianDBIdentityProvider::new()) as Arc<dyn IdentityProvider>;

        // --- 2. Creating the Components ---
        let id = address.to_string().to_string();
        let db_name = address.get_path().to_string();

        // Set 'directory' from the options or from a default.
        let directory = if opts.directory.is_empty() {
            Path::new("./GuardianDB")
                .join(&id)
                .to_str()
                .unwrap_or_default()
                .to_string()
        } else {
            opts.directory.clone()
        };

        // Set 'tracer' from the options or from a no-op tracer.
        let tracer = opts.tracer.take().unwrap_or_else(|| {
            Arc::new(TracerWrapper::Noop(
                NoopTracerProvider::new().tracer("berty.guardian-db"),
            ))
        });

        // Set 'cache' and 'cache_destroy' using the function from the `cache` module.
        // This is the correct call based on the project's structure.
        let (cache, _cache_destroy) = if let Some(cache) = opts.cache.take() {
            let _destroy = opts
                .cache_destroy
                .take()
                .unwrap_or_else(|| Box::new(|| std::result::Result::<(), Box<dyn std::error::Error + Send + Sync + 'static>>::Ok(())));
            (cache, _destroy)
        } else {
            // Use the sled-based cache implementation with an isolated directory.
            let cache_dir = Path::new(&directory).join("cache");
            let cache_dir_str = cache_dir.to_str().unwrap_or("./cache");
            let cache_impl = Self::create_cache(address.as_ref(), cache_dir_str)?;
            (
                cache_impl,
                Box::new(
                    move || -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
                        std::result::Result::<(), Box<dyn std::error::Error + Send + Sync + 'static>>::Ok(())
                    },
                )
                    as Box<
                        dyn FnOnce() -> std::result::Result<
                            (),
                            Box<dyn std::error::Error + Send + Sync + 'static>,
                        > + Send + Sync,
                    >,
            )
        };

        let sort_fn = opts.sort_fn.take().unwrap_or(default_sort_fn);
        let _index_builder = opts
            .index
            .take()
            .ok_or_else(|| GuardianError::Store("Index builder is required".to_string()))?;

        // Create the log with the appropriate settings using AdHocAccess.
        use crate::log::AdHocAccess;
        let adhoc_access = AdHocAccess; // It is a unit struct, no constructor needed.

        let log_options = LogOptions {
            id: Some(&id),
            access: adhoc_access,
            sort_fn: Some(Box::new(sort_fn)),
            entries: &[],
            heads: &[],
            clock: None,
        };

        // Use the provided Client.
        let oplog = Log::new(client.clone(), identity.as_ref().clone(), log_options);

        // Create an initial index using the provided index_builder.
        let public_key_bytes = if let Some(pk) = identity.public_key() {
            pk.to_bytes().to_vec()
        } else {
            // If the public key cannot be obtained, use the string as bytes.
            identity.pub_key().as_bytes().to_vec()
        };
        let initial_index = _index_builder(&public_key_bytes);
        let log_and_index = LogAndIndex::new(oplog, Some(initial_index));

        // Emitters need to be created from the event_bus.
        let emitters = generate_emitters(&event_bus).await?;

        // EventEmitter for compatibility with the Store trait.
        let emitter_interface =
            Arc::new(EventEmitter::default()) as Arc<dyn EmitterInterface + Send + Sync>;

        // --- 3. Building the Store ---

        // Derive the NodeId from the identity - simplified implementation.
        let node_id = if let Some(public_key) = identity.public_key() {
            // Use the Blake3 hash of the public key to derive a deterministic NodeId (consistent with Iroh).
            let key_hash = blake3::hash(&public_key.to_bytes());

            // Create a deterministic NodeId based on the hash.
            // NodeId requires exactly 32 bytes.
            let mut node_id_bytes = [0u8; 32];
            node_id_bytes.copy_from_slice(key_hash.as_bytes());

            // NodeId::from_bytes does not fail with 32 valid bytes.
            NodeId::from_bytes(&node_id_bytes).unwrap_or_else(|_| {
                warn!("Failed to create deterministic NodeId, using generated");
                iroh::SecretKey::generate().public()
            })
        } else {
            // If there is no public key, use the Blake3 hash of the identity string (consistent with Iroh).
            let id_hash = blake3::hash(identity.pub_key().as_bytes());

            let mut node_id_bytes = [0u8; 32];
            node_id_bytes.copy_from_slice(id_hash.as_bytes());

            NodeId::from_bytes(&node_id_bytes).unwrap_or_else(|_| {
                warn!("Failed to create NodeId from identity string, using generated");
                iroh::SecretKey::generate().public()
            })
        };

        let store = Arc::new(Self {
            id,
            node_id,
            identity,
            address: address.clone(),
            db_name,
            directory,
            reference_count: opts.reference_count.unwrap_or(64) as usize,
            sort_fn,
            client: client.clone(),
            access_controller: _access_controller,
            identity_provider,
            cache,
            log_and_index,
            pubsub: opts
                .pubsub
                .clone()
                .ok_or_else(|| GuardianError::Store("PubSub is required".to_string()))?,
            message_marshaler: opts
                .message_marshaler
                .clone()
                .ok_or_else(|| GuardianError::Store("MessageMarshaler is required".to_string()))?,
            direct_channel: Arc::new(tokio::sync::Mutex::new(
                opts.direct_channel
                    .take()
                    .ok_or_else(|| GuardianError::Store("DirectChannel is required".to_string()))?,
            )),
            topic: Arc::new(tokio::sync::Mutex::new(None)),
            event_bus: Arc::new(event_bus.clone()),
            emitter_interface,
            emitters,
            span,
            tracer,
            sync_observer: Arc::new(crate::reactive_synchronizer::SyncObserver::new(
                Arc::new(event_bus),
                address.clone(),
            )),
            retry_metrics: Arc::new(Mutex::new(RetryMetrics::new())),
            cancellation_token,
            tasks: Mutex::new(JoinSet::new()),
        });

        // --- 4. Starting the Events Task ---
        // Start the background task.
        let store_weak = Arc::downgrade(&store);
        store.tasks.lock().spawn(async move {
            // The task only continues while the store exists.
            while let Some(store) = store_weak.upgrade() {
                select! {
                    // Wait for cancellation.
                    _ = store.cancellation_token.cancelled() => {
                        debug!("Background task cancelled");
                        break;
                    }

                    // Process events periodically.
                    _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
                        // Check whether there is a cache that needs to be persisted and force a flush.
                        if let Some(redb_cache) = store.cache().as_any().downcast_ref::<crate::cache::RedbDatastore>() {
                            if let Err(e) = redb_cache.flush() {
                                warn!("Failed to flush cache during periodic maintenance: {}", e);
                            } else {
                                debug!("Cache successfully flushed during periodic maintenance");
                            }
                        } else if let Some(handle) = store.cache().as_any().downcast_ref::<crate::cache::RedbDatastoreHandle>() {
                            if let Err(e) = handle.flush() {
                                warn!("Failed to flush cache handle during periodic maintenance: {}", e);
                            } else {
                                debug!("Cache handle successfully flushed during periodic maintenance");
                            }
                        } else {
                            debug!("Cache type doesn't support direct flushing");
                        }

                        // Update the index statistics if needed.
                        if store.has_active_index()
                            && let Err(e) = store.update_index() {
                                warn!("Failed to update index in background: {}", e);
                            }
                    }
                }
            }
        });

        // --- 5. Finalization ---
        if opts.replicate.unwrap_or(true) {
            // Start the replication logic.
            debug!("Initiating store replication");

            // Clone the store to use in replication.
            let store_for_replication = store.clone();

            // Spawn replication in a separate task so as not to block store creation.
            tokio::spawn(async move {
                if let Err(e) = store_for_replication.replicate().await {
                    error!("Failed to start replication: {:?}", e);
                } else {
                    debug!("Store replication started successfully");
                }
            });
        } else {
            debug!("Replication disabled by configuration");
        }

        Ok(store)
    }

    /// Returns the pointer to the sorting function used by the OpLog.
    pub fn sort_fn(&self) -> SortFn {
        self.sort_fn
    }

    /// Updates the store's index based on the current OpLog state.
    pub fn update_index(&self) -> Result<usize> {
        // Create a span for performance tracking.
        let _span = self.tracer.start_span("update-index");

        // Use the thread-safe method of the new architecture.
        match self.log_and_index.update_index_safe() {
            Ok(count) => {
                if count > 0 {
                    debug!("Index updated successfully with {} entries", count);
                } else {
                    warn!("No active index to update");
                }
                Ok(count)
            }
            Err(e) => {
                error!("Failed to update index: {:?}", e);
                Err(e)
            }
        }
    }

    /// Loads additional entries into the store, processing them directly into the OpLog.
    pub fn load_more_from(&self, entries: Vec<Entry>) -> Result<usize> {
        if entries.is_empty() {
            return Ok(0);
        }

        debug!("Loading {} additional entries", entries.len());

        // Process the entries directly into the OpLog.
        let added_count = self.log_and_index.with_oplog_mut(|oplog| {
            let mut count = 0;
            for (i, entry) in entries.iter().enumerate() {
                // Check whether the entry already exists.
                if !oplog.has(entry.hash()) {
                    // For existing entries, we join instead of append.
                    // Create a temporary log with the entry and join it.
                    match self.create_temporary_log_with_entry(entry) {
                        Ok(temp_log) => {
                            if oplog.join(&temp_log, None).is_some() {
                                count += 1;
                                debug!("Successfully joined entry {}", entry.hash());

                                // Emit progress via SyncObserver synchronously
                                // (using block_on since we are in a synchronous context).
                                tokio::task::block_in_place(|| {
                                    tokio::runtime::Handle::current().block_on(async {
                                        self.sync_observer
                                            .emit_progress(
                                                *entry.hash(),
                                                entry.clone(),
                                                i + 1,
                                                entries.len(),
                                            )
                                            .await;
                                    });
                                });
                            } else {
                                warn!("Failed to join entry {}: join returned None", entry.hash());
                            }
                        }
                        Err(e) => {
                            warn!(
                                "Failed to create temporary log for entry {}: {}",
                                entry.hash(),
                                e
                            );
                        }
                    }
                }
            }
            count
        });

        // Update the index if entries were added.
        if added_count > 0 {
            self.update_index()?;

            // Emit replication events.
            for entry in &entries {
                // Emit via SyncObserver.
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        self.sync_observer.emit_replicated(*entry.hash()).await;
                    });
                });
            }

            // Emit the legacy replication event for loaded entries.
            let log_length = self.log_and_index.with_oplog(|oplog| oplog.len());
            let event = EventReplicated {
                address: self.address.clone(),
                log_length,
                entries: entries.clone(),
            };
            if let Err(e) = self.emitters.evt_replicated.emit(event) {
                warn!("Failed to emit replicated event: {}", e);
            }

            debug!("Successfully loaded {} new entries", added_count);
        }

        Ok(added_count)
    }

    /// Helper method to create a temporary log with a specific entry.
    fn create_temporary_log_with_entry(&self, entry: &Entry) -> Result<Log> {
        // Create a temporary log containing only the specified entry.
        debug!("Creating temporary log with entry hash: {}", entry.hash());

        // Convert the entry into Arc<Entry> as expected by LogOptions.
        let arc_entry = Arc::new(entry.clone());
        let entries_slice = std::slice::from_ref(&arc_entry);

        // Use the same store ID so that oplog.join() accepts the merge.
        // (Log::join returns None when self.id != other.id)
        let log_options = LogOptions::new()
            .id(&self.id)
            .entries(entries_slice)
            .heads(entries_slice) // The entry is also a head in this temporary log.
            .sort_fn(self.sort_fn); // Use the same sorting function as the store.

        // Use the store's Client for temporary logs.
        let client = self.client.clone();

        // Create the temporary log using the store's identity.
        let temp_log = Log::new(
            client,
            (*self.identity).clone(), // Dereference the Arc<Identity>.
            log_options,
        );

        debug!(
            "Successfully created temporary log '{}' with {} entries",
            self.id,
            temp_log.len()
        );

        Ok(temp_log)
    }

    /// Processes a list of "heads" received from other peers, validating
    /// access and persisting them to Iroh before queueing them for loading.
    /// The function is `async` due to the write call to Iroh via `Entry`.
    #[instrument(level = "debug", skip(self, heads))]
    pub async fn sync(&self, heads: Vec<Entry>) -> Result<()> {
        if heads.is_empty() {
            return Ok(());
        }

        let mut verified_heads = vec![];

        debug!("Sync: Processing {} heads", heads.len());

        // Emit a start event via SyncObserver.
        self.sync_observer.emit_started(heads.len()).await;

        for head in heads {
            // Basic validation: check that the head is not empty.
            let empty_hash = Hash::from([0u8; 32]);
            if head.hash() == &empty_hash || head.payload().is_empty() {
                debug!("Sync: head discarded (invalid data)");
                continue;
            }

            // Create a new context for each iteration to avoid borrow issues.
            let head_ac_context = self.create_append_context();

            // Use the store's IdentityProvider for access validation.
            let identity_provider = &self.identity_provider;

            // Access validation using the access_controller.
            if let Err(e) = self
                .access_controller
                .can_append(&head, identity_provider.as_ref(), &head_ac_context)
                .await
            {
                debug!("Sync: head discarded (no write access): {}", e);
                continue;
            }

            // Check whether the entry is already in Iroh or needs to be stored.
            let hash = head.hash();

            // Hash integrity validation - for now, we just check that it is not empty.
            let empty_hash = Hash::from([0u8; 32]);
            if hash == &empty_hash {
                debug!("Sync: head discarded (empty hash)");
                continue;
            }

            // Check whether we already have this entry in the oplog.
            let already_exists = self.log_and_index.with_oplog(|oplog| oplog.has(hash));

            if already_exists {
                debug!("Sync: head already exists in oplog");
                continue;
            }

            verified_heads.push(head);
        }

        if verified_heads.is_empty() {
            debug!("Sync: no new heads to process");
            // Emit ready even without processing.
            self.sync_observer.emit_ready(Vec::new()).await;
            return Ok(());
        }

        // Process the verified `heads` directly into the OpLog.
        debug!("Processing {} heads directly", verified_heads.len());

        // Add the entries to the oplog using add_entry (preserves the original hash).
        // Collect progress information for emission OUTSIDE the lock.
        let (added_count, progress_info) = self.log_and_index.with_oplog_mut(|oplog| {
            let mut count = 0;
            let mut progress = Vec::new();
            for (i, head) in verified_heads.iter().enumerate() {
                // Use add_entry to preserve the entry's original hash.
                if oplog.add_entry(head.clone()) {
                    count += 1;
                    debug!("Sync: added entry with hash {:?}", head.hash());
                } else {
                    debug!("Sync: entry already exists in oplog, skipping");
                }

                progress.push((*head.hash(), head.clone(), i + 1, verified_heads.len()));
            }
            (count, progress)
        });

        // Emit progress OUTSIDE the oplog lock to avoid block_in_place inside a lock.
        for (hash, head, current, total) in progress_info {
            self.sync_observer
                .emit_progress(hash, head, current, total)
                .await;
        }

        // Update the index if entries were added.
        if added_count > 0 {
            if let Err(e) = self.update_index() {
                warn!("Failed to update index after sync: {}", e);
                // Emit an error via SyncObserver.
                self.sync_observer
                    .emit_error(format!("Failed to update index: {}", e))
                    .await;
            } else {
                debug!("Sync completed: processed {} new heads", added_count);
                // Emit ready with the processed heads.
                self.sync_observer.emit_ready(verified_heads.clone()).await;
            }

            // Persist all oplog entries to the cache after sync.
            let all_entries = self.with_oplog(|oplog| {
                oplog
                    .values()
                    .iter()
                    .map(|arc_entry| (**arc_entry).clone())
                    .collect::<Vec<Entry>>()
            });
            if let Ok(all_entries_bytes) = crate::guardian::serializer::serialize(&all_entries) {
                let cache = self.cache();
                if let Err(e) = cache
                    .put("_allEntries".as_bytes(), &all_entries_bytes)
                    .await
                {
                    warn!("Failed to cache all entries after sync: {}", e);
                }
            }
        }

        Ok(())
    }

    /// The main method for adding data to the store. It serializes the operation,
    /// appends it to the OpLog, updates the index and the cache, and emits an event.
    #[instrument(level = "debug", skip(self, op, on_progress))]
    pub async fn add_operation(
        &self,
        op: Operation,
        on_progress: Option<mpsc::Sender<Entry>>,
    ) -> Result<Entry> {
        let data = op
            .marshal()
            .map_err(|e| GuardianError::Store(format!("Unable to marshal operation: {}", e)))?;

        // Use the new thread-safe architecture to add the entry.
        // IMPORTANT: Use base64 to preserve binary data when storing as a string.
        let new_entry = self.log_and_index.with_oplog_mut(|oplog| {
            use base64::{Engine as _, engine::general_purpose};
            let data_str = general_purpose::STANDARD.encode(&data);
            let entry = oplog.append(&data_str, Some(self.reference_count)).clone();
            debug!(
                "[ADD_OPERATION] After append: oplog.len()={}, oplog.heads().len()={}",
                oplog.len(),
                oplog.heads().len()
            );
            entry
        });

        // Update the index using the new architecture.
        self.update_index()
            .map_err(|e| GuardianError::Store(format!("Unable to update index: {}", e)))?;

        // Save the local heads to the cache using thread-safe access.
        let heads = self.with_oplog(|oplog| {
            let heads_vec = oplog
                .heads()
                .into_iter()
                .map(|arc_entry| {
                    // Since oplog.heads() returns Vec<Arc<Entry>>, we clone the Arc.
                    (*arc_entry).clone()
                })
                .collect::<Vec<Entry>>();
            debug!(
                "[ADD_OPERATION] Heads collected for cache: {} heads",
                heads_vec.len()
            );
            heads_vec
        });

        let local_heads_bytes = crate::guardian::serializer::serialize(&heads).map_err(|e| {
            GuardianError::Store(format!(
                "Failed to serialize local heads for caching: {}",
                e
            ))
        })?;

        let cache = self.cache();
        cache
            .put("_localHeads".as_bytes(), &local_heads_bytes)
            .await
            .map_err(|e| GuardianError::Store(format!("Failed to cache local heads: {}", e)))?;

        // Save ALL oplog entries to the cache for full persistence.
        let all_entries = self.with_oplog(|oplog| {
            oplog
                .values()
                .iter()
                .map(|arc_entry| (**arc_entry).clone())
                .collect::<Vec<Entry>>()
        });
        let all_entries_bytes =
            crate::guardian::serializer::serialize(&all_entries).map_err(|e| {
                GuardianError::Store(format!(
                    "Failed to serialize all entries for caching: {}",
                    e
                ))
            })?;
        cache
            .put("_allEntries".as_bytes(), &all_entries_bytes)
            .await
            .map_err(|e| GuardianError::Store(format!("Failed to cache all entries: {}", e)))?;

        // Emit a write event.
        let write_event = EventWrite {
            address: self.address.clone(),
            entry: new_entry.clone(),
            heads: heads.clone(),
        };

        self.emitters
            .evt_write
            .emit(write_event)
            .unwrap_or_else(|_| {
                warn!("Unable to emit write event");
            });

        if let Some(callback) = on_progress {
            callback.send(new_entry.clone()).await.ok();
        }

        Ok(new_entry)
    }

    /// Starts the replication logic, subscribing to the pubsub topic and
    /// initializing the internal and external event listeners.
    pub async fn replicate(self: &Arc<Self>) -> Result<()> {
        debug!("Starting replication for store: {}", self.id);

        // --- 1. CREATE THE PUBSUB TOPIC ---
        // **IMPORTANT**: Use only the log name (without the DB hash) so that
        // different nodes can share the same replication topic.
        let shared_topic_name = self.extract_log_name();
        debug!(
            "Creating pubsub topic for store replication: {} (from full id: {})",
            shared_topic_name, self.id
        );

        // **Since PubSubInterface::topic_subscribe requires &mut self, but we have Arc<dyn PubSubInterface>,
        // we use a concrete-type-based approach when available.
        let topic = if let Some(core_api_pubsub) = self
            .pubsub
            .as_ref()
            .as_any()
            .downcast_ref::<std::sync::Arc<crate::p2p::messaging::CoreApiPubSub>>()
        {
            // Use the internal method that works with &self.
            debug!("Using CoreApiPubSub for topic subscription");
            core_api_pubsub
                .topic_subscribe_internal(&shared_topic_name)
                .await?
        } else if let Some(epidemic_pubsub) =
            self.pubsub
                .as_ref()
                .as_any()
                .downcast_ref::<crate::p2p::network::core::gossip::EpidemicPubSub>()
        {
            // Use EpidemicPubSub directly for replication.
            debug!("Using EpidemicPubSub for topic subscription");
            epidemic_pubsub.topic_subscribe(&shared_topic_name).await?
        } else {
            return Err(GuardianError::Store(
                "Unknown PubSub implementation type".to_string(),
            ));
        };

        debug!(
            "Successfully created topic '{}' for replication",
            topic.topic()
        );

        // Store the topic in the struct field for later use.
        *self.topic.lock().await = Some(topic.clone());

        // --- 2. SET UP LISTENERS FOR WRITE EVENTS ---
        debug!("Setting up store write event listener");
        if let Err(e) = self.store_listener(topic.clone()) {
            error!("Failed to start store listener: {:?}", e);
            return Err(GuardianError::Store(format!(
                "Failed to configure write event listener: {}",
                e
            )));
        }

        // --- 3. SET UP LISTENERS FOR PEER EVENTS ---
        debug!("Setting up pubsub peer event listener");
        if let Err(e) = self.pubsub_chan_listener(topic.clone()) {
            error!("Failed to start pubsub listener: {:?}", e);
            return Err(GuardianError::Store(format!(
                "Failed to configure peer event listener: {}",
                e
            )));
        }

        // --- 4. SET UP LISTENER FOR INCOMING GOSSIP MESSAGES ---
        debug!("Setting up pubsub message listener");
        if let Err(e) = self.pubsub_message_listener(topic.clone()) {
            error!("Failed to start message listener: {:?}", e);
            return Err(GuardianError::Store(format!(
                "Failed to configure message listener: {}",
                e
            )));
        }

        // --- 5. START SYNCHRONIZATION WITH EXISTING PEERS ---
        debug!("Starting synchronization with existing peers");

        // Get peers already connected to the topic.
        match topic.peers().await {
            Ok(existing_peers) => {
                debug!(
                    "Found {} existing peers in topic: {:?}",
                    existing_peers.len(),
                    existing_peers
                );

                // Start a head exchange with each existing peer.
                for peer in existing_peers {
                    if peer != self.node_id {
                        debug!("Initiating head exchange with existing peer: {:?}", peer);

                        let store_clone = self.clone();

                        // Spawn a task for the asynchronous exchange.
                        tokio::spawn(async move {
                            match store_clone.on_new_peer_joined(peer).await {
                                Ok(()) => {
                                    debug!(
                                        "Successfully synchronized with existing peer: {:?}",
                                        peer
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to synchronize with existing peer {:?}: {:?}",
                                        peer, e
                                    );
                                }
                            }
                        });
                    }
                }
            }
            Err(e) => {
                warn!("Failed to get existing peers from topic: {:?}", e);
            }
        }

        // --- 5. CONFIGURE METRICS AND MONITORING ---
        debug!("Configuring replication metrics");

        // Record that replication has started.
        if let Some(_metrics) = self.retry_metrics.try_lock() {
            // Replication-specific metrics can be added here.
            debug!("Replication metrics initialized");
        }

        // --- 6. FINALIZATION ---
        debug!("Replication started successfully for store: {}", self.id);

        // Emit an event indicating that replication is ready.
        let current_heads = self.with_oplog(|oplog| {
            oplog
                .heads()
                .iter()
                .map(|arc_entry| (**arc_entry).clone())
                .collect::<Vec<Entry>>()
        });

        let ready_event =
            crate::stores::events::EventReady::new(self.address.clone(), current_heads);

        if let Err(e) = self.emitters.evt_ready.emit(ready_event) {
            warn!("Failed to emit replication ready event: {}", e);
        } else {
            debug!("Replication ready event emitted successfully");
        }

        Ok(())
    }

    /// Starts a background task that listens for write events (`EventWrite`)
    /// on the internal event bus. For each event, another task is started
    /// to propagate the update to the network via pubsub.
    fn store_listener(
        self: &Arc<Self>,
        topic: Arc<dyn PubSubTopic<Error = GuardianError> + Send + Sync>,
    ) -> Result<()> {
        let store_weak = Arc::downgrade(self);
        let cancellation_token = self.cancellation_token.clone();
        let event_bus = self.event_bus.clone();

        tokio::spawn(async move {
            // Create the subscriber inside the async task.
            let mut sub = match event_bus.subscribe::<EventWrite>().await {
                Ok(sub) => sub,
                Err(e) => {
                    // Log error if possible.
                    eprintln!("Failed to subscribe to EventWrite: {:?}", e);
                    return;
                }
            };

            loop {
                // `select!` waits for either a new event or the store's cancellation.
                select! {
                    _ = cancellation_token.cancelled() => break,
                    Ok(event) = sub.recv() => {
                        // Try to "promote" the weak reference to a strong one.
                        if let Some(store) = store_weak.upgrade() {
                            let topic_clone = topic.clone();
                            let store_clone = store.clone(); // Clone the Arc to move into the task.
                            // Start the task inside the store's JoinSet for proper management.
                            store.tasks.lock().spawn(async move {
                                if let Err(_e) = store_clone.handle_event_write(event, topic_clone).await {
                                    warn!("unable to handle EventWrite");
                                }
                            });
                        } else {
                            // The store was dropped, so the task should end.
                            break;
                        }
                    }
                }
            }
        });

        Ok(())
    }

    /// Spawns a task to handle peer join/leave events from PubSub.
    fn pubsub_chan_listener(
        self: &Arc<Self>,
        topic: Arc<dyn PubSubTopic<Error = GuardianError> + Send + Sync>,
    ) -> Result<()> {
        let store_weak = Arc::downgrade(self);
        let cancellation_token = self.cancellation_token.clone();

        tokio::spawn(async move {
            // Use the PubSubTopic's watch_peers() for events.
            debug!(
                "Starting pubsub peer events listener for topic: {}",
                topic.topic()
            );

            // Get the topic's peer events stream.
            let peer_events_stream = match topic.watch_peers().await {
                Ok(stream) => stream,
                Err(e) => {
                    error!("Failed to create peer events stream: {:?}", e);
                    return;
                }
            };

            use futures::StreamExt;
            let mut peer_events = peer_events_stream;

            loop {
                select! {
                    _ = cancellation_token.cancelled() => {
                        debug!("Pubsub peer listener cancelled");
                        break;
                    }
                    // Process peer events.
                    peer_event = peer_events.next() => {
                        match peer_event {
                            Some(event) => {
                                // Convert the Arc<dyn Any> into an EventPubSub.
                                if let Some(pubsub_event) = event.downcast_ref::<crate::traits::EventPubSub>() {
                                    if let Some(store_arc) = store_weak.upgrade() {
                                        debug!(
                                            "Processing peer event: {:?}",
                                            match pubsub_event {
                                                crate::traits::EventPubSub::Join { peer, topic } =>
                                                    format!("Join(peer: {:?}, topic: {})", peer, topic),
                                                crate::traits::EventPubSub::Leave { peer, topic } =>
                                                    format!("Leave(peer: {:?}, topic: {})", peer, topic),
                                            }
                                        );
                                        // Processa o evento usando o handler existente
                                        store_arc.handle_peer_event(pubsub_event.clone()).await;
                                    } else {
                                        debug!("Store dropped, ending pubsub peer listener");
                                        break;
                                    }
                                } else {
                                    warn!("Received unknown peer event type");
                                }
                            }
                            None => {
                                debug!("Peer events stream ended");
                                break;
                            }
                        }
                    }
                }
            }
        });
        Ok(())
    }
    /// Helper function for 'pubsub_chan_listener'.
    /// Handles a single peer join or leave event.
    /// Processes events with retry and robust error handling.
    async fn handle_peer_event(self: Arc<Self>, event: crate::traits::EventPubSub) {
        match event {
            crate::traits::EventPubSub::Join {
                topic: _,
                peer: node_id,
            } => {
                debug!(
                    "Peer joined event received: {:?} on topic: {}",
                    node_id, self.id
                );

                // **CRITICAL FIX**: When we receive a Join from a peer, we must also
                // call join_peers() to ensure a BIDIRECTIONAL connection.
                // Without this, Node A can send to Node B, but Node B cannot
                // receive because it has not established the connection on its end.
                let topic_name = self.extract_log_name();
                debug!(
                    "[BIDIRECTIONAL_MESH] Establishing bidirectional connection with peer {:?} for topic {}",
                    node_id, topic_name
                );

                if let Some(epidemic_pubsub) =
                    self.pubsub
                        .as_ref()
                        .as_any()
                        .downcast_ref::<crate::p2p::network::core::gossip::EpidemicPubSub>()
                {
                    if let Err(e) = epidemic_pubsub
                        .subscribe_with_peers(&topic_name, vec![node_id])
                        .await
                    {
                        warn!(
                            "[BIDIRECTIONAL_MESH] Failed to establish bidirectional connection: {}",
                            e
                        );
                    } else {
                        debug!(
                            "[BIDIRECTIONAL_MESH] Successfully established bidirectional connection with {:?}",
                            node_id
                        );
                    }
                } else if let Some(core_api_pubsub) = self
                    .pubsub
                    .as_ref()
                    .as_any()
                    .downcast_ref::<std::sync::Arc<crate::p2p::messaging::CoreApiPubSub>>()
                {
                    if let Err(e) = core_api_pubsub
                        .epidemic_pubsub
                        .subscribe_with_peers(&topic_name, vec![node_id])
                        .await
                    {
                        warn!(
                            "[BIDIRECTIONAL_MESH] Failed to establish bidirectional connection (CoreApiPubSub): {}",
                            e
                        );
                    } else {
                        debug!(
                            "[BIDIRECTIONAL_MESH] Successfully established bidirectional connection with {:?} (CoreApiPubSub)",
                            node_id
                        );
                    }
                }

                // Wait a bit for the mesh to stabilize bidirectionally.
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                // Emit a NewPeer event to the system using the correct type.
                let new_peer_event = crate::stores::events::EventNewPeer::new(node_id);
                match self
                    .event_bus
                    .emitter::<crate::stores::events::EventNewPeer>()
                    .await
                {
                    Ok(emitter) => {
                        if let Err(e) = emitter.emit(new_peer_event) {
                            warn!("Failed to emit EventNewPeer: {}", e);
                        } else {
                            debug!("Successfully emitted EventNewPeer for: {:?}", node_id);
                        }
                    }
                    Err(e) => {
                        error!("Failed to get event emitter for EventNewPeer: {}", e);
                    }
                }

                // Start a head exchange with robust retry.
                let store_clone = self.clone(); // Clone the Arc<Self>.

                tokio::spawn(async move {
                    debug!("Starting head exchange with peer: {:?}", node_id);

                    // Call the implemented head-exchange-with-retry method.
                    match store_clone.on_new_peer_joined(node_id).await {
                        Ok(()) => {
                            debug!(
                                "Successfully completed head exchange with peer: {:?}",
                                node_id
                            );
                        }
                        Err(e) => {
                            warn!(
                                "Failed to complete head exchange with peer {:?}: {:?}",
                                node_id, e
                            );
                        }
                    }
                });
            }
            crate::traits::EventPubSub::Leave {
                topic: _,
                peer: node_id,
            } => {
                debug!(
                    "Peer left event received: {:?} from topic: {}",
                    node_id, self.id
                );

                // Process the peer leaving.
                // Record disconnected-peer metrics.
                if let Some(mut metrics) = self.retry_metrics.try_lock() {
                    metrics.record_peer_disconnection();
                }

                // Emit a PeerDisconnected event using the available type.
                let peer_disconnect_event = crate::guardian::core::EventPeerDisconnected {
                    node_id: node_id.to_string(),
                    address: self.id.clone(),
                };
                if let Ok(emitter) = self
                    .event_bus
                    .emitter::<crate::guardian::core::EventPeerDisconnected>()
                    .await
                    && let Err(e) = emitter.emit(peer_disconnect_event)
                {
                    warn!("Failed to emit EventPeerDisconnected: {}", e);
                }
            }
        }
    }

    /// Spawns a task to handle incoming gossip messages from PubSub.
    fn pubsub_message_listener(
        self: &Arc<Self>,
        topic: Arc<dyn PubSubTopic<Error = GuardianError> + Send + Sync>,
    ) -> Result<()> {
        let store_weak = Arc::downgrade(self);
        let cancellation_token = self.cancellation_token.clone();

        tokio::spawn(async move {
            debug!(
                "Starting pubsub message listener for topic: {}",
                topic.topic()
            );

            // Get the topic's message stream.
            let message_stream = match topic.watch_messages().await {
                Ok(stream) => {
                    debug!("[✅ Message stream created successfully]");
                    stream
                }
                Err(e) => {
                    error!("Failed to create message stream: {:?}", e);
                    return;
                }
            };

            use futures::StreamExt;
            let mut messages = message_stream;

            debug!("[📬 Message listener loop starting]");

            loop {
                select! {
                    _ = cancellation_token.cancelled() => {
                        debug!("Pubsub message listener cancelled");
                        break;
                    }
                    // Process incoming messages.
                    message_event = messages.next() => {
                        match message_event {
                            Some(event) => {
                                debug!("[📬 Loop iteration - received event from gossip]");
                                if let Some(store_arc) = store_weak.upgrade() {
                                    debug!(
                                        "[📨 Gossip message received] {} bytes on topic {}",
                                        event.content.len(),
                                        store_arc.id
                                    );

                                    // Deserialize MessageExchangeHeads.
                                    match store_arc.message_marshaler.unmarshal(&event.content) {
                                        Ok(msg) => {
                                            debug!(
                                                "[🔄 Synchronizing] Received {} heads from address: {} (expected: {})",
                                                msg.heads.len(),
                                                msg.address,
                                                store_arc.id
                                            );

                                            // Process the received heads.
                                            if let Err(e) = store_arc.sync(msg.heads).await {
                                                error!("Failed to sync received heads: {}", e);
                                            } else {
                                                debug!("[✅ Sync complete] Heads processed successfully");
                                            }
                                        }
                                        Err(e) => {
                                            warn!("Failed to unmarshal gossip message: {}", e);
                                        }
                                    }
                                } else {
                                    debug!("Store dropped, ending pubsub message listener");
                                    break;
                                }
                            }
                            None => {
                                debug!("Message stream ended");
                                break;
                            }
                        }
                    }
                }
            }
        });
        Ok(())
    }

    /// Publishes the most recent "heads" from a local write to all
    /// peers connected to the pubsub topic.
    pub async fn handle_event_write(
        &self,
        event: EventWrite,
        topic: Arc<dyn PubSubTopic<Error = GuardianError> + Send + Sync>,
    ) -> Result<()> {
        debug!("received stores.write event");

        if event.heads.is_empty() {
            return Err(GuardianError::Store("'heads' are not defined".to_string()));
        }

        let topic_peers = match topic.peers().await {
            Ok(peers) => peers,
            Err(e) => {
                return Err(GuardianError::Store(format!(
                    "Failed to get topic peers: {:?}",
                    e
                )));
            }
        };
        if topic_peers.is_empty() {
            debug!("no peers in pubsub topic, skipping publish");
            return Ok(());
        }

        let msg = MessageExchangeHeads {
            address: self.id.clone(),
            heads: event.heads,
        };

        let payload = match self.message_marshaler.marshal(&msg) {
            Ok(payload) => payload,
            Err(e) => {
                return Err(GuardianError::Store(format!(
                    "unable to serialize heads: {:?}",
                    e
                )));
            }
        };

        topic.publish(payload).await.map_err(|e| {
            GuardianError::Store(format!("unable to publish message on pubsub: {}", e))
        })?;
        debug!("stores.write event: published event on pub sub");

        Ok(())
    }

    /// Starts the "heads" exchange with a newly connected peer.
    /// Includes retry, timeout and cancellation strategies.
    pub async fn on_new_peer_joined(&self, peer: NodeId) -> Result<()> {
        debug!(
            "{:?}: New peer '{:?}' connected to {}",
            self.node_id, peer, self.id
        );

        // **CRITICAL FIX**: Use the simplified log name (without hash) to ensure
        // that all peers use the SAME TopicId for the same log.
        let shared_topic_name = self.extract_log_name();

        // **CRITICAL SOLUTION**: Add the peer to the gossip mesh by re-subscribing with it as bootstrap.
        // This allows iroh-gossip to form a mesh between the peers.
        debug!(
            "[GOSSIP_MESH] Adding peer {:?} to gossip mesh for topic {}",
            peer, shared_topic_name
        );

        if let Some(core_api_pubsub) = self
            .pubsub
            .as_ref()
            .as_any()
            .downcast_ref::<std::sync::Arc<crate::p2p::messaging::CoreApiPubSub>>()
        {
            // Re-subscribe with the peer as bootstrap to form a mesh.
            // FIX: Use shared_topic_name instead of self.id to ensure the same TopicId.
            if let Err(e) = core_api_pubsub
                .epidemic_pubsub
                .get_or_create_topic_with_peers(&shared_topic_name, vec![peer])
                .await
            {
                warn!("[GOSSIP_MESH] Failed to add peer to gossip mesh: {}", e);
                // Does not fail - continues with exchange_heads even without a mesh.
            } else {
                debug!(
                    "[GOSSIP_MESH] Successfully added peer {:?} to gossip mesh",
                    peer
                );
            }
        } else if let Some(epidemic_pubsub) =
            self.pubsub
                .as_ref()
                .as_any()
                .downcast_ref::<crate::p2p::network::core::gossip::EpidemicPubSub>()
        {
            // FIX: Use shared_topic_name instead of self.id to ensure the same TopicId.
            if let Err(e) = epidemic_pubsub
                .get_or_create_topic_with_peers(&shared_topic_name, vec![peer])
                .await
            {
                warn!(
                    "[GOSSIP_MESH] Failed to add peer to gossip mesh (EpidemicPubSub): {}",
                    e
                );
            } else {
                debug!(
                    "[GOSSIP_MESH] Successfully added peer {:?} to gossip mesh (EpidemicPubSub)",
                    peer
                );
            }
        }

        // Small delay to allow the mesh to form.
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Robust retry and error-handling strategies.
        const MAX_PEER_EXCHANGE_RETRIES: u32 = 3;
        const PEER_EXCHANGE_TIMEOUT_SECS: u64 = 30;
        const PEER_EXCHANGE_BASE_DELAY_MS: u64 = 200;

        let mut retry_attempt = 0;
        let mut last_error = None;

        while retry_attempt <= MAX_PEER_EXCHANGE_RETRIES {
            retry_attempt += 1;

            // Create a timeout for the operation.
            let exchange_future = self.exchange_heads(peer);
            let timeout_duration = std::time::Duration::from_secs(PEER_EXCHANGE_TIMEOUT_SECS);

            match tokio::time::timeout(timeout_duration, exchange_future).await {
                Ok(Ok(())) => {
                    debug!(
                        "Successfully exchanged heads with peer {:?} on attempt {}",
                        peer, retry_attempt
                    );

                    // Record success metrics.
                    if let Some(mut metrics) = self.retry_metrics.try_lock() {
                        metrics.record_peer_exchange_success();
                    }

                    return Ok(());
                }
                Ok(Err(e)) => {
                    // Application error - analyze the error type to decide on a retry.
                    last_error = Some(e.clone());

                    match &e {
                        GuardianError::Store(msg) if msg.contains("cancelled") => {
                            // Cancellation error - do not retry.
                            warn!(
                                "Peer exchange with {:?} was cancelled, not retrying: {}",
                                peer, msg
                            );
                            return Err(e);
                        }
                        GuardianError::Store(msg) if msg.contains("timeout") => {
                            // Timeout error - may be temporary, retry.
                            warn!(
                                "Peer exchange with {:?} timed out (attempt {}): {}",
                                peer, retry_attempt, msg
                            );
                        }
                        GuardianError::Store(msg) if msg.contains("connection") => {
                            // Connection error - may be temporary, retry.
                            warn!(
                                "Connection error with peer {:?} (attempt {}): {}",
                                peer, retry_attempt, msg
                            );
                        }
                        GuardianError::Store(msg) if msg.contains("marshal") => {
                            // Serialization error - permanent, do not retry.
                            error!("Marshal error with peer {:?}, not retrying: {}", peer, msg);
                            return Err(e);
                        }
                        _ => {
                            // Other errors - try a limited retry.
                            warn!(
                                "Generic error with peer {:?} (attempt {}): {:?}",
                                peer, retry_attempt, e
                            );
                        }
                    }
                }
                Err(_) => {
                    // Timeout of the entire operation.
                    let timeout_error = GuardianError::Store(format!(
                        "Peer exchange with {:?} timed out after {} seconds",
                        peer, PEER_EXCHANGE_TIMEOUT_SECS
                    ));
                    last_error = Some(timeout_error.clone());

                    warn!(
                        "Peer exchange with {:?} timed out (attempt {}/{})",
                        peer,
                        retry_attempt,
                        MAX_PEER_EXCHANGE_RETRIES + 1
                    );
                }
            }

            // Record failure metrics.
            if let Some(mut metrics) = self.retry_metrics.try_lock() {
                metrics.record_peer_exchange_failure();
            }

            // If this is not the last attempt, wait before retrying.
            if retry_attempt <= MAX_PEER_EXCHANGE_RETRIES {
                // Exponential backoff with jitter to avoid a thundering herd.
                let delay_ms = PEER_EXCHANGE_BASE_DELAY_MS * (1 << (retry_attempt - 1));
                let jitter = fastrand::u64(0..=delay_ms / 4); // Up to 25% jitter.
                let total_delay = delay_ms + jitter;

                debug!(
                    "Retrying peer exchange with {:?} in {}ms (attempt {}/{})",
                    peer,
                    total_delay,
                    retry_attempt + 1,
                    MAX_PEER_EXCHANGE_RETRIES + 1
                );

                // Check whether the store was cancelled during the delay.
                select! {
                    _ = self.cancellation_token.cancelled() => {
                        warn!(

                            "Store cancelled during peer exchange retry delay for peer {:?}",
                            peer
                        );
                        return Err(GuardianError::Store("Store cancelled during retry".to_string()));
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(total_delay)) => {
                        // Continue to the next attempt.
                    }
                }
            }
        }

        // All attempts failed.
        let final_error = last_error.unwrap_or_else(|| {
            GuardianError::Store("Unknown error during peer exchange".to_string())
        });

        error!(
            "Failed to exchange heads with peer {:?} after {} attempts: {:?}",
            peer,
            MAX_PEER_EXCHANGE_RETRIES + 1,
            final_error
        );

        // Record final failure metrics.
        if let Some(mut metrics) = self.retry_metrics.try_lock() {
            metrics.record_peer_exchange_final_failure();
        }

        Err(final_error)
    }

    /// Connects to a peer via a direct channel, loads the local "heads"
    /// from the cache and sends them to the peer.
    pub async fn exchange_heads(&self, peer: NodeId) -> Result<()> {
        debug!("[EXCHANGE_HEADS] Starting exchange with peer: {:?}", peer);

        // **CRITICAL FIX**: For full synchronization, we send ALL oplog entries,
        // not just the heads (tips). The heads are only the leaf nodes,
        // but the receiver needs the entire chain of entries.
        let all_entries: Vec<Entry> = self.log_and_index.with_oplog(|oplog| {
            // values() returns all log entries, not just the heads.
            let entries_vec = oplog
                .values()
                .iter()
                .map(|arc_entry| (**arc_entry).clone())
                .collect::<Vec<Entry>>();
            debug!(
                "[EXCHANGE_HEADS] From oplog.values(): {} total entries, oplog.heads().len()={}",
                entries_vec.len(),
                oplog.heads().len()
            );
            entries_vec
        });

        debug!(
            "[EXCHANGE_HEADS] Sending {} total entries to peer: {:?}",
            all_entries.len(),
            peer
        );

        // USE ONLY THE LOG NAME, not the full address, to allow
        // different peers (with different DBNames) to synchronize the same log.
        let log_name = self.extract_log_name();
        let msg = MessageExchangeHeads {
            address: log_name.clone(),
            heads: all_entries, // Send all entries, not just heads.
        };

        let payload = self
            .message_marshaler
            .marshal(&msg)
            .map_err(|e| GuardianError::Store(format!("unable to marshall message: {}", e)))?;

        debug!(
            "[EXCHANGE_HEADS] Broadcasting {} entries ({} bytes) via gossip topic",
            msg.heads.len(),
            payload.len()
        );

        // Get the shared gossip topic.
        let topic_option = self.topic.lock().await;
        let topic = topic_option
            .as_ref()
            .ok_or_else(|| GuardianError::Store("Gossip topic not initialized".to_string()))?;

        // Use the PubSub API to publish the message.
        let topic_name = topic.topic();

        // **CRITICAL FIX**: Before publishing, ensure the target peer is in the mesh.
        // Re-subscribing with the peer as bootstrap ensures iroh-gossip forms a connection.
        if let Some(epidemic_pubsub) =
            self.pubsub
                .as_ref()
                .as_any()
                .downcast_ref::<crate::p2p::network::core::gossip::EpidemicPubSub>()
        {
            debug!(
                "[EXCHANGE_HEADS] Adding peer {:?} to gossip mesh before publishing",
                peer
            );

            // Add the peer to the mesh via subscribe_with_peers.
            epidemic_pubsub
                .subscribe_with_peers(topic_name, vec![peer])
                .await
                .map_err(|e| {
                    warn!(
                        "[EXCHANGE_HEADS] Failed to add peer to mesh (continuing anyway): {}",
                        e
                    );
                    GuardianError::Store(format!("Failed to add peer to mesh: {}", e))
                })
                .ok(); // Does not fail if it cannot add; tries to publish anyway.

            // **CRITICAL FIX**: Check that the peer is actually connected before publishing.
            // This avoids sending messages when the mesh is not yet formed bilaterally.
            let iroh_topic = epidemic_pubsub.get_topic(topic_name).await;
            if let Some(iroh_topic) = iroh_topic {
                let mut attempts = 0;
                const MAX_WAIT_ATTEMPTS: u32 = 30; // 30 * 100ms = 3 seconds max.

                loop {
                    let peers = iroh_topic.list_peers().await;
                    let peer_connected = peers.contains(&peer);

                    if peer_connected {
                        debug!(
                            "[EXCHANGE_HEADS] Peer {:?} confirmed in mesh, proceeding with broadcast",
                            peer
                        );
                        break;
                    }

                    attempts += 1;
                    if attempts >= MAX_WAIT_ATTEMPTS {
                        warn!(
                            "[EXCHANGE_HEADS] Timeout waiting for peer {:?} to appear in mesh after {}ms",
                            peer,
                            attempts * 100
                        );
                        // Continue anyway - it may work.
                        break;
                    }

                    debug!(
                        "[EXCHANGE_HEADS] Waiting for peer {:?} to appear in mesh (attempt {}/{})",
                        peer, attempts, MAX_WAIT_ATTEMPTS
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
            }

            epidemic_pubsub
                .publish_to_topic(topic_name, &payload)
                .await
                .map_err(|e| {
                    error!(
                        "[EXCHANGE_HEADS] Failed to broadcast via EpidemicPubSub: {}",
                        e
                    );
                    GuardianError::Store(format!("Failed to broadcast via gossip: {}", e))
                })?;
        } else if let Some(core_api_pubsub) =
            self.pubsub
                .as_ref()
                .as_any()
                .downcast_ref::<crate::p2p::messaging::CoreApiPubSub>()
        {
            // For CoreApiPubSub, use the internal method.
            debug!(
                "[EXCHANGE_HEADS] Adding peer {:?} to gossip mesh before publishing (CoreApiPubSub)",
                peer
            );

            core_api_pubsub
                .epidemic_pubsub
                .subscribe_with_peers(topic_name, vec![peer])
                .await
                .map_err(|e| {
                    warn!(
                        "[EXCHANGE_HEADS] Failed to add peer to mesh (continuing anyway): {}",
                        e
                    );
                    GuardianError::Store(format!("Failed to add peer to mesh: {}", e))
                })
                .ok();

            // **CRITICAL FIX**: Check that the peer is actually connected before publishing.
            let iroh_topic = core_api_pubsub.epidemic_pubsub.get_topic(topic_name).await;
            if let Some(iroh_topic) = iroh_topic {
                let mut attempts = 0;
                const MAX_WAIT_ATTEMPTS: u32 = 30; // 30 * 100ms = 3 seconds max.

                loop {
                    let peers = iroh_topic.list_peers().await;
                    let peer_connected = peers.contains(&peer);

                    if peer_connected {
                        debug!(
                            "[EXCHANGE_HEADS] Peer {:?} confirmed in mesh (CoreApiPubSub), proceeding with broadcast",
                            peer
                        );
                        break;
                    }

                    attempts += 1;
                    if attempts >= MAX_WAIT_ATTEMPTS {
                        warn!(
                            "[EXCHANGE_HEADS] Timeout waiting for peer {:?} to appear in mesh after {}ms (CoreApiPubSub)",
                            peer,
                            attempts * 100
                        );
                        break;
                    }

                    debug!(
                        "[EXCHANGE_HEADS] Waiting for peer {:?} to appear in mesh (attempt {}/{}) (CoreApiPubSub)",
                        peer, attempts, MAX_WAIT_ATTEMPTS
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
            }

            core_api_pubsub
                .epidemic_pubsub
                .publish_to_topic(topic_name, &payload)
                .await
                .map_err(|e| {
                    error!(
                        "[EXCHANGE_HEADS] Failed to broadcast via CoreApiPubSub: {}",
                        e
                    );
                    GuardianError::Store(format!("Failed to broadcast via gossip: {}", e))
                })?;
        } else {
            return Err(GuardianError::Store(
                "Unknown PubSub implementation - cannot publish".to_string(),
            ));
        }

        debug!(
            "[EXCHANGE_HEADS] Successfully broadcast {} entries to all peers via gossip topic",
            msg.heads.len()
        );

        Ok(())
    }

    /// Extracts the log name from the store's full address.
    /// Example: /GuardianDB/HASH/global-chat -> global-chat
    pub fn extract_log_name(&self) -> String {
        // The ID has the format: /GuardianDB/HASH/LOG_NAME
        // We want only the LOG_NAME to use as a shared topic.
        self.id
            .split('/')
            .next_back()
            .unwrap_or(&self.id)
            .to_string()
    }

    /// Loads the store's state from the heads saved in the cache. It processes
    /// each head concurrently, reports progress and joins the results.
    pub async fn load(&self, amount: Option<isize>) -> Result<()> {
        let _default_amount = amount.unwrap_or(-1); // -1 for "all".

        // Load heads from the cache.
        let mut heads = Vec::new();
        let cache = self.cache();

        BaseStore::load_heads_from_cache_key(&cache, "_localHeads", &mut heads).await?;
        BaseStore::load_heads_from_cache_key(&cache, "_remoteHeads", &mut heads).await?;

        // Emit a start event via SyncObserver.
        self.sync_observer.emit_started(heads.len()).await;

        // Emit a load-start event (legacy event).
        let load_event = EventLoad {
            address: self.address.clone(),
            heads: Vec::new(), // Initially empty.
        };
        if let Err(e) = self.emitters.evt_load.emit(load_event) {
            warn!("Failed to emit EventLoad: {}", e);
        }

        // Try to load all entries from the cache (full persistence).
        let mut all_entries: Vec<Entry> = Vec::new();
        match cache.get("_allEntries".as_bytes()).await {
            Ok(Some(bytes)) => {
                match crate::guardian::serializer::deserialize::<Vec<Entry>>(&bytes) {
                    Ok(entries) => {
                        all_entries = entries;
                    }
                    Err(e) => {
                        warn!("Failed to deserialize _allEntries: {}", e);
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                warn!("Error getting _allEntries from cache: {}", e);
            }
        }

        // If there are no full entries or heads, the store is empty.
        if heads.is_empty() && all_entries.is_empty() {
            // Emit a Ready event via SyncObserver.
            self.sync_observer.emit_ready(Vec::new()).await;

            // Emit an event indicating that loading finished (with no data).
            let ready_event = EventReady {
                address: self.address.clone(),
                heads: Vec::new(),
            };
            if let Err(e) = self.emitters.evt_ready.emit(ready_event) {
                warn!("Failed to emit EventReady: {}", e);
            }
            return Ok(());
        }

        // Use all entries if available, otherwise use the heads.
        let entries_to_load = if !all_entries.is_empty() {
            all_entries
        } else {
            heads.clone()
        };

        debug!("Loading {} entries into oplog", entries_to_load.len());

        // Insert the entries into the oplog so that list() can return them.
        // Use add_entry() instead of join() to accept entries from other peers.
        // The join() method uses diff(), which checks entry.id() == oplog.id, which
        // excludes entries from other peers (which have different IDs).
        let loaded_count = self.log_and_index.with_oplog_mut(|oplog| {
            let mut count = 0;
            for entry in &entries_to_load {
                if oplog.add_entry(entry.clone()) {
                    count += 1;
                }
            }
            // Update the Lamport clock to the maximum time of the loaded heads.
            oplog.sync_clock_from_heads();
            count
        });

        debug!("Loaded {} new entries into oplog", loaded_count);

        // Emit a progress event for each loaded entry.
        for (i, entry) in entries_to_load.iter().enumerate() {
            // Emit progress via SyncObserver.
            self.sync_observer
                .emit_progress(*entry.hash(), entry.clone(), i + 1, entries_to_load.len())
                .await;

            // Emit legacy progress.
            let load_progress_event = EventLoadProgress {
                address: self.address.clone(),
                hash: *entry.hash(),
                entry: entry.clone(),
                progress: (i + 1) as i32,
                max: entries_to_load.len() as i32,
            };
            if let Err(e) = self.emitters.evt_load_progress.emit(load_progress_event) {
                warn!("Failed to emit EventLoadProgress: {}", e);
            }
        }

        self.update_index()?;

        // Emit a Ready event via SyncObserver.
        self.sync_observer.emit_ready(heads.clone()).await;

        // Emit an event indicating that the store is ready.
        let ready_event = EventReady {
            address: self.address.clone(),
            heads: heads.clone(),
        };
        if let Err(e) = self.emitters.evt_ready.emit(ready_event) {
            warn!("Failed to emit EventReady: {}", e);
        }

        debug!("Load completed");

        Ok(())
    }

    /// Helper function for 'load'.
    /// Loads and deserializes a list of `Entry` from a cache key.
    async fn load_heads_from_cache_key(
        cache: &Arc<dyn Datastore>,
        key: &str,
        heads: &mut Vec<Entry>,
    ) -> Result<()> {
        if let Ok(Some(bytes)) = cache.get(key.as_bytes()).await {
            let cached_heads: Vec<Entry> = crate::guardian::serializer::deserialize(&bytes)
                .map_err(|e| {
                    GuardianError::Store(format!(
                        "Failed to deserialize heads from cache key '{}': {}",
                        key, e
                    ))
                })?;
            heads.extend(cached_heads);
        }
        Ok(())
    }

    pub async fn load_from_snapshot(&self) -> Result<()> {
        debug!("Loading from snapshot");

        // Process the pending sync queue first.
        if let Ok(Some(queue_bytes)) = self.cache().get("queue".as_bytes()).await {
            match crate::guardian::serializer::deserialize::<Vec<Entry>>(&queue_bytes) {
                Ok(queue) => {
                    debug!("Processing {} queued entries", queue.len());
                    self.sync(queue).await.map_err(|e| {
                        GuardianError::Store(format!("Unable to sync queued CIDs: {}", e))
                    })?;
                }
                Err(e) => warn!("Failed to deserialize queued entries: {}", e),
            }
        }

        // Get the snapshot path from the cache.
        let snapshot_path_result = self.cache().get("snapshot".as_bytes()).await;
        let snapshot_path_bytes = match snapshot_path_result {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                debug!("No snapshot found in cache");
                self.update_index()?;
                return Ok(());
            }
            Err(e) => {
                warn!("Error getting snapshot from cache: {}", e);
                self.update_index()?;
                return Ok(());
            }
        };

        let snapshot_path = String::from_utf8(snapshot_path_bytes)
            .map_err(|e| GuardianError::Store(format!("Invalid UTF-8 in snapshot path: {}", e)))?;

        debug!("Loading snapshot from path: {}", snapshot_path);

        // Load the snapshot from Iroh using cat_bytes.
        match self.client.cat_bytes(&snapshot_path).await {
            Ok(snapshot_data) => {
                // Process the snapshot data.
                match self.process_snapshot_data(snapshot_data).await {
                    Ok(entries_loaded) => {
                        debug!(
                            "Successfully loaded {} entries from snapshot",
                            entries_loaded
                        );

                        // Emit a load event using a simple log for now.
                        debug!("Snapshot load completed with {} entries", entries_loaded);
                    }
                    Err(e) => {
                        warn!("Failed to process snapshot data: {}", e);
                        return Err(e);
                    }
                }
            }
            Err(e) => {
                warn!("Failed to load snapshot from Iroh: {}", e);
                // Continue without error, just log the failure.
            }
        }

        self.update_index()?;
        Ok(())
    }

    /// Processes the data of a snapshot loaded from the Client.
    async fn process_snapshot_data(&self, data: Vec<u8>) -> Result<usize> {
        use std::io::Cursor;
        use tokio::io::AsyncReadExt;

        let mut cursor = Cursor::new(data);
        let mut entries_loaded = 0;

        // Read the snapshot data.
        while cursor.position() < cursor.get_ref().len() as u64 {
            // Read the entry size (4 bytes, big-endian).
            let mut size_bytes = [0u8; 4];
            if cursor.read_exact(&mut size_bytes).await.is_err() {
                break; // End of data.
            }
            let entry_size = u32::from_be_bytes(size_bytes) as usize;

            // Read the entry data.
            let mut entry_data = vec![0u8; entry_size];
            if cursor.read_exact(&mut entry_data).await.is_err() {
                break; // Corrupted data.
            }

            // Deserialize the entry.
            match crate::guardian::serializer::deserialize::<Entry>(&entry_data) {
                Ok(entry) => {
                    // Add the entry to the oplog using the appropriate methods.
                    let entry_hash = entry.hash();
                    if let Err(e) = self.log_and_index.with_oplog_mut(|oplog| {
                        // Check whether the entry already exists using has().
                        if !oplog.has(entry_hash) {
                            // Add the entry using append().
                            // Entry.payload is now Vec<u8>, we convert to a lossy string.
                            let payload_str = String::from_utf8_lossy(&entry.payload).to_string();
                            oplog.append(&payload_str, None);
                        }
                        Ok::<(), GuardianError>(())
                    }) {
                        warn!("Failed to add entry to oplog: {}", e);
                        continue;
                    }
                    entries_loaded += 1;
                }
                Err(e) => {
                    warn!("Failed to deserialize entry from snapshot: {}", e);
                    continue;
                }
            }
        }

        Ok(entries_loaded)
    }

    /// Helper function for 'load_from_snapshot'.
    /// Reads a u16 (big-endian) length prefix from a stream, reads the
    /// corresponding number of bytes and deserializes them into a type T using postcard.
    #[allow(dead_code)]
    async fn read_prefixed_json<T, R>(reader: &mut R) -> Result<T>
    where
        T: for<'de> serde::Deserialize<'de>,
        R: AsyncRead + Unpin,
    {
        let len = reader.read_u16().await.map_err(|e| {
            GuardianError::Store(format!("Failed to read the snapshot size prefix: {}", e))
        })?;

        let mut buf = vec![0; len as usize];
        reader.read_exact(&mut buf).await.map_err(|e| {
            GuardianError::Store(format!("Failed to read the snapshot data block: {}", e))
        })?;

        crate::guardian::serializer::deserialize(&buf).map_err(|e| {
            GuardianError::Store(format!("Failed to deserialize snapshot data: {}", e))
        })
    }
}

/// This function was extracted as a free function (not a `BaseStore` method)
/// to be used during the construction of the `store`, keeping the emitter
/// initialization logic separate.
async fn generate_emitters(bus: &EventBus) -> Result<Emitters> {
    Ok(Emitters {
        evt_write: bus.emitter::<EventWrite>().await.map_err(|e| {
            GuardianError::Store(format!("unable to create EventWrite emitter: {}", e))
        })?,
        evt_ready: bus.emitter::<EventReady>().await.map_err(|e| {
            GuardianError::Store(format!("unable to create EventReady emitter: {}", e))
        })?,
        evt_replicate_progress: bus.emitter::<EventReplicateProgress>().await.map_err(|e| {
            GuardianError::Store(format!(
                "unable to create EventReplicateProgress emitter: {}",
                e
            ))
        })?,
        evt_load: bus.emitter::<EventLoad>().await.map_err(|e| {
            GuardianError::Store(format!("unable to create EventLoad emitter: {}", e))
        })?,
        evt_load_progress: bus.emitter::<EventLoadProgress>().await.map_err(|e| {
            GuardianError::Store(format!("unable to create EventLoadProgress emitter: {}", e))
        })?,
        evt_replicated: bus.emitter::<EventReplicated>().await.map_err(|e| {
            GuardianError::Store(format!("unable to create EventReplicated emitter: {}", e))
        })?,
        evt_replicate: bus.emitter::<EventReplicate>().await.map_err(|e| {
            GuardianError::Store(format!("unable to create EventReplicate emitter: {}", e))
        })?,
    })
}

/// Store trait implementation for BaseStore.
///
/// This implementation makes BaseStore compatible with the Store interface,
/// allowing it to be used in any context that expects a Store.
#[async_trait::async_trait]
impl Store for BaseStore {
    type Error = GuardianError;

    #[allow(deprecated)]
    fn events(&self) -> &dyn EmitterInterface {
        self.emitter_interface.as_ref()
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        // Call the public close(&self) method, which is already implemented correctly.
        self.close().await
    }

    fn address(&self) -> &dyn Address {
        self.address.as_ref()
    }

    fn index(&self) -> Box<dyn StoreIndex<Error = Self::Error> + Send + Sync> {
        // Create a wrapper that holds a reference to the store's log_and_index
        // and delegates all operations to the active index when available.
        struct IndexWrapper {
            log_and_index: Arc<LogAndIndex>,
        }

        impl StoreIndex for IndexWrapper {
            type Error = GuardianError;

            fn contains_key(&self, key: &str) -> std::result::Result<bool, Self::Error> {
                // Delegate to the active index if available.
                if let Some(result) = self
                    .log_and_index
                    .with_index(|index| index.contains_key(key))
                {
                    result
                } else {
                    // If there is no active index, the key does not exist.
                    Ok(false)
                }
            }

            fn get_bytes(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
                // Delegate to the active index if available.
                if let Some(result) = self.log_and_index.with_index(|index| index.get_bytes(key)) {
                    result
                } else {
                    // If there is no active index, return None.
                    Ok(None)
                }
            }

            fn keys(&self) -> std::result::Result<Vec<String>, Self::Error> {
                // Delegate to the active index if available.
                if let Some(result) = self.log_and_index.with_index(|index| index.keys()) {
                    result
                } else {
                    // If there is no active index, return an empty list.
                    Ok(Vec::new())
                }
            }

            fn len(&self) -> std::result::Result<usize, Self::Error> {
                // Delegate to the active index if available.
                if let Some(result) = self.log_and_index.with_index(|index| index.len()) {
                    result
                } else {
                    // If there is no active index, the length is zero.
                    Ok(0)
                }
            }

            fn is_empty(&self) -> std::result::Result<bool, Self::Error> {
                // Delegate to the active index if available.
                if let Some(result) = self.log_and_index.with_index(|index| index.is_empty()) {
                    result
                } else {
                    // If there is no active index, we consider it empty.
                    Ok(true)
                }
            }

            fn update_index(
                &mut self,
                log: &crate::log::Log,
                entries: &[crate::log::entry::Entry],
            ) -> std::result::Result<(), Self::Error> {
                // Delegate to the active index if available.
                let mut guard = self.log_and_index.active_index.write();
                match guard.as_mut() {
                    Some(index) => index.update_index(log, entries),
                    None => Ok(()), // If there is no active index, do nothing.
                }
            }

            fn clear(&mut self) -> std::result::Result<(), Self::Error> {
                // Delegate to the active index if available.
                let mut guard = self.log_and_index.active_index.write();
                match guard.as_mut() {
                    Some(index) => index.clear(),
                    None => Ok(()), // If there is no active index, do nothing.
                }
            }
        }

        // Return the wrapper with a reference to the log_and_index.
        Box::new(IndexWrapper {
            log_and_index: Arc::new(LogAndIndex {
                oplog: self.log_and_index.oplog.clone(),
                active_index: self.log_and_index.active_index.clone(),
            }),
        }) as Box<dyn StoreIndex<Error = Self::Error> + Send + Sync>
    }
    fn store_type(&self) -> &str {
        "base"
    }

    async fn drop(&self) -> std::result::Result<(), Self::Error> {
        // Mutable version of drop.
        Ok(())
    }

    // Specific methods delegated to the existing implementations.
    fn cache(&self) -> Arc<dyn Datastore> {
        Self::cache(self)
    }

    async fn load(&self, amount: usize) -> std::result::Result<(), Self::Error> {
        // Use the existing method, but converting the type.
        Self::load(self, Some(amount as isize)).await
    }

    async fn sync(&self, heads: Vec<Entry>) -> std::result::Result<(), Self::Error> {
        Self::sync(self, heads).await
    }

    async fn load_more_from(&self, _amount: u64, entries: Vec<Entry>) {
        // Ignore the amount for now and use the existing method.
        let _ = Self::load_more_from(self, entries);
    }

    async fn load_from_snapshot(&self) -> std::result::Result<(), Self::Error> {
        Self::load_from_snapshot(self).await
    }

    fn op_log(&self) -> Arc<RwLock<Log>> {
        self.log_and_index.op_log_arc()
    }

    fn client(&self) -> Arc<IrohClient> {
        Self::client(self)
    }

    fn db_name(&self) -> &str {
        Self::db_name(self)
    }

    fn identity(&self) -> &Identity {
        Self::identity(self)
    }

    fn access_controller(&self) -> &dyn AccessController {
        Self::access_controller(self)
    }

    async fn add_operation(
        &self,
        op: Operation,
        on_progress: Option<mpsc::Sender<Entry>>,
    ) -> std::result::Result<Entry, Self::Error> {
        Self::add_operation(self, op, on_progress).await
    }

    fn span(&self) -> Arc<tracing::Span> {
        Arc::new(self.span.clone())
    }

    fn tracer(&self) -> Arc<TracerWrapper> {
        Self::tracer(self)
    }

    fn event_bus(&self) -> Arc<EventBus> {
        self.event_bus.clone()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
