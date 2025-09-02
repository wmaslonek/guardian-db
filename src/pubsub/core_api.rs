use crate::error::GuardianError;
use crate::events;
use crate::iface::{EventPubSubMessage, PubSubInterface, PubSubTopic, TracerWrapper};
use crate::ipfs_core_api::client::IpfsClient;
use crate::pubsub::event;
use futures::Stream;
use libp2p::PeerId;
use opentelemetry::trace::noop::NoopTracer;
use slog::{Logger, o};
use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tokio_util::sync::CancellationToken;

// equivalente a coreAPIPubSub em go
/// A struct `CoreApiPubSub` gerencia a lógica de pubsub para o nó do GuardianDB.
pub struct CoreApiPubSub {
    pub api: IpfsClient,
    pub logger: Logger,
    pub id: PeerId,
    pub poll_interval: Duration,
    pub tracer: Arc<TracerWrapper>,
    topics: Mutex<HashMap<String, Arc<PsTopic>>>,
    /// Token para cancelamento gracioso de todas as operações
    cancellation_token: CancellationToken,
}

#[async_trait::async_trait]
impl PubSubInterface for CoreApiPubSub {
    type Error = GuardianError;

    async fn topic_subscribe(
        &mut self,
        topic: &str,
    ) -> Result<Arc<dyn crate::iface::PubSubTopic<Error = GuardianError>>, Self::Error> {
        // Usa o método auxiliar para evitar problemas de ownership
        let mut topics_guard = self.topics.lock().await;

        // Se o tópico já estiver na nossa cache, retorna a instância existente.
        if let Some(t) = topics_guard.get(topic) {
            return Ok(t.clone() as Arc<dyn crate::iface::PubSubTopic<Error = GuardianError>>);
        }

        // Cria um novo tópico usando o método auxiliar
        let new_topic = self.create_topic(topic).await;

        // Insere o novo tópico no nosso cache.
        topics_guard.insert(topic.to_string(), new_topic.clone());

        Ok(new_topic as Arc<dyn crate::iface::PubSubTopic<Error = GuardianError>>)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// A struct `PsTopic` será
// armazenada em um `Arc` para compartilhamento seguro entre threads.
/// A struct `PsTopic` contém o estado e a lógica para um único tópico do pubsub.
pub struct PsTopic {
    topic: String,
    ps: Arc<CoreApiPubSub>,
    members: RwLock<Vec<PeerId>>,
    /// Token para cancelamento gracioso das operações deste tópico
    cancellation_token: CancellationToken,
}

impl PsTopic {
    // equivalente a Publish em go
    // Adiciona validação e melhor tratamento de erros
    pub async fn publish(&self, message: &[u8]) -> crate::error::Result<()> {
        // Verifica se o tópico não foi cancelado
        if self.cancellation_token.is_cancelled() {
            return Err(crate::error::GuardianError::Store(
                "Cannot publish to cancelled topic".to_string(),
            ));
        }

        // Validação básica da mensagem
        if message.is_empty() {
            return Err(crate::error::GuardianError::Store(
                "Cannot publish empty message".to_string(),
            ));
        }

        self.ps.api.pubsub_publish(&self.topic, message).await?;
        Ok(())
    }

    // equivalente a Peers em go
    pub async fn peers(&self) -> crate::error::Result<Vec<PeerId>> {
        // O bloqueio de leitura é assíncrono com tokio::sync::RwLock
        let members_guard = self.members.read().await;

        // .clone() cria uma nova cópia do Vec, liberando o bloqueio
        // quando `members_guard` sai de escopo.
        Ok(members_guard.clone())
    }

    // equivalente a peersDiff em go
    // Otimiza operações de lock e melhor gestão de conjuntos
    pub async fn peers_diff(&self) -> crate::error::Result<(Vec<PeerId>, Vec<PeerId>)> {
        // Usa a nova API do kubo_core_api para obter peers atuais
        let all_current_peers_vec = self.ps.api.pubsub_peers(&self.topic).await?;
        let current_peers_set: HashSet<PeerId> = all_current_peers_vec.iter().cloned().collect();

        // Obtém e atualiza membros em uma única operação para eficiência
        let (joining, leaving) = {
            let mut members_guard = self.members.write().await;
            let old_members_set: HashSet<PeerId> = members_guard.iter().cloned().collect();

            // Usa operações de conjunto para encontrar diferenças de forma eficiente e idiomática.
            let joining: Vec<PeerId> = current_peers_set
                .difference(&old_members_set)
                .cloned()
                .collect();
            let leaving: Vec<PeerId> = old_members_set
                .difference(&current_peers_set)
                .cloned()
                .collect();

            // Atualiza a lista de membros internos no mesmo guard
            *members_guard = all_current_peers_vec;

            (joining, leaving)
        };

        Ok((joining, leaving))
    }

    // equivalente a WatchPeers em go
    //
    // Retorna um receptor de canal (`Receiver`) que emitirá eventos de peers
    // entrando ou saindo do tópico.
    // Adiciona cancelamento adequado e melhor gestão de recursos
    pub async fn watch_peers_channel(
        self: &Arc<Self>,
    ) -> crate::error::Result<mpsc::Receiver<Arc<dyn std::any::Any + Send + Sync>>> {
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
                        log::error!("Erro ao verificar a diferença de peers: {:?}", e);
                        return;
                    }
                };

                for pid in joining {
                    let event = event::new_event_peer_join(pid, topic_clone.topic().to_string());
                    // Converte EventPubSub para o tipo esperado
                    let event_any: Arc<dyn std::any::Any + Send + Sync> = Arc::new(event);
                    if tx.send(event_any).await.is_err() {
                        // O receptor foi fechado, então a task não precisa mais rodar.
                        return;
                    }
                }

                for pid in leaving {
                    let event = event::new_event_peer_leave(pid, topic_clone.topic().to_string());
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

    // equivalente a WatchMessages em go
    // Adiciona cancelamento adequado e melhor tratamento de erros
    pub async fn watch_messages(&self) -> crate::error::Result<mpsc::Receiver<EventPubSubMessage>> {
        // A chamada para a API do Kubo para se inscrever no tópico.
        // Espera-se que retorne um Stream de mensagens.
        let mut subscription = self.ps.api.pubsub_subscribe(&self.topic).await?;

        let (tx, rx) = mpsc::channel(128);
        let self_peer_id = self.ps.id; // Usa o ID diretamente já que PeerId implementa Copy
        let cancellation_token = self.cancellation_token.clone();
        let topic_name = self.topic.clone();

        tokio::spawn(async move {
            loop {
                // Verifica cancelamento antes de cada iteração
                if cancellation_token.is_cancelled() {
                    break;
                }

                // Usa select! para permitir cancelamento durante a espera de mensagens
                tokio::select! {
                    msg_result = subscription.next() => {
                        match msg_result {
                            Some(Ok(msg)) => {
                                // Ignora mensagens enviadas pelo próprio nó.
                                if msg.from == self_peer_id {
                                    continue;
                                }

                                let event = event::new_event_message(msg.data);
                                if tx.send(event).await.is_err() {
                                    // O receptor foi fechado, encerra a task.
                                    break;
                                }
                            }
                            Some(Err(e)) => {
                                // Erro no stream, loga e continua tentando
                                log::warn!("Error in pubsub stream for topic {}: {:?}", topic_name, e);
                                continue;
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

    // equivalente a Topic em go
    // Retorna uma referência ao nome do tópico, o que é mais eficiente
    // do que clonar a String.
    pub fn topic(&self) -> &str {
        &self.topic
    }

    /// Cancela todas as operações ativas do tópico
    pub fn cancel(&self) {
        self.cancellation_token.cancel();
    }

    /// Verifica se o tópico foi cancelado
    pub fn is_cancelled(&self) -> bool {
        self.cancellation_token.is_cancelled()
    }

    /// Limpa a lista de membros do tópico
    pub async fn clear_members(&self) {
        let mut members_guard = self.members.write().await;
        members_guard.clear();
    }
}

#[async_trait::async_trait]
impl PubSubTopic for PsTopic {
    type Error = GuardianError;

    async fn publish(&self, message: Vec<u8>) -> crate::error::Result<()> {
        PsTopic::publish(self, &message).await
    }

    async fn peers(&self) -> crate::error::Result<Vec<PeerId>> {
        self.peers().await
    }

    async fn watch_peers(
        &self,
    ) -> crate::error::Result<Pin<Box<dyn Stream<Item = events::Event> + Send>>> {
        // Remove criação desnecessária de instância local
        let (tx, rx) = mpsc::channel(32);

        // Clona dados necessários para a task
        let topic_clone = Arc::new(PsTopic {
            topic: self.topic.clone(),
            ps: self.ps.clone(),
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
                        log::error!("Erro ao verificar a diferença de peers: {:?}", e);
                        return;
                    }
                };

                for pid in joining {
                    let event = event::new_event_peer_join(pid, topic_clone.topic().to_string());
                    // Converte EventPubSub para o tipo esperado como events::Event
                    let event_any: events::Event = Arc::new(event);
                    if tx.send(event_any).await.is_err() {
                        // O receptor foi fechado, então a task não precisa mais rodar.
                        return;
                    }
                }

                for pid in leaving {
                    let event = event::new_event_peer_leave(pid, topic_clone.topic().to_string());
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

    async fn watch_messages(
        &self,
    ) -> crate::error::Result<Pin<Box<dyn Stream<Item = EventPubSubMessage> + Send>>> {
        let receiver = self.watch_messages().await?;
        let stream = ReceiverStream::new(receiver);
        Ok(Box::pin(stream))
    }

    fn topic(&self) -> &str {
        &self.topic
    }
}

impl CoreApiPubSub {
    /// Método auxiliar para criar tópicos sem problemas de ownership
    async fn create_topic(&self, topic: &str) -> Arc<PsTopic> {
        Arc::new(PsTopic {
            topic: topic.to_string(),
            ps: unsafe {
                // SAFETY: Criamos um Arc temporário que não será armazenado
                Arc::from_raw(self as *const Self)
            },
            members: Default::default(),
            cancellation_token: self.cancellation_token.child_token(),
        })
    }

    // equivalente a TopicSubscribe em go
    /// Assina um tópico de pubsub, retornando uma instância de `PubSubTopic`.
    /// Se o tópico já existe, retorna a instância existente.
    pub async fn topic_subscribe_internal(
        self: &Arc<Self>,
        topic: &str,
    ) -> crate::error::Result<Arc<PsTopic>> {
        let mut topics_guard = self.topics.lock().await;

        // Se o tópico já estiver na nossa cache, retorna a instância existente.
        if let Some(t) = topics_guard.get(topic) {
            return Ok(t.clone());
        }

        // Se não, cria uma nova instância do tópico.
        let new_topic = Arc::new(PsTopic {
            topic: topic.to_string(),
            ps: self.clone(), // Clona a referência Arc para o CoreApiPubSub
            members: Default::default(),
            cancellation_token: self.cancellation_token.child_token(),
        });

        // Insere o novo tópico no nosso cache.
        topics_guard.insert(topic.to_string(), new_topic.clone());

        Ok(new_topic)
    }

    // equivalente a NewPubSub em go
    /// Cria uma nova instância de `CoreApiPubSub`.
    /// Os parâmetros `logger` e `tracer` podem ser opcionais.
    pub fn new(
        api: IpfsClient,
        id: PeerId,
        poll_interval: Duration,
        logger: Option<Logger>,
        tracer: Option<Arc<TracerWrapper>>,
    ) -> Arc<Self> {
        let default_logger = slog::Logger::root(slog::Discard, o!());

        // Criar tracer padrão caso não seja fornecido
        let default_tracer = Arc::new(TracerWrapper::Noop(NoopTracer::new()));

        Arc::new(Self {
            topics: Mutex::new(HashMap::new()),
            api,
            id,
            poll_interval,
            logger: logger.unwrap_or(default_logger),
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
    pub async fn remove_topic(&self, topic_name: &str) -> bool {
        let mut topics_guard = self.topics.lock().await;
        topics_guard.remove(topic_name).is_some()
    }

    /// Remove todos os tópicos cancelados do cache
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
