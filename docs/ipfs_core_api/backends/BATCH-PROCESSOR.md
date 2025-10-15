# Batch Processor - High-Performance Operation Batching Documentation

## Overview

The Batch Processor is an intelligent operation batching system designed to optimize throughput for IPFS operations in the Iroh backend. It reduces overhead and improves I/O efficiency by grouping similar operations together, processing them in optimized batches with priority-based scheduling and automatic resource management.

## Table of Contents

- [Architecture](#architecture)
- [Core Components](#core-components)
- [Operation Types](#operation-types)
- [Configuration](#configuration)
- [API Reference](#api-reference)
- [Smart Batching](#smart-batching)
- [Auto-Processing](#auto-processing)
- [Statistics and Monitoring](#statistics-and-monitoring)
- [Usage Examples](#usage-examples)
- [Performance Optimization](#performance-optimization)
- [Best Practices](#best-practices)

## Architecture

### High-Level Architecture

```
┌──────────────────────────────────────────────────────────┐
│                   BatchProcessor                         │
├──────────────────────────────────────────────────────────┤
│  ┌────────────────────────────────────────────────┐     │
│  │           Smart Batching Mode                  │     │
│  │  ┌──────────┐ ┌──────────┐ ┌──────────┐       │     │
│  │  │ Add      │ │ Get      │ │ Pin      │       │     │
│  │  │ Queue    │ │ Queue    │ │ Queue    │       │     │
│  │  └──────────┘ └──────────┘ └──────────┘       │     │
│  │  ┌──────────┐ ┌──────────┐ ┌──────────┐       │     │
│  │  │ DAG      │ │ PubSub   │ │ DHT      │       │     │
│  │  │ Queue    │ │ Queue    │ │ Queue    │       │     │
│  │  └──────────┘ └──────────┘ └──────────┘       │     │
│  └────────────────────────────────────────────────┘     │
│                         OR                               │
│  ┌────────────────────────────────────────────────┐     │
│  │           Simple Mode                          │     │
│  │      Pending Operations Queue                  │     │
│  └────────────────────────────────────────────────┘     │
│                                                          │
│  ┌────────────────────────────────────────────────┐     │
│  │       Processing Semaphore (8 concurrent)      │     │
│  └────────────────────────────────────────────────┘     │
│                                                          │
│  ┌────────────────────────────────────────────────┐     │
│  │       Statistics & History Tracking            │     │
│  └────────────────────────────────────────────────┘     │
└──────────────────────────────────────────────────────────┘
```

### Processing Flow

```
Operation Submitted
    ↓
[Smart Batching?]
    ├─Yes→ Route to Typed Queue (Add/Get/Pin/DAG/PubSub/DHT)
    └─No──→ Add to Pending Queue
    ↓
[Check Immediate Processing]
    ├─ Batch Size Threshold Met?
    ├─ Timeout Threshold Met?
    └─ Memory Threshold Met?
    ↓
[Acquire Semaphore Permit]
    ↓
Extract Batch (Priority-Sorted)
    ↓
Process Batch (Type-Specific Optimization)
    ↓
Update Statistics
    ↓
Return Results via Channels
```

## Core Components

### 1. BatchProcessor

Main processor structure:

```rust
pub struct BatchProcessor {
    /// Pending operations queue
    pending_operations: Arc<Mutex<VecDeque<BatchOperation>>>,
    
    /// Type-organized queues for optimization
    typed_queues: Arc<RwLock<TypedQueues>>,
    
    /// Processor configuration
    config: BatchConfig,
    
    /// Performance statistics
    stats: Arc<RwLock<BatchStats>>,
    
    /// Control channel
    control_sender: mpsc::Sender<BatchControl>,
    
    /// Concurrency control semaphore (8 concurrent batches max)
    processing_semaphore: Arc<Semaphore>,
    
    /// Operation history for optimization
    operation_history: Arc<RwLock<OperationHistory>>,
}
```

### 2. BatchOperation

Individual operation structure:

```rust
pub struct BatchOperation {
    /// Unique operation ID (UUID)
    pub id: String,
    
    /// Operation type
    pub operation_type: OperationType,
    
    /// Operation data
    pub data: OperationData,
    
    /// Creation timestamp
    pub created_at: Instant,
    
    /// Priority (0-10, higher = more important)
    pub priority: u8,
    
    /// Result channel
    pub result_sender: oneshot::Sender<Result<OperationResult>>,
    
    /// Resource estimate
    pub resource_estimate: ResourceEstimate,
}
```

### 3. TypedQueues

Separate queues for each operation type:

```rust
pub struct TypedQueues {
    /// Add operations queue
    add_queue: VecDeque<BatchOperation>,
    
    /// Get operations queue
    get_queue: VecDeque<BatchOperation>,
    
    /// Pin operations queue
    pin_queue: VecDeque<BatchOperation>,
    
    /// DAG operations queue
    dag_queue: VecDeque<BatchOperation>,
    
    /// PubSub operations queue
    pubsub_queue: VecDeque<BatchOperation>,
    
    /// DHT operations queue
    dht_queue: VecDeque<BatchOperation>,
}
```

## Operation Types

### Supported Operations

```rust
pub enum OperationType {
    /// Add data to IPFS
    Add,
    
    /// Retrieve data from IPFS
    Get,
    
    /// Pin content
    Pin,
    
    /// Unpin content
    Unpin,
    
    /// DAG Put operation
    DagPut,
    
    /// DAG Get operation
    DagGet,
    
    /// PubSub publish
    PubSubPublish,
    
    /// DHT operation
    DhtOperation,
}
```

### Operation Data

```rust
pub enum OperationData {
    /// Data to add
    AddData { data: Bytes, options: AddOptions },
    
    /// CID to retrieve
    GetCid { cid: String, options: GetOptions },
    
    /// CID to pin
    PinCid { cid: String, options: PinOptions },
    
    /// CID to unpin
    UnpinCid { cid: String },
    
    /// DAG data to store
    DagPutData { data: Bytes, format: String },
    
    /// DAG CID to retrieve
    DagGetCid { cid: String },
    
    /// Data to publish
    PubSubData { topic: String, data: Bytes },
    
    /// Generic DHT operation
    DhtData { key: String, value: Option<Bytes> },
}
```

### Operation Results

```rust
pub enum OperationResult {
    /// Add result
    AddResult(AddResponse),
    
    /// Get result
    GetResult(Bytes),
    
    /// Pin result
    PinResult(bool),
    
    /// Unpin result
    UnpinResult(bool),
    
    /// DAG Put result
    DagPutResult(Cid),
    
    /// DAG Get result
    DagGetResult(Bytes),
    
    /// PubSub result
    PubSubResult(bool),
    
    /// DHT result
    DhtResult(Option<Bytes>),
}
```

## Configuration

### BatchConfig

```rust
pub struct BatchConfig {
    /// Maximum batch size (operations)
    pub max_batch_size: usize,              // Default: 100
    
    /// Maximum wait time to form batch (ms)
    pub max_batch_wait_ms: u64,             // Default: 50
    
    /// Maximum processing threads
    pub max_processing_threads: usize,       // Default: 8
    
    /// Memory flush threshold (bytes)
    pub memory_flush_threshold: u64,         // Default: 64MB
    
    /// Enable smart batching
    pub enable_smart_batching: bool,         // Default: true
    
    /// Enable batch compression
    pub enable_batch_compression: bool,      // Default: true
    
    /// Compression threshold (bytes)
    pub compression_threshold: usize,        // Default: 1KB
}
```

### Default Configuration

```rust
impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_batch_size: 100,
            max_batch_wait_ms: 50,
            max_processing_threads: 8,
            memory_flush_threshold: 64 * 1024 * 1024,  // 64MB
            enable_smart_batching: true,
            enable_batch_compression: true,
            compression_threshold: 1024,                 // 1KB
        }
    }
}
```

## API Reference

### Creating a Batch Processor

```rust
pub fn new(config: BatchConfig) -> Self
```

**Parameters:**
- `config`: Batch processor configuration

**Returns:**
- `BatchProcessor` instance

**Example:**
```rust
let config = BatchConfig::default();
let processor = BatchProcessor::new(config);
```

### Adding Operations

```rust
pub async fn add_operation(
    &self,
    operation_type: OperationType,
    data: OperationData,
    priority: u8,
) -> Result<OperationResult>
```

**Parameters:**
- `operation_type`: Type of operation to perform
- `data`: Operation-specific data
- `priority`: Priority level (0-10, higher = more important)

**Returns:**
- `Result<OperationResult>`: Operation result or error

**Example:**
```rust
// Add data with high priority
let data = Bytes::from("Hello, IPFS!");
let result = processor.add_operation(
    OperationType::Add,
    OperationData::AddData {
        data,
        options: AddOptions::default(),
    },
    9,  // High priority
).await?;

if let OperationResult::AddResult(add_response) = result {
    println!("Added with CID: {}", add_response.hash);
}
```

### Starting Auto-Processor

```rust
pub fn start_auto_processor(&self) -> tokio::task::JoinHandle<()>
```

**Description:** Starts background task that automatically processes batches.

**Returns:**
- `JoinHandle` for the background task

**Example:**
```rust
let processor = BatchProcessor::new(config);
let handle = processor.start_auto_processor();

// Processor now runs in background
// Add operations and they'll be batched automatically
```

### Getting Statistics

```rust
pub async fn get_stats(&self) -> BatchStats
```

**Returns:**
- `BatchStats`: Current processor statistics

**Example:**
```rust
let stats = processor.get_stats().await;
println!("Total operations: {}", stats.total_operations);
println!("Batch efficiency: {:.1}%", stats.batch_efficiency * 100.0);
println!("Avg batch size: {:.2}", stats.avg_batch_size);
```

## Smart Batching

### Type-Based Queue Routing

Smart batching routes operations to type-specific queues:

```rust
async fn add_to_typed_queue(&self, operation: BatchOperation) -> Result<()> {
    let mut queues = self.typed_queues.write().await;
    
    match operation.operation_type {
        OperationType::Add => queues.add_queue.push_back(operation),
        OperationType::Get => queues.get_queue.push_back(operation),
        OperationType::Pin | OperationType::Unpin => queues.pin_queue.push_back(operation),
        OperationType::DagPut | OperationType::DagGet => queues.dag_queue.push_back(operation),
        OperationType::PubSubPublish => queues.pubsub_queue.push_back(operation),
        OperationType::DhtOperation => queues.dht_queue.push_back(operation),
    }
    
    Ok(())
}
```

### Batch Size Thresholds

Different operation types have different optimal batch sizes:

| Operation Type | Threshold | Reason |
|----------------|-----------|--------|
| Add | 25% of max | I/O intensive, benefit from larger batches |
| Get | 50% of max | Can be parallelized, process sooner |
| Pin | 33% of max | Lightweight, can wait for larger batch |
| DAG | 25% of max | Complex operations, moderate batch |
| PubSub | Immediate | Time-sensitive, process ASAP |
| DHT | Immediate | Network operations, process ASAP |

### Immediate Processing Conditions

Batches are processed immediately when:

1. **Size threshold met**: Queue reaches its threshold
2. **Timeout threshold met**: Oldest operation > 2× max_batch_wait_ms
3. **Memory threshold met**: Total queued data > memory_flush_threshold
4. **High priority operation**: Priority ≥ 8

```rust
async fn check_smart_processing(&self) -> Result<()> {
    let queues = self.typed_queues.read().await;
    
    let add_ready = queues.add_queue.len() >= self.config.max_batch_size / 4;
    let get_ready = queues.get_queue.len() >= self.config.max_batch_size / 2;
    let pin_ready = queues.pin_queue.len() >= self.config.max_batch_size / 3;
    let dag_ready = queues.dag_queue.len() >= self.config.max_batch_size / 4;
    
    if add_ready || get_ready || pin_ready || dag_ready {
        self.process_ready_batches().await?;
    }
    
    Ok(())
}
```

## Auto-Processing

### Background Processing Loop

The auto-processor runs continuously in the background:

```rust
pub fn start_auto_processor(&self) -> tokio::task::JoinHandle<()> {
    // Runs every max_batch_wait_ms
    let mut interval = tokio::time::interval(
        Duration::from_millis(config.max_batch_wait_ms)
    );
    
    loop {
        interval.tick().await;
        
        // Check each queue for readiness
        // Process batches that are ready
        // Controlled by semaphore (max 8 concurrent)
    }
}
```

**Processing Triggers:**
- **Timer-based**: Every `max_batch_wait_ms` milliseconds (default: 50ms)
- **Size-based**: When queue reaches threshold
- **Timeout-based**: When oldest operation ages > 2× max_batch_wait_ms

### Concurrency Control

Uses semaphore to limit concurrent batch processing:

```rust
// Max 8 concurrent batches
processing_semaphore: Arc::new(Semaphore::new(8))

// Acquire permit before processing
if let Ok(_permit) = processing_semaphore.try_acquire() {
    tokio::spawn(async move {
        // Process batch
        // Permit automatically released when dropped
    });
}
```

**Benefits:**
- Prevents resource exhaustion
- Maintains consistent performance
- Avoids thread pool saturation

### Timeout Detection

```rust
fn has_old_operations(queue: &VecDeque<BatchOperation>, max_wait_ms: u64) -> bool {
    if let Some(oldest) = queue.front() {
        let age = Instant::now().duration_since(oldest.created_at);
        age > Duration::from_millis(max_wait_ms * 2)
    } else {
        false
    }
}
```

**Timeout Behavior:**
- Checks oldest operation in each queue
- Triggers processing if age > 2× max_batch_wait_ms
- Prevents operation starvation

## Priority-Based Processing

### Priority Sorting

Operations are sorted by priority before processing:

```rust
fn extract_batch(
    &self,
    queue: &mut VecDeque<BatchOperation>,
    max_size: usize,
) -> Vec<BatchOperation> {
    let mut temp_vec: Vec<_> = queue.drain(..).collect();
    
    // Sort by priority (descending)
    temp_vec.sort_by(|a, b| b.priority.cmp(&a.priority));
    
    // Take first max_size operations
    temp_vec.into_iter().take(max_size).collect()
}
```

**Priority Levels:**
- **0-3**: Low priority (background operations)
- **4-6**: Normal priority (regular operations)
- **7-8**: High priority (user-facing operations)
- **9-10**: Critical priority (system operations)

## Batch Processing Strategies

### Add Batch Processing

Sequential processing (I/O optimization):

```rust
async fn process_add_batch_static(batch: Vec<BatchOperation>) -> Result<()> {
    for operation in batch {
        Self::process_individual_operation_static(operation).await?;
    }
    Ok(())
}
```

**Characteristics:**
- Sequential execution (I/O bound)
- Simulates file system writes
- 5ms base + size-based delay

### Get Batch Processing

Parallel processing (network optimization):

```rust
async fn process_get_batch_static(batch: Vec<BatchOperation>) -> Result<()> {
    // Spawn parallel tasks
    let futures = batch.into_iter().map(|operation| {
        tokio::spawn(async move {
            Self::process_individual_operation_static(operation).await
        })
    });
    
    // Wait for all
    let results = futures::future::join_all(futures).await;
    
    // Handle errors
    for result in results {
        if let Err(e) = result {
            debug!("Get operation error: {}", e);
        }
    }
    
    Ok(())
}
```

**Characteristics:**
- Parallel execution (network bound)
- Maximum concurrency for Gets
- 3ms base + size-based delay

### Pin Batch Processing

Sequential processing (metadata operation):

```rust
async fn process_pin_batch_static(batch: Vec<BatchOperation>) -> Result<()> {
    for operation in batch {
        Self::process_individual_operation_static(operation).await?;
    }
    Ok(())
}
```

**Characteristics:**
- Sequential execution (metadata)
- Fast processing (1ms base)
- High success rate (95%)

### PubSub Batch Processing

Sequential processing (order-preserving):

```rust
async fn process_pubsub_batch_static(batch: Vec<BatchOperation>) -> Result<()> {
    for operation in batch {
        Self::process_individual_operation_static(operation).await?;
    }
    Ok(())
}
```

**Characteristics:**
- Sequential execution (preserves order)
- Fast processing (2ms base)
- Validates topic and data

## Resource Estimation

### ResourceEstimate

```rust
pub struct ResourceEstimate {
    /// Estimated CPU usage (0.0-1.0)
    pub cpu_usage: f64,
    
    /// Estimated memory (bytes)
    pub memory_bytes: u64,
    
    /// Estimated I/O (bytes)
    pub io_bytes: u64,
    
    /// Estimated bandwidth (bytes)
    pub bandwidth_bytes: u64,
    
    /// Estimated time (ms)
    pub estimated_time_ms: u64,
}
```

**Usage:**
```rust
async fn estimate_resources(&self, operation_type: &OperationType) -> ResourceEstimate {
    match operation_type {
        OperationType::Add => ResourceEstimate {
            cpu_usage: 0.3,
            memory_bytes: 1024 * 1024,  // 1MB
            io_bytes: 1024 * 1024,
            bandwidth_bytes: 512 * 1024,
            estimated_time_ms: 50,
        },
        // ... other types
    }
}
```

## Statistics and Monitoring

### BatchStats

```rust
pub struct BatchStats {
    /// Total operations processed
    pub total_operations: u64,
    
    /// Operations processed in batches
    pub batched_operations: u64,
    
    /// Operations processed individually
    pub individual_operations: u64,
    
    /// Average batch size
    pub avg_batch_size: f64,
    
    /// Average processing time (ms)
    pub avg_batch_processing_time_ms: f64,
    
    /// Throughput (ops/second)
    pub operations_per_second: f64,
    
    /// Batch efficiency (0.0-1.0)
    pub batch_efficiency: f64,
    
    /// Total bytes processed
    pub total_bytes_processed: u64,
    
    /// Resource savings (0.0-1.0)
    pub resource_savings: f64,
}
```

### Efficiency Calculation

```rust
// Batch efficiency = batched_operations / total_operations
stats.batch_efficiency = stats.batched_operations as f64 / stats.total_operations as f64;

// Throughput calculation
stats.operations_per_second = stats.total_operations as f64 / elapsed_time.as_secs_f64();
```

### Statistics Monitoring

```rust
// Get current statistics
let stats = processor.get_stats().await;

println!("Batch Processor Statistics:");
println!("  Total operations: {}", stats.total_operations);
println!("  Batched: {} ({:.1}%)", 
    stats.batched_operations,
    stats.batch_efficiency * 100.0
);
println!("  Individual: {}", stats.individual_operations);
println!("  Avg batch size: {:.2}", stats.avg_batch_size);
println!("  Avg processing time: {:.2}ms", stats.avg_batch_processing_time_ms);
println!("  Throughput: {:.2} ops/s", stats.operations_per_second);
```

## Usage Examples

### Basic Usage

```rust
use guardian_db::ipfs_core_api::backends::batch_processor::*;

// Create processor with default config
let processor = BatchProcessor::new(BatchConfig::default());

// Start auto-processor
let handle = processor.start_auto_processor();

// Add operations
let data = Bytes::from("Hello, Batch!");
let result = processor.add_operation(
    OperationType::Add,
    OperationData::AddData {
        data,
        options: AddOptions::default(),
    },
    5,  // Normal priority
).await?;

// Handle result
match result {
    OperationResult::AddResult(response) => {
        println!("Added: {}", response.hash);
    },
    _ => unreachable!(),
}
```

### Batch Multiple Operations

```rust
// Submit multiple operations
let mut handles = Vec::new();

for i in 0..100 {
    let processor_clone = processor.clone();
    let handle = tokio::spawn(async move {
        let data = Bytes::from(format!("Data {}", i));
        processor_clone.add_operation(
            OperationType::Add,
            OperationData::AddData {
                data,
                options: AddOptions::default(),
            },
            5,
        ).await
    });
    handles.push(handle);
}

// Wait for all operations
let results = futures::future::join_all(handles).await;
```

### Custom Configuration

```rust
// High-throughput configuration
let high_throughput_config = BatchConfig {
    max_batch_size: 200,
    max_batch_wait_ms: 100,
    max_processing_threads: 16,
    enable_smart_batching: true,
    ..Default::default()
};

let processor = BatchProcessor::new(high_throughput_config);
```

### Priority-Based Processing

```rust
// Submit critical operation
let result = processor.add_operation(
    OperationType::Pin,
    OperationData::PinCid {
        cid: "QmXyz...".to_string(),
        options: PinOptions::default(),
    },
    10,  // Maximum priority
).await?;

// Submit background operation
let result = processor.add_operation(
    OperationType::Get,
    OperationData::GetCid {
        cid: "QmAbc...".to_string(),
        options: GetOptions::default(),
    },
    2,  // Low priority
).await?;
```

### Mixed Operation Types

```rust
// Add content
processor.add_operation(
    OperationType::Add,
    OperationData::AddData { /* ... */ },
    5,
).await?;

// Pin content
processor.add_operation(
    OperationType::Pin,
    OperationData::PinCid { /* ... */ },
    6,
).await?;

// Publish to PubSub
processor.add_operation(
    OperationType::PubSubPublish,
    OperationData::PubSubData {
        topic: "announcements".to_string(),
        data: Bytes::from("New content available!"),
    },
    8,
).await?;

// All operations are routed to appropriate queues
// and processed in optimized batches
```

## Operation History

### OperationHistory

Tracks patterns for future optimization:

```rust
pub struct OperationHistory {
    /// Patterns by operation type
    operation_patterns: HashMap<OperationType, OperationPattern>,
    
    /// Correlations between operations
    operation_correlations: HashMap<String, Vec<OperationType>>,
    
    /// Historical timing data
    timing_history: VecDeque<TimingEntry>,
}
```

### OperationPattern

```rust
pub struct OperationPattern {
    /// Average frequency
    pub avg_frequency: f64,
    
    /// Average data size
    pub avg_data_size: u64,
    
    /// Average processing time (ms)
    pub avg_processing_time_ms: f64,
    
    /// Peak hours (0-23)
    pub peak_hours: Vec<u8>,
    
    /// Correlated operations
    pub correlated_operations: Vec<OperationType>,
}
```

**Future Use:**
- Predictive batching
- Adaptive thresholds
- Pattern-based optimization
- Correlation detection

## Performance Optimization

### Throughput Optimization

**Key factors:**
- Larger batches = higher throughput
- Longer wait times = larger batches
- More threads = higher concurrency

**Recommended for high throughput:**
```rust
let config = BatchConfig {
    max_batch_size: 200,
    max_batch_wait_ms: 100,
    max_processing_threads: 16,
    enable_smart_batching: true,
    ..Default::default()
};
```

### Latency Optimization

**Key factors:**
- Smaller batches = lower latency
- Shorter wait times = faster processing
- Immediate processing for critical ops

**Recommended for low latency:**
```rust
let config = BatchConfig {
    max_batch_size: 20,
    max_batch_wait_ms: 10,
    max_processing_threads: 8,
    enable_smart_batching: true,
    ..Default::default()
};
```

### Memory Optimization

**Key factors:**
- Memory flush threshold
- Batch size limits
- Compression settings

**Recommended for low memory:**
```rust
let config = BatchConfig {
    max_batch_size: 50,
    memory_flush_threshold: 16 * 1024 * 1024,  // 16MB
    enable_batch_compression: true,
    compression_threshold: 512,  // 512 bytes
    ..Default::default()
};
```

## Batch Control

### BatchControl

Control commands for runtime management:

```rust
pub enum BatchControl {
    /// Force immediate processing
    FlushNow,
    
    /// Pause processing
    Pause,
    
    /// Resume processing
    Resume,
    
    /// Stop processor
    Stop,
    
    /// Update configuration
    UpdateConfig(BatchConfig),
}
```

**Usage:**
```rust
// Send control command
processor.control_sender.send(BatchControl::FlushNow).await?;
```

## Operation Options

### AddOptions

```rust
pub struct AddOptions {
    /// Automatically pin
    pub pin: bool,
    
    /// Wrap in directory
    pub wrap_with_directory: bool,
    
    /// Chunker to use
    pub chunker: Option<String>,
}
```

### GetOptions

```rust
pub struct GetOptions {
    /// Operation timeout
    pub timeout: Option<Duration>,
    
    /// Preferred peers to query
    pub preferred_peers: Vec<String>,
}
```

### PinOptions

```rust
pub struct PinOptions {
    /// Recursive pin
    pub recursive: bool,
    
    /// Progress callback enabled
    pub progress: bool,
}
```

## Best Practices

### 1. Use Smart Batching

```rust
// Enable smart batching for better optimization
let config = BatchConfig {
    enable_smart_batching: true,
    ..Default::default()
};
```

### 2. Set Appropriate Priorities

```rust
// Critical: User-facing operations
processor.add_operation(op_type, data, 9).await?;

// Normal: Background operations
processor.add_operation(op_type, data, 5).await?;

// Low: Maintenance operations
processor.add_operation(op_type, data, 2).await?;
```

### 3. Monitor Statistics

```rust
// Periodically check performance
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    loop {
        interval.tick().await;
        let stats = processor.get_stats().await;
        
        if stats.batch_efficiency < 0.5 {
            warn!("Low batch efficiency: {:.1}%", stats.batch_efficiency * 100.0);
        }
    }
});
```

### 4. Tune for Workload

```rust
// For write-heavy workload (many Adds)
let config = BatchConfig {
    max_batch_size: 150,
    max_batch_wait_ms: 75,
    ..Default::default()
};

// For read-heavy workload (many Gets)
let config = BatchConfig {
    max_batch_size: 100,
    max_batch_wait_ms: 25,  // Process Gets faster
    ..Default::default()
};
```

## Performance Metrics

### Throughput Calculation

```rust
operations_per_second = total_operations / elapsed_time_seconds
```

### Efficiency Calculation

```rust
batch_efficiency = batched_operations / total_operations
```

**Interpretation:**
- **>80%**: Excellent batching efficiency
- **60-80%**: Good batching efficiency
- **40-60%**: Moderate batching efficiency
- **<40%**: Poor batching efficiency (tune config)

### Average Batch Size

```rust
avg_batch_size = sum(batch_sizes) / number_of_batches
```

**Interpretation:**
- Higher = better resource utilization
- Lower = better latency
- Optimal depends on workload

## Troubleshooting

### Low Batch Efficiency

**Problem:** `batch_efficiency < 0.5`

```
Solutions:
1. Increase max_batch_wait_ms
2. Increase max_batch_size
3. Enable smart batching
4. Check operation patterns
```

### High Latency

**Problem:** Operations take too long

```
Solutions:
1. Decrease max_batch_wait_ms
2. Decrease max_batch_size
3. Increase max_processing_threads
4. Use higher priorities for critical ops
```

### Queue Buildup

**Problem:** Queues growing indefinitely

```
Solutions:
1. Increase max_processing_threads
2. Decrease max_batch_size (process more frequently)
3. Check for processing errors
4. Monitor semaphore contention
```

### Memory Growth

**Problem:** High memory usage

```
Solutions:
1. Decrease memory_flush_threshold
2. Decrease max_batch_size
3. Enable compression
4. Process batches more frequently
```

## Testing

### Unit Testing

```rust
#[tokio::test]
async fn test_batch_processor() {
    let processor = BatchProcessor::new(BatchConfig::default());
    let _handle = processor.start_auto_processor();
    
    // Add operation
    let data = Bytes::from("test data");
    let result = processor.add_operation(
        OperationType::Add,
        OperationData::AddData {
            data,
            options: AddOptions::default(),
        },
        5,
    ).await.unwrap();
    
    assert!(matches!(result, OperationResult::AddResult(_)));
}
```

### Performance Testing

```rust
#[tokio::test]
async fn test_batch_efficiency() {
    let processor = BatchProcessor::new(BatchConfig::default());
    let _handle = processor.start_auto_processor();
    
    // Submit 100 operations
    for i in 0..100 {
        let data = Bytes::from(format!("Data {}", i));
        processor.add_operation(
            OperationType::Add,
            OperationData::AddData {
                data,
                options: AddOptions::default(),
            },
            5,
        ).await.unwrap();
    }
    
    // Wait for processing
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // Check statistics
    let stats = processor.get_stats().await;
    assert!(stats.batch_efficiency > 0.7);  // >70% efficiency
}
```

## Limitations

### Current Limitations

1. **Simulated Operations**: Currently uses simulated I/O (not production-ready)
2. **No Persistence**: Operation queue is in-memory only
3. **No Retry Logic**: Failed operations are not automatically retried
4. **Limited Compression**: Compression not yet implemented
5. **No Operation Cancellation**: Cannot cancel queued operations

### Future Improvements

- Real backend integration (replace simulation)
- Persistent operation queue
- Automatic retry with exponential backoff
- Actual compression implementation
- Operation cancellation support
- Advanced pattern detection
- Adaptive threshold tuning

## Changelog

See the main Guardian DB changelog for version history.

## License

This module is part of GuardianDB and follows the same licensing terms.

## Contributing

Contributions are welcome! Please follow the project's contribution guidelines.

## References

- [Iroh Backend Documentation](IROH-BACKEND.md)
- [Tokio Async Runtime](https://docs.rs/tokio/latest/tokio/)
- [Batch Processing Patterns](https://en.wikipedia.org/wiki/Batch_processing)
