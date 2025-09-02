use crate::error::{GuardianError, Result};
use crate::events;
use crate::iface::{EventPubSubMessage, PubSubInterface, PubSubTopic};
use crate::ipfs_core_api::IpfsClient;
use crate::pubsub::event::new_event_message;
use async_trait::async_trait;
use futures::stream::{Stream, StreamExt};
use libp2p::PeerId;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Tópico PubSub integrado com ipfs_core_api
pub struct PsTopic {
    topic_name: String,
    ipfs_client: Arc<IpfsClient>,
    // Token para cancelamento de operações
    cancellation_token: CancellationToken,
}

/// PubSub principal usando IPFS Core API
pub struct RawPubSub {
    ipfs_client: Arc<IpfsClient>,
    id: PeerId,
    // Mapa thread-safe de tópicos ativos
    topics: RwLock<HashMap<String, Arc<PsTopic>>>,
}

#[async_trait]
impl PubSubTopic for PsTopic {
    type Error = GuardianError;

    /// Publica uma mensagem no tópico usando ipfs_core_api
    async fn publish(&self, message: Vec<u8>) -> std::result::Result<(), Self::Error> {
        info!(
            "Publicando mensagem no tópico '{}': {} bytes",
            self.topic_name,
            message.len()
        );

        self.ipfs_client
            .pubsub_publish(&self.topic_name, &message)
            .await?;

        debug!(
            "Mensagem publicada com sucesso no tópico '{}'",
            self.topic_name
        );
        Ok(())
    }

    /// Lista peers conectados ao tópico
    async fn peers(&self) -> std::result::Result<Vec<PeerId>, Self::Error> {
        debug!("Listando peers do tópico: {}", self.topic_name);

        let peers = self.ipfs_client.pubsub_peers(&self.topic_name).await?;

        debug!(
            "Encontrados {} peers para tópico '{}'",
            peers.len(),
            self.topic_name
        );
        Ok(peers)
    }

    /// Monitora mudanças nos peers do tópico
    async fn watch_peers(
        &self,
    ) -> std::result::Result<Pin<Box<dyn Stream<Item = events::Event> + Send>>, Self::Error> {
        let (tx, rx) = mpsc::channel(32);
        let ipfs_client = self.ipfs_client.clone();
        let topic_name = self.topic_name.clone();
        let token = self.cancellation_token.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            let mut last_peers: Vec<PeerId> = Vec::new();

            loop {
                tokio::select! {
                    _ = token.cancelled() => {
                        debug!("Monitoramento de peers cancelado para tópico: {}", topic_name);
                        break;
                    }
                    _ = interval.tick() => {
                        match ipfs_client.pubsub_peers(&topic_name).await {
                            Ok(current_peers) => {
                                // Detecta mudanças nos peers
                                for peer in &current_peers {
                                    if !last_peers.contains(peer) {
                                        // Cria evento de join como Arc<dyn Any>
                                        let join_event: events::Event = Arc::new(crate::iface::EventPubSub::Join {
                                            peer: *peer,
                                            topic: topic_name.clone()
                                        });
                                        if tx.send(join_event).await.is_err() {
                                            break;
                                        }
                                    }
                                }

                                for peer in &last_peers {
                                    if !current_peers.contains(peer) {
                                        // Cria evento de leave como Arc<dyn Any>
                                        let leave_event: events::Event = Arc::new(crate::iface::EventPubSub::Leave {
                                            peer: *peer,
                                            topic: topic_name.clone()
                                        });
                                        if tx.send(leave_event).await.is_err() {
                                            break;
                                        }
                                    }
                                }

                                last_peers = current_peers;
                            }
                            Err(e) => {
                                error!("Erro ao obter peers do tópico '{}': {}", topic_name, e);
                            }
                        }
                    }
                }
            }
        });

        let stream = ReceiverStream::new(rx);
        Ok(Box::pin(stream))
    }

    /// Monitora mensagens recebidas no tópico
    async fn watch_messages(
        &self,
    ) -> std::result::Result<Pin<Box<dyn Stream<Item = EventPubSubMessage> + Send>>, Self::Error>
    {
        let (tx, rx) = mpsc::channel(128);
        let ipfs_client = self.ipfs_client.clone();
        let topic_name = self.topic_name.clone();
        let token = self.cancellation_token.clone();

        // Inicia o monitoramento de mensagens
        tokio::spawn(async move {
            match ipfs_client.pubsub_subscribe(&topic_name).await {
                Ok(mut stream) => {
                    debug!("Stream de mensagens iniciado para tópico: {}", topic_name);

                    loop {
                        tokio::select! {
                            _ = token.cancelled() => {
                                debug!("Monitoramento de mensagens cancelado para tópico: {}", topic_name);
                                break;
                            }
                            msg_result = stream.next() => {
                                match msg_result {
                                    Some(Ok(msg)) => {
                                        let event = new_event_message(msg.data);
                                        if tx.send(event).await.is_err() {
                                            debug!("Receptor de mensagens desconectado para tópico: {}", topic_name);
                                            break;
                                        }
                                    }
                                    Some(Err(e)) => {
                                        error!("Erro no stream de mensagens para tópico '{}': {}", topic_name, e);
                                        break;
                                    }
                                    None => {
                                        debug!("Stream de mensagens finalizado para tópico: {}", topic_name);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    error!(
                        "Erro ao criar stream de mensagens para tópico '{}': {}",
                        topic_name, e
                    );
                }
            }
        });

        let stream = ReceiverStream::new(rx);
        Ok(Box::pin(stream))
    }

    /// Retorna o nome do tópico
    fn topic(&self) -> &str {
        &self.topic_name
    }
}

impl RawPubSub {
    /// Cria uma nova instância do RawPubSub usando ipfs_core_api
    pub async fn new(ipfs_client: Arc<IpfsClient>) -> Result<Arc<Self>> {
        let node_info = ipfs_client.id().await?;
        let id = node_info.id;

        info!("Inicializando RawPubSub com node ID: {}", id);

        Ok(Arc::new(RawPubSub {
            ipfs_client,
            id,
            topics: RwLock::new(HashMap::new()),
        }))
    }

    /// Método auxiliar para criar com configuração customizada
    pub async fn new_with_config(ipfs_client: Arc<IpfsClient>) -> Result<Arc<Self>> {
        Self::new(ipfs_client).await
    }

    /// Obtém estatísticas dos tópicos ativos
    pub async fn get_topic_stats(&self) -> HashMap<String, usize> {
        let topics = self.topics.read().await;
        let mut stats = HashMap::new();

        for (topic_name, topic) in topics.iter() {
            match topic.peers().await {
                Ok(peers) => {
                    stats.insert(topic_name.clone(), peers.len());
                }
                Err(e) => {
                    warn!("Erro ao obter peers do tópico '{}': {}", topic_name, e);
                    stats.insert(topic_name.clone(), 0);
                }
            }
        }

        stats
    }

    /// Lista todos os tópicos ativos
    pub async fn list_topics(&self) -> Vec<String> {
        let topics = self.topics.read().await;
        topics.keys().cloned().collect()
    }

    /// Obtém o ID do peer local
    pub fn local_peer_id(&self) -> PeerId {
        self.id
    }

    /// Remove um tópico (unsubscribe)
    pub async fn topic_unsubscribe(&self, topic_name: &str) -> Result<()> {
        let mut topics = self.topics.write().await;

        if let Some(topic) = topics.remove(topic_name) {
            topic.cancellation_token.cancel();
            info!("Tópico '{}' removido com sucesso", topic_name);
        } else {
            warn!("Tentativa de remover tópico inexistente: {}", topic_name);
        }

        Ok(())
    }
}

/// Implementação da interface PubSub usando ipfs_core_api
#[async_trait]
impl PubSubInterface for RawPubSub {
    type Error = GuardianError;

    /// Subscreve a um tópico e retorna uma instância PubSubTopic
    async fn topic_subscribe(
        &mut self,
        topic_name: &str,
    ) -> std::result::Result<Arc<dyn PubSubTopic<Error = GuardianError>>, Self::Error> {
        let mut topics = self.topics.write().await;

        // Se o tópico já existe, retorna a instância existente
        if let Some(existing_topic) = topics.get(topic_name) {
            info!("Reutilizando tópico existente: {}", topic_name);
            return Ok(existing_topic.clone() as Arc<dyn PubSubTopic<Error = GuardianError>>);
        }

        info!("Criando nova subscrição para tópico: {}", topic_name);

        // Cria novo tópico
        let new_topic = Arc::new(PsTopic {
            topic_name: topic_name.to_string(),
            ipfs_client: self.ipfs_client.clone(),
            cancellation_token: CancellationToken::new(),
        });

        // Registra o tópico na subscrição do IPFS
        let _subscription_stream = self
            .ipfs_client
            .pubsub_subscribe(topic_name)
            .await
            .map_err(|e| GuardianError::Ipfs(format!("Erro ao subscrever tópico IPFS: {}", e)))?;

        // Adiciona ao nosso mapa local
        topics.insert(topic_name.to_string(), new_topic.clone());

        info!("Tópico '{}' criado e subscrito com sucesso", topic_name);

        Ok(new_topic as Arc<dyn PubSubTopic<Error = GuardianError>>)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
