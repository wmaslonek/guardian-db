/// Sistema de Sincronização de Chaves para Guardian DB
///
/// Sincronização robusta de chaves entre peers,
/// garantindo consistência criptográfica e prevenindo ataques de replay.
use crate::error::{GuardianError, Result};
use crate::ipfs_core_api::config::ClientConfig;
use crate::ipfs_log::identity_provider::Keystore;
use crate::keystore::SledKeystore;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use libp2p::{PeerId, identity::Keypair as LibP2PKeypair};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Versão do protocolo de sincronização
const SYNC_PROTOCOL_VERSION: u32 = 1;

/// Tempo máximo para aceitar mensagens (previne replay attacks)
const MAX_MESSAGE_AGE: Duration = Duration::from_secs(300); // 5 minutos

/// Número máximo de tentativas de sincronização (reservado para uso futuro)
#[allow(dead_code)]
const MAX_SYNC_RETRIES: u8 = 3;

/// Tamanho máximo da fila de sincronização
const MAX_SYNC_QUEUE_SIZE: usize = 1000;

/// Status de sincronização de uma chave
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeySyncStatus {
    /// Chave está sincronizada
    Synchronized,
    /// Chave em processo de sincronização
    Synchronizing,
    /// Sincronização pendente
    Pending,
    /// Erro na sincronização
    Failed(String),
    /// Conflito detectado (necessária resolução manual)
    Conflict(String),
}

/// Tipo de operação de sincronização
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncOperation {
    /// Criar nova chave
    Create,
    /// Atualizar chave existente
    Update,
    /// Deletar chave
    Delete,
    /// Sincronizar metadados
    MetadataSync,
}

/// Metadados de uma chave sincronizada
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyMetadata {
    /// ID único da chave
    pub key_id: String,
    /// Versão da chave (para controle de conflitos)
    pub version: u64,
    /// Timestamp da última modificação
    pub last_modified: DateTime<Utc>,
    /// PeerID que criou a chave
    pub creator: PeerId,
    /// Assinatura dos metadados
    pub signature: Vec<u8>,
    /// Algoritmo de criptografia usado
    pub crypto_algorithm: String,
    /// Hash da chave pública
    pub public_key_hash: Vec<u8>,
}

/// Mensagem de sincronização entre peers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncMessage {
    /// ID único da mensagem
    pub message_id: Uuid,
    /// Versão do protocolo
    pub protocol_version: u32,
    /// Timestamp da mensagem
    pub timestamp: SystemTime,
    /// PeerID do remetente
    pub sender: PeerId,
    /// Tipo de operação
    pub operation: SyncOperation,
    /// Metadados da chave
    pub metadata: KeyMetadata,
    /// Dados da chave (encriptados)
    pub key_data: Option<Vec<u8>>,
    /// Assinatura da mensagem completa
    pub message_signature: Vec<u8>,
}

/// Entrada na fila de sincronização (reservado para uso futuro)
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct SyncQueueEntry {
    /// Mensagem a ser sincronizada
    #[allow(dead_code)]
    message: SyncMessage,
    /// Número de tentativas
    #[allow(dead_code)]
    retry_count: u8,
    /// Próxima tentativa
    #[allow(dead_code)]
    next_retry: SystemTime,
    /// Peers que devem receber a mensagem
    #[allow(dead_code)]
    target_peers: Vec<PeerId>,
}

/// Estatísticas de sincronização
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SyncStatistics {
    /// Total de mensagens sincronizadas
    pub messages_synced: u64,
    /// Mensagens pendentes na fila
    pub pending_messages: u64,
    /// Conflitos detectados
    pub conflicts_detected: u64,
    /// Conflitos resolvidos
    pub conflicts_resolved: u64,
    /// Taxa de sucesso de sincronização
    pub success_rate: f64,
    /// Latência média de sincronização (ms)
    pub avg_sync_latency_ms: f64,
    /// Peers ativos na sincronização
    pub active_peers: u32,
}

/// Sistema principal de sincronização de chaves
pub struct KeySynchronizer {
    /// Configuração do sistema (reservado para uso futuro)
    #[allow(dead_code)]
    config: ClientConfig,
    /// Keystore local
    local_keystore: Arc<SledKeystore>,
    /// Keypair principal do nó
    node_keypair: LibP2PKeypair,
    /// PeerID do nó
    peer_id: PeerId,
    /// Mapeamento de chaves sincronizadas
    synchronized_keys: Arc<RwLock<HashMap<String, KeyMetadata>>>,
    /// Status de sincronização por chave
    sync_status: Arc<RwLock<HashMap<String, KeySyncStatus>>>,
    /// Fila de sincronização
    sync_queue: Arc<Mutex<VecDeque<SyncQueueEntry>>>,
    /// Cache de mensagens recentes (previne replay)
    message_cache: Arc<RwLock<HashMap<Uuid, SystemTime>>>,
    /// Estatísticas de sincronização
    statistics: Arc<RwLock<SyncStatistics>>,
    /// Chaves de confiança (peers autorizados)
    trusted_peers: Arc<RwLock<HashMap<PeerId, VerifyingKey>>>,
}

impl KeySynchronizer {
    /// Cria nova instância do sincronizador de chaves
    pub async fn new(config: &ClientConfig) -> Result<Self> {
        let keystore_path = config
            .data_store_path
            .as_ref()
            .map(|p| p.join("keystore"))
            .unwrap_or_else(|| std::env::temp_dir().join("guardian_keystore"));

        let local_keystore = Arc::new(SledKeystore::new(Some(keystore_path))?);

        // Carregar ou gerar keypair principal
        let node_keypair = Self::load_or_generate_keypair(&local_keystore).await?;
        let peer_id = PeerId::from_public_key(&node_keypair.public());

        info!(
            "Inicializando sincronizador de chaves para PeerID: {}",
            peer_id
        );

        Ok(Self {
            config: config.clone(),
            local_keystore,
            node_keypair,
            peer_id,
            synchronized_keys: Arc::new(RwLock::new(HashMap::new())),
            sync_status: Arc::new(RwLock::new(HashMap::new())),
            sync_queue: Arc::new(Mutex::new(VecDeque::new())),
            message_cache: Arc::new(RwLock::new(HashMap::new())),
            statistics: Arc::new(RwLock::new(SyncStatistics::default())),
            trusted_peers: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Retorna o PeerID do nó
    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    /// Retorna o keypair do nó
    pub fn keypair(&self) -> &LibP2PKeypair {
        &self.node_keypair
    }

    /// Carrega ou gera keypair principal
    async fn load_or_generate_keypair(keystore: &SledKeystore) -> Result<LibP2PKeypair> {
        const MAIN_KEYPAIR_KEY: &str = "main_node_keypair";

        // Tentar carregar keypair existente
        if let Some(keypair) = keystore.get_keypair(MAIN_KEYPAIR_KEY).await? {
            debug!("Carregando keypair principal existente");
            return Ok(keypair);
        }

        // Gerar novo keypair
        let keypair = LibP2PKeypair::generate_ed25519();
        keystore.put_keypair(MAIN_KEYPAIR_KEY, &keypair).await?;

        info!("Novo keypair principal gerado e salvo");
        Ok(keypair)
    }

    /// Adiciona peer confiável para sincronização
    pub async fn add_trusted_peer(&self, peer_id: PeerId, public_key: VerifyingKey) -> Result<()> {
        let mut trusted = self.trusted_peers.write().await;
        trusted.insert(peer_id, public_key);
        info!("Peer confiável adicionado: {}", peer_id);
        Ok(())
    }

    /// Remove peer confiável
    pub async fn remove_trusted_peer(&self, peer_id: &PeerId) -> Result<bool> {
        let mut trusted = self.trusted_peers.write().await;
        let removed = trusted.remove(peer_id).is_some();
        if removed {
            info!("Peer removido da lista de confiança: {}", peer_id);
        }
        Ok(removed)
    }

    /// Sincroniza uma chave específica com peers
    pub async fn sync_key(&self, key_id: &str, operation: SyncOperation) -> Result<()> {
        debug!(
            "Iniciando sincronização da chave: {} (operação: {:?})",
            key_id, operation
        );

        // Obter metadados da chave
        let metadata = self.get_key_metadata(key_id).await?;

        // Criar mensagem de sincronização
        let message = self.create_sync_message(operation, metadata, None).await?;

        // Adicionar à fila de sincronização
        self.enqueue_sync_message(message).await?;

        // Atualizar status
        self.update_sync_status(key_id, KeySyncStatus::Synchronizing)
            .await;

        Ok(())
    }

    /// Processa mensagem de sincronização recebida
    pub async fn handle_sync_message(&self, message: SyncMessage) -> Result<()> {
        // Verificar idade da mensagem (prevenir replay attacks)
        if self.is_message_too_old(&message)? {
            warn!(
                "Mensagem de sincronização rejeitada (muito antiga): {:?}",
                message.message_id
            );
            return Err(GuardianError::Other("Mensagem muito antiga".to_string()));
        }

        // Verificar se já processamos esta mensagem
        if self.is_message_duplicate(&message).await? {
            debug!("Mensagem duplicada ignorada: {:?}", message.message_id);
            return Ok(());
        }

        // Verificar assinatura da mensagem
        self.verify_message_signature(&message).await?;

        // Processar operação
        match message.operation {
            SyncOperation::Create => self.handle_key_create(&message).await?,
            SyncOperation::Update => self.handle_key_update(&message).await?,
            SyncOperation::Delete => self.handle_key_delete(&message).await?,
            SyncOperation::MetadataSync => self.handle_metadata_sync(&message).await?,
        }

        // Adicionar à cache de mensagens processadas
        self.cache_processed_message(&message).await;

        // Atualizar estatísticas
        self.update_statistics().await;

        Ok(())
    }

    /// Obtém metadados de uma chave
    async fn get_key_metadata(&self, key_id: &str) -> Result<KeyMetadata> {
        let synchronized_keys = self.synchronized_keys.read().await;

        if let Some(metadata) = synchronized_keys.get(key_id) {
            return Ok(metadata.clone());
        }

        // Chave não encontrada, criar metadados iniciais
        let keypair = self
            .local_keystore
            .get_keypair(key_id)
            .await?
            .ok_or_else(|| GuardianError::Other(format!("Chave não encontrada: {}", key_id)))?;

        let public_key_hash = blake3::hash(&keypair.public().encode_protobuf())
            .as_bytes()
            .to_vec();

        let metadata = KeyMetadata {
            key_id: key_id.to_string(),
            version: 1,
            last_modified: Utc::now(),
            creator: self.peer_id,
            signature: Vec::new(), // Será preenchida depois
            crypto_algorithm: "Ed25519".to_string(),
            public_key_hash,
        };

        Ok(metadata)
    }

    /// Cria mensagem de sincronização
    async fn create_sync_message(
        &self,
        operation: SyncOperation,
        metadata: KeyMetadata,
        key_data: Option<Vec<u8>>,
    ) -> Result<SyncMessage> {
        let message = SyncMessage {
            message_id: Uuid::new_v4(),
            protocol_version: SYNC_PROTOCOL_VERSION,
            timestamp: SystemTime::now(),
            sender: self.peer_id,
            operation,
            metadata,
            key_data,
            message_signature: Vec::new(), // Será preenchida depois
        };

        // Assinar mensagem
        let signed_message = self.sign_sync_message(message).await?;

        Ok(signed_message)
    }

    /// Assina mensagem de sincronização
    async fn sign_sync_message(&self, mut message: SyncMessage) -> Result<SyncMessage> {
        // Serializar mensagem sem assinatura
        let mut message_copy = message.clone();
        message_copy.message_signature.clear();

        let message_bytes =
            bincode::serde::encode_to_vec(&message_copy, bincode::config::standard())
                .map_err(|e| GuardianError::Other(format!("Erro ao serializar mensagem: {}", e)))?;

        // Assinar com keypair do nó
        let signature = if let Ok(ed25519_keypair) = self.node_keypair.clone().try_into_ed25519() {
            let secret_bytes = ed25519_keypair.secret().as_ref().to_vec();
            let signing_key = SigningKey::try_from(&secret_bytes[..32]).map_err(|e| {
                GuardianError::Other(format!("Erro ao criar chave de assinatura: {}", e))
            })?;
            signing_key.sign(&message_bytes).to_bytes().to_vec()
        } else {
            return Err(GuardianError::Other(
                "Tipo de chave não suportado para assinatura".to_string(),
            ));
        };

        message.message_signature = signature;
        Ok(message)
    }

    /// Verifica assinatura de mensagem
    async fn verify_message_signature(&self, message: &SyncMessage) -> Result<()> {
        // Obter chave pública do remetente
        let trusted_peers = self.trusted_peers.read().await;
        let verifying_key = trusted_peers.get(&message.sender).ok_or_else(|| {
            GuardianError::Other(format!("Peer não confiável: {}", message.sender))
        })?;

        // Reconstruir mensagem sem assinatura
        let mut message_copy = message.clone();
        message_copy.message_signature.clear();

        let message_bytes =
            bincode::serde::encode_to_vec(&message_copy, bincode::config::standard())
                .map_err(|e| GuardianError::Other(format!("Erro ao serializar mensagem: {}", e)))?;

        // Verificar assinatura
        let signature = Signature::from_slice(&message.message_signature)
            .map_err(|e| GuardianError::Other(format!("Assinatura inválida: {}", e)))?;

        verifying_key
            .verify(&message_bytes, &signature)
            .map_err(|e| {
                GuardianError::Other(format!("Verificação de assinatura falhou: {}", e))
            })?;

        Ok(())
    }

    /// Verifica se mensagem é muito antiga
    fn is_message_too_old(&self, message: &SyncMessage) -> Result<bool> {
        let now = SystemTime::now();
        let age = now
            .duration_since(message.timestamp)
            .map_err(|_| GuardianError::Other("Timestamp inválido".to_string()))?;

        Ok(age > MAX_MESSAGE_AGE)
    }

    /// Verifica se mensagem é duplicata
    async fn is_message_duplicate(&self, message: &SyncMessage) -> Result<bool> {
        let cache = self.message_cache.read().await;
        Ok(cache.contains_key(&message.message_id))
    }

    /// Adiciona mensagem à fila de sincronização
    async fn enqueue_sync_message(&self, message: SyncMessage) -> Result<()> {
        let mut queue = self.sync_queue.lock().await;

        // Verificar se a fila não está cheia
        if queue.len() >= MAX_SYNC_QUEUE_SIZE {
            // Remove mensagem mais antiga
            queue.pop_front();
            warn!("Fila de sincronização cheia, removendo mensagem mais antiga");
        }

        let entry = SyncQueueEntry {
            message,
            retry_count: 0,
            next_retry: SystemTime::now(),
            target_peers: Vec::new(), // Será preenchido baseado em peers conectados
        };

        queue.push_back(entry);
        debug!("Mensagem adicionada à fila de sincronização");

        Ok(())
    }

    /// Processa criação de chave
    async fn handle_key_create(&self, message: &SyncMessage) -> Result<()> {
        let key_id = &message.metadata.key_id;

        // Verificar se a chave já existe
        if self.local_keystore.has(key_id).await? {
            // Verificar versões para detectar conflitos
            let local_metadata = self.get_key_metadata(key_id).await?;
            if local_metadata.version >= message.metadata.version {
                debug!("Chave já existe com versão igual ou superior: {}", key_id);
                return Ok(());
            }
        }

        // Criar/atualizar chave
        if let Some(key_data) = &message.key_data {
            self.local_keystore.put(key_id, key_data).await?;
        }

        // Atualizar metadados
        let mut synchronized_keys = self.synchronized_keys.write().await;
        synchronized_keys.insert(key_id.clone(), message.metadata.clone());

        self.update_sync_status(key_id, KeySyncStatus::Synchronized)
            .await;

        info!("Chave criada via sincronização: {}", key_id);
        Ok(())
    }

    /// Processa atualização de chave
    async fn handle_key_update(&self, message: &SyncMessage) -> Result<()> {
        let key_id = &message.metadata.key_id;

        // Verificar se a chave existe
        if !self.local_keystore.has(key_id).await? {
            warn!("Tentativa de atualizar chave inexistente: {}", key_id);
            return Err(GuardianError::Other(format!(
                "Chave não encontrada: {}",
                key_id
            )));
        }

        // Verificar versão para detectar conflitos
        let local_metadata = self.get_key_metadata(key_id).await?;
        if local_metadata.version > message.metadata.version {
            warn!("Conflito de versão detectado para chave: {}", key_id);
            self.update_sync_status(
                key_id,
                KeySyncStatus::Conflict(format!(
                    "Local: v{}, Remoto: v{}",
                    local_metadata.version, message.metadata.version
                )),
            )
            .await;
            return Ok(());
        }

        // Atualizar chave
        if let Some(key_data) = &message.key_data {
            self.local_keystore.put(key_id, key_data).await?;
        }

        // Atualizar metadados
        let mut synchronized_keys = self.synchronized_keys.write().await;
        synchronized_keys.insert(key_id.clone(), message.metadata.clone());

        self.update_sync_status(key_id, KeySyncStatus::Synchronized)
            .await;

        info!("Chave atualizada via sincronização: {}", key_id);
        Ok(())
    }

    /// Processa exclusão de chave
    async fn handle_key_delete(&self, message: &SyncMessage) -> Result<()> {
        let key_id = &message.metadata.key_id;

        // Deletar chave local
        self.local_keystore.delete(key_id).await?;

        // Remover metadados
        let mut synchronized_keys = self.synchronized_keys.write().await;
        synchronized_keys.remove(key_id);

        let mut sync_status = self.sync_status.write().await;
        sync_status.remove(key_id);

        info!("Chave deletada via sincronização: {}", key_id);
        Ok(())
    }

    /// Processa sincronização de metadados
    async fn handle_metadata_sync(&self, message: &SyncMessage) -> Result<()> {
        let key_id = &message.metadata.key_id;

        // Atualizar apenas metadados (não os dados da chave)
        let mut synchronized_keys = self.synchronized_keys.write().await;
        synchronized_keys.insert(key_id.clone(), message.metadata.clone());

        debug!("Metadados sincronizados para chave: {}", key_id);
        Ok(())
    }

    /// Adiciona mensagem processada à cache
    async fn cache_processed_message(&self, message: &SyncMessage) {
        let mut cache = self.message_cache.write().await;
        cache.insert(message.message_id, SystemTime::now());

        // Limpar mensagens antigas da cache
        let cutoff = SystemTime::now() - MAX_MESSAGE_AGE;
        cache.retain(|_, timestamp| *timestamp > cutoff);
    }

    /// Atualiza status de sincronização de uma chave
    async fn update_sync_status(&self, key_id: &str, status: KeySyncStatus) {
        let mut sync_status = self.sync_status.write().await;
        sync_status.insert(key_id.to_string(), status);
    }

    /// Atualiza estatísticas de sincronização
    async fn update_statistics(&self) {
        let mut stats = self.statistics.write().await;
        stats.messages_synced += 1;

        let queue = self.sync_queue.lock().await;
        stats.pending_messages = queue.len() as u64;

        let trusted_peers = self.trusted_peers.read().await;
        stats.active_peers = trusted_peers.len() as u32;

        // Calcular taxa de sucesso
        let sync_status = self.sync_status.read().await;
        let total_keys = sync_status.len() as u64;
        let synchronized_keys = sync_status
            .values()
            .filter(|status| matches!(status, KeySyncStatus::Synchronized))
            .count() as u64;

        stats.success_rate = if total_keys > 0 {
            (synchronized_keys as f64 / total_keys as f64) * 100.0
        } else {
            100.0
        };
    }

    /// Obtém estatísticas de sincronização
    pub async fn get_statistics(&self) -> SyncStatistics {
        self.statistics.read().await.clone()
    }

    /// Obtém status de sincronização de uma chave
    pub async fn get_key_sync_status(&self, key_id: &str) -> Option<KeySyncStatus> {
        let sync_status = self.sync_status.read().await;
        sync_status.get(key_id).cloned()
    }

    /// Lista todas as chaves sincronizadas
    pub async fn list_synchronized_keys(&self) -> Vec<String> {
        let synchronized_keys = self.synchronized_keys.read().await;
        synchronized_keys.keys().cloned().collect()
    }

    /// Força sincronização completa com peers
    pub async fn force_full_sync(&self) -> Result<()> {
        info!("Iniciando sincronização completa forçada");

        let keys = self.local_keystore.list_keys().await?;
        for key_id in keys {
            self.sync_key(&key_id, SyncOperation::MetadataSync).await?;
        }

        info!(
            "Sincronização completa forçada iniciada para {} chaves",
            self.synchronized_keys.read().await.len()
        );
        Ok(())
    }

    /// Exporta configuração de sincronização
    pub async fn export_sync_config(&self) -> Result<Vec<u8>> {
        let config = SyncExportConfig {
            peer_id: self.peer_id,
            trusted_peers: self.trusted_peers.read().await.clone(),
            synchronized_keys: self.synchronized_keys.read().await.clone(),
            statistics: self.statistics.read().await.clone(),
        };

        bincode::serde::encode_to_vec(&config, bincode::config::standard())
            .map_err(|e| GuardianError::Other(format!("Erro ao exportar configuração: {}", e)))
    }
}

/// Configuração para exportação
#[derive(Debug, Serialize, Deserialize)]
struct SyncExportConfig {
    peer_id: PeerId,
    trusted_peers: HashMap<PeerId, VerifyingKey>,
    synchronized_keys: HashMap<String, KeyMetadata>,
    statistics: SyncStatistics,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempdir::TempDir;

    #[tokio::test]
    async fn test_key_synchronizer_creation() {
        let temp_dir = TempDir::new("test_sync").unwrap();
        let config = ClientConfig {
            data_store_path: Some(temp_dir.path().to_path_buf()),
            ..Default::default()
        };

        let synchronizer = KeySynchronizer::new(&config).await.unwrap();
        assert!(!synchronizer.peer_id().to_string().is_empty());
    }

    #[tokio::test]
    async fn test_sync_message_creation_and_verification() {
        let temp_dir = TempDir::new("test_sync").unwrap();
        let config = ClientConfig {
            data_store_path: Some(temp_dir.path().to_path_buf()),
            ..Default::default()
        };

        let synchronizer = KeySynchronizer::new(&config).await.unwrap();

        // Criar metadados de teste
        let metadata = KeyMetadata {
            key_id: "test_key".to_string(),
            version: 1,
            last_modified: Utc::now(),
            creator: synchronizer.peer_id(),
            signature: Vec::new(),
            crypto_algorithm: "Ed25519".to_string(),
            public_key_hash: vec![1, 2, 3, 4],
        };

        // Criar mensagem
        let message = synchronizer
            .create_sync_message(SyncOperation::Create, metadata, Some(b"test_data".to_vec()))
            .await
            .unwrap();

        // Verificar estrutura da mensagem
        assert_eq!(message.protocol_version, SYNC_PROTOCOL_VERSION);
        assert_eq!(message.sender, synchronizer.peer_id());
        assert_eq!(message.operation, SyncOperation::Create);
        assert!(!message.message_signature.is_empty());
    }
}
