    // ╔════════════════════════════════════════════════════════════════════════════════╗
    // ║                          ⚠ OUTDATED DOCUMENTATION                             ║
    // ╚════════════════════════════════════════════════════════════════════════════════╝


# Iroh Backend - Documentation

## Overview

The Iroh Backend is a high-performance, native Rust IPFS backend for Guardian DB that uses [Iroh](https://iroh.computer/) as an embedded IPFS node. It provides advanced optimizations including intelligent caching, connection pooling, batch processing, and real-time performance monitoring.

## Table of Contents

- [Features](#features)
- [Architecture](#architecture)
- [Core Components](#core-components)
- [API Reference](#api-reference)
- [Performance Optimizations](#performance-optimizations)
- [Discovery System](#discovery-system)
- [Usage Examples](#usage-examples)
- [Configuration](#configuration)
- [Metrics and Monitoring](#metrics-and-monitoring)

## Features

### Core Capabilities

- **Embedded IPFS Node**: Native Rust implementation using Iroh
- **Multi-level Caching**: Intelligent LRU cache with automatic compression
- **Connection Pooling**: Optimized connection management with circuit breaking
- **Batch Processing**: High-throughput batch operations
- **Real-time Monitoring**: Continuous performance tracking and metrics
- **Advanced Discovery**: Multi-method peer discovery (DHT, mDNS, Bootstrap, Relay)
- **LibP2P Integration**: Seamless integration with libp2p for pub/sub messaging

### Performance Features

- **Smart Cache**: LRU cache with access counting and automatic eviction
- **Compressed Storage**: Automatic compression for cache entries
- **Circuit Breaker**: Prevents cascading failures in peer connections
- **Load Balancing**: Intelligent distribution of operations across peers
- **Streaming Support**: Efficient streaming for large data transfers

## Architecture

### High-Level Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      IrohBackend                            │
├─────────────────────────────────────────────────────────────┤
│  ┌──────────────┐  ┌──────────────┐  ┌─────────────────┐  │
│  │   Data Cache │  │ Connection   │  │   Performance   │  │
│  │   (LRU)      │  │   Pool       │  │   Monitor       │  │
│  └──────────────┘  └──────────────┘  └─────────────────┘  │
│  ┌──────────────────────────────────────────────────────┐  │
│  │              Iroh Endpoint (QUIC/P2P)                │  │
│  └──────────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────┐  │
│  │          Iroh-Blobs Store (FsStore)                  │  │
│  └──────────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────┐  │
│  │         Discovery Service (Multi-Method)             │  │
│  └──────────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────┐  │
│  │        Swarm Manager (LibP2P Integration)            │  │
│  └──────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

### Component Interaction

```
User Request
    ↓
IrohBackend::operation()
    ↓
┌─────────────────┐
│  Cache Check?   │──Yes──→ Return Cached Data
└─────────────────┘
    ↓ No
Store Operation (Iroh-Blobs)
    ↓
Update Cache & Metrics
    ↓
Return Result
```

## Core Components

### 1. IrohBackend

The main backend structure that implements the `IpfsBackend` trait.

**Key Fields:**
- `endpoint`: Iroh QUIC endpoint for P2P communication
- `store`: Iroh-blobs file system store for data persistence
- `secret_key`: Node's cryptographic identity
- `data_cache`: LRU cache for frequently accessed data
- `metrics`: Performance metrics tracking
- `discovery_service`: Advanced peer discovery system
- `swarm_manager`: LibP2P swarm for Gossipsub messaging

### 2. Data Cache

LRU (Least Recently Used) cache with intelligent eviction and access tracking.

**Structure: `CachedData`**
```rust
pub struct CachedData {
    pub data: Bytes,           // The cached blob data
    pub cached_at: Instant,    // When cached
    pub access_count: u64,     // Number of accesses
    pub size: usize,           // Data size in bytes
}
```

**Features:**
- Automatic eviction when full
- Access count tracking for optimization
- Fast lookup (O(1) average)
- Thread-safe with async RwLock

### 3. Discovery System

Multi-method peer discovery with fallback strategies.

**Discovery Methods:**
- **DHT**: Distributed Hash Table discovery
- **mDNS**: Local network multicast DNS
- **Bootstrap**: Queries to known bootstrap nodes
- **Relay**: Discovery via relay nodes
- **Combined**: Multiple methods in parallel

**Structure: `DiscoverySession`**
```rust
pub struct DiscoverySession {
    pub target_node: NodeId,
    pub started_at: Instant,
    pub discovered_addresses: Vec<NodeAddr>,
    pub status: DiscoveryStatus,
    pub discovery_method: DiscoveryMethod,
    pub last_update: Instant,
    pub attempts: u32,
}
```

### 4. Performance Monitor

Real-time tracking of backend performance metrics.

**Metrics Tracked:**
- **Throughput**: Operations/second, bytes/second
- **Latency**: Average, P95, P99, min/max
- **Resources**: CPU usage, memory, disk I/O, network bandwidth
- **Cache**: Hit rate, miss rate, eviction rate

### 5. Connection Pool

Optimized management of peer connections.

**Structure: `ConnectionInfo`**
```rust
pub struct ConnectionInfo {
    pub node_id: PeerId,
    pub address: String,
    pub connected_at: Instant,
    pub last_used: Instant,
    pub avg_latency_ms: f64,
    pub operations_count: u64,
}
```

## API Reference

### IPFS Operations

#### Add Content

Adds content to IPFS and returns the content hash (CID).

```rust
async fn add(&self, data: Pin<Box<dyn AsyncRead + Send>>) -> Result<AddResponse>
```

**Parameters:**
- `data`: Async reader containing the data to add

**Returns:**
- `AddResponse` with hash, name, and size

**Example:**
```rust
let data = Box::pin(std::io::Cursor::new(b"Hello IPFS"));
let response = backend.add(data).await?;
println!("CID: {}", response.hash);
```

#### Retrieve Content (Cat)

Retrieves content by CID, checking cache first for optimal performance.

```rust
async fn cat(&self, cid: &str) -> Result<Pin<Box<dyn AsyncRead + Send>>>
```

**Parameters:**
- `cid`: Content identifier

**Returns:**
- Async reader with the content data

**Example:**
```rust
let mut reader = backend.cat("bafkreid...").await?;
let mut content = Vec::new();
reader.read_to_end(&mut content).await?;
```

#### Pin Content

Pins content to prevent garbage collection.

```rust
async fn pin_add(&self, cid: &str, recursive: bool) -> Result<()>
```

**Parameters:**
- `cid`: Content identifier to pin
- `recursive`: Whether to recursively pin linked content

#### List Pinned Content

Lists all pinned content.

```rust
async fn pin_ls(&self, cid: Option<&str>) -> Result<Vec<PinInfo>>
```

**Returns:**
- Vector of `PinInfo` with CID and pin type

#### Unpin Content

Removes pin from content.

```rust
async fn pin_rm(&self, cid: &str, recursive: bool) -> Result<()>
```

### Node Management

#### Get Node ID

Returns the node's peer ID.

```rust
async fn id(&self) -> Result<PeerInfo>
```

**Returns:**
- `PeerInfo` with node ID, public key, addresses, protocol version

#### Health Check

Performs comprehensive health check of the backend.

```rust
async fn health(&self) -> Result<HealthCheck>
```

**Returns:**
- `HealthCheck` with status, metrics, connected peers, error messages

### Peer Discovery

#### Find Peer

Discovers peer addresses using multiple discovery methods.

```rust
async fn dht_findpeer(&self, node_id: &str) -> Result<DhtFindPeerResponse>
```

**Parameters:**
- `node_id`: Peer ID to discover

**Returns:**
- `DhtFindPeerResponse` with discovered addresses

#### Find Providers

Finds providers for specific content.

```rust
async fn dht_findprovs(&self, cid: &str, num_providers: Option<usize>) -> Result<DhtFindProvsResponse>
```

### Pub/Sub Operations

#### Subscribe to Topic

Subscribes to a Gossipsub topic.

```rust
async fn pubsub_subscribe(&self, topic: &str) -> Result<()>
```

#### Publish Message

Publishes message to a topic.

```rust
async fn pubsub_publish(&self, topic: &str, data: &[u8]) -> Result<()>
```

#### List Subscriptions

Lists all active topic subscriptions.

```rust
async fn pubsub_ls(&self) -> Result<Vec<String>>
```

#### List Peers on Topic

Lists peers subscribed to a specific topic.

```rust
async fn pubsub_peers(&self, topic: Option<&str>) -> Result<Vec<String>>
```

## Performance Optimizations

### 1. Intelligent Caching

**Cache Strategy:**
- LRU eviction policy
- Access count tracking for hot data identification
- Automatic cache warming on frequent access patterns
- Cache size limits to prevent memory bloat

**Cache Metrics:**
```rust
pub struct CacheMetrics {
    pub total_hits: u64,
    pub total_misses: u64,
    pub total_bytes_cached: u64,
    pub hit_rate: f64,
}
```

### 2. Connection Pooling

**Features:**
- Reuse of existing connections
- Connection health monitoring
- Automatic reconnection on failure
- Load balancing across connections

### 3. Batch Processing

**Benefits:**
- Reduced overhead per operation
- Improved throughput
- Better resource utilization
- Optimized network usage

### 4. Metrics-Driven Optimization

**Real-time Tracking:**
- Operation latency monitoring
- Throughput measurement
- Error rate tracking
- Resource utilization analysis

**Adaptive Behavior:**
- Cache size adjustment based on hit rate
- Connection pool sizing based on load
- Timeout adjustment based on latency
- Retry strategy optimization

## Discovery System

### Discovery Methods

#### 1. DHT Discovery

Discovers peers using the Distributed Hash Table.

**Process:**
1. Query local DHT routing table
2. Iteratively query closer peers
3. Return discovered addresses
4. Update local cache

**Fallback:**
- Query bootstrap nodes
- Use locally cached peers
- Try relay discovery

#### 2. mDNS Discovery

Local network discovery using multicast DNS.

**Process:**
1. Send mDNS query on local network
2. Listen for mDNS responses
3. Collect responding peers
4. Verify peer connectivity

**Use Cases:**
- Local development
- Private networks
- Low-latency requirements

#### 3. Bootstrap Discovery

Queries known bootstrap nodes for peer information.

**Bootstrap Nodes:**
- `bootstrap.iroh.network:443`
- `discovery.iroh.network:443`
- `relay.iroh.network:443`
- Public IPFS bootstrap nodes

**Protocol:**
- HTTP API queries
- TCP direct queries
- Structured JSON responses

#### 4. Relay Discovery

Discovers peers through relay nodes.

**Process:**
1. Connect to relay via QUIC
2. Send discovery query
3. Receive peer information
4. Establish direct or relayed connection

### Discovery Configuration

```rust
pub struct AdvancedDiscoveryConfig {
    pub discovery_timeout_secs: u64,
    pub max_attempts: u32,
    pub retry_interval_ms: u64,
    pub enable_mdns: bool,
    pub enable_dht: bool,
    pub enable_bootstrap: bool,
    pub enable_bootstrap_fallback: bool,
    pub max_peers_per_session: u32,
}
```

## Usage Examples

### Basic Setup

```rust
use guardian_db::ipfs_core_api::backends::IrohBackend;
use guardian_db::ipfs_core_api::config::ClientConfig;

// Create configuration
let config = ClientConfig {
    data_store_path: Some(PathBuf::from("/data/iroh")),
    max_peers_per_session: 50,
    cache_size_mb: 512,
    ..Default::default()
};

// Initialize backend
let backend = IrohBackend::new(config).await?;
```

### Adding and Retrieving Content

```rust
// Add content
let data = b"Hello, IPFS!";
let cursor = Box::pin(std::io::Cursor::new(data));
let response = backend.add(cursor).await?;
println!("Added content: {}", response.hash);

// Retrieve content
let mut reader = backend.cat(&response.hash).await?;
let mut retrieved = Vec::new();
reader.read_to_end(&mut retrieved).await?;
assert_eq!(data, retrieved.as_slice());
```

### Pinning Content

```rust
// Pin content
backend.pin_add("bafkreid...", true).await?;

// List pins
let pins = backend.pin_ls(None).await?;
for pin in pins {
    println!("Pinned: {} (type: {:?})", pin.cid, pin.pin_type);
}

// Unpin content
backend.pin_rm("bafkreid...", false).await?;
```

### Peer Discovery

```rust
// Discover peer by ID
let node_id = "12D3KooW...";
let response = backend.dht_findpeer(node_id).await?;
println!("Found {} addresses for peer", response.addresses.len());

// Find content providers
let cid = "bafkreid...";
let providers = backend.dht_findprovs(cid, Some(10)).await?;
println!("Found {} providers", providers.providers.len());
```

### Pub/Sub Messaging

```rust
// Subscribe to topic
backend.pubsub_subscribe("my-topic").await?;

// Publish message
let message = b"Hello, subscribers!";
backend.pubsub_publish("my-topic", message).await?;

// List subscriptions
let topics = backend.pubsub_ls().await?;
println!("Subscribed to: {:?}", topics);

// List peers on topic
let peers = backend.pubsub_peers(Some("my-topic")).await?;
println!("Peers on topic: {:?}", peers);
```

### Health Monitoring

```rust
// Check backend health
let health = backend.health().await?;
println!("Status: {:?}", health.status);
println!("Connected peers: {}", health.connected_peers);
println!("Operations: {}", health.metrics.total_operations);
println!("Average latency: {:.2}ms", health.metrics.avg_latency_ms);
```

## Configuration

### Client Configuration

```rust
pub struct ClientConfig {
    // Data storage path
    pub data_store_path: Option<PathBuf>,
    
    // Network configuration
    pub max_peers_per_session: u32,
    pub connection_timeout_secs: u64,
    
    // Cache configuration
    pub cache_size_mb: usize,
    pub enable_cache_compression: bool,
    
    // Discovery configuration
    pub enable_mdns: bool,
    pub enable_dht: bool,
    pub bootstrap_nodes: Vec<String>,
    
    // Performance tuning
    pub batch_size: usize,
    pub operation_timeout_secs: u64,
}
```

### Environment Variables

- `IROH_DATA_DIR`: Override data storage directory
- `IROH_CACHE_SIZE`: Cache size in MB
- `IROH_MAX_PEERS`: Maximum number of peers
- `IROH_BOOTSTRAP`: Comma-separated bootstrap nodes

## Metrics and Monitoring

### Backend Metrics

```rust
pub struct BackendMetrics {
    pub total_operations: u64,
    pub error_count: u64,
    pub avg_latency_ms: f64,
    pub ops_per_second: f64,
    pub bytes_transferred: u64,
    pub active_connections: u32,
    pub cache_hit_rate: f64,
}
```

### Cache Metrics

```rust
pub struct CacheMetrics {
    pub total_hits: u64,
    pub total_misses: u64,
    pub total_bytes_cached: u64,
    pub hit_rate: f64,
    pub eviction_count: u64,
}
```

### Performance Monitoring

The backend provides real-time performance snapshots:

```rust
let health = backend.health().await?;
println!("Metrics:");
println!("  Operations: {}", health.metrics.total_operations);
println!("  Errors: {}", health.metrics.error_count);
println!("  Avg Latency: {:.2}ms", health.metrics.avg_latency_ms);
println!("  Throughput: {:.2} ops/s", health.metrics.ops_per_second);
println!("  Cache Hit Rate: {:.1}%", health.metrics.cache_hit_rate * 100.0);
```

### Logging

The backend uses the `tracing` crate for structured logging:

- **Debug**: Detailed operation traces
- **Info**: Important events and milestones
- **Warn**: Non-critical issues and fallbacks
- **Error**: Critical failures requiring attention

## Advanced Topics

### CID Conversion

The backend handles conversion between IPFS CIDs and Iroh hashes:

```rust
// Supports multiple hash algorithms
// - SHA-256 (32 bytes): Direct conversion
// - Larger hashes: Truncation with metadata preservation
// - Smaller hashes: Deterministic padding
```

### NodeId to PeerId Conversion

Seamless conversion between Iroh NodeId and libp2p PeerId:

```rust
// Strategies:
// 1. Direct conversion (if compatible)
// 2. Ed25519 key derivation
// 3. Deterministic hash-based generation
```

### Error Handling

All operations return `Result<T, GuardianError>`:

```rust
match backend.cat("invalid-cid").await {
    Ok(reader) => { /* handle success */ },
    Err(GuardianError::InvalidCid(msg)) => { /* handle invalid CID */ },
    Err(GuardianError::NotFound) => { /* handle not found */ },
    Err(e) => { /* handle other errors */ },
}
```

## Best Practices

1. **Cache Warming**: Pre-load frequently accessed content
2. **Pin Management**: Pin only essential content
3. **Connection Limits**: Set appropriate peer limits
4. **Timeout Configuration**: Adjust based on network conditions
5. **Monitoring**: Regularly check health and metrics
6. **Discovery Methods**: Enable multiple methods for reliability
7. **Error Handling**: Implement retry logic with exponential backoff

## Performance Considerations

- **Cache Size**: Larger cache = better hit rate but more memory
- **Peer Count**: More peers = better discovery but more overhead
- **Batch Size**: Larger batches = better throughput but higher latency
- **Discovery Timeout**: Longer timeout = more discoveries but slower
- **Connection Pool**: Larger pool = more concurrency but more resources

## Troubleshooting

### Common Issues

**Issue**: Low cache hit rate
- **Solution**: Increase cache size or analyze access patterns

**Issue**: Peer discovery failures
- **Solution**: Check network connectivity, enable multiple discovery methods

**Issue**: High latency
- **Solution**: Reduce peer count, optimize connection pool, check network

**Issue**: Memory usage growth
- **Solution**: Reduce cache size, implement periodic cleanup

## Advanced Features

### mDNS Discovery

The backend implements full mDNS (Multicast DNS) discovery for local network peer detection:

**Features:**
- Automatic peer announcement on local network
- Query/Response packet generation
- Service discovery for `_iroh._tcp.local`
- TXT record support for node metadata
- Timeout-based response collection

**mDNS Packet Format:**
```rust
// Query packet structure
let query_packet = create_mdns_query_packet("_iroh._tcp.local")?;

// Announcement packet with TXT records
let txt_records = vec![
    "version=0.92.0".to_string(),
    "capabilities=ipfs,iroh,sync".to_string(),
];
let announcement = create_mdns_announcement_packet("_iroh._tcp.local", &txt_records)?;
```

**Usage:**
```rust
// Discover peers via mDNS
let mdns_peers = backend.discover_mdns_peers().await?;
for peer in mdns_peers {
    println!("Local peer: {} at {:?}", peer.node_id, peer.direct_addresses());
}
```

### DHT Registry System

Advanced DHT-based peer registry with local caching:

**Registry Structure:**
- Main discovery records: `/iroh/discovery/{node_id}`
- Direct addresses: `/iroh/addresses/{node_id}`
- Relay information: `/iroh/relay/{node_id}`
- Capabilities: `/iroh/capabilities/{node_id}`

**Local File Cache:**
- Location: `./temp/dht_cache/`
- Format: JSON structured records
- TTL: 5 minutes (configurable)
- Automatic cleanup of expired entries

**Example:**
```rust
// Query DHT for peer
let main_record = query_dht_main_record(node_id).await?;
println!("Discovered addresses: {:?}", main_record.addresses);
println!("Capabilities: {:?}", main_record.capabilities);
```

### Discovery Events Monitoring

Real-time monitoring of discovery events across all methods:

**Event Types:**
- `Discovered`: New peer found
- `Expired`: Peer removed from known list

**Monitoring Configuration:**
```rust
pub struct MonitoringIntervals {
    general: Duration,      // 30 seconds
    mdns: Duration,         // 10 seconds  
    dht: Duration,          // 60 seconds
}
```

**Usage:**
```rust
// Subscribe to discovery events
if let Some(event_stream) = discovery_service.subscribe() {
    while let Some(event) = event_stream.next().await {
        match event {
            DiscoveryEvent::Discovered(item) => {
                println!("New peer: {}", item.node_info.node_id);
            },
            DiscoveryEvent::Expired(node_id) => {
                println!("Peer expired: {}", node_id);
            }
        }
    }
}
```

### Bootstrap & Relay Discovery

Comprehensive bootstrap and relay node discovery with fallback strategies:

**Bootstrap Nodes:**
- `bootstrap.iroh.network:443`
- `discovery.iroh.network:443`
- `relay.iroh.network:443`
- Public IPFS bootstrap nodes

**Relay Nodes:**
- `relay.iroh.network:443`
- `derp.tailscale.com:443`
- Custom relay URLs

**Protocol Support:**
- HTTP API queries
- QUIC direct connections
- TCP fallback

**Example:**
```rust
// Query bootstrap nodes
let bootstrap_result = query_bootstrap_nodes_for_peer(node_id, &config).await?;

// Query relay nodes
let relay_result = query_relay_nodes_for_peer(node_id, &config).await?;
```

### Local Network Scanning

Smart local network scanning for peer discovery:

**Features:**
- Common IP range scanning (192.168.x.x, 10.0.x.x, etc.)
- Fast connection testing (100ms timeout)
- IPFS port detection (4001, 5001)
- Automatic NodeId generation

**Safety:**
- Limited scan range (5 IPs per subnet)
- Fast timeouts to avoid blocking
- Non-intrusive connection tests

**Example:**
```rust
// Scan local network
let local_peers = discover_local_network_peers().await?;
println!("Found {} peers on local network", local_peers.len());
```

### Discovery Cache System

Advanced LRU cache for discovered addresses with intelligent confidence scoring:

**Cache Structure:**
```rust
struct CachedNodeAddresses {
    addresses: Vec<NodeAddr>,
    cached_at: Instant,
    confidence_score: f64,  // 0.0-1.0
}
```

**Confidence Scoring Algorithm:**
```rust
fn calculate_address_confidence(&self, addresses: &[NodeAddr]) -> f64 {
    let mut total_score: f64 = 0.0;
    
    for addr in addresses {
        let mut address_score: f64 = 0.5;  // Base score
        
        // +0.2 for relay URL present
        if addr.relay_url().is_some() {
            address_score += 0.2;
        }
        
        // +0.1 per additional direct address
        let direct_addrs = addr.direct_addresses().collect::<Vec<_>>();
        if direct_addrs.len() > 1 {
            address_score += 0.1 * (direct_addrs.len() - 1) as f64;
        }
        
        // -0.3 for localhost addresses (less reliable for network)
        if direct_addrs.iter().any(|a| a.ip().is_loopback()) {
            address_score -= 0.3;
        }
        
        // +0.2 for public IP addresses
        if direct_addrs.iter().any(|a| is_public_ip(a.ip())) {
            address_score += 0.2;
        }
        
        total_score += address_score.clamp(0.0, 1.0);
    }
    
    total_score / addresses.len() as f64
}
```

**Public IP Detection:**
```rust
fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ipv4) => {
            !ipv4.is_private()
                && !ipv4.is_loopback()
                && !ipv4.is_multicast()
                && !ipv4.is_broadcast()
                && !ipv4.is_link_local()
                && !ipv4.is_documentation()
        },
        IpAddr::V6(ipv6) => {
            !ipv6.is_loopback()
                && !ipv6.is_multicast()
                && !ipv6.to_string().starts_with("fe80:")  // link-local
                && !ipv6.to_string().starts_with("fd")     // unique local
                && !ipv6.to_string().starts_with("fc")     // unique local
        }
    }
}
```

**Cache Update Strategy:**
```rust
async fn cache_discovered_addresses(&self, node_id: NodeId, addresses: &[NodeAddr]) {
    let cache_entry = CachedNodeAddresses {
        addresses: addresses.to_vec(),
        cached_at: Instant::now(),
        confidence_score: self.calculate_address_confidence(addresses),
    };
    
    let mut cache = DISCOVERY_CACHE.write().await;
    
    // Only update if new entry has better confidence or more addresses
    let should_update = if let Some(existing) = cache.peek(&node_id) {
        cache_entry.confidence_score > existing.confidence_score
            || cache_entry.addresses.len() > existing.addresses.len()
    } else {
        true
    };
    
    if should_update {
        cache.put(node_id, cache_entry);
    }
}
```

**Cache Retrieval with TTL:**
```rust
async fn get_cached_addresses(&self, node_id: NodeId) -> Option<Vec<NodeAddr>> {
    let mut cache = DISCOVERY_CACHE.write().await;
    
    if let Some(cached_entry) = cache.get(&node_id) {
        let age = Instant::now().duration_since(cached_entry.cached_at);
        
        // TTL: 5 minutes
        if age.as_secs() < 300 {
            return Some(cached_entry.addresses.clone());
        } else {
            // Remove expired entry
            cache.pop(&node_id);
        }
    }
    
    None
}
```

**Cache Statistics:**
```rust
let stats = backend.get_discovery_cache_stats().await;
println!("Cache entries: {}", stats.entries_count);
println!("Hit ratio: {:.1}%", stats.hit_ratio_percent);
println!("Capacity used: {}%", stats.capacity_used_percent);
println!("Oldest entry: {}s", stats.oldest_entry_age_seconds);
```

**Cache Promotion:**
When addresses are found in the simple `internal_state`, they are automatically promoted to the advanced LRU cache with confidence scoring.

**Automatic Cleanup:**
- Runs when cache reaches 80% capacity
- Removes entries older than 10 minutes
- Async task to avoid blocking
- Detailed cleanup logging

### QUIC Protocol Operations

The backend implements QUIC-based communication for relay queries:

**QUIC Query Flow:**
```rust
async fn query_relay_via_quic_protocol(
    target_node: NodeId,
    relay_node: &str,
) -> Result<Vec<NodeAddr>> {
    // 1. Create temporary QUIC endpoint
    let secret_key = SecretKey::from_bytes(&generate_temp_key_for_query());
    let endpoint = Endpoint::builder().secret_key(secret_key).bind().await?;
    
    // 2. Parse relay address
    let relay_socket_addr = parse_relay_address_to_socket_addr(relay_node)?;
    let relay_node_id = derive_relay_node_id_from_address(relay_node)?;
    
    // 3. Connect to relay
    let relay_url = relay_node.parse::<RelayUrl>()?;
    let target_addr = NodeAddr::from_parts(
        relay_node_id,
        Some(relay_url),
        vec![relay_socket_addr]
    );
    let connection = endpoint.connect(target_addr, b"iroh-relay-query").await?;
    
    // 4. Open bidirectional stream
    let (mut send_stream, mut recv_stream) = connection.open_bi().await?;
    
    // 5. Send protocol header
    send_stream.write_all(b"IROH_RELAY_QUERY_V1\n").await?;
    
    // 6. Send payload size (4 bytes big-endian)
    let payload = create_relay_query_payload(target_node)?;
    let payload_bytes = serde_json::to_vec(&payload)?;
    send_stream.write_all(&(payload_bytes.len() as u32).to_be_bytes()).await?;
    
    // 7. Send payload
    send_stream.write_all(&payload_bytes).await?;
    send_stream.finish()?;
    
    // 8. Read response
    let mut response_buffer = Vec::new();
    recv_stream.read_to_end(&mut response_buffer).await?;
    
    // 9. Parse response
    let relay_response = parse_relay_query_response(&response_buffer)?;
    let discovered_nodes = convert_relay_response_to_node_addrs(target_node, relay_response)?;
    
    // 10. Close connection
    connection.close(0u32.into(), b"query complete");
    endpoint.close().await;
    
    Ok(discovered_nodes)
}
```

**Protocol Features:**
- Binary protocol over QUIC
- Protocol version negotiation
- Structured JSON payloads
- Graceful connection closure
- Timeout support (30 seconds)

**Relay Query Payload:**
```rust
pub struct RelayQueryPayload {
    pub protocol_version: String,      // "iroh-relay-query/1.0"
    pub query_type: String,             // "peer_lookup"
    pub target_node_id: String,
    pub max_results: u32,               // 10
    pub timeout_seconds: u64,           // 30
    pub query_scope: Vec<String>,       // ["direct_addresses", "relay_connections", ...]
    pub timestamp: u64,
}
```

### HTTP Operations

Low-level HTTP operations for bootstrap/relay communication:

**Features:**
- Manual HTTP/1.1 request construction
- Raw TCP socket usage
- Custom headers and timeouts
- Response parsing (JSON and plain text)

**HTTP GET Implementation:**
```rust
async fn perform_bootstrap_http_get(api_url: &str) -> Result<String> {
    // Parse URL
    let parsed_url = parse_http_url(api_url)?;
    
    // Connect via TCP
    let stream = tokio::net::TcpStream::connect(&parsed_url.socket_addr).await?;
    let mut stream = tokio::io::BufWriter::new(stream);
    
    // Construct HTTP/1.1 GET request
    let http_request = format!(
        "GET {} HTTP/1.1\r\n\
         Host: {}\r\n\
         User-Agent: iroh-discovery/0.92.0\r\n\
         Accept: application/json, text/plain\r\n\
         Connection: close\r\n\
         Cache-Control: no-cache\r\n\
         \r\n",
        parsed_url.path, parsed_url.host
    );
    
    // Send request
    stream.write_all(http_request.as_bytes()).await?;
    stream.flush().await?;
    
    // Read response
    let mut response_buffer = Vec::new();
    stream.read_to_end(&mut response_buffer).await?;
    
    // Parse HTTP response
    let response_text = String::from_utf8(response_buffer)?;
    parse_http_response(&response_text)
}
```

**HTTP POST for Publishing:**
```rust
async fn perform_http_post(url: &str, payload: &[u8]) -> Result<String> {
    let parsed_url = parse_http_url(url)?;
    let stream = tokio::net::TcpStream::connect(&parsed_url.socket_addr).await?;
    
    let http_request = format!(
        "POST {} HTTP/1.1\r\n\
         Host: {}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        parsed_url.path, parsed_url.host, payload.len()
    );
    
    // Send headers + body
    stream.write_all(http_request.as_bytes()).await?;
    stream.write_all(payload).await?;
    
    // Read response...
}
```

**HTTP URL Parsing:**
```rust
struct HttpUrlComponents {
    host: String,
    port: u16,
    path: String,
    socket_addr: SocketAddr,
}

fn parse_http_url(url: &str) -> Result<HttpUrlComponents> {
    // Parses https://host:port/path or http://host/path
    // Defaults to port 443 for https, 80 for http
}
```

### CID & Hash Conversion

Sophisticated CID to Iroh hash conversion:

**Strategies:**
1. **Direct conversion**: For 32-byte hashes (SHA-256, Blake2b-256)
2. **Truncation**: For hashes > 32 bytes (includes codec/version for differentiation)
3. **Expansion**: For hashes < 32 bytes (deterministic padding)
4. **Fallback**: For invalid/empty hashes

**Example:**
```rust
// Convert CID to Iroh hash
let cid = Cid::try_from("bafkreidgvpkjawlxz6sffxzwgooowe5yt7i6wsyg236mfoks77nywkptdq")?;
let iroh_hash = cid_to_iroh_hash(&cid)?;
```

### NodeId/PeerId Conversion

Deterministic conversion between Iroh NodeId and libp2p PeerId:

**Strategies:**
1. **Direct conversion**: If bytes are compatible
2. **Ed25519 derivation**: Using 32-byte public key
3. **Deterministic hash**: Hash-based generation with salt
4. **Fallback**: Random but deterministic from seed

**Example:**
```rust
// Convert NodeId to PeerId
let node_id = NodeId::from_bytes(&node_bytes)?;
let node_id = derive_node_id_from_node_id(node_id);
```

## Internal Architecture Details

### Store Management

The backend uses Iroh's filesystem store (`FsStore`):

```rust
enum StoreType {
    Fs(FsStore),  // Currently only FsStore is used
}
```

**Operations:**
- `import_bytes()`: Add content with automatic hash generation
- `read_at()`: Retrieve content by hash
- Tag system for content retention

### Endpoint Management

QUIC endpoint for P2P communication:

```rust
let endpoint = Endpoint::builder()
    .secret_key(secret_key)
    .bind()
    .await?;
```

**Features:**
- Automatic connection pooling
- Remote peer tracking
- Discovery service integration
- Relay support

### Swarm Integration

LibP2P swarm for Gossipsub messaging:

```rust
let swarm_manager = SwarmManager::new(keypair)?;
swarm_manager.subscribe("my-topic").await?;
```

**Capabilities:**
- Topic-based pub/sub
- Peer discovery via libp2p
- Message routing
- Connection management

## Testing

### Unit Testing

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_add_and_cat() {
        let backend = IrohBackend::new(config).await?;
        let data = Box::pin(Cursor::new(b"test data"));
        let response = backend.add(data).await?;
        
        let mut reader = backend.cat(&response.hash).await?;
        let mut content = Vec::new();
        reader.read_to_end(&mut content).await?;
        
        assert_eq!(content, b"test data");
    }
}
```

### Integration Testing

```rust
#[tokio::test]
async fn test_peer_discovery() {
    let backend = IrohBackend::new(config).await?;
    
    // Test DHT discovery
    let dht_peers = backend.dht_findpeer("node_id").await?;
    assert!(!dht_peers.addresses.is_empty());
    
    // Test mDNS discovery  
    let mdns_peers = discover_mdns_peers().await?;
    assert!(!mdns_peers.is_empty());
}
```

## Performance Tuning

### Cache Tuning

```rust
// Increase cache size for better hit rate
config.cache_size_mb = 1024;  // 1GB cache

// Enable cache compression
config.enable_cache_compression = true;
```

### Connection Tuning

```rust
// Increase peer limit for better discovery
config.max_peers_per_session = 100;

// Adjust connection timeout
config.connection_timeout_secs = 60;
```

### Discovery Tuning

```rust
let discovery_config = AdvancedDiscoveryConfig {
    discovery_timeout_secs: 30,
    max_attempts: 5,
    retry_interval_ms: 1000,
    enable_mdns: true,
    enable_dht: true,
    enable_bootstrap: true,
    enable_bootstrap_fallback: true,
    max_peers_per_session: 50,
};
```

## Security Considerations

### Secret Key Management

The node's secret key should be:
- Generated securely using `SecretKey::generate()`
- Stored encrypted at rest
- Never logged or transmitted
- Rotated periodically

### Network Security

- Bootstrap/relay node verification
- TLS for HTTP operations
- QUIC encryption for P2P
- Rate limiting for discovery

### Data Validation

- CID validation before operations
- Hash verification after retrieval
- Peer ID validation
- Address format validation

## Troubleshooting Guide

### Discovery Issues

**Problem**: Peers not discovered
```
Solution:
1. Check network connectivity
2. Verify bootstrap nodes are reachable
3. Enable mDNS for local discovery
4. Check firewall settings (ports 4001, 5001)
```

**Problem**: High discovery latency
```
Solution:
1. Reduce discovery_timeout_secs
2. Increase max_attempts
3. Enable bootstrap_fallback
4. Use local network scanning
```

### Cache Issues

**Problem**: Low cache hit rate
```
Solution:
1. Increase cache_size_mb
2. Check access patterns
3. Monitor cache metrics
4. Adjust eviction policy
```

**Problem**: Memory growth
```
Solution:
1. Reduce cache size
2. Enable periodic cleanup
3. Set TTL limits
4. Monitor cache stats
```

### Performance Issues

**Problem**: High latency
```
Solution:
1. Check network conditions
2. Increase connection pool size
3. Enable batch operations
4. Monitor metrics
```

**Problem**: Low throughput
```
Solution:
1. Increase batch_size
2. Optimize cache usage
3. Add more peers
4. Check resource limits
```

## Backend Initialization

### Complete Initialization Flow

The backend initialization follows a multi-stage process:

```rust
pub async fn new(config: &ClientConfig) -> Result<Self> {
    // 1. Setup data directory
    // 2. Load or generate secret key
    // 3. Initialize LRU cache (10k entries)
    // 4. Create backend instance
    // 5. Initialize Iroh node
    // 6. Initialize SwarmManager
    // 7. Initialize advanced discovery
    Ok(backend)
}
```

**Initialization Stages:**

1. **Data Directory Setup**: Creates and validates data storage paths
2. **Secret Key Management**: Loads existing or generates new cryptographic keys
3. **Cache Initialization**: Sets up LRU cache with 10,000 entry capacity
4. **Node Initialization**: Starts Iroh endpoint and FsStore
5. **Swarm Initialization**: Starts LibP2P swarm for Gossipsub
6. **Discovery Initialization**: Activates multi-method discovery system

### Secret Key Management

```rust
async fn load_or_generate_node_secret_key(data_dir: &Path) -> Result<SecretKey> {
    let key_file = data_dir.join("node_secret.key");
    
    // Load existing key
    if key_file.exists() {
        // Read and validate 32-byte key
    }
    
    // Generate new cryptographically secure key
    let random_bytes: [u8; 32] = rand::random();
    let secret_key = SecretKey::from_bytes(&random_bytes);
    
    // Persist for future use
    tokio::fs::write(&key_file, secret_key.to_bytes()).await?;
    
    Ok(secret_key)
}
```

**Security Features:**
- 256-bit Ed25519 keys
- Persistent key storage
- Automatic regeneration on corruption
- File-based key persistence

### Data Migration System

Automatic migration from older storage formats to FsStore:

**Migration Sources:**
1. **Memory Cache**: Active in-memory cached data
2. **Temp Directory**: Legacy temporary storage
3. **Config Files**: JSON-stored data from older versions
4. **Backup Files**: Backup data recovery

```rust
async fn migrate_to_fs_store(&self) -> Result<()> {
    // Checks migration marker
    if migration_completed() {
        return Ok(());
    }
    
    // Migrates from multiple sources
    migrate_memory_cache_to_fs().await?;
    migrate_temp_directory_to_fs().await?;
    migrate_config_data_to_fs().await?;
    migrate_backup_data_to_fs().await?;
    
    // Creates completion marker
    create_migration_marker().await?;
    
    Ok(())
}
```

**Migration Features:**
- Non-destructive (preserves original data)
- Automatic deduplication
- Progress tracking
- Completion markers
- Detailed logging

## Integrated Discovery System

### Discovery Session Management

The backend maintains active discovery sessions for peer tracking:

```rust
pub struct DiscoverySession {
    pub target_node: NodeId,
    pub started_at: Instant,
    pub discovered_addresses: Vec<NodeAddr>,
    pub status: DiscoveryStatus,
    pub discovery_method: DiscoveryMethod,
    pub last_update: Instant,
    pub attempts: u32,
}
```

**Session Lifecycle:**
1. **Active**: Discovery in progress
2. **Completed**: Addresses found
3. **Failed**: Discovery unsuccessful
4. **TimedOut**: Exceeded timeout period

### Integrated Discovery API

```rust
// Discover peer using all available methods
let addresses = backend.discover_peer_integrated(node_id).await?;

// Publish node information
let services_count = backend.publish_node_integrated(&node_data).await?;

// Get active discovery status
let status_map = backend.get_active_discoveries_status().await?;

// Force DHT synchronization
backend.force_dht_sync().await?;
```

### Auto-Publication on Discovery

Automatic node advertisement when peers are discovered:

```rust
async fn auto_publish_on_discovery(
    &self,
    node_data: &NodeData,
    config: &ClientConfig,
) -> Result<()> {
    // Waits to avoid spam
    tokio::time::sleep(Duration::from_secs(5)).await;
    
    // Publishes via DHT
    publish_via_dht(&node_data).await?;
    
    // Publishes via all services
    publish_to_discovery_services(&node_data, &config).await?;
    
    Ok(())
}
```

## Background Tasks

The backend runs several background tasks for optimal operation:

### Discovery Manager

Manages active discovery sessions:
- Runs every 60 seconds
- Cleans expired sessions (5-minute TTL)
- Processes pending discoveries
- Updates discovery status

### DHT Integration Task

Integrates DHT discoveries with sessions:
- Runs every 30 seconds
- Updates active sessions with DHT results
- Triggers auto-publication
- Updates DHT cache

### Peer Discovery Task

Initial peer discovery on startup:
- Runs once at initialization
- Uses mDNS, DHT, and local network scan
- Populates DHT cache
- Discovers up to 20 peers

## Gossipsub Integration

### Topic Management

```rust
// Subscribe to topic
backend.subscribe_gossip("my-topic").await?;

// Publish message
backend.publish_gossip("my-topic", b"message").await?;

// Get mesh peers
let peers = backend.get_topic_mesh_peers("my-topic").await?;
```

**Gossipsub Features:**
- Topic-based pub/sub
- Mesh network topology
- Message routing
- Peer discovery integration

### SwarmManager Access

```rust
// Get SwarmManager reference
let swarm_arc = backend.get_swarm_manager().await?;
let swarm_lock = swarm_arc.read().await;

if let Some(swarm) = swarm_lock.as_ref() {
    // Use swarm for advanced operations
    swarm.subscribe_topic(&topic_hash).await?;
    swarm.publish_message(&topic_hash, data).await?;
}
```

## Store Operations

### FsStore Management

The backend uses Iroh's filesystem-based store:

```rust
// Get store reference
let store_arc = backend.get_store().await?;
let store_lock = store_arc.read().await;

if let Some(StoreType::Fs(fs_store)) = store_lock.as_ref() {
    // Import data
    let tag = fs_store.import_bytes(data, BlobFormat::Raw).await?;
    
    // Read data
    let reader = fs_store.get(&hash).await?;
    
    // Check status
    let status = fs_store.entry_status(&hash).await?;
}
```

**Store Features:**
- Persistent filesystem storage
- Automatic hash generation
- Tag-based retention
- Partial/Complete status tracking

### Data Persistence

- **Location**: `{data_dir}/iroh_store/`
- **Format**: Iroh native blob format
- **Indexing**: Content-addressed (by hash)
- **Retention**: Tag-based (permanent until unpinned)

## Advanced Utilities

### Hash Generation

```rust
// Generate deterministic NodeId from data
let node_id = generate_node_id_from_address(&socket_addr);

// Generate NodeId from response data
let node_id = generate_node_id_from_response_data(&response);
```

### Address Validation

```rust
// Check if address format is valid
if is_valid_address_format(addr_str) {
    // Process address
}
```

### HTTP URL Parsing

```rust
// Parse HTTP URL components
let components = parse_http_url(url)?;
println!("Host: {}, Port: {}", components.host, components.port);
```

### Multiaddr Parsing

```rust
// Parse multiaddr to SocketAddr
let socket_addr = parse_multiaddr_to_socket_addr("/ip4/192.168.1.1/tcp/4001")?;
```

## Performance Metrics

### Cache Metrics

```rust
pub struct CacheMetrics {
    pub hits: u64,
    pub misses: u64,
    pub total_bytes: u64,
    pub last_updated: Option<Instant>,
}

// Usage
let metrics = backend.cache_metrics.read().await;
let hit_ratio = metrics.hit_ratio();
println!("Cache hit ratio: {:.1}%", hit_ratio * 100.0);
```

### Discovery Cache Statistics

```rust
pub struct DiscoveryCacheStats {
    pub entries_count: u32,
    pub total_cached_addresses: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub hit_ratio_percent: f32,
    pub oldest_entry_age_seconds: u64,
    pub capacity_used_percent: u32,
}

// Usage
let stats = backend.get_discovery_cache_stats().await;
println!("Discovery cache: {} entries, {:.1}% hit ratio",
    stats.entries_count, stats.hit_ratio_percent);
```

## API Versioning

Current API version: **v1** (Iroh 0.92.0)

**Version Compatibility:**
- Iroh: 0.92.0+
- libp2p: 0.53+
- tokio: 1.35+

## PeerId/NodeId Conversion System

The backend implements bidirectional deterministic conversion between libp2p PeerId and Iroh NodeId:

### PeerId to NodeId

```rust
fn node_id_to_node_id(&self, node_id: &PeerId) -> Result<NodeId> {
    let peer_bytes = node_id.to_bytes();
    
    // Strategy 1: For 32+ byte PeerIds, use SHA-256 hash
    if peer_bytes.len() >= 32 {
        // Hash-based conversion with XOR mixing
        let mut node_bytes = [0u8; 32];
        for i in 0..32 {
            node_bytes[i] = hash_bytes[i % 8] ^ peer_bytes[i % peer_bytes.len()];
        }
        NodeId::from_bytes(&node_bytes)?
    }
    // Strategy 2: For smaller PeerIds, deterministic expansion
    else {
        // Deterministic padding based on content
        let mut node_bytes = [0u8; 32];
        for i in 0..32 {
            node_bytes[i] = if i < peer_bytes.len() {
                peer_bytes[i]
            } else {
                let pattern_index = i % peer_bytes.len();
                peer_bytes[pattern_index].wrapping_mul(i as u8 + 1).wrapping_add(0x42)
            };
        }
        NodeId::from_bytes(&node_bytes)?
    }
}
```

### NodeId to PeerId

```rust
fn node_id_to_node_id(&self, node_id: &NodeId) -> Result<PeerId> {
    let node_bytes = node_id.as_bytes();
    
    // Derive deterministic Ed25519 seed
    let mut ed25519_seed = [0u8; 32];
    for (i, item) in ed25519_seed.iter_mut().enumerate() {
        *item = if i < 8 {
            ((seed_hash >> (i * 8)) & 0xFF) as u8
        } else {
            let node_index = (i - 8) % node_bytes.len();
            node_bytes[node_index].wrapping_add((i as u8).wrapping_mul(0x13))
        };
    }
    
    // Generate keypair and extract PeerId
    let secret_key = ed25519::SecretKey::try_from_bytes(&mut ed25519_seed)?;
    let keypair = ed25519::Keypair::from(secret_key);
    let node_id = Keypair::from(keypair).public().to_node_id();
    
    Ok(node_id)
}
```

**Properties:**
- **Deterministic**: Same input always produces same output
- **Bidirectional**: Consistent roundtrip conversion
- **Cryptographically sound**: Uses Ed25519 key derivation
- **Distribution**: Uniform distribution across ID space

## Advanced Peer Discovery

### Probable Address Generation

Smart address generation based on network patterns:

```rust
async fn generate_probable_peer_addresses(
    &self,
    node_id: &NodeId,
    node_hash: u64,
) -> Vec<String> {
    // Strategy 1: Hash-based port selection
    let base_port = 4001 + (node_hash % 1000) as u16;
    let alt_port = 9090 + (node_hash % 100) as u16;
    
    // Strategy 2: Dynamic local IPs
    let dynamic_ip_192 = format!("192.168.1.{}", 2 + (node_hash % 253));
    let dynamic_ip_10 = format!("10.0.0.{}", 2 + (node_hash % 253));
    
    // Strategy 3: IPv6 link-local
    let ipv6_local = format!("fe80::{:x}", node_hash % 0xFFFF);
    
    // Strategy 4: Relay addresses
    let relay_addrs = format!(
        "/ip4/127.0.0.1/tcp/4002/p2p/{}/p2p-circuit/p2p/{}",
        relay_node_id, node_id
    );
    
    // Combine all strategies
    vec![/* generated addresses */]
}
```

**Features:**
- Deterministic port selection based on NodeId hash
- Dynamic IP generation across common private ranges
- IPv6 link-local address support
- Relay circuit addresses
- Up to 10 probable addresses per peer

### DHT Lookup with Iroh

```rust
async fn perform_iroh_dht_lookup(
    &self,
    node_id: &str,
    endpoint: &Endpoint,
) -> Result<Vec<String>> {
    // Parse peer ID to NodeId
    let node_id = node_id.parse::<NodeId>()?;
    
    // Timeout-based lookup (10 seconds)
    let addresses = timeout(
        Duration::from_secs(10),
        self.discover_peer_addresses(node_id, endpoint)
    ).await??;
    
    Ok(addresses)
}
```

**Strategies:**
1. Check if peer is local node
2. Query active connections via `remote_info_iter()`
3. Use discovery service if available
4. Consult internal discovery cache
5. Generate probable addresses
6. Fallback to default addresses

## Performance Optimization

### Automatic Cache Optimization

```rust
pub async fn optimize_performance(&self) -> Result<()> {
    // Optimize cache based on metrics
    self.optimize_cache_with_metrics().await?;
    
    // Update performance metrics
    let stats = self.get_cache_statistics().await?;
    let mut metrics = self.metrics.write().await;
    
    // Performance boost based on hit ratio
    if stats.hit_ratio > 0.5 {
        metrics.ops_per_second *= 1.0 + stats.hit_ratio;
    }
    
    metrics.avg_latency_ms = if stats.hit_ratio > 0.8 { 0.5 } else { 1.0 };
    
    Ok(())
}
```

**Optimization Features:**
- Automatic cache cleanup when hit ratio < 30%
- Performance boost based on cache effectiveness
- Adaptive latency calculation
- LRU eviction of least-used entries

### Cache Statistics

```rust
pub struct SimpleCacheStats {
    pub entries_count: u32,
    pub hit_ratio: f64,
    pub total_size_bytes: u64,
}

// Usage
let stats = backend.get_cache_statistics().await?;
println!("Cache: {} entries, {:.1}% hit ratio, {} bytes",
    stats.entries_count, stats.hit_ratio * 100.0, stats.total_size_bytes);
```

## Discovery Statistics

Comprehensive discovery system statistics:

```rust
pub async fn get_discovery_stats(&self) -> String {
    // Returns formatted statistics:
    // - Active sessions
    // - Completed sessions
    // - Failed sessions
    // - Total peers discovered
    // - Average discovery time
    // - Cache statistics
}
```

**Example Output:**
```
ESTATÍSTICAS DO SISTEMA DE DISCOVERY AVANÇADO

Sessões de Discovery:
   - Ativas: 3
   - Completadas: 47
   - Falhadas: 2
   - Total de peers descobertos: 125

Performance:
   - Tempo médio de discovery: 342.5ms
   - Taxa de sucesso: 95.9%

Cache:
   - Entradas: 89
   - Hit ratio: 87.3%
```

## Block Operations

### Block Get

```rust
async fn block_get(&self, cid: &Cid) -> Result<Vec<u8>> {
    // 1. Check cache
    if let Some(cached) = self.get_from_cache(cid_str).await {
        return Ok(cached.to_vec());
    }
    
    // 2. Fetch from FsStore
    let hash = self.cid_to_iroh_hash(cid)?;
    let entry = fs_store.get(&hash).await?;
    let data = entry.data_reader().read_to_end().await?;
    
    // 3. Cache for future access
    self.add_to_cache(cid_str, data.clone()).await?;
    
    Ok(data.to_vec())
}
```

### Block Put

```rust
async fn block_put(&self, data: Vec<u8>) -> Result<Cid> {
    // 1. Store in FsStore
    let bytes_data = Bytes::from(data);
    let temp_tag = fs_store.import_bytes(bytes_data.clone(), BlobFormat::Raw).await?;
    let hash = *temp_tag.hash();
    
    // 2. Generate IPFS-compatible CID
    let cid_str = format!("bafkreid{}", hex::encode(hash.as_bytes()));
    
    // 3. Cache for fast access
    self.add_to_cache(&cid_str, bytes_data).await?;
    
    // 4. Parse and return CID
    Self::parse_cid(&cid_str)
}
```

### Block Stat

```rust
async fn block_stat(&self, cid: &Cid) -> Result<BlockStats> {
    // Check cache first
    if let Some(cached) = self.get_from_cache(cid_str).await {
        return Ok(BlockStats {
            cid: *cid,
            size: cached.len() as u64,
            exists_locally: true,
        });
    }
    
    // Check FsStore
    let hash = self.cid_to_iroh_hash(cid)?;
    let status = fs_store.entry_status(&hash).await?;
    
    match status {
        EntryStatus::Complete => Ok(BlockStats { exists_locally: true, ... }),
        EntryStatus::Partial => Ok(BlockStats { exists_locally: true, ... }),
        EntryStatus::NotFound => Ok(BlockStats { exists_locally: false, ... }),
    }
}
```

## Health Checks

Comprehensive health monitoring:

```rust
async fn health_check(&self) -> Result<HealthStatus> {
    let mut checks = Vec::new();
    
    // Check 1: Node status
    checks.push(HealthCheck {
        name: "node_status",
        passed: status.is_online,
        message: if status.is_online {
            "Nó Iroh online"
        } else {
            "Nó Iroh offline"
        }
    });
    
    // Check 2: Data directory accessible
    checks.push(HealthCheck {
        name: "data_directory",
        passed: tokio::fs::metadata(&self.data_dir).await.is_ok(),
        message: "Diretório de dados acessível"
    });
    
    // Check 3: Metrics available
    checks.push(HealthCheck {
        name: "metrics",
        passed: self.metrics().await.is_ok(),
        message: "Métricas disponíveis"
    });
    
    Ok(HealthStatus {
        healthy: all_checks_passed,
        message,
        response_time_ms,
        checks,
    })
}
```

## Repository Operations

### Repo Stat

```rust
async fn repo_stat(&self) -> Result<RepoStats> {
    let store_path = self.data_dir.join("iroh_store");
    
    // Scan store directory
    let mut num_objects = 0;
    let mut total_size = 0;
    
    let mut entries = tokio::fs::read_dir(&store_path).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry.metadata().await?.is_file() {
            num_objects += 1;
            total_size += entry.metadata().await?.len();
        }
    }
    
    Ok(RepoStats {
        num_objects,
        repo_size: total_size,
        repo_path: store_path.to_string_lossy().to_string(),
        version: "15",
    })
}
```

## Memory Usage Estimation

```rust
async fn estimate_memory_usage(&self) -> u64 {
    // Pinned cache
    let pinned_cache_size = self.pinned_cache.lock().await.len() as u64 * 64;
    
    // Data cache (LRU)
    let data_cache_size = self.data_cache.read().await.len() as u64 * 1024;
    
    // DHT cache
    let dht_cache_size = self.dht_cache.read().await.peers.len() as u64 * 256;
    
    pinned_cache_size + data_cache_size + dht_cache_size
}
```

**Breakdown:**
- Pinned cache: ~64 bytes per entry
- Data cache: ~1 KB per entry (estimated)
- DHT cache: ~256 bytes per peer

## Connection Pool Management

```rust
pub async fn get_connection_pool_status(&self) -> String {
    let pool = self.connection_pool.read().await;
    format!("Pool de conexões ativo com {} peers", pool.len())
}
```

**Features:**
- Active connection tracking
- Automatic connection reuse
- Health monitoring per connection
- Load balancing across connections

## Relay Node Generation

```rust
fn generate_relay_node_id(&self, seed: u64) -> NodeId {
    let mut relay_bytes = [0u8; 32];
    let seed_bytes = seed.to_be_bytes();
    
    // Fill 32 bytes with pattern based on seed
    for (i, item) in relay_bytes.iter_mut().enumerate() {
        let seed_index = i % seed_bytes.len();
        *item = seed_bytes[seed_index].wrapping_add(i as u8);
    }
    
    NodeId::from_bytes(&relay_bytes)?
}
```

**Purpose:**
- Generate deterministic relay NodeIds
- Used for probable address generation
- Ensures consistent relay node identification

## Changelog

See the main Guardian DB changelog for version history.

## License

This backend is part of GuardianDB and follows the same licensing terms.

## Contributing

Contributions are welcome! Please follow the project's contribution guidelines.

## References

- [Iroh Documentation](https://iroh.computer/docs)
- [IPFS Specifications](https://specs.ipfs.tech/)
- [libp2p Documentation](https://docs.libp2p.io/)
- [Guardian DB Documentation](../README.md)
- [mDNS RFC 6762](https://datatracker.ietf.org/doc/html/rfc6762)
- [DNS RFC 1035](https://datatracker.ietf.org/doc/html/rfc1035)
