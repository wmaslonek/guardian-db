// Sistema avançado de métricas de networking
//
// Fornece visibilidade completa sobre performance de rede,
// Gossipsub, DHT e operações IPFS para otimizações futuras

use crate::error::{GuardianError, Result};
use crate::p2p::manager::SwarmManager;
use libp2p::{PeerId, gossipsub::TopicHash};
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

/// Métricas avançadas de networking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkingMetrics {
    /// Métricas de conectividade P2P
    pub connectivity: ConnectivityMetrics,
    /// Métricas do Gossipsub
    pub gossipsub: GossipsubMetrics,
    /// Métricas do DHT
    pub dht: DhtMetrics,
    /// Métricas de performance IPFS
    pub ipfs: IpfsMetrics,
    /// Timestamp da última atualização
    pub last_updated: u64,
}

/// Métricas de conectividade P2P
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectivityMetrics {
    /// Peers conectados atualmente
    pub connected_peers: u32,
    /// Total de conexões estabelecidas (histórico)
    pub total_connections: u64,
    /// Total de desconexões
    pub total_disconnections: u64,
    /// Conexões falharam
    pub failed_connections: u64,
    /// Latência média para peers conectados (ms)
    pub avg_peer_latency_ms: f64,
    /// Bandwidth de upload (bytes/sec)
    pub upload_bandwidth_bps: u64,
    /// Bandwidth de download (bytes/sec)  
    pub download_bandwidth_bps: u64,
    /// Distribuição geográfica de peers (país -> count)
    pub peer_distribution: HashMap<String, u32>,
}

/// Métricas específicas do Gossipsub
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipsubMetrics {
    /// Tópicos ativos
    pub active_topics: u32,
    /// Total de mensagens enviadas
    pub messages_sent: u64,
    /// Total de mensagens recebidas
    pub messages_received: u64,
    /// Mensagens duplicadas recebidas
    pub duplicate_messages: u64,
    /// Mensagens inválidas
    pub invalid_messages: u64,
    /// Latência média de propagação de mensagens (ms)
    pub avg_propagation_latency_ms: f64,
    /// Taxa de entrega de mensagens (%)
    pub message_delivery_rate: f64,
    /// Peers por tópico
    pub peers_per_topic: HashMap<String, u32>,
    /// Throughput de mensagens (mensagens/sec)
    pub message_throughput: f64,
}

/// Métricas do DHT (Distributed Hash Table)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DhtMetrics {
    /// Peers conhecidos na DHT
    pub known_peers: u32,
    /// Queries DHT executadas
    pub queries_executed: u64,
    /// Queries bem-sucedidas
    pub successful_queries: u64,
    /// Tempo médio de query (ms)
    pub avg_query_time_ms: f64,
    /// Cache hits DHT
    pub cache_hits: u64,
    /// Cache misses DHT
    pub cache_misses: u64,
    /// Records armazenados localmente
    pub local_records: u32,
}

/// Métricas de performance IPFS
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpfsMetrics {
    /// Operações add executadas
    pub add_operations: u64,
    /// Operações cat executadas
    pub cat_operations: u64,
    /// Tempo médio de add (ms)
    pub avg_add_time_ms: f64,
    /// Tempo médio de cat (ms)
    pub avg_cat_time_ms: f64,
    /// Throughput de dados (bytes/sec)
    pub data_throughput_bps: u64,
    /// Tamanho médio de objetos
    pub avg_object_size_bytes: u64,
    /// Cache hit rate (%)
    pub cache_hit_rate: f64,
}

/// Coletor de métricas em tempo real
pub struct NetworkingMetricsCollector {
    /// Métricas atuais
    metrics: Arc<RwLock<NetworkingMetrics>>,
    /// Contadores atômicos para performance
    counters: MetricsCounters,
    /// Histórico de latências para cálculo de médias
    latency_samples: Arc<RwLock<LatencySamples>>,
    /// Timestamp de início
    start_time: Instant,
}

/// Contadores atômicos para operações frequentes
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

/// Amostras de latência para cálculo de médias
#[derive(Debug, Default)]
struct LatencySamples {
    peer_latencies: Vec<f64>,
    message_propagation: Vec<f64>,
    dht_query_times: Vec<f64>,
    add_operation_times: Vec<f64>,
    cat_operation_times: Vec<f64>,
}

impl Default for NetworkingMetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkingMetricsCollector {
    /// Cria um novo coletor de métricas
    pub fn new() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            metrics: Arc::new(RwLock::new(NetworkingMetrics {
                connectivity: ConnectivityMetrics::default(),
                gossipsub: GossipsubMetrics::default(),
                dht: DhtMetrics::default(),
                ipfs: IpfsMetrics::default(),
                last_updated: now,
            })),
            counters: MetricsCounters::new(),
            latency_samples: Arc::new(RwLock::new(LatencySamples::default())),
            start_time: Instant::now(),
        }
    }

    /// Registra conexão de peer
    pub async fn record_peer_connected(&self, peer_id: PeerId, latency_ms: Option<f64>) {
        self.counters
            .connections_total
            .fetch_add(1, Ordering::Relaxed);

        if let Some(latency) = latency_ms {
            let mut samples = self.latency_samples.write().await;
            samples.peer_latencies.push(latency);
            // Manter apenas últimas 100 amostras
            if samples.peer_latencies.len() > 100 {
                samples.peer_latencies.remove(0);
            }
        }

        debug!("Peer conectado: {} (latência: {:?}ms)", peer_id, latency_ms);
    }

    /// Registra desconexão de peer
    pub async fn record_peer_disconnected(&self, peer_id: PeerId) {
        self.counters
            .disconnections_total
            .fetch_add(1, Ordering::Relaxed);
        debug!("Peer desconectado: {}", peer_id);
    }

    /// Registra mensagem Gossipsub enviada
    pub async fn record_message_sent(&self, topic: &TopicHash, size_bytes: usize) {
        self.counters.messages_sent.fetch_add(1, Ordering::Relaxed);
        debug!(
            "Mensagem enviada no tópico {:?}: {} bytes",
            topic, size_bytes
        );
    }

    /// Registra mensagem Gossipsub recebida
    pub async fn record_message_received(
        &self,
        topic: &TopicHash,
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
            "Mensagem recebida no tópico {:?}: {} bytes (latência: {:?}ms)",
            topic, size_bytes, propagation_latency_ms
        );
    }

    /// Registra operação IPFS add
    pub async fn record_add_operation(&self, duration_ms: f64, size_bytes: u64) {
        self.counters.add_operations.fetch_add(1, Ordering::Relaxed);

        let mut samples = self.latency_samples.write().await;
        samples.add_operation_times.push(duration_ms);
        if samples.add_operation_times.len() > 100 {
            samples.add_operation_times.remove(0);
        }

        debug!("Operação add: {}ms, {} bytes", duration_ms, size_bytes);
    }

    /// Registra operação IPFS cat
    pub async fn record_cat_operation(&self, duration_ms: f64, size_bytes: u64) {
        self.counters.cat_operations.fetch_add(1, Ordering::Relaxed);

        let mut samples = self.latency_samples.write().await;
        samples.cat_operation_times.push(duration_ms);
        if samples.cat_operation_times.len() > 100 {
            samples.cat_operation_times.remove(0);
        }

        debug!("Operação cat: {}ms, {} bytes", duration_ms, size_bytes);
    }

    /// Registra query DHT
    pub async fn record_dht_query(&self, duration_ms: f64, successful: bool) {
        self.counters.dht_queries.fetch_add(1, Ordering::Relaxed);

        if successful {
            self.counters
                .successful_dht_queries
                .fetch_add(1, Ordering::Relaxed);
        }

        let mut samples = self.latency_samples.write().await;
        samples.dht_query_times.push(duration_ms);
        if samples.dht_query_times.len() > 100 {
            samples.dht_query_times.remove(0);
        }

        debug!("Query DHT: {}ms, sucesso: {}", duration_ms, successful);
    }

    /// Atualiza métricas calculadas
    pub async fn update_computed_metrics(&self, _swarm_manager: Option<&SwarmManager>) {
        let mut metrics = self.metrics.write().await;
        let samples = self.latency_samples.read().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Atualizar métricas de conectividade
        metrics.connectivity.total_connections =
            self.counters.connections_total.load(Ordering::Relaxed);
        metrics.connectivity.total_disconnections =
            self.counters.disconnections_total.load(Ordering::Relaxed);
        metrics.connectivity.avg_peer_latency_ms = calculate_average(&samples.peer_latencies);

        // Atualizar métricas Gossipsub
        metrics.gossipsub.messages_sent = self.counters.messages_sent.load(Ordering::Relaxed);
        metrics.gossipsub.messages_received =
            self.counters.messages_received.load(Ordering::Relaxed);
        metrics.gossipsub.avg_propagation_latency_ms =
            calculate_average(&samples.message_propagation);

        // Calcular throughput de mensagens (últimos 60 segundos)
        let runtime_secs = self.start_time.elapsed().as_secs().max(1);
        metrics.gossipsub.message_throughput =
            metrics.gossipsub.messages_received as f64 / runtime_secs as f64;

        // Atualizar métricas DHT
        metrics.dht.queries_executed = self.counters.dht_queries.load(Ordering::Relaxed);
        metrics.dht.successful_queries =
            self.counters.successful_dht_queries.load(Ordering::Relaxed);
        metrics.dht.avg_query_time_ms = calculate_average(&samples.dht_query_times);

        // Atualizar métricas IPFS
        metrics.ipfs.add_operations = self.counters.add_operations.load(Ordering::Relaxed);
        metrics.ipfs.cat_operations = self.counters.cat_operations.load(Ordering::Relaxed);
        metrics.ipfs.avg_add_time_ms = calculate_average(&samples.add_operation_times);
        metrics.ipfs.avg_cat_time_ms = calculate_average(&samples.cat_operation_times);

        metrics.last_updated = now;

        info!(
            "Métricas atualizadas - Msgs: {}/{}, Conexões: {}, Queries DHT: {}/{}",
            metrics.gossipsub.messages_sent,
            metrics.gossipsub.messages_received,
            metrics.connectivity.total_connections,
            metrics.dht.successful_queries,
            metrics.dht.queries_executed
        );
    }

    /// Obtém snapshot das métricas atuais
    pub async fn get_metrics(&self) -> NetworkingMetrics {
        let metrics = self.metrics.read().await;
        metrics.clone()
    }

    /// Gera relatório detalhado das métricas
    pub async fn generate_report(&self) -> String {
        let metrics = self.get_metrics().await;

        format!(
            r#"
RELATÓRIO DE MÉTRICAS DE NETWORKING
==================================================

CONECTIVIDADE P2P:
   • Peers conectados: {}
   • Total conexões: {}  
   • Desconexões: {}
   • Latência média: {:.2}ms
   • Upload: {} bytes/s
   • Download: {} bytes/s

GOSSIPSUB:
   • Tópicos ativos: {}
   • Mensagens enviadas: {}
   • Mensagens recebidas: {}
   • Latência propagação: {:.2}ms
   • Throughput: {:.2} msgs/s
   • Taxa entrega: {:.1}%

DHT:
   • Peers conhecidos: {}
   • Queries executadas: {}
   • Queries bem-sucedidas: {}
   • Tempo médio query: {:.2}ms
   • Taxa sucesso: {:.1}%

IPFS:
   • Operações add: {}
   • Operações cat: {}
   • Tempo médio add: {:.2}ms
   • Tempo médio cat: {:.2}ms
   • Throughput dados: {} bytes/s

Última atualização: {}
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
            metrics.dht.known_peers,
            metrics.dht.queries_executed,
            metrics.dht.successful_queries,
            metrics.dht.avg_query_time_ms,
            if metrics.dht.queries_executed > 0 {
                metrics.dht.successful_queries as f64 / metrics.dht.queries_executed as f64 * 100.0
            } else {
                0.0
            },
            metrics.ipfs.add_operations,
            metrics.ipfs.cat_operations,
            metrics.ipfs.avg_add_time_ms,
            metrics.ipfs.avg_cat_time_ms,
            metrics.ipfs.data_throughput_bps,
            metrics.last_updated
        )
    }

    /// Exporta métricas como JSON para ferramentas externas
    pub async fn export_json(&self) -> Result<String> {
        let metrics = self.get_metrics().await;
        serde_json::to_string_pretty(&metrics)
            .map_err(|e| GuardianError::Other(format!("Erro ao serializar métricas: {}", e)))
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
            dht_queries: AtomicU64::new(0),
            successful_dht_queries: AtomicU64::new(0),
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

impl Default for DhtMetrics {
    fn default() -> Self {
        Self {
            known_peers: 0,
            queries_executed: 0,
            successful_queries: 0,
            avg_query_time_ms: 0.0,
            cache_hits: 0,
            cache_misses: 0,
            local_records: 0,
        }
    }
}

impl Default for IpfsMetrics {
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

/// Calcula média de uma lista de valores
fn calculate_average(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}
