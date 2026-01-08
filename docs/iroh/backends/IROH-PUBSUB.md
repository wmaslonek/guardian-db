    // ╔════════════════════════════════════════════════════════════════════════════════╗
    // ║                          ⚠ OUTDATED DOCUMENTATION                             ║
    // ╚════════════════════════════════════════════════════════════════════════════════╝


# Iroh PubSub - Gossipsub Integration Documentation

## Overview

The Iroh PubSub module provides a seamless integration between the Iroh backend and LibP2P's Gossipsub protocol. It implements the `PubSubInterface` trait, enabling topic-based publish/subscribe messaging using the high-performance Gossipsub protocol while leveraging Iroh's content operations.

## Table of Contents

- [Architecture](#architecture)
- [Core Components](#core-components)
- [API Reference](#api-reference)
- [Usage Examples](#usage-examples)
- [Message Flow](#message-flow)
- [Peer Management](#peer-management)
- [Event Streaming](#event-streaming)
- [Best Practices](#best-practices)
- [Troubleshooting](#troubleshooting)

## Architecture

### Integration Architecture

```
┌──────────────────────────────────────────────────────┐
│                  EpidemicPubSub                          │
├──────────────────────────────────────────────────────┤
│  ┌────────────────────────────────────────────┐     │
│  │         Topic Cache (HashMap)              │     │
│  │  topic_name -> Arc<IrohTopic>              │     │
│  └────────────────────────────────────────────┘     │
│                      │                               │
│  ┌───────────────────▼──────────────────────┐       │
│  │         IrohBackend Reference            │       │
│  │    (SwarmManager for Gossipsub)          │       │
│  └──────────────────────────────────────────┘       │
└──────────────────────────────────────────────────────┘

Topic Message Flow:
  Publisher → IrohTopic.publish() → SwarmManager → Gossipsub
                                                        ↓
  Subscriber ← BroadcastStream ← message_sender ← SwarmManager
```

### Component Relationship

```
IrohBackend (Content Ops)
    │
    ├── SwarmManager (LibP2P)
    │       │
    │       └── Gossipsub Protocol
    │
    └── EpidemicPubSub (Wrapper)
            │
            └── IrohTopic (Per-topic management)
                    │
                    ├── Message Broadcasting
                    ├── Peer Tracking
                    └── Event Streaming
```

## Core Components

### 1. EpidemicPubSub

Wrapper that implements the `PubSubInterface` for Iroh:

```rust
pub struct EpidemicPubSub {
    /// Reference to Iroh backend
    iroh_backend: Arc<IrohBackend>,
    
    /// Cache of active topics
    topics: Arc<RwLock<HashMap<String, Arc<IrohTopic>>>>,
}
```

**Responsibilities:**
- Topic lifecycle management
- SwarmManager access coordination
- Topic caching and reuse
- Interface implementation

### 2. IrohTopic

Topic-specific implementation of `PubSubTopic`:

```rust
pub struct IrohTopic {
    /// Topic name
    topic_name: String,
    
    /// Gossipsub topic hash
    topic_hash: TopicHash,
    
    /// SwarmManager reference
    swarm: Arc<RwLock<Option<SwarmManager>>>,
    
    /// Broadcast channel for received messages
    message_sender: Arc<broadcast::Sender<Vec<u8>>>,
    
    /// Connected peers on this topic
    peers: Arc<RwLock<Vec<PeerId>>>,
    
    /// Broadcast channel for peer events (join/leave)
    peer_events_sender: Arc<broadcast::Sender<Event>>,
}
```

**Features:**
- Message broadcasting to subscribers
- Peer tracking per topic
- Event generation for peer join/leave
- Stream-based message delivery

## API Reference

### PubSubInterface Implementation

#### Subscribe to Topic

```rust
async fn topic_subscribe(
    &mut self,
    topic: &str,
) -> Result<Arc<dyn PubSubTopic<Error = GuardianError>>>
```

**Description:** Subscribes to a topic, creating it if it doesn't exist.

**Parameters:**
- `topic`: Topic name (string identifier)

**Returns:**
- `Arc<dyn PubSubTopic>`: Topic handle for publishing and watching

**Example:**
```rust
let mut pubsub = iroh_backend.create_pubsub_interface();
let topic = pubsub.topic_subscribe("my-topic").await?;
```

### PubSubTopic Implementation

#### Publish Message

```rust
async fn publish(&self, message: Vec<u8>) -> Result<()>
```

**Description:** Publishes a message to the topic via Gossipsub.

**Parameters:**
- `message`: Message data as byte vector

**Returns:**
- `Result<()>`: Success or error

**Example:**
```rust
let topic = pubsub.topic_subscribe("announcements").await?;
topic.publish(b"Hello, network!".to_vec()).await?;
```

#### List Topic Peers

```rust
async fn peers(&self) -> Result<Vec<PeerId>>
```

**Description:** Lists all peers currently subscribed to the topic.

**Returns:**
- `Vec<PeerId>`: List of peer IDs

**Example:**
```rust
let peers = topic.peers().await?;
println!("Topic has {} subscribers", peers.len());
```

#### Watch Peer Events

```rust
async fn watch_peers(&self) -> Result<Pin<Box<dyn Stream<Item = Event> + Send>>>
```

**Description:** Creates a stream of peer join/leave events.

**Returns:**
- Stream of `Event` (peer join/leave notifications)

**Example:**
```rust
let mut peer_stream = topic.watch_peers().await?;
while let Some(event) = peer_stream.next().await {
    match event.as_ref() {
        s if s.starts_with("PeerConnected:") => println!("Peer joined: {}", s),
        s if s.starts_with("PeerDisconnected:") => println!("Peer left: {}", s),
        _ => {}
    }
}
```

#### Watch Messages

```rust
async fn watch_messages(&self) -> Result<Pin<Box<dyn Stream<Item = EventPubSubMessage> + Send>>>
```

**Description:** Creates a stream of received messages.

**Returns:**
- Stream of `EventPubSubMessage` containing message data

**Example:**
```rust
let mut message_stream = topic.watch_messages().await?;
while let Some(msg) = message_stream.next().await {
    println!("Received: {} bytes", msg.content.len());
}
```

#### Get Topic Name

```rust
fn topic(&self) -> &str
```

**Description:** Returns the topic name.

**Returns:**
- `&str`: Topic name string

## Usage Examples

### Complete Pub/Sub Example

```rust
use guardian_db::ipfs_core_api::backends::IrohBackend;
use std::sync::Arc;

// Initialize backend
let iroh_backend = Arc::new(IrohBackend::new(&config).await?);

// Create pubsub interface
let mut pubsub = iroh_backend.create_pubsub_interface();

// Subscribe to topic
let topic = pubsub.topic_subscribe("chat").await?;
println!("Subscribed to topic: {}", topic.topic());

// Publish messages
topic.publish(b"Hello, world!".to_vec()).await?;
topic.publish(b"This is a test message".to_vec()).await?;

// List peers on topic
let peers = topic.peers().await?;
println!("Topic has {} subscribers", peers.len());
for peer in peers {
    println!("  - {}", peer);
}
```

### Message Watching

```rust
use futures::StreamExt;

// Subscribe to topic
let topic = pubsub.topic_subscribe("notifications").await?;

// Watch for incoming messages
let mut message_stream = topic.watch_messages().await?;

tokio::spawn(async move {
    while let Some(msg) = message_stream.next().await {
        let content = String::from_utf8_lossy(&msg.content);
        println!("Received message: {}", content);
        
        // Process message...
    }
});
```

### Peer Event Monitoring

```rust
// Subscribe to topic
let topic = pubsub.topic_subscribe("network-events").await?;

// Watch peer join/leave events
let mut peer_stream = topic.watch_peers().await?;

tokio::spawn(async move {
    while let Some(event) = peer_stream.next().await {
        let event_str = event.as_ref();
        
        if let Some(peer_info) = event_str.strip_prefix("PeerConnected:") {
            let parts: Vec<&str> = peer_info.split(':').collect();
            if parts.len() == 2 {
                let node_id = parts[0];
                let topic = parts[1];
                println!("Peer {} joined topic {}", node_id, topic);
            }
        } else if let Some(peer_info) = event_str.strip_prefix("PeerDisconnected:") {
            let parts: Vec<&str> = peer_info.split(':').collect();
            if parts.len() == 2 {
                let node_id = parts[0];
                let topic = parts[1];
                println!("Peer {} left topic {}", node_id, topic);
            }
        }
    }
});
```

### Multi-Topic Management

```rust
// Subscribe to multiple topics
let topic1 = pubsub.topic_subscribe("general").await?;
let topic2 = pubsub.topic_subscribe("announcements").await?;
let topic3 = pubsub.topic_subscribe("alerts").await?;

// Publish to different topics
topic1.publish(b"General message".to_vec()).await?;
topic2.publish(b"Important announcement".to_vec()).await?;
topic3.publish(b"Alert notification".to_vec()).await?;

// Each topic maintains its own subscriber list
let general_peers = topic1.peers().await?;
let announcement_peers = topic2.peers().await?;
let alert_peers = topic3.peers().await?;
```

## Message Flow

### Publishing Flow

```
Application
    ↓
topic.publish(data)
    ↓
IrohTopic::publish()
    ↓
SwarmManager::publish_message()
    ↓
Gossipsub Protocol
    ↓
Network Distribution
```

### Receiving Flow

```
Gossipsub Protocol (receives message)
    ↓
SwarmManager (event loop)
    ↓
IrohTopic::notify_message()
    ↓
message_sender.send() (broadcast)
    ↓
BroadcastStream (to all subscribers)
    ↓
Application (via watch_messages())
```

## Peer Management

### Automatic Peer Tracking

The system automatically tracks peers on each topic:

```rust
// Add peer to topic
pub async fn add_peer(&self, node_id: PeerId) {
    let mut peers = self.peers.write().await;
    
    if !peers.contains(&node_id) {
        peers.push(node_id);
        
        // Emit peer connected event
        let event = Arc::new(format!("PeerConnected:{}:{}", node_id, self.topic_name));
        self.peer_events_sender.send(event);
    }
}
```

### Peer Removal

```rust
// Remove peer from topic
pub async fn remove_peer(&self, node_id: &PeerId) {
    let mut peers = self.peers.write().await;
    let was_present = peers.iter().any(|p| p == node_id);
    peers.retain(|p| p != node_id);
    
    if was_present {
        // Emit peer disconnected event
        let event = Arc::new(format!("PeerDisconnected:{}:{}", node_id, self.topic_name));
        self.peer_events_sender.send(event);
    }
}
```

### Peer Listing

```rust
// List all peers on topic
pub async fn list_peers(&self) -> Vec<PeerId> {
    let peers = self.peers.read().await;
    peers.clone()
}
```

## Event Streaming

### Message Streaming

The module uses Tokio broadcast channels for efficient message distribution:

**Channel Configuration:**
- Buffer size: 1000 messages
- Type: `broadcast::Sender<Vec<u8>>`
- Lagged messages are dropped (non-blocking)

**Stream Creation:**
```rust
pub fn subscribe_messages(&self) -> broadcast::Receiver<Vec<u8>> {
    self.message_sender.subscribe()
}
```

**Stream Conversion:**
```rust
// Convert broadcast receiver to Stream
let receiver = self.message_sender.subscribe();
let stream = BroadcastStream::new(receiver)
    .filter_map(|result| async move {
        result.ok()  // Drop lagged/closed errors
    });
```

### Peer Event Streaming

**Event Format:**
- `PeerConnected:{node_id}:{topic_name}`
- `PeerDisconnected:{node_id}:{topic_name}`

**Event Processing:**
```rust
let mut peer_stream = topic.watch_peers().await?;
while let Some(event) = peer_stream.next().await {
    if event.as_ref().starts_with("PeerConnected:") {
        // Handle peer join
    } else if event.as_ref().starts_with("PeerDisconnected:") {
        // Handle peer leave
    }
}
```

## Topic Management

### Topic Creation

Topics are created lazily on first subscription:

```rust
async fn get_or_create_topic(&self, topic: &str) -> Result<Arc<IrohTopic>> {
    let mut topics = self.topics.write().await;
    
    // Return existing topic if already subscribed
    if let Some(existing_topic) = topics.get(topic) {
        return Ok(existing_topic.clone());
    }
    
    // Create new topic
    let topic_hash = TopicHash::from_raw(topic);
    let (message_sender, _) = broadcast::channel(1000);
    let (peer_events_sender, _) = broadcast::channel(1000);
    
    // Get SwarmManager reference
    let swarm = self.iroh_backend.get_swarm_manager().await?;
    
    // Subscribe to topic in Gossipsub
    {
        let swarm_lock = swarm.read().await;
        if let Some(swarm_manager) = swarm_lock.as_ref() {
            swarm_manager.subscribe_topic(&topic_hash).await?;
        }
    }
    
    // Create IrohTopic instance
    let iroh_topic = Arc::new(IrohTopic {
        topic_name: topic.to_string(),
        topic_hash,
        swarm,
        message_sender: Arc::new(message_sender),
        peers: Arc::new(RwLock::new(Vec::new())),
        peer_events_sender: Arc::new(peer_events_sender),
    });
    
    // Cache topic
    topics.insert(topic.to_string(), iroh_topic.clone());
    
    Ok(iroh_topic)
}
```

**Topic Features:**
- Lazy creation (only when subscribed)
- Automatic Gossipsub subscription
- Shared topic instances (Arc-based)
- Cached for reuse

### Topic Lifecycle

1. **Creation**: On first `topic_subscribe()` call
2. **Active**: While subscribers exist
3. **Cached**: Remains in memory for fast resubscription
4. **Cleanup**: (Not implemented - topics persist)

## SwarmManager Integration

### SwarmManager Access

```rust
// Get SwarmManager from IrohBackend
let swarm = self.iroh_backend.get_swarm_manager().await?;

// Use SwarmManager for Gossipsub operations
let swarm_lock = swarm.read().await;
if let Some(swarm_manager) = swarm_lock.as_ref() {
    // Subscribe to topic
    swarm_manager.subscribe_topic(&topic_hash).await?;
    
    // Publish message
    swarm_manager.publish_message(&topic_hash, data).await?;
}
```

### Message Publishing

```rust
pub async fn publish(&self, data: &[u8]) -> Result<()> {
    let swarm_lock = self.swarm.read().await;
    
    if let Some(swarm_manager) = swarm_lock.as_ref() {
        swarm_manager
            .publish_message(&self.topic_hash, data)
            .await
            .map_err(|e| GuardianError::Other(
                format!("Gossipsub publish error: {}", e)
            ))?;
        
        Ok(())
    } else {
        Err(GuardianError::Other("SwarmManager not available"))
    }
}
```

### Message Reception

```rust
pub async fn notify_message(&self, data: Vec<u8>) -> Result<()> {
    match self.message_sender.send(data) {
        Ok(_) => {
            debug!("Message propagated to topic {} subscribers", self.topic_name);
            Ok(())
        },
        Err(_) => {
            warn!("No active subscribers for topic {}", self.topic_name);
            Ok(())  // Not a fatal error
        }
    }
}
```

## Best Practices

### 1. Reuse Topic Instances

```rust
// Good: Reuse topic handle
let topic = pubsub.topic_subscribe("events").await?;
for i in 0..100 {
    topic.publish(format!("Event {}", i).into_bytes()).await?;
}

// Avoid: Resubscribing for each message
for i in 0..100 {
    let topic = pubsub.topic_subscribe("events").await?;  // Inefficient
    topic.publish(format!("Event {}", i).into_bytes()).await?;
}
```

### 2. Handle Subscriber Lag

```rust
// Use filter_map to ignore lagged messages
let stream = BroadcastStream::new(receiver)
    .filter_map(|result| async move {
        match result {
            Ok(data) => Some(data),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!("Lagged by {} messages", n);
                None
            },
            Err(_) => None,
        }
    });
```

### 3. Monitor Peer Count

```rust
// Periodically check subscriber count
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        interval.tick().await;
        let peers = topic.peers().await?;
        info!("Topic {} has {} peers", topic.topic(), peers.len());
    }
});
```

### 4. Cleanup on Shutdown

```rust
// When shutting down, allow time for final messages
tokio::time::sleep(Duration::from_secs(2)).await;
drop(topic);  // Drop topic to close channels
```

## Message Broadcasting

### Broadcast Channel Configuration

```rust
// Channel capacity: 1000 messages
let (message_sender, _) = broadcast::channel(1000);
```

**Channel Behavior:**
- **Buffer full**: Oldest messages are dropped (lagged)
- **No subscribers**: Messages are dropped silently
- **Multiple subscribers**: Each gets a copy
- **Async delivery**: Non-blocking send

### Message Propagation

```rust
// Internal propagation when message received
pub async fn notify_message(&self, data: Vec<u8>) -> Result<()> {
    // Broadcast to all subscribers
    match self.message_sender.send(data) {
        Ok(subscriber_count) => {
            debug!("Sent to {} subscribers", subscriber_count);
            Ok(())
        },
        Err(_) => {
            // No subscribers - not an error
            warn!("No active subscribers");
            Ok(())
        }
    }
}
```

## Error Handling

### Common Errors

**SwarmManager Not Available:**
```rust
Err(GuardianError::Other("SwarmManager não disponível"))
```

**Solution:** Ensure IrohBackend is fully initialized before creating PubSub interface.

**Gossipsub Publish Error:**
```rust
Err(GuardianError::Other("Erro ao publicar no Gossipsub: {error}"))
```

**Solution:** Check network connectivity and peer count.

**No Subscribers:**
```rust
// Not an error - just a warning
warn!("No active subscribers for topic {}", topic_name);
```

**Solution:** Normal when no one is listening. Not a failure condition.

## Integration with IrohBackend

### Creating PubSub Interface

```rust
impl IrohBackend {
    pub fn create_pubsub_interface(self: Arc<Self>) -> EpidemicPubSub {
        EpidemicPubSub::new(self)
    }
}
```

**Usage:**
```rust
let iroh = Arc::new(IrohBackend::new(&config).await?);
let pubsub = iroh.create_pubsub_interface();
```

### SwarmManager Requirement

The PubSub interface requires that the IrohBackend has an initialized SwarmManager:

**Initialization Check:**
```rust
// SwarmManager is initialized in IrohBackend::new()
// via initialize_swarm() method
let swarm = iroh_backend.get_swarm_manager().await?;

if swarm.read().await.is_none() {
    return Err(GuardianError::Other("SwarmManager not initialized"));
}
```

## Advanced Features

### Topic Caching

Topics are cached for efficient reuse:

```rust
// First subscription: Creates topic and subscribes to Gossipsub
let topic1 = pubsub.topic_subscribe("chat").await?;

// Second subscription to same topic: Returns cached instance
let topic2 = pubsub.topic_subscribe("chat").await?;

// Both are the same Arc instance
assert!(Arc::ptr_eq(&topic1, &topic2));
```

### Message Ordering

Messages are delivered in the order received from Gossipsub, but:

- **No global ordering** across different publishers
- **Best-effort delivery** (gossip protocol characteristic)
- **Lagged messages** may be dropped on slow subscribers

### Peer Discovery Integration

Peers discovered via Gossipsub mesh are automatically tracked:

```rust
// When SwarmManager detects new peer on topic
topic.add_peer(node_id).await;

// Subscribers are notified via peer events
let event = "PeerConnected:{node_id}:{topic_name}";
peer_events_sender.send(Arc::new(event));
```

## Performance Considerations

### Memory Usage

- **Per topic**: ~2KB (channels + metadata)
- **Per message**: Variable (message size + overhead)
- **Per peer**: ~200 bytes (PeerId + metadata)
- **Channel buffer**: 1000 messages max (~1MB if 1KB average)

### Scalability

- **Topics**: Unlimited (HashMap-based)
- **Peers per topic**: Limited by Gossipsub mesh (typically 6-12)
- **Message rate**: Limited by network bandwidth
- **Subscribers**: Unlimited (broadcast channel)

### Latency

- **Message publish**: ~5-20ms (local processing)
- **Message propagation**: ~50-200ms (network gossip)
- **Peer events**: ~1-5ms (local notification)
- **Subscription**: ~10-50ms (Gossipsub join)

## Troubleshooting

### Messages Not Received

**Problem:** Messages published but not received by subscribers

```
Solutions:
1. Check if watch_messages() stream is active
2. Verify SwarmManager is initialized
3. Check if peers are connected to topic
4. Ensure subscriber is not lagged (buffer full)
```

### Peer Events Not Firing

**Problem:** Peer join/leave events not received

```
Solutions:
1. Verify watch_peers() stream is active
2. Check if add_peer/remove_peer are called
3. Ensure SwarmManager is processing events
4. Check peer_events_sender channel
```

### Topic Subscription Fails

**Problem:** topic_subscribe() returns error

```
Solutions:
1. Check SwarmManager availability
2. Verify network connectivity
3. Check Gossipsub configuration
4. Ensure IrohBackend is initialized
```

### High Memory Usage

**Problem:** Memory grows with message count

```
Solutions:
1. Reduce channel buffer size (default 1000)
2. Process messages faster to avoid lag
3. Limit number of active topics
4. Implement topic cleanup mechanism
```

## Comparison with Other Approaches

### EpidemicPubSub vs Direct Gossipsub

| Feature | EpidemicPubSub | Direct Gossipsub |
|---------|------------|------------------|
| Integration | Seamless with Iroh | Separate library |
| Setup complexity | Simple (one line) | Complex (swarm setup) |
| Content ops | Built-in via Iroh | Requires separate IPFS node |
| Message delivery | Stream-based | Callback-based |
| Peer tracking | Automatic | Manual |
| Topic caching | Built-in | Manual |

### EpidemicPubSub vs IPFS PubSub

| Feature | EpidemicPubSub | IPFS PubSub |
|---------|------------|-------------|
| Protocol | Gossipsub | Floodsub or Gossipsub |
| Performance | High (Rust native) | Medium (depends on impl) |
| Backend | Iroh embedded | External IPFS daemon |
| Dependencies | Minimal | Full IPFS stack |
| Latency | Low (~50ms) | Medium (~100-300ms) |

## Security Considerations

### Message Validation

Currently, messages are not validated. Consider implementing:

```rust
// Custom validation before publishing
fn validate_message(data: &[u8]) -> Result<()> {
    if data.len() > MAX_MESSAGE_SIZE {
        return Err(GuardianError::Other("Message too large"));
    }
    // Add signature verification, schema validation, etc.
    Ok(())
}
```

### Peer Authentication

Peers are identified by PeerId but not authenticated by default:

```rust
// Consider implementing peer verification
async fn verify_peer(&self, node_id: &PeerId) -> Result<bool> {
    // Check against allowlist
    // Verify peer signature
    // Check peer reputation
    Ok(true)
}
```

### Rate Limiting

Consider implementing rate limiting per peer:

```rust
// Track message rate per peer
let rate_limiter = RateLimiter::new(100, Duration::from_secs(60));

if !rate_limiter.check(node_id) {
    return Err(GuardianError::Other("Rate limit exceeded"));
}
```

## Testing

### Unit Testing

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_topic_subscribe_and_publish() {
        let iroh = Arc::new(IrohBackend::new(&config).await.unwrap());
        let mut pubsub = iroh.create_pubsub_interface();
        
        let topic = pubsub.topic_subscribe("test").await.unwrap();
        topic.publish(b"test message".to_vec()).await.unwrap();
    }
    
    #[tokio::test]
    async fn test_message_streaming() {
        let topic = pubsub.topic_subscribe("stream-test").await.unwrap();
        let mut stream = topic.watch_messages().await.unwrap();
        
        // Publish in background
        tokio::spawn(async move {
            topic.publish(b"message 1".to_vec()).await.unwrap();
        });
        
        // Receive message
        let msg = stream.next().await.unwrap();
        assert_eq!(msg.content, b"message 1");
    }
}
```

### Integration Testing

```rust
#[tokio::test]
async fn test_multi_peer_pubsub() {
    // Create two backends
    let backend1 = Arc::new(IrohBackend::new(&config1).await?);
    let backend2 = Arc::new(IrohBackend::new(&config2).await?);
    
    // Create pubsub interfaces
    let mut pubsub1 = backend1.create_pubsub_interface();
    let mut pubsub2 = backend2.create_pubsub_interface();
    
    // Subscribe to same topic
    let topic1 = pubsub1.topic_subscribe("test").await?;
    let topic2 = pubsub2.topic_subscribe("test").await?;
    
    // Watch messages on second peer
    let mut stream2 = topic2.watch_messages().await?;
    
    // Publish from first peer
    topic1.publish(b"Hello from peer 1".to_vec()).await?;
    
    // Receive on second peer
    let msg = stream2.next().await.unwrap();
    assert_eq!(msg.content, b"Hello from peer 1");
}
```

## Changelog

See the main Guardian DB changelog for version history.

## Future Enhancements

### Planned Features

- Topic cleanup/unsubscribe functionality
- Message filtering by sender
- Message size limits and validation
- Peer allowlist/blocklist
- Message encryption
- Persistence of topic state
- Topic discovery mechanism
- Mesh peer optimization

### Under Consideration

- Message acknowledgments
- Guaranteed delivery modes
- Message ordering guarantees
- Topic hierarchies
- Wildcard topic subscriptions
- Message TTL (Time To Live)

## License

This module is part of GuardianDB and follows the same licensing terms.

## Contributing

Contributions are welcome! Please follow the project's contribution guidelines.

## References

- [Iroh Backend Documentation](IROH-BACKEND.md)
- [libp2p Gossipsub Specification](https://github.com/libp2p/specs/blob/master/pubsub/gossipsub/README.md)
- [Tokio Broadcast Channels](https://docs.rs/tokio/latest/tokio/sync/broadcast/index.html)
