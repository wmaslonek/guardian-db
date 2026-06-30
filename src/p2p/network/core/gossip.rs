// Native integration of iroh-gossip with IrohBackend.
//
// PubSubInterface implementation using pure iroh-gossip
// with Epidemic Broadcast Trees.

use crate::guardian::error::{GuardianError, Result};
use crate::p2p::network::core::IrohBackend;
use crate::traits::{EventPubSubMessage, PubSubInterface, PubSubTopic};
use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use iroh::EndpointId as NodeId;
use iroh_gossip::api::GossipSender;
use iroh_gossip::net::Gossip;
use iroh_gossip::proto::TopicId;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::BroadcastStream;
use tracing::{debug, error, info, warn};

/// IrohBackend wrapper that implements PubSubInterface using iroh-gossip.
pub struct EpidemicPubSub {
    /// Reference to the Iroh backend.
    #[allow(dead_code)]
    backend: Arc<IrohBackend>,
    /// Gossip instance.
    gossip: Gossip,
    /// Cache of active topics.
    topics: Arc<RwLock<HashMap<String, Arc<IrohTopic>>>>,
}

/// PubSubTopic implementation for Iroh using native iroh-gossip.
pub struct IrohTopic {
    /// Topic name.
    topic_name: String,
    /// Topic ID for iroh-gossip.
    #[allow(dead_code)]
    topic_id: TopicId,
    /// Channel for broadcasting received messages.
    message_sender: Arc<broadcast::Sender<Bytes>>,
    /// Peers connected to this topic.
    peers: Arc<RwLock<Vec<NodeId>>>,
    /// Bootstrap peers used to form the mesh.
    bootstrap_peers: Arc<RwLock<Vec<NodeId>>>,
    /// Channel for peer events (join/leave).
    peer_events_sender: Arc<broadcast::Sender<crate::events::Event>>,
    /// Handle of the gossip event loop task.
    _event_task: Option<JoinHandle<()>>,
    /// Sender for broadcasting messages (obtained via gossip_topic.split()).
    gossip_sender: Arc<RwLock<GossipSender>>,
}

impl EpidemicPubSub {
    /// Creates a new EpidemicPubSub instance using iroh-gossip.
    ///
    /// IMPORTANT: Uses the IrohBackend's Gossip that was registered on the Router,
    /// ensuring that incoming connections are correctly routed to this same Gossip.
    pub async fn new(backend: Arc<IrohBackend>) -> Result<Self> {
        // FIX: Use the IrohBackend's Gossip instead of creating a new one.
        // This is crucial because the Router accepts ALPN connections and routes them
        // to the registered Gossip. If we used a different Gossip, incoming messages
        // would never reach our subscriptions.
        let gossip_arc = backend.get_gossip().await?;
        let gossip_lock = gossip_arc.read().await;
        let gossip = gossip_lock
            .as_ref()
            .ok_or_else(|| {
                GuardianError::Other("Gossip not initialized in IrohBackend".to_string())
            })?
            .clone();
        drop(gossip_lock);

        info!("EpidemicPubSub initialized using the IrohBackend's shared Gossip");

        Ok(Self {
            backend,
            gossip,
            topics: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Generates a TopicId from a string using Blake3 (consistent with Iroh).
    fn topic_id_from_str(topic: &str) -> TopicId {
        let hash = blake3::hash(topic.as_bytes());
        TopicId::from_bytes(hash.into())
    }

    /// Gets or creates a topic.
    async fn get_or_create_topic(&self, topic: &str) -> Result<Arc<IrohTopic>> {
        self.get_or_create_topic_with_peers(topic, vec![]).await
    }

    /// Gets or creates a topic with specific bootstrap peers.
    pub async fn get_or_create_topic_with_peers(
        &self,
        topic: &str,
        bootstrap_peers: Vec<NodeId>,
    ) -> Result<Arc<IrohTopic>> {
        let topic_id = Self::topic_id_from_str(topic);

        // If the topic already exists AND we have new bootstrap peers,
        // use join_peers to add the peers to the existing mesh.
        let topics_read = self.topics.read().await;
        if let Some(existing_topic) = topics_read.get(topic) {
            let topic_clone = existing_topic.clone();
            drop(topics_read);

            if !bootstrap_peers.is_empty() {
                debug!(
                    "Topic {} already exists, adding {} bootstrap peers to the mesh via join_peers",
                    topic,
                    bootstrap_peers.len()
                );

                // Update the existing IrohTopic's bootstrap_peers field.
                {
                    let mut bp = topic_clone.bootstrap_peers.write().await;
                    // Add new peers without duplicating.
                    for peer in &bootstrap_peers {
                        if !bp.contains(peer) {
                            bp.push(*peer);
                        }
                    }
                    debug!("Bootstrap peers updated: {} peers in total", bp.len());
                }

                // FIX: Use join_peers on the existing GossipSender to add peers to the mesh.
                // This is much more efficient than creating a new subscription and ensures that
                // the same GossipSender used to publish has the peers in its mesh.
                {
                    let sender = topic_clone.gossip_sender.write().await;
                    sender
                        .join_peers(bootstrap_peers.clone())
                        .await
                        .map_err(|e| {
                            warn!("[JOIN_PEERS] Error adding peers to the mesh: {}", e);
                            GuardianError::Other(format!(
                                "Error adding peers via join_peers: {}",
                                e
                            ))
                        })?;
                    info!(
                        "[JOIN_PEERS] Peers {:?} added to the mesh of topic {}",
                        bootstrap_peers
                            .iter()
                            .map(|p| p.fmt_short().to_string())
                            .collect::<Vec<_>>(),
                        topic
                    );
                }

                // FIX: Wait for NeighborUp before returning.
                // Check whether the peers were added to the neighbors list,
                // with a timeout to avoid blocking forever.
                let expected_peers: std::collections::HashSet<_> =
                    bootstrap_peers.iter().cloned().collect();
                let mut retry_count = 0;
                const MAX_RETRIES: u32 = 20; // 20 * 100ms = 2 seconds max.

                loop {
                    let current_peers = topic_clone.peers.read().await;
                    let connected: std::collections::HashSet<_> =
                        current_peers.iter().cloned().collect();

                    // Check whether all bootstrap peers are connected.
                    let all_connected = expected_peers.iter().all(|p| connected.contains(p));
                    drop(current_peers);

                    if all_connected {
                        debug!(
                            "[JOIN_PEERS] Mesh formed successfully for topic {}, {} peers connected",
                            topic,
                            expected_peers.len()
                        );
                        break;
                    }

                    retry_count += 1;
                    if retry_count >= MAX_RETRIES {
                        warn!(
                            "[JOIN_PEERS] Timeout waiting for NeighborUp for topic {} after {}ms. Expected peers: {:?}, Connected: {:?}",
                            topic,
                            retry_count * 100,
                            expected_peers
                                .iter()
                                .map(|p| p.fmt_short().to_string())
                                .collect::<Vec<_>>(),
                            connected
                                .iter()
                                .map(|p| p.fmt_short().to_string())
                                .collect::<Vec<_>>()
                        );
                        break;
                    }

                    // Wait and try again.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }

                // We no longer create a new subscription here - join_peers already adds the peers to the mesh,
                // and the events (NeighborUp, Received) keep arriving on the original receiver.
            } else {
                debug!("Topic {} already exists, no new peers to add", topic);
            }

            return Ok(topic_clone);
        }
        drop(topics_read);

        let mut topics = self.topics.write().await;

        // Double-check after acquiring the write lock.
        if let Some(existing_topic) = topics.get(topic) {
            return Ok(existing_topic.clone());
        }

        // Create a new topic.
        let (sender, _) = broadcast::channel(1000); // Buffer for 1000 messages.
        let (peer_events_sender, _) = broadcast::channel(1000); // Buffer for peer events.

        // Subscribe to the topic on iroh-gossip with the provided bootstrap peers.
        debug!(
            "Subscribing to topic {} with {} bootstrap peers",
            topic,
            bootstrap_peers.len()
        );

        // Clone bootstrap_peers to store it in the IrohTopic.
        let bootstrap_peers_for_topic = bootstrap_peers.clone();

        let gossip_topic = self
            .gossip
            .subscribe(topic_id, bootstrap_peers)
            .await
            .map_err(|e| {
                GuardianError::Other(format!("Error subscribing to topic {}: {}", topic, e))
            })?;

        info!(
            "Successfully subscribed to iroh-gossip topic: {} (topic_id: {})",
            topic,
            topic_id.fmt_short()
        );

        // FIX: Use split() to separate sender and receiver.
        // The sender will be stored and used to publish messages.
        // The receiver will be used by event_task to receive messages.
        let (gossip_sender, mut gossip_receiver) = gossip_topic.split();
        let gossip_sender_arc = Arc::new(RwLock::new(gossip_sender));

        // Clone references for the event loop.
        let message_sender = Arc::new(sender);
        let peers = Arc::new(RwLock::new(Vec::new()));
        let peer_events_sender_clone = Arc::new(peer_events_sender);
        let topic_name_clone = topic.to_string();

        let message_sender_clone = message_sender.clone();
        let peers_clone = peers.clone();
        let peer_events_sender_event_clone = peer_events_sender_clone.clone();

        // Spawn a task to process events from the gossip receiver.
        let event_task = tokio::spawn(async move {
            while let Some(event_result) = gossip_receiver.next().await {
                match event_result {
                    Ok(iroh_gossip::api::Event::Received(msg)) => {
                        debug!(
                            "Message received on topic {}: {} bytes",
                            topic_name_clone,
                            msg.content.len()
                        );
                        if let Err(e) = message_sender_clone.send(msg.content.clone()) {
                            warn!(
                                "Error sending message to subscribers of topic {}: {}",
                                topic_name_clone, e
                            );
                        }
                    }
                    Ok(iroh_gossip::api::Event::NeighborUp(node_id)) => {
                        debug!("Peer {} connected to topic {}", node_id, topic_name_clone);
                        let mut peers_lock = peers_clone.write().await;
                        if !peers_lock.contains(&node_id) {
                            peers_lock.push(node_id);
                        }

                        // FIX: Send EventPubSub::Join so the handler can process it correctly.
                        let event: crate::events::Event =
                            Arc::new(crate::traits::EventPubSub::Join {
                                peer: node_id,
                                topic: topic_name_clone.clone(),
                            });
                        let _ = peer_events_sender_event_clone.send(event);
                    }
                    Ok(iroh_gossip::api::Event::NeighborDown(node_id)) => {
                        debug!(
                            "Peer {} disconnected from topic {}",
                            node_id, topic_name_clone
                        );
                        let mut peers_lock = peers_clone.write().await;
                        peers_lock.retain(|p| p != &node_id);

                        // FIX: Send EventPubSub::Leave so the handler can process it correctly.
                        let event: crate::events::Event =
                            Arc::new(crate::traits::EventPubSub::Leave {
                                peer: node_id,
                                topic: topic_name_clone.clone(),
                            });
                        let _ = peer_events_sender_event_clone.send(event);
                    }
                    Ok(iroh_gossip::api::Event::Lagged) => {
                        warn!("Event loop lagging on topic {}", topic_name_clone);
                    }
                    Err(e) => {
                        error!("Error in event stream of topic {}: {}", topic_name_clone, e);
                        break;
                    }
                }
            }

            info!("Event loop ended for topic {}", topic_name_clone);
        });

        let iroh_topic = Arc::new(IrohTopic {
            topic_name: topic.to_string(),
            topic_id,
            message_sender,
            peers,
            bootstrap_peers: Arc::new(RwLock::new(bootstrap_peers_for_topic)),
            peer_events_sender: peer_events_sender_clone,
            _event_task: Some(event_task),
            gossip_sender: gossip_sender_arc,
        });

        topics.insert(topic.to_string(), iroh_topic.clone());

        Ok(iroh_topic)
    }

    /// Returns a reference to the Gossip.
    pub fn gossip(&self) -> &Gossip {
        &self.gossip
    }

    /// Returns an existing topic, if any.
    pub async fn get_topic(&self, topic: &str) -> Option<Arc<IrohTopic>> {
        let topics = self.topics.read().await;
        topics.get(topic).cloned()
    }

    /// Publishes a message to a specific topic.
    /// Convenience method that encapsulates access to the Gossip.
    pub async fn publish_to_topic(&self, topic: &str, data: &[u8]) -> Result<()> {
        let topics = self.topics.read().await;

        if let Some(iroh_topic) = topics.get(topic) {
            iroh_topic.publish(data, &self.gossip).await
        } else {
            Err(GuardianError::Other(format!(
                "Topic {} not found - subscribe first",
                topic
            )))
        }
    }

    /// Ensures a topic exists with specific peers as bootstrap.
    /// Used to connect peers to the gossip mesh before publishing messages.
    pub async fn ensure_topic_with_peers(
        &self,
        topic: &str,
        bootstrap_peers: Vec<NodeId>,
    ) -> Result<Arc<IrohTopic>> {
        self.get_or_create_topic_with_peers(topic, bootstrap_peers)
            .await
    }

    /// Re-subscribes to the topic with new bootstrap peers.
    /// Uses join_peers to add peers to the existing mesh.
    pub async fn subscribe_with_peers(
        &self,
        topic: &str,
        bootstrap_peers: Vec<NodeId>,
    ) -> Result<Arc<IrohTopic>> {
        if bootstrap_peers.is_empty() {
            debug!(
                "[SUBSCRIBE_WITH_PEERS] No bootstrap peers provided for topic {}",
                topic
            );
            return self.get_or_create_topic(topic).await;
        }

        debug!(
            "[SUBSCRIBE_WITH_PEERS] Adding {} bootstrap peers to topic {}",
            bootstrap_peers.len(),
            topic
        );

        // Use get_or_create_topic_with_peers, which now uses join_peers internally.
        self.get_or_create_topic_with_peers(topic, bootstrap_peers)
            .await
    }
}

#[async_trait]
impl PubSubInterface for EpidemicPubSub {
    type Error = GuardianError;

    async fn topic_subscribe(
        &self,
        topic: &str,
    ) -> std::result::Result<Arc<dyn PubSubTopic<Error = GuardianError>>, Self::Error> {
        debug!("Subscribing to topic via EpidemicPubSub: {}", topic);

        let iroh_topic = self.get_or_create_topic(topic).await?;

        Ok(iroh_topic as Arc<dyn PubSubTopic<Error = GuardianError>>)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl IrohTopic {
    /// Publishes a message to the topic using iroh-gossip.
    ///
    /// FIX: Uses the stored GossipSender instead of creating a new subscription.
    /// Creating a new subscription generates a different KEY, and messages may not be delivered
    /// correctly between subscriptions with different KEYs.
    pub async fn publish(&self, data: &[u8], _gossip: &Gossip) -> Result<()> {
        debug!(
            "Publishing message to topic {}: {} bytes",
            self.topic_name,
            data.len()
        );

        // Use the stored GossipSender for the broadcast.
        // This ensures we use the same subscription KEY that is receiving messages.
        let sender = self.gossip_sender.write().await;
        sender
            .broadcast(Bytes::copy_from_slice(data))
            .await
            .map_err(|e| {
                GuardianError::Other(format!("Error publishing message via iroh-gossip: {}", e))
            })?;

        debug!(
            "Message published successfully to topic {} via GossipSender",
            self.topic_name
        );
        Ok(())
    }

    /// Lists peers connected to the topic.
    pub async fn list_peers(&self) -> Vec<NodeId> {
        let peers = self.peers.read().await;
        peers.clone()
    }
}

/// PubSubTopic implementation using native iroh-gossip with NodeId.
#[async_trait]
impl PubSubTopic for IrohTopic {
    type Error = GuardianError;

    async fn publish(&self, message: Vec<u8>) -> std::result::Result<(), Self::Error> {
        debug!(
            "[IrohTopic::publish] Publishing {} bytes to topic {}",
            message.len(),
            self.topic_name
        );

        // Use the stored GossipSender for the broadcast.
        // This ensures we use the same subscription KEY that is receiving messages.
        let sender = self.gossip_sender.write().await;
        sender
            .broadcast(Bytes::copy_from_slice(&message))
            .await
            .map_err(|e| {
                error!(
                    "[IrohTopic::publish] Error publishing to topic {}: {}",
                    self.topic_name, e
                );
                GuardianError::Other(format!("Error publishing message via iroh-gossip: {}", e))
            })?;

        debug!(
            "[IrohTopic::publish] Message published successfully to topic {}",
            self.topic_name
        );
        Ok(())
    }

    async fn peers(&self) -> std::result::Result<Vec<NodeId>, Self::Error> {
        // Return the list of NodeIds directly without conversion.
        Ok(self.list_peers().await)
    }

    async fn watch_peers(
        &self,
    ) -> std::result::Result<Pin<Box<dyn Stream<Item = crate::events::Event> + Send>>, Self::Error>
    {
        // Create a receiver from the peer events channel.
        let receiver = self.peer_events_sender.subscribe();

        // Convert the broadcast::Receiver into a Stream.
        let stream = BroadcastStream::new(receiver).filter_map(|result| async {
            result.ok() // Ignore lagged/closed errors.
        });

        Ok(Box::pin(stream))
    }

    async fn watch_messages(
        &self,
    ) -> std::result::Result<Pin<Box<dyn Stream<Item = EventPubSubMessage> + Send>>, Self::Error>
    {
        // Create a receiver from the messages channel.
        let receiver = self.message_sender.subscribe();

        // Convert the broadcast::Receiver into a Stream of EventPubSubMessage.
        let stream = BroadcastStream::new(receiver).filter_map(move |result| async move {
            match result {
                Ok(data) => {
                    // Convert Bytes into Vec<u8>.
                    Some(EventPubSubMessage {
                        content: data.to_vec(),
                    })
                }
                Err(_) => None, // Ignore lagged/closed errors.
            }
        });

        Ok(Box::pin(stream))
    }

    fn topic(&self) -> &str {
        &self.topic_name
    }
}

impl IrohBackend {
    /// Creates a PubSub interface for this backend using iroh-gossip.
    pub async fn create_pubsub_interface(self: Arc<Self>) -> Result<EpidemicPubSub> {
        EpidemicPubSub::new(self).await
    }
}
