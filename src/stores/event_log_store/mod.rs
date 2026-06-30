use crate::data_store::Datastore;
use crate::guardian::error::{GuardianError, Result};
use crate::log::{entry::Entry, identity::Identity};
use crate::p2p::EventBus;
use crate::p2p::network::client::IrohClient;
use crate::stores::base_store::BaseStore;
use crate::stores::operation::{self, Operation};
use crate::traits::{self, EventLogStore, Store, StreamOptions};
use crate::{address::Address, stores::event_log_store::index::new_event_index};
use iroh_blobs::Hash;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{Span, instrument};

pub mod index;

/// `EventLogStore` trait implementation for `GuardianDBEventLogStore`.
#[async_trait::async_trait]
impl EventLogStore for GuardianDBEventLogStore {
    /// Adds a new piece of data to the log.
    async fn add(&self, data: Vec<u8>) -> std::result::Result<Operation, Self::Error> {
        // Call the inherent method of the GuardianDBEventLogStore struct.
        GuardianDBEventLogStore::add(self, data).await
    }

    /// Gets a specific log entry by its Hash.
    async fn get(&self, hash: &Hash) -> std::result::Result<Operation, Self::Error> {
        // Call the inherent method of the GuardianDBEventLogStore struct.
        GuardianDBEventLogStore::get(self, hash).await
    }

    /// Returns a list of operations that occurred in the store, with filter options.
    async fn list(
        &self,
        options: Option<StreamOptions>,
    ) -> std::result::Result<Vec<Operation>, Self::Error> {
        // Call the inherent method of the GuardianDBEventLogStore struct.
        GuardianDBEventLogStore::list(self, options).await
    }
}

#[derive(Clone)]
pub struct GuardianDBEventLogStore {
    basestore: Arc<BaseStore>,
    span: Span,
}

// Store trait implementation (which is inherited by EventLogStore).
#[async_trait::async_trait]
impl Store for GuardianDBEventLogStore {
    type Error = GuardianError;

    #[allow(deprecated)]
    fn events(&self) -> &dyn crate::events::EmitterInterface {
        self.basestore.events()
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        self.basestore.close().await
    }

    fn address(&self) -> &dyn crate::address::Address {
        Store::address(self.basestore.as_ref())
    }

    fn index(&self) -> Box<dyn crate::traits::StoreIndex<Error = GuardianError> + Send + Sync> {
        self.basestore.index()
    }

    fn store_type(&self) -> &str {
        "eventlog"
    }

    fn cache(&self) -> Arc<dyn Datastore> {
        self.basestore.cache()
    }

    async fn drop(&self) -> std::result::Result<(), Self::Error> {
        // ***BaseStore has no public async drop method, so we implement a basic cleanup.
        // Cleanup is done automatically when the BaseStore is dropped.
        Ok(())
    }

    async fn load(&self, amount: usize) -> std::result::Result<(), Self::Error> {
        self.basestore.load(Some(amount as isize)).await
    }

    async fn sync(
        &self,
        heads: Vec<crate::log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        self.basestore.sync(heads).await
    }

    async fn load_more_from(&self, _amount: u64, entries: Vec<crate::log::entry::Entry>) {
        let _ = self.basestore.load_more_from(entries);
    }

    async fn load_from_snapshot(&self) -> std::result::Result<(), Self::Error> {
        self.basestore.load_from_snapshot().await
    }

    fn op_log(&self) -> Arc<parking_lot::RwLock<crate::log::Log>> {
        self.basestore.op_log()
    }

    fn client(&self) -> Arc<crate::p2p::network::client::IrohClient> {
        self.basestore.client()
    }

    fn db_name(&self) -> &str {
        self.basestore.db_name()
    }

    fn identity(&self) -> &Identity {
        self.basestore.identity()
    }

    fn access_controller(&self) -> &dyn crate::access_control::traits::AccessController {
        self.basestore.access_controller()
    }

    async fn add_operation(
        &self,
        op: Operation,
        on_progress_callback: Option<tokio::sync::mpsc::Sender<crate::log::entry::Entry>>,
    ) -> std::result::Result<crate::log::entry::Entry, Self::Error> {
        self.basestore.add_operation(op, on_progress_callback).await
    }

    fn span(&self) -> Arc<tracing::Span> {
        Arc::new(self.span.clone())
    }

    fn tracer(&self) -> Arc<crate::traits::TracerWrapper> {
        self.basestore.tracer()
    }

    fn event_bus(&self) -> Arc<EventBus> {
        self.basestore.event_bus()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl GuardianDBEventLogStore {
    /// Getter to access the internal BaseStore.
    pub fn basestore(&self) -> &BaseStore {
        &self.basestore
    }

    /// Returns a reference to the tracing span used for instrumentation.
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Instantiates a new EventLogStore, adapted to use the native Iroh client.
    ///
    /// # Arguments
    ///
    /// * `iroh_client` - Iroh client shared via Arc for network operations
    /// * `identity` - Node identity for signing entries
    /// * `addr` - Store address for unique identification
    /// * `options` - Store configuration options (index, cache, etc.)
    ///
    /// # Returns
    ///
    /// A new `GuardianDBEventLogStore` instance configured and ready to use
    ///
    /// # Errors
    ///
    /// Returns `GuardianError::Store` if:
    /// - BaseStore initialization fails
    /// - The configuration options are invalid
    #[instrument(level = "debug", skip(iroh_client, identity, addr, options))]
    pub async fn new(
        iroh_client: Arc<IrohClient>,
        identity: Arc<Identity>,
        addr: Arc<dyn Address + Send + Sync>,
        mut options: traits::NewStoreOptions,
    ) -> Result<Self> {
        // Basic parameter validation - check that the essential components exist.
        if addr.to_string().is_empty() {
            return Err(GuardianError::Store(
                "Invalid address provided, cannot create EventLogStore".to_string(),
            ));
        }

        tracing::info!(
            "EventLogStore::new - Configuring index for {}",
            addr.to_string()
        );

        // CRITICAL: Configure the index BEFORE creating the BaseStore.
        options.index = Some(Box::new(new_event_index));

        tracing::info!(
            "EventLogStore::new - Index configured: {}",
            options.index.is_some()
        );

        // Initialize the BaseStore with the provided options.
        let basestore = BaseStore::new(iroh_client, identity, addr.clone(), Some(options))
            .await
            .map_err(|e| {
                GuardianError::Store(format!(
                    "Failed to initialize base store for EventLogStore: {}",
                    e
                ))
            })?;

        tracing::info!(
            "EventLogStore::new - BaseStore created with active index: {}",
            basestore.has_active_index()
        );

        // Create a span for this EventLogStore instance.
        let span = tracing::info_span!("event_log_store", address = %addr.to_string());

        Ok(GuardianDBEventLogStore { basestore, span })
    }

    /// Collects all operations from a stream into a vector.
    #[instrument(level = "debug", skip(self, options))]
    pub async fn list(&self, options: Option<StreamOptions>) -> Result<Vec<Operation>> {
        let _entered = self.span.enter();
        let (tx, mut rx) = mpsc::channel(100); // Larger buffer to avoid deadlock.

        // Spawn the stream in a separate task to avoid deadlock
        // (the stream sends data while list receives).
        let self_clone = self.clone();
        tokio::spawn(async move {
            let _ = self_clone.stream(tx, options).await;
        });

        let mut operations = Vec::new();
        while let Some(op) = rx.recv().await {
            operations.push(op);
        }

        Ok(operations)
    }

    /// Creates and adds a new "ADD" operation to the log.
    ///
    /// # Arguments
    ///
    /// * `value` - Byte data to add to the log
    ///
    /// # Returns
    ///
    /// The created operation with its unique Hash
    ///
    /// # Errors
    ///
    /// - If the data is empty (optional, depending on the policy)
    /// - If adding to the BaseStore fails
    /// - If the Entry -> Operation conversion fails
    #[instrument(level = "debug", skip(self, value))]
    pub async fn add(&self, value: Vec<u8>) -> Result<Operation> {
        // Optional validation: check that there is data.
        if value.is_empty() {
            return Err(GuardianError::Store(
                "Cannot add empty data to EventLogStore".to_string(),
            ));
        }

        let op = Operation::new(None, "ADD".to_string(), Some(value));

        // `add_operation` returns an `Entry`.
        let entry = self
            .add_operation(op, None)
            .await
            .map_err(|e| GuardianError::Store(format!("Failed to add operation to log: {}", e)))?;

        // `parse_operation` converts the `Entry` back into an `Operation`.
        let op_result = operation::parse_operation(entry).map_err(|e| {
            GuardianError::Store(format!("Failed to parse newly created entry: {}", e))
        })?;

        Ok(op_result)
    }

    /// Retrieves a single operation from the log by its Hash.
    ///
    /// # Arguments
    ///
    /// * `hash` - Hash of the desired entry
    ///
    /// # Returns
    ///
    /// The operation corresponding to the provided Hash
    ///
    /// # Errors
    ///
    /// - If the Hash is not found in the log
    /// - If the stream returns no results
    #[instrument(level = "debug", skip(self))]
    pub async fn get(&self, hash: &Hash) -> Result<Operation> {
        let _entered = self.span.enter();
        let (tx, mut rx) = mpsc::channel(1);

        let stream_options = StreamOptions {
            gte: Some(*hash),
            amount: Some(1),
            ..Default::default()
        };

        // For simplicity, let's run it directly.
        self.stream(tx, Some(stream_options)).await?;

        // Wait for the first value.
        if let Some(value) = rx.recv().await {
            Ok(value)
        } else {
            Err(GuardianError::Store(format!(
                "No operation found for Hash: {}",
                hex::encode(hash.as_bytes())
            )))
        }
    }

    /// Fetches entries, converts them into operations, and sends them through a channel.
    #[instrument(level = "debug", skip(self, result_chan, options))]
    pub async fn stream(
        &self,
        result_chan: mpsc::Sender<Operation>,
        options: Option<StreamOptions>,
    ) -> Result<()> {
        // The `query` function returns the log entries.
        let messages = self
            .query(options)
            .map_err(|e| GuardianError::Store(format!("unable to fetch query results: {}", e)))?;

        for message in messages {
            // Convert each entry into an Operation.
            let op = operation::parse_operation(message)
                .map_err(|e| GuardianError::Store(format!("unable to parse operation: {}", e)))?;

            // Send the operation through the channel. If the receiver is closed, the send fails
            // and the loop breaks, which is the expected behavior.
            if result_chan.send(op).await.is_err() {
                // The receiver was closed, so we can stop sending.
                break;
            }
        }

        // In Rust, the channel is closed automatically when `result_chan` (the Sender)
        // goes out of scope, so an explicit call like `close(resultChan)` is not needed.
        Ok(())
    }

    /// Runs the log-index lookup logic based on the filter options.
    ///
    /// # Performance
    ///
    /// - Uses the index when available for optimized queries
    /// - Falls back to direct oplog access when needed
    /// - Supports filtering by Hash
    #[instrument(level = "debug", skip(self, options))]
    fn query(&self, options: Option<StreamOptions>) -> Result<Vec<Entry>> {
        let options = options.unwrap_or_default();

        // Try to use the index first for better performance.
        let events = match self.basestore.with_index(|index| {
            // Implements optimized index lookup based on the StreamOptions.
            self.optimized_index_query(index, &options)
        }) {
            Some(Some(indexed_results)) => indexed_results,
            _ => {
                // Fallback: access the oplog directly when the index is not available
                // or does not support the specific query.
                self.basestore.with_oplog(|log| {
                    log.values()
                        .iter()
                        .map(|arc_entry| (**arc_entry).clone())
                        .collect::<Vec<_>>()
                })
            }
        };

        // Compute the number of items to return.
        let amount = match options.amount {
            Some(a) if a > -1 => a as usize,
            _ => events.len(), // If amount is -1 or None, take all.
        };

        if options.gt.is_some() || options.gte.is_some() {
            // "Greater Than" case.
            let hash = options.gt.or(options.gte).unwrap();
            let inclusive = options.gte.is_some();
            return Ok(self.read(&events, Some(hash), amount, inclusive));
        }

        let hash = options.lt.or(options.lte);

        // "Lower Than" case or the last N.
        // Reverse the events to go from the most recent to the oldest.
        let mut events = events;
        events.reverse();

        // The search is inclusive if LTE is set or if no bound (LT/LTE) is set.
        let inclusive = options.lte.is_some() || hash.is_none();
        let mut result = self.read(&events, hash, amount, inclusive);

        // Undo the result's reversal to keep the original chronological order.
        result.reverse();

        Ok(result)
    }

    /// Helper function to read a slice of entries starting from a hash.
    ///
    /// # Arguments
    ///
    /// * `ops` - Slice of entries to filter
    /// * `hash` - Optional hash to use as the starting point
    /// * `amount` - Maximum number of entries to return
    /// * `inclusive` - Whether to include the entry with the provided hash
    ///
    /// # Returns
    ///
    /// Vector of entries filtered based on the criteria
    ///
    /// # Performance
    ///
    /// - O(n) to find the starting index by hash
    /// - O(amount) to collect the results
    /// - Optimized for use with Rust iterators
    fn read(
        &self,
        ops: &[Entry],
        hash: Option<Hash>,
        amount: usize,
        inclusive: bool,
    ) -> Vec<Entry> {
        if amount == 0 {
            return Vec::new();
        }

        // Find the starting index.
        let mut start_index = 0;
        if let Some(h) = hash {
            if let Some(idx) = ops.iter().position(|e| e.hash() == &h) {
                start_index = idx;
            } else {
                // If the hash is not found, there is nothing to return.
                return Vec::new();
            }
        }

        // If not inclusive, start from the next element.
        if !inclusive {
            start_index += 1;
        }

        // Limit the number of elements and collect the result.
        ops.iter().skip(start_index).take(amount).cloned().collect()
    }

    /// Optimized index lookup based on the StreamOptions.
    ///
    /// Uses the new optional methods of the StoreIndex trait.
    ///
    /// # Arguments
    ///
    /// * `index` - Reference to the store's index
    /// * `options` - Stream filter options
    ///
    /// # Returns
    ///
    /// `Some(Vec<Entry>)` if it can process the optimized query
    /// `None` if it should use the fallback (direct oplog)
    ///
    /// # Optimized Cases (Implemented)
    ///
    /// 1. **Amount-only queries**: Last N entries using `get_last_entries()`
    /// 2. **Range queries**: Specific ranges using `get_entries_range()`
    /// 3. **Hash queries**: Lookup by Hash using `get_entry_by_hash()`
    ///
    /// # Fallback Cases
    ///
    /// 1. **Index does not support Entry**: `supports_entry_queries()` returns false
    /// 2. **Complex queries**: Non-optimized combinations
    /// 3. **Empty index**: No entries available
    ///
    /// # Performance
    ///
    /// - **get_last_entries()**: O(k) where k = number of requested entries
    /// - **get_entry_by_hash()**: O(n) currently, O(1) in the future with a Hash index
    /// - **get_entries_range()**: O(k) where k = range size
    fn optimized_index_query(
        &self,
        index: &dyn crate::traits::StoreIndex<Error = GuardianError>,
        options: &StreamOptions,
    ) -> Option<Vec<Entry>> {
        // Check whether the index supports optimized queries with full Entries.
        if !index.supports_entry_queries() {
            return None; // Fallback to the oplog.
        }

        // Quick validation: check whether the index has data.
        let total_entries = match index.len() {
            Ok(len) if len > 0 => len,
            _ => return None, // Empty index - use the fallback.
        };

        // Simple amount-based query (the most common case).
        let is_simple_amount_query = options.gt.is_none()
            && options.gte.is_none()
            && options.lt.is_none()
            && options.lte.is_none();

        if is_simple_amount_query {
            let amount = match options.amount {
                Some(a) if a > 0 => (a as usize).min(total_entries),
                Some(-1) | None => total_entries, // -1 or None means "all".
                _ => return None,                 // Invalid value.
            };

            // Use the index's optimized method.
            return index.get_last_entries(amount);
        }

        // Query by a specific Hash (get operation).
        if let Some(hash) = options.gte
            && options.amount == Some(1)
            && options.gt.is_none()
            && options.lt.is_none()
            && options.lte.is_none()
        {
            // Point query by Hash - use the optimized lookup.
            if let Some(entry) = index.get_entry_by_hash(&hash) {
                return Some(vec![entry]);
            } else {
                return Some(Vec::new()); // Hash not found.
            }
        }

        // Future optimizations: specific ranges (future).
        // For now, queries with multiple Hashes use the fallback,
        // which already implements the correct logic.
        //
        // Future optimizations:
        // - Range by position when Hashes are consecutive
        // - Cache of frequent queries
        // - Temporal index for timestamp filters

        None // Use the fallback for complex queries.
    }
}
