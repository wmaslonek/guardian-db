# Connection Pool - Optimized P2P Connection Management Documentation

## Overview

The Optimized Connection Pool provides intelligent management of P2P connections for the Iroh backend. It features connection reuse, load balancing, circuit breaking, automatic recovery, and comprehensive health monitoring to maximize throughput and reliability while minimizing resource usage.

## Table of Contents

- [Architecture](#architecture)
- [Core Components](#core-components)
- [Configuration](#configuration)
- [Connection Lifecycle](#connection-lifecycle)
- [Circuit Breaker Pattern](#circuit-breaker-pattern)
- [Health Monitoring](#health-monitoring)
- [API Reference](#api-reference)
- [Event System](#event-system)
- [Performance Optimization](#performance-optimization)
- [Usage Examples](#usage-examples)
- [Best Practices](#best-practices)
- [Troubleshooting](#troubleshooting)

## Architecture

### High-Level Architecture

```
┌──────────────────────────────────────────────────────────┐
│              OptimizedConnectionPool                     │
├──────────────────────────────────────────────────────────┤
│  ┌────────────────────────────────────────────────┐     │
│  │    Active Connections (HashMap)                │     │
│  │    peer_id -> ConnectionInfo                   │     │
│  └────────────────────────────────────────────────┘     │
│  ┌────────────────────────────────────────────────┐     │
│  │    Connection Pool (HashMap)                   │     │
│  │    peer_id -> Vec<PooledConnection>            │     │
│  └────────────────────────────────────────────────┘     │
│  ┌────────────────────────────────────────────────┐     │
│  │    Circuit Breakers (HashMap)                  │     │
│  │    peer_id -> CircuitBreaker                   │     │
│  └────────────────────────────────────────────────┘     │
│  ┌────────────────────────────────────────────────┐     │
│  │    Health Monitor                              │     │
│  │    - Peer health metrics                       │     │
│  │    - Unhealthy peer tracking                   │     │
│  └────────────────────────────────────────────────┘     │
│  ┌────────────────────────────────────────────────┐     │
│  │    Semaphore (Concurrency Control)             │     │
│  │    Max: max_total_connections                  │     │
│  └────────────────────────────────────────────────┘     │
└──────────────────────────────────────────────────────────┘
```

### Connection Flow

```
get_connection(peer_id, address)
    ↓
[Check Circuit Breaker]
    ↓ Open → Return Error
    ↓ Closed/HalfOpen
[Try Reuse Existing Connection]
    ↓ Found → Return connection_id
    ↓ Not Found
[Acquire Semaphore Permit]
    ↓
[Establish New Connection]
    ├─ Validate Multiaddr
    ├─ Measure Latency (Ping)
    ├─ Perform Handshake
    │    ├─ Protocol Negotiation
    │    ├─ PeerId Verification
    │    ├─ Timestamp Exchange
    │    └─ Final Confirmation
    ↓
[Update Statistics & Circuit Breaker]
    ↓
Return connection_id
```

## Core Components

### 1. OptimizedConnectionPool

Main connection pool structure:

```rust
pub struct OptimizedConnectionPool {
    /// Active connections by peer
    active_connections: Arc<RwLock<HashMap<PeerId, ConnectionInfo>>>,
    
    /// Pool of available connections
    connection_pool: Arc<RwLock<HashMap<PeerId, Vec<PooledConnection>>>>,
    
    /// Concurrency control semaphore
    connection_semaphore: Arc<Semaphore>,
    
    /// Pool configuration
    config: PoolConfig,
    
    /// Performance statistics
    stats: Arc<RwLock<PoolStats>>,
    
    /// Circuit breakers per peer
    circuit_breakers: Arc<RwLock<HashMap<PeerId, CircuitBreaker>>>,
    
    /// Connection health monitor
    health_monitor: Arc<RwLock<HealthMonitor>>,
    
    /// Event broadcast channel
    event_sender: broadcast::Sender<ConnectionEvent>,
}
```

### 2. ConnectionInfo

Information about an active connection:

```rust
pub struct ConnectionInfo {
    /// Unique connection ID
    pub connection_id: String,
    
    /// Peer's multiaddr
    pub peer_address: Multiaddr,
    
    /// Connection timestamp
    pub connected_at: Instant,
    
    /// Last used timestamp
    pub last_used: Instant,
    
    /// Number of operations performed
    pub operations_count: u64,
    
    /// Average latency (ms)
    pub avg_latency_ms: f64,
    
    /// Connection status
    pub status: ConnectionStatus,
    
    /// Priority (0-10)
    pub priority: u8,
    
    /// Available bandwidth (bytes/s)
    pub bandwidth_bps: u64,
}
```

### 3. PooledConnection

Connection in the reuse pool:

```rust
pub struct PooledConnection {
    /// Connection information
    pub info: ConnectionInfo,
    
    /// When pooled
    pub pooled_at: Instant,
    
    /// Number of times reused
    pub reuse_count: u32,
    
    /// Currently in use
    pub in_use: bool,
}
```

### 4. CircuitBreaker

Failure protection per peer:

```rust
pub struct CircuitBreaker {
    /// Current state
    pub state: CircuitState,
    
    /// Failure counter
    pub failure_count: u32,
    
    /// Failure threshold
    pub failure_threshold: u32,
    
    /// Last failure time
    pub last_failure_time: Option<Instant>,
    
    /// Retry timeout (ms)
    pub timeout_ms: u64,
    
    /// Consecutive success counter
    pub success_count: u32,
}
```

## Configuration

### PoolConfig

```rust
pub struct PoolConfig {
    /// Max connections per peer
    pub max_connections_per_peer: u32,       // Default: 8
    
    /// Max total connections
    pub max_total_connections: u32,          // Default: 1000
    
    /// Connection timeout (ms)
    pub connection_timeout_ms: u64,          // Default: 10000
    
    /// Idle timeout (seconds)
    pub idle_timeout_secs: u64,              // Default: 300
    
    /// Health check interval (seconds)
    pub health_check_interval_secs: u64,     // Default: 30
    
    /// Max retry attempts
    pub max_retry_attempts: u32,             // Default: 3
    
    /// Initial retry backoff (ms)
    pub initial_retry_backoff_ms: u64,       // Default: 1000
    
    /// Backoff multiplier
    pub backoff_multiplier: f64,             // Default: 2.0
    
    /// Circuit breaker threshold
    pub circuit_breaker_threshold: f64,      // Default: 0.5
    
    /// Enable intelligent load balancing
    pub enable_intelligent_load_balancing: bool,  // Default: true
}
```

### Default Configuration

```rust
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
```

## Connection Lifecycle

### 1. Connection Acquisition

```rust
pub async fn get_connection(&self, peer_id: PeerId, address: Multiaddr) -> Result<String>
```

**Process:**
1. Check circuit breaker status
2. Try to reuse existing connection from pool
3. Acquire semaphore permit (concurrency control)
4. Establish new connection if needed

**Example:**
```rust
let pool = OptimizedConnectionPool::new(PoolConfig::default());
let peer_id = "12D3KooW...".parse()?;
let address: Multiaddr = "/ip4/192.168.1.100/tcp/4001".parse()?;

let connection_id = pool.get_connection(peer_id, address).await?;
println!("Got connection: {}", connection_id);
```

### 2. Connection Reuse

```rust
async fn try_reuse_connection(&self, peer_id: PeerId) -> Result<Option<String>> {
    let mut pool = self.connection_pool.write().await;
    
    if let Some(connections) = pool.get_mut(&peer_id) {
        for conn in connections.iter_mut() {
            if !conn.in_use && conn.info.status == ConnectionStatus::Healthy {
                // Check idle time
                let idle_time = Instant::now().duration_since(conn.info.last_used);
                
                if idle_time.as_secs() < self.config.idle_timeout_secs {
                    // Mark as in use
                    conn.in_use = true;
                    conn.reuse_count += 1;
                    conn.info.last_used = Instant::now();
                    
                    // Update stats
                    let mut stats = self.stats.write().await;
                    stats.connections_reused += 1;
                    
                    return Ok(Some(conn.info.connection_id.clone()));
                }
            }
        }
    }
    
    Ok(None)
}
```

**Reuse Criteria:**
- Connection not currently in use
- Status is Healthy
- Not idle for > idle_timeout_secs
- Same peer_id

### 3. Connection Establishment

```rust
async fn establish_new_connection(
    &self,
    peer_id: PeerId,
    address: Multiaddr,
) -> Result<String> {
    // Generate unique connection ID
    let connection_id = format!("conn_{}_{}", peer_id, uuid::Uuid::new_v4());
    
    // Establish with timeout
    let connection_result = timeout(
        Duration::from_millis(self.config.connection_timeout_ms),
        self.establish_connection(peer_id, address.clone()),
    ).await;
    
    match connection_result {
        Ok(Ok(latency_ms)) => {
            // Success: Create ConnectionInfo
            let connection_info = ConnectionInfo {
                connection_id: connection_id.clone(),
                peer_address: address,
                connected_at: Instant::now(),
                last_used: Instant::now(),
                operations_count: 0,
                avg_latency_ms: latency_ms,
                status: ConnectionStatus::Healthy,
                priority: 5,
                bandwidth_bps: 10_000_000,  // 10 Mbps estimated
            };
            
            // Add to active connections
            self.active_connections.write().await.insert(peer_id, connection_info);
            
            // Update stats
            self.stats.write().await.connections_created += 1;
            
            // Record success in circuit breaker
            self.record_success(peer_id).await;
            
            // Emit event
            self.event_sender.send(ConnectionEvent::Connected {
                peer_id,
                latency_ms,
            });
            
            Ok(connection_id)
        },
        Ok(Err(e)) => {
            // Failure
            self.record_failure(peer_id).await;
            self.stats.write().await.connections_failed += 1;
            Err(e)
        },
        Err(_) => {
            // Timeout
            self.record_failure(peer_id).await;
            self.stats.write().await.connections_timeout += 1;
            Err(GuardianError::Other("Connection timeout"))
        }
    }
}
```

### 4. Connection Release

```rust
pub async fn release_connection(&self, peer_id: PeerId, connection_id: String) -> Result<()>
```

**Process:**
1. Find connection in pool by connection_id
2. Mark as not in use
3. Update last_used timestamp
4. If not in pool, move from active to pool

**Example:**
```rust
// Use connection
let connection_id = pool.get_connection(peer_id, address).await?;

// ... perform operations ...

// Release back to pool
pool.release_connection(peer_id, connection_id).await?;
```

## Connection Handshake Protocol

### 4-Phase Handshake

```
Client                                  Peer
  │                                       │
  │  Phase 1: Protocol Negotiation        │
  ├─────────────────────────────────────>│
  │  [len][protocol][peer_id][timestamp]  │
  │                                       │
  │  Phase 2: Peer Response               │
  │<─────────────────────────────────────┤
  │  [len][protocol][peer_id][timestamp]  │
  │                                       │
  │  Phase 3: Validation & Confirmation   │
  ├─────────────────────────────────────>│
  │  "HANDSHAKE_OK"                       │
  │                                       │
  │  Phase 4: Final Confirmation          │
  │<─────────────────────────────────────┤
  │  "HANDSHAKE_OK"                       │
  │                                       │
  │  Connection Established ✓             │
```

### Handshake Implementation

```rust
async fn perform_connection_handshake(
    &self,
    peer_id: PeerId,
    address: &Multiaddr,
) -> Result<()> {
    // Establish TCP stream
    let mut stream = TcpStream::connect(socket_addr).await?;
    
    // Phase 1: Send handshake
    let protocol_version = b"guardian-db/1.0";
    let mut handshake_msg = Vec::new();
    handshake_msg.extend_from_slice(&(protocol_version.len() as u16).to_be_bytes());
    handshake_msg.extend_from_slice(protocol_version);
    handshake_msg.extend_from_slice(&peer_id.to_bytes());
    handshake_msg.extend_from_slice(&timestamp.to_be_bytes());
    stream.write_all(&handshake_msg).await?;
    
    // Phase 2: Read response
    let mut response_len_buf = [0u8; 2];
    stream.read_exact(&mut response_len_buf).await?;
    let response_len = u16::from_be_bytes(response_len_buf);
    
    let mut response_buf = vec![0u8; response_len as usize];
    stream.read_exact(&mut response_buf).await?;
    
    // Phase 3: Validate response
    // - Check protocol version
    // - Verify PeerId matches
    // - Validate timestamp (±5 minutes)
    
    // Phase 4: Send confirmation
    stream.write_all(b"HANDSHAKE_OK").await?;
    let mut peer_confirmation = [0u8; 12];
    stream.read_exact(&mut peer_confirmation).await?;
    
    Ok(())
}
```

**Security Features:**
- Protocol version negotiation
- PeerId verification
- Timestamp validation (prevents replay attacks)
- Mutual confirmation

## Circuit Breaker Pattern

### Circuit States

```rust
pub enum CircuitState {
    /// Operating normally
    Closed,
    
    /// Open due to failures
    Open,
    
    /// Testing if recovered
    HalfOpen,
}
```

### State Transitions

```
    Closed ──[failures ≥ threshold]──> Open
      ↑                                  │
      │                                  │
      │                         [timeout elapsed]
      │                                  │
      │                                  ↓
      └────[3 successes]────────── HalfOpen
```

### Circuit Breaker Logic

```rust
async fn check_circuit_breaker(&self, peer_id: PeerId) -> Result<bool> {
    let breakers = self.circuit_breakers.read().await;
    
    if let Some(breaker) = breakers.get(&peer_id) {
        match breaker.state {
            CircuitState::Closed => Ok(true),  // Allow connections
            
            CircuitState::Open => {
                // Check if timeout elapsed
                if let Some(last_failure) = breaker.last_failure_time {
                    let elapsed = Instant::now().duration_since(last_failure);
                    
                    if elapsed.as_millis() > breaker.timeout_ms as u128 {
                        // Transition to HalfOpen
                        breaker.state = CircuitState::HalfOpen;
                        Ok(true)
                    } else {
                        Ok(false)  // Still in timeout
                    }
                } else {
                    Ok(false)
                }
            },
            
            CircuitState::HalfOpen => Ok(true),  // Allow limited attempts
        }
    } else {
        Ok(true)  // No breaker = allowed
    }
}
```

### Failure Recording

```rust
async fn record_failure(&self, peer_id: PeerId) {
    let mut breakers = self.circuit_breakers.write().await;
    
    let breaker = breakers.entry(peer_id).or_insert_with(CircuitBreaker::default);
    
    breaker.failure_count += 1;
    breaker.last_failure_time = Some(Instant::now());
    breaker.success_count = 0;
    
    // Open circuit breaker if threshold reached
    if breaker.failure_count >= breaker.failure_threshold
        && breaker.state == CircuitState::Closed {
        breaker.state = CircuitState::Open;
        
        self.event_sender.send(ConnectionEvent::CircuitBreakerOpen { peer_id });
        
        warn!("Circuit breaker opened for {} after {} failures",
            peer_id, breaker.failure_count);
    }
}
```

### Success Recording

```rust
async fn record_success(&self, peer_id: PeerId) {
    let mut breakers = self.circuit_breakers.write().await;
    
    if let Some(breaker) = breakers.get_mut(&peer_id) {
        breaker.success_count += 1;
        
        match breaker.state {
            CircuitState::HalfOpen => {
                // Close breaker after 3 consecutive successes
                if breaker.success_count >= 3 {
                    breaker.state = CircuitState::Closed;
                    breaker.failure_count = 0;
                    
                    self.event_sender.send(ConnectionEvent::CircuitBreakerClosed { peer_id });
                }
            },
            CircuitState::Closed => {
                // Reset failure count
                breaker.failure_count = 0;
            },
            _ => {}
        }
    }
}
```

## Health Monitoring

### Health Score Calculation

Multi-factor health scoring with weighted components:

```rust
async fn perform_health_check(connection_info: &ConnectionInfo) -> f64 {
    // 1. Connectivity score (40% weight)
    let connectivity_score = {
        let ping_latency = measure_ping(socket_addr).await;
        (100.0 - ping_latency.min(100.0)) / 100.0
    };
    
    // 2. Latency score (25% weight)
    let latency_score = (100.0 - connection_info.avg_latency_ms.min(100.0)) / 100.0;
    
    // 3. Age score (15% weight)
    let age_score = {
        let age_secs = connection_info.connected_at.elapsed().as_secs();
        if age_secs < 3600 { 1.0 }       // < 1 hour: excellent
        else if age_secs < 7200 { 0.8 }  // < 2 hours: good
        else { 0.5 }                     // > 2 hours: fair
    };
    
    // 4. Usage score (15% weight)
    let usage_score = {
        let idle_secs = connection_info.last_used.elapsed().as_secs();
        if idle_secs < 60 { 1.0 }        // < 1 min: active
        else if idle_secs < 300 { 0.8 }  // < 5 min: recent
        else { 0.5 }                     // > 5 min: idle
    };
    
    // 5. Operations score (5% weight)
    let operations_score = {
        if connection_info.operations_count > 100 { 1.0 }
        else if connection_info.operations_count > 10 { 0.8 }
        else { 0.6 }
    };
    
    // Weighted final score
    let final_score = (connectivity_score * 0.4)
        + (latency_score * 0.25)
        + (age_score * 0.15)
        + (usage_score * 0.15)
        + (operations_score * 0.05);
    
    final_score.clamp(0.0, 1.0)
}
```

### Health Monitor Background Task

```rust
pub fn start_health_monitor(&self) -> tokio::task::JoinHandle<()> {
    let pool = Arc::clone(&self.connection_pool);
    let health_monitor = Arc::clone(&self.health_monitor);
    let event_sender = self.event_sender.clone();
    let check_interval = Duration::from_secs(self.config.health_check_interval_secs);
    
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(check_interval);
        
        loop {
            interval.tick().await;
            
            // Check all connections
            for (peer_id, connections) in pool.read().await.iter() {
                for conn in connections {
                    let health_score = perform_health_check(&conn.info).await;
                    
                    // Update health metrics
                    let mut monitor = health_monitor.write().await;
                    monitor.peer_health.insert(*peer_id, PeerHealthMetrics {
                        current_latency_ms: conn.info.avg_latency_ms,
                        packet_loss_rate: 0.02,
                        throughput_bps: conn.info.bandwidth_bps,
                        uptime_secs: conn.info.connected_at.elapsed().as_secs(),
                        health_score,
                        last_measured: Instant::now(),
                    });
                    
                    // Mark unhealthy if score < 0.5
                    if health_score < 0.5 {
                        monitor.unhealthy_peers.insert(*peer_id, Instant::now());
                        event_sender.send(ConnectionEvent::Degraded {
                            peer_id: *peer_id,
                            health_score,
                        });
                    } else if monitor.unhealthy_peers.contains_key(peer_id) {
                        monitor.unhealthy_peers.remove(peer_id);
                        event_sender.send(ConnectionEvent::Recovered {
                            peer_id: *peer_id
                        });
                    }
                }
            }
        }
    })
}
```

### PeerHealthMetrics

```rust
pub struct PeerHealthMetrics {
    /// Current latency (ms)
    pub current_latency_ms: f64,
    
    /// Packet loss rate (0.0-1.0)
    pub packet_loss_rate: f64,
    
    /// Throughput (bytes/s)
    pub throughput_bps: u64,
    
    /// Uptime (seconds)
    pub uptime_secs: u64,
    
    /// Health score (0.0-1.0)
    pub health_score: f64,
    
    /// Last measurement timestamp
    pub last_measured: Instant,
}
```

## Connection Status

### ConnectionStatus

```rust
pub enum ConnectionStatus {
    /// Connected and healthy
    Healthy,
    
    /// Connected but degraded performance
    Degraded,
    
    /// Temporarily unavailable
    Unavailable,
    
    /// Disconnected
    Disconnected,
    
    /// Connection failed
    Failed,
}
```

## Event System

### ConnectionEvent

```rust
pub enum ConnectionEvent {
    /// New connection established
    Connected { peer_id: PeerId, latency_ms: f64 },
    
    /// Connection lost
    Disconnected { peer_id: PeerId, reason: String },
    
    /// Connection degraded
    Degraded { peer_id: PeerId, health_score: f64 },
    
    /// Connection recovered
    Recovered { peer_id: PeerId },
    
    /// Circuit breaker opened
    CircuitBreakerOpen { peer_id: PeerId },
    
    /// Circuit breaker closed
    CircuitBreakerClosed { peer_id: PeerId },
}
```

### Event Subscription

```rust
pub fn subscribe_events(&self) -> broadcast::Receiver<ConnectionEvent>
```

**Example:**
```rust
let mut event_stream = pool.subscribe_events();

tokio::spawn(async move {
    while let Ok(event) = event_stream.recv().await {
        match event {
            ConnectionEvent::Connected { peer_id, latency_ms } => {
                println!("Connected to {} ({}ms)", peer_id, latency_ms);
            },
            ConnectionEvent::CircuitBreakerOpen { peer_id } => {
                warn!("Circuit breaker opened for {}", peer_id);
            },
            ConnectionEvent::Degraded { peer_id, health_score } => {
                warn!("Connection degraded: {} (score: {:.2})", peer_id, health_score);
            },
            _ => {}
        }
    }
});
```

## Statistics

### PoolStats

```rust
pub struct PoolStats {
    /// Active connections count
    pub active_connections: u32,
    
    /// Pooled connections count
    pub pooled_connections: u32,
    
    /// Total connections created
    pub connections_created: u64,
    
    /// Total connections reused
    pub connections_reused: u64,
    
    /// Failed connections
    pub connections_failed: u64,
    
    /// Timed out connections
    pub connections_timeout: u64,
    
    /// Average connection time (ms)
    pub avg_connection_time_ms: f64,
    
    /// Reuse rate (0.0-1.0)
    pub reuse_rate: f64,
    
    /// Total bandwidth (bytes/s)
    pub total_bandwidth_bps: u64,
    
    /// Global average latency (ms)
    pub global_avg_latency_ms: f64,
}
```

### Getting Statistics

```rust
pub async fn get_stats(&self) -> PoolStats
```

**Example:**
```rust
let stats = pool.get_stats().await;
println!("Connection Pool Statistics:");
println!("  Active: {}", stats.active_connections);
println!("  Pooled: {}", stats.pooled_connections);
println!("  Created: {}", stats.connections_created);
println!("  Reused: {}", stats.connections_reused);
println!("  Reuse rate: {:.1}%", stats.reuse_rate * 100.0);
println!("  Failed: {}", stats.connections_failed);
println!("  Timeouts: {}", stats.connections_timeout);
```

### Reuse Rate Calculation

```rust
reuse_rate = connections_reused / (connections_created + connections_reused)
```

**Interpretation:**
- **>80%**: Excellent reuse efficiency
- **60-80%**: Good reuse efficiency
- **40-60%**: Moderate reuse efficiency
- **<40%**: Poor reuse efficiency

## API Reference

### Creating a Connection Pool

```rust
pub fn new(config: PoolConfig) -> Self
```

**Parameters:**
- `config`: Pool configuration

**Example:**
```rust
let config = PoolConfig::default();
let pool = OptimizedConnectionPool::new(config);
```

### Get Connection

```rust
pub async fn get_connection(&self, peer_id: PeerId, address: Multiaddr) -> Result<String>
```

**Parameters:**
- `peer_id`: Peer to connect to
- `address`: Peer's multiaddr

**Returns:**
- `String`: Unique connection ID

### Release Connection

```rust
pub async fn release_connection(&self, peer_id: PeerId, connection_id: String) -> Result<()>
```

**Parameters:**
- `peer_id`: Peer ID
- `connection_id`: Connection ID to release

### Start Health Monitor

```rust
pub fn start_health_monitor(&self) -> tokio::task::JoinHandle<()>
```

**Returns:**
- `JoinHandle` for background health monitoring task

### Get Statistics

```rust
pub async fn get_stats(&self) -> PoolStats
```

**Returns:**
- Current pool statistics

### Subscribe to Events

```rust
pub fn subscribe_events(&self) -> broadcast::Receiver<ConnectionEvent>
```

**Returns:**
- Event receiver for connection events

## Usage Examples

### Basic Usage

```rust
use guardian_db::ipfs_core_api::backends::connection_pool::*;

// Create pool
let config = PoolConfig::default();
let pool = OptimizedConnectionPool::new(config);

// Start health monitor
let monitor_handle = pool.start_health_monitor();

// Get connection
let peer_id: PeerId = "12D3KooW...".parse()?;
let address: Multiaddr = "/ip4/192.168.1.100/tcp/4001".parse()?;

let connection_id = pool.get_connection(peer_id, address).await?;

// Use connection for operations...

// Release connection
pool.release_connection(peer_id, connection_id).await?;
```

### Connection Reuse Example

```rust
let peer_id: PeerId = "12D3KooW...".parse()?;
let address: Multiaddr = "/ip4/192.168.1.100/tcp/4001".parse()?;

// First connection
let conn1 = pool.get_connection(peer_id, address.clone()).await?;
println!("First connection: {}", conn1);
pool.release_connection(peer_id, conn1).await?;

// Second connection (reuses first)
let conn2 = pool.get_connection(peer_id, address.clone()).await?;
println!("Second connection: {}", conn2);

// Check stats
let stats = pool.get_stats().await;
assert_eq!(stats.connections_created, 1);
assert_eq!(stats.connections_reused, 1);
```

### Event Monitoring

```rust
let mut events = pool.subscribe_events();

tokio::spawn(async move {
    while let Ok(event) = events.recv().await {
        match event {
            ConnectionEvent::Connected { peer_id, latency_ms } => {
                info!("New connection: {} ({}ms)", peer_id, latency_ms);
            },
            ConnectionEvent::Degraded { peer_id, health_score } => {
                warn!("Degraded: {} (score: {:.2})", peer_id, health_score);
            },
            ConnectionEvent::CircuitBreakerOpen { peer_id } => {
                error!("Circuit breaker opened for {}", peer_id);
            },
            ConnectionEvent::Recovered { peer_id } => {
                info!("Connection recovered: {}", peer_id);
            },
            _ => {}
        }
    }
});
```

### Custom Configuration

```rust
// High-throughput configuration
let high_throughput_config = PoolConfig {
    max_connections_per_peer: 16,
    max_total_connections: 2000,
    connection_timeout_ms: 15_000,
    idle_timeout_secs: 600,
    circuit_breaker_threshold: 0.3,  // More tolerant
    ..Default::default()
};

let pool = OptimizedConnectionPool::new(high_throughput_config);
```

## Performance Optimization

### Connection Reuse Strategy

**Benefits of reuse:**
- Eliminates connection establishment overhead
- Reduces handshake latency
- Saves network resources
- Improves throughput

**Optimization tips:**
- Increase `idle_timeout_secs` for better reuse
- Monitor `reuse_rate` in statistics
- Keep connections alive with periodic use

### Concurrency Control

The semaphore limits concurrent connections:

```rust
connection_semaphore: Arc::new(Semaphore::new(max_total_connections))
```

**Benefits:**
- Prevents connection exhaustion
- Controls resource usage
- Maintains consistent performance
- Avoids network congestion

### Circuit Breaker Benefits

**Protection against:**
- Cascading failures
- Resource waste on bad peers
- Network congestion
- Unresponsive peers

**Recovery features:**
- Automatic retry after timeout
- Gradual recovery (HalfOpen state)
- Success-based closing

## Best Practices

### 1. Always Release Connections

```rust
// Good: Use RAII pattern
let connection_id = pool.get_connection(peer_id, address).await?;
let result = perform_operation(&connection_id).await;
pool.release_connection(peer_id, connection_id).await?;

// Better: Use guard pattern
struct ConnectionGuard<'a> {
    pool: &'a OptimizedConnectionPool,
    peer_id: PeerId,
    connection_id: String,
}

impl Drop for ConnectionGuard<'_> {
    fn drop(&mut self) {
        // Auto-release on drop
    }
}
```

### 2. Monitor Health Regularly

```rust
// Start health monitor on initialization
let monitor_handle = pool.start_health_monitor();

// Periodically check unhealthy peers
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    loop {
        interval.tick().await;
        let monitor = pool.health_monitor.read().await;
        if !monitor.unhealthy_peers.is_empty() {
            warn!("Unhealthy peers: {}", monitor.unhealthy_peers.len());
        }
    }
});
```

### 3. Handle Circuit Breaker Events

```rust
let mut events = pool.subscribe_events();

tokio::spawn(async move {
    while let Ok(event) = events.recv().await {
        if let ConnectionEvent::CircuitBreakerOpen { peer_id } = event {
            // Take action: mark peer as unavailable, try alternative peers, etc.
            warn!("Circuit breaker opened for {}", peer_id);
        }
    }
});
```

### 4. Tune Configuration for Workload

```rust
// For low-latency applications
let low_latency_config = PoolConfig {
    max_batch_wait_ms: 10,
    connection_timeout_ms: 5_000,
    health_check_interval_secs: 10,
    ..Default::default()
};

// For high-throughput applications
let high_throughput_config = PoolConfig {
    max_connections_per_peer: 16,
    max_total_connections: 5000,
    idle_timeout_secs: 900,
    ..Default::default()
};
```

## Troubleshooting

### High Connection Failure Rate

**Problem:** `connections_failed` is high

```
Solutions:
1. Check network connectivity
2. Verify peer addresses are correct
3. Increase connection_timeout_ms
4. Check circuit breaker thresholds
5. Monitor peer health scores
```

### Low Reuse Rate

**Problem:** `reuse_rate < 0.4`

```
Solutions:
1. Increase idle_timeout_secs
2. Check if connections are being released
3. Verify health monitor isn't marking connections as unhealthy
4. Increase max_connections_per_peer
```

### Circuit Breakers Opening Frequently

**Problem:** Many `CircuitBreakerOpen` events

```
Solutions:
1. Increase circuit_breaker_threshold
2. Increase failure_threshold
3. Check peer reliability
4. Reduce timeout_ms for faster recovery
5. Investigate network issues
```

### Connection Timeouts

**Problem:** `connections_timeout` is high

```
Solutions:
1. Increase connection_timeout_ms
2. Check network latency
3. Verify peer availability
4. Use geographically closer peers
```

## Testing

### Unit Testing

```rust
#[tokio::test]
async fn test_connection_pool_reuse() {
    let pool = OptimizedConnectionPool::new(PoolConfig::default());
    let peer_id: PeerId = test_peer_id();
    let address: Multiaddr = test_multiaddr();
    
    // First connection
    let conn1 = pool.get_connection(peer_id, address.clone()).await.unwrap();
    pool.release_connection(peer_id, conn1).await.unwrap();
    
    // Second connection (should reuse)
    let conn2 = pool.get_connection(peer_id, address).await.unwrap();
    
    let stats = pool.get_stats().await;
    assert_eq!(stats.connections_created, 1);
    assert_eq!(stats.connections_reused, 1);
}
```

### Circuit Breaker Testing

```rust
#[tokio::test]
async fn test_circuit_breaker() {
    let config = PoolConfig {
        circuit_breaker_threshold: 0.3,  // 3 failures
        ..Default::default()
    };
    let pool = OptimizedConnectionPool::new(config);
    
    let peer_id: PeerId = test_peer_id();
    
    // Simulate failures
    for _ in 0..3 {
        pool.record_failure(peer_id).await;
    }
    
    // Circuit breaker should be open
    let can_connect = pool.check_circuit_breaker(peer_id).await.unwrap();
    assert_eq!(can_connect, false);
}
```

## Performance Metrics

### Key Performance Indicators

- **Reuse Rate**: Higher is better (target: >70%)
- **Average Connection Time**: Lower is better (target: <100ms)
- **Failure Rate**: Lower is better (target: <5%)
- **Health Score**: Higher is better (target: >0.7)

### Monitoring Example

```rust
// Periodic monitoring
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    
    loop {
        interval.tick().await;
        
        let stats = pool.get_stats().await;
        
        println!("Connection Pool Metrics:");
        println!("  Reuse rate: {:.1}%", stats.reuse_rate * 100.0);
        println!("  Avg connection time: {:.2}ms", stats.avg_connection_time_ms);
        println!("  Failure rate: {:.1}%", 
            stats.connections_failed as f64 / stats.connections_created as f64 * 100.0
        );
        
        if stats.reuse_rate < 0.5 {
            warn!("Low reuse rate detected!");
        }
    }
});
```

## Changelog

See the main Guardian DB changelog for version history.

## License

This module is part of GuardianDB and follows the same licensing terms.

## Contributing

Contributions are welcome! Please follow the project's contribution guidelines.

## References

- [Iroh Backend Documentation](IROH-BACKEND.md)
- [Circuit Breaker Pattern](https://martinfowler.com/bliki/CircuitBreaker.html)
- [Connection Pooling Best Practices](https://www.nginx.com/blog/http-keepalives-and-web-performance/)
- [libp2p Multiaddr](https://docs.libp2p.io/concepts/addressing/)
