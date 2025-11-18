use crate::error::{GuardianError, Result};
use crate::traits::{DirectChannelEmitter, EventPubSub, EventPubSubMessage, EventPubSubPayload};
use async_trait::async_trait;
use libp2p::PeerId;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};

// ============================================================================
// EVENT BUS IMPLEMENTATION usando Tokio Channels
// ============================================================================

/// Event Bus baseado em canais do Tokio
/// Oferece funcionalidade de pub/sub type-safe usando broadcast channels
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
    /// Cria um novo Event Bus
    pub fn new() -> Self {
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Cria um emitter para um tipo específico de evento
    pub async fn emitter<T>(&self) -> Result<Emitter<T>>
    where
        T: Clone + Send + Sync + 'static,
    {
        let type_id = TypeId::of::<T>();
        let mut channels = self.channels.write().await;

        channels.entry(type_id).or_insert_with(|| {
            let (sender, _) = broadcast::channel::<T>(1024); // Buffer de 1024 eventos
            Box::new(sender)
        });

        let sender = channels
            .get(&type_id)
            .and_then(|any| any.downcast_ref::<broadcast::Sender<T>>())
            .ok_or_else(|| GuardianError::Other("Failed to get sender for type".to_string()))?
            .clone();

        Ok(Emitter { sender })
    }

    /// Subscribe para receber eventos de um tipo específico
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

/// Emitter type-safe para um tipo específico de evento
pub struct Emitter<T> {
    sender: broadcast::Sender<T>,
}

impl<T> Emitter<T>
where
    T: Clone + Send + Sync + 'static,
{
    /// Emite um evento para todos os subscribers
    pub fn emit(&self, event: T) -> Result<()> {
        // broadcast::send retorna erro apenas se não há receivers
        // Neste caso, ignoramos o erro pois é normal não ter listeners
        let _ = self.sender.send(event);
        Ok(())
    }

    /// Retorna o número de subscribers ativos
    pub fn receiver_count(&self) -> usize {
        self.sender.receiver_count()
    }

    /// Fecha o emitter - implementação básica para compatibilidade
    /// Como broadcast::Sender não tem método close(), esta é uma implementação de compatibilidade
    pub async fn close(&self) -> Result<()> {
        // Para broadcast::Sender, não há método close() direto
        // O channel é fechado automaticamente quando todos os senders são dropados
        // ***Por enquanto, esta é uma implementação de compatibilidade que sempre retorna Ok
        Ok(())
    }
}

// ============================================================================
// PAYLOAD EMITTER
// ============================================================================

pub type Bus = EventBus;

pub struct PayloadEmitter {
    // EventBus baseado em Tokio
    emitter: Emitter<EventPubSubPayload>,
}

impl PayloadEmitter {
    /// Cria um novo emissor de eventos para payloads de pub/sub.
    pub async fn new(bus: &Bus) -> Result<Self> {
        let emitter = bus.emitter::<EventPubSubPayload>().await?;
        Ok(PayloadEmitter { emitter })
    }

    /// Emite um evento de payload.
    pub fn emit_payload(&self, evt: EventPubSubPayload) -> Result<()> {
        self.emitter.emit(evt)
    }
}

// Implementação do trait DirectChannelEmitter
#[async_trait]
impl DirectChannelEmitter for PayloadEmitter {
    type Error = GuardianError;

    async fn emit(&self, payload: EventPubSubPayload) -> std::result::Result<(), Self::Error> {
        self.emit_payload(payload)
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        // PayloadEmitter não precisa fechar nada especial
        Ok(())
    }
}

/// Cria um novo evento de Mensagem.
pub fn new_event_message(content: Vec<u8>) -> EventPubSubMessage {
    EventPubSubMessage { content }
}

/// Cria um novo evento de Payload.
pub fn new_event_payload(payload: Vec<u8>, peer: PeerId) -> EventPubSubPayload {
    EventPubSubPayload { payload, peer }
}

/// Cria um novo evento EventPubSubJoin.
pub fn new_event_peer_join(peer: PeerId, topic: String) -> EventPubSub {
    EventPubSub::Join { peer, topic }
}

/// Cria um novo evento EventPubSubLeave.
pub fn new_event_peer_leave(peer: PeerId, topic: String) -> EventPubSub {
    EventPubSub::Leave { peer, topic }
}
