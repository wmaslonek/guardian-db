use crate::guardian::error::{GuardianError, Result};
use crate::traits::{DirectChannelEmitter, EventPubSub, EventPubSubMessage, EventPubSubPayload};
use async_trait::async_trait;
use iroh::EndpointId as NodeId;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};

pub mod messaging;
pub mod network;

// ============================================================================
// EVENT BUS IMPLEMENTATION using Tokio Channels
// ============================================================================

/// Event Bus based on Tokio channels.
/// Provides type-safe pub/sub functionality using broadcast channels.
#[derive(Clone)]
pub struct EventBus {
    channels: Arc<RwLock<HashMap<TypeId, Box<dyn Any + Send + Sync>>>>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    /// Creates a new Event Bus.
    pub fn new() -> Self {
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Creates an emitter for a specific event type.
    pub async fn emitter<T>(&self) -> Result<Emitter<T>>
    where
        T: Clone + Send + Sync + 'static,
    {
        let type_id = TypeId::of::<T>();
        let mut channels = self.channels.write().await;

        channels.entry(type_id).or_insert_with(|| {
            let (sender, _) = broadcast::channel::<T>(1024); // Buffer of 1024 events.
            Box::new(sender)
        });

        let sender = channels
            .get(&type_id)
            .and_then(|any| any.downcast_ref::<broadcast::Sender<T>>())
            .ok_or_else(|| GuardianError::Other("Failed to get sender for type".to_string()))?
            .clone();

        Ok(Emitter { sender })
    }

    /// Subscribes to receive events of a specific type.
    pub async fn subscribe<T>(&self) -> Result<broadcast::Receiver<T>>
    where
        T: Clone + Send + Sync + 'static,
    {
        let type_id = TypeId::of::<T>();
        let mut channels = self.channels.write().await;

        channels.entry(type_id).or_insert_with(|| {
            let (sender, _) = broadcast::channel::<T>(1024);
            Box::new(sender)
        });

        let sender = channels
            .get(&type_id)
            .and_then(|any| any.downcast_ref::<broadcast::Sender<T>>())
            .ok_or_else(|| GuardianError::Other("Failed to get sender for type".to_string()))?;

        Ok(sender.subscribe())
    }
}

/// Type-safe emitter for a specific event type.
pub struct Emitter<T> {
    sender: broadcast::Sender<T>,
}

impl<T> Emitter<T>
where
    T: Clone + Send + Sync + 'static,
{
    /// Emits an event to all subscribers.
    pub fn emit(&self, event: T) -> Result<()> {
        // broadcast::send returns an error only when there are no receivers.
        // In that case we ignore the error since having no listeners is normal.
        let _ = self.sender.send(event);
        Ok(())
    }

    /// Returns the number of active subscribers.
    pub fn receiver_count(&self) -> usize {
        self.sender.receiver_count()
    }

    /// Closes the emitter - a basic implementation for compatibility.
    /// Since broadcast::Sender has no close() method, this is a compatibility shim.
    pub async fn close(&self) -> Result<()> {
        // For broadcast::Sender there is no direct close() method.
        // The channel is closed automatically when all senders are dropped.
        // ***For now this is a compatibility implementation that always returns Ok.
        Ok(())
    }
}

// ============================================================================
// PAYLOAD EMITTER
// ============================================================================

pub type Bus = EventBus;

pub struct PayloadEmitter {
    // Tokio-based EventBus.
    emitter: Emitter<EventPubSubPayload>,
}

impl PayloadEmitter {
    /// Creates a new event emitter for pub/sub payloads.
    pub async fn new(bus: &Bus) -> Result<Self> {
        let emitter = bus.emitter::<EventPubSubPayload>().await?;
        Ok(PayloadEmitter { emitter })
    }

    /// Emits a payload event.
    pub fn emit_payload(&self, evt: EventPubSubPayload) -> Result<()> {
        self.emitter.emit(evt)
    }
}

// DirectChannelEmitter trait implementation.
#[async_trait]
impl DirectChannelEmitter for PayloadEmitter {
    type Error = GuardianError;

    async fn emit(&self, payload: EventPubSubPayload) -> std::result::Result<(), Self::Error> {
        self.emit_payload(payload)
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        // PayloadEmitter does not need to close anything special.
        Ok(())
    }
}

/// Creates a new Message event.
pub fn new_event_message(content: Vec<u8>) -> EventPubSubMessage {
    EventPubSubMessage { content }
}

/// Creates a new Payload event.
pub fn new_event_payload(payload: Vec<u8>, peer: NodeId) -> EventPubSubPayload {
    EventPubSubPayload { payload, peer }
}

/// Creates a new EventPubSubJoin event.
pub fn new_event_peer_join(peer: NodeId, topic: String) -> EventPubSub {
    EventPubSub::Join { peer, topic }
}

/// Creates a new EventPubSubLeave event.
pub fn new_event_peer_leave(peer: NodeId, topic: String) -> EventPubSub {
    EventPubSub::Leave { peer, topic }
}
