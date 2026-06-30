use crate::access_control::acl_simple::SimpleAccessController;
use crate::access_control::traits::AccessController;
use crate::address::Address;
use crate::data_store::Datastore;
use crate::events::EventEmitter;
use crate::guardian::error::{GuardianError, Result};
use crate::log::identity::Identity;
use crate::log::lamport_clock::LamportClock;
use crate::p2p::EventBus;
use crate::p2p::network::core::docs::WillowDocs;
use crate::stores::operation::Operation;
use crate::traits::{KeyValueStore, NewStoreOptions, Store, StoreIndex, TracerWrapper};
use bytes::Bytes;
use iroh_docs::{AuthorId, Capability, api::Doc, store::Query};
use opentelemetry::trace::{TracerProvider, noop::NoopTracerProvider};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{Span, debug, info, instrument, warn};

pub mod index;

// Cache key used to persist the iroh-docs document's NamespaceId.
const NAMESPACE_CACHE_KEY: &[u8] = b"_iroh_docs_namespace_id";
// Cache key used to persist whether this replica holds the namespace write secret.
// Stored as a single byte: 1 = write-capable, 0 = read-only.
const WRITABLE_CACHE_KEY: &[u8] = b"_iroh_docs_writable";

/// StoreIndex implementation for the KeyValue Store.
///
/// Maintains a thread-safe in-memory index that mirrors the state of the
/// iroh-docs document. It is updated atomically after each put/delete
/// operation, serving as a synchronous cache for StoreIndex queries.
pub struct KeyValueIndex {
    /// Internal index that maps keys to values.
    index: Arc<RwLock<HashMap<String, Vec<u8>>>>,
}

impl Default for KeyValueIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyValueIndex {
    pub fn new() -> Self {
        Self {
            index: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Gets a value from the index.
    pub fn get_value(&self, key: &str) -> Option<Vec<u8>> {
        let guard = self.index.read();
        guard.get(key).cloned()
    }

    /// Gets all key-value pairs.
    pub fn get_all(&self) -> HashMap<String, Vec<u8>> {
        let guard = self.index.read();
        guard.clone()
    }

    /// Counts the number of entries.
    pub fn len(&self) -> usize {
        let guard = self.index.read();
        guard.len()
    }

    /// Checks whether the index is empty.
    pub fn is_empty(&self) -> bool {
        let guard = self.index.read();
        guard.is_empty()
    }

    /// Inserts a key-value pair into the index.
    pub fn insert(&self, key: String, value: Vec<u8>) {
        let mut guard = self.index.write();
        guard.insert(key, value);
    }

    /// Removes a key from the index.
    pub fn remove(&self, key: &str) {
        let mut guard = self.index.write();
        guard.remove(key);
    }

    /// Clears the entire index.
    pub fn clear_all(&self) {
        let mut guard = self.index.write();
        guard.clear();
    }
}

/// Rebuilds the in-memory index from the current state of the iroh-docs document.
///
/// Function shared between `sync_index_from_docs` (manual load/sync) and the reactive
/// live-sync task, avoiding logic duplication.
async fn refresh_kv_index(
    docs: &WillowDocs,
    doc: &Doc,
    client: &Arc<crate::p2p::network::client::IrohClient>,
    index: &Arc<KeyValueIndex>,
) -> Result<usize> {
    let entries = docs
        .get_many(doc, Query::single_latest_per_key().build())
        .await?;

    index.clear_all();
    let mut count = 0;

    for entry in &entries {
        let key = String::from_utf8_lossy(entry.key()).to_string();

        // Entries with content_len == 0 are deletion markers.
        if entry.content_len() == 0 {
            continue;
        }

        // Read the content bytes via the blob store using the content_hash.
        let hash_str = entry.content_hash().to_hex();
        match client.cat_bytes(&hash_str).await {
            Ok(value) => {
                index.insert(key, value);
                count += 1;
            }
            Err(e) => {
                warn!("Failed to read content for key from iroh-docs: {:?}", e);
            }
        }
    }

    debug!(
        "KeyValue index synchronized from iroh-docs: {} entries",
        count
    );
    Ok(count)
}

impl StoreIndex for KeyValueIndex {
    type Error = GuardianError;

    fn contains_key(&self, key: &str) -> std::result::Result<bool, Self::Error> {
        let guard = self.index.read();
        Ok(guard.contains_key(key))
    }

    fn get_bytes(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
        let guard = self.index.read();
        Ok(guard.get(key).cloned())
    }

    fn keys(&self) -> std::result::Result<Vec<String>, Self::Error> {
        let guard = self.index.read();
        Ok(guard.keys().cloned().collect())
    }

    fn len(&self) -> std::result::Result<usize, Self::Error> {
        let guard = self.index.read();
        Ok(guard.len())
    }

    fn is_empty(&self) -> std::result::Result<bool, Self::Error> {
        let guard = self.index.read();
        Ok(guard.is_empty())
    }

    /// No-op for the iroh-docs-based implementation.
    /// The local index is updated directly after each put/delete operation,
    /// with no need to replay the OpLog.
    fn update_index(
        &mut self,
        _log: &crate::log::Log,
        _entries: &[crate::log::entry::Entry],
    ) -> std::result::Result<(), Self::Error> {
        // iroh-docs manages its own state — the local index is updated
        // directly in the put/delete operations.
        Ok(())
    }

    fn clear(&mut self) -> std::result::Result<(), Self::Error> {
        let mut guard = self.index.write();
        guard.clear();
        Ok(())
    }
}

/// KeyValue Store implementation for GuardianDB using iroh-docs (WillowDocs).
///
/// This implementation uses the iroh-docs protocol for distributed KV storage
/// with Last-Write-Wins (LWW) conflict resolution, replacing the previous
/// architecture based on BaseStore + OpLog.
///
/// # Architecture
///
/// - **Backend**: iroh-docs (Willow range-based reconciliation)
/// - **Write**: `doc.set_bytes()` / `doc.del()` via WillowDocs
/// - **Read**: iroh-docs query + byte fetch via the blob store
/// - **Sync**: Automatic via Willow (no manual gossip heads)
/// - **Local index**: In-memory HashMap mirroring the iroh-docs state
///
/// Components kept from the previous architecture:
/// - `AccessController` — permission validation on each write
/// - `EventBus` — reactive events for UI/observers
/// - `Identity` → `AuthorId` consistent mapping
pub struct GuardianDBKeyValue {
    /// WillowDocs backend (iroh-docs).
    docs: WillowDocs,
    /// iroh-docs document handle for this KV namespace.
    doc_handle: Doc,
    /// AuthorId for write operations (mapped from the Identity).
    author_id: AuthorId,
    /// Access controller for permission validation.
    access_controller: Arc<dyn AccessController>,
    /// Event bus for reactive notifications.
    event_bus: Arc<EventBus>,
    /// Reference to the IrohClient (for reading blobs and Store trait compatibility).
    client: Arc<crate::p2p::network::client::IrohClient>,
    /// Cryptographic identity of the store.
    identity: Arc<Identity>,
    /// Store address (cached to resolve lifetime issues).
    cached_address: Arc<dyn Address + Send + Sync>,
    /// Database name.
    db_name: String,
    /// Local cache (sled) — used to persist the NamespaceId across reloads.
    cache: Arc<dyn Datastore>,
    /// Local in-memory index mirroring the iroh-docs state.
    index: Arc<KeyValueIndex>,
    /// Span for structured tracing.
    span: Span,
    /// Tracer for telemetry.
    tracer: Arc<TracerWrapper>,
    /// Event emission interface (for Store trait compatibility).
    emitter_interface: Arc<dyn crate::events::EmitterInterface + Send + Sync>,
    /// Empty log for compatibility with the Store trait (op_log()).
    empty_log: Arc<RwLock<crate::log::Log>>,
    /// Whether this replica may originate writes. `false` when the store was opened read-only
    /// (via `read_only` option) or imported from a read-only `DocTicket` (no namespace secret).
    writable: bool,
}

#[async_trait::async_trait]
impl Store for GuardianDBKeyValue {
    type Error = GuardianError;

    #[allow(deprecated)]
    fn events(&self) -> &dyn crate::events::EmitterInterface {
        self.emitter_interface.as_ref()
    }

    async fn close(&self) -> Result<()> {
        debug!("Starting KeyValue store close operation (iroh-docs backend)");

        // Close the iroh-docs document.
        if let Err(e) = self.docs.close_doc(&self.doc_handle).await {
            warn!("Failed to close iroh-docs document: {:?}", e);
        }

        debug!("KeyValue store close completed");
        Ok(())
    }

    fn address(&self) -> &dyn Address {
        self.cached_address.as_ref()
    }

    fn index(&self) -> Box<dyn StoreIndex<Error = GuardianError> + Send + Sync> {
        Box::new(KeyValueIndex {
            index: self.index.index.clone(),
        })
    }

    fn store_type(&self) -> &str {
        "keyvalue"
    }

    fn cache(&self) -> Arc<dyn Datastore> {
        self.cache.clone()
    }

    async fn drop(&self) -> Result<()> {
        debug!("Starting KeyValue store drop operation (iroh-docs backend)");

        // Clear the local index.
        self.index.clear_all();

        // Remove the iroh-docs document permanently.
        let namespace_id = self.doc_handle.id();
        if let Err(e) = self.docs.drop_doc(namespace_id).await {
            warn!("Failed to drop iroh-docs document: {:?}", e);
        }

        // Remove the NamespaceId from the cache.
        if let Err(e) = self.cache.delete(NAMESPACE_CACHE_KEY).await {
            warn!("Failed to remove namespace from cache: {:?}", e);
        }

        debug!("KeyValue store drop completed");
        Ok(())
    }

    /// No-op for iroh-docs — Willow sync handles loading automatically.
    async fn load(&self, _amount: usize) -> Result<()> {
        // iroh-docs uses Willow range sync, no manual load needed.
        // Synchronize the local index with the document's current state.
        self.sync_index_from_docs().await?;
        Ok(())
    }

    /// No-op for iroh-docs — Willow sync replaces the gossip heads exchange.
    async fn sync(&self, _heads: Vec<crate::log::entry::Entry>) -> Result<()> {
        // iroh-docs uses Willow range reconciliation internally.
        // After an external sync, we update the local index.
        self.sync_index_from_docs().await?;
        Ok(())
    }

    /// No-op for iroh-docs.
    async fn load_more_from(&self, _amount: u64, _entries: Vec<crate::log::entry::Entry>) {
        // iroh-docs manages its own incremental loading.
    }

    /// No-op for iroh-docs.
    async fn load_from_snapshot(&self) -> Result<()> {
        // iroh-docs does not use snapshots — the state is authoritative.
        self.sync_index_from_docs().await?;
        Ok(())
    }

    /// Returns an empty Log for compatibility with the Store trait.
    /// In the iroh-docs architecture, the OpLog is no longer used —
    /// iroh-docs manages its own state with LWW.
    fn op_log(&self) -> Arc<RwLock<crate::log::Log>> {
        self.empty_log.clone()
    }

    fn client(&self) -> Arc<crate::p2p::network::client::IrohClient> {
        self.client.clone()
    }

    fn db_name(&self) -> &str {
        &self.db_name
    }

    fn identity(&self) -> &Identity {
        &self.identity
    }

    fn access_controller(&self) -> &dyn crate::access_control::traits::AccessController {
        self.access_controller.as_ref()
    }

    /// Translates an Operation into iroh-docs operations (set_bytes/del).
    /// Returns a synthetic Entry for compatibility with the Store trait.
    async fn add_operation(
        &self,
        op: Operation,
        _on_progress_callback: Option<tokio::sync::mpsc::Sender<crate::log::entry::Entry>>,
    ) -> Result<crate::log::entry::Entry> {
        // Canonical write path for the public KeyValueStore API: enforce read-only here too,
        // not just in put_impl/delete_impl.
        self.ensure_writable()?;

        let key = op.key().cloned().unwrap_or_default();

        match op.op() {
            "PUT" => {
                let value = op.value().to_vec();
                self.docs
                    .set_bytes(
                        &self.doc_handle,
                        self.author_id,
                        Bytes::from(key.clone().into_bytes()),
                        Bytes::from(value.clone()),
                    )
                    .await?;

                // Update the local index.
                self.index.insert(key, value);
            }
            "DEL" => {
                self.docs
                    .del(
                        &self.doc_handle,
                        self.author_id,
                        Bytes::from(key.clone().into_bytes()),
                    )
                    .await?;

                // Update the local index.
                self.index.remove(&key);
            }
            other => {
                return Err(GuardianError::Store(format!(
                    "Unknown operation: {}",
                    other
                )));
            }
        }

        // Create a synthetic Entry for compatibility.
        let payload = crate::guardian::serializer::serialize(&op).unwrap_or_default();
        let clock = LamportClock::new(self.identity.pub_key());
        let entry_arc = crate::log::entry::Entry::create(
            &self.client,
            (*self.identity).clone(),
            "",
            &payload,
            &[],
            Some(clock),
        );
        let entry = (*entry_arc).clone();

        Ok(entry)
    }

    fn span(&self) -> Arc<tracing::Span> {
        Arc::new(self.span.clone())
    }

    fn tracer(&self) -> Arc<TracerWrapper> {
        self.tracer.clone()
    }

    fn event_bus(&self) -> Arc<EventBus> {
        self.event_bus.clone()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl GuardianDBKeyValue {
    /// Returns a reference to the tracing span used for instrumentation.
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Returns the NamespaceId of the underlying iroh-docs document.
    pub fn namespace_id(&self) -> iroh_docs::NamespaceId {
        self.doc_handle.id()
    }

    /// Returns the AuthorId used for write operations.
    pub fn author_id(&self) -> AuthorId {
        self.author_id
    }
}

// `KeyValueStore` trait implementation for `GuardianDBKeyValue`.
#[async_trait::async_trait]
impl KeyValueStore for GuardianDBKeyValue {
    fn all(&self) -> HashMap<String, Vec<u8>> {
        self.index.get_all()
    }

    async fn put(&self, key: &str, value: Vec<u8>) -> Result<Operation> {
        self.put_impl(key, value).await
    }

    async fn delete(&self, key: &str) -> Result<Operation> {
        self.delete_impl(key).await
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.get_impl(key).await
    }

    /// Generates a `DocTicket` (with write capability) for this store's iroh-docs namespace.
    /// The peer that receives the ticket can import the same namespace and replicate securely.
    ///
    /// Note: this grants write capability (carries the namespace secret). For role-gated
    /// sharing, the automatic ticket exchange hands out read or write tickets per requester
    /// (see [`GuardianDBKeyValue::share_tickets`]).
    async fn share_ticket(&self) -> Result<String> {
        let ticket = self.docs.share_doc(&self.doc_handle, true).await?;
        Ok(ticket.to_string())
    }
}

impl GuardianDBKeyValue {
    /// Generates both the read-only and read-write `DocTicket`s for this store's namespace.
    ///
    /// Returns `(read_ticket, write_ticket)`. The read ticket carries only the namespace
    /// public key (no write secret); the write ticket carries the namespace secret. These are
    /// registered with the ticket exchange so each requester receives the capability matching
    /// its authenticated role.
    async fn share_tickets(&self) -> Result<(String, String)> {
        let read_ticket = self.docs.share_doc(&self.doc_handle, false).await?;
        let write_ticket = self.docs.share_doc(&self.doc_handle, true).await?;
        Ok((read_ticket.to_string(), write_ticket.to_string()))
    }

    /// Returns whether this replica may originate writes.
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Fails fast if this replica is read-only, so callers get a clear error instead of
    /// producing entries that remote peers would silently reject.
    fn ensure_writable(&self) -> Result<()> {
        if self.writable {
            Ok(())
        } else {
            Err(GuardianError::Store(
                "store is read-only: this replica cannot originate writes".to_string(),
            ))
        }
    }

    /// Persists the replica's writability flag (best-effort; a failure only costs a fallback
    /// to the default on reopen).
    async fn persist_writable(cache: &dyn Datastore, writable: bool) {
        if let Err(e) = cache.put(WRITABLE_CACHE_KEY, &[writable as u8]).await {
            warn!("Failed to persist writability flag: {:?}", e);
        }
    }

    /// Loads the replica's writability flag, defaulting to `true` for legacy stores that
    /// predate the flag (they were always write-capable).
    async fn load_writable(cache: &dyn Datastore) -> bool {
        match cache.get(WRITABLE_CACHE_KEY).await {
            Ok(Some(bytes)) if !bytes.is_empty() => bytes[0] != 0,
            _ => true,
        }
    }

    /// Returns the number of key-value pairs in the store.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Checks whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Checks whether a key exists in the store.
    pub fn contains_key(&self, key: &str) -> bool {
        self.index.get_value(key).is_some()
    }

    /// Returns all keys in the store.
    pub fn keys(&self) -> Vec<String> {
        self.index.get_all().keys().cloned().collect()
    }

    /// Returns all key-value pairs in the store.
    pub fn all(&self) -> HashMap<String, Vec<u8>> {
        self.index.get_all()
    }

    /// Synchronizes the local index with the current state of the iroh-docs document.
    ///
    /// Queries all document entries and rebuilds the in-memory index.
    /// Used after load/sync operations or for state recovery.
    pub async fn sync_index_from_docs(&self) -> Result<usize> {
        refresh_kv_index(&self.docs, &self.doc_handle, &self.client, &self.index).await
    }

    /// Starts a background task that keeps the in-memory index synchronized with the
    /// iroh-docs namespace as REMOTE entries arrive via Willow sync.
    ///
    /// Without this, writes from a peer would not appear in this node's `all()`/`get()` until a
    /// manual `load()`/`sync()`, since `all()` reads the in-memory index (not the doc directly).
    fn spawn_live_index_sync(&self) {
        let docs = self.docs.clone();
        let doc = self.doc_handle.clone();
        let client = self.client.clone();
        let index = self.index.clone();

        tokio::spawn(async move {
            let mut stream = match doc.subscribe().await {
                Ok(s) => s,
                Err(e) => {
                    warn!("Failed to subscribe to iroh-docs doc events (KV): {:?}", e);
                    return;
                }
            };

            use futures::StreamExt;
            use iroh_docs::engine::LiveEvent;
            while let Some(event) = stream.next().await {
                // Rebuild the index ONLY on REMOTE-origin events (peer sync).
                // Local events (InsertLocal) are already reflected by put_impl/delete_impl, and
                // refreshing on them would race with the local write (clear_all + rebuild).
                let is_remote = matches!(
                    event,
                    Ok(LiveEvent::InsertRemote { .. })
                        | Ok(LiveEvent::ContentReady { .. })
                        | Ok(LiveEvent::PendingContentReady)
                        | Ok(LiveEvent::SyncFinished(_))
                );
                if is_remote && let Err(e) = refresh_kv_index(&docs, &doc, &client, &index).await {
                    warn!("Failed to update KV index via live sync: {:?}", e);
                }
            }
            debug!("Live index sync terminated for KV store");
        });
    }

    /// Adds or updates a value for a specific key.
    ///
    /// Writes directly to the iroh-docs document via `set_bytes()`.
    /// The value is stored in the blob store and referenced by the document.
    /// Synchronization with other peers is automatic via Willow.
    #[instrument(level = "debug", skip(self, value))]
    pub async fn put_impl(&self, key: &str, value: Vec<u8>) -> Result<Operation> {
        self.ensure_writable()?;

        if key.is_empty() {
            return Err(GuardianError::Store("The key cannot be empty".to_string()));
        }

        if value.is_empty() {
            return Err(GuardianError::Store(
                "The value cannot be empty".to_string(),
            ));
        }

        // Write to the iroh-docs document.
        self.docs
            .set_bytes(
                &self.doc_handle,
                self.author_id,
                Bytes::from(key.as_bytes().to_vec()),
                Bytes::from(value.clone()),
            )
            .await
            .map_err(|e| {
                GuardianError::Store(format!("Error writing key '{}' to iroh-docs: {}", key, e))
            })?;

        // Update the local index immediately.
        self.index.insert(key.to_string(), value.clone());

        debug!("PUT key='{}' ({} bytes) via iroh-docs", key, value.len());

        Ok(Operation::new(
            Some(key.to_string()),
            "PUT".to_string(),
            Some(value),
        ))
    }

    /// Removes the value associated with a specific key.
    ///
    /// Removes the key from the iroh-docs document via `del()`.
    /// The deletion is propagated to other peers automatically via Willow.
    #[instrument(level = "debug", skip(self))]
    pub async fn delete_impl(&self, key: &str) -> Result<Operation> {
        self.ensure_writable()?;

        if key.is_empty() {
            return Err(GuardianError::Store("The key cannot be empty".to_string()));
        }

        // Check whether the key exists in the local index.
        if !self.contains_key(key) {
            return Err(GuardianError::Store(format!("Key '{}' not found", key)));
        }

        // Remove from the iroh-docs document.
        let deleted = self
            .docs
            .del(
                &self.doc_handle,
                self.author_id,
                Bytes::from(key.as_bytes().to_vec()),
            )
            .await
            .map_err(|e| {
                GuardianError::Store(format!("Error deleting key '{}' in iroh-docs: {}", key, e))
            })?;

        // Update the local index immediately.
        self.index.remove(key);

        debug!(
            "DEL key='{}' ({} entries removed) via iroh-docs",
            key, deleted
        );

        Ok(Operation::new(
            Some(key.to_string()),
            "DEL".to_string(),
            None,
        ))
    }

    /// Gets the value associated with a specific key.
    ///
    /// Queries the local in-memory index first (synchronous cache).
    /// The read is O(1) via HashMap, with no need to replay the log.
    #[instrument(level = "debug", skip(self))]
    pub async fn get_impl(&self, key: &str) -> Result<Option<Vec<u8>>> {
        if key.is_empty() {
            return Err(GuardianError::Store("The key cannot be empty".to_string()));
        }

        // Query the local index (mirrors the iroh-docs state).
        Ok(self.index.get_value(key))
    }

    pub fn get_type(&self) -> &'static str {
        "keyvalue"
    }

    /// Creates a new GuardianDBKeyValue instance using iroh-docs as the backend.
    ///
    /// # Initialization flow
    ///
    /// 1. Initializes WillowDocs from the IrohClient backend
    /// 2. Gets/creates the default AuthorId
    /// 3. Creates or opens an iroh-docs document (namespace persisted via cache)
    /// 4. Configures the AccessController and EventBus
    /// 5. Synchronizes the local index with the document state
    #[instrument(level = "debug", skip(client, identity, addr, options))]
    pub async fn new(
        client: Arc<crate::p2p::network::client::IrohClient>,
        identity: Arc<Identity>,
        addr: Arc<dyn Address + Send + Sync>,
        options: Option<NewStoreOptions>,
    ) -> Result<Self> {
        let opts = options.unwrap_or_default();

        // --- 1. Initialize iroh-docs ---

        // Ensure the iroh-docs subsystem is initialized in the client.
        if !client.has_docs_client().await {
            client.init_docs().await.map_err(|e| {
                GuardianError::Store(format!("Failed to initialize iroh-docs: {}", e))
            })?;
        }

        let mut docs = client.docs_client().await.ok_or_else(|| {
            GuardianError::Store("iroh-docs not available after initialization".to_string())
        })?;

        // --- 2. Get the AuthorId ---

        let author_id = docs.get_or_init_author().await.map_err(|e| {
            GuardianError::Store(format!("Failed to initialize iroh-docs author: {}", e))
        })?;

        // --- 3. Configure the components ---

        let db_name = addr.get_path().to_string();
        let span = tracing::info_span!("keyvalue_store", address = %addr.to_string());

        // EventBus
        let event_bus = opts.event_bus.unwrap_or_default();
        let event_bus = Arc::new(event_bus);

        // AccessController
        let access_controller = opts.access_controller.unwrap_or_else(|| {
            let mut default_access = HashMap::new();
            default_access.insert("write".to_string(), vec!["*".to_string()]);
            Arc::new(SimpleAccessController::new(Some(default_access))) as Arc<dyn AccessController>
        });
        // Clone to register the ticket provider (gate for who can replicate).
        let access_controller_for_registry = access_controller.clone();

        // Tracer
        let tracer = opts.tracer.unwrap_or_else(|| {
            Arc::new(TracerWrapper::Noop(
                NoopTracerProvider::new().tracer("berty.guardian-db"),
            ))
        });

        // Cache (uses sled if a directory is provided, otherwise creates an in-memory cache).
        let cache: Arc<dyn Datastore> = if let Some(cache) = opts.cache {
            cache
        } else {
            let cache_dir = if opts.directory.is_empty() {
                format!("./GuardianDB/{}/cache", addr)
            } else {
                format!("{}/cache", opts.directory)
            };
            Self::create_cache(addr.as_ref(), &cache_dir)?
        };

        // EventEmitter for compatibility with the Store trait.
        let emitter_interface: Arc<dyn crate::events::EmitterInterface + Send + Sync> =
            Arc::new(EventEmitter::new());

        // --- 4. Create, open or import the iroh-docs document ---

        // Ticket exchange key = the store NAME (last segment of the address), consistent
        // across nodes — just like the gossip topic. The full address may vary per node.
        let store_key = addr
            .to_string()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();

        // A node opened read-only must never create a namespace (it would mint its own write
        // secret and become an isolated writer). It may only import an existing one.
        let requested_read_only = opts.read_only.unwrap_or(false);

        // Resolve the DocTicket to use: an explicit ticket (opts) takes priority; otherwise,
        // try AUTOMATIC EXCHANGE with known peers — joining the shared namespace of
        // a peer that already holds this store and that authorizes this node (gated via AccessController).
        let resolved_ticket: Option<String> = match opts.doc_ticket.clone() {
            Some(t) => Some(t),
            None => client.backend().resolve_shared_ticket(&store_key).await,
        };

        // Establish the document, tracking whether this replica holds the namespace write
        // secret (`doc_is_writable`). If a DocTicket was resolved, import the peer's SHARED
        // namespace — the secure replication path via capability: both nodes start using the
        // same namespace and sync (range-based + live) starts with the ticket's peers.
        let (doc_handle, doc_is_writable) = if let Some(ticket_str) = resolved_ticket.as_ref() {
            let ticket = ticket_str
                .parse::<iroh_docs::DocTicket>()
                .map_err(|e| GuardianError::Store(format!("Invalid DocTicket: {}", e)))?;
            // The ticket's capability determines whether we receive the write secret.
            let ticket_writable = matches!(ticket.capability, Capability::Write(_));
            let doc = docs.import_doc(ticket).await?;
            let ns_id = doc.id();
            // Persist the imported NamespaceId and its writability for future reopenings.
            cache
                .put(NAMESPACE_CACHE_KEY, ns_id.as_bytes())
                .await
                .map_err(|e| {
                    GuardianError::Store(format!("Failed to persist imported NamespaceId: {}", e))
                })?;
            Self::persist_writable(cache.as_ref(), ticket_writable).await;
            info!(
                writable = ticket_writable,
                "Imported shared iroh-docs document via ticket: {:?}", ns_id
            );
            (doc, ticket_writable)
        } else {
            // Try to retrieve the NamespaceId from the cache to reopen an existing document.
            match cache.get(NAMESPACE_CACHE_KEY).await {
                Ok(Some(namespace_bytes)) if namespace_bytes.len() == 32 => {
                    // Existing NamespaceId — try to reopen the document.
                    let mut ns_bytes = [0u8; 32];
                    ns_bytes.copy_from_slice(&namespace_bytes);
                    let namespace_id = iroh_docs::NamespaceId::from(ns_bytes);

                    match docs.open_doc(namespace_id).await? {
                        Some(doc) => {
                            // Writability was recorded when the namespace was first established;
                            // legacy stores without the flag are assumed write-capable.
                            let writable = Self::load_writable(cache.as_ref()).await;
                            info!(
                                writable,
                                "Reopened existing iroh-docs document: {:?}", namespace_id
                            );
                            (doc, writable)
                        }
                        None if requested_read_only => {
                            return Err(GuardianError::Store(format!(
                                "Read-only store '{}' cannot create a namespace and the cached \
                                 namespace {:?} was not found; no ticket available to import",
                                store_key, namespace_id
                            )));
                        }
                        None => {
                            // Document not found — create a new one.
                            warn!(
                                "Cached namespace {:?} not found, creating new document",
                                namespace_id
                            );
                            let doc = docs.create_doc().await?;
                            let ns_id = doc.id();
                            cache
                                .put(NAMESPACE_CACHE_KEY, ns_id.as_bytes())
                                .await
                                .map_err(|e| {
                                    GuardianError::Store(format!(
                                        "Failed to persist NamespaceId: {}",
                                        e
                                    ))
                                })?;
                            Self::persist_writable(cache.as_ref(), true).await;
                            info!("Created new iroh-docs document: {:?}", ns_id);
                            (doc, true)
                        }
                    }
                }
                _ if requested_read_only => {
                    return Err(GuardianError::Store(format!(
                        "Read-only store '{}' cannot create a namespace and none was available \
                         to import (no ticket, no cached namespace)",
                        store_key
                    )));
                }
                _ => {
                    // No NamespaceId in the cache — create a new document.
                    let doc = docs.create_doc().await?;
                    let ns_id = doc.id();
                    cache
                        .put(NAMESPACE_CACHE_KEY, ns_id.as_bytes())
                        .await
                        .map_err(|e| {
                            GuardianError::Store(format!("Failed to persist NamespaceId: {}", e))
                        })?;
                    Self::persist_writable(cache.as_ref(), true).await;
                    info!("Created new iroh-docs document: {:?}", ns_id);
                    (doc, true)
                }
            }
        };

        // Effective writability: a node explicitly opened read-only never writes, even if it
        // happens to hold a write-capable namespace (defense in depth).
        let writable = doc_is_writable && !requested_read_only;

        // --- 5. Create an empty Log for compatibility with the Store trait ---

        let empty_log = {
            use crate::log::{AdHocAccess, Log, LogOptions};
            let log_opts = LogOptions {
                id: Some(&db_name),
                access: AdHocAccess,
                entries: &[],
                heads: &[],
                clock: None,
                sort_fn: None,
            };
            Arc::new(RwLock::new(Log::new(
                client.clone(),
                (*identity).clone(),
                log_opts,
            )))
        };

        // --- 6. Create the instance and synchronize the index ---

        let index = Arc::new(KeyValueIndex::new());
        let cached_address = addr.clone();

        let store = GuardianDBKeyValue {
            docs,
            doc_handle,
            author_id,
            access_controller,
            event_bus,
            client,
            identity,
            cached_address,
            db_name,
            cache,
            index,
            span,
            tracer,
            emitter_interface,
            empty_log,
            writable,
        };

        // Synchronize the local index with the iroh-docs document state.
        match store.sync_index_from_docs().await {
            Ok(count) => {
                if count > 0 {
                    info!(
                        "KeyValue store initialized with {} entries from iroh-docs",
                        count
                    );
                }
            }
            Err(e) => {
                warn!(
                    "Failed to sync index on initialization: {:?}. Store will start empty.",
                    e
                );
            }
        }

        // Start the reactive live sync: keeps the index updated as remote entries
        // arrive via Willow sync (essential for P2P replication to reflect in all()/get()).
        store.spawn_live_index_sync();

        // Register this store as a DocTicket provider for authorized peers, enabling
        // automatic namespace exchange on the network. The capability is gated per requester
        // by the AccessController: write-authorized peers get the write ticket (namespace
        // secret), read-only peers get the read ticket (public key only).
        match store.share_tickets().await {
            Ok((read_ticket, write_ticket)) => {
                store
                    .client
                    .backend()
                    .register_ticket_provider(
                        store_key,
                        read_ticket,
                        write_ticket,
                        access_controller_for_registry,
                    )
                    .await;
            }
            Err(e) => {
                warn!(
                    "Failed to generate share tickets, store not registered for exchange: {:?}",
                    e
                );
            }
        }

        info!(
            "GuardianDBKeyValue initialized with iroh-docs backend (namespace={:?}, author={:?})",
            store.doc_handle.id(),
            store.author_id
        );

        Ok(store)
    }

    /// Creates a sled-based cache to persist the NamespaceId.
    fn create_cache(address: &dyn Address, cache_dir: &str) -> Result<Arc<dyn Datastore>> {
        use crate::cache::level_down::LevelDownCache;
        use crate::cache::{Cache, CacheMode, Options};

        let cache_options = Options {
            span: None,
            max_cache_size: Some(100 * 1024 * 1024),
            cache_mode: CacheMode::Auto,
        };

        let cache_manager = LevelDownCache::new(Some(&cache_options));
        let address_string = address.to_string();
        let parsed_address = crate::address::parse(&address_string)
            .map_err(|e| GuardianError::Store(format!("Failed to parse address: {}", e)))?;

        let boxed_datastore = cache_manager
            .load(cache_dir, &parsed_address)
            .map_err(|e| GuardianError::Store(format!("Failed to create cache: {}", e)))?;

        // Wrapper to convert Box<dyn Datastore + Send + Sync> into Arc<dyn Datastore>.
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

        Ok(Arc::new(DatastoreWrapper {
            inner: boxed_datastore,
        }))
    }
}
