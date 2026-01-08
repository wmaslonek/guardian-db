// Configuração do cliente Iroh
//
// Centraliza todas as opções de configuração do cliente Iroh nativo.
// Foca em iroh-blobs (armazenamento) e iroh-gossip (pubsub).
// Usa discovery via Pkarr/DNS/mDNS.

use iroh::NodeId;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Configuração completa do cliente Iroh
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    /// Habilita funcionalidades de PubSub (iroh-gossip)
    pub enable_pubsub: bool,

    /// Caminho para armazenar dados do Iroh (blobs + docs)
    pub data_store_path: Option<PathBuf>,

    /// Porta para endpoint Iroh (0 = porta aleatória)
    pub port: u16,

    /// Peers conhecidos para conectar inicialmente
    pub known_peers: Vec<NodeId>,

    /// Habilita descoberta via n0.computer (Pkarr + DNS)
    pub enable_discovery_n0: bool,

    /// Habilita descoberta via mDNS (rede local)
    pub enable_discovery_mdns: bool,

    /// Configurações de networking do Iroh
    pub network: NetworkConfig,

    /// Configurações de armazenamento (iroh-blobs)
    pub storage: StorageConfig,

    /// Configurações de Gossip (iroh-gossip)
    pub gossip: GossipConfig,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            enable_pubsub: true,
            data_store_path: Some(PathBuf::from("./iroh_data")),
            port: 0, // Porta aleatória
            known_peers: vec![],
            enable_discovery_n0: true,   // Discovery via Pkarr/DNS
            enable_discovery_mdns: true, // Discovery local
            network: NetworkConfig::default(),
            storage: StorageConfig::default(),
            gossip: GossipConfig::default(),
        }
    }
}

impl ClientConfig {
    /// Cria configuração mínima para desenvolvimento
    pub fn development() -> Self {
        Self {
            enable_pubsub: true,
            data_store_path: Some("./tmp/iroh_dev".into()),
            port: 0, // Porta aleatória
            known_peers: vec![],
            enable_discovery_n0: false,  // Desabilitado para dev local
            enable_discovery_mdns: true, // Apenas descoberta local
            network: NetworkConfig::development(),
            storage: StorageConfig::development(),
            gossip: GossipConfig::development(),
        }
    }

    /// Cria configuração para produção
    pub fn production() -> Self {
        Self {
            enable_pubsub: true,
            data_store_path: Some("/var/lib/iroh".into()),
            port: 4001,                  // Porta fixa para produção
            known_peers: vec![],         // Seria populado com peers
            enable_discovery_n0: true,   // Discovery global via n0.computer
            enable_discovery_mdns: true, // Discovery local também
            network: NetworkConfig::production(),
            storage: StorageConfig::production(),
            gossip: GossipConfig::production(),
        }
    }

    /// Configuração apenas para testes
    pub fn testing() -> Self {
        Self {
            enable_pubsub: true,
            data_store_path: None, // Em memória
            port: 0,               // Porta aleatória
            known_peers: vec![],
            enable_discovery_n0: false,
            enable_discovery_mdns: false,
            network: NetworkConfig::testing(),
            storage: StorageConfig::testing(),
            gossip: GossipConfig::testing(),
        }
    }

    /// Habilita modo offline (sem networking)
    pub fn offline() -> Self {
        Self {
            enable_pubsub: false,
            enable_discovery_n0: false,
            enable_discovery_mdns: false,
            ..Self::development()
        }
    }

    /// Adiciona um peer conhecido para conexão inicial
    pub fn add_known_peer(&mut self, peer: NodeId) {
        if !self.known_peers.contains(&peer) {
            self.known_peers.push(peer);
        }
    }

    /// Define a porta do endpoint Iroh
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Define o caminho de armazenamento
    pub fn with_data_path<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.data_store_path = Some(path.into());
        self
    }

    /// Valida a configuração
    pub fn validate(&self) -> Result<(), String> {
        // Verifica consistência da configuração Iroh

        // Valida porta
        // Porta 0 é válida (aleatória), portas < 1024 requerem privilégios
        if self.port > 0 && self.port < 1024 {
            return Err("Porta < 1024 requer privilégios de administrador".to_string());
        }

        // Valida caminho de armazenamento se fornecido
        if let Some(path) = &self.data_store_path
            && path.as_os_str().is_empty()
        {
            return Err("Caminho de armazenamento não pode estar vazio".to_string());
        }

        // Valida configurações de storage
        if self.storage.max_cache_size == 0 {
            return Err("Tamanho de cache não pode ser zero".to_string());
        }

        Ok(())
    }

    /// Verifica se usa armazenamento persistente (vs. em memória)
    pub fn uses_persistent_storage(&self) -> bool {
        self.data_store_path.is_some()
    }

    /// Verifica se algum método de discovery está habilitado
    pub fn has_discovery_enabled(&self) -> bool {
        self.enable_discovery_n0 || self.enable_discovery_mdns
    }
}

/// Configurações específicas de networking do Iroh
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Timeout para conexões do endpoint
    pub connection_timeout: Duration,

    /// Número máximo de peers simultâneos
    pub max_peers_per_session: usize,

    /// Tamanho do buffer de I/O de rede (bytes)
    pub io_buffer_size: usize,

    /// Intervalo de keep-alive para conexões
    pub keepalive_interval: Duration,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            connection_timeout: Duration::from_secs(30),
            max_peers_per_session: 100,
            io_buffer_size: 64 * 1024, // 64KB
            keepalive_interval: Duration::from_secs(60),
        }
    }
}

impl NetworkConfig {
    pub fn development() -> Self {
        Self {
            connection_timeout: Duration::from_secs(10),
            max_peers_per_session: 10,
            io_buffer_size: 16 * 1024, // 16KB
            keepalive_interval: Duration::from_secs(30),
        }
    }

    pub fn production() -> Self {
        Self {
            connection_timeout: Duration::from_secs(60),
            max_peers_per_session: 1000,
            io_buffer_size: 128 * 1024, // 128KB
            keepalive_interval: Duration::from_secs(120),
        }
    }

    pub fn testing() -> Self {
        Self {
            connection_timeout: Duration::from_secs(5),
            max_peers_per_session: 5,
            io_buffer_size: 8 * 1024, // 8KB
            keepalive_interval: Duration::from_secs(15),
        }
    }
}

/// Configurações de armazenamento (iroh-blobs) (iroh-blobs)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Habilita cache em memória para blobs
    pub enable_memory_cache: bool,

    /// Tamanho máximo do cache em memória (bytes)
    pub max_cache_size: usize,

    /// Tamanho máximo de um blob individual (bytes)
    pub max_blob_size: usize,

    /// Habilita garbage collection automático
    pub enable_gc: bool,

    /// Intervalo de garbage collection
    pub gc_interval: Duration,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            enable_memory_cache: true,
            max_cache_size: 100 * 1024 * 1024, // 100MB
            max_blob_size: 10 * 1024 * 1024,   // 10MB por blob
            enable_gc: true,
            gc_interval: Duration::from_secs(3600), // 1 hora
        }
    }
}

impl StorageConfig {
    pub fn development() -> Self {
        Self {
            enable_memory_cache: true,
            max_cache_size: 10 * 1024 * 1024, // 10MB
            max_blob_size: 5 * 1024 * 1024,   // 5MB por blob
            enable_gc: false,                 // Desabilitado para debug
            gc_interval: Duration::from_secs(3600),
        }
    }

    pub fn production() -> Self {
        Self {
            enable_memory_cache: true,
            max_cache_size: 1024 * 1024 * 1024, // 1GB
            max_blob_size: 100 * 1024 * 1024,   // 100MB por blob
            enable_gc: true,
            gc_interval: Duration::from_secs(1800), // 30 minutos
        }
    }

    pub fn testing() -> Self {
        Self {
            enable_memory_cache: false,
            max_cache_size: 1024 * 1024, // 1MB
            max_blob_size: 512 * 1024,   // 512KB por blob
            enable_gc: false,
            gc_interval: Duration::from_secs(3600),
        }
    }
}

/// Configurações de Gossip (iroh-gossip)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipConfig {
    /// Tamanho máximo de mensagem gossip (bytes)
    pub max_message_size: usize,

    /// Buffer size para streams de mensagens
    pub message_buffer_size: usize,

    /// Timeout para operações de gossip
    pub operation_timeout: Duration,

    /// Intervalo de heartbeat do protocolo gossip
    pub heartbeat_interval: Duration,

    /// Número máximo de tópicos simultâneos
    pub max_topics: usize,
}

impl Default for GossipConfig {
    fn default() -> Self {
        Self {
            max_message_size: 1024 * 1024, // 1MB
            message_buffer_size: 1000,
            operation_timeout: Duration::from_secs(30),
            heartbeat_interval: Duration::from_secs(1),
            max_topics: 100,
        }
    }
}

impl GossipConfig {
    pub fn development() -> Self {
        Self {
            max_message_size: 64 * 1024, // 64KB
            message_buffer_size: 100,
            operation_timeout: Duration::from_secs(10),
            heartbeat_interval: Duration::from_secs(2),
            max_topics: 10,
        }
    }

    pub fn production() -> Self {
        Self {
            max_message_size: 10 * 1024 * 1024, // 10MB
            message_buffer_size: 10000,
            operation_timeout: Duration::from_secs(60),
            heartbeat_interval: Duration::from_millis(500),
            max_topics: 1000,
        }
    }

    pub fn testing() -> Self {
        Self {
            max_message_size: 1024, // 1KB
            message_buffer_size: 10,
            operation_timeout: Duration::from_secs(5),
            heartbeat_interval: Duration::from_secs(5),
            max_topics: 5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ClientConfig::default();
        assert!(config.enable_pubsub);
        assert!(config.enable_discovery_n0);
        assert!(config.enable_discovery_mdns);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_development_config() {
        let config = ClientConfig::development();
        assert!(config.enable_pubsub);
        assert!(!config.enable_discovery_n0); // Desabilitado para dev
        assert!(config.enable_discovery_mdns);
        assert_eq!(config.port, 0); // Porta aleatória
    }

    #[test]
    fn test_production_config() {
        let config = ClientConfig::production();
        assert!(config.enable_pubsub);
        assert!(config.enable_discovery_n0);
        assert!(config.enable_discovery_mdns);
        assert_eq!(config.port, 4001); // Porta fixa
    }

    #[test]
    fn test_testing_config() {
        let config = ClientConfig::testing();
        assert!(config.enable_pubsub);
        assert!(!config.enable_discovery_n0);
        assert!(!config.enable_discovery_mdns);
        assert_eq!(config.port, 0);
    }

    #[test]
    fn test_offline_config() {
        let config = ClientConfig::offline();
        assert!(!config.enable_pubsub);
        assert!(!config.enable_discovery_n0);
        assert!(!config.enable_discovery_mdns);
    }

    #[test]
    fn test_config_validation() {
        let mut config = ClientConfig::default();

        // Configuração válida
        assert!(config.validate().is_ok());

        // Porta privilegiada (< 1024) sem ser 0
        config.port = 80;
        assert!(config.validate().is_err());

        // Porta 0 (aleatória) é válida
        config.port = 0;
        assert!(config.validate().is_ok());

        // Porta normal é válida
        config.port = 4001;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_add_known_peer() {
        use iroh::SecretKey;
        use rand_core::OsRng;

        let mut config = ClientConfig::default();
        let secret = SecretKey::generate(OsRng);
        let peer = secret.public();

        config.add_known_peer(peer);
        assert!(config.known_peers.contains(&peer));

        // Não deve duplicar
        config.add_known_peer(peer);
        assert_eq!(config.known_peers.len(), 1);
    }

    #[test]
    fn test_with_data_path() {
        let config = ClientConfig::default().with_data_path("/custom/path");
        assert_eq!(config.data_store_path, Some(PathBuf::from("/custom/path")));
    }

    #[test]
    fn test_with_port() {
        let config = ClientConfig::default().with_port(8080);
        assert_eq!(config.port, 8080);
    }

    #[test]
    fn test_persistent_storage_detection() {
        // Com armazenamento persistente
        let persistent_config = ClientConfig::development();
        assert!(persistent_config.uses_persistent_storage());

        // Em memória
        let memory_config = ClientConfig::testing();
        assert!(!memory_config.uses_persistent_storage());
    }

    #[test]
    fn test_discovery_detection() {
        // Com discovery habilitado
        let with_discovery = ClientConfig::default();
        assert!(with_discovery.has_discovery_enabled());

        // Sem discovery
        let without_discovery = ClientConfig::offline();
        assert!(!without_discovery.has_discovery_enabled());
    }
}
