use crate::access_control::{
    manifest::ManifestParams, traits::AccessController, traits::Option as AccessControllerOption,
};
use crate::address::Address;
use crate::data_store::Datastore;
use crate::events::{self, EmitterInterface};
use crate::guardian::error::GuardianError;
use crate::log::{Log, entry::Entry, identity::Identity};
use crate::p2p::EventBus;
use crate::p2p::network::client::IrohClient;
use crate::stores::operation::Operation;
use futures::stream::Stream;
use iroh::EndpointId as NodeId;
use iroh_blobs::Hash;
use opentelemetry::global::{BoxedSpan, BoxedTracer};
use opentelemetry::trace::{Tracer, noop::NoopTracer};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::Span;

// Type aliases to reduce type complexity.
type KeyExtractorFn =
    Arc<dyn Fn(&serde_json::Value) -> Result<String, GuardianError> + Send + Sync>;
type MarshalFn = Arc<dyn Fn(&serde_json::Value) -> Result<Vec<u8>, GuardianError> + Send + Sync>;
type UnmarshalFn = Arc<dyn Fn(&[u8]) -> Result<serde_json::Value, GuardianError> + Send + Sync>;
type ItemFactoryFn = Arc<dyn Fn() -> serde_json::Value + Send + Sync>;
type CleanupCallback = Box<
    dyn FnOnce() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> + Send + Sync,
>;

// Local definition of SortFn (moved from the replicator).
pub type SortFn = fn(&Entry, &Entry) -> std::cmp::Ordering;

// Type aliases to improve readability of complex signatures.
/// Alias for thread-safe dynamic documents.
pub type Document = Box<dyn Any + Send + Sync>;

/// Alias for the standard result with GuardianError.
pub type GuardianResult<T> = std::result::Result<T, GuardianError>;

/// Alias for asynchronous query filters.
pub type AsyncDocumentFilter = Pin<
    Box<
        dyn Fn(
                &Document,
            )
                -> Pin<Box<dyn Future<Output = Result<bool, Box<dyn Error + Send + Sync>>> + Send>>
            + Send
            + Sync,
    >,
>;

/// Alias for the progress callback.
pub type ProgressCallback = mpsc::Sender<Entry>;

/// Wrapper for different tracer types, integrated with the tracing system.
///
/// This enum allows using both OpenTelemetry tracers and Rust's native
/// tracing system transparently.
#[derive(Default)]
pub enum TracerWrapper {
    /// OpenTelemetry tracer for distributed observability.
    OpenTelemetry(Arc<BoxedTracer>),
    /// Tracer based on Rust's native tracing system.
    #[default]
    Tracing,
    /// No-op tracer for when telemetry is disabled.
    Noop(NoopTracer),
}

impl Clone for TracerWrapper {
    fn clone(&self) -> Self {
        match self {
            TracerWrapper::OpenTelemetry(tracer) => TracerWrapper::OpenTelemetry(tracer.clone()),
            TracerWrapper::Tracing => TracerWrapper::Tracing,
            TracerWrapper::Noop(_) => TracerWrapper::Noop(NoopTracer::new()),
        }
    }
}

impl TracerWrapper {
    /// Creates a new TracerWrapper using the native tracing system.
    pub fn new_tracing() -> Self {
        TracerWrapper::Tracing
    }

    /// Creates a new TracerWrapper using OpenTelemetry.
    pub fn new_opentelemetry(tracer: Arc<BoxedTracer>) -> Self {
        TracerWrapper::OpenTelemetry(tracer)
    }

    /// Creates a no-op TracerWrapper.
    pub fn new_noop() -> Self {
        TracerWrapper::Noop(NoopTracer::new())
    }

    /// Starts a new instrumented span.
    ///
    /// This method creates spans consistently regardless of
    /// the type of tracer being used.
    pub fn start_span(&self, name: &str) -> TracerSpan {
        match self {
            TracerWrapper::OpenTelemetry(tracer) => {
                // For OpenTelemetry, create a span using the Tracer trait.
                let span = tracer.start(name.to_string());
                TracerSpan::OpenTelemetry(span)
            }
            TracerWrapper::Tracing => {
                // For native tracing, use the tracing::span! macro.
                let span = tracing::info_span!("guardian_db", operation = name);
                TracerSpan::Tracing(span)
            }
            TracerWrapper::Noop(_) => {
                // For no-op, return an empty span.
                TracerSpan::Noop
            }
        }
    }

    /// Checks whether the tracer is active (not a no-op).
    pub fn is_active(&self) -> bool {
        !matches!(self, TracerWrapper::Noop(_))
    }

    /// Returns the tracer type as a string for logs/debugging.
    pub fn tracer_type(&self) -> &'static str {
        match self {
            TracerWrapper::OpenTelemetry(_) => "opentelemetry",
            TracerWrapper::Tracing => "tracing",
            TracerWrapper::Noop(_) => "noop",
        }
    }
}

/// Enum to represent different types of instrumented spans.
///
/// Allows working with spans from different tracing systems
/// in a unified way.
pub enum TracerSpan {
    /// OpenTelemetry span for distributed observability.
    OpenTelemetry(BoxedSpan),
    /// Span from Rust's native tracing system.
    Tracing(tracing::Span),
    /// No-op span for when telemetry is disabled.
    Noop,
}

impl TracerSpan {
    /// Adds an attribute/field to the span.
    pub fn set_attribute<T: Into<opentelemetry::Value>>(&mut self, key: &str, value: T) {
        match self {
            TracerSpan::OpenTelemetry(span) => {
                use opentelemetry::trace::Span as OtelSpan;
                span.set_attribute(opentelemetry::KeyValue::new(key.to_string(), value));
            }
            TracerSpan::Tracing(span) => {
                // For tracing, record it as an event within the span.
                span.in_scope(|| {
                    tracing::info!(key = %format!("{:?}", value.into()), "span_attribute");
                });
            }
            TracerSpan::Noop => {
                // No-op - does nothing.
            }
        }
    }

    /// Records an event on the span.
    pub fn add_event(&mut self, name: &str, attributes: Vec<(&str, &str)>) {
        match self {
            TracerSpan::OpenTelemetry(span) => {
                use opentelemetry::trace::Span as OtelSpan;
                let attrs: Vec<opentelemetry::KeyValue> = attributes
                    .into_iter()
                    .map(|(k, v)| opentelemetry::KeyValue::new(k.to_string(), v.to_string()))
                    .collect();
                span.add_event(name.to_string(), attrs);
            }
            TracerSpan::Tracing(span) => {
                // For tracing, record it as a structured event.
                span.in_scope(|| {
                    let fields: std::collections::HashMap<&str, &str> =
                        attributes.into_iter().collect();
                    tracing::info!(event = name, ?fields, "span_event");
                });
            }
            TracerSpan::Noop => {
                // No-op - does nothing.
            }
        }
    }

    /// Marks the span as an error.
    pub fn set_error<E: std::fmt::Display>(&mut self, error: E) {
        match self {
            TracerSpan::OpenTelemetry(span) => {
                use opentelemetry::trace::Span as OtelSpan;
                span.set_status(opentelemetry::trace::Status::Error {
                    description: std::borrow::Cow::Owned(error.to_string()),
                });
                span.set_attribute(opentelemetry::KeyValue::new("error".to_string(), true));
                span.set_attribute(opentelemetry::KeyValue::new(
                    "error.message".to_string(),
                    error.to_string(),
                ));
            }
            TracerSpan::Tracing(span) => {
                span.in_scope(|| {
                    tracing::error!(error = %error, "span_error");
                });
            }
            TracerSpan::Noop => {
                // No-op - does nothing.
            }
        }
    }

    /// Finishes the span explicitly.
    pub fn finish(mut self) {
        match &mut self {
            TracerSpan::OpenTelemetry(_span) => {
                // OpenTelemetry spans are finished automatically on Drop,
                // but we can mark them as complete here if needed.
            }
            TracerSpan::Tracing(_span) => {
                // Tracing spans are finished automatically when they go out of scope.
                // Nothing needs to be done here.
            }
            TracerSpan::Noop => {
                // No-op - does nothing.
            }
        }
        // Drop will be called automatically when self goes out of scope.
    }
}

impl Drop for TracerSpan {
    fn drop(&mut self) {
        // For OpenTelemetry, we ensure the span is finished.
        match self {
            TracerSpan::OpenTelemetry(_span) => {
                // OpenTelemetry spans are finished automatically on Drop.
                // We do not need to call end() explicitly here.
            }
            TracerSpan::Tracing(_) => {
                // Tracing spans are finished automatically when they go out of scope.
            }
            TracerSpan::Noop => {
                // No-op - does nothing.
            }
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MessageExchangeHeads {
    #[serde(rename = "address")]
    pub address: String,

    #[serde(rename = "heads")]
    pub heads: Vec<Entry>,
}

pub trait MessageMarshaler: Send + Sync {
    /// Defines an associated error type for implementation flexibility.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Serializes a message into a byte vector.
    fn marshal(&self, msg: &MessageExchangeHeads) -> Result<Vec<u8>, Self::Error>;

    /// Deserializes a byte vector into a message.
    fn unmarshal(&self, data: &[u8]) -> Result<MessageExchangeHeads, Self::Error>;
}

#[derive(Default)]
pub struct CreateDBOptions {
    pub event_bus: Option<EventBus>,
    pub directory: Option<String>,
    pub overwrite: Option<bool>,
    pub local_only: Option<bool>,
    pub create: Option<bool>,
    pub store_type: Option<String>,
    pub access_controller_address: Option<String>,
    pub access_controller: Option<Box<dyn ManifestParams>>,
    pub replicate: Option<bool>,
    pub keystore: Option<Arc<dyn crate::log::identity_provider::Keystore>>,
    pub cache: Option<Arc<dyn Datastore>>,
    pub identity: Option<Identity>,
    pub sort_fn: Option<SortFn>,
    pub timeout: Option<Duration>,
    pub message_marshaler: Option<Arc<dyn MessageMarshaler<Error = GuardianError>>>,
    pub span: Option<Span>,
    pub close_func: Option<Box<dyn FnOnce() + Send>>,
    pub store_specific_opts: Option<Box<dyn Any + Send + Sync>>,
    /// `DocTicket` (serialized) to import an iroh-docs namespace shared by a peer.
    /// Used by iroh-docs-based stores (KeyValue/Document) for secure replication via
    /// capability: when present, the store imports the ticket's namespace instead of creating a new one.
    pub doc_ticket: Option<String>,
    /// Marks this store as a **read-only replica**. When `Some(true)`, iroh-docs-based stores
    /// refuse local writes (`put`/`delete`) and never create a new namespace — they only import
    /// an existing one (from `doc_ticket` or a peer). This enforces, at the node level, that a
    /// designated reader cannot originate writes even if the namespace write secret is present.
    pub read_only: Option<bool>,
}

impl Clone for CreateDBOptions {
    fn clone(&self) -> Self {
        Self {
            event_bus: self.event_bus.clone(),
            directory: self.directory.clone(),
            overwrite: self.overwrite,
            local_only: self.local_only,
            create: self.create,
            store_type: self.store_type.clone(),
            access_controller_address: self.access_controller_address.clone(),
            access_controller: None, // Cannot clone Box<dyn ManifestParams>
            replicate: self.replicate,
            keystore: self.keystore.clone(),
            cache: self.cache.clone(),
            identity: self.identity.clone(),
            sort_fn: self.sort_fn,
            timeout: self.timeout,
            message_marshaler: self.message_marshaler.clone(),
            span: self.span.clone(),
            close_func: None,          // Cannot clone Box<dyn FnOnce()>
            store_specific_opts: None, // Cannot clone Box<dyn Any>
            doc_ticket: self.doc_ticket.clone(),
            read_only: self.read_only,
        }
    }
}

// Using Arc<dyn Fn> instead of Box<dyn Fn> to allow cloning.
pub type StoreConstructor = Arc<
    dyn Fn(
            Arc<IrohClient>,
            Arc<Identity>,
            Box<dyn Address>,
            NewStoreOptions,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Box<dyn Store<Error = GuardianError>>, GuardianError>>
                    + Send,
            >,
        > + Send
        + Sync,
>;

#[derive(Clone)]
pub struct CreateDocumentDBOptions {
    /// Extracts the key from a generic document.
    pub key_extractor: KeyExtractorFn,

    /// Serializes a generic document to bytes.
    pub marshal: MarshalFn,

    /// Deserializes bytes into a generic document.
    pub unmarshal: UnmarshalFn,

    /// Creates a new empty instance of the document's item type.
    pub item_factory: ItemFactoryFn,
}

#[derive(Default, Clone)]
pub struct DetermineAddressOptions {
    pub only_hash: Option<bool>,
    pub replicate: Option<bool>,
    pub access_controller: crate::access_control::manifest::CreateAccessControllerOptions,
}

#[async_trait::async_trait]
pub trait BaseGuardianDB: Send + Sync {
    /// Defines an associated error type for implementation flexibility.
    type Error: Error + Send + Sync + 'static;

    /// Returns the GuardianDB Client.
    fn client(&self) -> Arc<IrohClient>;

    /// Returns the identity used by GuardianDB.
    fn identity(&self) -> Arc<Identity>;

    /// Creates or opens a store with the provided address and options.
    async fn open(
        &self,
        address: &str,
        options: &mut CreateDBOptions,
    ) -> Result<Arc<dyn Store<Error = GuardianError>>, Self::Error>;

    /// Returns a store instance if it is already open.
    fn get_store(&self, address: &str) -> Option<Arc<dyn Store<Error = GuardianError>>>;

    /// Creates a new store with the provided name, type and options.
    async fn create(
        &self,
        name: &str,
        store_type: &str,
        options: &mut CreateDBOptions,
    ) -> Result<Arc<dyn Store<Error = GuardianError>>, Self::Error>;

    /// Determines a database's address based on its parameters.
    async fn determine_address(
        &self,
        name: &str,
        store_type: &str,
        options: &DetermineAddressOptions,
    ) -> Result<Box<dyn Address>, Self::Error>;

    /// Registers a new Store type.
    fn register_store_type(&mut self, store_type: &str, constructor: StoreConstructor);

    /// Unregisters a Store type.
    fn unregister_store_type(&mut self, store_type: &str);

    /// Registers a new Access Controller type.
    fn register_access_controller_type(
        &mut self,
        constructor: AccessControllerConstructor,
    ) -> Result<(), Self::Error>;

    /// Unregisters an Access Controller type.
    fn unregister_access_controller_type(&mut self, controller_type: &str);

    /// Gets an Access Controller constructor by its type.
    fn get_access_controller_type(
        &self,
        controller_type: &str,
    ) -> Option<AccessControllerConstructor>;

    /// Returns the event bus.
    fn event_bus(&self) -> EventBus;

    /// Returns the span for tracing.
    fn span(&self) -> &tracing::Span;

    /// Returns the tracer for telemetry.
    fn tracer(&self) -> Arc<TracerWrapper>;
}

/// Exposes a method to create or open a `DocumentStore`.
#[async_trait::async_trait]
pub trait GuardianDBDocumentStoreProvider {
    /// Defines an associated error type for this trait.
    type Error: Error + Send + Sync + 'static;

    /// Creates or opens a DocumentStore.
    async fn docs(
        &self,
        address: &str,
        options: &mut CreateDBOptions,
    ) -> Result<Box<dyn DocumentStore<Error = GuardianError>>, Self::Error>;
}
/// Combines the `BaseGuardianDB` and `GuardianDBDocumentStoreProvider` traits.
pub trait GuardianDBDocumentStore: BaseGuardianDB + GuardianDBDocumentStoreProvider {}

// "Blanket" implementation that automatically applies the `GuardianDBDocumentStore` trait.
impl<T: BaseGuardianDB + GuardianDBDocumentStoreProvider> GuardianDBDocumentStore for T {}

/// Exposes a method to create or open a `KeyValueStore`.
#[async_trait::async_trait]
pub trait GuardianDBKVStoreProvider: Send + Sync {
    /// Defines an associated error type for this trait.
    type Error: Error + Send + Sync + 'static;

    /// Creates or opens a KeyValueStore.
    async fn key_value(
        &self,
        address: &str,
        options: &mut CreateDBOptions,
    ) -> Result<Box<dyn KeyValueStore<Error = GuardianError>>, Self::Error>;
}

/// Combines the `BaseGuardianDB` and `GuardianDBKVStoreProvider` traits.
pub trait GuardianDBKVStore: BaseGuardianDB + GuardianDBKVStoreProvider {}

// "Blanket" implementation that automatically applies the `GuardianDBKVStore` trait
// to any type that already satisfies the conditions.
impl<T: BaseGuardianDB + GuardianDBKVStoreProvider> GuardianDBKVStore for T {}

/// Exposes a method to create or open an `EventLogStore`.
#[async_trait::async_trait]
pub trait GuardianDBLogStoreProvider {
    /// Defines an associated error type for this trait.
    type Error: Error + Send + Sync + 'static;

    /// Creates or opens an EventLogStore (an append-only event log).
    async fn log(
        &self,
        address: &str,
        options: &mut CreateDBOptions,
    ) -> Result<Box<dyn EventLogStore<Error = GuardianError>>, Self::Error>;
}

/// Combines the `BaseGuardianDB` and `GuardianDBLogStoreProvider` traits.
pub trait GuardianDBLogStore: BaseGuardianDB + GuardianDBLogStoreProvider {}

// "Blanket" implementation for `GuardianDBLogStore`.
impl<T: BaseGuardianDB + GuardianDBLogStoreProvider> GuardianDBLogStore for T {}

/// Combines all of GuardianDB's main traits.
pub trait GuardianDB:
    BaseGuardianDB
    + GuardianDBKVStoreProvider
    + GuardianDBLogStoreProvider
    + GuardianDBDocumentStoreProvider
{
}

// The "blanket" implementation allows any type that already satisfies all
// the constraints to be automatically considered a `GuardianDB`.
impl<
    T: BaseGuardianDB
        + GuardianDBKVStoreProvider
        + GuardianDBLogStoreProvider
        + GuardianDBDocumentStoreProvider,
> GuardianDB for T
{
}

#[derive(Default, Debug, Clone)]
pub struct StreamOptions {
    /// "Greater Than": Returns entries that come after the provided Hash.
    pub gt: Option<Hash>,

    /// "Greater Than or Equal": Returns entries that are the provided Hash or later.
    pub gte: Option<Hash>,

    /// "Less Than": Returns entries that come before the provided Hash.
    pub lt: Option<Hash>,

    /// "Less Than or Equal": Returns entries that are the provided Hash or earlier.
    pub lte: Option<Hash>,

    /// Limits the number of entries to be returned.
    pub amount: Option<i32>,
}

pub trait StoreEvents {
    fn subscribe(&mut self);
}

/// Defines the operations common to all store types.
#[async_trait::async_trait]
pub trait Store: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    #[deprecated(note = "use event_bus() instead")]
    fn events(&self) -> &dyn EmitterInterface;

    /// Closes the store and releases its resources.
    /// Modified to accept &self instead of &mut self for compatibility with Arc<T>.
    async fn close(&self) -> Result<(), Self::Error>;

    /// Returns the store's address.
    fn address(&self) -> &dyn Address;

    /// Returns the store's index, which maintains the current state of the data.
    /// Returns Box to avoid lifetime issues with RwLock.
    fn index(&self) -> Box<dyn StoreIndex<Error = Self::Error> + Send + Sync>;

    /// Returns the store type as a string (e.g. "eventlog", "kvstore").
    fn store_type(&self) -> &str;

    /// Returns the store's cache.
    fn cache(&self) -> Arc<dyn Datastore>;

    /// Removes all of the store's local content.
    async fn drop(&self) -> Result<(), Self::Error>;

    /// Loads the `amount` most recent entries from the network.
    async fn load(&self, amount: usize) -> Result<(), Self::Error>;

    /// Synchronizes the store with a list of `heads` (most recent entries) from another peer.
    async fn sync(&self, heads: Vec<Entry>) -> Result<(), Self::Error>;

    /// Loads more entries from a set of known CIDs.
    async fn load_more_from(&self, amount: u64, entries: Vec<Entry>);

    /// Loads the store's content from a snapshot.
    async fn load_from_snapshot(&self) -> Result<(), Self::Error>;

    /// Returns the underlying operation log (OpLog).
    /// Modified to return Arc to avoid lifetime issues.
    fn op_log(&self) -> Arc<RwLock<Log>>;

    /// Returns the GuardianDB Client.
    fn client(&self) -> Arc<IrohClient>;

    /// Returns the database name.
    fn db_name(&self) -> &str;

    /// Returns the identity used by the store.
    fn identity(&self) -> &Identity;

    /// Returns the store's access controller.
    fn access_controller(&self) -> &dyn AccessController;

    /// Adds a new operation to the store.
    async fn add_operation(
        &self,
        op: Operation,
        on_progress_callback: Option<ProgressCallback>,
    ) -> Result<Entry, Self::Error>;

    /// Returns the span.
    /// Modified to return Arc to avoid lifetime issues.
    fn span(&self) -> Arc<Span>;

    /// Returns the tracer for telemetry.
    fn tracer(&self) -> Arc<TracerWrapper>;

    /// Returns the event bus.
    fn event_bus(&self) -> Arc<EventBus>;

    /// Helper method for downcasting.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// A store that behaves like a distributed "append-only" event log.
/// Inherits all functionality from the `Store` trait and adds operations
/// specific to immutable sequential logs.
///
/// Ideal for use cases such as auditing, event sourcing, and systems
/// that require a complete, ordered event history.
#[async_trait::async_trait]
pub trait EventLogStore: Store {
    /// Adds a new piece of data to the log.
    /// The data is appended sequentially and immutably.
    ///
    /// # Arguments
    /// * `data` - The binary data to be added to the log
    ///
    /// # Returns
    /// The created ADD operation, containing metadata of the added event
    async fn add(&self, data: Vec<u8>) -> Result<Operation, Self::Error>;

    /// Gets a specific log entry by its Hash.
    /// Allows direct access to any historical entry.
    ///
    /// # Arguments
    /// * `hash` - The Hash of the desired entry
    ///
    /// # Returns
    /// The operation corresponding to the Hash, or an error if not found
    async fn get(&self, hash: &Hash) -> Result<Operation, Self::Error>;

    /// Returns a stream of operations, with filter options.
    /// In Rust, instead of passing a channel, it is idiomatic to return a `Stream`.
    ///
    /// # TODO
    /// This functionality requires careful Stream implementation to avoid
    /// lifetime issues. For now, use `list()` for synchronous cases.
    ///
    /// # Future implementation
    /// ```ignore
    /// async fn stream(&self, options: Option<StreamOptions>)
    ///     -> Result<Pin<Box<dyn Stream<Item = Operation> + Send>>, Self::Error>;
    /// ```
    /// Returns a list of operations that occurred in the store, with filter options.
    /// Allows historical queries with specific time/position criteria.
    ///
    /// # Arguments
    /// * `options` - Optional filters to limit/order the results
    ///
    /// # Returns
    /// An ordered list of operations that meet the criteria
    async fn list(&self, options: Option<StreamOptions>) -> Result<Vec<Operation>, Self::Error>;
}

/// A store that behaves like a distributed key-value database.
/// Inherits all functionality from the `Store` trait and adds operations
/// specific to key-value pairs with CRDT semantics.
///
/// All operations are replicated automatically across the network
/// and maintain eventual consistency among peers.
#[async_trait::async_trait]
pub trait KeyValueStore: Store {
    /// Returns all of the store's key-value pairs in a map.
    /// This operation reads the current state of the local index.
    fn all(&self) -> std::collections::HashMap<String, Vec<u8>>;

    /// Sets a value for a specific key.
    /// Creates a new PUT operation in the distributed log that will be replicated.
    ///
    /// # Arguments
    /// * `key` - The key to associate with the value (cannot be empty)
    /// * `value` - The binary data to be stored
    ///
    /// # Returns
    /// The created PUT operation, or an error if the operation fails
    async fn put(&self, key: &str, value: Vec<u8>) -> Result<Operation, Self::Error>;

    /// Removes a key and its associated value.
    /// Creates a new DEL operation in the distributed log that will be replicated.
    ///
    /// # Arguments
    /// * `key` - The key to be removed
    ///
    /// # Returns
    /// The created DEL operation, or an error if the key does not exist or the operation fails
    async fn delete(&self, key: &str) -> Result<Operation, Self::Error>;

    /// Gets the value associated with a key.
    /// Queries the local index for the most recent state.
    ///
    /// # Arguments
    /// * `key` - The key to look up
    ///
    /// # Returns
    /// `Some(Vec<u8>)` if the key exists, `None` if it does not, or an error if access fails
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Generates a (serialized) `DocTicket` that grants a peer access to synchronize this store.
    ///
    /// The ticket is a capability: only whoever receives it can import the underlying
    /// iroh-docs namespace and replicate the data. The peer must open the store passing
    /// the ticket in [`CreateDBOptions::doc_ticket`]. Stores that do not use iroh-docs should return an error.
    async fn share_ticket(&self) -> Result<String, Self::Error>;
}

/// A simple struct for passing options to a DocumentStore's `get` method.
#[derive(Default, Debug, Clone, Copy)]
pub struct DocumentStoreGetOptions {
    pub case_insensitive: bool,
    pub partial_matches: bool,
}

#[derive(Default, Debug, Clone)]
pub struct DocumentStoreQueryOptions {
    pub limit: Option<usize>,
    pub skip: Option<usize>,
    pub sort: Option<String>,
}

/// A store that handles documents (semi-structured objects).
///
/// This trait combines basic store functionality with document-specific
/// operations, including advanced queries and batch operations.
#[async_trait::async_trait]
pub trait DocumentStore: Store {
    /// Stores a single document.
    /// The document must implement the Send + Sync traits for thread safety.
    async fn put(&self, document: Document) -> Result<Operation, Self::Error>;

    /// Deletes a document by its key.
    /// Returns the delete operation that was applied to the log.
    async fn delete(&self, key: &str) -> Result<Operation, Self::Error>;

    /// Adds multiple documents in separate operations and returns the last one.
    /// Each document is processed individually, creating a separate log entry.
    async fn put_batch(&self, values: Vec<Document>) -> Result<Operation, Self::Error>;

    /// Adds multiple documents in a single operation and returns it.
    /// All documents are included in a single log entry.
    async fn put_all(&self, values: Vec<Document>) -> Result<Operation, Self::Error>;

    /// Retrieves documents by a key, with search options.
    /// Supports case-insensitive search and partial matches based on the options.
    async fn get(
        &self,
        key: &str,
        opts: Option<DocumentStoreGetOptions>,
    ) -> Result<Vec<Document>, Self::Error>;

    /// Finds documents using a filter function (predicate).
    async fn query(&self, filter: AsyncDocumentFilter) -> Result<Vec<Document>, Self::Error>;

    /// Generates a (serialized) `DocTicket` that grants a peer access to synchronize this store.
    ///
    /// Replication capability: the peer must open the store passing the ticket in
    /// [`CreateDBOptions::doc_ticket`]. Stores that do not use iroh-docs should return an error.
    async fn share_ticket(&self) -> Result<String, Self::Error>;
}

/// Index holds the current state of a store. It processes the operation
/// log (`OpLog`) to build the most recent view of the data,
/// implementing the CRDT logic.
pub trait StoreIndex: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    /// Checks whether a key exists in the index.
    /// A safe method that does not require access to the data itself.
    fn contains_key(&self, key: &str) -> std::result::Result<bool, Self::Error>;

    /// Returns a copy of the data for a specific key as bytes.
    /// A safe method that works with any synchronization implementation.
    fn get_bytes(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error>;

    /// Returns all keys available in the index.
    /// Useful for iteration and listing operations.
    fn keys(&self) -> std::result::Result<Vec<String>, Self::Error>;

    /// Returns the number of entries in the index.
    fn len(&self) -> std::result::Result<usize, Self::Error>;

    /// Checks whether the index is empty.
    fn is_empty(&self) -> std::result::Result<bool, Self::Error>;

    /// Updates the index by applying new entries from the operation log.
    /// Takes `&mut self` because this method modifies the index state.
    fn update_index(
        &mut self,
        log: &Log,
        entries: &[Entry],
    ) -> std::result::Result<(), Self::Error>;

    /// Clears all data from the index.
    /// Useful for reset or full rebuild.
    fn clear(&mut self) -> std::result::Result<(), Self::Error>;

    // === OPTIONAL OPTIMIZATION METHODS ===

    /// Returns a range of full entries (if supported by the index).
    ///
    /// This optional method allows indexes that keep full Entries
    /// to expose optimized direct access for range queries.
    ///
    /// # Arguments
    ///
    /// * `start` - Starting index (inclusive)
    /// * `end` - Ending index (exclusive)
    ///
    /// # Returns
    ///
    /// `Some(Vec<Entry>)` if the index supports direct access to Entry
    /// `None` if the index does not support it or the range is invalid
    ///
    /// # Performance
    ///
    /// - O(1) for range validation
    /// - O(end - start) for collecting the results
    /// - Avoids deserializing bytes into Entry
    fn get_entries_range(&self, _start: usize, _end: usize) -> Option<Vec<Entry>> {
        // ***Default implementation returns None - indexes that support it can override.
        None
    }

    /// Returns the last N entries (if supported by the index).
    ///
    /// A common optimization for EventLogStore where we frequently
    /// want the most recent entries.
    ///
    /// # Arguments
    ///
    /// * `count` - Number of entries to return
    ///
    /// # Returns
    ///
    /// `Some(Vec<Entry>)` if the index supports direct access
    /// `None` if not supported
    fn get_last_entries(&self, _count: usize) -> Option<Vec<Entry>> {
        // ***Default implementation returns None.
        None
    }

    /// Returns a specific Entry by Hash (if supported by the index).
    ///
    /// Allows O(1) or O(log n) lookup by Hash instead of a linear search.
    ///
    /// # Arguments
    ///
    /// * `hash` - Hash of the desired entry
    ///
    /// # Returns
    ///
    /// `Some(Entry)` if found and supported
    /// `None` if not found or not supported
    fn get_entry_by_hash(&self, _hash: &Hash) -> Option<Entry> {
        // ***Default implementation returns None.
        None
    }

    /// Checks whether the index supports optimized queries with full Entries.
    ///
    /// Allows client code to determine whether it can use the optional
    /// optimization methods.
    fn supports_entry_queries(&self) -> bool {
        // ***Default implementation returns false.
        false
    }
}

/// Detailed options for creating a new Store instance.
/// This struct is the central configuration point for all of a store's
/// advanced features, including indexes, cache, replication and telemetry.
pub struct NewStoreOptions {
    // === CORE CONFIGURATION ===
    /// Event bus for internal communication.
    pub event_bus: Option<EventBus>,

    /// Constructor of the custom index for the store.
    pub index: Option<IndexConstructor>,

    /// Access controller for permissions and authentication.
    pub access_controller: Option<Arc<dyn AccessController>>,

    /// Base directory for data storage.
    pub directory: String,

    /// Custom sorting function for log entries.
    pub sort_fn: Option<SortFn>,

    // === NETWORKING & P2P ===
    /// Unique peer identifier in the P2P network (using Iroh's NodeId).
    pub node_id: NodeId,

    /// PubSub interface for distributed communication.
    pub pubsub: Option<Arc<dyn PubSubInterface<Error = GuardianError>>>,

    /// Direct channel for peer-to-peer communication.
    pub direct_channel: Option<Arc<dyn DirectChannel<Error = GuardianError>>>,

    /// Marshaler for serializing network messages.
    pub message_marshaler: Option<Arc<dyn MessageMarshaler<Error = GuardianError>>>,

    // === PERFORMANCE & STORAGE ===
    /// Cache system for optimizing data access.
    pub cache: Option<Arc<dyn Datastore>>,

    /// Callback for cache destruction (may fail).
    pub cache_destroy: Option<CleanupCallback>,

    /// Number of workers for concurrent replication.
    pub replication_concurrency: Option<u32>,

    /// Reference counter for garbage collection.
    pub reference_count: Option<i32>,

    /// Maximum limit of entries in the history.
    pub max_history: Option<i32>,

    // === BEHAVIOR FLAGS ===
    /// Enables/disables automatic replication.
    pub replicate: Option<bool>,

    // === OBSERVABILITY ===
    /// Structured logging system.
    pub span: Option<Span>,

    /// Tracer for distributed telemetry (OpenTelemetry).
    pub tracer: Option<Arc<TracerWrapper>>,

    // === LIFECYCLE MANAGEMENT ===
    /// Callback executed when the store closes.
    pub close_func: Option<Box<dyn FnOnce() + Send>>,

    // === EXTENSIBILITY ===
    /// Store-type-specific options (extensibility).
    /// Allows different store types to have custom configurations.
    pub store_specific_opts: Option<Box<dyn Any + Send + Sync>>,

    /// `DocTicket` (serialized) to import an iroh-docs namespace shared by a peer.
    /// When present, iroh-docs-based stores import the ticket's namespace (secure replication
    /// via capability) instead of creating a new namespace.
    pub doc_ticket: Option<String>,

    /// Marks this store as a read-only replica: refuses local writes and never creates a
    /// namespace (it must import an existing one). See [`CreateDBOptions::read_only`].
    pub read_only: Option<bool>,
}

impl Default for NewStoreOptions {
    fn default() -> Self {
        let node_id = NodeId::from_bytes(&[0u8; 32]).unwrap();

        Self {
            event_bus: None,
            index: None,
            access_controller: None,
            directory: String::new(),
            sort_fn: None,
            node_id,
            pubsub: None,
            direct_channel: None,
            message_marshaler: None,
            cache: None,
            cache_destroy: None,
            replication_concurrency: None,
            reference_count: None,
            max_history: None,
            replicate: None,
            span: None,
            tracer: None,
            close_func: None,
            store_specific_opts: None,
            doc_ticket: None,
            read_only: None,
        }
    }
}

/// Options for configuring a `DirectChannel`.
#[derive(Default, Clone)]
pub struct DirectChannelOptions {
    pub span: Option<Span>,
}

/// Trait for direct communication with another peer on the network.
#[async_trait::async_trait]
pub trait DirectChannel: Send + Sync + std::any::Any {
    type Error: Error + Send + Sync + 'static;

    /// Waits until the connection with the other peer is established.
    async fn connect(&mut self, peer: NodeId) -> Result<(), Self::Error>;

    /// Sends data to the other peer.
    async fn send(&mut self, peer: NodeId, data: Vec<u8>) -> Result<(), Self::Error>;

    /// Closes the connection.
    async fn close(&mut self) -> Result<(), Self::Error>;

    /// Closes the connection using a shared reference (&self).
    /// This method allows closing the channel when used inside an Arc<>.
    async fn close_shared(&self) -> Result<(), Self::Error>;

    /// Helper method for downcasting.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Defines the content of a message received via pubsub or a direct channel.
/// This struct is required for the `DirectChannelEmitter` definition.
#[derive(Debug, Clone)]
pub struct EventPubSubPayload {
    pub payload: Vec<u8>,
    pub peer: NodeId,
}

/// A trait used to emit events received from a `DirectChannel`.
#[async_trait::async_trait]
pub trait DirectChannelEmitter: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    /// Emits a received payload.
    async fn emit(&self, payload: EventPubSubPayload) -> Result<(), Self::Error>;

    /// Closes the emitter.
    async fn close(&self) -> Result<(), Self::Error>;
}

/// A factory for creating `DirectChannel` instances.
pub type DirectChannelFactory = Arc<
    dyn Fn(
            Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
            Option<DirectChannelOptions>,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = Result<
                            Arc<dyn DirectChannel<Error = GuardianError>>,
                            Box<dyn Error + Send + Sync>,
                        >,
                    > + Send,
            >,
        > + Send
        + Sync,
>;

/// Defines the prototype of a function (or closure) that builds and returns
/// a new instance of a `StoreIndex`.
pub type IndexConstructor =
    Box<dyn Fn(&[u8]) -> Box<dyn StoreIndex<Error = GuardianError>> + Send + Sync>;

/// A prototype for the callback function triggered when new entries
/// (`Entry`) are written to the store. It is an asynchronous function type.
pub type OnWritePrototype = Box<
    dyn Fn(
            Hash,
            Entry,
            Vec<Hash>,
        )
            -> Pin<Box<dyn Future<Output = Result<(), Box<dyn Error + Send + Sync>>> + Send>>
        + Send
        + Sync,
>;

/// Represents a new message received on a pub/sub topic.
#[derive(Debug, Clone)]
pub struct EventPubSubMessage {
    pub content: Vec<u8>,
}

/// Defines the prototype for an `AccessController` constructor.
pub type AccessControllerConstructor = Arc<
    dyn Fn(
            Arc<dyn BaseGuardianDB<Error = GuardianError>>,
            &crate::access_control::manifest::CreateAccessControllerOptions,
            Option<Vec<AccessControllerOption>>,
        )
            -> Pin<Box<dyn Future<Output = Result<Arc<dyn AccessController>, GuardianError>> + Send>>
        + Send
        + Sync,
>;

/// Represents a subscription to a specific pub/sub topic.
#[async_trait::async_trait]
pub trait PubSubTopic: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    /// Publishes a new message on the topic.
    async fn publish(&self, message: Vec<u8>) -> Result<(), Self::Error>;

    /// Lists the peers connected to this topic using Iroh's NodeId.
    async fn peers(&self) -> Result<Vec<iroh::EndpointId>, Self::Error>;

    /// Watches for peers joining and leaving the topic.
    async fn watch_peers(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = events::Event> + Send>>, Self::Error>;

    /// Watches for new messages published on the topic.
    async fn watch_messages(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = EventPubSubMessage> + Send>>, Self::Error>;

    /// Returns the topic name.
    fn topic(&self) -> &str;
}

/// Main trait of the pub/sub system.
#[async_trait::async_trait]
pub trait PubSubInterface: Send + Sync + std::any::Any {
    type Error: Error + Send + Sync + 'static;

    /// Subscribes to a topic.
    async fn topic_subscribe(
        &self,
        topic: &str,
    ) -> Result<Arc<dyn PubSubTopic<Error = GuardianError>>, Self::Error>;

    /// Helper method for downcasting.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Options for creating a subscription to a Pub/Sub topic.
#[derive(Default, Clone)]
pub struct PubSubSubscriptionOptions {
    pub span: Option<Span>,
    pub tracer: Option<Arc<TracerWrapper>>,
}

/// EventPubSub::Leave
/// Represents an event fired when a peer leaves
/// a topic on the Pub/Sub channel.
///
/// EventPubSub::Join
/// Represents an event fired when a peer joins
/// a topic on the Pub/Sub channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventPubSub {
    Join { topic: String, peer: NodeId },
    Leave { topic: String, peer: NodeId },
}
