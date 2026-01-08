use crate::events;
use crate::guardian::error::GuardianError;
use crate::p2p;
use crate::p2p::network::core::gossip::EpidemicPubSub;
use crate::traits::{EventPubSubMessage, PubSubInterface, PubSubTopic, TracerWrapper};
use futures::Stream;
use iroh::NodeId;
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

pub const PROTOCOL: &str = "/guardian-db/direct-channel/1.2.0";
pub const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
pub const MAX_MESSAGE_SIZE: usize = 1024 * 1024; // 1MB

/// A struct `CoreApiPubSub` gerencia a lógica de pubsub P2P para o nó do GuardianDB via iroh-gossip.
pub struct CoreApiPubSub {
    pub epidemic_pubsub: Arc<EpidemicPubSub>,
    pub span: Span,
    pub id: NodeId,
    pub poll_interval: Duration,
    pub tracer: Arc<TracerWrapper>,
    topics: Mutex<HashMap<String, Arc<PsTopic>>>,
    /// Token para cancelamento gracioso de todas as operações
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

        // Se o tópico já estiver na nossa cache, retorna a instância existente.
        if let Some(t) = topics_guard.get(topic) {
            return Ok(t.clone() as Arc<dyn crate::traits::PubSubTopic<Error = GuardianError>>);
        }

        // Subscreve ao tópico via EpidemicPubSub
        let inner_topic = self.epidemic_pubsub.topic_subscribe(topic).await?;

        // Cria um novo Arc<CoreApiPubSub> compartilhando os mesmos recursos
        let ps_arc = Arc::new(CoreApiPubSub {
            epidemic_pubsub: self.epidemic_pubsub.clone(),
            span: self.span.clone(),
            id: self.id,
            poll_interval: self.poll_interval,
            tracer: self.tracer.clone(),
            topics: Mutex::new(HashMap::new()),
            cancellation_token: self.cancellation_token.clone(),
        });

        // Cria um novo tópico
        let new_topic = Arc::new(PsTopic {
            topic: topic.to_string(),
            ps: ps_arc,
            inner_topic,
            members: Default::default(),
            cancellation_token: self.cancellation_token.child_token(),
        });

        // Insere o novo tópico no nosso cache.
        topics_guard.insert(topic.to_string(), new_topic.clone());

        Ok(new_topic as Arc<dyn crate::traits::PubSubTopic<Error = GuardianError>>)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// A struct `PsTopic` será
// armazenada em um `Arc` para compartilhamento seguro entre threads.
/// A struct `PsTopic` contém o estado e a lógica para um único tópico do pubsub P2P via iroh-gossip.
pub struct PsTopic {
    topic: String,
    ps: Arc<CoreApiPubSub>,
    /// Tópico do EpidemicPubSub para comunicação P2P
    inner_topic: Arc<dyn PubSubTopic<Error = GuardianError>>,
    members: RwLock<Vec<NodeId>>,
    /// Token para cancelamento gracioso das operações deste tópico
    cancellation_token: CancellationToken,
}

impl PsTopic {
    // Publica mensagem via iroh-gossip
    #[instrument(level = "debug", skip(self, message))]
    pub async fn publish(&self, message: &[u8]) -> crate::guardian::error::Result<()> {
        // Verifica se o tópico não foi cancelado
        if self.cancellation_token.is_cancelled() {
            return Err(crate::guardian::error::GuardianError::Store(
                "Cannot publish to cancelled topic".to_string(),
            ));
        }

        // Validação básica da mensagem
        if message.is_empty() {
            return Err(crate::guardian::error::GuardianError::Store(
                "Cannot publish empty message".to_string(),
            ));
        }

        // Publica via iroh-gossip
        self.inner_topic.publish(message.to_vec()).await?;
        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    pub async fn peers(&self) -> crate::guardian::error::Result<Vec<NodeId>> {
        // Obtém peers do tópico via iroh-gossip
        self.inner_topic.peers().await
    }

    // Calcula diferença de peers (joining/leaving) via iroh-gossip
    #[instrument(level = "debug", skip(self))]
    pub async fn peers_diff(&self) -> crate::guardian::error::Result<(Vec<NodeId>, Vec<NodeId>)> {
        let current_peers = self.inner_topic.peers().await?;
        let mut members_guard = self.members.write().await;

        // Identifica peers que entraram
        let joining: Vec<NodeId> = current_peers
            .iter()
            .filter(|peer| !members_guard.contains(peer))
            .copied()
            .collect();

        // Identifica peers que saíram
        let leaving: Vec<NodeId> = members_guard
            .iter()
            .filter(|peer| !current_peers.contains(peer))
            .copied()
            .collect();

        // Atualiza lista de membros
        *members_guard = current_peers;

        Ok((joining, leaving))
    }

    // Retorna um receptor de canal (`Receiver`) que emitirá eventos de peers
    // entrando ou saindo do tópico.
    // Adiciona cancelamento adequado e melhor gestão de recursos
    #[instrument(level = "debug", skip(self))]
    pub async fn watch_peers_channel(
        self: &Arc<Self>,
    ) -> crate::guardian::error::Result<mpsc::Receiver<Arc<dyn std::any::Any + Send + Sync>>> {
        let (tx, rx) = mpsc::channel(32);

        // Clona o Arc para que a nova task possa ter sua própria referência.
        let topic_clone = self.clone();
        let cancellation_token = self.cancellation_token.clone();

        tokio::spawn(async move {
            loop {
                // Verifica cancelamento antes de cada iteração
                if cancellation_token.is_cancelled() {
                    break;
                }

                // Chama a função que calcula a diferença de peers
                let peers_diff_result = topic_clone.peers_diff().await;

                let (joining, leaving) = match peers_diff_result {
                    Ok((j, l)) => (j, l),
                    Err(e) => {
                        // Loga o erro e encerra a task.
                        // Quando `tx` sai de escopo (é dropado), o lado do receptor
                        // saberá que o canal foi fechado.
                        error!("Erro ao verificar a diferença de peers: {:?}", e);
                        return;
                    }
                };

                for node_id in joining {
                    let event = p2p::new_event_peer_join(node_id, topic_clone.topic().to_string());
                    // Converte EventPubSub para o tipo esperado
                    let event_any: Arc<dyn std::any::Any + Send + Sync> = Arc::new(event);
                    if tx.send(event_any).await.is_err() {
                        // O receptor foi fechado, então a task não precisa mais rodar.
                        return;
                    }
                }

                for node_id in leaving {
                    let event = p2p::new_event_peer_leave(node_id, topic_clone.topic().to_string());
                    // Converte EventPubSub para o tipo esperado
                    let event_any: Arc<dyn std::any::Any + Send + Sync> = Arc::new(event);
                    if tx.send(event_any).await.is_err() {
                        return;
                    }
                }

                // Usa select! para permitir cancelamento durante o sleep
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
        // Obtém stream de mensagens do tópico P2P via iroh-gossip
        let mut message_stream = self.inner_topic.watch_messages().await?;

        let (tx, rx) = mpsc::channel(128);
        let cancellation_token = self.cancellation_token.clone();
        let _topic_name = self.topic.clone();

        tokio::spawn(async move {
            loop {
                // Verifica cancelamento antes de cada iteração
                if cancellation_token.is_cancelled() {
                    break;
                }

                // Usa select! para permitir cancelamento durante a espera de mensagens
                tokio::select! {
                    msg_result = message_stream.next() => {
                        match msg_result {
                            Some(msg) => {
                                // Mensagem já vem filtrada do EpidemicPubSub
                                if tx.send(msg).await.is_err() {
                                    // O receptor foi fechado, encerra a task.
                                    break;
                                }
                            }
                            None => {
                                // Stream fechado, encerra a task
                                break;
                            }
                        }
                    }
                    _ = cancellation_token.cancelled() => {
                        // Cancelamento solicitado, encerra a task
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }

    // Retorna uma referência ao nome do tópico, o que é mais eficiente
    // do que clonar a String.
    #[instrument(level = "debug", skip(self))]
    pub fn topic(&self) -> &str {
        &self.topic
    }

    /// Cancela todas as operações ativas do tópico
    #[instrument(level = "debug", skip(self))]
    pub fn cancel(&self) {
        self.cancellation_token.cancel();
    }

    /// Verifica se o tópico foi cancelado
    #[instrument(level = "debug", skip(self))]
    pub fn is_cancelled(&self) -> bool {
        self.cancellation_token.is_cancelled()
    }

    /// Limpa a lista de membros do tópico
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

        // Clona dados necessários para a task
        let topic_clone = Arc::new(PsTopic {
            topic: self.topic.clone(),
            ps: self.ps.clone(),
            inner_topic: self.inner_topic.clone(),
            members: RwLock::new(self.members.read().await.clone()),
            cancellation_token: self.cancellation_token.clone(),
        });

        tokio::spawn(async move {
            loop {
                // Verifica cancelamento antes de cada iteração
                if topic_clone.cancellation_token.is_cancelled() {
                    break;
                }

                // Chama a função que calcula a diferença de peers
                let peers_diff_result = topic_clone.peers_diff().await;

                let (joining, leaving) = match peers_diff_result {
                    Ok((j, l)) => (j, l),
                    Err(e) => {
                        // Loga o erro e encerra a task.
                        error!("Erro ao verificar a diferença de peers: {:?}", e);
                        return;
                    }
                };

                for node_id in joining {
                    let event = p2p::new_event_peer_join(node_id, topic_clone.topic().to_string());
                    // Converte EventPubSub para o tipo esperado como events::Event
                    let event_any: events::Event = Arc::new(event);
                    if tx.send(event_any).await.is_err() {
                        // O receptor foi fechado, então a task não precisa mais rodar.
                        return;
                    }
                }

                for node_id in leaving {
                    let event = p2p::new_event_peer_leave(node_id, topic_clone.topic().to_string());
                    // Converte EventPubSub para o tipo esperado como events::Event
                    let event_any: events::Event = Arc::new(event);
                    if tx.send(event_any).await.is_err() {
                        return;
                    }
                }

                // Usa select! para permitir cancelamento durante o sleep
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
    /// Assina um tópico de pubsub P2P via iroh-gossip, retornando uma instância de `PubSubTopic`.
    /// Se o tópico já existe, retorna a instância existente.
    /// Este método deve ser chamado em contextos onde já se tem um Arc<CoreApiPubSub>
    #[instrument(level = "debug", skip(self))]
    pub async fn topic_subscribe_internal(
        self: &Arc<Self>,
        topic: &str,
    ) -> crate::guardian::error::Result<Arc<PsTopic>> {
        let mut topics_guard = self.topics.lock().await;

        // Se o tópico já estiver na nossa cache, retorna a instância existente.
        if let Some(t) = topics_guard.get(topic) {
            return Ok(t.clone());
        }

        // Subscreve ao tópico via EpidemicPubSub
        let inner_topic = self.epidemic_pubsub.topic_subscribe(topic).await?;

        // Cria uma nova instância do tópico com o tópico P2P
        let new_topic = Arc::new(PsTopic {
            topic: topic.to_string(),
            ps: Arc::clone(self),
            inner_topic,
            members: Default::default(),
            cancellation_token: self.cancellation_token.child_token(),
        });

        // Insere o novo tópico no nosso cache.
        topics_guard.insert(topic.to_string(), new_topic.clone());

        Ok(new_topic)
    }

    /// Cria uma nova instância de `CoreApiPubSub` usando EpidemicPubSub para comunicação P2P.
    /// Os parâmetros `span` e `tracer` podem ser opcionais.
    #[instrument(level = "debug", skip(epidemic_pubsub, span, tracer))]
    pub fn new(
        epidemic_pubsub: Arc<EpidemicPubSub>,
        id: NodeId,
        poll_interval: Duration,
        span: Option<Span>,
        tracer: Option<Arc<TracerWrapper>>,
    ) -> Arc<Self> {
        // Criar tracer padrão caso não seja fornecido
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

    /// Método para cancelar todas as operações do PubSub
    pub fn cancel(&self) {
        self.cancellation_token.cancel();
    }

    /// Verifica se o PubSub foi cancelado
    pub fn is_cancelled(&self) -> bool {
        self.cancellation_token.is_cancelled()
    }

    /// Remove um tópico específico do cache
    #[instrument(level = "debug", skip(self))]
    pub async fn remove_topic(&self, topic_name: &str) -> bool {
        let mut topics_guard = self.topics.lock().await;
        topics_guard.remove(topic_name).is_some()
    }

    /// Remove todos os tópicos cancelados do cache
    #[instrument(level = "debug", skip(self))]
    pub async fn cleanup_cancelled_topics(&self) -> usize {
        let mut topics_guard = self.topics.lock().await;
        let mut cancelled_topics = Vec::new();

        // Identifica tópicos cancelados
        for (name, topic) in topics_guard.iter() {
            if topic.is_cancelled() {
                cancelled_topics.push(name.clone());
            }
        }

        // Remove tópicos cancelados
        for topic_name in &cancelled_topics {
            topics_guard.remove(topic_name);
        }

        cancelled_topics.len()
    }

    /// Retorna estatísticas dos tópicos ativos
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
