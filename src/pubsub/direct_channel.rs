use crate::error::{GuardianError, Result};
use crate::iface::{
    DirectChannelEmitter, DirectChannelFactory, DirectChannelOptions, EventPubSubPayload,
};
use async_trait::async_trait;
use libp2p::{
    PeerId,
    gossipsub::{Message, TopicHash},
};
use serde::{Deserialize, Serialize};
use slog::Logger;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, RwLock, mpsc};

// Equivalente às constantes globais em go
const PROTOCOL: &str = "/go-orbit-db/direct-channel/1.2.0";
const DELIMITED_READ_MAX_SIZE: usize = 1024 * 1024 * 4; // 4mb
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const MAX_MESSAGE_SIZE: usize = 1024 * 1024; // 1MB

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

// Interface para comunicação com libp2p usando Gossipsub
pub trait LibP2PInterface: Send + Sync {
    fn publish_message(&self, topic: &TopicHash, message: &[u8]) -> Result<()>;
    fn subscribe_topic(&self, topic: &TopicHash) -> Result<()>;
    fn get_connected_peers(&self) -> Vec<PeerId>;
    fn get_topic_peers(&self, topic: &TopicHash) -> Vec<PeerId>;
}

// Implementação real do LibP2PInterface usando Gossipsub
pub struct GossipsubInterface {
    logger: Logger,
    // Em uma implementação real, você teria acesso ao Swarm aqui
    // Por enquanto, usamos um placeholder que pode ser expandido
}

impl GossipsubInterface {
    pub fn new(logger: Logger) -> Self {
        Self { logger }
    }
}

impl LibP2PInterface for GossipsubInterface {
    fn publish_message(&self, topic: &TopicHash, message: &[u8]) -> Result<()> {
        slog::debug!(
            self.logger,
            "Publicando mensagem no tópico: {:?}, {} bytes",
            topic,
            message.len()
        );
        // TODO: Implementar com swarm.behaviour_mut().gossipsub.publish(topic, message)
        Ok(())
    }

    fn subscribe_topic(&self, topic: &TopicHash) -> Result<()> {
        slog::debug!(self.logger, "Inscrevendo no tópico: {:?}", topic);
        // TODO: Implementar com swarm.behaviour_mut().gossipsub.subscribe(topic)
        Ok(())
    }

    fn get_connected_peers(&self) -> Vec<PeerId> {
        // TODO: Retornar peers reais conectados
        vec![]
    }

    fn get_topic_peers(&self, topic: &TopicHash) -> Vec<PeerId> {
        slog::debug!(self.logger, "Obtendo peers do tópico: {:?}", topic);
        // TODO: Implementar com swarm.behaviour().gossipsub.mesh_peers(topic)
        vec![]
    }
}

// Estado interno do DirectChannel
#[derive(Debug, Clone)]
struct ChannelState {
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

// Equivalente à struct `directChannel` em go
pub struct DirectChannel {
    logger: Logger,
    libp2p: Arc<dyn LibP2PInterface>,
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
        logger: Logger,
        libp2p: Arc<dyn LibP2PInterface>,
        emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
        own_peer_id: PeerId,
    ) -> Self {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();

        Self {
            logger,
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
        let logger = self.logger.clone();
        let channels = self.channels.clone();
        let running_flag = self.running.clone();

        tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                let running = *running_flag.lock().await;
                if !running {
                    break;
                }

                if let Err(e) = Self::handle_event(event, &emitter, &logger, &channels).await {
                    slog::error!(logger, "Erro ao processar evento: {}", e);
                }
            }
            slog::info!(logger, "Event processing loop terminated");
        });

        // Inicia o heartbeat loop
        self.start_heartbeat_loop().await;

        Ok(())
    }

    // Inicia o loop de heartbeat para manter conexões ativas
    async fn start_heartbeat_loop(&self) {
        let channels = self.channels.clone();
        let event_sender = self.event_sender.clone();
        let logger = self.logger.clone();
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
                    if let Err(e) = Self::send_heartbeat(&libp2p, &topic, &logger).await {
                        slog::warn!(logger, "Falha ao enviar heartbeat para {}: {}", peer, e);
                        let _ = event_sender.send(DirectChannelEvent::HeartbeatTimeout(peer));
                    } else {
                        slog::trace!(logger, "Heartbeat enviado para peer: {}", peer);
                    }
                }
            }
        });
    }

    // Envia um heartbeat para um tópico específico
    async fn send_heartbeat(
        libp2p: &Arc<dyn LibP2PInterface>,
        topic: &TopicHash,
        logger: &Logger,
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
        slog::trace!(logger, "Heartbeat enviado no tópico: {:?}", topic);
        Ok(())
    }

    // Processa eventos internos
    async fn handle_event(
        event: DirectChannelEvent,
        emitter: &Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
        logger: &Logger,
        channels: &Arc<RwLock<HashMap<PeerId, ChannelState>>>,
    ) -> Result<()> {
        match event {
            DirectChannelEvent::MessageReceived { peer, payload } => {
                slog::debug!(
                    logger,
                    "Mensagem recebida de {}: {} bytes",
                    peer,
                    payload.len()
                );

                // Valida tamanho da mensagem
                if payload.len() > MAX_MESSAGE_SIZE {
                    slog::warn!(
                        logger,
                        "Mensagem muito grande de {}: {} bytes",
                        peer,
                        payload.len()
                    );
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
                slog::info!(logger, "Peer conectado: {}", peer);
                let mut channels_map = channels.write().await;
                if let Some(state) = channels_map.get_mut(&peer) {
                    state.connection_status = ConnectionStatus::Connected;
                    state.last_activity = Instant::now();
                    state.last_heartbeat = Instant::now();
                }
            }
            DirectChannelEvent::PeerDisconnected(peer) => {
                slog::info!(logger, "Peer desconectado: {}", peer);
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
                    slog::debug!(logger, "Mensagem enviada com sucesso para: {}", peer);
                } else {
                    slog::warn!(
                        logger,
                        "Falha ao enviar mensagem para {}: {:?}",
                        peer,
                        error
                    );
                }
            }
            DirectChannelEvent::HeartbeatReceived(peer) => {
                slog::trace!(logger, "Heartbeat recebido de: {}", peer);
                let mut channels_map = channels.write().await;
                if let Some(state) = channels_map.get_mut(&peer) {
                    state.last_activity = Instant::now();
                    state.last_heartbeat = Instant::now();
                }
            }
            DirectChannelEvent::HeartbeatTimeout(peer) => {
                slog::warn!(logger, "Timeout de heartbeat para peer: {}", peer);
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
    async fn send_data(&self, peer: PeerId, payload: Vec<u8>) -> Result<()> {
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
                slog::debug!(
                    self.logger,
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
    async fn connect_to_peer(&self, peer: PeerId) -> Result<()> {
        let topic = self.get_channel_topic(peer);
        let mut channels_map = self.channels.write().await;

        if let Some(state) = channels_map.get(&peer) {
            match state.connection_status {
                ConnectionStatus::Connected => {
                    slog::debug!(self.logger, "Já conectado ao peer: {}", peer);
                    return Ok(());
                }
                ConnectionStatus::Connecting => {
                    slog::debug!(self.logger, "Conexão em andamento com peer: {}", peer);
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

        slog::info!(
            self.logger,
            "Conectando ao peer {} no tópico: {:?}",
            peer,
            topic
        );

        // Simula conexão estabelecida (em implementação real seria baseado em eventos do libp2p)
        let _ = self
            .event_sender
            .send(DirectChannelEvent::PeerConnected(peer));

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
                let _ = self
                    .event_sender
                    .send(DirectChannelEvent::HeartbeatReceived(sender_peer));
            }
            MessageType::Ack => {
                slog::trace!(self.logger, "ACK recebido de: {}", sender_peer);
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
                slog::info!(
                    self.logger,
                    "Peer removido: {} (tópico: {:?})",
                    peer,
                    state.topic
                );
                let _ = self
                    .event_sender
                    .send(DirectChannelEvent::PeerDisconnected(peer));
            }
        }

        slog::info!(self.logger, "DirectChannel parado");
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
}

// Implementação do trait DirectChannel do iface.rs
#[async_trait]
impl crate::iface::DirectChannel for DirectChannel {
    type Error = GuardianError;

    async fn connect(&mut self, peer: PeerId) -> std::result::Result<(), Self::Error> {
        slog::info!(self.logger, "Conectando ao peer: {}", peer);
        self.connect_to_peer(peer).await
    }

    async fn send(&mut self, peer: PeerId, data: Vec<u8>) -> std::result::Result<(), Self::Error> {
        slog::debug!(self.logger, "Enviando {} bytes para {}", data.len(), peer);
        self.send_data(peer, data).await
    }

    async fn close(&mut self) -> std::result::Result<(), Self::Error> {
        slog::info!(self.logger, "Fechando DirectChannel...");

        // Para o processamento
        self.stop().await?;

        // Fecha o emitter
        if let Err(e) = self.emitter.close().await {
            slog::warn!(self.logger, "Erro ao fechar emitter: {}", e);
        }

        slog::info!(self.logger, "DirectChannel fechado com sucesso");
        Ok(())
    }
}

// Equivalente à struct `holderChannels` em go
pub struct HolderChannels {
    libp2p: Arc<dyn LibP2PInterface>,
    logger: Logger,
    own_peer_id: PeerId,
}

impl HolderChannels {
    pub fn new(logger: Logger, libp2p: Arc<dyn LibP2PInterface>, own_peer_id: PeerId) -> Self {
        Self {
            libp2p,
            logger,
            own_peer_id,
        }
    }

    // equivalente a `NewChannel` em go
    pub async fn new_channel(
        &self,
        emitter: Box<dyn DirectChannelEmitter<Error = GuardianError>>,
        opts: Option<DirectChannelOptions>,
    ) -> Result<Box<dyn crate::iface::DirectChannel<Error = GuardianError>>> {
        let resolved_opts = opts.unwrap_or_default();
        let logger = resolved_opts.logger.unwrap_or_else(|| self.logger.clone());

        let dc = DirectChannel::new(
            logger.clone(),
            self.libp2p.clone(),
            Arc::from(emitter),
            self.own_peer_id,
        );

        // Inicia o processamento
        dc.start().await?;

        slog::info!(logger, "DirectChannel criado com protocolo: {}", PROTOCOL);

        Ok(Box::new(dc))
    }
}

// equivalente a `InitDirectChannelFactory` em go
pub fn init_direct_channel_factory(logger: Logger, own_peer_id: PeerId) -> DirectChannelFactory {
    Box::new(
        move |emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
              opts: Option<DirectChannelOptions>| {
            let logger = logger.clone();
            let own_peer_id = own_peer_id;
            Box::pin(async move {
                slog::info!(
                    logger,
                    "Inicializando DirectChannel factory para peer: {}",
                    own_peer_id
                );

                // Cria uma interface para libp2p usando Gossipsub
                let libp2p_interface = Arc::new(GossipsubInterface::new(logger.clone()));

                // Cria o holder para gerenciar o DirectChannel
                let holder = HolderChannels::new(logger.clone(), libp2p_interface, own_peer_id);

                // Converte Arc para Box para compatibilidade
                let emitter_box = Box::new(EmitterWrapper::new(emitter));

                // Cria o canal direto
                let channel = holder
                    .new_channel(emitter_box, opts)
                    .await
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

                Ok(Arc::from(channel)
                    as Arc<
                        dyn crate::iface::DirectChannel<Error = GuardianError>,
                    >)
            })
        },
    )
}

// Wrapper para converter Arc<dyn DirectChannelEmitter> para Box<dyn DirectChannelEmitter>
struct EmitterWrapper {
    inner: Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
}

impl EmitterWrapper {
    fn new(inner: Arc<dyn DirectChannelEmitter<Error = GuardianError>>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl DirectChannelEmitter for EmitterWrapper {
    type Error = GuardianError;

    async fn emit(&self, payload: EventPubSubPayload) -> std::result::Result<(), Self::Error> {
        self.inner.emit(payload).await
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        self.inner.close().await
    }
}

// Função auxiliar para criar um DirectChannel com interface libp2p customizada
pub async fn create_direct_channel_with_libp2p(
    libp2p: Arc<dyn LibP2PInterface>,
    emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
    logger: Logger,
    own_peer_id: PeerId,
) -> Result<DirectChannel> {
    let channel = DirectChannel::new(logger.clone(), libp2p, emitter, own_peer_id);

    // Inicia o processamento
    channel.start().await?;

    slog::info!(
        logger,
        "DirectChannel criado com interface libp2p customizada"
    );
    Ok(channel)
}

// Função para criar um PeerId de teste
pub fn create_test_peer_id() -> PeerId {
    let keypair = libp2p::identity::Keypair::generate_ed25519();
    PeerId::from(keypair.public())
}
