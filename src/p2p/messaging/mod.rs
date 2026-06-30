use crate::events;
use crate::guardian::error::GuardianError;
use crate::p2p;
use crate::p2p::network::core::gossip::EpidemicPubSub;
use crate::traits::{EventPubSubMessage, PubSubInterface, PubSubTopic, TracerWrapper};
use futures::Stream;
use iroh::EndpointId as NodeId;
use opentelemetry::trace::noop::NoopTracer;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tokio_util::sync::CancellationToken;
use tracing::{Span, error, instrument, warn};

pub mod direct_channel;
pub mod one_on_one_channel;

pub const PROTOCOL: &str = "/guardian-db/direct-channel/1";
pub const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
pub const MAX_MESSAGE_SIZE: usize = 1024 * 1024; // 1MB

/// The `CoreApiPubSub` struct manages the P2P pubsub logic for the GuardianDB node via iroh-gossip.
pub struct CoreApiPubSub {
    pub epidemic_pubsub: Arc<EpidemicPubSub>,
    pub span: Span,
    pub id: NodeId,
    pub poll_interval: Duration,
    pub tracer: Arc<TracerWrapper>,
    topics: Mutex<HashMap<String, Arc<PsTopic>>>,
    /// Token for the graceful cancellation of all operations.
    cancellation_token: CancellationToken,
}

#[async_trait::async_trait]
impl PubSubInterface for CoreApiPubSub {
    type Error = GuardianError;

    #[instrument(level = "debug", skip(self))]
    async fn topic_subscribe(
        &self,
        topic: &str,
    ) -> Result<Arc<dyn crate::traits::PubSubTopic<Error = GuardianError>>, Self::Error> {
        let mut topics_guard = self.topics.lock().await;

        // If the topic is already in our cache, return the existing instance.
        if let Some(t) = topics_guard.get(topic) {
            return Ok(t.clone() as Arc<dyn crate::traits::PubSubTopic<Error = GuardianError>>);
        }

        // Subscribe to the topic via EpidemicPubSub.
        let inner_topic = self.epidemic_pubsub.topic_subscribe(topic).await?;

        // Create a new Arc<CoreApiPubSub> sharing the same resources.
        let ps_arc = Arc::new(CoreApiPubSub {
            epidemic_pubsub: self.epidemic_pubsub.clone(),
            span: self.span.clone(),
            id: self.id,
            poll_interval: self.poll_interval,
            tracer: self.tracer.clone(),
            topics: Mutex::new(HashMap::new()),
            cancellation_token: self.cancellation_token.clone(),
        });

        // Create a new topic.
        let new_topic = Arc::new(PsTopic {
            topic: topic.to_string(),
            ps: ps_arc,
            inner_topic,
            members: Default::default(),
            cancellation_token: self.cancellation_token.child_token(),
        });

        // Insert the new topic into our cache.
        topics_guard.insert(topic.to_string(), new_topic.clone());

        Ok(new_topic as Arc<dyn crate::traits::PubSubTopic<Error = GuardianError>>)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// The `PsTopic` struct is stored in an `Arc` for safe sharing across threads.
/// The `PsTopic` struct holds the state and logic for a single P2P pubsub topic via iroh-gossip.
pub struct PsTopic {
    topic: String,
    ps: Arc<CoreApiPubSub>,
    /// EpidemicPubSub topic for P2P communication.
    inner_topic: Arc<dyn PubSubTopic<Error = GuardianError>>,
    members: RwLock<Vec<NodeId>>,
    /// Token for the graceful cancellation of this topic's operations.
    cancellation_token: CancellationToken,
}

impl PsTopic {
    // Publishes a message via iroh-gossip.
    #[instrument(level = "debug", skip(self, message))]
    pub async fn publish(&self, message: &[u8]) -> crate::guardian::error::Result<()> {
        // Check that the topic has not been cancelled.
        if self.cancellation_token.is_cancelled() {
            return Err(crate::guardian::error::GuardianError::Store(
                "Cannot publish to cancelled topic".to_string(),
            ));
        }

        // Basic message validation.
        if message.is_empty() {
            return Err(crate::guardian::error::GuardianError::Store(
                "Cannot publish empty message".to_string(),
            ));
        }

        // Publish via EpidemicPubSub using the proper method.
        self.ps
            .epidemic_pubsub
            .publish_to_topic(&self.topic, message)
            .await?;
        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    pub async fn peers(&self) -> crate::guardian::error::Result<Vec<NodeId>> {
        // Get the topic's peers via iroh-gossip.
        self.inner_topic.peers().await
    }

    // Computes the peer difference (joining/leaving) via iroh-gossip.
    #[instrument(level = "debug", skip(self))]
    pub async fn peers_diff(&self) -> crate::guardian::error::Result<(Vec<NodeId>, Vec<NodeId>)> {
        let current_peers = self.inner_topic.peers().await?;
        let mut members_guard = self.members.write().await;

        // Identify peers that joined.
        let joining: Vec<NodeId> = current_peers
            .iter()
            .filter(|peer| !members_guard.contains(peer))
            .copied()
            .collect();

        // Identify peers that left.
        let leaving: Vec<NodeId> = members_guard
            .iter()
            .filter(|peer| !current_peers.contains(peer))
            .copied()
            .collect();

        // Update the members list.
        *members_guard = current_peers;

        Ok((joining, leaving))
    }

    // Returns a channel `Receiver` that will emit events for peers
    // joining or leaving the topic.
    // Adds proper cancellation and better resource management.
    #[instrument(level = "debug", skip(self))]
    pub async fn watch_peers_channel(
        self: &Arc<Self>,
    ) -> crate::guardian::error::Result<mpsc::Receiver<Arc<dyn std::any::Any + Send + Sync>>> {
        let (tx, rx) = mpsc::channel(32);

        // Clone the Arc so the new task can have its own reference.
        let topic_clone = self.clone();
        let cancellation_token = self.cancellation_token.clone();

        tokio::spawn(async move {
            loop {
                // Check for cancellation before each iteration.
                if cancellation_token.is_cancelled() {
                    break;
                }

                // Call the function that computes the peer difference.
                let peers_diff_result = topic_clone.peers_diff().await;

                let (joining, leaving) = match peers_diff_result {
                    Ok((j, l)) => (j, l),
                    Err(e) => {
                        // Log the error and end the task.
                        // When `tx` goes out of scope (is dropped), the receiver side
                        // will know the channel was closed.
                        error!("Error checking the peer difference: {:?}", e);
                        return;
                    }
                };

                for node_id in joining {
                    let event = p2p::new_event_peer_join(node_id, topic_clone.topic().to_string());
                    // Convert EventPubSub into the expected type.
                    let event_any: Arc<dyn std::any::Any + Send + Sync> = Arc::new(event);
                    if tx.send(event_any).await.is_err() {
                        // The receiver was closed, so the task no longer needs to run.
                        return;
                    }
                }

                for node_id in leaving {
                    let event = p2p::new_event_peer_leave(node_id, topic_clone.topic().to_string());
                    // Convert EventPubSub into the expected type.
                    let event_any: Arc<dyn std::any::Any + Send + Sync> = Arc::new(event);
                    if tx.send(event_any).await.is_err() {
                        return;
                    }
                }

                // Use select! to allow cancellation during the sleep.
                tokio::select! {
                    _ = tokio::time::sleep(topic_clone.ps.poll_interval) => {},
                    _ = cancellation_token.cancelled() => {
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }

    #[instrument(level = "debug", skip(self))]
    pub async fn watch_messages(
        &self,
    ) -> crate::guardian::error::Result<mpsc::Receiver<EventPubSubMessage>> {
        // Get the message stream of the P2P topic via iroh-gossip.
        let mut message_stream = self.inner_topic.watch_messages().await?;

        let (tx, rx) = mpsc::channel(128);
        let cancellation_token = self.cancellation_token.clone();
        let _topic_name = self.topic.clone();

        tokio::spawn(async move {
            loop {
                // Check for cancellation before each iteration.
                if cancellation_token.is_cancelled() {
                    break;
                }

                // Use select! to allow cancellation while waiting for messages.
                tokio::select! {
                    msg_result = message_stream.next() => {
                        match msg_result {
                            Some(msg) => {
                                // The message already comes filtered from EpidemicPubSub.
                                if tx.send(msg).await.is_err() {
                                    // The receiver was closed, end the task.
                                    break;
                                }
                            }
                            None => {
                                // Stream closed, end the task.
                                break;
                            }
                        }
                    }
                    _ = cancellation_token.cancelled() => {
                        // Cancellation requested, end the task.
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }

    // Returns a reference to the topic name, which is more efficient
    // than cloning the String.
    #[instrument(level = "debug", skip(self))]
    pub fn topic(&self) -> &str {
        &self.topic
    }

    /// Cancels all active operations of the topic.
    #[instrument(level = "debug", skip(self))]
    pub fn cancel(&self) {
        self.cancellation_token.cancel();
    }

    /// Returns whether the topic has been cancelled.
    #[instrument(level = "debug", skip(self))]
    pub fn is_cancelled(&self) -> bool {
        self.cancellation_token.is_cancelled()
    }

    /// Clears the topic's members list.
    #[instrument(level = "debug", skip(self))]
    pub async fn clear_members(&self) {
        let mut members_guard = self.members.write().await;
        members_guard.clear();
    }
}

#[async_trait::async_trait]
impl PubSubTopic for PsTopic {
    type Error = GuardianError;

    #[instrument(level = "debug", skip(self, message))]
    async fn publish(&self, message: Vec<u8>) -> crate::guardian::error::Result<()> {
        PsTopic::publish(self, &message).await
    }

    #[instrument(level = "debug", skip(self))]
    async fn peers(&self) -> crate::guardian::error::Result<Vec<NodeId>> {
        self.peers().await
    }

    #[instrument(level = "debug", skip(self))]
    async fn watch_peers(
        &self,
    ) -> crate::guardian::error::Result<Pin<Box<dyn Stream<Item = events::Event> + Send>>> {
        let (tx, rx) = mpsc::channel(32);

        // Clone the data needed for the task.
        let topic_clone = Arc::new(PsTopic {
            topic: self.topic.clone(),
            ps: self.ps.clone(),
            inner_topic: self.inner_topic.clone(),
            members: RwLock::new(self.members.read().await.clone()),
            cancellation_token: self.cancellation_token.clone(),
        });

        tokio::spawn(async move {
            loop {
                // Check for cancellation before each iteration.
                if topic_clone.cancellation_token.is_cancelled() {
                    break;
                }

                // Call the function that computes the peer difference.
                let peers_diff_result = topic_clone.peers_diff().await;

                let (joining, leaving) = match peers_diff_result {
                    Ok((j, l)) => (j, l),
                    Err(e) => {
                        // Log the error and end the task.
                        error!("Error checking the peer difference: {:?}", e);
                        return;
                    }
                };

                for node_id in joining {
                    let event = p2p::new_event_peer_join(node_id, topic_clone.topic().to_string());
                    // Convert EventPubSub into the expected events::Event type.
                    let event_any: events::Event = Arc::new(event);
                    if tx.send(event_any).await.is_err() {
                        // The receiver was closed, so the task no longer needs to run.
                        return;
                    }
                }

                for node_id in leaving {
                    let event = p2p::new_event_peer_leave(node_id, topic_clone.topic().to_string());
                    // Convert EventPubSub into the expected events::Event type.
                    let event_any: events::Event = Arc::new(event);
                    if tx.send(event_any).await.is_err() {
                        return;
                    }
                }

                // Use select! to allow cancellation during the sleep.
                tokio::select! {
                    _ = tokio::time::sleep(topic_clone.ps.poll_interval) => {},
                    _ = topic_clone.cancellation_token.cancelled() => {
                        break;
                    }
                }
            }
        });

        let stream = ReceiverStream::new(rx);
        Ok(Box::pin(stream))
    }

    #[instrument(level = "debug", skip(self))]
    async fn watch_messages(
        &self,
    ) -> crate::guardian::error::Result<Pin<Box<dyn Stream<Item = EventPubSubMessage> + Send>>>
    {
        let receiver = self.watch_messages().await?;
        let stream = ReceiverStream::new(receiver);
        Ok(Box::pin(stream))
    }

    #[instrument(level = "debug", skip(self))]
    fn topic(&self) -> &str {
        &self.topic
    }
}

impl CoreApiPubSub {
    /// Subscribes to a P2P pubsub topic via iroh-gossip, returning a `PubSubTopic` instance.
    /// If the topic already exists, returns the existing instance.
    /// This method should be called in contexts where an Arc<CoreApiPubSub> is already held.
    #[instrument(level = "debug", skip(self))]
    pub async fn topic_subscribe_internal(
        self: &Arc<Self>,
        topic: &str,
    ) -> crate::guardian::error::Result<Arc<PsTopic>> {
        let mut topics_guard = self.topics.lock().await;

        // If the topic is already in our cache, return the existing instance.
        if let Some(t) = topics_guard.get(topic) {
            return Ok(t.clone());
        }

        // Subscribe to the topic via EpidemicPubSub.
        let inner_topic = self.epidemic_pubsub.topic_subscribe(topic).await?;

        // Create a new topic instance with the P2P topic.
        let new_topic = Arc::new(PsTopic {
            topic: topic.to_string(),
            ps: Arc::clone(self),
            inner_topic,
            members: Default::default(),
            cancellation_token: self.cancellation_token.child_token(),
        });

        // Insert the new topic into our cache.
        topics_guard.insert(topic.to_string(), new_topic.clone());

        Ok(new_topic)
    }

    /// Creates a new `CoreApiPubSub` instance using EpidemicPubSub for P2P communication.
    /// The `span` and `tracer` parameters can be optional.
    #[instrument(level = "debug", skip(epidemic_pubsub, span, tracer))]
    pub fn new(
        epidemic_pubsub: Arc<EpidemicPubSub>,
        id: NodeId,
        poll_interval: Duration,
        span: Option<Span>,
        tracer: Option<Arc<TracerWrapper>>,
    ) -> Arc<Self> {
        // Create a default tracer if none is provided.
        let default_tracer = Arc::new(TracerWrapper::Noop(NoopTracer::new()));

        Arc::new(Self {
            topics: Mutex::new(HashMap::new()),
            epidemic_pubsub,
            id,
            poll_interval,
            span: span.unwrap_or_else(tracing::Span::current),
            tracer: tracer.unwrap_or(default_tracer),
            cancellation_token: CancellationToken::new(),
        })
    }

    /// Method to cancel all PubSub operations.
    pub fn cancel(&self) {
        self.cancellation_token.cancel();
    }

    /// Returns whether the PubSub has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancellation_token.is_cancelled()
    }

    /// Removes a specific topic from the cache.
    #[instrument(level = "debug", skip(self))]
    pub async fn remove_topic(&self, topic_name: &str) -> bool {
        let mut topics_guard = self.topics.lock().await;
        topics_guard.remove(topic_name).is_some()
    }

    /// Removes all cancelled topics from the cache.
    #[instrument(level = "debug", skip(self))]
    pub async fn cleanup_cancelled_topics(&self) -> usize {
        let mut topics_guard = self.topics.lock().await;
        let mut cancelled_topics = Vec::new();

        // Identify cancelled topics.
        for (name, topic) in topics_guard.iter() {
            if topic.is_cancelled() {
                cancelled_topics.push(name.clone());
            }
        }

        // Remove cancelled topics.
        for topic_name in &cancelled_topics {
            topics_guard.remove(topic_name);
        }

        cancelled_topics.len()
    }

    /// Returns statistics about the active topics.
    #[instrument(level = "debug", skip(self))]
    pub async fn topic_stats(&self) -> (usize, usize) {
        let topics_guard = self.topics.lock().await;
        let total_topics = topics_guard.len();
        let mut active_topics = 0;

        for topic in topics_guard.values() {
            if !topic.is_cancelled() {
                active_topics += 1;
            }
        }

        (total_topics, active_topics)
    }
}
