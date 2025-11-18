// Integração do Gossipsub com IrohBackend
//
// Implementação de PubSubInterface
// usando o SwarmManager do LibP2P Gossipsub integrado ao Iroh

use crate::error::{GuardianError, Result};
use crate::ipfs_core_api::backends::iroh::IrohBackend;
use crate::p2p::manager::SwarmManager;
use crate::traits::{EventPubSubMessage, PubSubInterface, PubSubTopic};
use async_trait::async_trait;
use futures::stream::{Stream, StreamExt};
use libp2p::{PeerId, gossipsub::TopicHash};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};
use tokio_stream::wrappers::BroadcastStream;
use tracing::{debug, info, warn};

/// Wrapper do IrohBackend que implementa PubSubInterface
pub struct IrohPubSub {
    /// Referência ao backend Iroh
    iroh_backend: Arc<IrohBackend>,
    /// Cache de tópicos ativos
    topics: Arc<RwLock<HashMap<String, Arc<IrohTopic>>>>,
}

/// Implementação de PubSubTopic para Iroh com Gossipsub
pub struct IrohTopic {
    /// Nome do tópico
    topic_name: String,
    /// Hash do tópico para Gossipsub
    topic_hash: TopicHash,
    /// Referência ao SwarmManager
    swarm: Arc<RwLock<Option<SwarmManager>>>,
    /// Canal para broadcast de mensagens recebidas
    message_sender: Arc<broadcast::Sender<Vec<u8>>>,
    /// Peers conectados neste tópico
    peers: Arc<RwLock<Vec<PeerId>>>,
    /// Canal para eventos de peers (join/leave)
    peer_events_sender: Arc<broadcast::Sender<crate::events::Event>>,
}

impl IrohPubSub {
    /// Cria uma nova instância do IrohPubSub
    pub fn new(iroh_backend: Arc<IrohBackend>) -> Self {
        Self {
            iroh_backend,
            topics: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Obtém ou cria um tópico
    async fn get_or_create_topic(&self, topic: &str) -> Result<Arc<IrohTopic>> {
        let mut topics = self.topics.write().await;

        if let Some(existing_topic) = topics.get(topic) {
            return Ok(existing_topic.clone());
        }

        // Cria novo tópico
        let topic_hash = TopicHash::from_raw(topic);
        let (sender, _) = broadcast::channel(1000); // Buffer para 1000 mensagens
        let (peer_events_sender, _) = broadcast::channel(1000); // Buffer para eventos de peers

        // Obtém referência ao SwarmManager do IrohBackend
        let swarm = self.iroh_backend.get_swarm_manager().await?;

        // Subscreve ao tópico no Gossipsub
        {
            let swarm_lock = swarm.read().await;
            if let Some(swarm_manager) = swarm_lock.as_ref() {
                swarm_manager
                    .subscribe_topic(&topic_hash)
                    .await
                    .map_err(|e| {
                        GuardianError::Other(format!("Erro ao subscrever tópico {}: {}", topic, e))
                    })?;
                info!("Subscrito com sucesso ao tópico Gossipsub: {}", topic);
            } else {
                return Err(GuardianError::Other(
                    "SwarmManager não disponível".to_string(),
                ));
            }
        }

        let iroh_topic = Arc::new(IrohTopic {
            topic_name: topic.to_string(),
            topic_hash,
            swarm,
            message_sender: Arc::new(sender),
            peers: Arc::new(RwLock::new(Vec::new())),
            peer_events_sender: Arc::new(peer_events_sender),
        });

        topics.insert(topic.to_string(), iroh_topic.clone());

        Ok(iroh_topic)
    }
}

#[async_trait]
impl PubSubInterface for IrohPubSub {
    type Error = GuardianError;

    async fn topic_subscribe(
        &mut self,
        topic: &str,
    ) -> std::result::Result<Arc<dyn PubSubTopic<Error = GuardianError>>, Self::Error> {
        debug!("Subscrevendo ao tópico via IrohPubSub: {}", topic);

        let iroh_topic = self.get_or_create_topic(topic).await?;

        Ok(iroh_topic as Arc<dyn PubSubTopic<Error = GuardianError>>)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl IrohTopic {
    /// Publica mensagem no tópico
    pub async fn publish(&self, data: &[u8]) -> Result<()> {
        debug!(
            "Publicando mensagem no tópico {}: {} bytes",
            self.topic_name,
            data.len()
        );

        let swarm_lock = self.swarm.read().await;
        if let Some(swarm_manager) = swarm_lock.as_ref() {
            swarm_manager
                .publish_message(&self.topic_hash, data)
                .await
                .map_err(|e| {
                    GuardianError::Other(format!("Erro ao publicar no Gossipsub: {}", e))
                })?;

            debug!(
                "Mensagem publicada com sucesso no tópico {}",
                self.topic_name
            );
            Ok(())
        } else {
            Err(GuardianError::Other(
                "SwarmManager não disponível".to_string(),
            ))
        }
    }

    /// Cria um receiver para mensagens do tópico
    pub fn subscribe_messages(&self) -> broadcast::Receiver<Vec<u8>> {
        self.message_sender.subscribe()
    }

    /// Notifica sobre mensagem recebida (chamado pelo event loop do SwarmManager)
    pub async fn notify_message(&self, data: Vec<u8>) -> Result<()> {
        match self.message_sender.send(data) {
            Ok(_) => {
                debug!(
                    "Mensagem propagada para subscribers do tópico {}",
                    self.topic_name
                );
                Ok(())
            }
            Err(_) => {
                warn!("Nenhum subscriber ativo para o tópico {}", self.topic_name);
                Ok(()) // Não é erro fatal
            }
        }
    }

    /// Adiciona peer ao tópico
    pub async fn add_peer(&self, peer_id: PeerId) {
        let mut peers = self.peers.write().await;
        if !peers.contains(&peer_id) {
            peers.push(peer_id);
            debug!("Peer {} adicionado ao tópico {}", peer_id, self.topic_name);

            // Envia evento de peer conectado (simplesmente usando o PeerId como evento)
            let event: crate::events::Event =
                Arc::new(format!("PeerConnected:{}:{}", peer_id, self.topic_name));
            let _ = self.peer_events_sender.send(event);
        }
    }

    /// Remove peer do tópico
    pub async fn remove_peer(&self, peer_id: &PeerId) {
        let mut peers = self.peers.write().await;
        let was_present = peers.iter().any(|p| p == peer_id);
        peers.retain(|p| p != peer_id);

        if was_present {
            debug!("Peer {} removido do tópico {}", peer_id, self.topic_name);

            // Envia evento de peer desconectado (simplesmente usando o PeerId como evento)
            let event: crate::events::Event =
                Arc::new(format!("PeerDisconnected:{}:{}", peer_id, self.topic_name));
            let _ = self.peer_events_sender.send(event);
        }
    }

    /// Lista peers conectados ao tópico
    pub async fn list_peers(&self) -> Vec<PeerId> {
        let peers = self.peers.read().await;
        peers.clone()
    }
}

#[async_trait]
impl PubSubTopic for IrohTopic {
    type Error = GuardianError;

    async fn publish(&self, message: Vec<u8>) -> std::result::Result<(), Self::Error> {
        self.publish(&message).await
    }

    async fn peers(&self) -> std::result::Result<Vec<PeerId>, Self::Error> {
        Ok(self.list_peers().await)
    }

    async fn watch_peers(
        &self,
    ) -> std::result::Result<Pin<Box<dyn Stream<Item = crate::events::Event> + Send>>, Self::Error>
    {
        // Cria receiver do canal de eventos de peers
        let receiver = self.peer_events_sender.subscribe();

        // Converte broadcast::Receiver em Stream
        let stream = BroadcastStream::new(receiver).filter_map(|result| async {
            result.ok() // Ignora erros de lagged/closed
        });

        Ok(Box::pin(stream))
    }

    async fn watch_messages(
        &self,
    ) -> std::result::Result<Pin<Box<dyn Stream<Item = EventPubSubMessage> + Send>>, Self::Error>
    {
        // Cria receiver do canal de mensagens
        let receiver = self.message_sender.subscribe();
        let _topic_name = self.topic_name.clone();

        // Converte broadcast::Receiver em Stream de EventPubSubMessage
        let stream = BroadcastStream::new(receiver).filter_map(move |result| async move {
            match result {
                Ok(data) => {
                    // Cria EventPubSubMessage a partir dos dados recebidos
                    Some(EventPubSubMessage { content: data })
                }
                Err(_) => None, // Ignora erros de lagged/closed
            }
        });

        Ok(Box::pin(stream))
    }

    fn topic(&self) -> &str {
        &self.topic_name
    }
}

impl IrohBackend {
    /// Cria uma interface PubSub para este backend
    pub fn create_pubsub_interface(self: Arc<Self>) -> IrohPubSub {
        IrohPubSub::new(self)
    }
}
