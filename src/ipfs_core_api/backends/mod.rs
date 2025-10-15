/// Backend intercambiável para operações IPFS
///
/// Este módulo define a interface comum para diferentes implementações de IPFS,
/// permitindo suporte nativo ao Iroh e implementações híbridas.
use crate::error::{GuardianError, Result};
use crate::ipfs_core_api::types::*;
use async_trait::async_trait;
use cid::Cid;
use libp2p::PeerId;
use std::pin::Pin;
use tokio::io::AsyncRead;

// Módulos principais
pub mod hybrid;
pub mod iroh;
pub mod iroh_pubsub;
pub mod key_synchronizer;
pub mod networking_metrics;

// Módulos de otimização
pub mod batch_processor;
pub mod connection_pool;
pub mod optimized_cache;

pub use hybrid::HybridBackend;
pub use iroh::IrohBackend;
pub use iroh_pubsub::IrohPubSub;

/// Trait principal para backends IPFS intercambiáveis
///
/// Define a interface comum que todos os backends devem implementar,
/// garantindo compatibilidade total independentemente da implementação subjacente.
#[async_trait]
pub trait IpfsBackend: Send + Sync + 'static {
    // === OPERAÇÕES DE CONTEÚDO ===

    /// Adiciona dados ao IPFS e retorna o CID resultante
    ///
    /// # Argumentos
    /// * `data` - Stream de dados para adicionar
    ///
    /// # Retorna
    /// Resposta contendo o CID e metadados do objeto adicionado
    async fn add(&self, data: Pin<Box<dyn AsyncRead + Send>>) -> Result<AddResponse>;

    /// Recupera dados do IPFS pelo CID
    ///
    /// # Argumentos  
    /// * `cid` - Content Identifier do objeto desejado
    ///
    /// # Retorna
    /// Stream de dados do objeto
    async fn cat(&self, cid: &str) -> Result<Pin<Box<dyn AsyncRead + Send>>>;

    /// Fixa um objeto no storage local (evita garbage collection)
    ///
    /// # Argumentos
    /// * `cid` - CID do objeto a ser fixado
    async fn pin_add(&self, cid: &str) -> Result<()>;

    /// Remove fixação de um objeto (permite garbage collection)
    ///
    /// # Argumentos  
    /// * `cid` - CID do objeto a ser desfixado
    async fn pin_rm(&self, cid: &str) -> Result<()>;

    /// Lista objetos fixados
    ///
    /// # Retorna
    /// Lista de CIDs fixados com seus tipos
    async fn pin_ls(&self) -> Result<Vec<PinInfo>>;

    // === OPERAÇÕES DE REDE ===

    /// Conecta explicitamente a um peer
    ///
    /// # Argumentos
    /// * `peer` - ID do peer para conectar
    async fn connect(&self, peer: &PeerId) -> Result<()>;

    /// Lista peers atualmente conectados
    ///
    /// # Retorna
    /// Lista de informações dos peers conectados
    async fn peers(&self) -> Result<Vec<PeerInfo>>;

    /// Obtém informações do nó local
    ///
    /// # Retorna
    /// Informações do nó incluindo ID, chaves públicas, endereços
    async fn id(&self) -> Result<NodeInfo>;

    /// Resolve um peer ID para seus endereços conhecidos
    ///
    /// # Argumentos
    /// * `peer` - PeerId a ser resolvido
    ///
    /// # Retorna
    /// Lista de endereços multiaddr do peer
    async fn dht_find_peer(&self, peer: &PeerId) -> Result<Vec<String>>;

    // === OPERAÇÕES DO REPOSITÓRIO ===

    /// Obtém estatísticas do repositório local
    ///
    /// # Retorna
    /// Estatísticas incluindo tamanho, número de objetos, etc.
    async fn repo_stat(&self) -> Result<RepoStats>;

    /// Obtém versão e informações do nó IPFS
    ///
    /// # Retorna
    /// Informações de versão do software IPFS
    async fn version(&self) -> Result<VersionInfo>;

    // === OPERAÇÕES DE BLOCOS (LOWER-LEVEL) ===

    /// Obtém um bloco raw pelo seu CID
    ///
    /// # Argumentos
    /// * `cid` - CID do bloco desejado
    ///
    /// # Retorna
    /// Dados raw do bloco
    async fn block_get(&self, cid: &Cid) -> Result<Vec<u8>>;

    /// Armazena dados raw como bloco
    ///
    /// # Argumentos
    /// * `data` - Dados raw para armazenar
    ///
    /// # Retorna
    /// CID do bloco criado
    async fn block_put(&self, data: Vec<u8>) -> Result<Cid>;

    /// Verifica se um bloco existe localmente
    ///
    /// # Argumentos
    /// * `cid` - CID do bloco a verificar
    ///
    /// # Retorna
    /// true se o bloco existe localmente
    async fn block_stat(&self, cid: &Cid) -> Result<BlockStats>;

    // === METADADOS DO BACKEND ===

    /// Retorna o tipo/nome do backend
    fn backend_type(&self) -> BackendType;

    /// Verifica se o backend está online e operacional
    async fn is_online(&self) -> bool;

    /// Obtém métricas de performance do backend
    async fn metrics(&self) -> Result<BackendMetrics>;

    /// Executa health check completo do backend
    async fn health_check(&self) -> Result<HealthStatus>;
}

/// Tipo do backend IPFS
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendType {
    /// Backend usando Iroh embarcado (padrão)
    Iroh,
    /// Backend híbrido (Iroh + LibP2P)
    Hybrid,
    /// Backend de teste/mock
    Mock,
}

impl BackendType {
    pub fn as_str(&self) -> &'static str {
        match self {
            BackendType::Iroh => "iroh",
            BackendType::Hybrid => "hybrid",
            BackendType::Mock => "mock",
        }
    }
}

/// Informações de um objeto fixado
#[derive(Debug, Clone)]
pub struct PinInfo {
    pub cid: String,
    pub pin_type: PinType,
}

/// Tipo de fixação (pin)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinType {
    /// Fixação direta do objeto
    Direct,
    /// Fixação recursiva (inclui referencias)
    Recursive,
    /// Fixação indireta (referenciado por outro pin)
    Indirect,
}

/// Estatísticas de um bloco
#[derive(Debug, Clone)]
pub struct BlockStats {
    pub cid: Cid,
    pub size: u64,
    pub exists_locally: bool,
}

/// Estatísticas de garbage collection
#[derive(Debug, Clone)]
pub struct GcStats {
    pub blocks_removed: u64,
    pub bytes_freed: u64,
    pub duration_ms: u64,
}

/// Métricas de performance do backend
#[derive(Debug, Clone)]
pub struct BackendMetrics {
    /// Operações por segundo
    pub ops_per_second: f64,
    /// Latência média em ms
    pub avg_latency_ms: f64,
    /// Número de operações totais
    pub total_operations: u64,
    /// Número de erros
    pub error_count: u64,
    /// Uso de memória em bytes
    pub memory_usage_bytes: u64,
}

/// Status de saúde do backend
#[derive(Debug, Clone)]
pub struct HealthStatus {
    /// Backend está saudável
    pub healthy: bool,
    /// Mensagem descritiva
    pub message: String,
    /// Tempo de resposta em ms
    pub response_time_ms: u64,
    /// Componentes verificados
    pub checks: Vec<HealthCheck>,
}

/// Verificação individual de saúde
#[derive(Debug, Clone)]
pub struct HealthCheck {
    pub name: String,
    pub passed: bool,
    pub message: String,
}

/// Factory para criar backends baseado na configuração
pub struct BackendFactory;

impl BackendFactory {
    /// Cria um backend baseado no tipo especificado
    ///
    /// # Argumentos
    /// * `backend_type` - Tipo do backend desejado
    /// * `config` - Configuração específica do backend
    ///
    /// # Retorna
    /// Instância do backend configurado
    pub async fn create_backend(
        backend_type: BackendType,
        config: &crate::ipfs_core_api::config::ClientConfig,
    ) -> Result<Box<dyn IpfsBackend>> {
        match backend_type {
            BackendType::Iroh => {
                let backend = iroh::IrohBackend::new(config).await?;
                Ok(Box::new(backend))
            }
            BackendType::Hybrid => {
                let backend = hybrid::HybridBackend::new(config).await?;
                Ok(Box::new(backend))
            }
            BackendType::Mock => Err(GuardianError::Other(
                "Mock backend não implementado ainda".to_string(),
            )),
        }
    }

    /// Auto-detecta o melhor backend baseado na configuração disponível
    ///
    /// # Argumentos
    /// * `config` - Configuração do cliente
    ///
    /// # Retorna
    /// Backend mais apropriado para a configuração
    pub async fn auto_detect_backend(
        config: &crate::ipfs_core_api::config::ClientConfig,
    ) -> Result<Box<dyn IpfsBackend>> {
        // Prioridade: Hybrid > Iroh (nativo)

        // Tenta Hybrid primeiro (melhor desempenho + compatibilidade P2P)
        if config.data_store_path.is_some()
            && config.enable_pubsub
            && let Ok(backend) = hybrid::HybridBackend::new(config).await
        {
            return Ok(Box::new(backend));
        }

        // Fallback para Iroh puro (sempre disponível)
        if let Ok(backend) = iroh::IrohBackend::new(config).await {
            return Ok(Box::new(backend));
        }

        Err(GuardianError::Other(
            "Falha ao inicializar backend Iroh".to_string(),
        ))
    }
}
