use crate::access_control::acl_simple::SimpleAccessController;
use crate::access_control::traits::AccessController;
use crate::address::Address;
use crate::data_store::Datastore;
use crate::events::EventEmitter;
use crate::guardian::error::{GuardianError, Result};
use crate::log::identity::Identity;
use crate::log::lamport_clock::LamportClock;
use crate::p2p::EventBus;
use crate::p2p::network::client::IrohClient;
use crate::p2p::network::core::docs::WillowDocs;
use crate::stores::operation::Operation;
use crate::traits::{
    CreateDocumentDBOptions, DocumentStoreGetOptions, NewStoreOptions, Store, StoreIndex,
    TracerWrapper,
};
use bytes::Bytes;
use iroh_docs::{AuthorId, Capability, api::Doc, store::Query};
use opentelemetry::trace::{TracerProvider, noop::NoopTracerProvider};
use parking_lot::RwLock;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{Span, debug, info, instrument, warn};

/// Represents a generic document.
pub type Document = Value;

/// Cache key used to persist the iroh-docs document's NamespaceId.
const NAMESPACE_CACHE_KEY: &[u8] = b"_iroh_docs_doc_namespace_id";
/// Cache key used to persist whether this replica holds the namespace write secret.
/// Stored as a single byte: 1 = write-capable, 0 = read-only.
const WRITABLE_CACHE_KEY: &[u8] = b"_iroh_docs_doc_writable";

/// Local in-memory index that mirrors the iroh-docs document state.
///
/// Updated atomically after each put/delete operation,
/// serving as a synchronous cache for StoreIndex queries.
pub struct DocumentStoreIndex {
    index: Arc<RwLock<HashMap<String, Vec<u8>>>>,
}

impl Default for DocumentStoreIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl DocumentStoreIndex {
    pub fn new() -> Self {
        Self {
            index: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn get_value(&self, key: &str) -> Option<Vec<u8>> {
        let guard = self.index.read();
        guard.get(key).cloned()
    }

    pub fn keys(&self) -> Vec<String> {
        let guard = self.index.read();
        guard.keys().cloned().collect()
    }

    pub fn insert(&self, key: String, value: Vec<u8>) {
        let mut guard = self.index.write();
        guard.insert(key, value);
    }

    pub fn remove(&self, key: &str) {
        let mut guard = self.index.write();
        guard.remove(key);
    }

    pub fn clear_all(&self) {
        let mut guard = self.index.write();
        guard.clear();
    }

    pub fn len(&self) -> usize {
        let guard = self.index.read();
        guard.len()
    }

    pub fn is_empty(&self) -> bool {
        let guard = self.index.read();
        guard.is_empty()
    }
}

/// Rebuilds the in-memory index from the current state of the iroh-docs document.
/// Shared between `sync_index_from_docs` and the reactive live-sync task.
async fn refresh_doc_index(
    docs: &WillowDocs,
    doc: &Doc,
    client: &Arc<IrohClient>,
    index: &Arc<DocumentStoreIndex>,
) -> Result<usize> {
    let entries = docs
        .get_many(doc, Query::single_latest_per_key().build())
        .await?;

    index.clear_all();
    let mut count = 0;

    for entry in &entries {
        let key = String::from_utf8_lossy(entry.key()).to_string();

        if entry.content_len() == 0 {
            continue;
        }

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
        "DocumentStore index synchronized from iroh-docs: {} entries",
        count
    );
    Ok(count)
}

impl StoreIndex for DocumentStoreIndex {
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
    /// The local index is updated directly after each put/delete operation.
    fn update_index(
        &mut self,
        _log: &crate::log::Log,
        _entries: &[crate::log::entry::Entry],
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn clear(&mut self) -> std::result::Result<(), Self::Error> {
        let mut guard = self.index.write();
        guard.clear();
        Ok(())
    }
}

/// DocumentStore implementation for GuardianDB using iroh-docs (WillowDocs).
///
/// This implementation uses the iroh-docs protocol for distributed document
/// storage with Last-Write-Wins (LWW) conflict resolution, replacing the
/// previous architecture based on BaseStore + OpLog.
///
/// # Architecture
///
/// - **Backend**: iroh-docs (Willow range-based reconciliation)
/// - **Write**: `doc.set_bytes()` / `doc.del()` via WillowDocs
/// - **Read**: Local in-memory index mirroring the iroh-docs state
/// - **Sync**: Automatic via Willow (no manual gossip heads)
/// - **Local index**: In-memory HashMap mirroring the iroh-docs state
pub struct GuardianDBDocumentStore {
    /// WillowDocs backend (iroh-docs) — replaces BaseStore.
    docs: WillowDocs,
    /// iroh-docs document handle for this namespace.
    doc_handle: Doc,
    /// AuthorId for write operations (mapped from the Identity).
    author_id: AuthorId,
    /// Access controller for permission validation.
    access_controller: Arc<dyn AccessController>,
    /// Event bus for reactive notifications.
    event_bus: Arc<EventBus>,
    /// Reference to the IrohClient (Store trait compatibility).
    client: Arc<IrohClient>,
    /// Cryptographic identity of the store.
    identity: Arc<Identity>,
    /// Store address (cached to resolve lifetime issues).
    cached_address: Arc<dyn Address + Send + Sync>,
    /// Database name.
    db_name: String,
    /// Local cache (sled) — used to persist the NamespaceId across reloads.
    cache: Arc<dyn Datastore>,
    /// Local in-memory index mirroring the iroh-docs state.
    index: Arc<DocumentStoreIndex>,
    /// Document options (marshal, unmarshal, key_extractor).
    doc_opts: CreateDocumentDBOptions,
    /// Span for structured tracing.
    span: Span,
    /// Tracer for telemetry.
    tracer: Arc<TracerWrapper>,
    /// Event emission interface (Store trait compatibility).
    emitter_interface: Arc<dyn crate::events::EmitterInterface + Send + Sync>,
    /// Empty log for compatibility with the Store trait (op_log()).
    empty_log: Arc<RwLock<crate::log::Log>>,
    /// Optional: BlobStore for large binary attachments.
    #[allow(dead_code)]
    blob_store: Option<Arc<crate::p2p::network::core::blobs::BlobStore>>,
    /// Whether this replica may originate writes. `false` when opened read-only (via the
    /// `read_only` option) or imported from a read-only `DocTicket` (no namespace secret).
    writable: bool,
}

#[async_trait::async_trait]
impl Store for GuardianDBDocumentStore {
    type Error = GuardianError;

    #[allow(deprecated)]
    fn events(&self) -> &dyn crate::events::EmitterInterface {
        self.emitter_interface.as_ref()
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        debug!("Starting DocumentStore close operation (iroh-docs backend)");
        if let Err(e) = self.docs.close_doc(&self.doc_handle).await {
            warn!("Failed to close iroh-docs document: {:?}", e);
        }
        debug!("DocumentStore close completed");
        Ok(())
    }

    fn address(&self) -> &dyn Address {
        self.cached_address.as_ref()
    }

    fn index(&self) -> Box<dyn crate::traits::StoreIndex<Error = GuardianError> + Send + Sync> {
        Box::new(DocumentStoreIndex {
            index: self.index.index.clone(),
        })
    }

    fn store_type(&self) -> &str {
        "document"
    }

    fn cache(&self) -> Arc<dyn Datastore> {
        self.cache.clone()
    }

    async fn drop(&self) -> std::result::Result<(), Self::Error> {
        debug!("Starting DocumentStore drop operation (iroh-docs backend)");
        self.index.clear_all();
        let namespace_id = self.doc_handle.id();
        if let Err(e) = self.docs.drop_doc(namespace_id).await {
            warn!("Failed to drop iroh-docs document: {:?}", e);
        }
        if let Err(e) = self.cache.delete(NAMESPACE_CACHE_KEY).await {
            warn!("Failed to remove namespace from cache: {:?}", e);
        }
        debug!("DocumentStore drop completed");
        Ok(())
    }

    /// No-op for iroh-docs — Willow sync handles loading automatically.
    async fn load(&self, _amount: usize) -> std::result::Result<(), Self::Error> {
        self.sync_index_from_docs().await?;
        Ok(())
    }

    /// No-op for iroh-docs — Willow sync replaces the gossip heads exchange.
    async fn sync(
        &self,
        _heads: Vec<crate::log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        self.sync_index_from_docs().await?;
        Ok(())
    }

    /// No-op for iroh-docs.
    async fn load_more_from(&self, _amount: u64, _entries: Vec<crate::log::entry::Entry>) {
        // iroh-docs manages its own incremental loading.
    }

    /// No-op for iroh-docs.
    async fn load_from_snapshot(&self) -> std::result::Result<(), Self::Error> {
        self.sync_index_from_docs().await?;
        Ok(())
    }

    /// Returns an empty Log for compatibility with the Store trait.
    /// In the iroh-docs architecture, the OpLog is no longer used.
    fn op_log(&self) -> Arc<parking_lot::RwLock<crate::log::Log>> {
        self.empty_log.clone()
    }

    fn client(&self) -> Arc<IrohClient> {
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
    ) -> std::result::Result<crate::log::entry::Entry, Self::Error> {
        // Canonical write path for the public DocumentStore API: enforce read-only here too.
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

impl GuardianDBDocumentStore {
    /// Returns a reference to the tracing span used for instrumentation.
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Returns the NamespaceId of the underlying iroh-docs document.
    pub fn namespace_id(&self) -> iroh_docs::NamespaceId {
        self.doc_handle.id()
    }

    /// Generates a `DocTicket` (with write capability) for this store's iroh-docs namespace.
    /// The peer that receives the ticket can import the same namespace and replicate securely.
    ///
    /// Note: this grants write capability (carries the namespace secret). For role-gated
    /// sharing, the automatic ticket exchange hands out read or write tickets per requester
    /// (see [`GuardianDBDocumentStore::share_tickets`]).
    pub async fn share_ticket(&self) -> Result<String> {
        let ticket = self.docs.share_doc(&self.doc_handle, true).await?;
        Ok(ticket.to_string())
    }

    /// Generates both the read-only and read-write `DocTicket`s for this store's namespace.
    ///
    /// Returns `(read_ticket, write_ticket)`. The read ticket carries only the namespace
    /// public key (no write secret); the write ticket carries the namespace secret.
    pub async fn share_tickets(&self) -> Result<(String, String)> {
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

    /// Persists the replica's writability flag (best-effort).
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

    /// Returns the AuthorId used for write operations.
    pub fn author_id(&self) -> AuthorId {
        self.author_id
    }

    /// Synchronizes the local index with the current state of the iroh-docs document.
    ///
    /// Queries all document entries and rebuilds the in-memory index.
    pub async fn sync_index_from_docs(&self) -> Result<usize> {
        refresh_doc_index(&self.docs, &self.doc_handle, &self.client, &self.index).await
    }

    /// Starts a background task that keeps the in-memory index synchronized with the
    /// iroh-docs namespace as REMOTE documents arrive via Willow sync.
    fn spawn_live_index_sync(&self) {
        let docs = self.docs.clone();
        let doc = self.doc_handle.clone();
        let client = self.client.clone();
        let index = self.index.clone();

        tokio::spawn(async move {
            let mut stream = match doc.subscribe().await {
                Ok(s) => s,
                Err(e) => {
                    warn!(
                        "Failed to subscribe to iroh-docs doc events (Document): {:?}",
                        e
                    );
                    return;
                }
            };

            use futures::StreamExt;
            use iroh_docs::engine::LiveEvent;
            while let Some(event) = stream.next().await {
                // Rebuild the index ONLY on REMOTE-origin events (peer sync),
                // avoiding a race with local writes (which already update the index directly).
                let is_remote = matches!(
                    event,
                    Ok(LiveEvent::InsertRemote { .. })
                        | Ok(LiveEvent::ContentReady { .. })
                        | Ok(LiveEvent::PendingContentReady)
                        | Ok(LiveEvent::SyncFinished(_))
                );
                if is_remote && let Err(e) = refresh_doc_index(&docs, &doc, &client, &index).await {
                    warn!("Failed to update Document index via live sync: {:?}", e);
                }
            }
            debug!("Live index sync terminated for Document store");
        });
    }

    #[instrument(level = "debug", skip(client, identity, options))]
    pub async fn new(
        client: Arc<IrohClient>,
        identity: Arc<Identity>,
        addr: Arc<dyn Address>,
        mut options: NewStoreOptions,
    ) -> Result<Self> {
        // 1. If store-specific options are not provided, use the default for
        //    documents with an "_id" key.
        if options.store_specific_opts.is_none() {
            let default_opts = default_store_opts_for_map("_id");
            options.store_specific_opts = Some(Box::new(default_opts));
        }

        // 2. Downcast the specific options to the expected type.
        let specific_opts_box = options.store_specific_opts.take().ok_or_else(|| {
            GuardianError::InvalidArgument("StoreSpecificOpts is required".to_string())
        })?;
        let doc_opts = *specific_opts_box
            .downcast::<CreateDocumentDBOptions>()
            .map_err(|_| {
                GuardianError::InvalidArgument(
                    "Invalid type provided for opts.StoreSpecificOpts".to_string(),
                )
            })?;

        // --- 3. Initialize iroh-docs ---

        if !client.has_docs_client().await {
            client.init_docs().await.map_err(|e| {
                GuardianError::Store(format!("Failed to initialize iroh-docs: {}", e))
            })?;
        }

        let mut docs = client.docs_client().await.ok_or_else(|| {
            GuardianError::Store("iroh-docs not available after initialization".to_string())
        })?;

        // --- 4. Get the AuthorId ---

        let author_id = docs.get_or_init_author().await.map_err(|e| {
            GuardianError::Store(format!("Failed to initialize iroh-docs author: {}", e))
        })?;

        // --- 5. Configure the components ---

        let db_name = addr.get_path().to_string();
        let span = tracing::info_span!("document_store", address = %addr.to_string());

        let event_bus = Arc::new(options.event_bus.unwrap_or_default());

        let access_controller = options.access_controller.unwrap_or_else(|| {
            let mut default_access = HashMap::new();
            default_access.insert("write".to_string(), vec!["*".to_string()]);
            Arc::new(SimpleAccessController::new(Some(default_access))) as Arc<dyn AccessController>
        });
        // Clone to register the ticket provider (gate for who can replicate).
        let access_controller_for_registry = access_controller.clone();
        // Ticket exchange key = the store NAME (last segment of the address), consistent
        // across nodes (same as the gossip topic). Captured before `addr` is moved.
        let store_key = addr
            .to_string()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();

        let tracer = options.tracer.unwrap_or_else(|| {
            Arc::new(TracerWrapper::Noop(
                NoopTracerProvider::new().tracer("berty.guardian-db"),
            ))
        });

        let cache: Arc<dyn Datastore> = if let Some(cache) = options.cache {
            cache
        } else {
            let cache_dir = if options.directory.is_empty() {
                format!("./GuardianDB/{}/cache", addr)
            } else {
                format!("{}/cache", options.directory)
            };
            Self::create_cache(addr.as_ref(), &cache_dir)?
        };

        let emitter_interface: Arc<dyn crate::events::EmitterInterface + Send + Sync> =
            Arc::new(EventEmitter::new());

        // --- 6. Create, open or import the iroh-docs document ---

        // A node opened read-only must never create a namespace (it would mint its own write
        // secret and become an isolated writer). It may only import an existing one.
        let requested_read_only = options.read_only.unwrap_or(false);

        // Resolve the DocTicket: explicit (options) takes priority; otherwise try AUTOMATIC EXCHANGE
        // with known peers (joining the shared namespace of an authorized peer).
        let resolved_ticket: Option<String> = match options.doc_ticket.clone() {
            Some(t) => Some(t),
            None => client.backend().resolve_shared_ticket(&store_key).await,
        };

        // Establish the document, tracking whether this replica holds the namespace write
        // secret (`doc_is_writable`). If a DocTicket was resolved, import the peer's SHARED
        // namespace (secure replication via capability). Otherwise, create/reopen locally.
        let (doc_handle, doc_is_writable) = if let Some(ticket_str) = resolved_ticket.as_ref() {
            let ticket = ticket_str
                .parse::<iroh_docs::DocTicket>()
                .map_err(|e| GuardianError::Store(format!("Invalid DocTicket: {}", e)))?;
            // The ticket's capability determines whether we receive the write secret.
            let ticket_writable = matches!(ticket.capability, Capability::Write(_));
            let doc = docs.import_doc(ticket).await?;
            let ns_id = doc.id();
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
            match cache.get(NAMESPACE_CACHE_KEY).await {
                Ok(Some(namespace_bytes)) if namespace_bytes.len() == 32 => {
                    let mut ns_bytes = [0u8; 32];
                    ns_bytes.copy_from_slice(&namespace_bytes);
                    let namespace_id = iroh_docs::NamespaceId::from(ns_bytes);

                    match docs.open_doc(namespace_id).await? {
                        Some(doc) => {
                            // Writability recorded when the namespace was established; legacy
                            // stores without the flag are assumed write-capable.
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

        // --- 7. Create an empty Log for compatibility with the Store trait ---

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

        // --- 8. Initialize the BlobStore (optional, for large attachments) ---

        let blob_store = client.blobs_client().await.map(Arc::new);

        // --- 9. Create the instance and synchronize the index ---

        let index = Arc::new(DocumentStoreIndex::new());
        // Address trait already requires Send + Sync, so this coercion is safe
        let cached_address: Arc<dyn Address + Send + Sync> = addr as Arc<dyn Address + Send + Sync>;

        let store = GuardianDBDocumentStore {
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
            doc_opts,
            span,
            tracer,
            emitter_interface,
            empty_log,
            blob_store,
            writable,
        };

        // Synchronize the local index with the iroh-docs document state.
        match store.sync_index_from_docs().await {
            Ok(count) => {
                if count > 0 {
                    info!(
                        "DocumentStore initialized with {} entries from iroh-docs",
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

        // Start the reactive live sync: keeps the index updated as remote documents
        // arrive via Willow sync (essential for P2P replication to reflect in get()/query()).
        store.spawn_live_index_sync();

        // Register this store as a DocTicket provider for authorized peers (automatic exchange).
        // The capability is gated per requester by the AccessController: write-authorized peers
        // get the write ticket (namespace secret), read-only peers get the read ticket.
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
            "GuardianDBDocumentStore initialized with iroh-docs backend (namespace={:?}, author={:?})",
            store.doc_handle.id(),
            store.author_id
        );

        Ok(store)
    }

    #[instrument(level = "debug", skip(self, opts))]
    pub async fn get(
        &self,
        key: &str,
        opts: Option<DocumentStoreGetOptions>,
    ) -> Result<Vec<Document>> {
        let _entered = self.span.enter();
        let opts = opts.unwrap_or_default();

        let has_multiple_terms = key.contains(' ');
        let mut key_for_search = key.to_string();

        if has_multiple_terms {
            key_for_search = key_for_search.replace('.', " ");
        }
        if opts.case_insensitive {
            key_for_search = key_for_search.to_lowercase();
        }

        let mut documents: Vec<Document> = Vec::new();

        for index_key in self.index.keys() {
            let mut index_key_for_search = index_key.clone();

            if opts.case_insensitive {
                index_key_for_search = index_key_for_search.to_lowercase();
                if has_multiple_terms {
                    index_key_for_search = index_key_for_search.replace('.', " ");
                }
            }

            let matches = if opts.partial_matches {
                index_key_for_search.contains(&key_for_search)
            } else {
                index_key_for_search == key_for_search
            };

            if !matches {
                continue;
            }

            if let Some(value_bytes) = self.index.get_value(&index_key) {
                let doc: Document = serde_json::from_slice(&value_bytes).map_err(|e| {
                    GuardianError::Serialization(format!(
                        "Unable to deserialize the value for key {}: {}",
                        index_key, e
                    ))
                })?;
                documents.push(doc);
            }
        }

        Ok(documents)
    }

    #[instrument(level = "debug", skip(self, document))]
    pub async fn put(&mut self, document: Document) -> Result<Operation> {
        self.ensure_writable()?;
        let _entered = self.span.enter();

        let key = (self.doc_opts.key_extractor)(&document)?;
        let data = (self.doc_opts.marshal)(&document)?;

        // Write directly to iroh-docs (Willow handles sync).
        self.docs
            .set_bytes(
                &self.doc_handle,
                self.author_id,
                Bytes::from(key.clone().into_bytes()),
                Bytes::from(data.clone()),
            )
            .await
            .map_err(|e| {
                GuardianError::Store(format!("Error writing key '{}' to iroh-docs: {}", key, e))
            })?;

        // Update the local index immediately.
        self.index.insert(key.clone(), data.clone());

        debug!("PUT key='{}' ({} bytes) via iroh-docs", key, data.len());

        Ok(Operation::new(Some(key), "PUT".to_string(), Some(data)))
    }

    #[instrument(level = "debug", skip(self))]
    pub async fn delete(&mut self, document_id: &str) -> Result<Operation> {
        self.ensure_writable()?;
        let _entered = self.span.enter();

        // Check whether the entry exists in the local index.
        if self.index.get_value(document_id).is_none() {
            return Err(GuardianError::NotFound(format!(
                "No entry with key '{}' in the database",
                document_id
            )));
        }

        // Remove from iroh-docs (Willow handles sync).
        let deleted = self
            .docs
            .del(
                &self.doc_handle,
                self.author_id,
                Bytes::from(document_id.as_bytes().to_vec()),
            )
            .await
            .map_err(|e| {
                GuardianError::Store(format!(
                    "Error deleting key '{}' in iroh-docs: {}",
                    document_id, e
                ))
            })?;

        // Update the local index immediately.
        self.index.remove(document_id);

        debug!(
            "DEL key='{}' ({} entries removed) via iroh-docs",
            document_id, deleted
        );

        Ok(Operation::new(
            Some(document_id.to_string()),
            "DEL".to_string(),
            None,
        ))
    }

    #[instrument(level = "debug", skip(self, documents))]
    pub async fn put_batch(&mut self, documents: Vec<Document>) -> Result<Vec<Operation>> {
        self.ensure_writable()?;
        if documents.is_empty() {
            return Err(GuardianError::InvalidArgument(
                "Nothing to add to the store".to_string(),
            ));
        }

        let mut operations = Vec::new();

        for doc in documents {
            let op = self.put(doc).await?;
            operations.push(op);
        }

        Ok(operations)
    }

    #[instrument(level = "debug", skip(self, documents))]
    pub async fn put_all(&mut self, documents: Vec<Document>) -> Result<Operation> {
        self.ensure_writable()?;
        if documents.is_empty() {
            return Err(GuardianError::InvalidArgument(
                "Nothing to add to the store".to_string(),
            ));
        }

        let mut to_add: Vec<(String, Vec<u8>)> = Vec::new();

        for doc in documents {
            let key = (self.doc_opts.key_extractor)(&doc).map_err(|_| {
                GuardianError::InvalidArgument(
                    "One of the provided documents has no index key".to_string(),
                )
            })?;

            let data = (self.doc_opts.marshal)(&doc).map_err(|_| {
                GuardianError::Serialization(
                    "Could not serialize one of the provided documents".to_string(),
                )
            })?;

            to_add.push((key, data));
        }

        // Each document is an individual set_bytes (iroh-docs has no batch API).
        for (key, data) in &to_add {
            self.docs
                .set_bytes(
                    &self.doc_handle,
                    self.author_id,
                    Bytes::from(key.clone().into_bytes()),
                    Bytes::from(data.clone()),
                )
                .await
                .map_err(|e| {
                    GuardianError::Store(format!("Error writing key '{}' to iroh-docs: {}", key, e))
                })?;

            // Update the local index immediately.
            self.index.insert(key.clone(), data.clone());
        }

        debug!("PUTALL {} documents via iroh-docs", to_add.len());

        // Return an Operation representing the batch.
        let first_key = to_add.first().map(|(k, _)| k.clone());
        Ok(Operation::new(first_key, "PUTALL".to_string(), None))
    }

    #[instrument(level = "debug", skip(self, filter))]
    pub fn query<F>(&self, mut filter: F) -> Result<Vec<Document>>
    where
        F: FnMut(&Document) -> Result<bool>,
    {
        let mut results: Vec<Document> = Vec::new();

        for index_key in self.index.keys() {
            if let Some(doc_bytes) = self.index.get_value(&index_key) {
                let doc: Document = serde_json::from_slice(&doc_bytes).map_err(|e| {
                    GuardianError::Serialization(format!(
                        "Could not deserialize the document: {}",
                        e
                    ))
                })?;

                if filter(&doc)? {
                    results.push(doc);
                }
            }
        }

        Ok(results)
    }

    pub fn store_type(&self) -> &'static str {
        "document"
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

/// Returns a closure that extracts a field from a `serde_json::Value::Object`.
///
/// The returned closure captures the `key_field` for later use.
pub fn map_key_extractor(key_field: String) -> impl Fn(&Document) -> Result<String> {
    move |doc: &Document| {
        // Ensure the document is a JSON object (map).
        let obj = doc.as_object().ok_or_else(|| {
            GuardianError::InvalidArgument(
                "The entry must be a JSON object (map[string]interface{{}})".to_string(),
            )
        })?;

        // Look up the key field in the object.
        let value = obj.get(&key_field).ok_or_else(|| {
            GuardianError::NotFound(format!(
                "Missing value for field `{}` in the entry",
                key_field
            ))
        })?;

        // Ensure the found value is a string.
        let key = value.as_str().ok_or_else(|| {
            GuardianError::InvalidArgument(format!(
                "The value for field `{}` is not a string",
                key_field
            ))
        })?;

        // Validate that the key is not empty.
        if key.is_empty() {
            return Err(GuardianError::InvalidArgument(format!(
                "The field `{}` cannot be an empty string",
                key_field
            )));
        }

        Ok(key.to_string())
    }
}

/// Creates a default set of options for a store that handles map-based documents
/// (JSON Objects), using a specific field as the key.
pub fn default_store_opts_for_map(key_field: &str) -> CreateDocumentDBOptions {
    CreateDocumentDBOptions {
        marshal: Arc::new(|doc: &Document| serde_json::to_vec(doc).map_err(GuardianError::from)),
        unmarshal: Arc::new(|bytes: &[u8]| {
            serde_json::from_slice(bytes).map_err(GuardianError::from)
        }),
        // Use the higher-order function to create the key-extractor closure.
        key_extractor: Arc::new(map_key_extractor(key_field.to_string())),

        item_factory: Arc::new(|| Value::Object(Map::new())),
    }
}
