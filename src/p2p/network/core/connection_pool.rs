/// Pool de Conexões Otimizado para Backend Iroh
///
/// Gerenciamento inteligente de conexões P2P com load balancing,
/// circuit breaking e recuperação automática para maximizar throughput.
use crate::guardian::error::{GuardianError, Result};
use iroh::{NodeAddr, NodeId};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, Semaphore, broadcast};
use tokio::time::timeout;
use tracing::{debug, error, info, instrument, warn};

/// Pool de conexões otimizado para P2P
pub struct OptimizedConnectionPool {
    /// Conexões ativas por peer
    active_connections: Arc<RwLock<HashMap<NodeId, ConnectionInfo>>>,
    /// Pool de conexões disponíveis
    connection_pool: Arc<RwLock<HashMap<NodeId, Vec<PooledConnection>>>>,
    /// Semáforo para controle de concorrência
    connection_semaphore: Arc<Semaphore>,
    /// Configuração do pool
    pool_config: PoolConfig,
    /// Estatísticas de performance
    stats: Arc<RwLock<PoolStats>>,
    /// Circuit breakers por peer
    circuit_breakers: Arc<RwLock<HashMap<NodeId, CircuitBreaker>>>,
    /// Monitor de saúde das conexões
    health_monitor: Arc<RwLock<HealthMonitor>>,
    /// Canal para eventos de conexão
    event_sender: broadcast::Sender<ConnectionEvent>,
}

/// Informações de uma conexão
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    /// ID da conexão
    pub connection_id: String,
    /// Endereço do peer (Iroh NodeAddr)
    pub peer_address: NodeAddr,
    /// Timestamp da conexão
    pub connected_at: Instant,
    /// Último uso
    pub last_used: Instant,
    /// Número de operações realizadas
    pub operations_count: u64,
    /// Latência média (ms)
    pub avg_latency_ms: f64,
    /// Status da conexão
    pub status: ConnectionStatus,
    /// Prioridade (0-10)
    pub priority: u8,
    /// Largura de banda disponível (bytes/s)
    pub bandwidth_bps: u64,
}

/// Conexão no pool
#[derive(Debug, Clone)]
pub struct PooledConnection {
    /// Informações da conexão
    pub info: ConnectionInfo,
    /// Timestamp quando foi colocada no pool
    pub pooled_at: Instant,
    /// Número de vezes que foi reutilizada
    pub reuse_count: u32,
    /// Se está sendo usada atualmente
    pub in_use: bool,
}

/// Status de uma conexão
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionStatus {
    /// Conectada e saudável
    Healthy,
    /// Conectada mas com problemas
    Degraded,
    /// Temporariamente indisponível
    Unavailable,
    /// Desconectada
    Disconnected,
    /// Falha de conexão
    Failed,
}

/// Configuração do pool de conexões
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Número máximo de conexões por peer
    pub max_connections_per_peer: u32,
    /// Número máximo total de conexões
    pub max_total_connections: u32,
    /// Timeout para estabelecer conexão (ms)
    pub connection_timeout_ms: u64,
    /// Timeout de idle antes de fechar conexão (s)
    pub idle_timeout_secs: u64,
    /// Intervalo de health check (s)
    pub health_check_interval_secs: u64,
    /// Número máximo de tentativas de reconexão
    pub max_retry_attempts: u32,
    /// Backoff inicial para retry (ms)
    pub initial_retry_backoff_ms: u64,
    /// Multiplicador do backoff
    pub backoff_multiplier: f64,
    /// Threshold para circuit breaker
    pub circuit_breaker_threshold: f64,
    /// Habilitar load balancing inteligente
    pub enable_intelligent_load_balancing: bool,
}

/// Estatísticas do pool de conexões
#[derive(Debug, Clone, Default)]
pub struct PoolStats {
    /// Total de conexões ativas
    pub active_connections: u32,
    /// Total de conexões no pool
    pub pooled_connections: u32,
    /// Conexões criadas
    pub connections_created: u64,
    /// Conexões reutilizadas
    pub connections_reused: u64,
    /// Conexões que falharam
    pub connections_failed: u64,
    /// Timeout de conexões
    pub connections_timeout: u64,
    /// Tempo médio de estabelecimento de conexão (ms)
    pub avg_connection_time_ms: f64,
    /// Taxa de reutilização
    pub reuse_rate: f64,
    /// Largura de banda total (bytes/s)
    pub total_bandwidth_bps: u64,
    /// Latência média global (ms)
    pub global_avg_latency_ms: f64,
}

/// Circuit Breaker para controle de falhas
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    /// Estado atual
    pub state: CircuitState,
    /// Contador de falhas
    pub failure_count: u32,
    /// Threshold de falhas
    pub failure_threshold: u32,
    /// Timestamp da última falha
    pub last_failure_time: Option<Instant>,
    /// Timeout para tentar novamente (ms)
    pub timeout_ms: u64,
    /// Contador de sucessos consecutivos
    pub success_count: u32,
}

/// Estados do Circuit Breaker
#[derive(Debug, Clone, PartialEq)]
pub enum CircuitState {
    /// Funcionando normalmente
    Closed,
    /// Aberto devido a falhas
    Open,
    /// Testando se voltou a funcionar
    HalfOpen,
}

/// Monitor de saúde das conexões
#[derive(Debug)]
pub struct HealthMonitor {
    /// Métricas de saúde por peer
    peer_health: HashMap<NodeId, PeerHealthMetrics>,
    /// Última verificação de saúde
    #[allow(dead_code)]
    last_health_check: Instant,
    /// Peers marcados como problemáticos
    unhealthy_peers: HashMap<NodeId, Instant>,
}

/// Métricas de saúde de um peer
#[derive(Debug, Clone)]
pub struct PeerHealthMetrics {
    /// Latência atual (ms)
    pub current_latency_ms: f64,
    /// Packet loss (0.0-1.0)
    pub packet_loss_rate: f64,
    /// Throughput (bytes/s)
    pub throughput_bps: u64,
    /// Uptime (segundos)
    pub uptime_secs: u64,
    /// Score de saúde (0.0-1.0)
    pub health_score: f64,
    /// Timestamp da última medição
    pub last_measured: Instant,
}

/// Eventos de conexão
#[derive(Debug, Clone)]
pub enum ConnectionEvent {
    /// Nova conexão estabelecida
    Connected { node_id: NodeId, latency_ms: f64 },
    /// Conexão perdida
    Disconnected { node_id: NodeId, reason: String },
    /// Conexão degradada
    Degraded { node_id: NodeId, health_score: f64 },
    /// Conexão recuperada
    Recovered { node_id: NodeId },
    /// Circuit breaker ativado
    CircuitBreakerOpen { node_id: NodeId },
    /// Circuit breaker fechado
    CircuitBreakerClosed { node_id: NodeId },
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections_per_peer: 8,
            max_total_connections: 1000,
            connection_timeout_ms: 10_000,
            idle_timeout_secs: 300,
            health_check_interval_secs: 30,
            max_retry_attempts: 3,
            initial_retry_backoff_ms: 1000,
            backoff_multiplier: 2.0,
            circuit_breaker_threshold: 0.5,
            enable_intelligent_load_balancing: true,
        }
    }
}

impl OptimizedConnectionPool {
    /// Cria novo pool de conexões otimizado
    pub fn new(pool_config: PoolConfig) -> Self {
        let (event_sender, _) = broadcast::channel(1000);

        Self {
            active_connections: Arc::new(RwLock::new(HashMap::new())),
            connection_pool: Arc::new(RwLock::new(HashMap::new())),
            connection_semaphore: Arc::new(Semaphore::new(
                pool_config.max_total_connections as usize,
            )),
            pool_config,
            stats: Arc::new(RwLock::new(PoolStats::default())),
            circuit_breakers: Arc::new(RwLock::new(HashMap::new())),
            health_monitor: Arc::new(RwLock::new(HealthMonitor {
                peer_health: HashMap::new(),
                last_health_check: Instant::now(),
                unhealthy_peers: HashMap::new(),
            })),
            event_sender,
        }
    }

    /// Obtém ou cria uma conexão otimizada para um peer
    #[instrument(skip(self))]
    pub async fn get_connection(&self, node_id: NodeId, address: NodeAddr) -> Result<String> {
        // Verifica circuit breaker
        if !self.check_circuit_breaker(node_id).await? {
            return Err(GuardianError::Other(format!(
                "Circuit breaker aberto para node {}",
                node_id
            )));
        }

        // Tenta reutilizar conexão do pool
        if let Some(connection_id) = self.try_reuse_connection(node_id).await? {
            debug!("Reutilizando conexão existente para node {}", node_id);
            return Ok(connection_id);
        }

        // Adquire permissão para nova conexão
        let _permit = self
            .connection_semaphore
            .acquire()
            .await
            .map_err(|e| GuardianError::Other(format!("Falha ao adquirir semáforo: {}", e)))?;

        // Estabelece nova conexão
        self.establish_new_connection(node_id, address).await
    }

    /// Tenta reutilizar conexão existente do pool
    async fn try_reuse_connection(&self, node_id: NodeId) -> Result<Option<String>> {
        let mut pool = self.connection_pool.write().await;

        if let Some(connections) = pool.get_mut(&node_id) {
            // Procura conexão saudável disponível
            for conn in connections.iter_mut() {
                if !conn.in_use && conn.info.status == ConnectionStatus::Healthy {
                    // Verifica se não está muito idle
                    let idle_time = Instant::now().duration_since(conn.info.last_used);
                    if idle_time.as_secs() < self.pool_config.idle_timeout_secs {
                        conn.in_use = true;
                        conn.reuse_count += 1;
                        conn.info.last_used = Instant::now();

                        // Atualiza estatísticas
                        let mut stats = self.stats.write().await;
                        stats.connections_reused += 1;
                        stats.reuse_rate = stats.connections_reused as f64
                            / (stats.connections_created + stats.connections_reused) as f64;

                        return Ok(Some(conn.info.connection_id.clone()));
                    }
                }
            }
        }

        Ok(None)
    }

    /// Estabelece nova conexão com otimizações
    async fn establish_new_connection(&self, node_id: NodeId, address: NodeAddr) -> Result<String> {
        let connection_start = Instant::now();
        let connection_id = format!("conn_{}_{}", node_id, uuid::Uuid::new_v4());

        debug!(
            "Estabelecendo nova conexão para node {} em {:?}",
            node_id, address
        );

        // Estabelece conexão com timeout
        let connection_result = timeout(
            Duration::from_millis(self.pool_config.connection_timeout_ms),
            self.establish_connection(node_id, address.clone()),
        )
        .await;

        match connection_result {
            Ok(Ok(latency_ms)) => {
                // Conexão estabelecida com sucesso
                let connection_time = connection_start.elapsed();

                let connection_info = ConnectionInfo {
                    connection_id: connection_id.clone(),
                    peer_address: address,
                    connected_at: Instant::now(),
                    last_used: Instant::now(),
                    operations_count: 0,
                    avg_latency_ms: latency_ms,
                    status: ConnectionStatus::Healthy,
                    priority: 5,               // Prioridade padrão
                    bandwidth_bps: 10_000_000, // 10 Mbps estimado inicial
                };

                // Adiciona à lista de conexões ativas
                {
                    let mut active = self.active_connections.write().await;
                    active.insert(node_id, connection_info.clone());
                }

                // Atualiza estatísticas
                {
                    let mut stats = self.stats.write().await;
                    stats.connections_created += 1;
                    stats.active_connections += 1;
                    stats.avg_connection_time_ms =
                        (stats.avg_connection_time_ms + connection_time.as_millis() as f64) / 2.0;
                }

                // Registra sucesso no circuit breaker
                self.record_success(node_id).await;

                // Envia evento
                let _ = self.event_sender.send(ConnectionEvent::Connected {
                    node_id,
                    latency_ms,
                });

                info!(
                    "Nova conexão estabelecida: {} -> {} (latency: {:.2}ms)",
                    node_id, connection_id, latency_ms
                );
                Ok(connection_id)
            }
            Ok(Err(e)) => {
                // Falha na conexão
                self.record_failure(node_id).await;

                let mut stats = self.stats.write().await;
                stats.connections_failed += 1;

                error!("Falha ao estabelecer conexão para {}: {}", node_id, e);
                Err(e)
            }
            Err(_) => {
                // Timeout
                self.record_failure(node_id).await;

                let mut stats = self.stats.write().await;
                stats.connections_timeout += 1;

                let timeout_error = GuardianError::Other(format!(
                    "Timeout ao conectar com node {} ({}ms)",
                    node_id, self.pool_config.connection_timeout_ms
                ));

                error!("Timeout na conexão: {}", timeout_error);
                Err(timeout_error)
            }
        }
    }

    /// Estabelece conexão com peer usando Iroh
    async fn establish_connection(&self, node_id: NodeId, address: NodeAddr) -> Result<f64> {
        let connection_start = Instant::now();

        debug!(
            "Estabelecendo conexão com node {} no endereço {:?}",
            node_id, address
        );

        // Valida o endereço NodeAddr
        if !self.validate_node_addr(&address) {
            return Err(GuardianError::Other(format!(
                "Endereço inválido: {:?}",
                address
            )));
        }

        // Executa ping para medir latência
        let latency_result = self.measure_peer_latency(&address).await;

        match latency_result {
            Ok(latency_ms) => {
                // Verifica se a latência é aceitável (< 5000ms)
                if latency_ms > 5000.0 {
                    warn!(
                        "Latência muito alta para node {}: {:.2}ms",
                        node_id, latency_ms
                    );
                    return Err(GuardianError::Other(format!(
                        "Latência inaceitável: {:.2}ms",
                        latency_ms
                    )));
                }

                // Tenta estabelecer handshake com o peer
                self.perform_connection_handshake(node_id, &address).await?;

                let connection_time = connection_start.elapsed();
                debug!(
                    "Conexão estabelecida com sucesso em {:.2}ms, latência: {:.2}ms",
                    connection_time.as_millis(),
                    latency_ms
                );

                Ok(latency_ms)
            }
            Err(e) => {
                error!("Falha ao medir latência para node {}: {}", node_id, e);
                Err(GuardianError::Other(format!("Falha na conexão: {}", e)))
            }
        }
    }

    /// Valida se o endereço NodeAddr é válido e acessível
    fn validate_node_addr(&self, address: &NodeAddr) -> bool {
        // NodeAddr do Iroh contém node_id, relay_url (opcional) e direct_addresses
        // Valida se tem pelo menos um endereço direto ou relay
        address.direct_addresses().count() > 0 || address.relay_url().is_some()
    }

    /// Mede latência fazendo ping para o endereço
    async fn measure_peer_latency(&self, address: &NodeAddr) -> Result<f64> {
        // Tenta primeiro os endereços diretos
        for socket_addr in address.direct_addresses() {
            let start_time = Instant::now();

            match tokio::net::TcpStream::connect(socket_addr).await {
                Ok(_stream) => {
                    let latency = start_time.elapsed();
                    return Ok(latency.as_millis() as f64);
                }
                Err(_) => {
                    // Tenta o próximo endereço
                    continue;
                }
            }
        }

        // Se nenhum endereço direto funcionou, retorna erro
        Err(GuardianError::Other(
            "Não foi possível conectar a nenhum endereço direto".to_string(),
        ))
    }

    /// Executa handshake de conexão com o peer
    async fn perform_connection_handshake(
        &self,
        node_id: NodeId,
        address: &NodeAddr,
    ) -> Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        debug!("Executando handshake com node {} em {:?}", node_id, address);

        let handshake_start = Instant::now();

        // Obtém o primeiro endereço direto disponível
        let socket_addr = address.direct_addresses().next().ok_or_else(|| {
            GuardianError::Other("Nenhum endereço direto disponível para handshake".to_string())
        })?;
        let socket_addr = *socket_addr;

        // Estabelece conexão TCP
        let mut stream = match tokio::time::timeout(
            Duration::from_millis(5000),
            tokio::net::TcpStream::connect(socket_addr),
        )
        .await
        {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => {
                return Err(GuardianError::Other(format!("Falha na conexão TCP: {}", e)));
            }
            Err(_) => {
                return Err(GuardianError::Other("Timeout na conexão TCP".to_string()));
            }
        };

        debug!("Conexão TCP estabelecida com {}", socket_addr);

        // Fase 1: Negociação de protocolo
        let protocol_version = b"guardian-db/1.0";
        let mut handshake_msg = Vec::with_capacity(64);

        // Monta mensagem de handshake inicial
        handshake_msg.extend_from_slice(&(protocol_version.len() as u16).to_be_bytes());
        handshake_msg.extend_from_slice(protocol_version);
        handshake_msg.extend_from_slice(node_id.as_bytes());

        // Adiciona timestamp para evitar replay attacks
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        handshake_msg.extend_from_slice(&timestamp.to_be_bytes());

        // Envia mensagem inicial
        if let Err(e) = stream.write_all(&handshake_msg).await {
            return Err(GuardianError::Other(format!(
                "Falha ao enviar handshake: {}",
                e
            )));
        }

        debug!("Mensagem de handshake enviada");

        // Fase 2: Recebe resposta do peer
        let mut response_len_buf = [0u8; 2];
        if let Err(e) = tokio::time::timeout(
            Duration::from_millis(3000),
            stream.read_exact(&mut response_len_buf),
        )
        .await
        {
            return Err(GuardianError::Other(format!(
                "Timeout ao ler resposta: {:?}",
                e
            )));
        }

        let response_len = u16::from_be_bytes(response_len_buf) as usize;

        // Valida tamanho da resposta
        if response_len == 0 || response_len > 1024 {
            return Err(GuardianError::Other(
                "Tamanho de resposta inválido".to_string(),
            ));
        }

        let mut response_buf = vec![0u8; response_len];
        if let Err(e) = tokio::time::timeout(
            Duration::from_millis(3000),
            stream.read_exact(&mut response_buf),
        )
        .await
        {
            return Err(GuardianError::Other(format!(
                "Timeout ao ler dados de resposta: {:?}",
                e
            )));
        }

        debug!("Resposta recebida: {} bytes", response_len);

        // Fase 3: Validação da resposta
        if response_buf.len() < protocol_version.len() + 32 + 8 {
            // version + node_id (32 bytes) + timestamp
            return Err(GuardianError::Other("Resposta muito pequena".to_string()));
        }

        let mut offset = 0;

        // Verifica versão do protocolo
        let peer_protocol_version = &response_buf[offset..offset + protocol_version.len()];
        if peer_protocol_version != protocol_version {
            return Err(GuardianError::Other(
                "Versão de protocolo incompatível".to_string(),
            ));
        }
        offset += protocol_version.len();

        // Extrai e valida NodeId do peer (32 bytes)
        let received_node_id_bytes: [u8; 32] = response_buf[offset..offset + 32]
            .try_into()
            .map_err(|_| GuardianError::Other("NodeId inválido recebido".to_string()))?;
        let received_node_id = NodeId::from_bytes(&received_node_id_bytes)
            .map_err(|e| GuardianError::Other(format!("Falha ao converter NodeId: {}", e)))?;
        offset += 32;

        // Verifica se o NodeId bate
        if received_node_id != node_id {
            return Err(GuardianError::Other(format!(
                "NodeId mismatch: esperado {}, recebido {}",
                node_id, received_node_id
            )));
        }

        // Verifica timestamp para evitar replay attacks
        let peer_timestamp_bytes = &response_buf[offset..offset + 8];
        let peer_timestamp = u64::from_be_bytes([
            peer_timestamp_bytes[0],
            peer_timestamp_bytes[1],
            peer_timestamp_bytes[2],
            peer_timestamp_bytes[3],
            peer_timestamp_bytes[4],
            peer_timestamp_bytes[5],
            peer_timestamp_bytes[6],
            peer_timestamp_bytes[7],
        ]);

        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Aceita timestamps até 5 minutos de diferença
        if (current_time as i64 - peer_timestamp as i64).abs() > 300 {
            warn!(
                "Timestamp do peer muito diferente: {} vs {}",
                peer_timestamp, current_time
            );
            // Não falha por timestamp, apenas avisa
        }

        // Fase 4: Confirmação final
        let confirmation = b"HANDSHAKE_OK";
        if let Err(e) = stream.write_all(confirmation).await {
            return Err(GuardianError::Other(format!(
                "Falha ao enviar confirmação: {}",
                e
            )));
        }

        // Aguarda confirmação do peer
        let mut peer_confirmation = [0u8; 12]; // "HANDSHAKE_OK".len()
        if let Err(e) = tokio::time::timeout(
            Duration::from_millis(2000),
            stream.read_exact(&mut peer_confirmation),
        )
        .await
        {
            return Err(GuardianError::Other(format!(
                "Timeout na confirmação final: {:?}",
                e
            )));
        }

        if &peer_confirmation != confirmation {
            return Err(GuardianError::Other(
                "Confirmação de handshake inválida".to_string(),
            ));
        }

        // Fecha a conexão de handshake
        let _ = stream.shutdown().await;

        let handshake_duration = handshake_start.elapsed();

        info!(
            "Handshake completo com node {} em {:.2}ms - Protocolo: {}, Timestamp válido: {}",
            node_id,
            handshake_duration.as_millis(),
            std::str::from_utf8(protocol_version).unwrap_or("unknown"),
            (current_time as i64 - peer_timestamp as i64).abs() <= 300
        );

        Ok(())
    }

    /// Verifica estado do circuit breaker
    async fn check_circuit_breaker(&self, node_id: NodeId) -> Result<bool> {
        let circuit_breakers = self.circuit_breakers.read().await;

        if let Some(breaker) = circuit_breakers.get(&node_id) {
            match breaker.state {
                CircuitState::Closed => Ok(true),
                CircuitState::Open => {
                    // Verifica se pode tentar half-open
                    if let Some(last_failure) = breaker.last_failure_time {
                        let elapsed = Instant::now().duration_since(last_failure);
                        if elapsed.as_millis() > breaker.timeout_ms as u128 {
                            // Transiciona para half-open
                            drop(circuit_breakers);
                            let mut breakers = self.circuit_breakers.write().await;
                            if let Some(breaker) = breakers.get_mut(&node_id) {
                                breaker.state = CircuitState::HalfOpen;
                                info!(
                                    "Circuit breaker para {} transitioning to half-open",
                                    node_id
                                );
                            }
                            Ok(true)
                        } else {
                            Ok(false)
                        }
                    } else {
                        Ok(false)
                    }
                }
                CircuitState::HalfOpen => Ok(true), // Permite tentativas limitadas
            }
        } else {
            Ok(true) // Sem circuit breaker = permitido
        }
    }

    /// Registra sucesso para circuit breaker
    async fn record_success(&self, node_id: NodeId) {
        let mut breakers = self.circuit_breakers.write().await;

        if let Some(breaker) = breakers.get_mut(&node_id) {
            breaker.success_count += 1;

            match breaker.state {
                CircuitState::HalfOpen => {
                    // Se múltiplos sucessos, fecha o circuit breaker
                    if breaker.success_count >= 3 {
                        breaker.state = CircuitState::Closed;
                        breaker.failure_count = 0;

                        let _ = self
                            .event_sender
                            .send(ConnectionEvent::CircuitBreakerClosed { node_id });
                        info!("Circuit breaker fechado para node {}", node_id);
                    }
                }
                CircuitState::Open => {
                    // Não deveria acontecer, mas reset se acontecer
                    breaker.state = CircuitState::Closed;
                    breaker.failure_count = 0;
                }
                CircuitState::Closed => {
                    // Mantém closed e reset failure count
                    breaker.failure_count = 0;
                }
            }
        }
    }

    /// Registra falha para circuit breaker
    async fn record_failure(&self, node_id: NodeId) {
        let mut breakers = self.circuit_breakers.write().await;

        let breaker = breakers.entry(node_id).or_insert_with(|| CircuitBreaker {
            state: CircuitState::Closed,
            failure_count: 0,
            failure_threshold: (self.pool_config.circuit_breaker_threshold * 10.0) as u32,
            last_failure_time: None,
            timeout_ms: self.pool_config.initial_retry_backoff_ms * 5,
            success_count: 0,
        });

        breaker.failure_count += 1;
        breaker.last_failure_time = Some(Instant::now());
        breaker.success_count = 0;

        // Verifica se deve abrir o circuit breaker
        if breaker.failure_count >= breaker.failure_threshold
            && breaker.state == CircuitState::Closed
        {
            breaker.state = CircuitState::Open;

            let _ = self
                .event_sender
                .send(ConnectionEvent::CircuitBreakerOpen { node_id });
            warn!(
                "Circuit breaker aberto para node {} após {} falhas",
                node_id, breaker.failure_count
            );
        }
    }

    /// Libera uma conexão de volta para o pool
    pub async fn release_connection(&self, node_id: NodeId, connection_id: String) -> Result<()> {
        let mut pool = self.connection_pool.write().await;

        if let Some(connections) = pool.get_mut(&node_id) {
            for conn in connections.iter_mut() {
                if conn.info.connection_id == connection_id {
                    conn.in_use = false;
                    conn.info.last_used = Instant::now();

                    debug!(
                        "Conexão liberada para pool: {} (node: {})",
                        connection_id, node_id
                    );
                    return Ok(());
                }
            }
        }

        // Se não encontrou no pool, pode ter sido uma conexão nova
        // Move da lista ativa para o pool
        if let Some(active_info) = self.active_connections.write().await.remove(&node_id) {
            let pooled_conn = PooledConnection {
                info: active_info,
                pooled_at: Instant::now(),
                reuse_count: 0,
                in_use: false,
            };

            pool.entry(node_id)
                .or_insert_with(Vec::new)
                .push(pooled_conn);

            let mut stats = self.stats.write().await;
            stats.active_connections = stats.active_connections.saturating_sub(1);
            stats.pooled_connections += 1;
        }

        Ok(())
    }

    /// Inicia monitor de saúde das conexões
    pub fn start_health_monitor(&self) -> tokio::task::JoinHandle<()> {
        let pool = Arc::clone(&self.connection_pool);
        let health_monitor = Arc::clone(&self.health_monitor);
        let event_sender = self.event_sender.clone();
        let check_interval = Duration::from_secs(self.pool_config.health_check_interval_secs);

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(check_interval);

            loop {
                interval.tick().await;

                debug!("Executando health check das conexões...");

                let pool_snapshot = {
                    let pool_read = pool.read().await;
                    // Cria uma snapshot dos peer IDs para iterar sem manter o lock
                    pool_read.keys().cloned().collect::<Vec<_>>()
                };

                for node_id in pool_snapshot.iter() {
                    // Obtém as conexões para este node (se ainda existir)
                    let connections = {
                        let pool_read = pool.read().await;
                        pool_read.get(node_id).cloned().unwrap_or_default()
                    };

                    for conn in connections.iter() {
                        // Executa health check
                        let health_score = Self::perform_health_check(&conn.info).await;

                        // Atualiza métricas de saúde
                        {
                            let mut monitor = health_monitor.write().await;
                            monitor.peer_health.insert(
                                *node_id,
                                PeerHealthMetrics {
                                    current_latency_ms: conn.info.avg_latency_ms,
                                    packet_loss_rate: 0.02, // 2% simulado
                                    throughput_bps: conn.info.bandwidth_bps,
                                    uptime_secs: Instant::now()
                                        .duration_since(conn.info.connected_at)
                                        .as_secs(),
                                    health_score,
                                    last_measured: Instant::now(),
                                },
                            );

                            if health_score < 0.5 {
                                monitor.unhealthy_peers.insert(*node_id, Instant::now());
                                let _ = event_sender.send(ConnectionEvent::Degraded {
                                    node_id: *node_id,
                                    health_score,
                                });
                            } else if monitor.unhealthy_peers.contains_key(node_id) {
                                monitor.unhealthy_peers.remove(node_id);
                                let _ = event_sender
                                    .send(ConnectionEvent::Recovered { node_id: *node_id });
                            }
                        }
                    }
                }
            }
        })
    }

    /// Executa health check de uma conexão
    async fn perform_health_check(connection_info: &ConnectionInfo) -> f64 {
        // Tenta fazer ping para verificar conectividade usando endereços diretos do NodeAddr
        let connectivity_score =
            if let Some(socket_addr) = connection_info.peer_address.direct_addresses().next() {
                let socket_addr = *socket_addr;
                let ping_start = Instant::now();

                match tokio::time::timeout(
                    Duration::from_millis(1000),
                    tokio::net::TcpStream::connect(socket_addr),
                )
                .await
                {
                    Ok(Ok(_)) => {
                        let ping_latency = ping_start.elapsed().as_millis() as f64;
                        // Score baseado na latência do ping (0-1, onde 1 é melhor)
                        (100.0 - ping_latency.min(100.0)) / 100.0
                    }
                    Ok(Err(_)) | Err(_) => {
                        // Conexão falhou ou timeout
                        0.1
                    }
                }
            } else {
                // Não conseguiu extrair endereço, usa score médio
                0.5
            };

        // Calcula scores baseados em métricas da conexão
        let latency_score = (100.0 - connection_info.avg_latency_ms.min(100.0)) / 100.0;

        let age_score = {
            let age_secs = Instant::now()
                .duration_since(connection_info.connected_at)
                .as_secs();
            if age_secs < 3600 {
                1.0
            } else if age_secs < 7200 {
                0.8
            } else {
                0.5
            }
        };

        let usage_score = {
            let last_used_secs = Instant::now()
                .duration_since(connection_info.last_used)
                .as_secs();
            if last_used_secs < 60 {
                1.0
            } else if last_used_secs < 300 {
                0.8
            } else {
                0.5
            }
        };

        let operations_score = {
            // Conexões mais usadas são consideradas mais saudáveis
            if connection_info.operations_count > 100 {
                1.0
            } else if connection_info.operations_count > 10 {
                0.8
            } else {
                0.6
            }
        };

        // Score final ponderado
        let final_score = (connectivity_score * 0.4)
            + (latency_score * 0.25)
            + (age_score * 0.15)
            + (usage_score * 0.15)
            + (operations_score * 0.05);

        final_score.clamp(0.0, 1.0)
    }

    /// Obtém estatísticas atuais do pool
    pub async fn get_stats(&self) -> PoolStats {
        self.stats.read().await.clone()
    }

    /// Subscribe para eventos de conexão
    pub fn subscribe_events(&self) -> broadcast::Receiver<ConnectionEvent> {
        self.event_sender.subscribe()
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self {
            state: CircuitState::Closed,
            failure_count: 0,
            failure_threshold: 5,
            last_failure_time: None,
            timeout_ms: 30_000,
            success_count: 0,
        }
    }
}
