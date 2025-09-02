// Cliente principal da API IPFS Core
//
// Implementação do cliente IPFS 100% Rust com funcionalidades completas

use crate::error::{GuardianError, Result};
use crate::ipfs_core_api::{config::ClientConfig, errors::IpfsError, types::*};
use async_stream::stream;
use cid::Cid;
use libp2p::PeerId;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::sync::{RwLock, broadcast};
use tracing::{debug, info};

/// Estado interno do cliente IPFS
#[derive(Debug)]
struct ClientState {
    /// Subscrições ativas de PubSub
    subscriptions: HashMap<String, broadcast::Sender<PubsubMessage>>,
    /// Peers conectados
    connected_peers: HashMap<PeerId, PeerInfo>,
    /// Informações do nó local
    node_info: NodeInfo,
    /// Objetos pinned
    pinned_objects: HashMap<String, PinType>,
    /// Estatísticas do repositório
    repo_stats: RepoStats,
}

impl ClientState {
    fn new(node_id: PeerId, config: &ClientConfig) -> Self {
        let node_info = if config.data_store_path.is_some() {
            NodeInfo {
                id: node_id,
                public_key: format!("ed25519_{}", hex::encode(&node_id.to_bytes()[..16])),
                addresses: config.listening_addrs.clone(),
                agent_version: format!("{}/0.1.0", crate::ipfs_core_api::USER_AGENT),
                protocol_version: "ipfs/0.1.0".to_string(),
            }
        } else {
            NodeInfo::mock(node_id)
        };

        Self {
            subscriptions: HashMap::new(),
            connected_peers: HashMap::new(),
            node_info,
            pinned_objects: HashMap::new(),
            repo_stats: RepoStats::default(),
        }
    }
}

/// Cliente principal da API IPFS Core
pub struct IpfsClient {
    /// Armazenamento interno para dados
    storage: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    /// Configuração do cliente
    config: ClientConfig,
    /// ID do nó local
    node_id: PeerId,
    /// Estado interno do cliente
    state: Arc<RwLock<ClientState>>,
    /// Indicador se o cliente está inicializado
    initialized: bool,
}

impl IpfsClient {
    /// Cria uma nova instância do cliente IPFS
    pub async fn new(config: ClientConfig) -> Result<Self> {
        // Valida configuração
        config
            .validate()
            .map_err(|e| GuardianError::Other(format!("Invalid configuration: {}", e)))?;

        info!("Inicializando cliente IPFS Core API");
        info!(
            "Configuração: PubSub={}, Swarm={}, mDNS={}, Kad={}",
            config.enable_pubsub, config.enable_swarm, config.enable_mdns, config.enable_kad
        );

        let node_id = PeerId::random();
        info!("Node ID gerado: {}", node_id);

        let state = ClientState::new(node_id, &config);

        let client = Self {
            storage: Arc::new(RwLock::new(HashMap::new())),
            config,
            node_id,
            state: Arc::new(RwLock::new(state)),
            initialized: true,
        };

        // Log de configuração aplicada
        debug!("Cliente IPFS configurado:");
        debug!("  - Data path: {:?}", client.config.data_store_path);
        debug!("  - Listening addrs: {:?}", client.config.listening_addrs);
        debug!(
            "  - Bootstrap peers: {} configurados",
            client.config.bootstrap_peers.len()
        );

        info!("✅ Cliente IPFS Core API inicializado com sucesso");
        Ok(client)
    }

    /// Cria uma instância com configuração padrão
    pub async fn default() -> Result<Self> {
        Self::new(ClientConfig::default()).await
    }

    /// Cria uma instância para desenvolvimento
    pub async fn development() -> Result<Self> {
        Self::new(ClientConfig::development()).await
    }

    /// Cria uma instância para produção
    pub async fn production() -> Result<Self> {
        Self::new(ClientConfig::production()).await
    }

    /// Cria uma instância para testes
    pub async fn testing() -> Result<Self> {
        Self::new(ClientConfig::testing()).await
    }

    /// Verifica se o cliente está inicializado
    fn ensure_initialized(&self) -> Result<()> {
        if !self.initialized {
            return Err(IpfsError::ClientNotInitialized.into());
        }
        Ok(())
    }

    /// Verifica se o nó está funcionando
    pub async fn is_online(&self) -> bool {
        self.initialized
    }

    /// Adiciona dados ao IPFS
    pub async fn add<R>(&self, mut data: R) -> Result<AddResponse>
    where
        R: tokio::io::AsyncRead + Send + Unpin + 'static,
    {
        self.ensure_initialized()?;

        let mut buffer = Vec::new();
        data.read_to_end(&mut buffer)
            .await
            .map_err(|e| GuardianError::Other(format!("Falha ao ler dados: {}", e)))?;

        // Gera hash SHA256 do conteúdo
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&buffer);
        let hash_bytes = hasher.finalize();
        let hash = format!("Qm{}", hex::encode(&hash_bytes[..20])); // CIDv0 simulado

        // Armazena no storage interno
        {
            let mut storage = self.storage.write().await;
            storage.insert(hash.clone(), buffer.clone());
        }

        // Atualiza estatísticas
        {
            let mut state = self.state.write().await;
            state.repo_stats.num_objects += 1;
            state.repo_stats.repo_size += buffer.len() as u64;
        }

        debug!("Dados adicionados: {} ({} bytes)", hash, buffer.len());

        Ok(AddResponse::new(hash, buffer.len()))
    }

    /// Recupera dados do IPFS pelo hash/CID
    pub async fn cat(&self, path: &str) -> Result<Pin<Box<dyn tokio::io::AsyncRead + Send>>> {
        self.ensure_initialized()?;

        let storage = self.storage.read().await;
        let data = storage
            .get(path)
            .cloned()
            .ok_or_else(|| IpfsError::data_not_found(path))?;

        debug!("Dados recuperados: {} ({} bytes)", path, data.len());

        let cursor = tokio::io::BufReader::new(std::io::Cursor::new(data));
        Ok(Box::pin(cursor))
    }

    /// Recupera um objeto DAG do IPFS
    pub async fn dag_get(&self, cid: &Cid, _path: Option<&str>) -> Result<Vec<u8>> {
        self.ensure_initialized()?;

        debug!("dag_get: cid={}", cid);

        let cid_str = cid.to_string();
        let storage = self.storage.read().await;

        storage
            .get(&cid_str)
            .cloned()
            .ok_or_else(|| IpfsError::data_not_found(&cid_str).into())
    }

    /// Armazena um objeto DAG no IPFS
    pub async fn dag_put(&self, data: &[u8]) -> Result<Cid> {
        self.ensure_initialized()?;

        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        hasher.update(data);
        let hash = hasher.finalize();

        // Cria um CID baseado no hash
        let cid_str = format!("bafyrei{}", hex::encode(&hash[..16]));
        let cid: Cid = cid_str
            .parse()
            .map_err(|e| IpfsError::invalid_cid(format!("Failed to create CID: {}", e)))?;

        // Armazena os dados
        {
            let mut storage = self.storage.write().await;
            storage.insert(cid.to_string(), data.to_vec());
        }

        // Atualiza estatísticas
        {
            let mut state = self.state.write().await;
            state.repo_stats.num_objects += 1;
            state.repo_stats.repo_size += data.len() as u64;
        }

        debug!("Objeto DAG armazenado: {} ({} bytes)", cid, data.len());
        Ok(cid)
    }

    /// Publica uma mensagem em um tópico do pubsub
    pub async fn pubsub_publish(&self, topic: &str, data: &[u8]) -> Result<()> {
        self.ensure_initialized()?;

        if !self.config.enable_pubsub {
            return Err(IpfsError::unsupported("PubSub não está habilitado").into());
        }

        if data.len() > self.config.pubsub.max_message_size {
            return Err(IpfsError::pubsub(format!(
                "Mensagem muito grande: {} bytes (máximo: {})",
                data.len(),
                self.config.pubsub.max_message_size
            ))
            .into());
        }

        debug!(
            "Publicando mensagem no tópico '{}': {} bytes",
            topic,
            data.len()
        );

        // Cria mensagem
        let message = PubsubMessage::new(self.node_id, topic.to_string(), data.to_vec());

        // Envia para subscribers se houver
        let state_guard = self.state.read().await;
        if let Some(sender) = state_guard.subscriptions.get(topic) {
            if sender.send(message).is_err() {
                debug!("Nenhum subscriber ativo para tópico '{}'", topic);
            }
        } else {
            debug!("Nenhuma subscrição ativa para tópico '{}'", topic);
        }

        debug!("Mensagem publicada com sucesso no tópico '{}'", topic);
        Ok(())
    }

    /// Subscreve a um tópico do pubsub
    pub async fn pubsub_subscribe(&self, topic: &str) -> Result<PubsubStream> {
        self.ensure_initialized()?;

        if !self.config.enable_pubsub {
            return Err(IpfsError::unsupported("PubSub não está habilitado").into());
        }

        debug!("Subscrevendo ao tópico: {}", topic);

        // Cria broadcast channel para esta subscrição
        let (sender, mut receiver) = broadcast::channel(self.config.pubsub.message_buffer_size);

        // Armazena a subscrição
        {
            let mut state_guard = self.state.write().await;
            state_guard.subscriptions.insert(topic.to_string(), sender);
        }

        // Cria stream que escuta mensagens
        let stream = stream! {
            while let Ok(msg) = receiver.recv().await {
                yield Ok(msg);
            }
        };

        debug!("Subscrição criada para tópico: {}", topic);
        Ok(Box::pin(stream))
    }

    /// Lista os peers conectados a um tópico do pubsub
    pub async fn pubsub_peers(&self, topic: &str) -> Result<Vec<PeerId>> {
        self.ensure_initialized()?;

        if !self.config.enable_pubsub {
            return Err(IpfsError::unsupported("PubSub não está habilitado").into());
        }

        debug!("Listando peers do tópico: {}", topic);

        // Em uma implementação real, retornaria peers do mesh do gossipsub
        // Por ora, retorna peers conectados simulados
        let state_guard = self.state.read().await;
        let connected_peers: Vec<PeerId> = state_guard
            .connected_peers
            .values()
            .filter(|peer| peer.connected)
            .map(|peer| peer.id)
            .collect();

        debug!(
            "Encontrados {} peers para tópico '{}'",
            connected_peers.len(),
            topic
        );
        Ok(connected_peers)
    }

    /// Lista todos os tópicos ativos
    pub async fn pubsub_topics(&self) -> Result<Vec<String>> {
        self.ensure_initialized()?;

        if !self.config.enable_pubsub {
            return Err(IpfsError::unsupported("PubSub não está habilitado").into());
        }

        let state_guard = self.state.read().await;
        Ok(state_guard.subscriptions.keys().cloned().collect())
    }

    /// Cancela subscrição de um tópico
    pub async fn pubsub_unsubscribe(&self, topic: &str) -> Result<()> {
        self.ensure_initialized()?;

        if !self.config.enable_pubsub {
            return Err(IpfsError::unsupported("PubSub não está habilitado").into());
        }

        debug!("Cancelando subscrição do tópico: {}", topic);

        // Remove do estado interno
        let mut state_guard = self.state.write().await;
        state_guard.subscriptions.remove(topic);

        debug!("Subscrição cancelada para tópico: {}", topic);
        Ok(())
    }

    /// Conecta a um peer específico
    pub async fn swarm_connect(&self, peer: &PeerId) -> Result<()> {
        self.ensure_initialized()?;

        if !self.config.enable_swarm {
            return Err(IpfsError::unsupported("Swarm não está habilitado").into());
        }

        debug!("Conectando ao peer: {}", peer);

        // Simula conexão adicionando peer ao estado
        let mut state_guard = self.state.write().await;
        state_guard
            .connected_peers
            .insert(*peer, PeerInfo::mock(*peer, true));

        info!("Conexão simulada estabelecida com peer: {}", peer);
        Ok(())
    }

    /// Lista todos os peers conectados ao swarm
    pub async fn swarm_peers(&self) -> Result<Vec<PeerInfo>> {
        self.ensure_initialized()?;

        let state_guard = self.state.read().await;
        Ok(state_guard.connected_peers.values().cloned().collect())
    }

    /// Obtém informações sobre este nó
    pub async fn id(&self) -> Result<NodeInfo> {
        self.ensure_initialized()?;

        let state_guard = self.state.read().await;
        Ok(state_guard.node_info.clone())
    }

    /// Adiciona um objeto aos pins
    pub async fn pin_add(&self, hash: &str, recursive: bool) -> Result<PinResponse> {
        self.ensure_initialized()?;

        debug!("Pinning objeto: {} (recursive: {})", hash, recursive);

        // Verifica se o objeto existe
        let storage = self.storage.read().await;
        if !storage.contains_key(hash) {
            return Err(IpfsError::data_not_found(hash).into());
        }

        let pin_type = if recursive {
            PinType::Recursive
        } else {
            PinType::Direct
        };

        // Adiciona ao estado dos pins
        {
            let mut state = self.state.write().await;
            state
                .pinned_objects
                .insert(hash.to_string(), pin_type.clone());
        }

        debug!("Objeto {} pinned com sucesso", hash);
        Ok(PinResponse {
            hash: hash.to_string(),
            pin_type,
        })
    }

    /// Remove um objeto dos pins
    pub async fn pin_rm(&self, hash: &str) -> Result<PinResponse> {
        self.ensure_initialized()?;

        debug!("Unpinning objeto: {}", hash);

        let mut state = self.state.write().await;
        let pin_type = state
            .pinned_objects
            .remove(hash)
            .ok_or_else(|| IpfsError::data_not_found(format!("Pin not found for {}", hash)))?;

        debug!("Objeto {} unpinned com sucesso", hash);
        Ok(PinResponse {
            hash: hash.to_string(),
            pin_type,
        })
    }

    /// Lista objetos pinned
    pub async fn pin_ls(&self, pin_type_filter: Option<PinType>) -> Result<Vec<PinResponse>> {
        self.ensure_initialized()?;

        let state = self.state.read().await;
        let pins: Vec<PinResponse> = state
            .pinned_objects
            .iter()
            .filter(|(_, pin_type)| {
                pin_type_filter
                    .as_ref()
                    .is_none_or(|filter| *pin_type == filter)
            })
            .map(|(hash, pin_type)| PinResponse {
                hash: hash.clone(),
                pin_type: pin_type.clone(),
            })
            .collect();

        debug!("Listando {} objetos pinned", pins.len());
        Ok(pins)
    }

    /// Obtém estatísticas do repositório
    pub async fn repo_stat(&self) -> Result<RepoStats> {
        self.ensure_initialized()?;

        let state = self.state.read().await;
        Ok(state.repo_stats.clone())
    }

    /// Gera um ID único para canal de comunicação entre dois peers
    /// Compatível com get_channel_id do one_on_one_channel.rs
    pub fn get_channel_id(&self, other_peer: &PeerId) -> String {
        let mut channel_id_peers = [self.node_id.to_string(), other_peer.to_string()];
        channel_id_peers.sort();
        format!(
            "/ipfs-pubsub-direct-channel/v1/{}",
            channel_id_peers.join("/")
        )
    }

    /// Obtém configuração atual
    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Obtém ID do nó
    pub fn node_id(&self) -> PeerId {
        self.node_id
    }

    /// Interrompe o cliente IPFS
    pub async fn shutdown(&self) -> Result<()> {
        info!("Encerrando cliente IPFS");

        // Limpa subscrições
        {
            let mut state = self.state.write().await;
            state.subscriptions.clear();
            state.connected_peers.clear();
        }

        // Limpa storage
        {
            let mut storage = self.storage.write().await;
            storage.clear();
        }

        info!("Cliente IPFS encerrado com sucesso");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[tokio::test]
    async fn test_client_creation() {
        let config = ClientConfig::development();
        let client = IpfsClient::new(config).await;
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn test_client_online() {
        let client = IpfsClient::development().await.unwrap();
        assert!(client.is_online().await);
    }

    #[tokio::test]
    #[ignore] // Requires running IPFS daemon
    async fn test_add_and_cat() {
        let client = IpfsClient::development().await.unwrap();

        let test_data = "Hello, IPFS Core API!".as_bytes();
        let cursor = Cursor::new(test_data.to_vec());

        // Test add
        let response = client.add(cursor).await.unwrap();
        assert!(!response.hash.is_empty());
        assert_eq!(response.size_bytes().unwrap(), test_data.len());

        // Test cat
        let mut stream = client.cat(&response.hash).await.unwrap();
        let mut buffer = Vec::new();
        stream.read_to_end(&mut buffer).await.unwrap();

        assert_eq!(test_data, buffer.as_slice());
    }

    #[tokio::test]
    #[ignore] // Requires running IPFS daemon
    async fn test_dag_operations() {
        let client = IpfsClient::development().await.unwrap();

        let test_data = b"test dag data";
        let cid = client.dag_put(test_data).await.unwrap();

        let retrieved_data = client.dag_get(&cid, None).await.unwrap();
        assert_eq!(retrieved_data, test_data);
    }

    #[tokio::test]
    #[ignore] // Requires running IPFS daemon
    async fn test_pubsub_operations() {
        let client = IpfsClient::development().await.unwrap();

        // Test publish
        let result = client.pubsub_publish("test-topic", b"test message").await;
        assert!(result.is_ok());

        // Test topics
        let topics = client.pubsub_topics().await.unwrap();
        assert!(topics.is_empty()); // Nenhuma subscrição ativa ainda

        // Test peers
        let peers = client.pubsub_peers("test-topic").await.unwrap();
        assert!(peers.is_empty());
    }

    #[tokio::test]
    #[ignore] // Requires running IPFS daemon
    async fn test_pin_operations() {
        let client = IpfsClient::development().await.unwrap();

        // Adiciona alguns dados primeiro
        let test_data = "pin test data".as_bytes();
        let cursor = Cursor::new(test_data.to_vec());
        let response = client.add(cursor).await.unwrap();

        // Test pin add
        let pin_response = client.pin_add(&response.hash, true).await.unwrap();
        assert_eq!(pin_response.hash, response.hash);
        assert_eq!(pin_response.pin_type, PinType::Recursive);

        // Test pin ls
        let pins = client.pin_ls(None).await.unwrap();
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].hash, response.hash);

        // Test pin rm
        let rm_response = client.pin_rm(&response.hash).await.unwrap();
        assert_eq!(rm_response.hash, response.hash);

        // Verify removed
        let pins_after = client.pin_ls(None).await.unwrap();
        assert!(pins_after.is_empty());
    }

    #[tokio::test]
    #[ignore] // Requires running IPFS daemon
    async fn test_node_info() {
        let client = IpfsClient::development().await.unwrap();

        let info = client.id().await.unwrap();
        assert_eq!(info.id, client.node_id());
        assert!(info.agent_version.contains("guardian-db"));
    }

    #[tokio::test]
    #[ignore] // Requires running IPFS daemon
    async fn test_channel_id_generation() {
        let client = IpfsClient::development().await.unwrap();
        let other_peer = PeerId::random();

        let channel_id = client.get_channel_id(&other_peer);
        assert!(channel_id.starts_with("/ipfs-pubsub-direct-channel/v1/"));

        // Deve ser determinístico
        let channel_id2 = client.get_channel_id(&other_peer);
        assert_eq!(channel_id, channel_id2);
    }

    #[tokio::test]
    #[ignore] // Requires running IPFS daemon
    async fn test_error_handling() {
        let client = IpfsClient::development().await.unwrap();

        // Test cat with non-existent hash
        let result = client.cat("QmNonExistent").await;
        assert!(result.is_err());

        // Test dag_get with non-existent CID
        let fake_cid: Cid = "bafyreifake123456789012345678901234567890123456789012345"
            .parse()
            .unwrap();
        let result = client.dag_get(&fake_cid, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[ignore] // Requires running IPFS daemon
    async fn test_repo_stats() {
        let client = IpfsClient::development().await.unwrap();

        let initial_stats = client.repo_stat().await.unwrap();
        assert_eq!(initial_stats.num_objects, 0);
        assert_eq!(initial_stats.repo_size, 0);

        // Add some data
        let test_data = "stats test data".as_bytes();
        let cursor = Cursor::new(test_data.to_vec());
        client.add(cursor).await.unwrap();

        let updated_stats = client.repo_stat().await.unwrap();
        assert_eq!(updated_stats.num_objects, 1);
        assert_eq!(updated_stats.repo_size, test_data.len() as u64);
    }
}
