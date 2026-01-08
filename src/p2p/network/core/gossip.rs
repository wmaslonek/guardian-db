// Integração nativa do iroh-gossip com IrohBackend
//
// Implementação de PubSubInterface usando iroh-gossip puro
// Com Epidemic Broadcast Trees

use crate::guardian::error::{GuardianError, Result};
use crate::p2p::network::core::IrohBackend;
use crate::traits::{EventPubSubMessage, PubSubInterface, PubSubTopic};
use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use iroh::NodeId;
use iroh_gossip::net::Gossip;
use iroh_gossip::proto::TopicId;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::BroadcastStream;
use tracing::{debug, error, info, warn};

/// Wrapper do IrohBackend que implementa PubSubInterface usando iroh-gossip
pub struct EpidemicPubSub {
    /// Referência ao backend Iroh
    #[allow(dead_code)]
    backend: Arc<IrohBackend>,
    /// Instância do Gossip
    gossip: Gossip,
    /// Cache de tópicos ativos
    topics: Arc<RwLock<HashMap<String, Arc<IrohTopic>>>>,
}

/// Implementação de PubSubTopic para Iroh usando iroh-gossip nativo
pub struct IrohTopic {
    /// Nome do tópico
    topic_name: String,
    /// ID do tópico para iroh-gossip
    topic_id: TopicId,
    /// Canal para broadcast de mensagens recebidas
    message_sender: Arc<broadcast::Sender<Bytes>>,
    /// Peers conectados neste tópico
    peers: Arc<RwLock<Vec<NodeId>>>,
    /// Canal para eventos de peers (join/leave)
    peer_events_sender: Arc<broadcast::Sender<crate::events::Event>>,
    /// Handle da task de event loop do gossip
    _event_task: Option<JoinHandle<()>>,
}

impl EpidemicPubSub {
    /// Cria uma nova instância do EpidemicPubSub usando iroh-gossip
    pub async fn new(backend: Arc<IrohBackend>) -> Result<Self> {
        // Obtém referência ao endpoint do IrohBackend
        let endpoint_arc = backend.get_endpoint().await?;
        let endpoint_lock = endpoint_arc.read().await;
        let endpoint = endpoint_lock
            .as_ref()
            .ok_or_else(|| GuardianError::Other("Endpoint não disponível".to_string()))?
            .clone();
        drop(endpoint_lock);

        // Inicializa o gossip usando o endpoint do IrohBackend
        // Gossip::builder().spawn() retorna Gossip diretamente (construtor não falha)
        let gossip = Gossip::builder().spawn(endpoint);

        info!("EpidemicPubSub inicializado com iroh-gossip nativo");

        Ok(Self {
            backend,
            gossip,
            topics: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Gera TopicId a partir de uma string usando Blake3 (consistente com Iroh)
    fn topic_id_from_str(topic: &str) -> TopicId {
        let hash = blake3::hash(topic.as_bytes());
        TopicId::from_bytes(hash.into())
    }

    /// Obtém ou cria um tópico
    async fn get_or_create_topic(&self, topic: &str) -> Result<Arc<IrohTopic>> {
        let mut topics = self.topics.write().await;

        if let Some(existing_topic) = topics.get(topic) {
            return Ok(existing_topic.clone());
        }

        // Cria novo tópico
        let topic_id = Self::topic_id_from_str(topic);
        let (sender, _) = broadcast::channel(1000); // Buffer para 1000 mensagens
        let (peer_events_sender, _) = broadcast::channel(1000); // Buffer para eventos de peers

        // Subscreve ao tópico no iroh-gossip (sem bootstrap peers inicialmente)
        let gossip_topic = self.gossip.subscribe(topic_id, vec![]).await.map_err(|e| {
            GuardianError::Other(format!("Erro ao subscrever tópico {}: {}", topic, e))
        })?;

        info!(
            "Subscrito com sucesso ao tópico iroh-gossip: {} (topic_id: {})",
            topic,
            topic_id.fmt_short()
        );

        // Clona referências para event loop
        let message_sender = Arc::new(sender);
        let peers = Arc::new(RwLock::new(Vec::new()));
        let peer_events_sender_clone = Arc::new(peer_events_sender);
        let topic_name_clone = topic.to_string();

        let message_sender_clone = message_sender.clone();
        let peers_clone = peers.clone();
        let peer_events_sender_event_clone = peer_events_sender_clone.clone();

        // Spawna task para processar eventos do gossip
        let event_task = tokio::spawn(async move {
            let mut gossip_topic = gossip_topic;

            while let Some(event_result) = gossip_topic.next().await {
                match event_result {
                    Ok(iroh_gossip::api::Event::Received(msg)) => {
                        debug!(
                            "Mensagem recebida no tópico {}: {} bytes",
                            topic_name_clone,
                            msg.content.len()
                        );
                        if let Err(e) = message_sender_clone.send(msg.content.clone()) {
                            warn!(
                                "Erro ao enviar mensagem para subscribers do tópico {}: {}",
                                topic_name_clone, e
                            );
                        }
                    }
                    Ok(iroh_gossip::api::Event::NeighborUp(node_id)) => {
                        debug!("Peer {} conectado ao tópico {}", node_id, topic_name_clone);
                        let mut peers_lock = peers_clone.write().await;
                        if !peers_lock.contains(&node_id) {
                            peers_lock.push(node_id);
                        }

                        let event: crate::events::Event =
                            Arc::new(format!("PeerConnected:{}:{}", node_id, topic_name_clone));
                        let _ = peer_events_sender_event_clone.send(event);
                    }
                    Ok(iroh_gossip::api::Event::NeighborDown(node_id)) => {
                        debug!(
                            "Peer {} desconectado do tópico {}",
                            node_id, topic_name_clone
                        );
                        let mut peers_lock = peers_clone.write().await;
                        peers_lock.retain(|p| p != &node_id);

                        let event: crate::events::Event =
                            Arc::new(format!("PeerDisconnected:{}:{}", node_id, topic_name_clone));
                        let _ = peer_events_sender_event_clone.send(event);
                    }
                    Ok(iroh_gossip::api::Event::Lagged) => {
                        warn!("Event loop atrasado no tópico {}", topic_name_clone);
                    }
                    Err(e) => {
                        error!("Erro no event stream do tópico {}: {}", topic_name_clone, e);
                        break;
                    }
                }
            }

            info!("Event loop encerrado para tópico {}", topic_name_clone);
        });

        let iroh_topic = Arc::new(IrohTopic {
            topic_name: topic.to_string(),
            topic_id,
            message_sender,
            peers,
            peer_events_sender: peer_events_sender_clone,
            _event_task: Some(event_task),
        });

        topics.insert(topic.to_string(), iroh_topic.clone());

        Ok(iroh_topic)
    }

    /// Retorna referência ao Gossip
    pub fn gossip(&self) -> &Gossip {
        &self.gossip
    }

    /// Publica mensagem em um tópico específico
    /// Método de conveniência que encapsula o acesso ao Gossip
    pub async fn publish_to_topic(&self, topic: &str, data: &[u8]) -> Result<()> {
        let topics = self.topics.read().await;

        if let Some(iroh_topic) = topics.get(topic) {
            iroh_topic.publish(data, &self.gossip).await
        } else {
            Err(GuardianError::Other(format!(
                "Tópico {} não encontrado - subscreva primeiro",
                topic
            )))
        }
    }
}

#[async_trait]
impl PubSubInterface for EpidemicPubSub {
    type Error = GuardianError;

    async fn topic_subscribe(
        &self,
        topic: &str,
    ) -> std::result::Result<Arc<dyn PubSubTopic<Error = GuardianError>>, Self::Error> {
        debug!("Subscrevendo ao tópico via EpidemicPubSub: {}", topic);

        let iroh_topic = self.get_or_create_topic(topic).await?;

        Ok(iroh_topic as Arc<dyn PubSubTopic<Error = GuardianError>>)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl IrohTopic {
    /// Publica mensagem no tópico usando iroh-gossip
    pub async fn publish(&self, data: &[u8], gossip: &Gossip) -> Result<()> {
        debug!(
            "Publicando mensagem no tópico {}: {} bytes",
            self.topic_name,
            data.len()
        );

        // Subscreve novamente para obter GossipTopic handle (método idiomático)
        let mut topic = gossip.subscribe(self.topic_id, vec![]).await.map_err(|e| {
            GuardianError::Other(format!("Erro ao acessar tópico para publicação: {}", e))
        })?;

        // Publica mensagem
        topic
            .broadcast(Bytes::copy_from_slice(data))
            .await
            .map_err(|e| {
                GuardianError::Other(format!("Erro ao publicar mensagem via iroh-gossip: {}", e))
            })?;

        debug!(
            "Mensagem publicada com sucesso no tópico {}",
            self.topic_name
        );
        Ok(())
    }

    /// Lista peers conectados ao tópico
    pub async fn list_peers(&self) -> Vec<NodeId> {
        let peers = self.peers.read().await;
        peers.clone()
    }
}

/// Implementação de PubSubTopic usando iroh-gossip nativo com NodeId
#[async_trait]
impl PubSubTopic for IrohTopic {
    type Error = GuardianError;

    async fn publish(&self, _message: Vec<u8>) -> std::result::Result<(), Self::Error> {
        // NOTA: Método deprecado - use EpidemicPubSub::publish_to_topic() ao invés
        // Este método existe apenas para compatibilidade com a trait
        Err(GuardianError::Other(
            "Use EpidemicPubSub::publish_to_topic() para publicar mensagens".to_string(),
        ))
    }

    async fn peers(&self) -> std::result::Result<Vec<NodeId>, Self::Error> {
        // Retorna lista de NodeIds diretamente sem conversão
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

        // Converte broadcast::Receiver em Stream de EventPubSubMessage
        let stream = BroadcastStream::new(receiver).filter_map(move |result| async move {
            match result {
                Ok(data) => {
                    // Converte Bytes para Vec<u8>
                    Some(EventPubSubMessage {
                        content: data.to_vec(),
                    })
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
    /// Cria uma interface PubSub para este backend usando iroh-gossip
    pub async fn create_pubsub_interface(self: Arc<Self>) -> Result<EpidemicPubSub> {
        EpidemicPubSub::new(self).await
    }
}
