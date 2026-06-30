/// Batch processing system for the Iroh backend.
///
/// Intelligent batch processing to optimize the throughput of Iroh operations,
/// reducing overhead and improving I/O efficiency.
use crate::guardian::error::{GuardianError, Result};
use crate::p2p::network::types::AddResponse;
use bytes::Bytes;
use futures;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, Semaphore, mpsc, oneshot};
use tracing::{debug, info, instrument, warn};
use uuid::Uuid;

/// Batch operation processor.
pub struct BatchProcessor {
    /// Queue of pending operations.
    pending_operations: Arc<Mutex<VecDeque<BatchOperation>>>,
    /// Operations grouped by type for optimization.
    typed_queues: Arc<RwLock<TypedQueues>>,
    /// Processor configuration.
    batch_config: BatchConfig,
    /// Performance statistics.
    stats: Arc<RwLock<BatchStats>>,
    /// Channel for processing control.
    #[allow(dead_code)]
    control_sender: mpsc::Sender<BatchControl>,
    /// Semaphore for concurrency control.
    processing_semaphore: Arc<Semaphore>,
    /// Operation history for optimization.
    operation_history: Arc<RwLock<OperationHistory>>,
    /// Iroh backend for iroh operations.
    backend: Arc<crate::p2p::network::core::IrohBackend>,
}

/// A batched operation.
#[derive(Debug)]
pub struct BatchOperation {
    /// Unique operation ID.
    pub id: String,
    /// Operation type.
    pub operation_type: OperationType,
    /// Operation data.
    pub data: OperationData,
    /// Creation timestamp.
    pub created_at: Instant,
    /// Priority (0-10).
    pub priority: u8,
    /// Channel for returning the result.
    pub result_sender: oneshot::Sender<Result<OperationResult>>,
    /// Estimate of the required resources.
    pub resource_estimate: ResourceEstimate,
}

/// Operation types supported by Iroh.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OperationType {
    /// Add data to Iroh.
    Add,
    /// Retrieve data from Iroh.
    Get,
    /// Pin content (permanent via Tags).
    Pin,
    /// Unpin content (removes the Tag).
    Unpin,
    /// Publish to PubSub (iroh-gossip).
    PubSubPublish,
}

/// Iroh operation data.
#[derive(Debug)]
pub enum OperationData {
    /// Data to add to Iroh.
    AddData { data: Bytes, options: AddOptions },
    /// Hash to retrieve from Iroh.
    GetHash { hash: String, options: GetOptions },
    /// Hash to pin (create a permanent Tag).
    PinHash { hash: String, options: PinOptions },
    /// Hash to unpin (remove the Tag).
    UnpinHash { hash: String },
    /// Data to publish via iroh-gossip.
    PubSubData { topic: String, data: Bytes },
}

/// Iroh operation result.
#[derive(Debug)]
pub enum OperationResult {
    /// Add result (hash of the added blob).
    AddResult(AddResponse),
    /// Get result (retrieved data).
    GetResult(Bytes),
    /// Pin result (success/failure).
    PinResult(bool),
    /// Unpin result (success/failure).
    UnpinResult(bool),
    /// PubSub result (success/failure).
    PubSubResult(bool),
}

/// Queues organized by Iroh operation type.
#[derive(Debug, Default)]
pub struct TypedQueues {
    /// Blob add operations.
    add_queue: VecDeque<BatchOperation>,
    /// Blob retrieval operations.
    get_queue: VecDeque<BatchOperation>,
    /// Pin operations (Tag management).
    pin_queue: VecDeque<BatchOperation>,
    /// PubSub operations (iroh-gossip).
    pubsub_queue: VecDeque<BatchOperation>,
}

/// Options for the Add operation.
#[derive(Debug, Clone, Default)]
pub struct AddOptions {
    /// Pin automatically.
    pub pin: bool,
    /// Wrap in directory.
    pub wrap_with_directory: bool,
    /// Chunker to use.
    pub chunker: Option<String>,
}

/// Options for the Get operation.
#[derive(Debug, Clone, Default)]
pub struct GetOptions {
    /// Timeout for the operation.
    pub timeout: Option<Duration>,
    /// Try specific peers.
    pub preferred_peers: Vec<String>,
}

/// Options for the Pin operation.
#[derive(Debug, Clone, Default)]
pub struct PinOptions {
    /// Pin type (direct or recursive).
    pub recursive: bool,
    /// Progress callback.
    pub progress: bool,
}

/// Resource estimate for an operation.
#[derive(Debug, Clone)]
pub struct ResourceEstimate {
    /// Estimated CPU (0.0-1.0).
    pub cpu_usage: f64,
    /// Estimated memory (bytes).
    pub memory_bytes: u64,
    /// Estimated I/O (bytes).
    pub io_bytes: u64,
    /// Estimated bandwidth (bytes).
    pub bandwidth_bytes: u64,
    /// Estimated time (ms).
    pub estimated_time_ms: u64,
}

/// Batch processor configuration.
#[derive(Debug, Clone)]
pub struct BatchConfig {
    /// Maximum batch size.
    pub max_batch_size: usize,
    /// Maximum wait time to form a batch (ms).
    pub max_batch_wait_ms: u64,
    /// Maximum number of processing threads.
    pub max_processing_threads: usize,
    /// Memory threshold for flushing (bytes).
    pub memory_flush_threshold: u64,
    /// Enable intelligent optimizations.
    pub enable_smart_batching: bool,
    /// Enable batch compression.
    pub enable_batch_compression: bool,
    /// Minimum size for compression (bytes).
    pub compression_threshold: usize,
}

/// Batch statistics.
#[derive(Debug, Clone, Default)]
pub struct BatchStats {
    /// Total operations processed.
    pub total_operations: u64,
    /// Operations processed in a batch.
    pub batched_operations: u64,
    /// Operations processed individually.
    pub individual_operations: u64,
    /// Average batch size.
    pub avg_batch_size: f64,
    /// Average batch processing time (ms).
    pub avg_batch_processing_time_ms: f64,
    /// Throughput (operations/second).
    pub operations_per_second: f64,
    /// Batch efficiency (0.0-1.0).
    pub batch_efficiency: f64,
    /// Bytes processed.
    pub total_bytes_processed: u64,
    /// Resource savings (0.0-1.0).
    pub resource_savings: f64,
}

/// Processing controls.
#[derive(Debug)]
pub enum BatchControl {
    /// Force immediate processing.
    FlushNow,
    /// Pause processing.
    Pause,
    /// Resume processing.
    Resume,
    /// Stop the processor.
    Stop,
    /// Adjust the configuration.
    UpdateConfig(BatchConfig),
}

/// Operation history for optimization.
#[derive(Debug, Default)]
pub struct OperationHistory {
    /// Operation patterns per type.
    operation_patterns: HashMap<OperationType, OperationPattern>,
    /// Correlations between operations.
    #[allow(dead_code)]
    operation_correlations: HashMap<String, Vec<OperationType>>,
    /// Timing history.
    timing_history: VecDeque<TimingEntry>,
}

/// An identified operation pattern.
#[derive(Debug, Clone)]
pub struct OperationPattern {
    /// Average frequency.
    pub avg_frequency: f64,
    /// Average data size.
    pub avg_data_size: u64,
    /// Average processing time.
    pub avg_processing_time_ms: f64,
    /// Peak hours.
    pub peak_hours: Vec<u8>,
    /// Correlation with other operations.
    pub correlated_operations: Vec<OperationType>,
}

/// A timing entry.
#[derive(Debug, Clone)]
pub struct TimingEntry {
    /// Operation type.
    pub operation_type: OperationType,
    /// Timestamp.
    pub timestamp: Instant,
    /// Duration (ms).
    pub duration_ms: f64,
    /// Data size.
    pub data_size: u64,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_batch_size: 100,
            max_batch_wait_ms: 50,
            max_processing_threads: 8,
            memory_flush_threshold: 64 * 1024 * 1024, // 64 MB
            enable_smart_batching: true,
            enable_batch_compression: true,
            compression_threshold: 1024, // 1 KB
        }
    }
}

impl BatchProcessor {
    /// Creates a new batch processor with an IrohBackend.
    pub fn new(
        batch_config: BatchConfig,
        backend: Arc<crate::p2p::network::core::IrohBackend>,
    ) -> Self {
        let (control_sender, _control_receiver) = mpsc::channel(100);

        Self {
            pending_operations: Arc::new(Mutex::new(VecDeque::new())),
            typed_queues: Arc::new(RwLock::new(TypedQueues::default())),
            batch_config,
            stats: Arc::new(RwLock::new(BatchStats::default())),
            control_sender,
            processing_semaphore: Arc::new(Semaphore::new(8)), // max concurrent batches
            operation_history: Arc::new(RwLock::new(OperationHistory::default())),
            backend,
        }
    }

    /// Adds an operation for batch processing.
    #[instrument(skip(self, data))]
    pub async fn add_batch_operation(
        &self,
        operation_type: OperationType,
        data: OperationData,
        priority: u8,
    ) -> Result<OperationResult> {
        let (result_sender, result_receiver) = oneshot::channel();

        let operation = BatchOperation {
            id: Uuid::new_v4().to_string(),
            operation_type: operation_type.clone(),
            data,
            created_at: Instant::now(),
            priority,
            result_sender,
            resource_estimate: self.estimate_resources(&operation_type).await,
        };

        // Add it to the appropriate queue based on the type.
        if self.batch_config.enable_smart_batching {
            self.add_to_typed_queue(operation).await?;
        } else {
            let mut pending = self.pending_operations.lock().await;
            pending.push_back(operation);
        }

        // Check whether it should be processed immediately.
        self.check_immediate_processing().await?;

        // Wait for the result.
        result_receiver
            .await
            .map_err(|e| GuardianError::Other(format!("Failed to receive result: {}", e)))?
    }

    /// Adds an operation to the appropriate typed queue.
    async fn add_to_typed_queue(&self, operation: BatchOperation) -> Result<()> {
        let mut queues = self.typed_queues.write().await;

        let operation_type_debug = operation.operation_type.clone();

        match operation.operation_type {
            OperationType::Add => queues.add_queue.push_back(operation),
            OperationType::Get => queues.get_queue.push_back(operation),
            OperationType::Pin | OperationType::Unpin => queues.pin_queue.push_back(operation),
            OperationType::PubSubPublish => queues.pubsub_queue.push_back(operation),
        }

        debug!(
            "Operation added to the typed queue: {:?}",
            operation_type_debug
        );
        Ok(())
    }

    /// Checks whether it should be processed immediately.
    async fn check_immediate_processing(&self) -> Result<()> {
        if self.batch_config.enable_smart_batching {
            self.check_smart_processing().await
        } else {
            self.check_simple_processing().await
        }
    }

    /// Intelligent processing check.
    async fn check_smart_processing(&self) -> Result<()> {
        let queues = self.typed_queues.read().await;

        // Check each queue type.
        let add_ready = queues.add_queue.len() >= self.batch_config.max_batch_size / 4;
        let get_ready = queues.get_queue.len() >= self.batch_config.max_batch_size / 2;
        let pin_ready = queues.pin_queue.len() >= self.batch_config.max_batch_size / 3;

        if add_ready || get_ready || pin_ready {
            drop(queues);
            self.process_ready_batches().await?;
        }

        Ok(())
    }

    /// Simple processing check.
    async fn check_simple_processing(&self) -> Result<()> {
        let pending_count = self.pending_operations.lock().await.len();

        if pending_count >= self.batch_config.max_batch_size {
            self.process_pending_batch().await?;
        }

        Ok(())
    }

    /// Processes ready batches intelligently.
    async fn process_ready_batches(&self) -> Result<()> {
        let _permit = self
            .processing_semaphore
            .acquire()
            .await
            .map_err(|e| GuardianError::Other(format!("Failed to acquire semaphore: {}", e)))?;

        let mut batches_to_process = Vec::new();

        // Collect ready batches of each type.
        {
            let mut queues = self.typed_queues.write().await;

            // Process Add operations.
            if queues.add_queue.len() >= self.batch_config.max_batch_size / 4 {
                let batch =
                    self.extract_batch(&mut queues.add_queue, self.batch_config.max_batch_size / 4);
                if !batch.is_empty() {
                    batches_to_process.push((OperationType::Add, batch));
                }
            }

            // Process Get operations.
            if queues.get_queue.len() >= self.batch_config.max_batch_size / 2 {
                let batch =
                    self.extract_batch(&mut queues.get_queue, self.batch_config.max_batch_size / 2);
                if !batch.is_empty() {
                    batches_to_process.push((OperationType::Get, batch));
                }
            }

            // Similar for other types...
        }

        // Process each batch.
        for (batch_type, batch) in batches_to_process {
            self.process_typed_batch(batch_type, batch).await?;
        }

        Ok(())
    }

    /// Extracts a batch from a queue.
    fn extract_batch(
        &self,
        queue: &mut VecDeque<BatchOperation>,
        max_size: usize,
    ) -> Vec<BatchOperation> {
        let mut batch = Vec::with_capacity(max_size);

        // Sort by priority.
        let mut temp_vec: Vec<_> = queue.drain(..).collect();
        temp_vec.sort_by_key(|operation| std::cmp::Reverse(operation.priority));

        // Take the first max_size.
        for operation in temp_vec.into_iter().take(max_size) {
            batch.push(operation);
        }

        batch
    }

    /// Processes a batch of a specific type.
    async fn process_typed_batch(
        &self,
        batch_type: OperationType,
        batch: Vec<BatchOperation>,
    ) -> Result<()> {
        let batch_size = batch.len();
        let start_time = Instant::now();

        debug!(
            "Processing batch of {} operations of type {:?}",
            batch_size, batch_type
        );

        match batch_type {
            OperationType::Add => self.process_add_batch(batch).await?,
            OperationType::Get => self.process_get_batch(batch).await?,
            OperationType::Pin => self.process_pin_batch(batch).await?,
            OperationType::PubSubPublish => self.process_pubsub_batch(batch).await?,
            _ => {
                // Individual processing for non-optimized types.
                for operation in batch {
                    self.process_individual_operation(operation).await?;
                }
            }
        }

        // Update statistics.
        let processing_time = start_time.elapsed();
        let mut stats = self.stats.write().await;
        stats.batched_operations += batch_size as u64;
        stats.avg_batch_size = (stats.avg_batch_size + batch_size as f64) / 2.0;
        stats.avg_batch_processing_time_ms =
            (stats.avg_batch_processing_time_ms + processing_time.as_millis() as f64) / 2.0;

        info!(
            "Batch of {} {:?} operations processed in {:.2}ms",
            batch_size,
            batch_type,
            processing_time.as_millis()
        );

        Ok(())
    }

    /// Processes a batch of Add operations.
    async fn process_add_batch(&self, batch: Vec<BatchOperation>) -> Result<()> {
        // Optimization: group small data into a single blob.
        let mut combined_data = Vec::new();
        let mut data_map = HashMap::new();

        for operation in &batch {
            if let OperationData::AddData { data, .. } = &operation.data {
                let start_offset = combined_data.len();
                combined_data.extend_from_slice(data);
                let end_offset = combined_data.len();

                data_map.insert(
                    operation.id.clone(),
                    (start_offset, end_offset, data.clone()),
                );
            }
        }

        // If we have enough data, process it as a single blob.
        if combined_data.len() > self.batch_config.compression_threshold && batch.len() > 1 {
            // Process it as a combined blob.
            let combined_blob = Bytes::from(combined_data);
            let combined_result = self.add_operation(combined_blob).await?;

            // Distribute the results.
            for operation in batch {
                if let Some((start, end, _original_data)) = data_map.get(&operation.id) {
                    // Create an individual response based on the combined result.
                    let individual_result = AddResponse {
                        name: format!("{}_{}", combined_result.name, operation.id),
                        hash: format!("{}_{}", combined_result.hash, start),
                        size: ((end - start) as u64).to_string(),
                    };

                    let _ = operation
                        .result_sender
                        .send(Ok(OperationResult::AddResult(individual_result)));
                }
            }
        } else {
            // Process individually.
            for operation in batch {
                self.process_individual_operation(operation).await?;
            }
        }

        Ok(())
    }

    /// Processes a batch of Get operations.
    async fn process_get_batch(&self, batch: Vec<BatchOperation>) -> Result<()> {
        // Optimization: make parallel requests.
        let mut futures = Vec::new();

        for operation in batch {
            if let OperationData::GetHash { hash, .. } = &operation.data {
                let hash_clone = hash.clone();
                let future = async move {
                    let result = self.get_operation(hash_clone).await;
                    (operation, result)
                };
                futures.push(future);
            }
        }

        // Run all operations in parallel.
        let results = futures::future::join_all(futures).await;

        for (operation, result) in results {
            match result {
                Ok(data) => {
                    let _ = operation
                        .result_sender
                        .send(Ok(OperationResult::GetResult(data)));
                }
                Err(e) => {
                    let _ = operation.result_sender.send(Err(e));
                }
            }
        }

        Ok(())
    }

    /// Processes a batch of Pin operations.
    async fn process_pin_batch(&self, batch: Vec<BatchOperation>) -> Result<()> {
        // Group the pins into a single operation.
        let mut pin_hashes = Vec::new();

        for operation in &batch {
            if let OperationData::PinHash { hash, .. } = &operation.data {
                pin_hashes.push(hash.clone());
            }
        }

        // Run the pins as a batch.
        let batch_result = self.batch_pin_operation(pin_hashes).await?;

        // Distribute the results.
        for (i, operation) in batch.into_iter().enumerate() {
            let individual_result = batch_result.get(i).copied().unwrap_or(false);
            let _ = operation
                .result_sender
                .send(Ok(OperationResult::PinResult(individual_result)));
        }

        Ok(())
    }

    /// Processes a batch of PubSub operations.
    async fn process_pubsub_batch(&self, batch: Vec<BatchOperation>) -> Result<()> {
        // Group by topic.
        let mut topic_groups: HashMap<String, Vec<BatchOperation>> = HashMap::new();

        for operation in batch {
            if let OperationData::PubSubData { topic, .. } = &operation.data {
                topic_groups
                    .entry(topic.clone())
                    .or_default()
                    .push(operation);
            }
        }

        // Process each topic group.
        for (topic, operations) in topic_groups {
            self.process_pubsub_topic_batch(topic, operations).await?;
        }

        Ok(())
    }

    /// Processes a PubSub batch for a specific topic.
    async fn process_pubsub_topic_batch(
        &self,
        _topic: String,
        batch: Vec<BatchOperation>,
    ) -> Result<()> {
        // Combine messages of the same topic.
        for operation in batch {
            self.process_individual_operation(operation).await?;
        }
        Ok(())
    }

    /// Processes a pending batch (simple mode).
    async fn process_pending_batch(&self) -> Result<()> {
        let batch = {
            let mut pending = self.pending_operations.lock().await;
            let batch_size = self.batch_config.max_batch_size.min(pending.len());
            pending.drain(..batch_size).collect::<Vec<_>>()
        };

        if batch.is_empty() {
            return Ok(());
        }

        let batch_size = batch.len();
        let start_time = Instant::now();

        // Process each operation.
        for operation in batch {
            self.process_individual_operation(operation).await?;
        }

        // Update statistics.
        let processing_time = start_time.elapsed();
        let mut stats = self.stats.write().await;
        stats.total_operations += batch_size as u64;
        stats.avg_batch_processing_time_ms =
            (stats.avg_batch_processing_time_ms + processing_time.as_millis() as f64) / 2.0;

        Ok(())
    }

    /// Processes an individual operation.
    async fn process_individual_operation(&self, operation: BatchOperation) -> Result<()> {
        let start_time = Instant::now();

        let result = match operation.data {
            OperationData::AddData { data, .. } => {
                let add_result = self.add_operation(data).await?;
                Ok(OperationResult::AddResult(add_result))
            }
            OperationData::GetHash { hash, .. } => {
                let get_result = self.get_operation(hash).await?;
                Ok(OperationResult::GetResult(get_result))
            }
            OperationData::PinHash { hash, .. } => {
                let pin_result = self.pin_operation(hash).await?;
                Ok(OperationResult::PinResult(pin_result))
            }
            OperationData::UnpinHash { hash } => {
                let unpin_result = self.unpin_operation(hash).await?;
                Ok(OperationResult::UnpinResult(unpin_result))
            }
            OperationData::PubSubData { topic, data } => {
                let pubsub_result = self.pubsub_operation(topic, data).await?;
                Ok(OperationResult::PubSubResult(pubsub_result))
            }
        };

        // Record timing.
        let processing_time = start_time.elapsed();
        self.record_operation_timing(
            operation.operation_type,
            processing_time,
            operation.resource_estimate.memory_bytes,
        )
        .await;

        // Send the result.
        let _ = operation.result_sender.send(result);

        Ok(())
    }

    /// Add operation using the IrohBackend.
    async fn add_operation(&self, data: Bytes) -> Result<AddResponse> {
        use std::pin::Pin;
        use tokio::io::AsyncRead;

        // Convert Bytes into AsyncRead using a cursor.
        let cursor = std::io::Cursor::new(data.to_vec());
        let async_read: Pin<Box<dyn AsyncRead + Send>> = Box::pin(cursor);

        // Call the IrohBackend's add method.
        let add_result = self
            .backend
            .add(async_read)
            .await
            .map_err(|e| GuardianError::Other(format!("Error in IrohBackend.add(): {}", e)))?;

        debug!(
            "BatchProcessor: Content added via IrohBackend - Hash: {}, Size: {}",
            add_result.hash, add_result.size
        );

        Ok(add_result)
    }

    /// Get operation using the IrohBackend.
    async fn get_operation(&self, hash: String) -> Result<Bytes> {
        use tokio::io::AsyncReadExt;

        // Call the IrohBackend's cat method.
        let mut async_read = self.backend.cat(&hash).await.map_err(|e| {
            GuardianError::Other(format!("Error in IrohBackend.cat({}): {}", hash, e))
        })?;

        // Read all data from the stream.
        let mut buffer = Vec::new();
        async_read.read_to_end(&mut buffer).await.map_err(|e| {
            GuardianError::Other(format!("Error reading data for Hash {}: {}", hash, e))
        })?;

        debug!(
            "BatchProcessor: Content retrieved via IrohBackend - Hash: {}, Size: {} bytes",
            hash,
            buffer.len()
        );

        Ok(Bytes::from(buffer))
    }

    /// Pin operation using the IrohBackend (creates a permanent Tag).
    async fn pin_operation(&self, hash: String) -> Result<bool> {
        // Call the IrohBackend's pin_add method.
        match self.backend.pin_add(&hash).await {
            Ok(_) => {
                debug!(
                    "BatchProcessor: Content pinned successfully via IrohBackend - Hash: {}",
                    hash
                );
                Ok(true)
            }
            Err(e) => {
                warn!(
                    "BatchProcessor: Error pinning content via IrohBackend - Hash: {}, Error: {}",
                    hash, e
                );
                // Return false instead of an error to keep batch compatibility.
                Ok(false)
            }
        }
    }

    /// Batch Pin operation using the IrohBackend.
    async fn batch_pin_operation(&self, hashes: Vec<String>) -> Result<Vec<bool>> {
        debug!(
            "BatchProcessor: Processing {} pin operations in a batch",
            hashes.len()
        );

        // Run the pins in parallel for throughput optimization.
        let pin_futures: Vec<_> = hashes
            .iter()
            .map(|hash| {
                let backend = Arc::clone(&self.backend);
                let hash_clone = hash.clone();
                async move {
                    match backend.pin_add(&hash_clone).await {
                        Ok(_) => {
                            debug!("Batch pin succeeded: {}", hash_clone);
                            true
                        }
                        Err(e) => {
                            warn!("Batch pin failed for {}: {}", hash_clone, e);
                            false
                        }
                    }
                }
            })
            .collect();

        // Wait for all pins in parallel.
        let results = futures::future::join_all(pin_futures).await;

        let successful_pins = results.iter().filter(|&&r| r).count();
        info!(
            "BatchProcessor: Batch pin complete - {}/{} successful",
            successful_pins,
            hashes.len()
        );

        Ok(results)
    }

    /// Unpin operation using the IrohBackend.
    async fn unpin_operation(&self, hash: String) -> Result<bool> {
        // Call the IrohBackend's pin_rm method.
        match self.backend.pin_rm(&hash).await {
            Ok(_) => {
                debug!(
                    "BatchProcessor: Content unpinned successfully via IrohBackend - Hash: {}",
                    hash
                );
                Ok(true)
            }
            Err(e) => {
                warn!(
                    "BatchProcessor: Error unpinning content via IrohBackend - Hash: {}, Error: {}",
                    hash, e
                );
                // Return false instead of an error to keep batch compatibility.
                Ok(false)
            }
        }
    }

    /// PubSub operation using the IrohBackend with native iroh-gossip.
    async fn pubsub_operation(&self, topic: String, data: Bytes) -> Result<bool> {
        // Create a PubSub interface using iroh-gossip.
        // self.backend is already Arc<IrohBackend>, so we use it directly.
        let backend_arc = Arc::clone(&self.backend);
        let pubsub = backend_arc.create_pubsub_interface().await?;

        // Publish the message using the convenience method.
        match pubsub.publish_to_topic(&topic, &data).await {
            Ok(_) => {
                debug!(
                    "BatchProcessor: PubSub message published via iroh-gossip - Topic: {}, Size: {} bytes",
                    topic,
                    data.len()
                );
                Ok(true)
            }
            Err(e) => {
                warn!(
                    "BatchProcessor: Error publishing via iroh-gossip - Topic: {}, Error: {}",
                    topic, e
                );
                Ok(false)
            }
        }
    }

    /// Estimates the resources required for an operation.
    async fn estimate_resources(&self, operation_type: &OperationType) -> ResourceEstimate {
        match operation_type {
            OperationType::Add => ResourceEstimate {
                cpu_usage: 0.3,
                memory_bytes: 64 * 1024,
                io_bytes: 128 * 1024,
                bandwidth_bytes: 256 * 1024,
                estimated_time_ms: 20,
            },
            OperationType::Get => ResourceEstimate {
                cpu_usage: 0.2,
                memory_bytes: 32 * 1024,
                io_bytes: 64 * 1024,
                bandwidth_bytes: 128 * 1024,
                estimated_time_ms: 15,
            },
            OperationType::Pin | OperationType::Unpin => ResourceEstimate {
                cpu_usage: 0.1,
                memory_bytes: 8 * 1024,
                io_bytes: 16 * 1024,
                bandwidth_bytes: 32 * 1024,
                estimated_time_ms: 5,
            },
            OperationType::PubSubPublish => ResourceEstimate {
                cpu_usage: 0.15,
                memory_bytes: 16 * 1024,
                io_bytes: 8 * 1024,
                bandwidth_bytes: 64 * 1024,
                estimated_time_ms: 8,
            },
        }
    }

    /// Records operation timing for optimization.
    async fn record_operation_timing(
        &self,
        operation_type: OperationType,
        duration: Duration,
        data_size: u64,
    ) {
        let mut history = self.operation_history.write().await;

        let timing_entry = TimingEntry {
            operation_type: operation_type.clone(),
            timestamp: Instant::now(),
            duration_ms: duration.as_millis() as f64,
            data_size,
        };

        history.timing_history.push_back(timing_entry);

        // Keep the history bounded.
        if history.timing_history.len() > 10000 {
            history.timing_history.pop_front();
        }

        // Update the patterns.
        let pattern = history
            .operation_patterns
            .entry(operation_type)
            .or_insert_with(|| OperationPattern {
                avg_frequency: 0.0,
                avg_data_size: 0,
                avg_processing_time_ms: 0.0,
                peak_hours: vec![],
                correlated_operations: vec![],
            });

        pattern.avg_processing_time_ms =
            (pattern.avg_processing_time_ms + duration.as_millis() as f64) / 2.0;
        pattern.avg_data_size = (pattern.avg_data_size + data_size) / 2;
    }

    /// Starts the automatic batch processor.
    pub fn start_auto_processor(&self) -> tokio::task::JoinHandle<()> {
        let pending_operations = Arc::clone(&self.pending_operations);
        let typed_queues = Arc::clone(&self.typed_queues);
        let batch_config = self.batch_config.clone();
        let stats = Arc::clone(&self.stats);
        let processing_semaphore = Arc::clone(&self.processing_semaphore);
        let operation_history = Arc::clone(&self.operation_history);
        let backend = Arc::clone(&self.backend);

        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_millis(batch_config.max_batch_wait_ms));

            loop {
                interval.tick().await;

                // Process the typed queues if enabled.
                if batch_config.enable_smart_batching {
                    // Automatic processing of the typed queues.
                    let should_process = {
                        let queues = typed_queues.read().await;

                        // Check whether any queue has reached the processing limits.
                        let add_ready = queues.add_queue.len() >= batch_config.max_batch_size / 4;
                        let get_ready = queues.get_queue.len() >= batch_config.max_batch_size / 2;
                        let pin_ready = queues.pin_queue.len() >= batch_config.max_batch_size / 3;
                        let pubsub_ready =
                            queues.pubsub_queue.len() >= batch_config.max_batch_size / 6;

                        // Or check the timeout of the oldest operations.
                        let now = Instant::now();
                        let timeout_threshold =
                            Duration::from_millis(batch_config.max_batch_wait_ms * 2);

                        let add_timeout = queues
                            .add_queue
                            .front()
                            .map(|op| {
                                now.saturating_duration_since(op.created_at) > timeout_threshold
                            })
                            .unwrap_or(false);
                        let get_timeout = queues
                            .get_queue
                            .front()
                            .map(|op| {
                                now.saturating_duration_since(op.created_at) > timeout_threshold
                            })
                            .unwrap_or(false);
                        let pin_timeout = queues
                            .pin_queue
                            .front()
                            .map(|op| {
                                now.saturating_duration_since(op.created_at) > timeout_threshold
                            })
                            .unwrap_or(false);

                        add_ready
                            || get_ready
                            || pin_ready
                            || pubsub_ready
                            || add_timeout
                            || get_timeout
                            || pin_timeout
                    };

                    if should_process {
                        // Process batches using the semaphore to control concurrency.
                        if let Ok(_permit) = processing_semaphore.try_acquire() {
                            let queues_clone = Arc::clone(&typed_queues);
                            let stats_clone = Arc::clone(&stats);
                            let batch_config_clone = batch_config.clone();
                            let history_clone = Arc::clone(&operation_history);
                            let backend_clone = Arc::clone(&backend);

                            tokio::spawn(async move {
                                if let Err(e) = Self::process_automatic_typed_batches(
                                    queues_clone,
                                    stats_clone,
                                    batch_config_clone,
                                    history_clone,
                                    backend_clone,
                                )
                                .await
                                {
                                    debug!(target: "batch_processor", error = %e, "Error in automatic processing");
                                }
                                // The permit is automatically released when it goes out of scope.
                            });
                        }
                    }
                } else {
                    // Process the simple queue.
                    let pending_count = pending_operations.lock().await.len();
                    if pending_count > 0 {
                        // Trigger processing using the semaphore.
                        if let Ok(_permit) = processing_semaphore.try_acquire() {
                            let ops_clone = Arc::clone(&pending_operations);
                            let stats_clone = Arc::clone(&stats);
                            let batch_config_clone = batch_config.clone();
                            let backend_clone = Arc::clone(&backend);

                            tokio::spawn(async move {
                                if let Err(e) = Self::process_automatic_simple_batch(
                                    ops_clone,
                                    stats_clone,
                                    batch_config_clone,
                                    backend_clone,
                                )
                                .await
                                {
                                    debug!(target: "batch_processor", error = %e, "Error in simple processing");
                                }
                            });
                        }
                    }
                }
            }
        })
    }

    /// Processes automatic batches from the typed queues with the IrohBackend.
    async fn process_automatic_typed_batches(
        typed_queues: Arc<RwLock<TypedQueues>>,
        stats: Arc<RwLock<BatchStats>>,
        batch_config: BatchConfig,
        _operation_history: Arc<RwLock<OperationHistory>>,
        backend: Arc<crate::p2p::network::core::IrohBackend>,
    ) -> Result<()> {
        let mut batches_to_process = Vec::new();

        // Extract batches from each queue that needs processing.
        {
            let mut queues = typed_queues.write().await;

            // Add processing.
            if !queues.add_queue.is_empty()
                && (queues.add_queue.len() >= batch_config.max_batch_size / 4
                    || Self::has_old_operations(&queues.add_queue, batch_config.max_batch_wait_ms))
            {
                let batch_size = (batch_config.max_batch_size / 4).max(queues.add_queue.len());
                let batch = Self::extract_operations_static(&mut queues.add_queue, batch_size);
                if !batch.is_empty() {
                    batches_to_process.push((OperationType::Add, batch));
                }
            }

            // Get processing.
            if !queues.get_queue.is_empty()
                && (queues.get_queue.len() >= batch_config.max_batch_size / 2
                    || Self::has_old_operations(&queues.get_queue, batch_config.max_batch_wait_ms))
            {
                let batch_size = (batch_config.max_batch_size / 2).max(queues.get_queue.len());
                let batch = Self::extract_operations_static(&mut queues.get_queue, batch_size);
                if !batch.is_empty() {
                    batches_to_process.push((OperationType::Get, batch));
                }
            }

            // Pin processing.
            if !queues.pin_queue.is_empty()
                && (queues.pin_queue.len() >= batch_config.max_batch_size / 3
                    || Self::has_old_operations(&queues.pin_queue, batch_config.max_batch_wait_ms))
            {
                let batch_size = (batch_config.max_batch_size / 3).max(queues.pin_queue.len());
                let batch = Self::extract_operations_static(&mut queues.pin_queue, batch_size);
                if !batch.is_empty() {
                    batches_to_process.push((OperationType::Pin, batch));
                }
            }

            // PubSub processing.
            if !queues.pubsub_queue.is_empty() {
                let pubsub_len = queues.pubsub_queue.len();
                let batch = Self::extract_operations_static(&mut queues.pubsub_queue, pubsub_len);
                if !batch.is_empty() {
                    batches_to_process.push((OperationType::PubSubPublish, batch));
                }
            }
        }

        // Process each extracted batch.
        for (batch_type, batch) in batches_to_process {
            let batch_size = batch.len();
            let start_time = Instant::now();

            debug!(target: "batch_processor",
                batch_type = ?batch_type,
                batch_size = batch_size,
                "Processing automatic batch"
            );

            // Process the batch based on its type.
            match batch_type {
                OperationType::Add => Self::process_add_batch_static(batch, &backend).await?,
                OperationType::Get => Self::process_get_batch_static(batch, &backend).await?,
                OperationType::Pin => Self::process_pin_batch_static(batch, &backend).await?,
                OperationType::PubSubPublish => {
                    Self::process_pubsub_batch_static(batch, &backend).await?
                }
                _ => Self::process_individual_batch_static(batch, &backend).await?,
            }

            // Update statistics.
            let processing_time = start_time.elapsed();
            let mut stats_lock = stats.write().await;
            stats_lock.batched_operations += batch_size as u64;
            stats_lock.total_operations += batch_size as u64;
            stats_lock.avg_batch_size = (stats_lock.avg_batch_size + batch_size as f64) / 2.0;
            stats_lock.avg_batch_processing_time_ms = (stats_lock.avg_batch_processing_time_ms
                + processing_time.as_millis() as f64)
                / 2.0;

            info!(target: "batch_processor",
                batch_type = ?batch_type,
                batch_size = batch_size,
                processing_time_ms = processing_time.as_millis(),
                "Automatic batch processed successfully"
            );
        }

        Ok(())
    }

    /// Processes a simple automatic batch with the IrohBackend.
    async fn process_automatic_simple_batch(
        pending_operations: Arc<Mutex<VecDeque<BatchOperation>>>,
        stats: Arc<RwLock<BatchStats>>,
        batch_config: BatchConfig,
        backend: Arc<crate::p2p::network::core::IrohBackend>,
    ) -> Result<()> {
        let batch = {
            let mut pending = pending_operations.lock().await;
            let batch_size = batch_config.max_batch_size.min(pending.len());
            pending.drain(..batch_size).collect::<Vec<_>>()
        };

        if batch.is_empty() {
            return Ok(());
        }

        let batch_size = batch.len();
        let start_time = Instant::now();

        debug!(target: "batch_processor",
            batch_size = batch_size,
            "Processing simple automatic batch"
        );

        // Process operations individually.
        Self::process_individual_batch_static(batch, &backend).await?;

        // Update statistics.
        let processing_time = start_time.elapsed();
        let mut stats_lock = stats.write().await;
        stats_lock.total_operations += batch_size as u64;
        stats_lock.individual_operations += batch_size as u64;
        stats_lock.avg_batch_processing_time_ms =
            (stats_lock.avg_batch_processing_time_ms + processing_time.as_millis() as f64) / 2.0;

        info!(target: "batch_processor",
            batch_size = batch_size,
            processing_time_ms = processing_time.as_millis(),
            "Simple automatic batch processed"
        );

        Ok(())
    }

    /// Checks whether there are old operations in the queue.
    fn has_old_operations(queue: &VecDeque<BatchOperation>, max_wait_ms: u64) -> bool {
        if let Some(oldest) = queue.front() {
            let age = Instant::now().saturating_duration_since(oldest.created_at);
            age > Duration::from_millis(max_wait_ms * 2)
        } else {
            false
        }
    }

    /// Extracts operations from a queue (static version).
    fn extract_operations_static(
        queue: &mut VecDeque<BatchOperation>,
        max_size: usize,
    ) -> Vec<BatchOperation> {
        let mut batch = Vec::with_capacity(max_size);

        // Sort by priority.
        let mut temp_vec: Vec<_> = queue.drain(..).collect();
        temp_vec.sort_by_key(|operation| std::cmp::Reverse(operation.priority));

        // Take the first max_size.
        for operation in temp_vec.into_iter().take(max_size) {
            batch.push(operation);
        }

        batch
    }

    /// Processes an Add batch (static version) with the IrohBackend.
    async fn process_add_batch_static(
        batch: Vec<BatchOperation>,
        backend: &crate::p2p::network::core::IrohBackend,
    ) -> Result<()> {
        for operation in batch {
            Self::process_individual_operation_static(operation, backend).await?;
        }
        Ok(())
    }

    /// Processes a Get batch (static version) with the IrohBackend.
    async fn process_get_batch_static(
        batch: Vec<BatchOperation>,
        backend: &crate::p2p::network::core::IrohBackend,
    ) -> Result<()> {
        for operation in batch {
            Self::process_individual_operation_static(operation, backend).await?;
        }
        Ok(())
    }

    /// Processes a Pin batch (static version) with the IrohBackend.
    async fn process_pin_batch_static(
        batch: Vec<BatchOperation>,
        backend: &crate::p2p::network::core::IrohBackend,
    ) -> Result<()> {
        for operation in batch {
            Self::process_individual_operation_static(operation, backend).await?;
        }
        Ok(())
    }

    /// Processes a PubSub batch (static version) with the IrohBackend.
    async fn process_pubsub_batch_static(
        batch: Vec<BatchOperation>,
        backend: &crate::p2p::network::core::IrohBackend,
    ) -> Result<()> {
        for operation in batch {
            Self::process_individual_operation_static(operation, backend).await?;
        }
        Ok(())
    }

    /// Processes an individual batch (static version) with the IrohBackend.
    async fn process_individual_batch_static(
        batch: Vec<BatchOperation>,
        backend: &crate::p2p::network::core::IrohBackend,
    ) -> Result<()> {
        for operation in batch {
            Self::process_individual_operation_static(operation, backend).await?;
        }
        Ok(())
    }

    /// Processes an individual operation (static version) with the IrohBackend.
    async fn process_individual_operation_static(
        operation: BatchOperation,
        backend: &crate::p2p::network::core::IrohBackend,
    ) -> Result<()> {
        let result = match operation.data {
            OperationData::AddData { data, .. } => Self::add_operation_static(data, backend)
                .await
                .map(OperationResult::AddResult),
            OperationData::GetHash { hash, .. } => Self::get_operation_static(hash, backend)
                .await
                .map(OperationResult::GetResult),
            OperationData::PinHash { hash, .. } => Self::pin_operation_static(hash, backend)
                .await
                .map(OperationResult::PinResult),
            OperationData::PubSubData { topic, data } => {
                Self::pubsub_operation_static(topic, data, backend)
                    .await
                    .map(OperationResult::PubSubResult)
            }
            _ => Err(GuardianError::Other(
                "Operation not implemented".to_string(),
            )),
        };

        let _ = operation.result_sender.send(result);
        Ok(())
    }

    /// Static Add operation using the IrohBackend.
    async fn add_operation_static(
        data: Bytes,
        backend: &crate::p2p::network::core::IrohBackend,
    ) -> Result<AddResponse> {
        use std::pin::Pin;
        use tokio::io::AsyncRead;

        let cursor = std::io::Cursor::new(data.to_vec());
        let async_read: Pin<Box<dyn AsyncRead + Send>> = Box::pin(cursor);

        backend
            .add(async_read)
            .await
            .map_err(|e| GuardianError::Other(format!("Error in add: {}", e)))
    }

    /// Static Get operation using the IrohBackend.
    async fn get_operation_static(
        hash: String,
        backend: &crate::p2p::network::core::IrohBackend,
    ) -> Result<Bytes> {
        use tokio::io::AsyncReadExt;

        let mut async_read = backend
            .cat(&hash)
            .await
            .map_err(|e| GuardianError::Other(format!("Error in cat for {}: {}", hash, e)))?;

        let mut buffer = Vec::new();
        async_read
            .read_to_end(&mut buffer)
            .await
            .map_err(|e| GuardianError::Other(format!("Error reading data: {}", e)))?;

        Ok(Bytes::from(buffer))
    }

    /// Static Pin operation using the IrohBackend.
    async fn pin_operation_static(
        hash: String,
        backend: &crate::p2p::network::core::IrohBackend,
    ) -> Result<bool> {
        backend.pin_add(&hash).await.map(|_| true).or(Ok(false))
    }

    /// Static PubSub operation - not supported (requires Arc).
    async fn pubsub_operation_static(
        _topic: String,
        _data: Bytes,
        _iroh_backend: &crate::p2p::network::core::IrohBackend,
    ) -> Result<bool> {
        warn!("PubSub operation not supported in a static context");
        Ok(false)
    }

    /// Returns the current statistics.
    pub async fn get_stats(&self) -> BatchStats {
        let stats = self.stats.read().await;
        let mut stats_copy = stats.clone();

        // Compute efficiency.
        if stats_copy.total_operations > 0 {
            stats_copy.batch_efficiency =
                stats_copy.batched_operations as f64 / stats_copy.total_operations as f64;
            // operations_per_second is updated during batch processing.
            // We do not recompute it here to avoid timing issues.
        }

        stats_copy
    }
}
