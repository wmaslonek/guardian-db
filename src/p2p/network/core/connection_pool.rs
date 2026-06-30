/// Optimized connection pool for the Iroh backend.
///
/// Intelligent management of P2P connections with load balancing,
/// circuit breaking and automatic recovery to maximize throughput.
use crate::guardian::error::{GuardianError, Result};
use iroh::{EndpointAddr as NodeAddr, EndpointId as NodeId, TransportAddr};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, Semaphore, broadcast};
use tokio::time::timeout;
use tracing::{debug, error, info, instrument, warn};

/// Iterates the direct IP addresses (SocketAddr) of an EndpointAddr.
///
/// Iroh 1.0: EndpointAddr exposes `addrs: BTreeSet<TransportAddr>` (IP + relay unified),
/// replacing the former `direct_addresses()` and `relay_url()` methods.
fn direct_socket_addrs(addr: &NodeAddr) -> impl Iterator<Item = SocketAddr> + '_ {
    addr.addrs.iter().filter_map(|a| match a {
        TransportAddr::Ip(sa) => Some(*sa),
        _ => None,
    })
}

/// Optimized connection pool for P2P.
pub struct OptimizedConnectionPool {
    /// Active connections per peer.
    active_connections: Arc<RwLock<HashMap<NodeId, ConnectionInfo>>>,
    /// Pool of available connections.
    connection_pool: Arc<RwLock<HashMap<NodeId, Vec<PooledConnection>>>>,
    /// Semaphore for concurrency control.
    connection_semaphore: Arc<Semaphore>,
    /// Pool configuration.
    pool_config: PoolConfig,
    /// Performance statistics.
    stats: Arc<RwLock<PoolStats>>,
    /// Circuit breakers per peer.
    circuit_breakers: Arc<RwLock<HashMap<NodeId, CircuitBreaker>>>,
    /// Connection health monitor.
    health_monitor: Arc<RwLock<HealthMonitor>>,
    /// Channel for connection events.
    event_sender: broadcast::Sender<ConnectionEvent>,
}

/// Information about a connection.
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    /// Connection ID.
    pub connection_id: String,
    /// Peer address (Iroh NodeAddr).
    pub peer_address: NodeAddr,
    /// Connection timestamp.
    pub connected_at: Instant,
    /// Last use.
    pub last_used: Instant,
    /// Number of operations performed.
    pub operations_count: u64,
    /// Average latency (ms).
    pub avg_latency_ms: f64,
    /// Connection status.
    pub status: ConnectionStatus,
    /// Priority (0-10).
    pub priority: u8,
    /// Available bandwidth (bytes/s).
    pub bandwidth_bps: u64,
}

/// A connection in the pool.
#[derive(Debug, Clone)]
pub struct PooledConnection {
    /// Connection information.
    pub info: ConnectionInfo,
    /// Timestamp when it was placed in the pool.
    pub pooled_at: Instant,
    /// Number of times it has been reused.
    pub reuse_count: u32,
    /// Whether it is currently in use.
    pub in_use: bool,
}

/// Status of a connection.
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionStatus {
    /// Connected and healthy.
    Healthy,
    /// Connected but with problems.
    Degraded,
    /// Temporarily unavailable.
    Unavailable,
    /// Disconnected.
    Disconnected,
    /// Connection failure.
    Failed,
}

/// Connection pool configuration.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Maximum number of connections per peer.
    pub max_connections_per_peer: u32,
    /// Maximum total number of connections.
    pub max_total_connections: u32,
    /// Timeout for establishing a connection (ms).
    pub connection_timeout_ms: u64,
    /// Idle timeout before closing a connection (s).
    pub idle_timeout_secs: u64,
    /// Health check interval (s).
    pub health_check_interval_secs: u64,
    /// Maximum number of reconnection attempts.
    pub max_retry_attempts: u32,
    /// Initial retry backoff (ms).
    pub initial_retry_backoff_ms: u64,
    /// Backoff multiplier.
    pub backoff_multiplier: f64,
    /// Circuit breaker threshold.
    pub circuit_breaker_threshold: f64,
    /// Enable intelligent load balancing.
    pub enable_intelligent_load_balancing: bool,
}

/// Connection pool statistics.
#[derive(Debug, Clone, Default)]
pub struct PoolStats {
    /// Total active connections.
    pub active_connections: u32,
    /// Total connections in the pool.
    pub pooled_connections: u32,
    /// Connections created.
    pub connections_created: u64,
    /// Connections reused.
    pub connections_reused: u64,
    /// Connections that failed.
    pub connections_failed: u64,
    /// Connection timeouts.
    pub connections_timeout: u64,
    /// Average connection establishment time (ms).
    pub avg_connection_time_ms: f64,
    /// Reuse rate.
    pub reuse_rate: f64,
    /// Total bandwidth (bytes/s).
    pub total_bandwidth_bps: u64,
    /// Global average latency (ms).
    pub global_avg_latency_ms: f64,
}

/// Circuit breaker for failure control.
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    /// Current state.
    pub state: CircuitState,
    /// Failure counter.
    pub failure_count: u32,
    /// Failure threshold.
    pub failure_threshold: u32,
    /// Timestamp of the last failure.
    pub last_failure_time: Option<Instant>,
    /// Timeout before retrying (ms).
    pub timeout_ms: u64,
    /// Counter of consecutive successes.
    pub success_count: u32,
}

/// Circuit breaker states.
#[derive(Debug, Clone, PartialEq)]
pub enum CircuitState {
    /// Working normally.
    Closed,
    /// Open due to failures.
    Open,
    /// Testing whether it works again.
    HalfOpen,
}

/// Connection health monitor.
#[derive(Debug)]
pub struct HealthMonitor {
    /// Health metrics per peer.
    peer_health: HashMap<NodeId, PeerHealthMetrics>,
    /// Last health check.
    #[allow(dead_code)]
    last_health_check: Instant,
    /// Peers marked as problematic.
    unhealthy_peers: HashMap<NodeId, Instant>,
}

/// Health metrics of a peer.
#[derive(Debug, Clone)]
pub struct PeerHealthMetrics {
    /// Current latency (ms).
    pub current_latency_ms: f64,
    /// Packet loss (0.0-1.0).
    pub packet_loss_rate: f64,
    /// Throughput (bytes/s).
    pub throughput_bps: u64,
    /// Uptime (seconds).
    pub uptime_secs: u64,
    /// Health score (0.0-1.0).
    pub health_score: f64,
    /// Timestamp of the last measurement.
    pub last_measured: Instant,
}

/// Connection events.
#[derive(Debug, Clone)]
pub enum ConnectionEvent {
    /// New connection established.
    Connected { node_id: NodeId, latency_ms: f64 },
    /// Connection lost.
    Disconnected { node_id: NodeId, reason: String },
    /// Connection degraded.
    Degraded { node_id: NodeId, health_score: f64 },
    /// Connection recovered.
    Recovered { node_id: NodeId },
    /// Circuit breaker opened.
    CircuitBreakerOpen { node_id: NodeId },
    /// Circuit breaker closed.
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
    /// Creates a new optimized connection pool.
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

    /// Gets or creates an optimized connection for a peer.
    #[instrument(skip(self))]
    pub async fn get_connection(&self, node_id: NodeId, address: NodeAddr) -> Result<String> {
        // Check the circuit breaker.
        if !self.check_circuit_breaker(node_id).await? {
            return Err(GuardianError::Other(format!(
                "Circuit breaker open for node {}",
                node_id
            )));
        }

        // Try to reuse a connection from the pool.
        if let Some(connection_id) = self.try_reuse_connection(node_id).await? {
            debug!("Reusing existing connection for node {}", node_id);
            return Ok(connection_id);
        }

        // Acquire a permit for a new connection.
        let _permit = self
            .connection_semaphore
            .acquire()
            .await
            .map_err(|e| GuardianError::Other(format!("Failed to acquire semaphore: {}", e)))?;

        // Establish a new connection.
        self.establish_new_connection(node_id, address).await
    }

    /// Tries to reuse an existing connection from the pool.
    async fn try_reuse_connection(&self, node_id: NodeId) -> Result<Option<String>> {
        let mut pool = self.connection_pool.write().await;

        if let Some(connections) = pool.get_mut(&node_id) {
            // Look for an available healthy connection.
            for conn in connections.iter_mut() {
                if !conn.in_use && conn.info.status == ConnectionStatus::Healthy {
                    // Check that it is not too idle.
                    let idle_time = Instant::now().saturating_duration_since(conn.info.last_used);
                    if idle_time.as_secs() < self.pool_config.idle_timeout_secs {
                        conn.in_use = true;
                        conn.reuse_count += 1;
                        conn.info.last_used = Instant::now();

                        // Update statistics.
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

    /// Establishes a new connection with optimizations.
    async fn establish_new_connection(&self, node_id: NodeId, address: NodeAddr) -> Result<String> {
        let connection_start = Instant::now();
        let connection_id = format!("conn_{}_{}", node_id, uuid::Uuid::new_v4());

        debug!(
            "Establishing new connection for node {} at {:?}",
            node_id, address
        );

        // Establish the connection with a timeout.
        let connection_result = timeout(
            Duration::from_millis(self.pool_config.connection_timeout_ms),
            self.establish_connection(node_id, address.clone()),
        )
        .await;

        match connection_result {
            Ok(Ok(latency_ms)) => {
                // Connection established successfully.
                let connection_time = connection_start.elapsed();

                let connection_info = ConnectionInfo {
                    connection_id: connection_id.clone(),
                    peer_address: address,
                    connected_at: Instant::now(),
                    last_used: Instant::now(),
                    operations_count: 0,
                    avg_latency_ms: latency_ms,
                    status: ConnectionStatus::Healthy,
                    priority: 5,               // Default priority.
                    bandwidth_bps: 10_000_000, // 10 Mbps initial estimate.
                };

                // Add it to the active connections list.
                {
                    let mut active = self.active_connections.write().await;
                    active.insert(node_id, connection_info.clone());
                }

                // Update statistics.
                {
                    let mut stats = self.stats.write().await;
                    stats.connections_created += 1;
                    stats.active_connections += 1;
                    stats.avg_connection_time_ms =
                        (stats.avg_connection_time_ms + connection_time.as_millis() as f64) / 2.0;
                }

                // Record the success in the circuit breaker.
                self.record_success(node_id).await;

                // Send an event.
                let _ = self.event_sender.send(ConnectionEvent::Connected {
                    node_id,
                    latency_ms,
                });

                info!(
                    "New connection established: {} -> {} (latency: {:.2}ms)",
                    node_id, connection_id, latency_ms
                );
                Ok(connection_id)
            }
            Ok(Err(e)) => {
                // Connection failure.
                self.record_failure(node_id).await;

                let mut stats = self.stats.write().await;
                stats.connections_failed += 1;

                error!("Failed to establish connection to {}: {}", node_id, e);
                Err(e)
            }
            Err(_) => {
                // Timeout.
                self.record_failure(node_id).await;

                let mut stats = self.stats.write().await;
                stats.connections_timeout += 1;

                let timeout_error = GuardianError::Other(format!(
                    "Timeout connecting to node {} ({}ms)",
                    node_id, self.pool_config.connection_timeout_ms
                ));

                error!("Connection timeout: {}", timeout_error);
                Err(timeout_error)
            }
        }
    }

    /// Establishes a connection to a peer using Iroh.
    async fn establish_connection(&self, node_id: NodeId, address: NodeAddr) -> Result<f64> {
        let connection_start = Instant::now();

        debug!(
            "Establishing connection to node {} at address {:?}",
            node_id, address
        );

        // Validate the NodeAddr address.
        if !self.validate_node_addr(&address) {
            return Err(GuardianError::Other(format!(
                "Invalid address: {:?}",
                address
            )));
        }

        // Perform a ping to measure latency.
        let latency_result = self.measure_peer_latency(&address).await;

        match latency_result {
            Ok(latency_ms) => {
                // Check whether the latency is acceptable (< 5000ms).
                if latency_ms > 5000.0 {
                    warn!("Latency too high for node {}: {:.2}ms", node_id, latency_ms);
                    return Err(GuardianError::Other(format!(
                        "Unacceptable latency: {:.2}ms",
                        latency_ms
                    )));
                }

                // Try to establish a handshake with the peer.
                self.perform_connection_handshake(node_id, &address).await?;

                let connection_time = connection_start.elapsed();
                debug!(
                    "Connection established successfully in {:.2}ms, latency: {:.2}ms",
                    connection_time.as_millis(),
                    latency_ms
                );

                Ok(latency_ms)
            }
            Err(e) => {
                error!("Failed to measure latency for node {}: {}", node_id, e);
                Err(GuardianError::Other(format!("Connection failure: {}", e)))
            }
        }
    }

    /// Validates whether the NodeAddr address is valid and reachable.
    fn validate_node_addr(&self, address: &NodeAddr) -> bool {
        // Iroh's EndpointAddr contains id and addrs (IP + relay unified in TransportAddr).
        // Validate that it has at least one transport address.
        !address.addrs.is_empty()
    }

    /// Measures latency by pinging the address.
    async fn measure_peer_latency(&self, address: &NodeAddr) -> Result<f64> {
        // Try the direct addresses first.
        for socket_addr in direct_socket_addrs(address) {
            let start_time = Instant::now();

            match tokio::net::TcpStream::connect(socket_addr).await {
                Ok(_stream) => {
                    let latency = start_time.elapsed();
                    return Ok(latency.as_millis() as f64);
                }
                Err(_) => {
                    // Try the next address.
                    continue;
                }
            }
        }

        // If no direct address worked, return an error.
        Err(GuardianError::Other(
            "Could not connect to any direct address".to_string(),
        ))
    }

    /// Performs the connection handshake with the peer.
    async fn perform_connection_handshake(
        &self,
        node_id: NodeId,
        address: &NodeAddr,
    ) -> Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        debug!(
            "Performing handshake with node {} at {:?}",
            node_id, address
        );

        let handshake_start = Instant::now();

        // Get the first available direct address.
        let socket_addr = direct_socket_addrs(address).next().ok_or_else(|| {
            GuardianError::Other("No direct address available for handshake".to_string())
        })?;

        // Estabelece conexão TCP
        let mut stream = match tokio::time::timeout(
            Duration::from_millis(5000),
            tokio::net::TcpStream::connect(socket_addr),
        )
        .await
        {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => {
                return Err(GuardianError::Other(format!(
                    "TCP connection failure: {}",
                    e
                )));
            }
            Err(_) => {
                return Err(GuardianError::Other("TCP connection timeout".to_string()));
            }
        };

        debug!("TCP connection established with {}", socket_addr);

        // Phase 1: Protocol negotiation.
        let protocol_version = b"guardian-db/1.0";
        let mut handshake_msg = Vec::with_capacity(64);

        // Build the initial handshake message.
        handshake_msg.extend_from_slice(&(protocol_version.len() as u16).to_be_bytes());
        handshake_msg.extend_from_slice(protocol_version);
        handshake_msg.extend_from_slice(node_id.as_bytes());

        // Add a timestamp to prevent replay attacks.
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        handshake_msg.extend_from_slice(&timestamp.to_be_bytes());

        // Send the initial message.
        if let Err(e) = stream.write_all(&handshake_msg).await {
            return Err(GuardianError::Other(format!(
                "Failed to send handshake: {}",
                e
            )));
        }

        debug!("Handshake message sent");

        // Phase 2: Receive the peer's response.
        let mut response_len_buf = [0u8; 2];
        if let Err(e) = tokio::time::timeout(
            Duration::from_millis(3000),
            stream.read_exact(&mut response_len_buf),
        )
        .await
        {
            return Err(GuardianError::Other(format!(
                "Timeout reading response: {:?}",
                e
            )));
        }

        let response_len = u16::from_be_bytes(response_len_buf) as usize;

        // Validate the response size.
        if response_len == 0 || response_len > 1024 {
            return Err(GuardianError::Other("Invalid response size".to_string()));
        }

        let mut response_buf = vec![0u8; response_len];
        if let Err(e) = tokio::time::timeout(
            Duration::from_millis(3000),
            stream.read_exact(&mut response_buf),
        )
        .await
        {
            return Err(GuardianError::Other(format!(
                "Timeout reading response data: {:?}",
                e
            )));
        }

        debug!("Response received: {} bytes", response_len);

        // Phase 3: Response validation.
        if response_buf.len() < protocol_version.len() + 32 + 8 {
            // version + node_id (32 bytes) + timestamp
            return Err(GuardianError::Other("Response too small".to_string()));
        }

        let mut offset = 0;

        // Check the protocol version.
        let peer_protocol_version = &response_buf[offset..offset + protocol_version.len()];
        if peer_protocol_version != protocol_version {
            return Err(GuardianError::Other(
                "Incompatible protocol version".to_string(),
            ));
        }
        offset += protocol_version.len();

        // Extract and validate the peer's NodeId (32 bytes).
        let received_node_id_bytes: [u8; 32] = response_buf[offset..offset + 32]
            .try_into()
            .map_err(|_| GuardianError::Other("Invalid NodeId received".to_string()))?;
        let received_node_id = NodeId::from_bytes(&received_node_id_bytes)
            .map_err(|e| GuardianError::Other(format!("Failed to convert NodeId: {}", e)))?;
        offset += 32;

        // Check that the NodeId matches.
        if received_node_id != node_id {
            return Err(GuardianError::Other(format!(
                "NodeId mismatch: expected {}, received {}",
                node_id, received_node_id
            )));
        }

        // Check the timestamp to prevent replay attacks.
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

        // Accept timestamps up to 5 minutes apart.
        if (current_time as i64 - peer_timestamp as i64).abs() > 300 {
            warn!(
                "Peer timestamp too different: {} vs {}",
                peer_timestamp, current_time
            );
            // Does not fail on timestamp, only warns.
        }

        // Phase 4: Final confirmation.
        let confirmation = b"HANDSHAKE_OK";
        if let Err(e) = stream.write_all(confirmation).await {
            return Err(GuardianError::Other(format!(
                "Failed to send confirmation: {}",
                e
            )));
        }

        // Wait for the peer's confirmation.
        let mut peer_confirmation = [0u8; 12]; // "HANDSHAKE_OK".len()
        if let Err(e) = tokio::time::timeout(
            Duration::from_millis(2000),
            stream.read_exact(&mut peer_confirmation),
        )
        .await
        {
            return Err(GuardianError::Other(format!(
                "Timeout on final confirmation: {:?}",
                e
            )));
        }

        if &peer_confirmation != confirmation {
            return Err(GuardianError::Other(
                "Invalid handshake confirmation".to_string(),
            ));
        }

        // Close the handshake connection.
        let _ = stream.shutdown().await;

        let handshake_duration = handshake_start.elapsed();

        info!(
            "Handshake complete with node {} in {:.2}ms - Protocol: {}, Timestamp valid: {}",
            node_id,
            handshake_duration.as_millis(),
            std::str::from_utf8(protocol_version).unwrap_or("unknown"),
            (current_time as i64 - peer_timestamp as i64).abs() <= 300
        );

        Ok(())
    }

    /// Checks the circuit breaker state.
    async fn check_circuit_breaker(&self, node_id: NodeId) -> Result<bool> {
        let circuit_breakers = self.circuit_breakers.read().await;

        if let Some(breaker) = circuit_breakers.get(&node_id) {
            match breaker.state {
                CircuitState::Closed => Ok(true),
                CircuitState::Open => {
                    // Check whether it can attempt half-open.
                    if let Some(last_failure) = breaker.last_failure_time {
                        let elapsed = Instant::now().saturating_duration_since(last_failure);
                        if elapsed.as_millis() > breaker.timeout_ms as u128 {
                            // Transition to half-open.
                            drop(circuit_breakers);
                            let mut breakers = self.circuit_breakers.write().await;
                            if let Some(breaker) = breakers.get_mut(&node_id) {
                                breaker.state = CircuitState::HalfOpen;
                                info!("Circuit breaker for {} transitioning to half-open", node_id);
                            }
                            Ok(true)
                        } else {
                            Ok(false)
                        }
                    } else {
                        Ok(false)
                    }
                }
                CircuitState::HalfOpen => Ok(true), // Allows limited attempts.
            }
        } else {
            Ok(true) // No circuit breaker = allowed.
        }
    }

    /// Records a success for the circuit breaker.
    async fn record_success(&self, node_id: NodeId) {
        let mut breakers = self.circuit_breakers.write().await;

        if let Some(breaker) = breakers.get_mut(&node_id) {
            breaker.success_count += 1;

            match breaker.state {
                CircuitState::HalfOpen => {
                    // After multiple successes, close the circuit breaker.
                    if breaker.success_count >= 3 {
                        breaker.state = CircuitState::Closed;
                        breaker.failure_count = 0;

                        let _ = self
                            .event_sender
                            .send(ConnectionEvent::CircuitBreakerClosed { node_id });
                        info!("Circuit breaker closed for node {}", node_id);
                    }
                }
                CircuitState::Open => {
                    // Should not happen, but reset if it does.
                    breaker.state = CircuitState::Closed;
                    breaker.failure_count = 0;
                }
                CircuitState::Closed => {
                    // Keep it closed and reset the failure count.
                    breaker.failure_count = 0;
                }
            }
        }
    }

    /// Records a failure for the circuit breaker.
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

        // Check whether the circuit breaker should open.
        if breaker.failure_count >= breaker.failure_threshold
            && breaker.state == CircuitState::Closed
        {
            breaker.state = CircuitState::Open;

            let _ = self
                .event_sender
                .send(ConnectionEvent::CircuitBreakerOpen { node_id });
            warn!(
                "Circuit breaker opened for node {} after {} failures",
                node_id, breaker.failure_count
            );
        }
    }

    /// Releases a connection back to the pool.
    pub async fn release_connection(&self, node_id: NodeId, connection_id: String) -> Result<()> {
        let mut pool = self.connection_pool.write().await;

        if let Some(connections) = pool.get_mut(&node_id) {
            for conn in connections.iter_mut() {
                if conn.info.connection_id == connection_id {
                    conn.in_use = false;
                    conn.info.last_used = Instant::now();

                    debug!(
                        "Connection released to pool: {} (node: {})",
                        connection_id, node_id
                    );
                    return Ok(());
                }
            }
        }

        // If it was not found in the pool, it may have been a new connection.
        // Move it from the active list to the pool.
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

    /// Starts the connection health monitor.
    pub fn start_health_monitor(&self) -> tokio::task::JoinHandle<()> {
        let pool = Arc::clone(&self.connection_pool);
        let health_monitor = Arc::clone(&self.health_monitor);
        let event_sender = self.event_sender.clone();
        let check_interval = Duration::from_secs(self.pool_config.health_check_interval_secs);

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(check_interval);

            loop {
                interval.tick().await;

                debug!("Running connection health check...");

                let pool_snapshot = {
                    let pool_read = pool.read().await;
                    // Create a snapshot of the peer IDs to iterate without holding the lock.
                    pool_read.keys().cloned().collect::<Vec<_>>()
                };

                for node_id in pool_snapshot.iter() {
                    // Get the connections for this node (if it still exists).
                    let connections = {
                        let pool_read = pool.read().await;
                        pool_read.get(node_id).cloned().unwrap_or_default()
                    };

                    for conn in connections.iter() {
                        // Run a health check.
                        let health_score = Self::perform_health_check(&conn.info).await;

                        // Update the health metrics.
                        {
                            let mut monitor = health_monitor.write().await;
                            monitor.peer_health.insert(
                                *node_id,
                                PeerHealthMetrics {
                                    current_latency_ms: conn.info.avg_latency_ms,
                                    packet_loss_rate: 0.02, // 2% simulated
                                    throughput_bps: conn.info.bandwidth_bps,
                                    uptime_secs: Instant::now()
                                        .saturating_duration_since(conn.info.connected_at)
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

    /// Performs a health check of a connection.
    async fn perform_health_check(connection_info: &ConnectionInfo) -> f64 {
        // Try to ping to check connectivity using the NodeAddr's direct addresses.
        let connectivity_score =
            if let Some(socket_addr) = direct_socket_addrs(&connection_info.peer_address).next() {
                let ping_start = Instant::now();

                match tokio::time::timeout(
                    Duration::from_millis(1000),
                    tokio::net::TcpStream::connect(socket_addr),
                )
                .await
                {
                    Ok(Ok(_)) => {
                        let ping_latency = ping_start.elapsed().as_millis() as f64;
                        // Score based on the ping latency (0-1, where 1 is best).
                        (100.0 - ping_latency.min(100.0)) / 100.0
                    }
                    Ok(Err(_)) | Err(_) => {
                        // Connection failed or timed out.
                        0.1
                    }
                }
            } else {
                // Could not extract an address, use an average score.
                0.5
            };

        // Compute scores based on connection metrics.
        let latency_score = (100.0 - connection_info.avg_latency_ms.min(100.0)) / 100.0;

        let age_score = {
            let age_secs = Instant::now()
                .saturating_duration_since(connection_info.connected_at)
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
                .saturating_duration_since(connection_info.last_used)
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
            // More heavily used connections are considered healthier.
            if connection_info.operations_count > 100 {
                1.0
            } else if connection_info.operations_count > 10 {
                0.8
            } else {
                0.6
            }
        };

        // Final weighted score.
        let final_score = (connectivity_score * 0.4)
            + (latency_score * 0.25)
            + (age_score * 0.15)
            + (usage_score * 0.15)
            + (operations_score * 0.05);

        final_score.clamp(0.0, 1.0)
    }

    /// Returns the current pool statistics.
    pub async fn get_stats(&self) -> PoolStats {
        self.stats.read().await.clone()
    }

    /// Subscribes to connection events.
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
