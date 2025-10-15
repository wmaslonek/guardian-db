# Hybrid Backend - Iroh + LibP2P Documentation

## Overview

The Hybrid Backend combines the best of both worlds: **Iroh** for high-performance IPFS content operations and **LibP2P** for robust P2P communication and pub/sub messaging. This backend provides a unified interface that seamlessly integrates both systems while maintaining PeerId synchronization and cross-system connectivity.

## Table of Contents

- [Architecture](#architecture)
- [Core Components](#core-components)
- [Key Synchronization](#key-synchronization)
- [API Reference](#api-reference)
- [Peer Management](#peer-management)
- [Discovery System](#discovery-system)
- [Health Monitoring](#health-monitoring)
- [Usage Examples](#usage-examples)
- [Best Practices](#best-practices)

## Architecture

### High-Level Architecture

```
┌──────────────────────────────────────────────────────────┐
│                   HybridBackend                          │
├──────────────────────────────────────────────────────────┤
│  ┌────────────────────┐      ┌────────────────────┐    │
│  │   IrohBackend      │      │   LibP2PSwarm      │    │
│  │                    │      │                    │    │
│  │  - Content Ops     │      │  - P2P Networking  │    │
│  │  - Add/Cat/Pin     │      │  - PubSub/Gossip   │    │
│  │  - Block Ops       │      │  - DHT Discovery   │    │
│  │  - Caching         │      │  - Peer Connect    │    │
│  └────────────────────┘      └────────────────────┘    │
│           │                          │                  │
│           └──────────┬───────────────┘                  │
│                      │                                  │
│           ┌──────────▼──────────┐                      │
│           │  KeySynchronizer    │                      │
│           │  (Unified PeerId)   │                      │
│           └─────────────────────┘                      │
└──────────────────────────────────────────────────────────┘
```

### Component Responsibilities

**Iroh Backend:**
- Content storage and retrieval (add, cat)
- Pin management (pin_add, pin_rm, pin_ls)
- Block operations (block_get, block_put, block_stat)
- Repository statistics
- Content caching and optimization

**LibP2P Swarm:**
- Peer-to-peer networking
- Topic-based pub/sub messaging
- DHT peer discovery
- Connection management
- Gossipsub protocol support

**Key Synchronizer:**
- Maintains unified PeerId across both systems
- Ensures cryptographic key consistency
- Provides synchronization statistics
- Handles key derivation and conversion

## Core Components

### 1. HybridBackend

Main backend structure that orchestrates both Iroh and LibP2P:

```rust
pub struct HybridBackend {
    /// Iroh backend for content operations
    iroh: IrohBackend,
    
    /// LibP2P swarm for P2P communication
    libp2p_swarm: Arc<LibP2PSwarm>,
    
    /// Key synchronizer for unified identity
    key_sync: KeySynchronizer,
    
    /// Backend configuration
    config: ClientConfig,
    
    /// Combined metrics from both systems
    metrics: Arc<RwLock<BackendMetrics>>,
}
```

### 2. LibP2PSwarm

Dedicated LibP2P swarm for pub/sub operations:

```rust
pub struct LibP2PSwarm {
    /// PeerId synchronized with Iroh
    peer_id: PeerId,
    
    /// Online connectivity status
    is_online: Arc<RwLock<bool>>,
    
    /// List of connected peers
    connected_peers: Arc<RwLock<Vec<PeerInfo>>>,
    
    /// SwarmManager for DHT and discovery
    swarm_manager: Option<Arc<SwarmManager>>,
}
```

## Key Synchronization

### PeerId Synchronization

The backend ensures all three components use the same PeerId:

```rust
async fn verify_peer_id_sync(&self) -> Result<()> {
    let iroh_id = self.iroh.id().await?.id;
    let libp2p_id = self.libp2p_swarm.peer_id();
    let sync_id = self.key_sync.peer_id();
    
    if iroh_id != sync_id || libp2p_id != sync_id {
        return Err(GuardianError::Other(
            format!("PeerID desincronizado: Iroh={}, LibP2P={}, Sync={}",
                iroh_id, libp2p_id, sync_id)
        ));
    }
    
    Ok(())
}
```

**Synchronization Features:**
- Automatic verification on initialization
- Cryptographic key consistency
- Statistics tracking
- Error detection and reporting

### Initialization Flow

```rust
pub async fn new(config: &ClientConfig) -> Result<Self> {
    // 1. Initialize key synchronizer
    let key_sync = KeySynchronizer::new(config).await?;
    
    // 2. Initialize Iroh backend
    let iroh = IrohBackend::new(config).await?;
    
    // 3. Initialize LibP2P swarm with same keys
    let libp2p_swarm = LibP2PSwarm::new(&key_sync, config).await?;
    
    // 4. Create hybrid backend
    let backend = Self {
        iroh,
        libp2p_swarm: Arc::new(libp2p_swarm),
        key_sync,
        config: config.clone(),
        metrics: Arc::new(RwLock::new(BackendMetrics::default())),
    };
    
    // 5. Verify PeerId synchronization
    backend.verify_peer_id_sync().await?;
    
    Ok(backend)
}
```

## API Reference

### Content Operations

All content operations are delegated to the Iroh backend:

#### Add Content

```rust
async fn add(&self, data: Pin<Box<dyn AsyncRead + Send>>) -> Result<AddResponse>
```

**Delegation:** → `IrohBackend::add()`

#### Retrieve Content

```rust
async fn cat(&self, cid: &str) -> Result<Pin<Box<dyn AsyncRead + Send>>>
```

**Delegation:** → `IrohBackend::cat()`

#### Pin Management

```rust
async fn pin_add(&self, cid: &str) -> Result<()>
async fn pin_rm(&self, cid: &str) -> Result<()>
async fn pin_ls(&self) -> Result<Vec<PinInfo>>
```

**Delegation:** → `IrohBackend::pin_*`

### Network Operations

Network operations use **both** Iroh and LibP2P for redundancy:

#### Connect to Peer

```rust
async fn connect(&self, peer: &PeerId) -> Result<()> {
    // Connects via both systems
    let libp2p_result = self.libp2p_swarm.connect(peer).await;
    let iroh_result = self.iroh.connect(peer).await;
    
    // Success if at least one succeeds
    if libp2p_result.is_ok() || iroh_result.is_ok() {
        Ok(())
    } else {
        Err(GuardianError::Other("Both systems failed to connect"))
    }
}
```

#### List Peers

```rust
async fn peers(&self) -> Result<Vec<PeerInfo>> {
    // 1. Sync peer lists between systems
    self.sync_peer_lists().await?;
    
    // 2. Get peers from both systems
    let libp2p_peers = self.libp2p_swarm.peers().await;
    let iroh_peers = self.iroh.peers().await?;
    
    // 3. Merge and deduplicate
    let mut all_peers = libp2p_peers;
    for iroh_peer in iroh_peers {
        // Merge addresses and protocols
        // Keep connected status from either system
    }
    
    Ok(all_peers)
}
```

#### Find Peer (DHT)

```rust
async fn dht_find_peer(&self, peer: &PeerId) -> Result<Vec<String>> {
    // Try both systems
    let libp2p_addrs = self.libp2p_swarm.find_peer(peer).await;
    let iroh_addrs = self.iroh.dht_find_peer(peer).await?;
    
    // Combine and deduplicate results
    let mut all_addrs = libp2p_addrs;
    all_addrs.extend(iroh_addrs);
    all_addrs.dedup();
    
    Ok(all_addrs)
}
```

### Block Operations

All block operations are delegated to Iroh:

```rust
async fn block_get(&self, cid: &Cid) -> Result<Vec<u8>>
async fn block_put(&self, data: Vec<u8>) -> Result<Cid>
async fn block_stat(&self, cid: &Cid) -> Result<BlockStats>
```

**Delegation:** → `IrohBackend::block_*`

### Repository Operations

```rust
async fn repo_stat(&self) -> Result<RepoStats>
async fn version(&self) -> Result<VersionInfo>
```

**Delegation:** → `IrohBackend::repo_stat()`

**Version Override:** Returns "hybrid-iroh+libp2p-0.1.0"

### Metadata Operations

#### Backend Type

```rust
fn backend_type(&self) -> BackendType {
    BackendType::Hybrid
}
```

#### Online Status

```rust
async fn is_online(&self) -> bool {
    // Online if at least one system is online
    let iroh_online = self.iroh.is_online().await;
    let libp2p_online = *self.libp2p_swarm.is_online.read().await;
    
    iroh_online || libp2p_online
}
```

#### Metrics

```rust
async fn metrics(&self) -> Result<BackendMetrics> {
    // Combines metrics from both backends
    self.update_combined_metrics().await?;
    Ok(self.metrics.read().await.clone())
}
```

## Peer Management

### Peer List Synchronization

Automatically synchronizes peer lists between Iroh and LibP2P:

```rust
async fn sync_peer_lists(&self) -> Result<()> {
    // Get peers from both systems
    let iroh_peers = self.iroh.peers().await?;
    let libp2p_peers = self.libp2p_swarm.peers().await;
    
    // Identify unique peers in each system
    let mut peers_to_connect_iroh = Vec::new();
    let mut peers_to_connect_libp2p = Vec::new();
    
    // Peers in LibP2P but not in Iroh
    for libp2p_peer in &libp2p_peers {
        if !iroh_peers.iter().any(|p| p.id == libp2p_peer.id) {
            peers_to_connect_iroh.push(libp2p_peer.id);
        }
    }
    
    // Peers in Iroh but not in LibP2P
    for iroh_peer in &iroh_peers {
        if !libp2p_peers.iter().any(|p| p.id == iroh_peer.id) {
            peers_to_connect_libp2p.push(iroh_peer.id);
        }
    }
    
    // Connect missing peers
    for peer in peers_to_connect_iroh {
        self.iroh.connect(&peer).await?;
    }
    
    for peer in peers_to_connect_libp2p {
        self.libp2p_swarm.connect(&peer).await?;
    }
    
    Ok(())
}
```

**Synchronization Benefits:**
- Maximizes peer connectivity
- Shares discovered peers between systems
- Reduces discovery overhead
- Improves network resilience

### Cross-System Connectivity Check

```rust
async fn check_cross_system_connectivity(&self) -> Result<usize> {
    // Get connected peers from both systems
    let iroh_peers = self.iroh.peers().await?
        .into_iter()
        .filter(|p| p.connected)
        .collect::<Vec<_>>();
    
    let libp2p_peers = self.libp2p_swarm.connected_peers.read().await
        .iter()
        .filter(|p| p.connected)
        .cloned()
        .collect::<Vec<_>>();
    
    // Count common peers (connected in both systems)
    let mut common_peers = 0;
    let mut all_peer_ids = HashSet::new();
    
    for peer in &iroh_peers {
        all_peer_ids.insert(peer.id);
    }
    for peer in &libp2p_peers {
        all_peer_ids.insert(peer.id);
    }
    
    for peer_id in all_peer_ids {
        let in_iroh = iroh_peers.iter().any(|p| p.id == peer_id);
        let in_libp2p = libp2p_peers.iter().any(|p| p.id == peer_id);
        
        if in_iroh && in_libp2p {
            common_peers += 1;
        }
    }
    
    // Healthy if > 80% of peers are synchronized
    let connectivity_ratio = common_peers as f64 / all_peer_ids.len() as f64;
    
    if connectivity_ratio >= 0.8 {
        Ok(common_peers)
    } else {
        Err(GuardianError::Other(
            format!("Low connectivity: {:.1}%", connectivity_ratio * 100.0)
        ))
    }
}
```

**Connectivity Metrics:**
- Peers in both systems (ideal)
- Peers only in Iroh
- Peers only in LibP2P
- Connectivity ratio (%)

## Discovery System

### Multi-System Discovery

The hybrid backend performs discovery across both Iroh and LibP2P:

```rust
async fn dht_find_peer(&self, peer: &PeerId) -> Result<Vec<String>> {
    // Try both systems
    let libp2p_addrs = self.libp2p_swarm.find_peer(peer).await;
    let iroh_addrs = self.iroh.dht_find_peer(peer).await?;
    
    // Combine results
    let mut all_addrs = libp2p_addrs;
    all_addrs.extend(iroh_addrs);
    all_addrs.dedup();
    
    Ok(all_addrs)
}
```

### LibP2P Discovery Process

```rust
async fn find_peer(&self, peer: &PeerId) -> Vec<String> {
    // 1. Check connected peers first
    if let Some(peer_info) = connected_peers.find(peer) {
        return peer_info.addresses;
    }
    
    // 2. Use SwarmManager DHT lookup
    if let Some(swarm_manager) = self.swarm_manager {
        if let Ok(addresses) = self.perform_dht_lookup(swarm_manager, peer).await {
            return addresses;
        }
    }
    
    // 3. Fallback to network search
    self.discover_peer_via_network_search(peer).await
}
```

### Iroh-Backed DHT Lookup

The LibP2P swarm delegates DHT lookups to Iroh's discovery system:

```rust
async fn perform_dht_lookup(
    &self,
    swarm_manager: &Arc<SwarmManager>,
    peer: &PeerId,
) -> Result<Vec<String>> {
    // Query Iroh backend directly
    match self.query_iroh_backend_directly(peer).await {
        Ok(addresses) if !addresses.is_empty() => Ok(addresses),
        _ => self.query_connected_peers_for_addresses(peer).await,
    }
}
```

### Network Search Discovery

Generates probable addresses based on common network patterns:

```rust
async fn discover_peer_via_network_search(&self, peer: &PeerId) -> Vec<String> {
    let mut discovered_addresses = Vec::new();
    
    // Localhost addresses
    discovered_addresses.extend([
        format!("/ip4/127.0.0.1/tcp/4001/p2p/{}", peer),
        format!("/ip6/::1/tcp/4001/p2p/{}", peer),
    ]);
    
    // Local network addresses
    for subnet in ["192.168.1", "192.168.0", "10.0.0", "172.16.0"] {
        for host in [1, 100, 101, 254] {
            discovered_addresses.push(
                format!("/ip4/{}.{}/tcp/4001/p2p/{}", subnet, host, peer)
            );
        }
    }
    
    // Limit to prevent overhead
    discovered_addresses.truncate(10);
    
    discovered_addresses
}
```

### Iroh Endpoint Lookup

Direct lookup using Iroh's discovery mechanisms:

```rust
async fn execute_iroh_peer_lookup(
    &self,
    peer: &PeerId,
    endpoint: &iroh::Endpoint,
) -> Result<Vec<String>> {
    // 1. Convert PeerId to NodeId
    let node_id = peer_to_node_id(peer)?;
    
    // 2. Check if local node
    if endpoint.node_id() == node_id {
        return Ok(vec![
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{}", peer),
            format!("/ip6/::1/tcp/4001/p2p/{}", peer),
        ]);
    }
    
    // 3. Discover via Iroh network
    let addresses = self.discover_peer_via_iroh_network(node_id, endpoint).await?;
    
    Ok(addresses)
}
```

### Probable Address Generation (Iroh)

```rust
async fn discover_peer_via_iroh_network(
    &self,
    node_id: iroh::NodeId,
    _endpoint: &iroh::Endpoint,
) -> Result<Vec<String>> {
    let mut network_addresses = Vec::new();
    
    // Common local subnets
    let local_subnets = ["192.168.1", "192.168.0", "10.0.0", "172.16.0"];
    
    // Common Iroh ports
    let common_ports = [4001, 9090, 11204];
    
    for subnet in &local_subnets {
        for host in [1, 100, 254] {
            for port in &common_ports {
                network_addresses.push(
                    format!("/ip4/{}.{}/tcp/{}/p2p/{}", subnet, host, port, node_id)
                );
            }
        }
    }
    
    // Limit to 12 addresses
    network_addresses.truncate(12);
    
    Ok(network_addresses)
}
```

## Connection Management

### Dual-System Connection

```rust
async fn connect(&self, peer: &PeerId) -> Result<()> {
    // Attempt connection via both systems
    let libp2p_result = self.libp2p_swarm.connect(peer).await;
    let iroh_result = self.iroh.connect(peer).await;
    
    // Success if either succeeds
    match (libp2p_result, iroh_result) {
        (Ok(_), _) | (_, Ok(_)) => Ok(()),
        (Err(e1), Err(e2)) => Err(GuardianError::Other(
            format!("Both failed: LibP2P={}, Iroh={}", e1, e2)
        ))
    }
}
```

### Direct Multiaddr Connection

```rust
async fn attempt_direct_multiaddr_connection(
    &self,
    multiaddr: &libp2p::Multiaddr,
) -> Result<()> {
    use libp2p::multiaddr::Protocol;
    
    // Extract components
    let mut ip_addr = None;
    let mut port = None;
    let mut peer_id = None;
    
    for component in multiaddr.iter() {
        match component {
            Protocol::Ip4(addr) => ip_addr = Some(addr.to_string()),
            Protocol::Ip6(addr) => ip_addr = Some(addr.to_string()),
            Protocol::Tcp(p) => port = Some(p),
            Protocol::P2p(p) => peer_id = Some(PeerId::from_bytes(&p.to_bytes())?),
            _ => {}
        }
    }
    
    // Attempt TCP connection
    if let (Some(addr), Some(port_num)) = (ip_addr, port) {
        let mut stream = tokio::net::TcpStream::connect(
            format!("{}:{}", addr, port_num)
        ).await?;
        stream.shutdown().await?;
        
        Ok(())
    } else {
        Err(GuardianError::Other("Invalid multiaddr"))
    }
}
```

### Connection Fallback Strategy

```rust
async fn fallback_to_direct_addresses(&self, addresses: &[String]) -> Result<()> {
    if addresses.is_empty() {
        return Err(GuardianError::Other("No addresses available"));
    }
    
    let mut connection_errors = Vec::new();
    
    for address in addresses {
        if let Ok(multiaddr) = address.parse::<libp2p::Multiaddr>() {
            match self.attempt_direct_multiaddr_connection(&multiaddr).await {
                Ok(()) => return Ok(()),
                Err(e) => connection_errors.push(format!("{}: {}", address, e)),
            }
        }
    }
    
    Err(GuardianError::Other(
        format!("All connections failed: {:?}", connection_errors)
    ))
}
```

## Health Monitoring

### Comprehensive Health Check

```rust
async fn health_check(&self) -> Result<HealthStatus> {
    let start = Instant::now();
    let mut checks = Vec::new();
    let mut healthy = true;
    
    // Check 1: Iroh backend
    match self.iroh.health_check().await {
        Ok(iroh_health) => {
            checks.push(HealthCheck {
                name: "iroh_backend".to_string(),
                passed: iroh_health.healthy,
                message: format!("Iroh: {}", iroh_health.message),
            });
            if !iroh_health.healthy {
                healthy = false;
            }
        },
        Err(e) => {
            checks.push(HealthCheck {
                name: "iroh_backend".to_string(),
                passed: false,
                message: format!("Iroh error: {}", e),
            });
            healthy = false;
        }
    }
    
    // Check 2: LibP2P swarm
    let libp2p_health = self.libp2p_swarm.health_check().await;
    checks.push(HealthCheck {
        name: "libp2p_swarm".to_string(),
        passed: libp2p_health.healthy,
        message: libp2p_health.message,
    });
    if !libp2p_health.healthy {
        healthy = false;
    }
    
    // Check 3: PeerId synchronization
    let sync_check = self.verify_peer_id_sync().await;
    checks.push(HealthCheck {
        name: "peer_id_sync".to_string(),
        passed: sync_check.is_ok(),
        message: if sync_check.is_ok() {
            "PeerIDs synchronized".to_string()
        } else {
            format!("Sync error: {}", sync_check.unwrap_err())
        },
    });
    if sync_check.is_err() {
        healthy = false;
    }
    
    // Check 4: Cross-system connectivity
    let connectivity_check = self.check_cross_system_connectivity().await;
    checks.push(HealthCheck {
        name: "cross_system_connectivity".to_string(),
        passed: connectivity_check.is_ok(),
        message: if let Ok(common_peers) = connectivity_check {
            format!("{} peers connected in both systems", common_peers)
        } else {
            "Low cross-system connectivity".to_string()
        },
    });
    
    Ok(HealthStatus {
        healthy,
        message: if healthy {
            "Hybrid backend operational".to_string()
        } else {
            "Hybrid backend has issues".to_string()
        },
        response_time_ms: start.elapsed().as_millis() as u64,
        checks,
    })
}
```

### Combined Metrics

```rust
async fn update_combined_metrics(&self) -> Result<()> {
    let iroh_metrics = self.iroh.metrics().await?;
    let libp2p_metrics = self.libp2p_swarm.metrics().await;
    
    let mut combined = self.metrics.write().await;
    
    // Combine metrics from both backends
    combined.total_operations = iroh_metrics.total_operations;
    combined.error_count = iroh_metrics.error_count;
    combined.avg_latency_ms = 
        (iroh_metrics.avg_latency_ms + libp2p_metrics.avg_latency_ms) / 2.0;
    combined.memory_usage_bytes = 
        iroh_metrics.memory_usage_bytes + libp2p_metrics.memory_usage_bytes;
    
    Ok(())
}
```

## Usage Examples

### Basic Setup

```rust
use guardian_db::ipfs_core_api::backends::HybridBackend;
use guardian_db::ipfs_core_api::config::ClientConfig;

// Create configuration
let config = ClientConfig {
    data_store_path: Some(PathBuf::from("/data/guardian")),
    ..Default::default()
};

// Initialize hybrid backend
let backend = HybridBackend::new(&config).await?;
```

### Content Operations

```rust
// Add content (via Iroh)
let data = Box::pin(std::io::Cursor::new(b"Hello, Hybrid!"));
let response = backend.add(data).await?;
println!("CID: {}", response.hash);

// Retrieve content (via Iroh)
let mut reader = backend.cat(&response.hash).await?;
let mut content = Vec::new();
reader.read_to_end(&mut content).await?;

// Pin content (via Iroh)
backend.pin_add(&response.hash).await?;
```

### Peer Operations

```rust
// Connect to peer (via both systems)
let peer_id = "12D3KooW...".parse()?;
backend.connect(&peer_id).await?;

// List all peers (merged from both systems)
let peers = backend.peers().await?;
for peer in peers {
    println!("Peer: {} (connected: {})", peer.id, peer.connected);
}

// Find peer addresses (via both systems)
let addresses = backend.dht_find_peer(&peer_id).await?;
println!("Found {} addresses", addresses.len());
```

### Health Monitoring

```rust
// Check backend health
let health = backend.health_check().await?;
println!("Status: {}", health.message);
println!("Healthy: {}", health.healthy);

for check in health.checks {
    println!("  {}: {} - {}", 
        check.name,
        if check.passed { "✓" } else { "✗" },
        check.message
    );
}
```

### Metrics Monitoring

```rust
// Get combined metrics
let metrics = backend.metrics().await?;
println!("Total operations: {}", metrics.total_operations);
println!("Average latency: {:.2}ms", metrics.avg_latency_ms);
println!("Memory usage: {} bytes", metrics.memory_usage_bytes);
println!("Ops/second: {:.2}", metrics.ops_per_second);
```

## Best Practices

### 1. Always Verify Synchronization

```rust
// On initialization
backend.verify_peer_id_sync().await?;

// Periodically check
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(300));
    loop {
        interval.tick().await;
        if let Err(e) = backend.verify_peer_id_sync().await {
            warn!("PeerId sync error: {}", e);
        }
    }
});
```

### 2. Monitor Cross-System Connectivity

```rust
// Check connectivity regularly
let connectivity = backend.check_cross_system_connectivity().await?;
if connectivity < expected_minimum {
    warn!("Low connectivity detected");
    backend.sync_peer_lists().await?;
}
```

### 3. Use Appropriate Backend for Operations

- **Content operations** (add, cat, pin): Automatically uses Iroh
- **Peer discovery**: Uses both systems for maximum coverage
- **Network operations**: Dual-system for redundancy

### 4. Handle Partial Failures

```rust
// Connection example
match backend.connect(&peer).await {
    Ok(()) => {
        // Connection succeeded in at least one system
        info!("Connected to peer {}", peer);
    },
    Err(e) => {
        // Both systems failed
        warn!("Failed to connect: {}", e);
    }
}
```

## Performance Considerations

### Delegation Strategy

- **Content operations**: Direct delegation to Iroh (no overhead)
- **Network operations**: Parallel execution when beneficial
- **Discovery operations**: Combined results from both systems

### Memory Usage

```rust
// Estimated memory per component:
// - Iroh backend: Variable (depends on cache size)
// - LibP2P swarm: ~50KB per connected peer
// - Key synchronizer: <1KB
// - Hybrid overhead: <10KB
```

### Latency Impact

- **Content operations**: Same as Iroh (low)
- **Peer listing**: Small overhead for merging (~5-10ms)
- **Discovery**: Potentially better (parallel lookups)
- **Connection**: Small overhead for dual-system attempts

## Troubleshooting

### PeerId Synchronization Issues

**Problem**: PeerIDs are not synchronized

```
Solution:
1. Check KeySynchronizer initialization
2. Verify key files are not corrupted
3. Check key_sync statistics
4. Reinitialize backend if needed
```

### Connectivity Issues

**Problem**: Low cross-system connectivity

```
Solution:
1. Run sync_peer_lists() manually
2. Check network connectivity
3. Verify both backends are online
4. Check firewall settings
```

### Discovery Failures

**Problem**: Peers not found via discovery

```
Solution:
1. Try both systems independently
2. Check network configuration
3. Verify bootstrap nodes
4. Use network search fallback
```

### Performance Issues

**Problem**: High latency in peer operations

```
Solution:
1. Check if both backends are online
2. Monitor individual backend metrics
3. Consider using single backend for specific ops
4. Optimize network configuration
```

## Advanced Topics

### PeerId/NodeId Conversion

The hybrid backend handles conversions between libp2p PeerId and Iroh NodeId:

```rust
// PeerId to NodeId (for Iroh operations)
let peer_bytes = peer.to_bytes();
let mut node_id_bytes = [0u8; 32];
let copy_len = std::cmp::min(peer_bytes.len(), 32);
node_id_bytes[..copy_len].copy_from_slice(&peer_bytes[..copy_len]);
let node_id = iroh::NodeId::from_bytes(&node_id_bytes)?;
```

### Temporary Endpoint Creation

For discovery operations, temporary Iroh endpoints are created:

```rust
let secret_key = SecretKey::from_bytes(&[1u8; 32]);
let endpoint = Endpoint::builder()
    .secret_key(secret_key)
    .bind()
    .await?;

// Use for discovery...

endpoint.close().await;  // Always close when done
```

### Multiaddr Parsing

```rust
use libp2p::multiaddr::Protocol;

let mut ip_addr = None;
let mut port = None;
let mut peer_id = None;

for component in multiaddr.iter() {
    match component {
        Protocol::Ip4(addr) => ip_addr = Some(addr),
        Protocol::Ip6(addr) => ip_addr = Some(addr),
        Protocol::Tcp(p) => port = Some(p),
        Protocol::P2p(p) => peer_id = Some(PeerId::from_bytes(&p.to_bytes())?),
        _ => {}
    }
}
```

## Advantages of Hybrid Backend

### 1. Best of Both Worlds

- **Iroh**: High-performance content operations with native Rust
- **LibP2P**: Battle-tested P2P networking and pub/sub

### 2. Redundancy

- Dual discovery systems increase success rate
- Fallback mechanisms when one system fails
- Higher availability and reliability

### 3. Compatibility

- IPFS-compatible content operations
- Standard libp2p networking
- Works with existing IPFS and libp2p tools

### 4. Performance

- Optimized content operations via Iroh
- Efficient P2P communication via libp2p
- Combined caching from both systems

## Limitations

### 1. Increased Complexity

- Two systems to manage and monitor
- Synchronization overhead
- More failure points

### 2. Memory Overhead

- Running both Iroh and LibP2P concurrently
- Duplicate peer tracking
- Additional synchronization structures

### 3. Configuration Complexity

- Need to configure both backends
- Manage two sets of network ports
- Coordinate discovery settings

## When to Use Hybrid Backend

### Use Cases

**Use hybrid backend when:**
- You need both IPFS content ops AND robust pub/sub
- High availability is critical
- You want redundant discovery mechanisms
- You need compatibility with both Iroh and libp2p ecosystems

**Don't use hybrid backend when:**
- You only need content operations (use Iroh)
- You only need pub/sub (use LibP2P)
- Resources are constrained
- Simplicity is priority

## License

This backend is part of GuardianDB and follows the same licensing terms.

## Contributing

Contributions are welcome! Please follow the project's contribution guidelines.

## References

- [Iroh Backend Documentation](IROH-BACKEND.md)
- [libp2p Documentation](https://docs.libp2p.io/)
- [Guardian DB Documentation](../README.md)
- [Key Synchronizer](KEY-SYNCHRONIZER.md)
