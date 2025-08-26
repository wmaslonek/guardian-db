use libp2p::PeerId;
use crate::iface::{EventPubSubMessage, EventPubSubPayload, EventPubSub, DirectChannelEmitter};
use crate::error::{GuardianError, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use std::any::{Any, TypeId};
use async_trait::async_trait;

// ============================================================================
// EVENT BUS IMPLEMENTATION usando Tokio Channels
// ============================================================================

/// Event Bus baseado em canais do Tokio - substitui o event bus do Go
/// Oferece funcionalidade de pub/sub type-safe usando broadcast channels
pub struct EventBus {
    channels: Arc<RwLock<HashMap<TypeId, Box<dyn Any + Send + Sync>>>>,
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
        
        if !channels.contains_key(&type_id) {
            let (sender, _) = broadcast::channel::<T>(1024); // Buffer de 1024 eventos
            channels.insert(type_id, Box::new(sender));
        }
        
        let sender = channels.get(&type_id)
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
        
        if !channels.contains_key(&type_id) {
            let (sender, _) = broadcast::channel::<T>(1024);
            channels.insert(type_id, Box::new(sender));
        }
        
        let sender = channels.get(&type_id)
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
}

// ============================================================================
// PAYLOAD EMITTER - compatível com a API existente
// ============================================================================

/// Type alias para simplificar - substitui a referência ao Bus do Go
pub type Bus = EventBus;

/// equivalente a 'type PayloadEmitter struct' em Go
pub struct PayloadEmitter {
    // Agora usa nosso EventBus baseado em Tokio
    emitter: Emitter<EventPubSubPayload>,
}

impl PayloadEmitter {
    /// equivalente a 'NewPayloadEmitter' em Go
    /// Cria um novo emissor de eventos para payloads de pub/sub.
    pub async fn new(bus: &Bus) -> Result<Self> {
        let emitter = bus.emitter::<EventPubSubPayload>().await?;
        Ok(PayloadEmitter { emitter })
    }

    /// equivalente a 'Emit' em Go
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

/// equivalente a 'NewEventMessage' em Go
/// Cria um novo evento de Mensagem. Em Go, retornava um ponteiro; em Rust,
/// geralmente retornamos a própria struct por valor.
pub fn new_event_message(content: Vec<u8>) -> EventPubSubMessage {
    EventPubSubMessage {
        content,
    }
}

/// equivalente a 'NewEventPayload' em Go
/// Cria um novo evento de Payload.
pub fn new_event_payload(payload: Vec<u8>, peer: PeerId) -> EventPubSubPayload {
    EventPubSubPayload {
        payload,
        peer,
    }
}

/// equivalente a 'NewEventPeerJoin' em Go
/// Cria um novo evento EventPubSubJoin.
pub fn new_event_peer_join(peer: PeerId, topic: String) -> EventPubSub {
    EventPubSub::Join {
        peer,
        topic,
    }
}

/// equivalente a 'NewEventPeerLeave' em Go
/// Cria um novo evento EventPubSubLeave.
pub fn new_event_peer_leave(peer: PeerId, topic: String) -> EventPubSub {
    EventPubSub::Leave {
        peer,
        topic,
    }
}