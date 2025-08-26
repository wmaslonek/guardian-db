use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use async_trait::async_trait;
use std::error::Error;
use libp2p::{
    gossipsub::{self, Behaviour, TopicHash, Message, IdentTopic as Topic},
    PeerId,
};
use crate::iface::EventPubSubMessage;
use crate::events;
use crate::pubsub::event::new_event_message;

#[async_trait]
pub trait PubSubTopic: Send + Sync {
    async fn publish(&self, message: Vec<u8>) -> Result<(), gossipsub::PublishError>;
    fn peers(&self) -> Vec<PeerId>;
    async fn watch_peers(&self) -> Result<mpsc::Receiver<events::Event>, Box<dyn Error + Send>>;
    async fn watch_messages(&self) -> Result<mpsc::Receiver<EventPubSubMessage>, Box<dyn Error + Send>>;
    fn topic(&self) -> &str;
}

#[async_trait]
pub trait PubSubInterface: Send + Sync {
    async fn topic_subscribe(&self, topic_name: &str) -> Result<Arc<dyn PubSubTopic>, Box<dyn Error + Send>>;
}

// Equivalente à struct `psTopic` em Go.
// Usamos `Arc` para permitir que esta struct seja compartilhada de forma segura
// entre múltiplas tarefas assíncronas.
pub struct PsTopic {
    topic_hash: TopicHash,
    topic_name: String,
    // Em vez de Arc<RawPubSub>, vamos usar campos independentes
    // que permitam operações thread-safe
    message_broadcaster: tokio::sync::broadcast::Sender<Arc<Message>>,
}

// Equivalente à struct `rawPubSub` em Go.
pub struct RawPubSub {
    pubsub: Arc<Mutex<Behaviour>>,
    id: PeerId,
    // Mutex assíncrono do Tokio para proteger o acesso concorrente ao mapa de tópicos.
    // O `Arc` dentro do HashMap permite compartilhar `PsTopic`s individuais.
    topics: Mutex<HashMap<String, Arc<PsTopic>>>,
    // Adicionando um canal para distribuir eventos de mensagem recebidos
    // Esta é uma adaptação comum em Rust para desacoplar o loop de eventos da rede
    // dos consumidores de mensagens.
    message_broadcaster: tokio::sync::broadcast::Sender<Arc<Message>>,
}

#[async_trait]
impl PubSubTopic for PsTopic {
    // equivalente a `Publish` em go
    async fn publish(&self, _message: Vec<u8>) -> Result<(), gossipsub::PublishError> {
        // Nota: Em libp2p, publish geralmente requer uma referência mutável
        // Para contornar isso, assumimos que há um canal de comando ou método thread-safe
        // Por enquanto, simulamos o comportamento
        // TODO: Implementar canal de comando para publish thread-safe
        Ok(())
    }

    // equivalente a `Peers` em go
    fn peers(&self) -> Vec<PeerId> {
        // A chamada em `rust-libp2p` retorna os pares do "mesh" para um determinado tópico.
        // Como não temos acesso direto ao Behaviour em um contexto thread-safe,
        // retornamos lista vazia por enquanto
        // TODO: Implementar acesso thread-safe aos peers do mesh
        Vec::new()
    }

    // equivalente a `WatchPeers` em go
    async fn watch_peers(&self) -> Result<mpsc::Receiver<events::Event>, Box<dyn std::error::Error + Send>> {
        let (_tx, rx) = mpsc::channel(32);
        let _topic_name_clone = self.topic_name.clone();

        // Simulação de monitoramento de peers
        // TODO: Integrar com sistema de eventos do libp2p
        tokio::spawn(async move {
            // Por enquanto, não emite eventos - implementação mock
            // Em produção, isso seria integrado com o event loop do libp2p
        });

        Ok(rx)
    }

    // equivalente a `WatchMessages` em go
    async fn watch_messages(&self) -> Result<mpsc::Receiver<EventPubSubMessage>, Box<dyn std::error::Error + Send>> {
        // Em vez de criar uma nova assinatura, vamos nos inscrever no "broadcaster"
        // de mensagens. Isso é mais eficiente do que ter múltiplos
        // listeners no stream principal do libp2p.
        let mut receiver = self.message_broadcaster.subscribe();
        let (tx, rx) = mpsc::channel(128); // Buffer de 128 como no original
        let topic_hash = self.topic_hash.clone();

        tokio::spawn(async move {
            while let Ok(message) = receiver.recv().await {
                // Filtra mensagens apenas deste tópico
                if message.topic == topic_hash {
                    let event = new_event_message(message.data.clone());
                    if tx.send(event).await.is_err() {
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }

    // equivalente a `Topic` em go
    fn topic(&self) -> &str {
        &self.topic_name
    }
}

impl RawPubSub {
    // equivalente a `NewPubSub` em go
    pub fn new(ps: Arc<Mutex<Behaviour>>, id: PeerId) -> Arc<Self> {
        // No Go, um logger e tracer nulos são fornecidos se não forem passados.
        // Em Rust, a crate `tracing` é configurada globalmente para a aplicação,
        // então não precisamos passá-la como argumento aqui.

        // Capacidade do canal de broadcast. Pode ser ajustado conforme necessário.
        let (tx, _) = tokio::sync::broadcast::channel(256);

        Arc::new(RawPubSub {
            pubsub: ps,
            topics: Mutex::new(HashMap::new()),
            id,
            message_broadcaster: tx,
        })
    }

    // Método auxiliar para criar com swarm (mock implementation)
    pub fn new_with_swarm(_swarm: Arc<std::sync::Mutex<impl std::marker::Send + 'static>>) -> Result<Arc<Self>, Box<dyn std::error::Error + Send + Sync>> {
        // Por enquanto, implementação mock que cria um Behaviour padrão
        use libp2p::identity::Keypair;
        
        let local_key = Keypair::generate_ed25519();
        let local_peer_id = local_key.public().to_peer_id();
        
        // Cria um comportamento gossipsub padrão
        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .build()
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
        
        let gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Signed(local_key),
            gossipsub_config,
        ).map_err(|e| Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())) as Box<dyn std::error::Error + Send + Sync>)?;

        Ok(Self::new(Arc::new(Mutex::new(gossipsub)), local_peer_id))
    }

    
}

// Esta implementação garante que `RawPubSub` satisfaça o contrato de `PubSubInterface`.
// É o equivalente em Rust a `var _ iface.PubSubInterface = &rawPubSub{}`.
#[async_trait]
impl PubSubInterface for RawPubSub {
    // equivalente a `TopicSubscribe` em go
    async fn topic_subscribe(&self, topic_name: &str) -> Result<Arc<dyn PubSubTopic>, Box<dyn std::error::Error + Send>> {
        // Bloqueia o mutex para garantir acesso exclusivo ao mapa de tópicos.
        // O `await` é necessário porque é um Mutex assíncrono.
        let mut topics = self.topics.lock().await;

        // Se o tópico já existe no nosso mapa, retorna a instância existente.
        if let Some(existing_topic) = topics.get(topic_name) {
            return Ok(existing_topic.clone() as Arc<dyn PubSubTopic>);
        }

        // Se não existir, nos inscrevemos no tópico no libp2p.
        let topic = Topic::new(topic_name.to_string());
        {
            let mut pubsub = self.pubsub.lock().await;
            pubsub.subscribe(&topic).map_err(|e| Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())) as Box<dyn std::error::Error + Send>)?;
        }

        // Cria uma nova instância de PsTopic.
        let new_ps_topic = Arc::new(PsTopic {
            topic_hash: topic.hash(),
            topic_name: topic_name.to_string(),
            message_broadcaster: self.message_broadcaster.clone(),
        });

        // Insere o novo tópico no mapa e o retorna.
        topics.insert(topic_name.to_string(), Arc::clone(&new_ps_topic));

        Ok(new_ps_topic as Arc<dyn PubSubTopic>)
    }
}