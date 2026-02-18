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
    #[allow(dead_code)]
    topic_id: TopicId,
    /// Canal para broadcast de mensagens recebidas
    message_sender: Arc<broadcast::Sender<Bytes>>,
    /// Peers conectados neste tópico
    peers: Arc<RwLock<Vec<NodeId>>>,
    /// Bootstrap peers usados para formar o mesh
    bootstrap_peers: Arc<RwLock<Vec<NodeId>>>,
    /// Canal para eventos de peers (join/leave)
    peer_events_sender: Arc<broadcast::Sender<crate::events::Event>>,
    /// Handle da task de event loop do gossip
    _event_task: Option<JoinHandle<()>>,
    /// Sender para broadcast de mensagens (obtido via gossip_topic.split())
    gossip_sender: Arc<RwLock<GossipSender>>,
}

impl EpidemicPubSub {
    /// Cria uma nova instância do EpidemicPubSub usando iroh-gossip
    ///
    /// IMPORTANTE: Usa o Gossip do IrohBackend que foi registrado no Router,
    /// garantindo que as conexões de entrada sejam corretamente roteadas para
    /// este mesmo Gossip.
    pub async fn new(backend: Arc<IrohBackend>) -> Result<Self> {
        // CORREÇÃO: Usar o Gossip do IrohBackend ao invés de criar um novo
        // Isso é crucial porque o Router aceita conexões ALPN e roteia para
        // o Gossip registrado. Se usarmos um Gossip diferente, as mensagens
        // de entrada nunca chegarão aos nossos subscriptions.
        let gossip_arc = backend.get_gossip().await?;
        let gossip_lock = gossip_arc.read().await;
        let gossip = gossip_lock
            .as_ref()
            .ok_or_else(|| {
                GuardianError::Other("Gossip não inicializado no IrohBackend".to_string())
            })?
            .clone();
        drop(gossip_lock);

        info!("EpidemicPubSub inicializado usando Gossip compartilhado do IrohBackend");

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
        self.get_or_create_topic_with_peers(topic, vec![]).await
    }

    /// Obtém ou cria um tópico com bootstrap peers específicos
    pub async fn get_or_create_topic_with_peers(
        &self,
        topic: &str,
        bootstrap_peers: Vec<NodeId>,
    ) -> Result<Arc<IrohTopic>> {
        let topic_id = Self::topic_id_from_str(topic);

        // Se o tópico já existe E temos novos bootstrap peers,
        // usa join_peers para adicionar os peers ao mesh existente
        let topics_read = self.topics.read().await;
        if let Some(existing_topic) = topics_read.get(topic) {
            let topic_clone = existing_topic.clone();
            drop(topics_read);

            if !bootstrap_peers.is_empty() {
                debug!(
                    "Topic {} já existe, adicionando {} bootstrap peers ao mesh via join_peers",
                    topic,
                    bootstrap_peers.len()
                );

                // Atualiza o campo bootstrap_peers do IrohTopic existente
                {
                    let mut bp = topic_clone.bootstrap_peers.write().await;
                    // Adiciona novos peers sem duplicar
                    for peer in &bootstrap_peers {
                        if !bp.contains(peer) {
                            bp.push(*peer);
                        }
                    }
                    debug!("Bootstrap peers atualizados: {} peers no total", bp.len());
                }

                // CORREÇÃO: Usa join_peers no GossipSender existente para adicionar peers ao mesh
                // Isso é muito mais eficiente que criar nova subscription e garante que
                // o mesmo GossipSender usado para publicar tenha os peers no seu mesh
                {
                    let sender = topic_clone.gossip_sender.write().await;
                    sender
                        .join_peers(bootstrap_peers.clone())
                        .await
                        .map_err(|e| {
                            warn!("[JOIN_PEERS] Erro ao adicionar peers ao mesh: {}", e);
                            GuardianError::Other(format!(
                                "Erro ao adicionar peers via join_peers: {}",
                                e
                            ))
                        })?;
                    info!(
                        "[JOIN_PEERS] Peers {:?} adicionados ao mesh do tópico {}",
                        bootstrap_peers
                            .iter()
                            .map(|p| p.fmt_short())
                            .collect::<Vec<_>>(),
                        topic
                    );
                }

                // CORREÇÃO: Espera pelo NeighborUp antes de retornar
                // Verifica se os peers foram adicionados à lista de neighbors
                // com timeout para evitar bloqueio infinito
                let expected_peers: std::collections::HashSet<_> =
                    bootstrap_peers.iter().cloned().collect();
                let mut retry_count = 0;
                const MAX_RETRIES: u32 = 20; // 20 * 100ms = 2 segundos max

                loop {
                    let current_peers = topic_clone.peers.read().await;
                    let connected: std::collections::HashSet<_> =
                        current_peers.iter().cloned().collect();

                    // Verifica se todos os bootstrap peers estão conectados
                    let all_connected = expected_peers.iter().all(|p| connected.contains(p));
                    drop(current_peers);

                    if all_connected {
                        debug!(
                            "[JOIN_PEERS] Mesh formado com sucesso para tópico {}, {} peers conectados",
                            topic,
                            expected_peers.len()
                        );
                        break;
                    }

                    retry_count += 1;
                    if retry_count >= MAX_RETRIES {
                        warn!(
                            "[JOIN_PEERS] Timeout esperando NeighborUp para tópico {} após {}ms. Peers esperados: {:?}, Conectados: {:?}",
                            topic,
                            retry_count * 100,
                            expected_peers
                                .iter()
                                .map(|p| p.fmt_short())
                                .collect::<Vec<_>>(),
                            connected.iter().map(|p| p.fmt_short()).collect::<Vec<_>>()
                        );
                        break;
                    }

                    // Aguarda e tenta novamente
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }

                // NÃO criamos mais nova subscription aqui - join_peers já adiciona os peers ao mesh
                // e os eventos (NeighborUp, Received) continuarão chegando no receiver original
            } else {
                debug!("Topic {} já existe, sem novos peers para adicionar", topic);
            }

            return Ok(topic_clone);
        }
        drop(topics_read);

        let mut topics = self.topics.write().await;

        // Double-check após adquirir write lock
        if let Some(existing_topic) = topics.get(topic) {
            return Ok(existing_topic.clone());
        }

        // Cria novo tópico
        let (sender, _) = broadcast::channel(1000); // Buffer para 1000 mensagens
        let (peer_events_sender, _) = broadcast::channel(1000); // Buffer para eventos de peers

        // Subscreve ao tópico no iroh-gossip com bootstrap peers fornecidos
        debug!(
            "Subscrevendo ao tópico {} com {} bootstrap peers",
            topic,
            bootstrap_peers.len()
        );

        // Clone bootstrap_peers para armazenar no IrohTopic
        let bootstrap_peers_for_topic = bootstrap_peers.clone();

        let gossip_topic = self
            .gossip
            .subscribe(topic_id, bootstrap_peers)
            .await
            .map_err(|e| {
                GuardianError::Other(format!("Erro ao subscrever tópico {}: {}", topic, e))
            })?;

        info!(
            "Subscrito com sucesso ao tópico iroh-gossip: {} (topic_id: {})",
            topic,
            topic_id.fmt_short()
        );

        // CORREÇÃO: Usa split() para separar sender e receiver
        // O sender será armazenado e usado para publicar mensagens
        // O receiver será usado pelo event_task para receber mensagens
        let (gossip_sender, mut gossip_receiver) = gossip_topic.split();
        let gossip_sender_arc = Arc::new(RwLock::new(gossip_sender));

        // Clona referências para event loop
        let message_sender = Arc::new(sender);
        let peers = Arc::new(RwLock::new(Vec::new()));
        let peer_events_sender_clone = Arc::new(peer_events_sender);
        let topic_name_clone = topic.to_string();

        let message_sender_clone = message_sender.clone();
        let peers_clone = peers.clone();
        let peer_events_sender_event_clone = peer_events_sender_clone.clone();

        // Spawna task para processar eventos do gossip receiver
        let event_task = tokio::spawn(async move {
            while let Some(event_result) = gossip_receiver.next().await {
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

                        // CORREÇÃO: Envia EventPubSub::Join para que o handler possa processá-lo corretamente
                        let event: crate::events::Event =
                            Arc::new(crate::traits::EventPubSub::Join {
                                peer: node_id,
                                topic: topic_name_clone.clone(),
                            });
                        let _ = peer_events_sender_event_clone.send(event);
                    }
                    Ok(iroh_gossip::api::Event::NeighborDown(node_id)) => {
                        debug!(
                            "Peer {} desconectado do tópico {}",
                            node_id, topic_name_clone
                        );
                        let mut peers_lock = peers_clone.write().await;
                        peers_lock.retain(|p| p != &node_id);

                        // CORREÇÃO: Envia EventPubSub::Leave para que o handler possa processá-lo corretamente
                        let event: crate::events::Event =
                            Arc::new(crate::traits::EventPubSub::Leave {
                                peer: node_id,
                                topic: topic_name_clone.clone(),
                            });
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
            bootstrap_peers: Arc::new(RwLock::new(bootstrap_peers_for_topic)),
            peer_events_sender: peer_events_sender_clone,
            _event_task: Some(event_task),
            gossip_sender: gossip_sender_arc,
        });

        topics.insert(topic.to_string(), iroh_topic.clone());

        Ok(iroh_topic)
    }

    /// Retorna referência ao Gossip
    pub fn gossip(&self) -> &Gossip {
        &self.gossip
    }

    /// Retorna um tópico existente, se houver
    pub async fn get_topic(&self, topic: &str) -> Option<Arc<IrohTopic>> {
        let topics = self.topics.read().await;
        topics.get(topic).cloned()
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

    /// Garante que um tópico existe com peers específicos como bootstrap
    /// Usado para conectar peers ao gossip mesh antes de publicar mensagens
    pub async fn ensure_topic_with_peers(
        &self,
        topic: &str,
        bootstrap_peers: Vec<NodeId>,
    ) -> Result<Arc<IrohTopic>> {
        self.get_or_create_topic_with_peers(topic, bootstrap_peers)
            .await
    }

    /// Re-subscreve ao tópico com novos bootstrap peers
    /// Usa join_peers para adicionar peers ao mesh existente
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

        // Usa get_or_create_topic_with_peers que agora usa join_peers internamente
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
    ///
    /// CORREÇÃO: Usa o GossipSender armazenado ao invés de criar nova subscription.
    /// Criar nova subscription gera um KEY diferente, e mensagens podem não ser entregues
    /// corretamente entre subscriptions com KEYs diferentes.
    pub async fn publish(&self, data: &[u8], _gossip: &Gossip) -> Result<()> {
        debug!(
            "Publicando mensagem no tópico {}: {} bytes",
            self.topic_name,
            data.len()
        );

        // Usa o GossipSender armazenado para broadcast
        // Isso garante que usamos a mesma subscription KEY que está recebendo mensagens
        let sender = self.gossip_sender.write().await;
        sender
            .broadcast(Bytes::copy_from_slice(data))
            .await
            .map_err(|e| {
                GuardianError::Other(format!("Erro ao publicar mensagem via iroh-gossip: {}", e))
            })?;

        debug!(
            "Mensagem publicada com sucesso no tópico {} via GossipSender",
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

    async fn publish(&self, message: Vec<u8>) -> std::result::Result<(), Self::Error> {
        debug!(
            "[IrohTopic::publish] Publicando {} bytes no tópico {}",
            message.len(),
            self.topic_name
        );

        // Usa o GossipSender armazenado para broadcast
        // Isso garante que usamos a mesma subscription KEY que está recebendo mensagens
        let sender = self.gossip_sender.write().await;
        sender
            .broadcast(Bytes::copy_from_slice(&message))
            .await
            .map_err(|e| {
                error!(
                    "[IrohTopic::publish] Erro ao publicar no tópico {}: {}",
                    self.topic_name, e
                );
                GuardianError::Other(format!("Erro ao publicar mensagem via iroh-gossip: {}", e))
            })?;

        debug!(
            "[IrohTopic::publish] Mensagem publicada com sucesso no tópico {}",
            self.topic_name
        );
        Ok(())
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
