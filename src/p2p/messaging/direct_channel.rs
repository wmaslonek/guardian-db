use crate::guardian::error::{GuardianError, Result};
use crate::p2p::messaging::{CONNECTION_TIMEOUT, HEARTBEAT_INTERVAL, MAX_MESSAGE_SIZE, PROTOCOL};
use crate::p2p::network::core::IrohBackend;
use crate::traits::{
    DirectChannelEmitter, DirectChannelFactory, DirectChannelOptions, EventPubSubPayload,
};
use async_trait::async_trait;
use futures::StreamExt;
use iroh::EndpointId as NodeId;
use iroh_gossip::net::Gossip;
use iroh_gossip::proto::TopicId;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::Span;

type TopicMessageChannels = Arc<RwLock<HashMap<TopicId, broadcast::Sender<(NodeId, Vec<u8>)>>>>;

// Timeout for the beacon response (a fraction of CONNECTION_TIMEOUT).
const BEACON_TIMEOUT: Duration = Duration::from_secs(CONNECTION_TIMEOUT.as_secs() / 6);

// Direct channel protocol messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectChannelMessage {
    pub message_type: MessageType,
    pub payload: Vec<u8>,
    pub timestamp: u64,
    pub sender: String, // NodeId as string
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageType {
    Data,
    Heartbeat,
    Ack,
}

#[async_trait]
pub trait DirectChannelNetwork: Send + Sync {
    async fn publish_message(&self, topic: &TopicId, message: &[u8]) -> Result<()>;
    async fn subscribe_topic(&self, topic: &TopicId, bootstrap_peers: Vec<NodeId>) -> Result<()>;
    async fn get_connected_peers(&self) -> Vec<NodeId>;
    async fn get_topic_peers(&self, topic: &TopicId) -> Vec<NodeId>;

    /// Allows downcasting to concrete types.
    fn as_any(&self) -> &dyn std::any::Any;
}

// DirectChannelNetwork implementation using IrohBackend + iroh-gossip.
pub struct IrohBridge {
    span: Span,
    #[allow(dead_code)] // Kept for future reference.
    backend: Arc<IrohBackend>,
    gossip: Gossip,
    connected_peers: Arc<RwLock<Vec<NodeId>>>,
    topic_peers: Arc<RwLock<HashMap<TopicId, Vec<NodeId>>>>,
    subscribed_topics: Arc<RwLock<HashMap<TopicId, bool>>>,
    own_node_id: NodeId,
    // Per-topic message channels for the event loops to consume.
    topic_message_channels: TopicMessageChannels,
    // Active event loops per topic.
    topic_event_loops: Arc<RwLock<HashMap<TopicId, JoinHandle<()>>>>,
}

impl IrohBridge {
    pub async fn new(span: Span, backend: Arc<IrohBackend>) -> Result<Self> {
        // Get the endpoint from the IrohBackend.
        let endpoint_arc = backend.get_endpoint().await?;
        let endpoint_lock = endpoint_arc.read().await;
        let endpoint = endpoint_lock
            .as_ref()
            .ok_or_else(|| GuardianError::Other("Endpoint not available".to_string()))?
            .clone();
        let own_node_id = endpoint.id();
        drop(endpoint_lock);

        // Initialize gossip.
        let gossip = Gossip::builder()
            .max_message_size(backend.config().gossip.max_message_size)
            .spawn(endpoint);

        Ok(Self {
            span,
            backend,
            gossip,
            connected_peers: Arc::new(RwLock::new(Vec::new())),
            topic_peers: Arc::new(RwLock::new(HashMap::new())),
            subscribed_topics: Arc::new(RwLock::new(HashMap::new())),
            own_node_id,
            topic_message_channels: Arc::new(RwLock::new(HashMap::new())),
            topic_event_loops: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Returns a reference to the span used for instrumentation.
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Returns its own NodeId.
    pub fn node_id(&self) -> NodeId {
        self.own_node_id
    }

    pub async fn start(&self) -> Result<()> {
        let _entered = self.span.enter();
        tracing::info!("IrohBridge started with iroh-gossip");
        Ok(())
    }

    /// Updates the list of connected peers.
    pub async fn update_connected_peers(&self, peers: Vec<NodeId>) {
        let _entered = self.span.enter();
        let mut connected = self.connected_peers.write().await;
        *connected = peers.clone();

        tracing::debug!("Connected peers updated: {}", connected.len());
    }

    /// Updates the peers of a specific topic.
    pub async fn update_topic_peers(&self, topic: TopicId, peers: Vec<NodeId>) {
        let mut topic_peers = self.topic_peers.write().await;
        topic_peers.insert(topic, peers.clone());

        tracing::debug!(
            "Peers of topic {} updated: {}",
            topic.fmt_short(),
            peers.len()
        );
    }

    /// Message publication using iroh-gossip.
    async fn publish(&self, topic: &TopicId, message: &[u8]) -> Result<()> {
        // To publish, we need an active subscription to the topic.
        // Here we do a simplified publication - in production we would keep
        // a cache of active subscriptions for reuse.
        let subscribed_topics = self.subscribed_topics.read().await;
        if !subscribed_topics.contains_key(topic) {
            return Err(GuardianError::Other(format!(
                "Topic {} is not subscribed for publication",
                topic.fmt_short()
            )));
        }
        drop(subscribed_topics);

        // Subscribe again to obtain the GossipTopic handle (the idiomatic iroh-gossip method).
        let mut gossip_topic = self.gossip.subscribe(*topic, vec![]).await.map_err(|e| {
            GuardianError::Other(format!("Error accessing topic for publication: {}", e))
        })?;

        // Publish the message using broadcast.
        gossip_topic
            .broadcast(bytes::Bytes::copy_from_slice(message))
            .await
            .map_err(|e| {
                GuardianError::Other(format!("Error publishing message via iroh-gossip: {}", e))
            })?;

        tracing::debug!(
            "Message published via iroh-gossip on topic: {}",
            topic.fmt_short()
        );
        Ok(())
    }

    pub async fn stop(&self) -> Result<()> {
        // Gossip does not require an explicit stop.
        tracing::info!("IrohBridge stopped");
        Ok(())
    }

    /// Returns essential interface statistics.
    pub async fn get_interface_stats(&self) -> HashMap<String, u64> {
        let mut stats = HashMap::new();

        // Basic statistics.
        let connected = self.connected_peers.read().await;
        stats.insert(
            "interface_connected_peers".to_string(),
            connected.len() as u64,
        );

        let topics = self.topic_peers.read().await;
        stats.insert("interface_tracked_topics".to_string(), topics.len() as u64);

        stats
    }

    /// Generates a TopicId from a string using Blake3 (consistent with Iroh).
    fn topic_id_from_str(topic: &str) -> TopicId {
        let hash = blake3::hash(topic.as_bytes());
        TopicId::from_bytes(hash.into())
    }

    /// Gets a receiver for messages of a specific topic.
    /// Returns None if the topic is not subscribed.
    pub async fn get_topic_receiver(
        &self,
        topic: &TopicId,
    ) -> Option<broadcast::Receiver<(NodeId, Vec<u8>)>> {
        let channels = self.topic_message_channels.read().await;
        channels.get(topic).map(|sender| sender.subscribe())
    }
}

#[async_trait]
impl DirectChannelNetwork for IrohBridge {
    async fn publish_message(&self, topic: &TopicId, message: &[u8]) -> Result<()> {
        tracing::debug!(
            "Publicando mensagem no tópico: {}, {} bytes",
            topic.fmt_short(),
            message.len()
        );

        // Publica via iroh-gossip (método assíncrono nativo)
        self.publish(topic, message).await?;

        tracing::info!(
            "Mensagem publicada com sucesso no tópico via iroh-gossip: {}",
            topic.fmt_short()
        );
        Ok(())
    }

    async fn subscribe_topic(&self, topic: &TopicId, bootstrap_peers: Vec<NodeId>) -> Result<()> {
        tracing::debug!(
            "Subscribing to topic: {} with {} bootstrap peers",
            topic.fmt_short(),
            bootstrap_peers.len()
        );

        // Check whether it is already subscribed.
        {
            let topics = self.subscribed_topics.read().await;
            if topics.contains_key(topic) {
                tracing::debug!(
                    "Topic {} is already subscribed, re-subscribing with new peers",
                    topic.fmt_short()
                );
                // If already subscribed, re-subscribe with new peers to add them to the mesh.
                if !bootstrap_peers.is_empty() {
                    let gossip_topic_new = self
                        .gossip
                        .subscribe(*topic, bootstrap_peers.clone())
                        .await
                        .map_err(|e| {
                            GuardianError::Other(format!("Error re-subscribing to topic: {}", e))
                        })?;

                    // FIX: We do NOT discard the new stream. We need to process events from it.
                    // Get the existing message channel to forward to.
                    let message_tx = {
                        let channels = self.topic_message_channels.read().await;
                        channels.get(topic).cloned()
                    };

                    if let Some(tx) = message_tx {
                        let topic_id = *topic;
                        let topic_peers_map = self.topic_peers.clone();
                        let span = self.span.clone();

                        // Spawn an additional event loop for the new subscription.
                        tokio::spawn(async move {
                            let _entered = span.enter();
                            let mut gossip_topic = gossip_topic_new;
                            tracing::info!(
                                "[PEER_MESH] Additional event loop started for topic {} with new peers",
                                topic_id.fmt_short()
                            );

                            while let Some(event_result) = gossip_topic.next().await {
                                match event_result {
                                    Ok(iroh_gossip::api::Event::Received(msg)) => {
                                        tracing::info!(
                                            "[PEER_MESH] Message received via the new mesh on topic {}: {} bytes from peer {}",
                                            topic_id.fmt_short(),
                                            msg.content.len(),
                                            msg.delivered_from
                                        );
                                        let _ = tx.send((msg.delivered_from, msg.content.to_vec()));
                                    }
                                    Ok(iroh_gossip::api::Event::NeighborUp(peer_id)) => {
                                        tracing::info!(
                                            "[PEER_MESH] Peer {} connected to topic {} via the new mesh",
                                            peer_id,
                                            topic_id.fmt_short()
                                        );
                                        let mut peers = topic_peers_map.write().await;
                                        peers
                                            .entry(topic_id)
                                            .or_insert_with(Vec::new)
                                            .push(peer_id);
                                    }
                                    Ok(iroh_gossip::api::Event::NeighborDown(peer_id)) => {
                                        tracing::debug!(
                                            "[PEER_MESH] Peer {} disconnected from topic {} via the new mesh",
                                            peer_id,
                                            topic_id.fmt_short()
                                        );
                                        let mut peers = topic_peers_map.write().await;
                                        if let Some(peer_list) = peers.get_mut(&topic_id) {
                                            peer_list.retain(|p| *p != peer_id);
                                        }
                                    }
                                    Ok(iroh_gossip::api::Event::Lagged) => {
                                        tracing::warn!(
                                            "[PEER_MESH] Event loop lagging on topic {} (new mesh)",
                                            topic_id.fmt_short()
                                        );
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            "[PEER_MESH] Error in the event stream of topic {} (new mesh): {}",
                                            topic_id.fmt_short(),
                                            e
                                        );
                                        break;
                                    }
                                }
                            }

                            tracing::debug!(
                                "[PEER_MESH] Event loop ended for topic {} (new mesh)",
                                topic_id.fmt_short()
                            );
                        });

                        tracing::info!(
                            "Re-subscription with an additional event loop performed for topic: {}",
                            topic.fmt_short()
                        );
                    } else {
                        tracing::warn!(
                            "Message channel not found for topic {} - discarding the re-subscription",
                            topic.fmt_short()
                        );
                    }
                }
                return Ok(());
            }
        }

        // Mark the topic as subscribed.
        {
            let mut topics = self.subscribed_topics.write().await;
            topics.insert(*topic, true);
            let mut topic_peers = self.topic_peers.write().await;
            topic_peers.entry(*topic).or_insert_with(Vec::new);
        }

        // Create a message channel for this topic (capacity: 100 messages).
        let (message_tx, _message_rx) = broadcast::channel::<(NodeId, Vec<u8>)>(100);

        {
            let mut channels = self.topic_message_channels.write().await;
            channels.insert(*topic, message_tx.clone());
        }

        // Subscribe via iroh-gossip with bootstrap peers.
        let mut gossip_topic = self
            .gossip
            .subscribe(*topic, bootstrap_peers.clone())
            .await
            .map_err(|e| GuardianError::Other(format!("Error subscribing to topic: {}", e)))?;

        // Create an event loop to process received messages.
        let topic_id = *topic;
        let topic_peers_map = self.topic_peers.clone();
        let span = self.span.clone();

        let event_loop = tokio::spawn(async move {
            let _entered = span.enter();
            tracing::info!("Event loop started for topic: {}", topic_id.fmt_short());

            while let Some(event_result) = gossip_topic.next().await {
                match event_result {
                    Ok(iroh_gossip::api::Event::Received(msg)) => {
                        tracing::debug!(
                            "Message received on topic {}: {} bytes from peer {}",
                            topic_id.fmt_short(),
                            msg.content.len(),
                            msg.delivered_from
                        );

                        // Send the message to the channel (ignore errors if there are no receivers).
                        let _ = message_tx.send((msg.delivered_from, msg.content.to_vec()));
                    }
                    Ok(iroh_gossip::api::Event::NeighborUp(peer_id)) => {
                        tracing::debug!(
                            "Peer {} connected to topic {}",
                            peer_id,
                            topic_id.fmt_short()
                        );
                        let mut peers = topic_peers_map.write().await;
                        peers.entry(topic_id).or_insert_with(Vec::new).push(peer_id);
                    }
                    Ok(iroh_gossip::api::Event::NeighborDown(peer_id)) => {
                        tracing::debug!(
                            "Peer {} disconnected from topic {}",
                            peer_id,
                            topic_id.fmt_short()
                        );
                        let mut peers = topic_peers_map.write().await;
                        if let Some(peer_list) = peers.get_mut(&topic_id) {
                            peer_list.retain(|p| *p != peer_id);
                        }
                    }
                    Ok(iroh_gossip::api::Event::Lagged) => {
                        tracing::warn!("Event loop lagging on topic {}", topic_id.fmt_short());
                    }
                    Err(e) => {
                        tracing::error!(
                            "Error in the event stream of topic {}: {}",
                            topic_id.fmt_short(),
                            e
                        );
                        break;
                    }
                }
            }

            tracing::info!("Event loop ended for topic: {}", topic_id.fmt_short());
        });

        // Store the event loop handle.
        {
            let mut loops = self.topic_event_loops.write().await;
            loops.insert(*topic, event_loop);
        }

        tracing::info!(
            "Successfully subscribed to topic via iroh-gossip: {} with {} peers",
            topic.fmt_short(),
            bootstrap_peers.len()
        );
        Ok(())
    }

    async fn get_connected_peers(&self) -> Vec<NodeId> {
        let peers = self.connected_peers.read().await;
        let peer_list = peers.clone();
        tracing::debug!("Returning {} connected peers", peer_list.len());
        peer_list
    }

    async fn get_topic_peers(&self, topic: &TopicId) -> Vec<NodeId> {
        tracing::debug!("Getting peers of topic: {}", topic.fmt_short());

        let topic_peers = self.topic_peers.read().await;
        let peers = topic_peers.get(topic).cloned().unwrap_or_default();

        tracing::debug!(
            "Topic {} has {} connected peers",
            topic.fmt_short(),
            peers.len()
        );
        peers
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// Internal state of the DirectChannel.
#[derive(Debug, Clone)]
struct ChannelState {
    #[allow(dead_code)]
    node_id: NodeId,
    topic: TopicId,
    connection_status: ConnectionStatus,
    last_activity: Instant,
    message_count: u64,
    last_heartbeat: Instant,
}

#[derive(Debug, Clone)]
enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    #[allow(dead_code)]
    Error(String),
}

// Internal DirectChannel events.
#[derive(Debug)]
enum DirectChannelEvent {
    PeerConnected(NodeId),
    PeerDisconnected(NodeId),
    MessageReceived {
        peer: NodeId,
        payload: Vec<u8>,
    },
    MessageSent {
        peer: NodeId,
        success: bool,
        error: Option<String>,
    },
    HeartbeatReceived(NodeId),
    HeartbeatTimeout(NodeId),
}

pub struct DirectChannel {
    span: Span,
    iroh_network: Arc<dyn DirectChannelNetwork>,
    emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
    channels: Arc<RwLock<HashMap<NodeId, ChannelState>>>,
    event_sender: mpsc::UnboundedSender<DirectChannelEvent>,
    _event_receiver: Arc<Mutex<Option<mpsc::UnboundedReceiver<DirectChannelEvent>>>>,
    own_node_id: NodeId,
    running: Arc<Mutex<bool>>,
}

impl DirectChannel {
    // Public constructor.
    pub fn new(
        span: Span,
        iroh_network: Arc<dyn DirectChannelNetwork>,
        emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
        own_node_id: NodeId,
    ) -> Self {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();

        Self {
            span,
            iroh_network,
            emitter,
            channels: Arc::new(RwLock::new(HashMap::new())),
            event_sender,
            _event_receiver: Arc::new(Mutex::new(Some(event_receiver))),
            own_node_id,
            running: Arc::new(Mutex::new(false)),
        }
    }

    // Generates the unique topic for communication with a specific peer.
    fn get_channel_topic(&self, peer: NodeId) -> TopicId {
        // Sort the node IDs to ensure the same topic on both sides.
        let (first, second) = if self.own_node_id.as_bytes() < peer.as_bytes() {
            (self.own_node_id, peer)
        } else {
            (peer, self.own_node_id)
        };
        let topic_string = format!("{}/channel/{}/{}", PROTOCOL, first, second);
        IrohBridge::topic_id_from_str(&topic_string)
    }

    // Starts event processing.
    pub async fn start(&self) -> Result<()> {
        let mut running = self.running.lock().await;
        if *running {
            return Ok(());
        }
        *running = true;

        let mut receiver = self
            ._event_receiver
            .lock()
            .await
            .take()
            .ok_or_else(|| GuardianError::Other("Event receiver already taken".to_string()))?;

        let emitter = self.emitter.clone();
        let span = self.span.clone();
        let channels = self.channels.clone();
        let running_flag = self.running.clone();

        tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                let running = *running_flag.lock().await;
                if !running {
                    break;
                }

                if let Err(e) = Self::handle_event(event, &emitter, &span, &channels).await {
                    tracing::error!("Error processing event: {}", e);
                }
            }
            tracing::info!("Event processing loop terminated");
        });

        // Start the heartbeat loop.
        self.start_heartbeat_loop().await;

        Ok(())
    }

    // Starts the heartbeat loop to keep connections alive.
    async fn start_heartbeat_loop(&self) {
        let channels = self.channels.clone();
        let event_sender = self.event_sender.clone();
        let span = self.span.clone();
        let running_flag = self.running.clone();
        let iroh_network = self.iroh_network.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);

            loop {
                interval.tick().await;

                let running = *running_flag.lock().await;
                if !running {
                    break;
                }

                let peers_to_heartbeat: Vec<(NodeId, TopicId)> = {
                    let channels_map = channels.read().await;
                    channels_map
                        .iter()
                        .filter_map(|(node_id, state)| {
                            match state.connection_status {
                                ConnectionStatus::Connected => {
                                    // Check whether a heartbeat is needed.
                                    if state.last_heartbeat.elapsed() > HEARTBEAT_INTERVAL {
                                        Some((*node_id, state.topic))
                                    } else {
                                        None
                                    }
                                }
                                _ => None,
                            }
                        })
                        .collect()
                };

                for (peer, topic) in peers_to_heartbeat {
                    // Send a heartbeat.
                    if let Err(e) = Self::send_heartbeat(&iroh_network, &topic, &span).await {
                        tracing::warn!("Failed to send heartbeat to {}: {}", peer, e);
                        let _ = event_sender.send(DirectChannelEvent::HeartbeatTimeout(peer));
                    } else {
                        tracing::trace!(peer = %peer, "Heartbeat sent to peer");
                    }
                }

                // Check peers in an error state and try to reconnect.
                let peers_to_reconnect: Vec<NodeId> = {
                    let channels_map = channels.read().await;
                    channels_map
                        .iter()
                        .filter_map(|(node_id, state)| {
                            match &state.connection_status {
                                ConnectionStatus::Error(err) => {
                                    // Try to reconnect after 30 seconds in error.
                                    if state.last_activity.elapsed() > Duration::from_secs(30) {
                                        tracing::debug!(
                                            "Attempting to reconnect with peer {} after error: {}",
                                            node_id,
                                            err
                                        );
                                        Some(*node_id)
                                    } else {
                                        None
                                    }
                                }
                                ConnectionStatus::Disconnected => {
                                    // Try to reconnect disconnected peers after 60 seconds.
                                    if state.last_activity.elapsed() > Duration::from_secs(60) {
                                        tracing::debug!(
                                            "Attempting to reconnect with disconnected peer: {}",
                                            node_id
                                        );
                                        Some(*node_id)
                                    } else {
                                        None
                                    }
                                }
                                _ => None,
                            }
                        })
                        .collect()
                };

                // Update the state to "Connecting" and try to reconnect.
                for peer in peers_to_reconnect {
                    let mut channels_map = channels.write().await;
                    if let Some(state) = channels_map.get_mut(&peer) {
                        state.connection_status = ConnectionStatus::Connecting;
                        state.last_activity = Instant::now();

                        // Try to reconnect (discovery beacon).
                        if let Err(e) =
                            Self::send_heartbeat(&iroh_network, &state.topic, &span).await
                        {
                            tracing::warn!("Reconnection attempt failed with {}: {}", peer, e);
                        } else {
                            tracing::info!("Reconnection attempt started for peer: {}", peer);
                        }
                    }
                }
            }
        });
    }

    // Sends a heartbeat to a specific topic.
    async fn send_heartbeat(
        iroh_network: &Arc<dyn DirectChannelNetwork>,
        topic: &TopicId,
        _span: &Span,
    ) -> Result<()> {
        let heartbeat_msg = DirectChannelMessage {
            message_type: MessageType::Heartbeat,
            payload: vec![],
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            sender: "heartbeat".to_string(),
        };

        let serialized = serde_cbor::to_vec(&heartbeat_msg)
            .map_err(|e| GuardianError::Other(format!("Heartbeat serialization error: {}", e)))?;

        iroh_network.publish_message(topic, &serialized).await?;
        tracing::trace!(topic = %topic.fmt_short(), "Heartbeat sent on topic");
        Ok(())
    }

    // Processes internal events.
    async fn handle_event(
        event: DirectChannelEvent,
        emitter: &Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
        _span: &Span,
        channels: &Arc<RwLock<HashMap<NodeId, ChannelState>>>,
    ) -> Result<()> {
        match event {
            DirectChannelEvent::MessageReceived { peer, payload } => {
                tracing::debug!("Message received from {}: {} bytes", peer, payload.len());

                // Validate the message size.
                if payload.len() > MAX_MESSAGE_SIZE {
                    tracing::warn!("Message too large from {}: {} bytes", peer, payload.len());
                    return Ok(());
                }

                // Update the channel activity.
                {
                    let mut channels_map = channels.write().await;
                    if let Some(state) = channels_map.get_mut(&peer) {
                        state.last_activity = Instant::now();
                        state.message_count += 1;
                    }
                }

                let event_payload = EventPubSubPayload { payload, peer };
                emitter
                    .emit(event_payload)
                    .await
                    .map_err(|e| GuardianError::Other(format!("Error emitting event: {}", e)))?;
            }
            DirectChannelEvent::PeerConnected(peer) => {
                tracing::info!("Peer connected: {}", peer);
                let mut channels_map = channels.write().await;
                if let Some(state) = channels_map.get_mut(&peer) {
                    state.connection_status = ConnectionStatus::Connected;
                    state.last_activity = Instant::now();
                    state.last_heartbeat = Instant::now();
                }
            }
            DirectChannelEvent::PeerDisconnected(peer) => {
                tracing::info!("Peer disconnected: {}", peer);
                let mut channels_map = channels.write().await;
                if let Some(state) = channels_map.get_mut(&peer) {
                    state.connection_status = ConnectionStatus::Disconnected;
                }
            }
            DirectChannelEvent::MessageSent {
                peer,
                success,
                error,
            } => {
                if success {
                    tracing::debug!("Message sent successfully to: {}", peer);
                } else {
                    tracing::warn!("Failed to send message to {}: {:?}", peer, error);
                }
            }
            DirectChannelEvent::HeartbeatReceived(peer) => {
                tracing::trace!(peer = %peer, "Heartbeat received from");
                let mut channels_map = channels.write().await;
                if let Some(state) = channels_map.get_mut(&peer) {
                    state.last_activity = Instant::now();
                    state.last_heartbeat = Instant::now();
                }
            }
            DirectChannelEvent::HeartbeatTimeout(peer) => {
                tracing::warn!("Heartbeat timeout for peer: {}", peer);
                let mut channels_map = channels.write().await;
                if let Some(state) = channels_map.get_mut(&peer) {
                    state.connection_status =
                        ConnectionStatus::Error("Heartbeat timeout".to_string());
                }
            }
        }
        Ok(())
    }

    // Sends data to a specific peer.
    pub async fn send_data(&self, peer: NodeId, payload: Vec<u8>) -> Result<()> {
        if payload.len() > MAX_MESSAGE_SIZE {
            return Err(GuardianError::Other(format!(
                "Message too large: {} bytes (maximum: {})",
                payload.len(),
                MAX_MESSAGE_SIZE
            )));
        }

        let topic = self.get_channel_topic(peer);
        let message = DirectChannelMessage {
            message_type: MessageType::Data,
            payload,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            sender: self.own_node_id.to_string(),
        };

        let serialized = serde_cbor::to_vec(&message)
            .map_err(|e| GuardianError::Other(format!("Serialization error: {}", e)))?;

        match self.iroh_network.publish_message(&topic, &serialized).await {
            Ok(()) => {
                let _ = self.event_sender.send(DirectChannelEvent::MessageSent {
                    peer,
                    success: true,
                    error: None,
                });
                tracing::debug!("Data sent to {}: {} bytes", peer, message.payload.len());
                Ok(())
            }
            Err(e) => {
                let error_msg = format!("Error publishing message: {}", e);
                let _ = self.event_sender.send(DirectChannelEvent::MessageSent {
                    peer,
                    success: false,
                    error: Some(error_msg.clone()),
                });
                Err(GuardianError::Other(error_msg))
            }
        }
    }

    // Connects to a specific peer.
    pub async fn connect_to_peer(&self, peer: NodeId) -> Result<()> {
        let topic = self.get_channel_topic(peer);
        let mut channels_map = self.channels.write().await;

        if let Some(state) = channels_map.get(&peer) {
            match state.connection_status {
                ConnectionStatus::Connected => {
                    tracing::debug!("Already connected to peer: {}", peer);
                    return Ok(());
                }
                ConnectionStatus::Connecting => {
                    tracing::debug!("Connection in progress with peer: {}", peer);
                    return Ok(());
                }
                _ => {}
            }
        }

        // Add or update the channel state.
        channels_map.insert(
            peer,
            ChannelState {
                node_id: peer,
                topic,
                connection_status: ConnectionStatus::Connecting,
                last_activity: Instant::now(),
                message_count: 0,
                last_heartbeat: Instant::now(),
            },
        );
        drop(channels_map); // Release the lock before asynchronous operations.

        // Subscribe to the topic (creates an event loop in IrohBridge if one does not exist yet).
        // IMPORTANT: Pass the peer as bootstrap to form the gossip mesh.
        self.iroh_network
            .subscribe_topic(&topic, vec![peer])
            .await?;

        // Start a consumer loop to process messages received on this topic.
        self.start_message_consumer_for_topic(topic).await?;

        tracing::info!(
            "Connecting to peer {} on topic: {}",
            peer,
            topic.fmt_short()
        );
        self.establish_peer_connection(peer, topic).await?;
        Ok(())
    }

    /// Starts a loop to consume messages from a specific topic.
    async fn start_message_consumer_for_topic(&self, topic: TopicId) -> Result<()> {
        // Downcast to access the concrete IrohBridge methods.
        let iroh_bridge = self
            .iroh_network
            .as_any()
            .downcast_ref::<IrohBridge>()
            .ok_or_else(|| GuardianError::Other("Cannot downcast to IrohBridge".to_string()))?;

        // Get a receiver for messages of this topic.
        let Some(mut receiver): Option<broadcast::Receiver<(NodeId, Vec<u8>)>> =
            iroh_bridge.get_topic_receiver(&topic).await
        else {
            return Err(GuardianError::Other(format!(
                "Could not get a receiver for topic: {}",
                topic.fmt_short()
            )));
        };

        // Capture only the parts of self that are needed.
        let event_sender = self.event_sender.clone();
        let span = self.span.clone();

        // Spawn a task to process messages.
        tokio::spawn(async move {
            let _entered = span.enter();
            tracing::debug!("Consumer loop started for topic: {}", topic.fmt_short());

            loop {
                match receiver.recv().await {
                    Ok((peer, data)) => {
                        tracing::debug!(
                            "Message received from peer {} on topic {}: {} bytes",
                            peer,
                            topic.fmt_short(),
                            data.len()
                        );

                        // Deserialize the DirectChannel message.
                        match serde_cbor::from_slice::<DirectChannelMessage>(&data) {
                            Ok(decoded_msg) => {
                                match decoded_msg.message_type {
                                    MessageType::Data => {
                                        // Send a message-received event.
                                        let _ = event_sender.send(
                                            DirectChannelEvent::MessageReceived {
                                                peer,
                                                payload: decoded_msg.payload,
                                            },
                                        );
                                    }
                                    MessageType::Heartbeat => {
                                        // Send a heartbeat-received event.
                                        let _ = event_sender
                                            .send(DirectChannelEvent::HeartbeatReceived(peer));
                                    }
                                    MessageType::Ack => {
                                        // Process the ACK/handshake.
                                        tracing::trace!("ACK received from: {}", peer);
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Error decoding message from {} on topic {}: {}",
                                    peer,
                                    topic.fmt_short(),
                                    e
                                );
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            "Consumer loop lagging on topic {}: {} messages lost",
                            topic.fmt_short(),
                            n
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::info!("Channel closed for topic: {}", topic.fmt_short());
                        break;
                    }
                }
            }

            tracing::debug!("Consumer loop ended for topic: {}", topic.fmt_short());
        });

        Ok(())
    }

    // Establishes a connection with a specific peer.
    async fn establish_peer_connection(&self, peer: NodeId, topic: TopicId) -> Result<()> {
        tracing::debug!("Establishing connection with peer: {}", peer);

        // 1. Check whether the peer is already among the connected peers.
        let connected_peers = self.iroh_network.get_connected_peers().await;
        let is_peer_connected = connected_peers.contains(&peer);

        if is_peer_connected {
            tracing::debug!("Peer {} is already connected globally", peer);
            // Send a connection-established event.
            let _ = self
                .event_sender
                .send(DirectChannelEvent::PeerConnected(peer));
            return Ok(());
        }

        // 2. Wait for a while for peer discovery on the topic.
        let discovery_timeout = CONNECTION_TIMEOUT;
        let start_time = Instant::now();

        while start_time.elapsed() < discovery_timeout {
            // Check the peers of the specific topic.
            let topic_peers = self.iroh_network.get_topic_peers(&topic).await;

            if topic_peers.contains(&peer) {
                tracing::info!("Peer {} discovered on topic: {}", peer, topic.fmt_short());

                // Send a handshake message to verify connectivity.
                if self.send_handshake_message(&topic, peer).await.is_ok() {
                    tracing::info!("Handshake successful with peer: {}", peer);
                    let _ = self
                        .event_sender
                        .send(DirectChannelEvent::PeerConnected(peer));
                    return Ok(());
                }
            }

            // Check the global peers again.
            let updated_peers = self.iroh_network.get_connected_peers().await;
            if updated_peers.contains(&peer) {
                tracing::info!("Peer {} connected via global discovery", peer);
                let _ = self
                    .event_sender
                    .send(DirectChannelEvent::PeerConnected(peer));
                return Ok(());
            }

            // Wait before the next check.
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // 3. If a direct connection could not be made, try sending a beacon.
        tracing::warn!(
            "Peer {} not found directly, sending a discovery beacon",
            peer
        );
        if let Err(e) = self.send_discovery_beacon(&topic, peer).await {
            tracing::error!("Failed to send discovery beacon to {}: {}", peer, e);

            // Mark it as a connection error but do not fail completely.
            let mut channels_map = self.channels.write().await;
            if let Some(state) = channels_map.get_mut(&peer) {
                state.connection_status =
                    ConnectionStatus::Error(format!("Discovery timeout: {}", e));
            }

            return Err(GuardianError::Other(format!(
                "Timeout discovering peer {} after {}s",
                peer,
                discovery_timeout.as_secs()
            )));
        }

        // 4. Wait for a response to the beacon for an additional limited time.
        let beacon_timeout = BEACON_TIMEOUT;
        let beacon_start = Instant::now();

        while beacon_start.elapsed() < beacon_timeout {
            let topic_peers = self.iroh_network.get_topic_peers(&topic).await;
            if topic_peers.contains(&peer) {
                tracing::info!("Peer {} responded to the discovery beacon", peer);
                let _ = self
                    .event_sender
                    .send(DirectChannelEvent::PeerConnected(peer));
                return Ok(());
            }

            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        // 5. Connection not established - keep the state as "Connecting" for a future retry.
        tracing::warn!(
            "Connection with peer {} could not be established at the moment",
            peer
        );
        Ok(())
    }

    // Sends a handshake message to verify connectivity.
    async fn send_handshake_message(&self, topic: &TopicId, target_peer: NodeId) -> Result<()> {
        let handshake_msg = DirectChannelMessage {
            message_type: MessageType::Ack, // Use ACK as the handshake.
            payload: format!("handshake:{}", self.own_node_id).into_bytes(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            sender: self.own_node_id.to_string(),
        };

        let serialized = serde_cbor::to_vec(&handshake_msg)
            .map_err(|e| GuardianError::Other(format!("Handshake serialization error: {}", e)))?;

        self.iroh_network
            .publish_message(topic, &serialized)
            .await?;
        tracing::debug!("Handshake sent to peer: {}", target_peer);
        Ok(())
    }

    // Sends a discovery beacon to attract peers.
    async fn send_discovery_beacon(&self, topic: &TopicId, target_peer: NodeId) -> Result<()> {
        let beacon_msg = DirectChannelMessage {
            message_type: MessageType::Heartbeat, // Use Heartbeat as the beacon.
            payload: format!("discovery_beacon:{}:{}", self.own_node_id, target_peer).into_bytes(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            sender: self.own_node_id.to_string(),
        };

        let serialized = serde_cbor::to_vec(&beacon_msg)
            .map_err(|e| GuardianError::Other(format!("Beacon serialization error: {}", e)))?;

        self.iroh_network
            .publish_message(topic, &serialized)
            .await?;
        tracing::debug!("Discovery beacon sent on topic: {}", topic.fmt_short());
        Ok(())
    }

    // Processes a message received from iroh-gossip.
    pub async fn handle_iroh_message(
        &self,
        message_data: &[u8],
        sender_peer: NodeId,
    ) -> Result<()> {
        // Decode the message.
        let decoded_msg: DirectChannelMessage = serde_cbor::from_slice(message_data)
            .map_err(|e| GuardianError::Other(format!("Error decoding message: {}", e)))?;

        match decoded_msg.message_type {
            MessageType::Data => {
                let _ = self.event_sender.send(DirectChannelEvent::MessageReceived {
                    peer: sender_peer,
                    payload: decoded_msg.payload,
                });
            }
            MessageType::Heartbeat => {
                // Check whether it is a discovery beacon.
                if let Ok(payload_str) = String::from_utf8(decoded_msg.payload.clone()) {
                    if payload_str.starts_with("discovery_beacon:") {
                        self.handle_discovery_beacon(sender_peer, payload_str)
                            .await?;
                    } else {
                        let _ = self
                            .event_sender
                            .send(DirectChannelEvent::HeartbeatReceived(sender_peer));
                    }
                } else {
                    let _ = self
                        .event_sender
                        .send(DirectChannelEvent::HeartbeatReceived(sender_peer));
                }
            }
            MessageType::Ack => {
                // Check whether it is a handshake.
                if let Ok(payload_str) = String::from_utf8(decoded_msg.payload.clone()) {
                    if payload_str.starts_with("handshake:") {
                        self.handle_handshake_response(sender_peer, payload_str)
                            .await?;
                    } else {
                        tracing::trace!(sender_peer = %sender_peer, "ACK received from");
                    }
                } else {
                    tracing::trace!(sender_peer = %sender_peer, "ACK received from");
                }
            }
        }

        Ok(())
    }

    // Processes a received discovery beacon.
    async fn handle_discovery_beacon(
        &self,
        sender_peer: NodeId,
        beacon_payload: String,
    ) -> Result<()> {
        tracing::debug!(
            "Discovery beacon received from: {} - {}",
            sender_peer,
            beacon_payload
        );

        // Parse the beacon: "discovery_beacon:sender_peer:target_peer".
        let parts: Vec<&str> = beacon_payload.split(':').collect();
        if parts.len() >= 3 {
            let _beacon_sender = parts[1]; // ID of the original sender.
            let beacon_target = parts[2];

            // Check whether we are the target of the beacon.
            if beacon_target == self.own_node_id.to_string() {
                tracing::info!("Discovery beacon directed at us from: {}", sender_peer);

                // Respond with a handshake if we are not connected yet.
                let channels_map = self.channels.read().await;
                if let Some(state) = channels_map.get(&sender_peer)
                    && matches!(
                        state.connection_status,
                        ConnectionStatus::Connecting | ConnectionStatus::Disconnected
                    )
                {
                    drop(channels_map); // Release the lock.

                    // Respond to the beacon.
                    let topic = self.get_channel_topic(sender_peer);
                    if let Err(e) = self.send_handshake_message(&topic, sender_peer).await {
                        tracing::warn!("Failed to respond to beacon from {}: {}", sender_peer, e);
                    } else {
                        tracing::info!("Response handshake sent to: {}", sender_peer);
                    }
                }
            }
        }

        Ok(())
    }

    // Processes a handshake response.
    async fn handle_handshake_response(
        &self,
        sender_peer: NodeId,
        handshake_payload: String,
    ) -> Result<()> {
        tracing::debug!(
            "Handshake received from: {} - {}",
            sender_peer,
            handshake_payload
        );

        // Parse the handshake: "handshake:node_id".
        let parts: Vec<&str> = handshake_payload.split(':').collect();
        if parts.len() >= 2 {
            let handshake_peer = parts[1];
            tracing::info!(
                "Valid handshake received from peer: {} (id: {})",
                sender_peer,
                handshake_peer
            );

            // Update the state to connected if it was still connecting.
            let mut channels_map = self.channels.write().await;
            if let Some(state) = channels_map.get_mut(&sender_peer) {
                match state.connection_status {
                    ConnectionStatus::Connecting => {
                        state.connection_status = ConnectionStatus::Connected;
                        state.last_activity = Instant::now();
                        state.last_heartbeat = Instant::now();

                        // Notify that the connection was established.
                        let _ = self
                            .event_sender
                            .send(DirectChannelEvent::PeerConnected(sender_peer));

                        tracing::info!("Connection established with peer: {}", sender_peer);
                    }
                    ConnectionStatus::Connected => {
                        // Update only the timestamps.
                        state.last_activity = Instant::now();
                        state.last_heartbeat = Instant::now();
                        tracing::trace!("Maintenance handshake received from: {}", sender_peer);
                    }
                    _ => {
                        tracing::debug!(
                            "Handshake received from peer in state: {:?}",
                            state.connection_status
                        );
                    }
                }
            }
        }

        Ok(())
    }

    // Stops the DirectChannel.
    pub async fn stop(&self) -> Result<()> {
        let mut running = self.running.lock().await;
        *running = false;

        // Disconnect all peers.
        let peers: Vec<NodeId> = {
            let channels_map = self.channels.read().await;
            channels_map.keys().cloned().collect()
        };

        for peer in peers {
            let mut channels_map = self.channels.write().await;
            if let Some(state) = channels_map.remove(&peer) {
                tracing::info!(
                    "Peer removed: {} (topic: {})",
                    peer,
                    state.topic.fmt_short()
                );
                let _ = self
                    .event_sender
                    .send(DirectChannelEvent::PeerDisconnected(peer));
            }
        }

        tracing::info!("DirectChannel stopped");
        Ok(())
    }

    // Lists connected peers.
    pub async fn list_connected_peers(&self) -> Vec<NodeId> {
        let channels_map = self.channels.read().await;
        channels_map
            .iter()
            .filter_map(|(node_id, state)| match state.connection_status {
                ConnectionStatus::Connected => Some(*node_id),
                _ => None,
            })
            .collect()
    }

    // Gets channel statistics.
    pub async fn get_channel_stats(&self) -> HashMap<NodeId, (u64, Duration)> {
        let channels_map = self.channels.read().await;
        channels_map
            .iter()
            .map(|(node_id, state)| {
                (
                    *node_id,
                    (state.message_count, state.last_activity.elapsed()),
                )
            })
            .collect()
    }

    /// Unified internal method for closing.
    async fn close_internal(&self) -> Result<()> {
        tracing::info!("Closing DirectChannel...");

        // Stop processing.
        self.stop().await?;

        // Close the emitter.
        if let Err(e) = self.emitter.close().await {
            tracing::warn!("Error closing emitter: {}", e);
        }

        tracing::info!("DirectChannel closed successfully");
        Ok(())
    }
}

// DirectChannel trait implementation from traits.rs.
#[async_trait]
impl crate::traits::DirectChannel for DirectChannel {
    type Error = GuardianError;

    async fn connect(&mut self, peer: NodeId) -> std::result::Result<(), Self::Error> {
        tracing::info!("Connecting to peer: {}", peer);
        self.connect_to_peer(peer).await
    }

    async fn send(&mut self, peer: NodeId, data: Vec<u8>) -> std::result::Result<(), Self::Error> {
        tracing::debug!("Sending {} bytes to {}", data.len(), peer);
        self.send_data(peer, data).await
    }

    async fn close(&mut self) -> std::result::Result<(), Self::Error> {
        self.close_internal().await
    }

    async fn close_shared(&self) -> std::result::Result<(), Self::Error> {
        self.close_internal().await
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct HolderChannels {
    iroh_network: Arc<dyn DirectChannelNetwork>,
    span: Span,
    own_node_id: NodeId,
}

impl HolderChannels {
    pub fn new(
        span: Span,
        iroh_network: Arc<dyn DirectChannelNetwork>,
        own_node_id: NodeId,
    ) -> Self {
        Self {
            iroh_network,
            span,
            own_node_id,
        }
    }

    pub async fn new_channel(
        &self,
        emitter: Box<dyn DirectChannelEmitter<Error = GuardianError>>,
        opts: Option<DirectChannelOptions>,
    ) -> Result<Box<dyn crate::traits::DirectChannel<Error = GuardianError>>> {
        let resolved_opts = opts.unwrap_or_default();
        let span = resolved_opts.span.unwrap_or_else(|| self.span.clone());

        let dc = DirectChannel::new(
            span.clone(),
            self.iroh_network.clone(),
            Arc::from(emitter),
            self.own_node_id,
        );

        // Start processing.
        dc.start().await?;

        tracing::info!(protocol = PROTOCOL, "DirectChannel created with protocol");

        Ok(Box::new(dc))
    }
}

pub fn init_direct_channel_factory(
    span: Span,
    own_node_id: NodeId,
    backend: Arc<IrohBackend>,
) -> DirectChannelFactory {
    Arc::new(
        move |emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
              opts: Option<DirectChannelOptions>| {
            let span = span.clone();
            let own_node_id = own_node_id;
            let backend = backend.clone();
            Box::pin(async move {
                tracing::info!(
                    "Initializing DirectChannel factory for node: {}",
                    own_node_id
                );

                // Create an interface for Iroh using IrohBridge.
                let iroh_interface = Arc::new(
                    create_unified_iroh_interface(span.clone(), backend.clone())
                        .await
                        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?,
                );

                // Create the holder to manage the DirectChannel.
                let holder = HolderChannels::new(span.clone(), iroh_interface, own_node_id);

                // Convert Arc into Box for compatibility.
                let emitter_box = Box::new(EmitterWrapper(emitter));

                // Create the direct channel.
                let channel = holder
                    .new_channel(emitter_box, opts)
                    .await
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

                Ok(Arc::from(channel)
                    as Arc<
                        dyn crate::traits::DirectChannel<Error = GuardianError>,
                    >)
            })
        },
    )
}

// Simplified wrapper to convert Arc<dyn DirectChannelEmitter> into Box<dyn DirectChannelEmitter>.
struct EmitterWrapper(Arc<dyn DirectChannelEmitter<Error = GuardianError>>);

#[async_trait]
impl DirectChannelEmitter for EmitterWrapper {
    type Error = GuardianError;

    async fn emit(&self, payload: EventPubSubPayload) -> std::result::Result<(), Self::Error> {
        self.0.emit(payload).await
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        self.0.close().await
    }
}

// Helper function to create a DirectChannel with a custom Iroh interface.
pub async fn create_direct_channel_with_iroh(
    iroh_network: Arc<dyn DirectChannelNetwork>,
    emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
    span: Span,
    own_node_id: NodeId,
) -> Result<DirectChannel> {
    let channel = DirectChannel::new(span.clone(), iroh_network, emitter, own_node_id);

    // Start processing.
    channel.start().await?;

    tracing::info!("DirectChannel created with an integrated Iroh interface");
    Ok(channel)
}

// Unified Iroh interface configuration.
pub async fn create_unified_iroh_interface(
    span: Span,
    backend: Arc<IrohBackend>,
) -> Result<IrohBridge> {
    let interface = IrohBridge::new(span.clone(), backend).await?;

    // Start the IrohBridge.
    interface.start().await?;

    tracing::info!("Unified Iroh interface initialized with integrated iroh-gossip");
    Ok(interface)
}

// Function to create a test NodeId.
pub fn create_test_node_id() -> NodeId {
    let secret_key = iroh::SecretKey::generate();
    secret_key.public()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Wire format: DirectChannelMessage (serde_cbor codec) ─────────────────
    // The direct channel serializes/deserializes with serde_cbor (see send_data/handle_iroh_message).
    // These tests protect the wire format against regressions.

    fn roundtrip(msg: &DirectChannelMessage) -> DirectChannelMessage {
        let bytes = serde_cbor::to_vec(msg).expect("serialize");
        serde_cbor::from_slice::<DirectChannelMessage>(&bytes).expect("deserialize")
    }

    #[test]
    fn direct_channel_message_roundtrip_data() {
        let msg = DirectChannelMessage {
            message_type: MessageType::Data,
            payload: b"hello world".to_vec(),
            timestamp: 1_725_000_000,
            sender: "node-abc".to_string(),
        };
        let back = roundtrip(&msg);
        assert!(matches!(back.message_type, MessageType::Data));
        assert_eq!(back.payload, msg.payload);
        assert_eq!(back.timestamp, msg.timestamp);
        assert_eq!(back.sender, msg.sender);
    }

    #[test]
    fn direct_channel_message_roundtrip_all_types() {
        for mt in [MessageType::Data, MessageType::Heartbeat, MessageType::Ack] {
            let msg = DirectChannelMessage {
                message_type: mt.clone(),
                payload: vec![1, 2, 3, 4],
                timestamp: 42,
                sender: "peer".to_string(),
            };
            let back = roundtrip(&msg);
            // Discriminate the type after the round-trip.
            assert_eq!(
                std::mem::discriminant(&back.message_type),
                std::mem::discriminant(&mt)
            );
            assert_eq!(back.payload, vec![1, 2, 3, 4]);
        }
    }

    #[test]
    fn direct_channel_message_empty_payload_roundtrip() {
        let msg = DirectChannelMessage {
            message_type: MessageType::Heartbeat,
            payload: vec![],
            timestamp: 0,
            sender: String::new(),
        };
        let back = roundtrip(&msg);
        assert!(back.payload.is_empty());
        assert!(matches!(back.message_type, MessageType::Heartbeat));
    }

    #[test]
    fn corrupt_bytes_fail_to_deserialize() {
        let garbage = [0xff, 0x00, 0x13, 0x37, 0x42];
        assert!(serde_cbor::from_slice::<DirectChannelMessage>(&garbage).is_err());
    }

    // ─── Deterministic TopicId derivation (blake3) ────────────────────────────

    #[test]
    fn topic_id_is_deterministic_for_same_name() {
        let a = IrohBridge::topic_id_from_str("shared-kv");
        let b = IrohBridge::topic_id_from_str("shared-kv");
        assert_eq!(a, b);
    }

    #[test]
    fn topic_id_differs_for_different_names() {
        let a = IrohBridge::topic_id_from_str("topic-a");
        let b = IrohBridge::topic_id_from_str("topic-b");
        assert_ne!(a, b);
    }
}
