    // ╔════════════════════════════════════════════════════════════════════════════════╗
    // ║                          ⚠ OUTDATED DOCUMENTATION                             ║
    // ╚════════════════════════════════════════════════════════════════════════════════╝


# Networking Metrics - Advanced Network Performance Monitoring Documentation

## Overview

The Networking Metrics system provides comprehensive real-time visibility into network performance for Guardian DB. It tracks P2P connectivity, Gossipsub messaging, DHT operations, and IPFS performance using atomic counters and latency sampling for minimal overhead and accurate measurements.

## Table of Contents

- [Architecture](#architecture)
- [Core Components](#core-components)
- [Metric Categories](#metric-categories)
- [API Reference](#api-reference)
- [Recording Operations](#recording-operations)
- [Statistics Calculation](#statistics-calculation)
- [Reporting](#reporting)
- [Usage Examples](#usage-examples)
- [Performance Considerations](#performance-considerations)
- [Best Practices](#best-practices)

## Architecture

### High-Level Architecture

```
┌──────────────────────────────────────────────────────────┐
│          NetworkingMetricsCollector                      │
├──────────────────────────────────────────────────────────┤
│  ┌────────────────────────────────────────────────┐     │
│  │    Atomic Counters (Lock-Free)                 │     │
│  │  - messages_sent                               │     │
│  │  - messages_received                           │     │
│  │  - connections_total                           │     │
│  │  - disconnections_total                        │     │
│  │  - add_operations                              │     │
│  │  - cat_operations                              │     │
│  │  - dht_queries                                 │     │
│  │  - successful_dht_queries                      │     │
│  └────────────────────────────────────────────────┘     │
│  ┌────────────────────────────────────────────────┐     │
│  │    Latency Samples (Rolling Window)            │     │
│  │  - peer_latencies (last 100)                   │     │
│  │  - message_propagation (last 100)              │     │
│  │  - dht_query_times (last 100)                  │     │
│  │  - add_operation_times (last 100)              │     │
│  │  - cat_operation_times (last 100)              │     │
│  └────────────────────────────────────────────────┘     │
│  ┌────────────────────────────────────────────────┐     │
│  │    Computed Metrics (Aggregated)               │     │
│  │  - ConnectivityMetrics                         │     │
│  │  - GossipsubMetrics                            │     │
│  │  - DhtMetrics                                  │     │
│  │  - IpfsMetrics                                 │     │
│  └────────────────────────────────────────────────┘     │
└──────────────────────────────────────────────────────────┘
```

### Data Flow

```
Operation Occurs
    ↓
record_*() Method
    ↓
[Atomic Counter Increment]
[Latency Sample Added]
    ↓
update_computed_metrics()
    ↓
[Calculate Averages]
[Update Derived Metrics]
    ↓
get_metrics() / generate_report()
```

## Core Components

### 1. NetworkingMetricsCollector

Main metrics collector:

```rust
pub struct NetworkingMetricsCollector {
    /// Current metrics (computed)
    metrics: Arc<RwLock<NetworkingMetrics>>,
    
    /// Atomic counters (lock-free)
    counters: MetricsCounters,
    
    /// Latency samples (rolling window)
    latency_samples: Arc<RwLock<LatencySamples>>,
    
    /// Start timestamp (for throughput calculation)
    start_time: Instant,
}
```

### 2. NetworkingMetrics

Aggregated metrics structure:

```rust
pub struct NetworkingMetrics {
    /// P2P connectivity metrics
    pub connectivity: ConnectivityMetrics,
    
    /// Gossipsub metrics
    pub gossipsub: GossipsubMetrics,
    
    /// DHT metrics
    pub dht: DhtMetrics,
    
    /// IPFS performance metrics
    pub ipfs: IpfsMetrics,
    
    /// Last update timestamp
    pub last_updated: u64,
}
```

### 3. MetricsCounters

Atomic counters for lock-free updates:

```rust
struct MetricsCounters {
    messages_sent: AtomicU64,
    messages_received: AtomicU64,
    connections_total: AtomicU64,
    disconnections_total: AtomicU64,
    add_operations: AtomicU64,
    cat_operations: AtomicU64,
    dht_queries: AtomicU64,
    successful_dht_queries: AtomicU64,
}
```

**Benefits:**
- Lock-free increments
- No contention
- Minimal overhead
- Thread-safe

### 4. LatencySamples

Rolling window for latency measurements:

```rust
struct LatencySamples {
    peer_latencies: Vec<f64>,           // Last 100 samples
    message_propagation: Vec<f64>,       // Last 100 samples
    dht_query_times: Vec<f64>,          // Last 100 samples
    add_operation_times: Vec<f64>,      // Last 100 samples
    cat_operation_times: Vec<f64>,      // Last 100 samples
}
```

**Features:**
- Fixed-size rolling window (100 samples)
- Automatic oldest-sample removal
- Efficient average calculation
- Memory-bounded

## Metric Categories

### 1. ConnectivityMetrics

P2P connectivity metrics:

```rust
pub struct ConnectivityMetrics {
    /// Currently connected peers
    pub connected_peers: u32,
    
    /// Total connections (historical)
    pub total_connections: u64,
    
    /// Total disconnections
    pub total_disconnections: u64,
    
    /// Failed connections
    pub failed_connections: u64,
    
    /// Average peer latency (ms)
    pub avg_peer_latency_ms: f64,
    
    /// Upload bandwidth (bytes/s)
    pub upload_bandwidth_bps: u64,
    
    /// Download bandwidth (bytes/s)
    pub download_bandwidth_bps: u64,
    
    /// Peer distribution (country -> count)
    pub peer_distribution: HashMap<String, u32>,
}
```

### 2. GossipsubMetrics

Gossipsub protocol metrics:

```rust
pub struct GossipsubMetrics {
    /// Active topics count
    pub active_topics: u32,
    
    /// Total messages sent
    pub messages_sent: u64,
    
    /// Total messages received
    pub messages_received: u64,
    
    /// Duplicate messages received
    pub duplicate_messages: u64,
    
    /// Invalid messages
    pub invalid_messages: u64,
    
    /// Average message propagation latency (ms)
    pub avg_propagation_latency_ms: f64,
    
    /// Message delivery rate (%)
    pub message_delivery_rate: f64,
    
    /// Peers per topic
    pub peers_per_topic: HashMap<String, u32>,
    
    /// Message throughput (messages/s)
    pub message_throughput: f64,
}
```

### 3. DhtMetrics

DHT operation metrics:

```rust
pub struct DhtMetrics {
    /// Known peers in DHT
    pub known_peers: u32,
    
    /// DHT queries executed
    pub queries_executed: u64,
    
    /// Successful queries
    pub successful_queries: u64,
    
    /// Average query time (ms)
    pub avg_query_time_ms: f64,
    
    /// DHT cache hits
    pub cache_hits: u64,
    
    /// DHT cache misses
    pub cache_misses: u64,
    
    /// Locally stored records
    pub local_records: u32,
}
```

### 4. IpfsMetrics

IPFS operation performance metrics:

```rust
pub struct IpfsMetrics {
    /// Add operations executed
    pub add_operations: u64,
    
    /// Cat operations executed
    pub cat_operations: u64,
    
    /// Average add time (ms)
    pub avg_add_time_ms: f64,
    
    /// Average cat time (ms)
    pub avg_cat_time_ms: f64,
    
    /// Data throughput (bytes/s)
    pub data_throughput_bps: u64,
    
    /// Average object size (bytes)
    pub avg_object_size_bytes: u64,
    
    /// Cache hit rate (%)
    pub cache_hit_rate: f64,
}
```

## API Reference

### Creating a Collector

```rust
pub fn new() -> Self
```

**Returns:**
- `NetworkingMetricsCollector` instance

**Example:**
```rust
let collector = NetworkingMetricsCollector::new();
```

### Recording Operations

#### Record Peer Connected

```rust
pub async fn record_peer_connected(&self, node_id: PeerId, latency_ms: Option<f64>)
```

**Parameters:**
- `node_id`: Connected peer ID
- `latency_ms`: Optional connection latency

**Example:**
```rust
collector.record_peer_connected(node_id, Some(45.5)).await;
```

#### Record Peer Disconnected

```rust
pub async fn record_peer_disconnected(&self, node_id: PeerId)
```

**Example:**
```rust
collector.record_peer_disconnected(node_id).await;
```

#### Record Gossipsub Message Sent

```rust
pub async fn record_message_sent(&self, topic: &TopicHash, size_bytes: usize)
```

**Parameters:**
- `topic`: Topic hash
- `size_bytes`: Message size

**Example:**
```rust
let topic = TopicHash::from_raw("announcements");
collector.record_message_sent(&topic, 1024).await;
```

#### Record Gossipsub Message Received

```rust
pub async fn record_message_received(
    &self,
    topic: &TopicHash,
    size_bytes: usize,
    propagation_latency_ms: Option<f64>,
)
```

**Parameters:**
- `topic`: Topic hash
- `size_bytes`: Message size
- `propagation_latency_ms`: Optional propagation latency

**Example:**
```rust
collector.record_message_received(&topic, 2048, Some(125.3)).await;
```

#### Record IPFS Add Operation

```rust
pub async fn record_add_operation(&self, duration_ms: f64, size_bytes: u64)
```

**Parameters:**
- `duration_ms`: Operation duration
- `size_bytes`: Data size

**Example:**
```rust
let start = Instant::now();
// ... perform add operation ...
let duration = start.elapsed().as_millis() as f64;
collector.record_add_operation(duration, 4096).await;
```

#### Record IPFS Cat Operation

```rust
pub async fn record_cat_operation(&self, duration_ms: f64, size_bytes: u64)
```

**Example:**
```rust
let start = Instant::now();
// ... perform cat operation ...
let duration = start.elapsed().as_millis() as f64;
collector.record_cat_operation(duration, 8192).await;
```

#### Record DHT Query

```rust
pub async fn record_dht_query(&self, duration_ms: f64, successful: bool)
```

**Parameters:**
- `duration_ms`: Query duration
- `successful`: Whether query succeeded

**Example:**
```rust
let start = Instant::now();
let result = dht_query().await;
let duration = start.elapsed().as_millis() as f64;
collector.record_dht_query(duration, result.is_ok()).await;
```

### Getting Metrics

#### Get Current Metrics

```rust
pub async fn get_metrics(&self) -> NetworkingMetrics
```

**Returns:**
- Snapshot of current metrics

**Example:**
```rust
let metrics = collector.get_metrics().await;
println!("Connected peers: {}", metrics.connectivity.connected_peers);
```

#### Update Computed Metrics

```rust
pub async fn update_computed_metrics(&self, swarm_manager: Option<&SwarmManager>)
```

**Description:** Recalculates all derived metrics from counters and samples.

**Example:**
```rust
// Periodically update
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    loop {
        interval.tick().await;
        collector.update_computed_metrics(None).await;
    }
});
```

### Reporting

#### Generate Text Report

```rust
pub async fn generate_report(&self) -> String
```

**Returns:**
- Formatted text report

**Example:**
```rust
let report = collector.generate_report().await;
println!("{}", report);
```

**Output:**
```
RELATÓRIO DE MÉTRICAS DE NETWORKING
==================================================

CONECTIVIDADE P2P:
   • Peers conectados: 12
   • Total conexões: 45
   • Desconexões: 3
   • Latência média: 67.35ms
   • Upload: 1024000 bytes/s
   • Download: 2048000 bytes/s

GOSSIPSUB:
   • Tópicos ativos: 5
   • Mensagens enviadas: 234
   • Mensagens recebidas: 189
   • Latência propagação: 124.56ms
   • Throughput: 3.15 msgs/s
   • Taxa entrega: 98.5%

DHT:
   • Peers conhecidos: 89
   • Queries executadas: 67
   • Queries bem-sucedidas: 62
   • Tempo médio query: 234.12ms
   • Taxa sucesso: 92.5%

IPFS:
   • Operações add: 45
   • Operações cat: 123
   • Tempo médio add: 56.78ms
   • Tempo médio cat: 23.45ms
   • Throughput dados: 512000 bytes/s
```

#### Export as JSON

```rust
pub async fn export_json(&self) -> Result<String>
```

**Returns:**
- JSON-formatted metrics

**Example:**
```rust
let json = collector.export_json().await?;
std::fs::write("metrics.json", json)?;

// Can be consumed by:
// - Monitoring tools (Prometheus, Grafana)
// - Log aggregators
// - Analytics platforms
```

## Recording Operations

### Connectivity Operations

```rust
// Peer connected
collector.record_peer_connected(node_id, Some(latency_ms)).await;

// Peer disconnected
collector.record_peer_disconnected(node_id).await;
```

### Gossipsub Operations

```rust
// Message sent
let topic = TopicHash::from_raw("my-topic");
collector.record_message_sent(&topic, message.len()).await;

// Message received
collector.record_message_received(
    &topic,
    message.len(),
    Some(propagation_latency_ms)
).await;
```

### DHT Operations

```rust
// DHT query
let start = Instant::now();
let result = perform_dht_query().await;
let duration = start.elapsed().as_millis() as f64;

collector.record_dht_query(duration, result.is_ok()).await;
```

### IPFS Operations

```rust
// Add operation
let start = Instant::now();
let response = backend.add(data).await?;
let duration = start.elapsed().as_millis() as f64;

collector.record_add_operation(duration, data_size).await;

// Cat operation
let start = Instant::now();
let data = backend.cat(cid).await?;
let duration = start.elapsed().as_millis() as f64;

collector.record_cat_operation(duration, data.len() as u64).await;
```

## Statistics Calculation

### Average Calculation

```rust
fn calculate_average(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}
```

**Used for:**
- Peer latency average
- Message propagation latency average
- DHT query time average
- IPFS operation time averages

### Throughput Calculation

```rust
// Message throughput
let runtime_secs = start_time.elapsed().as_secs().max(1);
message_throughput = messages_received as f64 / runtime_secs as f64;

// Data throughput
data_throughput_bps = total_bytes / runtime_secs;
```

### Success Rate Calculation

```rust
// DHT success rate
let success_rate = if queries_executed > 0 {
    (successful_queries as f64 / queries_executed as f64) * 100.0
} else {
    0.0
};
```

### Update Process

```rust
pub async fn update_computed_metrics(&self, swarm_manager: Option<&SwarmManager>) {
    let mut metrics = self.metrics.write().await;
    let samples = self.latency_samples.read().await;
    
    // Update connectivity metrics
    metrics.connectivity.total_connections = 
        self.counters.connections_total.load(Ordering::Relaxed);
    metrics.connectivity.avg_peer_latency_ms = 
        calculate_average(&samples.peer_latencies);
    
    // Update Gossipsub metrics
    metrics.gossipsub.messages_sent = 
        self.counters.messages_sent.load(Ordering::Relaxed);
    metrics.gossipsub.avg_propagation_latency_ms = 
        calculate_average(&samples.message_propagation);
    
    // Update DHT metrics
    metrics.dht.queries_executed = 
        self.counters.dht_queries.load(Ordering::Relaxed);
    metrics.dht.avg_query_time_ms = 
        calculate_average(&samples.dht_query_times);
    
    // Update IPFS metrics
    metrics.ipfs.avg_add_time_ms = 
        calculate_average(&samples.add_operation_times);
    metrics.ipfs.avg_cat_time_ms = 
        calculate_average(&samples.cat_operation_times);
    
    // Calculate throughput
    let runtime_secs = self.start_time.elapsed().as_secs().max(1);
    metrics.gossipsub.message_throughput = 
        metrics.gossipsub.messages_received as f64 / runtime_secs as f64;
    
    metrics.last_updated = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
}
```

## Usage Examples

### Basic Setup

```rust
use guardian_db::ipfs_core_api::backends::networking_metrics::*;

// Create collector
let collector = NetworkingMetricsCollector::new();

// Start periodic updates
tokio::spawn({
    let collector = collector.clone();
    async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            collector.update_computed_metrics(None).await;
        }
    }
});
```

### Integration with Backend

```rust
use std::sync::Arc;

// Create shared collector
let collector = Arc::new(NetworkingMetricsCollector::new());

// Use in IPFS operations
let collector_clone = collector.clone();
tokio::spawn(async move {
    let start = Instant::now();
    let result = backend.add(data).await;
    let duration = start.elapsed().as_millis() as f64;
    
    collector_clone.record_add_operation(duration, data_size).await;
});

// Use in P2P operations
let collector_clone = collector.clone();
tokio::spawn(async move {
    collector_clone.record_peer_connected(node_id, Some(latency)).await;
});
```

### Monitoring Dashboard

```rust
// Periodic metrics monitoring
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    
    loop {
        interval.tick().await;
        
        let metrics = collector.get_metrics().await;
        
        // Log key metrics
        info!("=== Network Metrics ===");
        info!("Peers: {}", metrics.connectivity.connected_peers);
        info!("Latency: {:.2}ms", metrics.connectivity.avg_peer_latency_ms);
        info!("Messages sent/recv: {}/{}",
            metrics.gossipsub.messages_sent,
            metrics.gossipsub.messages_received
        );
        info!("DHT success rate: {:.1}%",
            metrics.dht.successful_queries as f64 / 
            metrics.dht.queries_executed.max(1) as f64 * 100.0
        );
        
        // Check for issues
        if metrics.connectivity.avg_peer_latency_ms > 500.0 {
            warn!("High peer latency detected!");
        }
        
        if metrics.gossipsub.message_delivery_rate < 90.0 {
            warn!("Low message delivery rate!");
        }
    }
});
```

### Export for External Tools

```rust
// Export to JSON file
let json = collector.export_json().await?;
std::fs::write("metrics.json", json)?;

// Export to monitoring system
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    
    loop {
        interval.tick().await;
        
        let metrics = collector.get_metrics().await;
        
        // Send to Prometheus/Grafana
        push_to_prometheus(&metrics).await;
        
        // Or send to custom monitoring
        send_to_monitoring_system(&metrics).await;
    }
});
```

### Custom Reporting

```rust
// Generate custom report
async fn custom_report(collector: &NetworkingMetricsCollector) {
    let metrics = collector.get_metrics().await;
    
    println!("Network Health Report");
    println!("=====================");
    
    // Connectivity health
    let conn_health = if metrics.connectivity.avg_peer_latency_ms < 100.0 {
        "Excellent"
    } else if metrics.connectivity.avg_peer_latency_ms < 300.0 {
        "Good"
    } else {
        "Poor"
    };
    println!("Connectivity: {} ({:.2}ms avg latency)",
        conn_health, metrics.connectivity.avg_peer_latency_ms);
    
    // Gossipsub health
    let msg_health = if metrics.gossipsub.message_delivery_rate > 95.0 {
        "Excellent"
    } else if metrics.gossipsub.message_delivery_rate > 85.0 {
        "Good"
    } else {
        "Poor"
    };
    println!("Messaging: {} ({:.1}% delivery rate)",
        msg_health, metrics.gossipsub.message_delivery_rate);
    
    // DHT health
    let dht_success = if metrics.dht.queries_executed > 0 {
        metrics.dht.successful_queries as f64 / metrics.dht.queries_executed as f64
    } else {
        1.0
    };
    let dht_health = if dht_success > 0.9 { "Excellent" }
        else if dht_success > 0.7 { "Good" }
        else { "Poor" };
    println!("DHT: {} ({:.1}% success rate)",
        dht_health, dht_success * 100.0);
}
```

## Performance Considerations

### Lock-Free Counters

Atomic operations for high-frequency updates:

```rust
// Lock-free increment (no contention)
self.counters.messages_sent.fetch_add(1, Ordering::Relaxed);

// vs. traditional mutex (contention possible)
let mut counter = counter_mutex.lock().await;
*counter += 1;
```

**Benefits:**
- No lock contention
- O(1) operations
- No async overhead
- Scales with concurrency

### Rolling Window Sampling

Limits memory usage for latency samples:

```rust
// Add sample
samples.peer_latencies.push(latency);

// Keep only last 100
if samples.peer_latencies.len() > 100 {
    samples.peer_latencies.remove(0);
}
```

**Benefits:**
- Fixed memory usage
- Recent data prioritized
- Smooth average calculation
- No unbounded growth

### Minimal Overhead

**Metrics collection overhead:**
- Atomic increment: ~1-2ns
- Latency sample: ~100ns (with lock)
- Total per operation: <200ns

**Impact on throughput:** <0.1%

## Best Practices

### 1. Periodic Updates

```rust
// Update computed metrics regularly
let collector_clone = collector.clone();
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    loop {
        interval.tick().await;
        collector_clone.update_computed_metrics(None).await;
    }
});
```

### 2. Monitor Key Metrics

```rust
// Alert on anomalies
if metrics.connectivity.avg_peer_latency_ms > 1000.0 {
    error!("Very high peer latency: {:.2}ms", metrics.connectivity.avg_peer_latency_ms);
}

if metrics.gossipsub.message_delivery_rate < 80.0 {
    error!("Low message delivery rate: {:.1}%", metrics.gossipsub.message_delivery_rate);
}

if metrics.dht.successful_queries as f64 / metrics.dht.queries_executed as f64 < 0.5 {
    error!("DHT queries failing: {}/{}", 
        metrics.dht.successful_queries, metrics.dht.queries_executed);
}
```

### 3. Export for Analysis

```rust
// Regular exports for trending analysis
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(300));  // 5 min
    
    loop {
        interval.tick().await;
        
        let json = collector.export_json().await.unwrap();
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let filename = format!("metrics_{}.json", timestamp);
        
        std::fs::write(&filename, json).unwrap();
    }
});
```

### 4. Integrate with Application Metrics

```rust
// Combine with application-level metrics
struct ApplicationMetrics {
    networking: NetworkingMetrics,
    database: DatabaseMetrics,
    api: ApiMetrics,
}

async fn get_full_metrics(collector: &NetworkingMetricsCollector) -> ApplicationMetrics {
    ApplicationMetrics {
        networking: collector.get_metrics().await,
        database: get_database_metrics().await,
        api: get_api_metrics().await,
    }
}
```

## Metric Interpretation

### Connectivity Metrics

**avg_peer_latency_ms:**
- <100ms: Excellent
- 100-300ms: Good
- 300-500ms: Fair
- >500ms: Poor

**connected_peers:**
- Monitor for stability
- Sudden drops indicate network issues
- Gradual growth is healthy

### Gossipsub Metrics

**message_delivery_rate:**
- >95%: Excellent
- 85-95%: Good
- 70-85%: Fair
- <70%: Poor (investigate)

**message_throughput:**
- Compare to expected load
- Spikes indicate burst traffic
- Sustained high values may require scaling

### DHT Metrics

**Success rate:**
- >90%: Excellent
- 70-90%: Good
- 50-70%: Fair
- <50%: Poor (check network/peers)

**avg_query_time_ms:**
- <200ms: Excellent
- 200-500ms: Good
- 500-1000ms: Fair
- >1000ms: Poor

### IPFS Metrics

**avg_add_time_ms:**
- <50ms: Excellent
- 50-200ms: Good
- 200-500ms: Fair
- >500ms: Poor (check I/O)

**avg_cat_time_ms:**
- <20ms: Excellent (cache hit)
- 20-100ms: Good
- 100-300ms: Fair
- >300ms: Poor (check network)

## Troubleshooting

### High Peer Latency

**Problem:** `avg_peer_latency_ms > 500ms`

```
Solutions:
1. Check network connectivity
2. Connect to geographically closer peers
3. Review peer quality
4. Check local network issues
5. Consider peer rotation
```

### Low Message Delivery Rate

**Problem:** `message_delivery_rate < 85%`

```
Solutions:
1. Check peer count on topics
2. Verify Gossipsub mesh health
3. Review network stability
4. Check for message size issues
5. Increase mesh parameters
```

### DHT Query Failures

**Problem:** Low DHT success rate

```
Solutions:
1. Check peer connectivity
2. Verify DHT is bootstrapped
3. Review bootstrap nodes
4. Check for network partitioning
5. Increase query timeout
```

### Slow IPFS Operations

**Problem:** High `avg_add_time_ms` or `avg_cat_time_ms`

```
Solutions:
1. Check disk I/O performance
2. Review cache configuration
3. Check network for cat operations
4. Consider larger buffer sizes
5. Review concurrent operation limits
```

## Changelog

See the main Guardian DB changelog for version history.

## License

This module is part of GuardianDB and follows the same licensing terms.

## Contributing

Contributions are welcome! Please follow the project's contribution guidelines.

## References

- [Iroh Backend Documentation](IROH-BACKEND.md)
- [Prometheus Metrics Best Practices](https://prometheus.io/docs/practices/naming/)
- [libp2p Metrics](https://docs.libp2p.io/concepts/observability/)
