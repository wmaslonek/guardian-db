use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use crate::error::GuardianError;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use opentelemetry::trace::noop::NoopTracer;
use libp2p::PeerId;
use slog::{o, Logger};
use crate::kubo_core_api::client::KuboCoreApiClient;
use crate::iface::{PubSubInterface, EventPubSubMessage, PubSubTopic, TracerWrapper};
use crate::events;
use crate::pubsub::event;
use std::pin::Pin;
use futures::Stream;

// equivalente a coreAPIPubSub em go
/// A struct `CoreApiPubSub` gerencia a lógica de pubsub para o nó do GuardianDB.
pub struct CoreApiPubSub {
    pub api: KuboCoreApiClient,
    pub logger: Logger,
    pub id: PeerId,
    pub poll_interval: Duration,
    pub tracer: Arc<TracerWrapper>,
    topics: Mutex<HashMap<String, Arc<PsTopic>>>,
}

#[async_trait::async_trait]
impl PubSubInterface for Arc<CoreApiPubSub> {
    type Error = GuardianError;

    async fn topic_subscribe(&mut self, topic: &str) -> Result<Arc<dyn crate::iface::PubSubTopic<Error = GuardianError>>, Self::Error> {
        let ps_topic = self.topic_subscribe_internal(topic).await?;
        Ok(ps_topic as Arc<dyn crate::iface::PubSubTopic<Error = GuardianError>>)
    }
}

// A struct `PsTopic` será
// armazenada em um `Arc` para compartilhamento seguro entre threads.
/// A struct `PsTopic` contém o estado e a lógica para um único tópico do pubsub.
pub struct PsTopic {
    topic: String,
    ps: Arc<CoreApiPubSub>,
    members: RwLock<Vec<PeerId>>,
}

impl PsTopic {
    // equivalente a Publish em go
    pub async fn publish(&self, message: &[u8]) -> crate::error::Result<()> {
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
    pub async fn peers_diff(&self) -> crate::error::Result<(Vec<PeerId>, Vec<PeerId>)> {
        let old_members_set: HashSet<PeerId> = self.peers().await?.into_iter().collect();

        // Usa a nova API do kubo_core_api
        let all_current_peers_vec = self.ps.api.pubsub_peers(&self.topic).await?;
        let current_peers_set: HashSet<PeerId> = all_current_peers_vec.iter().cloned().collect();

        // Usa operações de conjunto para encontrar diferenças de forma eficiente e idiomática.
        let joining: Vec<PeerId> = current_peers_set.difference(&old_members_set).cloned().collect();
        let leaving: Vec<PeerId> = old_members_set.difference(&current_peers_set).cloned().collect();
        
        // Atualiza a lista de membros internos com um bloqueio de escrita
        let mut members_guard = self.members.write().await;
        *members_guard = all_current_peers_vec;

        Ok((joining, leaving))
    }

    // equivalente a WatchPeers em go
    //
    // Retorna um receptor de canal (`Receiver`) que emitirá eventos de peers
    // entrando ou saindo do tópico.
    // NOTA: Para que `tokio::spawn` funcione, a chamada a esta função
    // provavelmente virá de uma `Arc<PsTopic>` para permitir o clone e
    // a movimentação da referência para dentro da nova task.
    pub async fn watch_peers_channel(self: &Arc<Self>) -> crate::error::Result<mpsc::Receiver<Arc<dyn std::any::Any + Send + Sync>>> {
        let (tx, rx) = mpsc::channel(32);
        
        // Clona o Arc para que a nova task possa ter sua própria referência.
        let topic_clone = self.clone();

        tokio::spawn(async move {
            loop {
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

                // O `select!` do Go é substituído por um simples sleep. O cancelamento
                // (ctx.Done()) é tratado em Rust quando o `Future` da task é abortado
                // ou o receptor do canal (`rx`) é dropado.
                let poll_interval = topic_clone.ps.poll_interval; // Supondo que poll_interval é acessível
                tokio::time::sleep(poll_interval).await;
            }
        });

        Ok(rx)
    }

    // equivalente a WatchMessages em go
    pub async fn watch_messages(&self) -> crate::error::Result<mpsc::Receiver<EventPubSubMessage>> {
        // A chamada para a API do Kubo para se inscrever no tópico.
        // Espera-se que retorne um Stream de mensagens.
        let mut subscription = self.ps.api.pubsub_subscribe(&self.topic)
            .await?;

        let (tx, rx) = mpsc::channel(128);
        let self_peer_id = self.ps.id.clone(); // Clona o ID para usar na task

        tokio::spawn(async move {
            // Itera sobre o stream de mensagens da inscrição.
            // O loop termina quando o stream é fechado (retorna None).
            while let Some(msg_result) = subscription.next().await {
                match msg_result {
                    Ok(msg) => {
                        // Ignora mensagens enviadas pelo próprio nó.
                        if msg.from == self_peer_id {
                            continue;
                        }

                        let event = event::new_event_message(msg.data);
                        if tx.send(event).await.is_err() {
                            // O receptor foi fechado, encerra a task.
                            break;
                        }
                    },
                    Err(_e) => {
                        // Erro no stream, encerra a task
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

    async fn watch_peers(&self) -> crate::error::Result<Pin<Box<dyn Stream<Item = events::Event> + Send>>> {
        // Criamos um novo receiver para a trait
        let (tx, rx) = mpsc::channel(32);
        
        // Cria uma cópia dos dados necessários para evitar problemas de lifetime
        let topic_name = self.topic.clone();
        let ps_clone = self.ps.clone();
        // Clona os dados dentro do RwLock, não o RwLock em si
        let members_data = self.members.read().await.clone();

        tokio::spawn(async move {
            // Cria uma instância local que não depende de self
            let local_topic = PsTopic {
                topic: topic_name,
                ps: ps_clone,
                members: RwLock::new(members_data),
            };
            
            loop {
                // Chama a função que calcula a diferença de peers
                let peers_diff_result = local_topic.peers_diff().await;

                let (joining, leaving) = match peers_diff_result {
                    Ok((j, l)) => (j, l),
                    Err(e) => {
                        // Loga o erro e encerra a task.
                        log::error!("Erro ao verificar a diferença de peers: {:?}", e);
                        return;
                    }
                };

                for pid in joining {
                    let event = event::new_event_peer_join(pid, local_topic.topic().to_string());
                    // Converte EventPubSub para o tipo esperado como events::Event
                    let event_any: events::Event = Arc::new(event);
                    if tx.send(event_any).await.is_err() {
                        // O receptor foi fechado, então a task não precisa mais rodar.
                        return;
                    }
                }

                for pid in leaving {
                    let event = event::new_event_peer_leave(pid, local_topic.topic().to_string());
                    // Converte EventPubSub para o tipo esperado como events::Event
                    let event_any: events::Event = Arc::new(event);
                    if tx.send(event_any).await.is_err() {
                        return;
                    }
                }

                // O `select!` do Go é substituído por um simples sleep. O cancelamento
                // (ctx.Done()) é tratado em Rust quando o `Future` da task é abortado
                // ou o receptor do canal (`rx`) é dropado.
                let poll_interval = local_topic.ps.poll_interval;
                tokio::time::sleep(poll_interval).await;
            }
        });

        let stream = ReceiverStream::new(rx);
        Ok(Box::pin(stream))
    }
    
    async fn watch_messages(&self) -> crate::error::Result<Pin<Box<dyn Stream<Item = EventPubSubMessage> + Send>>> {
        let receiver = self.watch_messages().await?;
        let stream = ReceiverStream::new(receiver);
        Ok(Box::pin(stream))
    }

    fn topic(&self) -> &str {
        &self.topic
    }
}

impl CoreApiPubSub {
    // equivalente a TopicSubscribe em go
    /// Assina um tópico de pubsub, retornando uma instância de `PubSubTopic`.
    /// Se o tópico já existe, retorna a instância existente.
    pub async fn topic_subscribe_internal(self: &Arc<Self>, topic: &str) -> crate::error::Result<Arc<PsTopic>> {
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
        });
        
        // Insere o novo tópico no nosso cache.
        topics_guard.insert(topic.to_string(), new_topic.clone());

        Ok(new_topic)
    }

    // equivalente a NewPubSub em go
    /// Cria uma nova instância de `CoreApiPubSub`.
    /// Os parâmetros `logger` e `tracer` podem ser opcionais.
    pub fn new(
        api: KuboCoreApiClient,
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
        })
    }
}