use crate::error::{GuardianError, Result};
use crate::p2p::manager::SwarmManager;
use crate::p2p::pubsub::{HEARTBEAT_INTERVAL, MAX_MESSAGE_SIZE, PROTOCOL};
use crate::traits::{
    DirectChannelEmitter, DirectChannelFactory, DirectChannelOptions, EventPubSubPayload,
};
use async_trait::async_trait;
use futures;
use libp2p::{
    PeerId,
    gossipsub::{Message, TopicHash},
    identity::Keypair,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, RwLock, mpsc};
use tracing::Span;

// Mensagens do protocolo direct channel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectChannelMessage {
    pub message_type: MessageType,
    pub payload: Vec<u8>,
    pub timestamp: u64,
    pub sender: String, // PeerId as string
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageType {
    Data,
    Heartbeat,
    Ack,
}

pub trait DirectChannelNetwork: Send + Sync {
    fn publish_message(&self, topic: &TopicHash, message: &[u8]) -> Result<()>;
    fn subscribe_topic(&self, topic: &TopicHash) -> Result<()>;
    fn get_connected_peers(&self) -> Vec<PeerId>;
    fn get_topic_peers(&self, topic: &TopicHash) -> Vec<PeerId>;
}

// Implementação do DirectChannelNetwork usando SwarmManager
pub struct SwarmBridge {
    span: Span,
    swarm_manager: Arc<Mutex<SwarmManager>>,
    connected_peers: Arc<RwLock<Vec<PeerId>>>,
    topic_peers: Arc<RwLock<HashMap<TopicHash, Vec<PeerId>>>>,
    subscribed_topics: Arc<RwLock<HashMap<TopicHash, bool>>>,
}

impl SwarmBridge {
    pub async fn new(span: Span) -> Result<Self> {
        let keypair = Keypair::generate_ed25519();
        let swarm_manager = SwarmManager::new(span.clone(), keypair)?;

        Ok(Self {
            span,
            swarm_manager: Arc::new(Mutex::new(swarm_manager)),
            connected_peers: Arc::new(RwLock::new(Vec::new())),
            topic_peers: Arc::new(RwLock::new(HashMap::new())),
            subscribed_topics: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Retorna uma referência ao span span para instrumentação
    pub fn span(&self) -> &Span {
        &self.span
    }

    pub async fn start(&self) -> Result<()> {
        let _entered = self.span.enter();
        let mut manager = self.swarm_manager.lock().await;
        manager.start().await?;
        tracing::info!("SwarmBridge iniciada com SwarmManager");
        Ok(())
    }

    /// Atualiza a lista de peers conectados (chamado pelo SwarmManager)
    pub async fn update_connected_peers(&self, peers: Vec<PeerId>) {
        let _entered = self.span.enter();
        let mut connected = self.connected_peers.write().await;
        *connected = peers.clone();

        // Notifica o SwarmManager sobre novos peers
        let manager = self.swarm_manager.lock().await;
        for peer in peers {
            manager.notify_peer_connected(peer).await;
        }

        tracing::debug!(
            "Peers conectados atualizados pelo SwarmManager: {}",
            connected.len()
        );
    }

    /// Atualiza peers de um tópico específico (chamado pelo SwarmManager)
    pub async fn update_topic_peers(&self, topic: TopicHash, peers: Vec<PeerId>) {
        let mut topic_peers = self.topic_peers.write().await;
        topic_peers.insert(topic.clone(), peers.clone());

        // Atualiza no SwarmManager
        let manager = self.swarm_manager.lock().await;
        manager
            .update_topic_peers(topic.clone(), peers.clone())
            .await;

        tracing::debug!(
            "Peers do tópico {:?} atualizados pelo SwarmManager: {}",
            topic,
            peers.len()
        );
    }

    /// Publicação de mensagem integrada com o SwarmManager
    async fn publish(&self, topic: &TopicHash, message: &[u8]) -> Result<()> {
        let manager = self.swarm_manager.lock().await;
        manager.publish_message(topic, message).await?;
        tracing::debug!(
            "Mensagem publicada pelo SwarmManager no tópico: {:?}",
            topic
        );
        Ok(())
    }

    pub async fn stop(&self) -> Result<()> {
        let manager = self.swarm_manager.lock().await;
        manager.stop().await?;
        tracing::info!("SwarmBridge parada");
        Ok(())
    }

    /// Obtém estatísticas essenciais da interface
    pub async fn get_interface_stats(&self) -> HashMap<String, u64> {
        let manager = self.swarm_manager.lock().await;
        let mut stats = manager.get_detailed_stats().await;

        // Adiciona estatísticas específicas da interface
        let connected = self.connected_peers.read().await;
        stats.insert(
            "interface_connected_peers".to_string(),
            connected.len() as u64,
        );

        let topics = self.topic_peers.read().await;
        stats.insert("interface_tracked_topics".to_string(), topics.len() as u64);

        stats
    }
}

impl DirectChannelNetwork for SwarmBridge {
    fn publish_message(&self, topic: &TopicHash, message: &[u8]) -> Result<()> {
        tracing::debug!(
            "Publicando mensagem no tópico: {:?}, {} bytes",
            topic,
            message.len()
        );

        // Verifica se o tópico está inscrito
        let subscribed = {
            let topics = futures::executor::block_on(self.subscribed_topics.read());
            topics.get(topic).copied().unwrap_or(false)
        };

        if !subscribed {
            return Err(GuardianError::Other(format!(
                "Tópico {:?} não está inscrito",
                topic
            )));
        }

        // Usa publicação integrada com SwarmManager
        futures::executor::block_on(self.publish(topic, message))?;

        tracing::info!(
            "Mensagem publicada com sucesso no tópico via SwarmManager: {:?}",
            topic
        );
        Ok(())
    }

    fn subscribe_topic(&self, topic: &TopicHash) -> Result<()> {
        tracing::debug!("Inscrevendo no tópico: {:?}", topic);

        // Marca o tópico como inscrito
        let mut topics = futures::executor::block_on(self.subscribed_topics.write());
        topics.insert(topic.clone(), true);

        // Inicializa lista de peers para o tópico
        let mut topic_peers = futures::executor::block_on(self.topic_peers.write());
        if !topic_peers.contains_key(topic) {
            topic_peers.insert(topic.clone(), Vec::new());
        }

        // Usa inscrição do SwarmManager
        let manager = futures::executor::block_on(self.swarm_manager.lock());
        futures::executor::block_on(manager.subscribe_topic(topic))?;

        tracing::info!(
            "Inscrição realizada com sucesso no tópico via SwarmManager: {:?}",
            topic
        );
        Ok(())
    }

    fn get_connected_peers(&self) -> Vec<PeerId> {
        let peers = futures::executor::block_on(self.connected_peers.read());
        let peer_list = peers.clone();
        tracing::debug!(
            "Retornando {} peers conectados via SwarmManager",
            peer_list.len()
        );
        peer_list
    }

    fn get_topic_peers(&self, topic: &TopicHash) -> Vec<PeerId> {
        tracing::debug!("Obtendo peers do tópico: {:?}", topic);

        let topic_peers = futures::executor::block_on(self.topic_peers.read());
        let peers = topic_peers.get(topic).cloned().unwrap_or_default();

        tracing::debug!(
            "Tópico {:?} tem {} peers conectados via SwarmManager",
            topic,
            peers.len()
        );
        peers
    }
}

// Estado interno do DirectChannel
#[derive(Debug, Clone)]
struct ChannelState {
    #[allow(dead_code)]
    peer_id: PeerId,
    topic: TopicHash,
    connection_status: ConnectionStatus,
    last_activity: Instant,
    message_count: u64,
    last_heartbeat: Instant,
}

#[derive(Debug, Clone)]
enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    #[allow(dead_code)]
    Error(String),
}

// Eventos internos do DirectChannel
#[derive(Debug)]
enum DirectChannelEvent {
    PeerConnected(PeerId),
    PeerDisconnected(PeerId),
    MessageReceived {
        peer: PeerId,
        payload: Vec<u8>,
    },
    MessageSent {
        peer: PeerId,
        success: bool,
        error: Option<String>,
    },
    HeartbeatReceived(PeerId),
    HeartbeatTimeout(PeerId),
}

pub struct DirectChannel {
    span: Span,
    libp2p: Arc<dyn DirectChannelNetwork>,
    emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
    channels: Arc<RwLock<HashMap<PeerId, ChannelState>>>,
    event_sender: mpsc::UnboundedSender<DirectChannelEvent>,
    _event_receiver: Arc<Mutex<Option<mpsc::UnboundedReceiver<DirectChannelEvent>>>>,
    own_peer_id: PeerId,
    running: Arc<Mutex<bool>>,
}

impl DirectChannel {
    // Construtor público
    pub fn new(
        span: Span,
        libp2p: Arc<dyn DirectChannelNetwork>,
        emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
        own_peer_id: PeerId,
    ) -> Self {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();

        Self {
            span,
            libp2p,
            emitter,
            channels: Arc::new(RwLock::new(HashMap::new())),
            event_sender,
            _event_receiver: Arc::new(Mutex::new(Some(event_receiver))),
            own_peer_id,
            running: Arc::new(Mutex::new(false)),
        }
    }

    // Gera o tópico único para comunicação com um peer específico
    fn get_channel_topic(&self, peer: PeerId) -> TopicHash {
        // Ordena os peer IDs para garantir o mesmo tópico em ambos os lados
        let (first, second) = if self.own_peer_id < peer {
            (self.own_peer_id, peer)
        } else {
            (peer, self.own_peer_id)
        };
        let topic_string = format!("{}/channel/{}/{}", PROTOCOL, first, second);
        TopicHash::from_raw(topic_string)
    }

    // Inicia o processamento de eventos
    pub async fn start(&self) -> Result<()> {
        let mut running = self.running.lock().await;
        if *running {
            return Ok(());
        }
        *running = true;

        let mut receiver = self
            ._event_receiver
            .lock()
            .await
            .take()
            .ok_or_else(|| GuardianError::Other("Event receiver already taken".to_string()))?;

        let emitter = self.emitter.clone();
        let span = self.span.clone();
        let channels = self.channels.clone();
        let running_flag = self.running.clone();

        tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                let running = *running_flag.lock().await;
                if !running {
                    break;
                }

                if let Err(e) = Self::handle_event(event, &emitter, &span, &channels).await {
                    tracing::error!("Erro ao processar evento: {}", e);
                }
            }
            tracing::info!("Event processing loop terminated");
        });

        // Inicia o heartbeat loop
        self.start_heartbeat_loop().await;

        Ok(())
    }

    // Inicia o loop de heartbeat para manter conexões ativas
    async fn start_heartbeat_loop(&self) {
        let channels = self.channels.clone();
        let event_sender = self.event_sender.clone();
        let span = self.span.clone();
        let running_flag = self.running.clone();
        let libp2p = self.libp2p.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);

            loop {
                interval.tick().await;

                let running = *running_flag.lock().await;
                if !running {
                    break;
                }

                let peers_to_heartbeat: Vec<(PeerId, TopicHash)> = {
                    let channels_map = channels.read().await;
                    channels_map
                        .iter()
                        .filter_map(|(peer_id, state)| {
                            match state.connection_status {
                                ConnectionStatus::Connected => {
                                    // Verifica se precisa de heartbeat
                                    if state.last_heartbeat.elapsed() > HEARTBEAT_INTERVAL {
                                        Some((*peer_id, state.topic.clone()))
                                    } else {
                                        None
                                    }
                                }
                                _ => None,
                            }
                        })
                        .collect()
                };

                for (peer, topic) in peers_to_heartbeat {
                    // Envia heartbeat
                    if let Err(e) = Self::send_heartbeat(&libp2p, &topic, &span).await {
                        tracing::warn!("Falha ao enviar heartbeat para {}: {}", peer, e);
                        let _ = event_sender.send(DirectChannelEvent::HeartbeatTimeout(peer));
                    } else {
                        tracing::trace!(peer = %peer, "Heartbeat enviado para peer");
                    }
                }

                // Verifica peers em estado de erro e tenta reconectar
                let peers_to_reconnect: Vec<PeerId> = {
                    let channels_map = channels.read().await;
                    channels_map
                        .iter()
                        .filter_map(|(peer_id, state)| {
                            match &state.connection_status {
                                ConnectionStatus::Error(err) => {
                                    // Tenta reconectar após 30 segundos em erro
                                    if state.last_activity.elapsed() > Duration::from_secs(30) {
                                        tracing::debug!(
                                            "Tentando reconexão com peer {} após erro: {}",
                                            peer_id,
                                            err
                                        );
                                        Some(*peer_id)
                                    } else {
                                        None
                                    }
                                }
                                ConnectionStatus::Disconnected => {
                                    // Tenta reconectar peers desconectados após 60 segundos
                                    if state.last_activity.elapsed() > Duration::from_secs(60) {
                                        tracing::debug!(
                                            "Tentando reconexão com peer desconectado: {}",
                                            peer_id
                                        );
                                        Some(*peer_id)
                                    } else {
                                        None
                                    }
                                }
                                _ => None,
                            }
                        })
                        .collect()
                };

                // Atualiza estado para "Connecting" e tenta reconectar
                for peer in peers_to_reconnect {
                    let mut channels_map = channels.write().await;
                    if let Some(state) = channels_map.get_mut(&peer) {
                        state.connection_status = ConnectionStatus::Connecting;
                        state.last_activity = Instant::now();

                        // Tenta reconectar (beacon de descoberta)
                        if let Err(e) = Self::send_heartbeat(&libp2p, &state.topic, &span).await {
                            tracing::warn!("Falha na tentativa de reconexão com {}: {}", peer, e);
                        } else {
                            tracing::info!("Tentativa de reconexão iniciada para peer: {}", peer);
                        }
                    }
                }
            }
        });
    }

    // Envia um heartbeat para um tópico específico
    async fn send_heartbeat(
        libp2p: &Arc<dyn DirectChannelNetwork>,
        topic: &TopicHash,
        _span: &Span,
    ) -> Result<()> {
        let heartbeat_msg = DirectChannelMessage {
            message_type: MessageType::Heartbeat,
            payload: vec![],
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            sender: "heartbeat".to_string(),
        };

        let serialized = serde_cbor::to_vec(&heartbeat_msg)
            .map_err(|e| GuardianError::Other(format!("Erro de serialização heartbeat: {}", e)))?;

        libp2p.publish_message(topic, &serialized)?;
        tracing::trace!(topic = ?topic, "Heartbeat enviado no tópico");
        Ok(())
    }

    // Processa eventos internos
    async fn handle_event(
        event: DirectChannelEvent,
        emitter: &Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
        _span: &Span,
        channels: &Arc<RwLock<HashMap<PeerId, ChannelState>>>,
    ) -> Result<()> {
        match event {
            DirectChannelEvent::MessageReceived { peer, payload } => {
                tracing::debug!("Mensagem recebida de {}: {} bytes", peer, payload.len());

                // Valida tamanho da mensagem
                if payload.len() > MAX_MESSAGE_SIZE {
                    tracing::warn!("Mensagem muito grande de {}: {} bytes", peer, payload.len());
                    return Ok(());
                }

                // Atualiza atividade do canal
                {
                    let mut channels_map = channels.write().await;
                    if let Some(state) = channels_map.get_mut(&peer) {
                        state.last_activity = Instant::now();
                        state.message_count += 1;
                    }
                }

                let event_payload = EventPubSubPayload { payload, peer };
                emitter
                    .emit(event_payload)
                    .await
                    .map_err(|e| GuardianError::Other(format!("Erro ao emitir evento: {}", e)))?;
            }
            DirectChannelEvent::PeerConnected(peer) => {
                tracing::info!("Peer conectado: {}", peer);
                let mut channels_map = channels.write().await;
                if let Some(state) = channels_map.get_mut(&peer) {
                    state.connection_status = ConnectionStatus::Connected;
                    state.last_activity = Instant::now();
                    state.last_heartbeat = Instant::now();
                }
            }
            DirectChannelEvent::PeerDisconnected(peer) => {
                tracing::info!("Peer desconectado: {}", peer);
                let mut channels_map = channels.write().await;
                if let Some(state) = channels_map.get_mut(&peer) {
                    state.connection_status = ConnectionStatus::Disconnected;
                }
            }
            DirectChannelEvent::MessageSent {
                peer,
                success,
                error,
            } => {
                if success {
                    tracing::debug!("Mensagem enviada com sucesso para: {}", peer);
                } else {
                    tracing::warn!("Falha ao enviar mensagem para {}: {:?}", peer, error);
                }
            }
            DirectChannelEvent::HeartbeatReceived(peer) => {
                tracing::trace!(peer = %peer, "Heartbeat recebido de");
                let mut channels_map = channels.write().await;
                if let Some(state) = channels_map.get_mut(&peer) {
                    state.last_activity = Instant::now();
                    state.last_heartbeat = Instant::now();
                }
            }
            DirectChannelEvent::HeartbeatTimeout(peer) => {
                tracing::warn!("Timeout de heartbeat para peer: {}", peer);
                let mut channels_map = channels.write().await;
                if let Some(state) = channels_map.get_mut(&peer) {
                    state.connection_status =
                        ConnectionStatus::Error("Heartbeat timeout".to_string());
                }
            }
        }
        Ok(())
    }

    // Envia dados para um peer específico
    pub async fn send_data(&self, peer: PeerId, payload: Vec<u8>) -> Result<()> {
        if payload.len() > MAX_MESSAGE_SIZE {
            return Err(GuardianError::Other(format!(
                "Mensagem muito grande: {} bytes (máximo: {})",
                payload.len(),
                MAX_MESSAGE_SIZE
            )));
        }

        let topic = self.get_channel_topic(peer);
        let message = DirectChannelMessage {
            message_type: MessageType::Data,
            payload,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            sender: self.own_peer_id.to_string(),
        };

        let serialized = serde_cbor::to_vec(&message)
            .map_err(|e| GuardianError::Other(format!("Erro de serialização: {}", e)))?;

        match self.libp2p.publish_message(&topic, &serialized) {
            Ok(()) => {
                let _ = self.event_sender.send(DirectChannelEvent::MessageSent {
                    peer,
                    success: true,
                    error: None,
                });
                tracing::debug!(
                    "Dados enviados para {}: {} bytes",
                    peer,
                    message.payload.len()
                );
                Ok(())
            }
            Err(e) => {
                let error_msg = format!("Erro ao publicar mensagem: {}", e);
                let _ = self.event_sender.send(DirectChannelEvent::MessageSent {
                    peer,
                    success: false,
                    error: Some(error_msg.clone()),
                });
                Err(GuardianError::Other(error_msg))
            }
        }
    }

    // Conecta a um peer específico
    pub async fn connect_to_peer(&self, peer: PeerId) -> Result<()> {
        let topic = self.get_channel_topic(peer);
        let mut channels_map = self.channels.write().await;

        if let Some(state) = channels_map.get(&peer) {
            match state.connection_status {
                ConnectionStatus::Connected => {
                    tracing::debug!("Já conectado ao peer: {}", peer);
                    return Ok(());
                }
                ConnectionStatus::Connecting => {
                    tracing::debug!("Conexão em andamento com peer: {}", peer);
                    return Ok(());
                }
                _ => {}
            }
        }
        // Inscreve no tópico
        self.libp2p.subscribe_topic(&topic)?;
        // Adiciona ou atualiza o estado do canal
        channels_map.insert(
            peer,
            ChannelState {
                peer_id: peer,
                topic: topic.clone(),
                connection_status: ConnectionStatus::Connecting,
                last_activity: Instant::now(),
                message_count: 0,
                last_heartbeat: Instant::now(),
            },
        );
        tracing::info!("Conectando ao peer {} no tópico: {:?}", peer, topic);
        self.establish_peer_connection(peer, topic.clone()).await?;
        Ok(())
    }

    // Estabelece conexão com um peer específico
    async fn establish_peer_connection(&self, peer: PeerId, topic: TopicHash) -> Result<()> {
        tracing::debug!("Estabelecendo conexão com peer: {}", peer);

        // 1. Verifica se o peer já está nos peers conectados do libp2p
        let connected_peers = self.libp2p.get_connected_peers();
        let is_peer_connected = connected_peers.contains(&peer);

        if is_peer_connected {
            tracing::debug!("Peer {} já está conectado globalmente", peer);
            // Envia evento de conexão estabelecida
            let _ = self
                .event_sender
                .send(DirectChannelEvent::PeerConnected(peer));
            return Ok(());
        }

        // 2. Aguarda um tempo para descoberta de peers no tópico
        let discovery_timeout = Duration::from_secs(10);
        let start_time = Instant::now();

        while start_time.elapsed() < discovery_timeout {
            // Verifica peers do tópico específico
            let topic_peers = self.libp2p.get_topic_peers(&topic);

            if topic_peers.contains(&peer) {
                tracing::info!("Peer {} descoberto no tópico: {:?}", peer, topic);

                // Envia mensagem de handshake para verificar conectividade
                if self.send_handshake_message(&topic, peer).await.is_ok() {
                    tracing::info!("Handshake bem-sucedido com peer: {}", peer);
                    let _ = self
                        .event_sender
                        .send(DirectChannelEvent::PeerConnected(peer));
                    return Ok(());
                }
            }

            // Verifica novamente peers globais (podem ter sido descobertos via mDNS/Kademlia)
            let updated_peers = self.libp2p.get_connected_peers();
            if updated_peers.contains(&peer) {
                tracing::info!("Peer {} conectado via discovery global", peer);
                let _ = self
                    .event_sender
                    .send(DirectChannelEvent::PeerConnected(peer));
                return Ok(());
            }

            // Aguarda antes da próxima verificação
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // 3. Se não conseguiu conectar diretamente, tenta envio de beacon
        tracing::warn!(
            "Peer {} não encontrado diretamente, enviando beacon de descoberta",
            peer
        );
        if let Err(e) = self.send_discovery_beacon(&topic, peer).await {
            tracing::error!("Falha ao enviar beacon de descoberta para {}: {}", peer, e);

            // Marca como erro de conexão mas não falha completamente
            let mut channels_map = self.channels.write().await;
            if let Some(state) = channels_map.get_mut(&peer) {
                state.connection_status =
                    ConnectionStatus::Error(format!("Discovery timeout: {}", e));
            }

            return Err(GuardianError::Other(format!(
                "Timeout na descoberta do peer {} após {}s",
                peer,
                discovery_timeout.as_secs()
            )));
        }

        // 4. Aguarda resposta ao beacon por mais um tempo limitado
        let beacon_timeout = Duration::from_secs(5);
        let beacon_start = Instant::now();

        while beacon_start.elapsed() < beacon_timeout {
            let topic_peers = self.libp2p.get_topic_peers(&topic);
            if topic_peers.contains(&peer) {
                tracing::info!("Peer {} respondeu ao beacon de descoberta", peer);
                let _ = self
                    .event_sender
                    .send(DirectChannelEvent::PeerConnected(peer));
                return Ok(());
            }

            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        // 5. Conexão não estabelecida - mantém estado como "Connecting" para retry futuro
        tracing::warn!(
            "Conexão com peer {} não pôde ser estabelecida no momento",
            peer
        );
        Ok(())
    }

    // Envia mensagem de handshake para verificar conectividade
    async fn send_handshake_message(&self, topic: &TopicHash, target_peer: PeerId) -> Result<()> {
        let handshake_msg = DirectChannelMessage {
            message_type: MessageType::Ack, // Usa ACK como handshake
            payload: format!("handshake:{}", self.own_peer_id).into_bytes(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            sender: self.own_peer_id.to_string(),
        };

        let serialized = serde_cbor::to_vec(&handshake_msg)
            .map_err(|e| GuardianError::Other(format!("Erro serialização handshake: {}", e)))?;

        self.libp2p.publish_message(topic, &serialized)?;
        tracing::debug!("Handshake enviado para peer: {}", target_peer);
        Ok(())
    }

    // Envia beacon de descoberta para atrair peers
    async fn send_discovery_beacon(&self, topic: &TopicHash, target_peer: PeerId) -> Result<()> {
        let beacon_msg = DirectChannelMessage {
            message_type: MessageType::Heartbeat, // Usa Heartbeat como beacon
            payload: format!("discovery_beacon:{}:{}", self.own_peer_id, target_peer).into_bytes(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            sender: self.own_peer_id.to_string(),
        };

        let serialized = serde_cbor::to_vec(&beacon_msg)
            .map_err(|e| GuardianError::Other(format!("Erro serialização beacon: {}", e)))?;

        self.libp2p.publish_message(topic, &serialized)?;
        tracing::debug!("Discovery beacon enviado no tópico: {:?}", topic);
        Ok(())
    }

    // Processa mensagem recebida do Gossipsub
    pub async fn handle_gossipsub_message(&self, message: Message) -> Result<()> {
        // Decodifica a mensagem
        let decoded_msg: DirectChannelMessage = serde_cbor::from_slice(&message.data)
            .map_err(|e| GuardianError::Other(format!("Erro ao decodificar mensagem: {}", e)))?;

        let sender_peer = message
            .source
            .ok_or_else(|| GuardianError::Other("Mensagem sem remetente".to_string()))?;

        match decoded_msg.message_type {
            MessageType::Data => {
                let _ = self.event_sender.send(DirectChannelEvent::MessageReceived {
                    peer: sender_peer,
                    payload: decoded_msg.payload,
                });
            }
            MessageType::Heartbeat => {
                // Verifica se é um discovery beacon
                if let Ok(payload_str) = String::from_utf8(decoded_msg.payload.clone()) {
                    if payload_str.starts_with("discovery_beacon:") {
                        self.handle_discovery_beacon(sender_peer, payload_str)
                            .await?;
                    } else {
                        let _ = self
                            .event_sender
                            .send(DirectChannelEvent::HeartbeatReceived(sender_peer));
                    }
                } else {
                    let _ = self
                        .event_sender
                        .send(DirectChannelEvent::HeartbeatReceived(sender_peer));
                }
            }
            MessageType::Ack => {
                // Verifica se é um handshake
                if let Ok(payload_str) = String::from_utf8(decoded_msg.payload.clone()) {
                    if payload_str.starts_with("handshake:") {
                        self.handle_handshake_response(sender_peer, payload_str)
                            .await?;
                    } else {
                        tracing::trace!(sender_peer = %sender_peer, "ACK recebido de");
                    }
                } else {
                    tracing::trace!(sender_peer = %sender_peer, "ACK recebido de");
                }
            }
        }

        Ok(())
    }

    // Processa beacon de descoberta recebido
    async fn handle_discovery_beacon(
        &self,
        sender_peer: PeerId,
        beacon_payload: String,
    ) -> Result<()> {
        tracing::debug!(
            "Discovery beacon recebido de: {} - {}",
            sender_peer,
            beacon_payload
        );

        // Parse do beacon: "discovery_beacon:sender_peer:target_peer"
        let parts: Vec<&str> = beacon_payload.split(':').collect();
        if parts.len() >= 3 {
            let _beacon_sender = parts[1]; // ID do remetente original
            let beacon_target = parts[2];

            // Verifica se somos o alvo do beacon
            if beacon_target == self.own_peer_id.to_string() {
                tracing::info!("Beacon de descoberta direcionado a nós de: {}", sender_peer);

                // Responde com handshake se ainda não estamos conectados
                let channels_map = self.channels.read().await;
                if let Some(state) = channels_map.get(&sender_peer)
                    && matches!(
                        state.connection_status,
                        ConnectionStatus::Connecting | ConnectionStatus::Disconnected
                    )
                {
                    drop(channels_map); // Libera o lock

                    // Responde ao beacon
                    let topic = self.get_channel_topic(sender_peer);
                    if let Err(e) = self.send_handshake_message(&topic, sender_peer).await {
                        tracing::warn!("Falha ao responder beacon de {}: {}", sender_peer, e);
                    } else {
                        tracing::info!("Handshake de resposta enviado para: {}", sender_peer);
                    }
                }
            }
        }

        Ok(())
    }

    // Processa resposta de handshake
    async fn handle_handshake_response(
        &self,
        sender_peer: PeerId,
        handshake_payload: String,
    ) -> Result<()> {
        tracing::debug!(
            "Handshake recebido de: {} - {}",
            sender_peer,
            handshake_payload
        );

        // Parse do handshake: "handshake:peer_id"
        let parts: Vec<&str> = handshake_payload.split(':').collect();
        if parts.len() >= 2 {
            let handshake_peer = parts[1];
            tracing::info!(
                "Handshake válido recebido de peer: {} (id: {})",
                sender_peer,
                handshake_peer
            );

            // Atualiza estado para conectado se ainda estava conectando
            let mut channels_map = self.channels.write().await;
            if let Some(state) = channels_map.get_mut(&sender_peer) {
                match state.connection_status {
                    ConnectionStatus::Connecting => {
                        state.connection_status = ConnectionStatus::Connected;
                        state.last_activity = Instant::now();
                        state.last_heartbeat = Instant::now();

                        // Notifica conexão estabelecida
                        let _ = self
                            .event_sender
                            .send(DirectChannelEvent::PeerConnected(sender_peer));

                        tracing::info!("Conexão estabelecida com peer: {}", sender_peer);
                    }
                    ConnectionStatus::Connected => {
                        // Atualiza apenas timestamps
                        state.last_activity = Instant::now();
                        state.last_heartbeat = Instant::now();
                        tracing::trace!("Handshake de manutenção recebido de: {}", sender_peer);
                    }
                    _ => {
                        tracing::debug!(
                            "Handshake recebido de peer em estado: {:?}",
                            state.connection_status
                        );
                    }
                }
            }
        }

        Ok(())
    }

    // Para o DirectChannel
    pub async fn stop(&self) -> Result<()> {
        let mut running = self.running.lock().await;
        *running = false;

        // Desconecta todos os peers
        let peers: Vec<PeerId> = {
            let channels_map = self.channels.read().await;
            channels_map.keys().cloned().collect()
        };

        for peer in peers {
            let mut channels_map = self.channels.write().await;
            if let Some(state) = channels_map.remove(&peer) {
                tracing::info!("Peer removido: {} (tópico: {:?})", peer, state.topic);
                let _ = self
                    .event_sender
                    .send(DirectChannelEvent::PeerDisconnected(peer));
            }
        }

        tracing::info!("DirectChannel parado");
        Ok(())
    }

    // Lista peers conectados
    pub async fn list_connected_peers(&self) -> Vec<PeerId> {
        let channels_map = self.channels.read().await;
        channels_map
            .iter()
            .filter_map(|(peer_id, state)| match state.connection_status {
                ConnectionStatus::Connected => Some(*peer_id),
                _ => None,
            })
            .collect()
    }

    // Obter estatísticas do canal
    pub async fn get_channel_stats(&self) -> HashMap<PeerId, (u64, Duration)> {
        let channels_map = self.channels.read().await;
        channels_map
            .iter()
            .map(|(peer_id, state)| {
                (
                    *peer_id,
                    (state.message_count, state.last_activity.elapsed()),
                )
            })
            .collect()
    }

    /// Método interno unificado para fechamento
    async fn close_internal(&self) -> Result<()> {
        tracing::info!("Fechando DirectChannel...");

        // Para o processamento
        self.stop().await?;

        // Fecha o emitter
        if let Err(e) = self.emitter.close().await {
            tracing::warn!("Erro ao fechar emitter: {}", e);
        }

        tracing::info!("DirectChannel fechado com sucesso");
        Ok(())
    }
}

// Implementação do trait DirectChannel do traits.rs
#[async_trait]
impl crate::traits::DirectChannel for DirectChannel {
    type Error = GuardianError;

    async fn connect(&mut self, peer: PeerId) -> std::result::Result<(), Self::Error> {
        tracing::info!("Conectando ao peer: {}", peer);
        self.connect_to_peer(peer).await
    }

    async fn send(&mut self, peer: PeerId, data: Vec<u8>) -> std::result::Result<(), Self::Error> {
        tracing::debug!("Enviando {} bytes para {}", data.len(), peer);
        self.send_data(peer, data).await
    }

    async fn close(&mut self) -> std::result::Result<(), Self::Error> {
        self.close_internal().await
    }

    async fn close_shared(&self) -> std::result::Result<(), Self::Error> {
        self.close_internal().await
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct HolderChannels {
    libp2p: Arc<dyn DirectChannelNetwork>,
    span: Span,
    own_peer_id: PeerId,
}

impl HolderChannels {
    pub fn new(span: Span, libp2p: Arc<dyn DirectChannelNetwork>, own_peer_id: PeerId) -> Self {
        Self {
            libp2p,
            span,
            own_peer_id,
        }
    }

    pub async fn new_channel(
        &self,
        emitter: Box<dyn DirectChannelEmitter<Error = GuardianError>>,
        opts: Option<DirectChannelOptions>,
    ) -> Result<Box<dyn crate::traits::DirectChannel<Error = GuardianError>>> {
        let resolved_opts = opts.unwrap_or_default();
        let span = resolved_opts.span.unwrap_or_else(|| self.span.clone());

        let dc = DirectChannel::new(
            span.clone(),
            self.libp2p.clone(),
            Arc::from(emitter),
            self.own_peer_id,
        );

        // Inicia o processamento
        dc.start().await?;

        tracing::info!(protocol = PROTOCOL, "DirectChannel criado com protocolo");

        Ok(Box::new(dc))
    }
}

pub fn init_direct_channel_factory(span: Span, own_peer_id: PeerId) -> DirectChannelFactory {
    Arc::new(
        move |emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
              opts: Option<DirectChannelOptions>| {
            let span = span.clone();
            let own_peer_id = own_peer_id;
            Box::pin(async move {
                tracing::info!(
                    "Inicializando DirectChannel factory para peer: {}",
                    own_peer_id
                );

                // Cria uma interface para libp2p usando SwarmBridge integrada
                let libp2p_interface = Arc::new(
                    create_unified_libp2p_interface(span.clone())
                        .await
                        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?,
                );

                // Cria o holder para gerenciar o DirectChannel
                let holder = HolderChannels::new(span.clone(), libp2p_interface, own_peer_id);

                // Converte Arc para Box para compatibilidade
                let emitter_box = Box::new(EmitterWrapper(emitter));

                // Cria o canal direto
                let channel = holder
                    .new_channel(emitter_box, opts)
                    .await
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

                Ok(Arc::from(channel)
                    as Arc<
                        dyn crate::traits::DirectChannel<Error = GuardianError>,
                    >)
            })
        },
    )
}

// Wrapper simplificado para converter Arc<dyn DirectChannelEmitter> para Box<dyn DirectChannelEmitter>
struct EmitterWrapper(Arc<dyn DirectChannelEmitter<Error = GuardianError>>);

#[async_trait]
impl DirectChannelEmitter for EmitterWrapper {
    type Error = GuardianError;

    async fn emit(&self, payload: EventPubSubPayload) -> std::result::Result<(), Self::Error> {
        self.0.emit(payload).await
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        self.0.close().await
    }
}

// Função auxiliar para criar um DirectChannel com interface libp2p customizada
pub async fn create_direct_channel_with_libp2p(
    libp2p: Arc<dyn DirectChannelNetwork>,
    emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
    span: Span,
    own_peer_id: PeerId,
) -> Result<DirectChannel> {
    let channel = DirectChannel::new(span.clone(), libp2p, emitter, own_peer_id);

    // Inicia o processamento
    channel.start().await?;

    tracing::info!("DirectChannel criado com interface libp2p integrada");
    Ok(channel)
}

// Configuração unificada da interface LibP2P
pub async fn create_unified_libp2p_interface(span: Span) -> Result<SwarmBridge> {
    let interface = SwarmBridge::new(span.clone()).await?;

    // Inicia o SwarmManager
    interface.start().await?;

    tracing::info!("Interface libp2p unificada inicializada com SwarmManager integrado");
    Ok(interface)
}

// Função para criar um PeerId de teste
pub fn create_test_peer_id() -> PeerId {
    let keypair = libp2p::identity::Keypair::generate_ed25519();
    PeerId::from(keypair.public())
}
