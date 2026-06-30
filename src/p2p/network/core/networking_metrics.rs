// Advanced networking metrics system.
//
// Provides complete visibility into network performance, Gossipsub, Discovery
// and Iroh operations for future optimizations.

use crate::guardian::error::{GuardianError, Result};
use iroh::EndpointId as NodeId;
use iroh_gossip::proto::TopicId;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::RwLock;
use tracing::{debug, info};

/// Advanced networking metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkingMetrics {
    /// P2P connectivity metrics.
    pub connectivity: ConnectivityMetrics,
    /// Gossipsub metrics.
    pub gossipsub: GossipsubMetrics,
    /// Discovery metrics (DNS/Pkarr/mDNS).
    pub discovery: DiscoveryMetrics,
    /// Iroh performance metrics.
    pub backend_metrics: IrohMetrics,
    /// Timestamp of the last update.
    pub last_updated: u64,
}

/// P2P connectivity metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectivityMetrics {
    /// Currently connected peers.
    pub connected_peers: u32,
    /// Total connections established (historical).
    pub total_connections: u64,
    /// Total disconnections.
    pub total_disconnections: u64,
    /// Failed connections.
    pub failed_connections: u64,
    /// Average latency to connected peers (ms).
    pub avg_peer_latency_ms: f64,
    /// Upload bandwidth (bytes/sec).
    pub upload_bandwidth_bps: u64,
    /// Download bandwidth (bytes/sec).
    pub download_bandwidth_bps: u64,
    /// Geographic distribution of peers (country -> count).
    pub peer_distribution: HashMap<String, u32>,
}

/// Gossipsub-specific metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipsubMetrics {
    /// Active topics.
    pub active_topics: u32,
    /// Total messages sent.
    pub messages_sent: u64,
    /// Total messages received.
    pub messages_received: u64,
    /// Duplicate messages received.
    pub duplicate_messages: u64,
    /// Invalid messages.
    pub invalid_messages: u64,
    /// Average message propagation latency (ms).
    pub avg_propagation_latency_ms: f64,
    /// Message delivery rate (%).
    pub message_delivery_rate: f64,
    /// Peers per topic.
    pub peers_per_topic: HashMap<String, u32>,
    /// Message throughput (messages/sec).
    pub message_throughput: f64,
}

/// Discovery metrics (DNS, Pkarr, mDNS).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryMetrics {
    /// Discovered peers.
    pub discovered_peers: u32,
    /// Discovery attempts performed.
    pub discovery_attempts: u64,
    /// Successful discoveries.
    pub successful_discoveries: u64,
    /// Average discovery time (ms).
    pub avg_discovery_time_ms: f64,
    /// Expired peers.
    pub expired_peers: u64,
    /// Discovery via DNS.
    pub dns_discoveries: u64,
    /// Discovery via mDNS (local).
    pub mdns_discoveries: u64,
}

/// Iroh performance metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrohMetrics {
    /// Add operations performed.
    pub add_operations: u64,
    /// Cat operations performed.
    pub cat_operations: u64,
    /// Average add time (ms).
    pub avg_add_time_ms: f64,
    /// Average cat time (ms).
    pub avg_cat_time_ms: f64,
    /// Data throughput (bytes/sec).
    pub data_throughput_bps: u64,
    /// Average object size.
    pub avg_object_size_bytes: u64,
    /// Cache hit rate (%).
    pub cache_hit_rate: f64,
}

/// Real-time metrics collector.
pub struct NetworkingMetricsCollector {
    /// Current metrics.
    metrics: Arc<RwLock<NetworkingMetrics>>,
    /// Atomic counters for performance.
    counters: MetricsCounters,
    /// History of latencies for computing averages.
    latency_samples: Arc<RwLock<LatencySamples>>,
    /// Start timestamp.
    start_time: Instant,
}

/// Atomic counters for frequent operations.
struct MetricsCounters {
    messages_sent: AtomicU64,
    messages_received: AtomicU64,
    connections_total: AtomicU64,
    disconnections_total: AtomicU64,
    add_operations: AtomicU64,
    cat_operations: AtomicU64,
    discovery_attempts: AtomicU64,
    successful_discoveries: AtomicU64,
}

/// Latency samples for computing averages.
#[derive(Debug, Default)]
struct LatencySamples {
    peer_latencies: Vec<f64>,
    message_propagation: Vec<f64>,
    discovery_times: Vec<f64>,
    add_operation_times: Vec<f64>,
    cat_operation_times: Vec<f64>,
}

impl Default for NetworkingMetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkingMetricsCollector {
    /// Creates a new metrics collector.
    pub fn new() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            metrics: Arc::new(RwLock::new(NetworkingMetrics {
                connectivity: ConnectivityMetrics::default(),
                gossipsub: GossipsubMetrics::default(),
                discovery: DiscoveryMetrics::default(),
                backend_metrics: IrohMetrics::default(),
                last_updated: now,
            })),
            counters: MetricsCounters::new(),
            latency_samples: Arc::new(RwLock::new(LatencySamples::default())),
            start_time: Instant::now(),
        }
    }

    /// Records a peer connection.
    pub async fn record_peer_connected(&self, node_id: NodeId, latency_ms: Option<f64>) {
        self.counters
            .connections_total
            .fetch_add(1, Ordering::Relaxed);

        if let Some(latency) = latency_ms {
            let mut samples = self.latency_samples.write().await;
            samples.peer_latencies.push(latency);
            // Keep only the last 100 samples.
            if samples.peer_latencies.len() > 100 {
                samples.peer_latencies.remove(0);
            }
        }

        debug!("Peer connected: {} (latency: {:?}ms)", node_id, latency_ms);
    }

    /// Records a peer disconnection.
    pub async fn record_peer_disconnected(&self, node_id: NodeId) {
        self.counters
            .disconnections_total
            .fetch_add(1, Ordering::Relaxed);
        debug!("Peer disconnected: {}", node_id);
    }

    /// Records a Gossipsub message sent.
    pub async fn record_message_sent(&self, topic: &TopicId, size_bytes: usize) {
        self.counters.messages_sent.fetch_add(1, Ordering::Relaxed);
        debug!("Message sent on topic {:?}: {} bytes", topic, size_bytes);
    }

    /// Records a Gossipsub message received.
    pub async fn record_message_received(
        &self,
        topic: &TopicId,
        size_bytes: usize,
        propagation_latency_ms: Option<f64>,
    ) {
        self.counters
            .messages_received
            .fetch_add(1, Ordering::Relaxed);

        if let Some(latency) = propagation_latency_ms {
            let mut samples = self.latency_samples.write().await;
            samples.message_propagation.push(latency);
            if samples.message_propagation.len() > 100 {
                samples.message_propagation.remove(0);
            }
        }

        debug!(
            "Message received on topic {:?}: {} bytes (latency: {:?}ms)",
            topic, size_bytes, propagation_latency_ms
        );
    }

    /// Records an iroh add operation.
    pub async fn record_add_operation(&self, duration_ms: f64, size_bytes: u64) {
        self.counters.add_operations.fetch_add(1, Ordering::Relaxed);

        let mut samples = self.latency_samples.write().await;
        samples.add_operation_times.push(duration_ms);
        if samples.add_operation_times.len() > 100 {
            samples.add_operation_times.remove(0);
        }

        debug!("Add operation: {}ms, {} bytes", duration_ms, size_bytes);
    }

    /// Records an iroh cat operation.
    pub async fn record_cat_operation(&self, duration_ms: f64, size_bytes: u64) {
        self.counters.cat_operations.fetch_add(1, Ordering::Relaxed);

        let mut samples = self.latency_samples.write().await;
        samples.cat_operation_times.push(duration_ms);
        if samples.cat_operation_times.len() > 100 {
            samples.cat_operation_times.remove(0);
        }

        debug!("Cat operation: {}ms, {} bytes", duration_ms, size_bytes);
    }

    /// Records a discovery attempt.
    pub async fn record_discovery(&self, duration_ms: f64, successful: bool) {
        self.counters
            .discovery_attempts
            .fetch_add(1, Ordering::Relaxed);

        if successful {
            self.counters
                .successful_discoveries
                .fetch_add(1, Ordering::Relaxed);
        }

        let mut samples = self.latency_samples.write().await;
        samples.discovery_times.push(duration_ms);
        if samples.discovery_times.len() > 100 {
            samples.discovery_times.remove(0);
        }

        debug!("Discovery: {}ms, success: {}", duration_ms, successful);
    }

    /// Updates the computed metrics.
    pub async fn update_computed_metrics(&self) {
        let mut metrics = self.metrics.write().await;
        let samples = self.latency_samples.read().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Update connectivity metrics.
        metrics.connectivity.total_connections =
            self.counters.connections_total.load(Ordering::Relaxed);
        metrics.connectivity.total_disconnections =
            self.counters.disconnections_total.load(Ordering::Relaxed);
        metrics.connectivity.avg_peer_latency_ms = calculate_average(&samples.peer_latencies);

        // Update Gossipsub metrics.
        metrics.gossipsub.messages_sent = self.counters.messages_sent.load(Ordering::Relaxed);
        metrics.gossipsub.messages_received =
            self.counters.messages_received.load(Ordering::Relaxed);
        metrics.gossipsub.avg_propagation_latency_ms =
            calculate_average(&samples.message_propagation);

        // Compute message throughput (over the runtime so far).
        let runtime_secs = self.start_time.elapsed().as_secs().max(1);
        metrics.gossipsub.message_throughput =
            metrics.gossipsub.messages_received as f64 / runtime_secs as f64;

        // Update Discovery metrics.
        metrics.discovery.discovery_attempts =
            self.counters.discovery_attempts.load(Ordering::Relaxed);
        metrics.discovery.successful_discoveries =
            self.counters.successful_discoveries.load(Ordering::Relaxed);
        metrics.discovery.avg_discovery_time_ms = calculate_average(&samples.discovery_times);

        // Update iroh metrics.
        metrics.backend_metrics.add_operations =
            self.counters.add_operations.load(Ordering::Relaxed);
        metrics.backend_metrics.cat_operations =
            self.counters.cat_operations.load(Ordering::Relaxed);
        metrics.backend_metrics.avg_add_time_ms = calculate_average(&samples.add_operation_times);
        metrics.backend_metrics.avg_cat_time_ms = calculate_average(&samples.cat_operation_times);

        metrics.last_updated = now;

        info!(
            "Metrics updated - Msgs: {}/{}, Connections: {}, Discovery: {}/{}",
            metrics.gossipsub.messages_sent,
            metrics.gossipsub.messages_received,
            metrics.connectivity.total_connections,
            metrics.discovery.successful_discoveries,
            metrics.discovery.discovery_attempts
        );
    }

    /// Returns a snapshot of the current metrics.
    pub async fn get_metrics(&self) -> NetworkingMetrics {
        let metrics = self.metrics.read().await;
        metrics.clone()
    }

    /// Generates a detailed metrics report.
    pub async fn generate_report(&self) -> String {
        let metrics = self.get_metrics().await;

        format!(
            r#"
NETWORKING METRICS REPORT
==================================================

P2P CONNECTIVITY:
   • Connected peers: {}
   • Total connections: {}
   • Disconnections: {}
   • Average latency: {:.2}ms
   • Upload: {} bytes/s
   • Download: {} bytes/s

GOSSIPSUB:
   • Active topics: {}
   • Messages sent: {}
   • Messages received: {}
   • Propagation latency: {:.2}ms
   • Throughput: {:.2} msgs/s
   • Delivery rate: {:.1}%

DISCOVERY (DNS/Pkarr/mDNS):
   • Discovered peers: {}
   • Attempts: {}
   • Successful discoveries: {}
   • Average time: {:.2}ms
   • Success rate: {:.1}%

Iroh:
   • Add operations: {}
   • Cat operations: {}
   • Average add time: {:.2}ms
   • Average cat time: {:.2}ms
   • Data throughput: {} bytes/s

Last update: {}
"#,
            metrics.connectivity.connected_peers,
            metrics.connectivity.total_connections,
            metrics.connectivity.total_disconnections,
            metrics.connectivity.avg_peer_latency_ms,
            metrics.connectivity.upload_bandwidth_bps,
            metrics.connectivity.download_bandwidth_bps,
            metrics.gossipsub.active_topics,
            metrics.gossipsub.messages_sent,
            metrics.gossipsub.messages_received,
            metrics.gossipsub.avg_propagation_latency_ms,
            metrics.gossipsub.message_throughput,
            metrics.gossipsub.message_delivery_rate,
            metrics.discovery.discovered_peers,
            metrics.discovery.discovery_attempts,
            metrics.discovery.successful_discoveries,
            metrics.discovery.avg_discovery_time_ms,
            if metrics.discovery.discovery_attempts > 0 {
                metrics.discovery.successful_discoveries as f64
                    / metrics.discovery.discovery_attempts as f64
                    * 100.0
            } else {
                0.0
            },
            metrics.backend_metrics.add_operations,
            metrics.backend_metrics.cat_operations,
            metrics.backend_metrics.avg_add_time_ms,
            metrics.backend_metrics.avg_cat_time_ms,
            metrics.backend_metrics.data_throughput_bps,
            metrics.last_updated
        )
    }

    /// Exports the metrics as JSON for external tools.
    pub async fn export_json(&self) -> Result<String> {
        let metrics = self.get_metrics().await;
        serde_json::to_string_pretty(&metrics)
            .map_err(|e| GuardianError::Other(format!("Error serializing metrics: {}", e)))
    }
}

impl MetricsCounters {
    fn new() -> Self {
        Self {
            messages_sent: AtomicU64::new(0),
            messages_received: AtomicU64::new(0),
            connections_total: AtomicU64::new(0),
            disconnections_total: AtomicU64::new(0),
            add_operations: AtomicU64::new(0),
            cat_operations: AtomicU64::new(0),
            discovery_attempts: AtomicU64::new(0),
            successful_discoveries: AtomicU64::new(0),
        }
    }
}

impl Default for ConnectivityMetrics {
    fn default() -> Self {
        Self {
            connected_peers: 0,
            total_connections: 0,
            total_disconnections: 0,
            failed_connections: 0,
            avg_peer_latency_ms: 0.0,
            upload_bandwidth_bps: 0,
            download_bandwidth_bps: 0,
            peer_distribution: HashMap::new(),
        }
    }
}

impl Default for GossipsubMetrics {
    fn default() -> Self {
        Self {
            active_topics: 0,
            messages_sent: 0,
            messages_received: 0,
            duplicate_messages: 0,
            invalid_messages: 0,
            avg_propagation_latency_ms: 0.0,
            message_delivery_rate: 100.0,
            peers_per_topic: HashMap::new(),
            message_throughput: 0.0,
        }
    }
}

impl Default for DiscoveryMetrics {
    fn default() -> Self {
        Self {
            discovered_peers: 0,
            discovery_attempts: 0,
            successful_discoveries: 0,
            avg_discovery_time_ms: 0.0,
            expired_peers: 0,
            dns_discoveries: 0,
            mdns_discoveries: 0,
        }
    }
}

impl Default for IrohMetrics {
    fn default() -> Self {
        Self {
            add_operations: 0,
            cat_operations: 0,
            avg_add_time_ms: 0.0,
            avg_cat_time_ms: 0.0,
            data_throughput_bps: 0,
            avg_object_size_bytes: 0,
            cache_hit_rate: 0.0,
        }
    }
}

/// Computes the average of a list of values.
fn calculate_average(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}
