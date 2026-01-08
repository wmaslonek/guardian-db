// Cliente de alto nível da API Guardian DB
//
// Este módulo fornece uma interface simplificada para uso do Guardian DB,
// focando em:
// - Factory methods para diferentes ambientes
// - Helper methods convenientes (add_bytes, get_channel_id)
// - Integração de subsistemas (docs, blobs, document stores)
// - PubSub local (apenas no processo, não distribuído via rede)
//
// Para acesso direto às funcionalidades otimizadas do backend (cache, connection pool,
// performance monitor, etc.), use IrohBackend diretamente via backend().

use crate::guardian::error::{GuardianError, Result};
use crate::p2p::network::{config::ClientConfig, types::*};
use iroh::{NodeId, SecretKey};
use rand_core::OsRng;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Cliente de alto nível da API Guardian DB
///
/// Fornece:
/// - Factory methods convenientes para diferentes ambientes
/// - Helpers úteis (add_bytes, get_channel_id)
/// - Integração de subsistemas (docs, blobs, document stores)
///
/// Para operações de baixo nível e acesso a otimizações (cache, connection pool,
/// performance monitor, key synchronizer), use `backend()` para obter o IrohBackend.
/// Para comunicação P2P, use EpidemicPubSub via backend.create_pubsub_interface().
#[derive(Clone)]
pub struct IrohClient {
    /// Backend Iroh otimizado (acesso via backend() para operações avançadas)
    backend: Arc<crate::p2p::network::core::IrohBackend>,
    /// Configuração do cliente
    config: ClientConfig,
    /// ID do nó (NodeId do Iroh)
    node_id: NodeId,
    /// Chave secreta do nó (Iroh SecretKey)
    secret_key: SecretKey,
    /// Cliente iroh-docs para KV stores distribuídos (opcional)
    docs_client: Arc<RwLock<Option<crate::p2p::network::core::docs::WillowDocs>>>,
    /// Cliente iroh-blobs para armazenamento content-addressed (opcional)
    blobs_client: Arc<RwLock<Option<crate::p2p::network::core::blobs::BlobStore>>>,
}

impl IrohClient {
    /// Cria uma nova instância do cliente Guardian DB
    ///
    /// Inicializa o backend otimizado e prepara subsistemas opcionais.
    pub async fn new(config: ClientConfig) -> Result<Self> {
        // Valida configuração
        config
            .validate()
            .map_err(|e| GuardianError::Other(format!("Invalid configuration: {}", e)))?;

        info!("Inicializando cliente Guardian DB");

        // Criar backend Iroh otimizado primeiro
        let backend = Arc::new(crate::p2p::network::core::IrohBackend::new(&config).await?);

        // Obtém o NodeId do backend (ele carrega ou gera a chave persistente)
        let node_info = backend.id().await?;
        let node_id = node_info.id;

        // Obtém a secret_key do backend para manter consistência
        let secret_key = backend.secret_key().clone();

        info!("NodeId: {}", node_id);

        let client = Self {
            backend,
            config: config.clone(),
            node_id,
            secret_key,
            docs_client: Arc::new(RwLock::new(None)),
            blobs_client: Arc::new(RwLock::new(None)),
        };

        // Inicializa iroh-blobs automaticamente usando store compartilhado do backend
        if config.data_store_path.is_some() {
            match client.init_blobs().await {
                Ok(_) => info!("✓ iroh-blobs inicializado com store compartilhado"),
                Err(e) => {
                    warn!("Aviso: iroh-blobs não inicializado: {}", e);
                    debug!("  Use init_blobs() manualmente se precisar");
                }
            }
        }

        info!("✓ Cliente Guardian DB inicializado");
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

    /// Cria uma instância usando um backend Iroh existente
    pub async fn new_with_backend(
        backend: Arc<crate::p2p::network::core::IrohBackend>,
    ) -> Result<Self> {
        let config = ClientConfig::default();

        // Gera chave e NodeId usando Iroh
        let secret_key = SecretKey::generate(OsRng);
        let node_id = secret_key.public();

        Ok(Self {
            backend,
            config,
            node_id,
            secret_key,
            docs_client: Arc::new(RwLock::new(None)),
            blobs_client: Arc::new(RwLock::new(None)),
        })
    }

    // ==================== Acesso ao Backend ====================

    /// Obtém referência ao backend otimizado do Iroh
    ///
    /// Use para acesso direto a:
    /// - Cache inteligente: `backend.optimized_cache`
    /// - Connection pool: `backend.list_active_connections()`
    /// - Performance monitor: `backend.get_performance_metrics()`
    /// - Key synchronizer: `backend.sync_key_with_peers()`
    /// - Networking metrics: `backend.get_networking_metrics()`
    ///
    /// # Exemplo
    /// ```no_run
    /// # use guardian_db::p2p::network::IrohClient;
    /// # async fn example() -> guardian_db::error::Result<()> {
    /// let client = IrohClient::development().await?;
    ///
    /// // Acesso direto ao backend otimizado
    /// let backend = client.backend();
    /// let report = backend.generate_performance_report().await;
    /// println!("{}", report);
    /// # Ok(())
    /// # }
    /// ```
    pub fn backend(&self) -> &Arc<crate::p2p::network::core::IrohBackend> {
        &self.backend
    }

    /// Verifica se o nó está online
    pub async fn is_online(&self) -> bool {
        self.backend.is_online().await
    }

    // ==================== Helper Methods (Valor Agregado) ====================

    /// Helper: Adiciona dados de Vec<u8> ao backend
    ///
    /// Converte Vec<u8> para AsyncRead automaticamente.
    ///
    /// # Exemplo
    /// ```no_run
    /// # use guardian_db::p2p::network::IrohClient;
    /// # async fn example() -> guardian_db::error::Result<()> {
    /// let client = IrohClient::development().await?;
    /// let data = b"Hello, Guardian!".to_vec();
    /// let response = client.add_bytes(data).await?;
    /// println!("Hash: {}", response.hash);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn add_bytes(&self, data: Vec<u8>) -> Result<AddResponse> {
        struct BytesReader {
            data: Vec<u8>,
            pos: usize,
        }

        impl tokio::io::AsyncRead for BytesReader {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                let remaining = self.data.len() - self.pos;
                let to_read = std::cmp::min(remaining, buf.remaining());

                if to_read == 0 {
                    return std::task::Poll::Ready(Ok(()));
                }

                buf.put_slice(&self.data[self.pos..self.pos + to_read]);
                self.pos += to_read;

                std::task::Poll::Ready(Ok(()))
            }
        }

        let reader = BytesReader { data, pos: 0 };
        let pinned_data = Pin::new(Box::new(reader));
        self.backend.add(pinned_data).await
    }

    /// Helper: Recupera dados do backend e lê para Vec<u8>
    ///
    /// # Exemplo
    /// ```no_run
    /// # use guardian_db::p2p::network::IrohClient;
    /// # async fn example() -> guardian_db::error::Result<()> {
    /// let client = IrohClient::development().await?;
    /// let hash = "..."; // hash do conteúdo
    /// let data = client.cat_bytes(hash).await?;
    /// println!("Recuperados {} bytes", data.len());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn cat_bytes(&self, hash: &str) -> Result<Vec<u8>> {
        let mut reader = self.backend.cat(hash).await?;
        let mut data = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut data).await?;
        Ok(data)
    }

    /// Helper: Gera ID único para canal de comunicação entre peers
    ///
    /// Formato: `/iroh-pubsub-direct-channel/v1/{peer1}/{peer2}` (ordenado)
    pub fn get_channel_id(&self, other_peer: &NodeId) -> String {
        let mut channel_id_peers = [self.node_id.to_string(), other_peer.to_string()];
        channel_id_peers.sort();
        format!(
            "/iroh-pubsub-direct-channel/v1/{}",
            channel_id_peers.join("/")
        )
    }

    // ==================== Getters e Informações ====================

    /// Obtém configuração atual
    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Obtém ID do nó (Iroh NodeId)
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Obtém chave secreta do nó (Iroh SecretKey)
    pub fn secret_key(&self) -> &SecretKey {
        &self.secret_key
    }

    /// Obtém informações do nó via backend
    pub async fn id(&self) -> Result<NodeInfo> {
        self.backend.id().await
    }

    // ==================== Integração de Subsistemas ====================
    // Métodos iroh-docs

    /// Inicializa o cliente iroh-docs obtendo Docs do backend
    ///
    /// # Retorna
    /// Ok(()) se inicializado com sucesso, Err em caso de erro
    pub async fn init_docs(&self) -> Result<()> {
        // Obtém backend para passar ao WillowDocs
        let backend = self.backend.clone();

        let mut client = crate::p2p::network::core::docs::WillowDocs::new(backend).await?;

        // Inicializa o author padrão
        client.init_default_author().await?;

        // Armazena o cliente
        let mut docs_guard = self.docs_client.write().await;
        *docs_guard = Some(client);

        info!("iroh-docs client inicializado com sucesso");
        Ok(())
    }

    /// Retorna uma referência ao cliente iroh-docs se inicializado
    ///
    /// # Retorna
    /// Some(WillowDocs) se inicializado, None caso contrário
    pub async fn docs_client(&self) -> Option<crate::p2p::network::core::docs::WillowDocs> {
        let guard = self.docs_client.read().await;
        (*guard).clone()
    }

    /// Verifica se o cliente iroh-docs está inicializado
    pub async fn has_docs_client(&self) -> bool {
        let guard = self.docs_client.read().await;
        guard.is_some()
    }

    // ==================== Métodos iroh-blobs ====================

    /// Inicializa o cliente iroh-blobs usando o store compartilhado do backend
    ///
    /// O cliente iroh-blobs agora utiliza o store compartilhado do IrohBackend,
    /// garantindo consistência e evitando duplicação de armazenamento.
    ///
    /// # Retorna
    /// Ok(()) se inicializado com sucesso, Err em caso de erro
    pub async fn init_blobs(&self) -> Result<()> {
        // Obtém o store do backend
        let store = self.backend.get_store_for_blobs().await?;

        // Cria cliente com store compartilhado
        let client = crate::p2p::network::core::blobs::BlobStore::new(store);

        let mut blobs_guard = self.blobs_client.write().await;
        *blobs_guard = Some(client);

        info!("iroh-blobs client inicializado com store compartilhado");
        Ok(())
    }

    /// Retorna uma referência ao cliente iroh-blobs se inicializado
    ///
    /// # Retorna
    /// Some(BlobStore) se inicializado, None caso contrário
    pub async fn blobs_client(&self) -> Option<crate::p2p::network::core::blobs::BlobStore> {
        let guard = self.blobs_client.read().await;
        (*guard).clone()
    }

    /// Verifica se o cliente iroh-blobs está inicializado
    pub async fn has_blobs_client(&self) -> bool {
        let guard = self.blobs_client.read().await;
        guard.is_some()
    }

    // ==================== Document Store Factory ====================

    /// Cria uma nova instância de GuardianDBDocumentStore
    ///
    /// # Argumentos
    /// * `identity` - Identidade para operações na store
    /// * `addr` - Endereço da store
    /// * `options` - Opções de configuração da store
    ///
    /// # Retorna
    /// Ok(GuardianDBDocumentStore) se bem-sucedido
    ///
    /// # Exemplo
    /// ```no_run
    /// use guardian_db::p2p::network::IrohClient;
    /// use guardian_db::p2p::network::config::ClientConfig;
    /// use guardian_db::log::identity::DefaultIdentificator;
    /// use guardian_db::traits::{NewStoreOptions, CreateDocumentDBOptions, Identificator};
    /// use std::sync::Arc;
    ///
    /// # async fn example() -> guardian_db::error::Result<()> {
    /// let client = IrohClient::new(ClientConfig::development()).await?;
    ///
    /// // Cria identidade
    /// let mut identificator = DefaultIdentificator::new();
    /// let identity = Arc::new(identificator.create("user"));
    ///
    /// // Cria endereço
    /// let addr = Arc::new(guardian_db::address::GuardianDBAddress::parse("/guardiandb/zdpuAm...")?);
    ///
    /// // Configura opções
    /// let mut options = NewStoreOptions::default();
    /// let doc_opts = CreateDocumentDBOptions::new("_id".to_string());
    /// options.store_specific_opts = Some(Box::new(doc_opts));
    ///
    /// let store = client.create_document_store(
    ///     identity,
    ///     addr,
    ///     options
    /// ).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn create_document_store(
        &self,
        identity: Arc<crate::log::identity::Identity>,
        addr: Arc<dyn crate::address::Address>,
        options: crate::traits::NewStoreOptions,
    ) -> Result<crate::stores::document_store::GuardianDBDocumentStore> {
        crate::stores::document_store::GuardianDBDocumentStore::new(
            Arc::new(self.clone()),
            identity,
            addr,
            options,
        )
        .await
    }

    /// Interrompe o cliente
    pub async fn shutdown(&self) -> Result<()> {
        info!("Encerrando cliente Guardian DB");
        info!("Cliente Guardian DB encerrado com sucesso");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_client_creation() {
        let mut config = ClientConfig::development();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        config.data_store_path = Some(format!("./tmp/test_creation_{}", timestamp).into());
        let client = IrohClient::new(config).await;
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn test_client_online() {
        let mut config = ClientConfig::development();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        config.data_store_path = Some(format!("./tmp/test_online_{}", timestamp).into());
        let client = IrohClient::new(config).await.unwrap();
        assert!(client.is_online().await);
    }

    #[tokio::test]
    async fn test_blobs_client_initialization() {
        let mut config = ClientConfig::development();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let data_path = format!("./tmp/test_blobs_{}", timestamp);
        config.data_store_path = Some(data_path.into());
        let client = IrohClient::new(config).await.unwrap();

        // Verifica que blobs_client foi inicializado automaticamente
        assert!(
            client.has_blobs_client().await,
            "blobs_client deve ser inicializado automaticamente"
        );

        // Verifica que podemos obter uma referência
        let blobs = client.blobs_client().await;
        assert!(blobs.is_some(), "blobs_client() deve retornar Some");
    }

    #[tokio::test]
    async fn test_blobs_client_manual_init() {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        let mut config = ClientConfig::development();
        let base_path = format!("./tmp/test_manual_blobs_{}", timestamp);
        config.data_store_path = Some(base_path.clone().into()); // Backend precisa de data_path

        let client = IrohClient::new(config).await.unwrap();

        // Com data_store_path, blobs_client já é inicializado automaticamente
        assert!(
            client.has_blobs_client().await,
            "blobs_client deve ser inicializado automaticamente quando data_store_path está presente"
        );

        // Teste de re-inicialização (agora usa o store do backend)
        let result = client.init_blobs().await;
        assert!(result.is_ok(), "Re-inicialização deve ser permitida");
    }

    #[tokio::test]
    async fn test_add_bytes_helper() {
        let mut config = ClientConfig::development();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        config.data_store_path = Some(format!("./tmp/test_add_bytes_{}", timestamp).into());
        let client = IrohClient::new(config).await.unwrap();

        let test_data = b"Hello, Guardian!".to_vec();
        let response = client.add_bytes(test_data.clone()).await.unwrap();

        assert!(!response.hash.is_empty());
        assert_eq!(response.size_bytes().unwrap(), test_data.len());

        // Test cat_bytes helper
        let retrieved = client.cat_bytes(&response.hash).await.unwrap();
        assert_eq!(retrieved, test_data);
    }

    #[tokio::test]
    async fn test_backend_access() {
        let mut config = ClientConfig::development();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        config.data_store_path = Some(format!("./tmp/test_backend_{}", timestamp).into());
        let client = IrohClient::new(config).await.unwrap();

        // Test backend access
        let backend = client.backend();
        let report = backend.generate_performance_report().await;
        assert!(!report.is_empty());
    }

    #[tokio::test]
    async fn test_node_info() {
        let test_dir = format!("./tmp/iroh_test_info_{}", std::process::id());
        let mut config = ClientConfig::development();
        config.data_store_path = Some(test_dir.into());
        let client = IrohClient::new(config).await.unwrap();
        let info = client.id().await.unwrap();
        // O node_id deve ser consistente com o NodeId do backend
        assert_eq!(info.id, client.node_id());
    }

    #[tokio::test]
    async fn test_get_channel_id() {
        let client = IrohClient::development().await.unwrap();
        let other_peer = SecretKey::generate(OsRng).public();

        let channel_id = client.get_channel_id(&other_peer);
        assert!(channel_id.starts_with("/iroh-pubsub-direct-channel/v1/"));

        // Deve ser determinístico
        let channel_id2 = client.get_channel_id(&other_peer);
        assert_eq!(channel_id, channel_id2);
    }

    #[tokio::test]
    async fn test_error_handling() {
        let mut config = ClientConfig::development();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        config.data_store_path = Some(format!("./tmp/test_errors_{}", timestamp).into());
        let client = IrohClient::new(config).await.unwrap();

        // Test cat_bytes with non-existent hash
        let result = client.cat_bytes("invalid_hash").await;
        assert!(result.is_err());
    }
}
