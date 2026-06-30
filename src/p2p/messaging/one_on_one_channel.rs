use crate::guardian::error::{GuardianError, Result};
use crate::p2p::network::IrohClient;
use crate::p2p::network::core::gossip::EpidemicPubSub;
use crate::p2p::new_event_payload;
use crate::traits::{
    DirectChannel, DirectChannelEmitter, DirectChannelFactory, DirectChannelOptions,
    PubSubInterface, PubSubTopic,
};
use async_trait::async_trait;
use futures::stream::StreamExt;
use iroh::EndpointId as NodeId;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{Span, debug, error, info, instrument, warn};

// Protocol constants.
const PROTOCOL: &str = "/guardian-db/one-on-one-channel/1";

// `DirectChannel` trait implementation for the `Channels` struct.
#[async_trait]
impl DirectChannel for Channels {
    type Error = GuardianError;

    /// Starts the connection to and monitoring of a specific peer. If a connection
    /// with the peer does not already exist, it creates a new pubsub topic, subscribes
    /// to it, and starts a background task (`monitor_topic`) to listen for messages.
    #[instrument(level = "debug", skip(self))]
    async fn connect(&mut self, target: NodeId) -> std::result::Result<(), Self::Error> {
        let id = self.get_channel_id(&target);
        let mut subs = self.subs.write().await;

        // Only run the logic if we are not already subscribed to the channel with this peer.
        if let std::collections::hash_map::Entry::Vacant(e) = subs.entry(target) {
            info!(peer = %target, topic = %id, "Starting P2P connection and channel subscription via iroh-gossip.");

            // Create a "child token" for this specific connection.
            // When the main token in `self.token` is cancelled (in `close`),
            // this child token is automatically cancelled in cascade.
            let child_token = self.token.child_token();

            // Subscribe to the topic via EpidemicPubSub.
            let topic = self.epidemic_pubsub.topic_subscribe(&id).await?;

            // Store the token and topic.
            let sub_info = SubscriptionInfo {
                token: child_token.clone(),
                topic: topic.clone(),
            };
            e.insert(sub_info);

            // Clone the references needed for the new asynchronous task.
            let self_clone = self.clone();

            // Start the background task that will monitor the topic.
            tokio::spawn(async move {
                // Pass the topic and child token to the monitoring task.
                self_clone.monitor_topic(topic, target, child_token).await;

                // After monitoring ends (whether by cancellation or end of stream),
                // remove the peer from the active subscriptions map for cleanup.
                let mut subs = self_clone.subs.write().await;
                subs.remove(&target);
                debug!("Monitor for {} ended and removed.", target);
            });
        }

        // Release the write lock before continuing.
        drop(subs);

        // Note: In Iroh, P2P connections are established automatically via discovery.
        // iroh-gossip uses Epidemic Broadcast Trees for efficient propagation.
        debug!(peer = %target, "P2P channel configured via iroh-gossip. The connection will be established automatically.");

        // Wait until the peer becomes visible in the pubsub topic.
        self.wait_for_peers(target, &id).await
    }

    /// Publishes a message (byte slice) on the P2P communication channel
    /// established with peer `p` via iroh-gossip.
    #[instrument(level = "debug", skip(self, head))]
    async fn send(&mut self, p: NodeId, head: Vec<u8>) -> std::result::Result<(), Self::Error> {
        // Get the topic of the active subscription.
        let topic = {
            let subs = self.subs.read().await;
            subs.get(&p).map(|info| info.topic.clone()).ok_or_else(|| {
                GuardianError::Other(format!(
                    "Peer {} is not connected. Call connect() first.",
                    p
                ))
            })?
        };

        // Publish the data on the topic via iroh-gossip.
        topic.publish(head).await?;

        Ok(())
    }

    /// Shuts down all active connections and monitoring tasks,
    /// cleaning up all resources associated with `Channels`.
    #[instrument(level = "debug", skip(self))]
    async fn close(&mut self) -> std::result::Result<(), Self::Error> {
        info!("Shutting down all channels and monitoring tasks...");

        // With a single call, we cancel the main token.
        // This action propagates and cancels ALL child tokens that were
        // passed to the `monitor_topic` tasks, signaling them to stop
        // cleanly and cooperatively.
        self.token.cancel();

        // Clear the subscriptions map.
        self.subs.write().await.clear();

        // Close the event emitter.
        self.emitter.close().await?;

        Ok(())
    }

    /// A version of close() that works with a shared reference (&self).
    /// Allows closing the channel when used inside an Arc<>.
    #[instrument(level = "debug", skip(self))]
    async fn close_shared(&self) -> std::result::Result<(), Self::Error> {
        info!("Shutting down all channels (shared reference)...");

        // Cancel the main token to stop all monitoring tasks.
        self.token.cancel();

        // Clear the subscriptions map.
        self.subs.write().await.clear();

        // Close the event emitter.
        self.emitter.close().await?;

        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// Subscription information for each peer.
struct SubscriptionInfo {
    #[allow(dead_code)] // Kept for lifecycle control.
    token: CancellationToken,
    topic: Arc<dyn PubSubTopic<Error = GuardianError>>,
}

// The main struct that manages the channels.
#[derive(Clone)]
pub struct Channels {
    subs: Arc<RwLock<HashMap<NodeId, SubscriptionInfo>>>,
    self_id: NodeId,
    emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError> + Send + Sync>,
    epidemic_pubsub: Arc<EpidemicPubSub>,
    span: Span,
    // The main token that controls the lifetime of the entire Channels instance.
    token: CancellationToken,
}

impl Channels {
    /// Returns a reference to the tracing span used for instrumentation.
    pub fn span(&self) -> &Span {
        &self.span
    }

    #[instrument(level = "debug", skip(self))]
    pub async fn connect(&self, target: NodeId) -> Result<()> {
        let _entered = self.span.enter();
        let id = self.get_channel_id(&target);
        let mut subs = self.subs.write().await;

        if let std::collections::hash_map::Entry::Vacant(e) = subs.entry(target) {
            debug!(topic = %id, "subscribing to the topic via iroh-gossip (P2P)");

            // Subscribe to the topic via EpidemicPubSub.
            let topic = self.epidemic_pubsub.topic_subscribe(&id).await?;

            let cancel_token = CancellationToken::new();

            let sub_info = SubscriptionInfo {
                token: cancel_token.clone(),
                topic: topic.clone(),
            };
            e.insert(sub_info);

            // Spawn the task to monitor the topic.
            let self_clone = self.clone();
            tokio::spawn(async move {
                self_clone.monitor_topic(topic, target, cancel_token).await;

                // When monitor_topic ends, remove the peer from the cache.
                let mut subs = self_clone.subs.write().await;
                subs.remove(&target);
            });
        }
        // Release the write lock before the network calls.
        drop(subs);

        // Note: In Iroh, P2P connections are established automatically via discovery.
        debug!(peer = %target, "P2P channel configured via iroh-gossip. The connection will be established automatically.");

        self.wait_for_peers(target, &id).await
    }

    #[instrument(level = "debug", skip(self, head))]
    pub async fn send(&self, p: NodeId, head: &[u8]) -> Result<()> {
        let _entered = self.span.enter();

        // Get the topic of the active subscription.
        let topic = {
            let subs = self.subs.read().await;
            subs.get(&p).map(|info| info.topic.clone()).ok_or_else(|| {
                GuardianError::Other(format!(
                    "Peer {} is not connected. Call connect() first.",
                    p
                ))
            })?
        };

        // Publish via iroh-gossip.
        topic.publish(head.to_vec()).await.map_err(|e| {
            GuardianError::Other(format!("failed to publish data via iroh-gossip: {}", e))
        })?;

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    async fn wait_for_peers(&self, other_peer: NodeId, channel_id: &str) -> Result<()> {
        // With iroh-gossip, peers are discovered automatically via Epidemic Broadcast Trees.
        // The gossip protocol propagates messages even without immediately visible peers.

        debug!(peer = %other_peer, channel = %channel_id,
               "P2P channel via iroh-gossip configured. Peers will be discovered automatically.");

        // iroh-gossip manages peer discovery automatically.
        Ok(())
    }

    // Helper function to generate unique channel identifiers.
    // Implements pure, deterministic logic to create one-on-one channel IDs.
    // Ensures the same ID is generated regardless of the peer order.
    #[instrument(level = "debug", skip(self))]
    fn get_channel_id(&self, p: &NodeId) -> String {
        let mut channel_id_peers = [self.self_id.to_string(), p.to_string()];
        channel_id_peers.sort();
        // PROTOCOL already starts with '/', so it is not prefixed again here.
        format!("{}/{}", PROTOCOL, channel_id_peers.join("/"))
    }

    #[instrument(level = "debug", skip(self, topic, token))]
    async fn monitor_topic(
        &self,
        topic: Arc<dyn PubSubTopic<Error = GuardianError>>,
        p: NodeId,
        token: CancellationToken, // Receives the (child) token.
    ) {
        // Get the message stream of the topic via iroh-gossip.
        let mut stream = match topic.watch_messages().await {
            Ok(s) => s,
            Err(e) => {
                error!("Error getting the topic's message stream: {}", e);
                return;
            }
        };

        loop {
            tokio::select! {
                // Wait for the cancellation signal on the token.
                // `biased;` can be used to always check cancellation first.
                biased;
                _ = token.cancelled() => {
                    debug!(remote = %p, "closing the P2P topic monitor due to cancellation");
                    break;
                },

                // Process the next message from the stream (iroh-gossip).
                maybe_msg = stream.next() => {
                    match maybe_msg {
                        Some(msg) => {
                            // Emit the event payload - msg.content is already Vec<u8>.
                            let event = new_event_payload(msg.content, p);
                            if let Err(e) = self.emitter.emit(event).await {
                                warn!("could not emit the event payload: {}", e);
                            }
                        },
                        // Stream finished.
                        None => {
                             debug!(remote = %p, "iroh-gossip P2P stream finished");
                             break;
                        }
                    }
                }
            }
        }
    }
}

#[instrument(level = "debug", skip(client))]
pub async fn new_channel_factory(client: Arc<IrohClient>) -> Result<DirectChannelFactory> {
    let self_id = client
        .id()
        .await
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
        .id;

    info!(
        "Local node ID: {} (P2P communication via iroh-gossip)",
        self_id
    );

    // Create EpidemicPubSub for P2P communication.
    let backend = client.backend().clone();
    let epidemic_pubsub = Arc::new(
        backend
            .create_pubsub_interface()
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?,
    );

    let factory = move |emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
                        _opts: Option<DirectChannelOptions>| {
        let epidemic_pubsub = epidemic_pubsub.clone();
        let self_id = self_id;

        Box::pin(async move {
            // Create a span for the direct channel.
            let span = tracing::info_span!("direct_channel_p2p", self_id = %self_id);

            let ch = Arc::new(Channels {
                emitter,
                subs: Arc::new(RwLock::new(HashMap::new())),
                self_id,
                epidemic_pubsub,
                span,
                token: CancellationToken::new(),
            });

            Ok(ch as Arc<dyn DirectChannel<Error = GuardianError>>)
        })
            as Pin<
                Box<
                    dyn Future<
                            Output = std::result::Result<
                                Arc<dyn DirectChannel<Error = GuardianError>>,
                                Box<dyn std::error::Error + Send + Sync>,
                            >,
                        > + Send,
                >,
            >
    };

    Ok(Arc::new(factory))
}
