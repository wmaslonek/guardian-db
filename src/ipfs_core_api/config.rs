// Configuração do cliente IPFS
//
// Centraliza todas as opções de configuração do cliente IPFS

use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Configuração completa do cliente IPFS
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    /// Habilita funcionalidades de PubSub
    pub enable_pubsub: bool,

    /// Habilita funcionalidades de Swarm/networking
    pub enable_swarm: bool,

    /// Caminho para armazenar dados do IPFS (opcional)
    pub data_store_path: Option<PathBuf>,

    /// Endereços de rede para escutar conexões
    pub listening_addrs: Vec<String>,

    /// Peers de bootstrap para conectar inicialmente
    pub bootstrap_peers: Vec<PeerId>,

    /// Habilita descoberta de peers via mDNS
    pub enable_mdns: bool,

    /// Habilita protocolo Kademlia DHT
    pub enable_kad: bool,

    /// Configurações de networking
    pub network: NetworkConfig,

    /// Configurações de armazenamento
    pub storage: StorageConfig,

    /// Configurações de PubSub
    pub pubsub: PubsubConfig,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            enable_pubsub: true,
            enable_swarm: true,
            data_store_path: None,
            listening_addrs: vec![
                "/ip4/0.0.0.0/tcp/0".to_string(),
                "/ip6/::/tcp/0".to_string(),
            ],
            bootstrap_peers: vec![],
            enable_mdns: true,
            enable_kad: true,
            network: NetworkConfig::default(),
            storage: StorageConfig::default(),
            pubsub: PubsubConfig::default(),
        }
    }
}

impl ClientConfig {
    /// Cria configuração mínima para desenvolvimento
    pub fn development() -> Self {
        Self {
            enable_pubsub: true, // Habilitado para testes de desenvolvimento
            enable_swarm: true,  // Necessário para pubsub funcionar
            data_store_path: Some("./tmp/ipfs_dev".into()),
            listening_addrs: vec!["/ip4/127.0.0.1/tcp/0".to_string()],
            bootstrap_peers: vec![],
            enable_mdns: false,
            enable_kad: false,
            network: NetworkConfig::development(),
            storage: StorageConfig::development(),
            pubsub: PubsubConfig::development(),
        }
    }

    /// Cria configuração para produção
    pub fn production() -> Self {
        Self {
            enable_pubsub: true,
            enable_swarm: true,
            data_store_path: Some("/var/lib/ipfs".into()),
            listening_addrs: vec![
                "/ip4/0.0.0.0/tcp/4001".to_string(),
                "/ip6/::/tcp/4001".to_string(),
                "/ip4/0.0.0.0/tcp/8081/ws".to_string(),
            ],
            bootstrap_peers: vec![], // Seria populado com peers reais
            enable_mdns: true,
            enable_kad: true,
            network: NetworkConfig::production(),
            storage: StorageConfig::production(),
            pubsub: PubsubConfig::production(),
        }
    }

    /// Configuração apenas para testes
    pub fn testing() -> Self {
        Self {
            enable_pubsub: true,
            enable_swarm: false,
            data_store_path: None, // Em memória
            listening_addrs: vec![],
            bootstrap_peers: vec![],
            enable_mdns: false,
            enable_kad: false,
            network: NetworkConfig::testing(),
            storage: StorageConfig::testing(),
            pubsub: PubsubConfig::testing(),
        }
    }

    /// Habilita modo offline (sem networking)
    pub fn offline() -> Self {
        Self {
            enable_pubsub: false,
            enable_swarm: false,
            enable_mdns: false,
            enable_kad: false,
            ..Self::development()
        }
    }

    /// Adiciona um peer de bootstrap
    pub fn add_bootstrap_peer(&mut self, peer: PeerId) {
        if !self.bootstrap_peers.contains(&peer) {
            self.bootstrap_peers.push(peer);
        }
    }

    /// Adiciona um endereço de escuta
    pub fn add_listening_addr(&mut self, addr: String) {
        if !self.listening_addrs.contains(&addr) {
            self.listening_addrs.push(addr);
        }
    }

    /// Define o caminho de armazenamento
    pub fn with_data_path<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.data_store_path = Some(path.into());
        self
    }

    /// Valida a configuração
    pub fn validate(&self) -> Result<(), String> {
        // Verifica consistência da configuração
        if self.enable_pubsub && !self.enable_swarm {
            return Err("PubSub requer Swarm habilitado".to_string());
        }

        if self.enable_kad && !self.enable_swarm {
            return Err("Kademlia DHT requer Swarm habilitado".to_string());
        }

        if self.enable_swarm && self.listening_addrs.is_empty() {
            return Err("Swarm requer pelo menos um endereço de escuta".to_string());
        }

        // Valida endereços
        for addr in &self.listening_addrs {
            if addr.is_empty() {
                return Err("Endereço de escuta não pode estar vazio".to_string());
            }
        }

        Ok(())
    }

    /// Cria configuração a partir de um cliente Hyper IPFS
    pub fn from_hyper_client(_hyper_client: &ipfs_api_backend_hyper::IpfsClient) -> Self {
        // Por enquanto, retorna configuração padrão já que o HyperIpfsClient
        // não expõe sua configuração interna facilmente
        Self::development()
    }
}

/// Configurações específicas de networking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Timeout para conexões
    pub connection_timeout: Duration,

    /// Número máximo de conexões simultâneas
    pub max_connections: usize,

    /// Habilita relay de conexões
    pub enable_relay: bool,

    /// Configurações de transport
    pub transport: TransportConfig,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            connection_timeout: Duration::from_secs(30),
            max_connections: 100,
            enable_relay: false,
            transport: TransportConfig::default(),
        }
    }
}

impl NetworkConfig {
    pub fn development() -> Self {
        Self {
            connection_timeout: Duration::from_secs(10),
            max_connections: 10,
            enable_relay: false,
            transport: TransportConfig::development(),
        }
    }

    pub fn production() -> Self {
        Self {
            connection_timeout: Duration::from_secs(60),
            max_connections: 1000,
            enable_relay: true,
            transport: TransportConfig::production(),
        }
    }

    pub fn testing() -> Self {
        Self {
            connection_timeout: Duration::from_secs(5),
            max_connections: 5,
            enable_relay: false,
            transport: TransportConfig::testing(),
        }
    }
}

/// Configurações de transport de rede
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportConfig {
    /// Habilita TCP
    pub enable_tcp: bool,

    /// Habilita QUIC
    pub enable_quic: bool,

    /// Habilita WebSocket
    pub enable_websocket: bool,

    /// Configurações de segurança
    pub security: SecurityConfig,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            enable_tcp: true,
            enable_quic: false,
            enable_websocket: false,
            security: SecurityConfig::default(),
        }
    }
}

impl TransportConfig {
    pub fn development() -> Self {
        Self {
            enable_tcp: true,
            enable_quic: false,
            enable_websocket: false,
            security: SecurityConfig::development(),
        }
    }

    pub fn production() -> Self {
        Self {
            enable_tcp: true,
            enable_quic: true,
            enable_websocket: true,
            security: SecurityConfig::production(),
        }
    }

    pub fn testing() -> Self {
        Self {
            enable_tcp: true,
            enable_quic: false,
            enable_websocket: false,
            security: SecurityConfig::testing(),
        }
    }
}

/// Configurações de segurança
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Habilita encriptação Noise
    pub enable_noise: bool,

    /// Força uso de TLS
    pub require_tls: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            enable_noise: true,
            require_tls: false,
        }
    }
}

impl SecurityConfig {
    pub fn development() -> Self {
        Self {
            enable_noise: false, // Simplificado para desenvolvimento
            require_tls: false,
        }
    }

    pub fn production() -> Self {
        Self {
            enable_noise: true,
            require_tls: true,
        }
    }

    pub fn testing() -> Self {
        Self {
            enable_noise: false,
            require_tls: false,
        }
    }
}

/// Configurações de armazenamento
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Habilita cache em memória
    pub enable_memory_cache: bool,

    /// Tamanho máximo do cache (bytes)
    pub max_cache_size: usize,

    /// Habilita compressão de dados
    pub enable_compression: bool,

    /// Tipo de backend de armazenamento
    pub backend: StorageBackend,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            enable_memory_cache: true,
            max_cache_size: 100 * 1024 * 1024, // 100MB
            enable_compression: false,
            backend: StorageBackend::Memory,
        }
    }
}

impl StorageConfig {
    pub fn development() -> Self {
        Self {
            enable_memory_cache: true,
            max_cache_size: 10 * 1024 * 1024, // 10MB
            enable_compression: false,
            backend: StorageBackend::Memory,
        }
    }

    pub fn production() -> Self {
        Self {
            enable_memory_cache: true,
            max_cache_size: 1024 * 1024 * 1024, // 1GB
            enable_compression: true,
            backend: StorageBackend::Sled,
        }
    }

    pub fn testing() -> Self {
        Self {
            enable_memory_cache: false,
            max_cache_size: 1024 * 1024, // 1MB
            enable_compression: false,
            backend: StorageBackend::Memory,
        }
    }
}

/// Tipos de backend de armazenamento
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StorageBackend {
    /// Armazenamento apenas em memória
    Memory,
    /// Backend Sled (embedded database)
    Sled,
    /// Sistema de arquivos
    FileSystem,
}

/// Configurações de PubSub
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PubsubConfig {
    /// Habilita validação de mensagens
    pub enable_message_validation: bool,

    /// Tamanho máximo de mensagem (bytes)
    pub max_message_size: usize,

    /// Buffer size para streams de mensagens
    pub message_buffer_size: usize,

    /// Timeout para operações de PubSub
    pub operation_timeout: Duration,
}

impl Default for PubsubConfig {
    fn default() -> Self {
        Self {
            enable_message_validation: true,
            max_message_size: 1024 * 1024, // 1MB
            message_buffer_size: 1000,
            operation_timeout: Duration::from_secs(30),
        }
    }
}

impl PubsubConfig {
    pub fn development() -> Self {
        Self {
            enable_message_validation: false,
            max_message_size: 64 * 1024, // 64KB
            message_buffer_size: 100,
            operation_timeout: Duration::from_secs(10),
        }
    }

    pub fn production() -> Self {
        Self {
            enable_message_validation: true,
            max_message_size: 10 * 1024 * 1024, // 10MB
            message_buffer_size: 10000,
            operation_timeout: Duration::from_secs(60),
        }
    }

    pub fn testing() -> Self {
        Self {
            enable_message_validation: false,
            max_message_size: 1024, // 1KB
            message_buffer_size: 10,
            operation_timeout: Duration::from_secs(5),
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
        assert!(config.enable_swarm);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_development_config() {
        let config = ClientConfig::development();
        assert!(config.enable_pubsub);
        assert!(config.enable_swarm);
        assert_eq!(config.listening_addrs.len(), 1);
    }

    #[test]
    fn test_production_config() {
        let config = ClientConfig::production();
        assert!(config.enable_pubsub);
        assert!(config.enable_swarm);
        assert!(config.listening_addrs.len() >= 2);
    }

    #[test]
    fn test_testing_config() {
        let config = ClientConfig::testing();
        assert!(config.enable_pubsub);
        assert!(!config.enable_swarm);
        assert!(config.listening_addrs.is_empty());
    }

    #[test]
    fn test_offline_config() {
        let config = ClientConfig::offline();
        assert!(!config.enable_pubsub);
        assert!(!config.enable_swarm);
        assert!(!config.enable_mdns);
        assert!(!config.enable_kad);
    }

    #[test]
    fn test_config_validation() {
        let mut config = ClientConfig::default();

        // Configuração válida
        assert!(config.validate().is_ok());

        // PubSub sem Swarm (inválido)
        config.enable_pubsub = true;
        config.enable_swarm = false;
        assert!(config.validate().is_err());

        // Swarm sem endereços de escuta (inválido)
        config.enable_swarm = true;
        config.listening_addrs.clear();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_add_bootstrap_peer() {
        let mut config = ClientConfig::default();
        let peer = PeerId::random();

        config.add_bootstrap_peer(peer);
        assert!(config.bootstrap_peers.contains(&peer));

        // Não deve duplicar
        config.add_bootstrap_peer(peer);
        assert_eq!(config.bootstrap_peers.len(), 1);
    }

    #[test]
    fn test_with_data_path() {
        let config = ClientConfig::default().with_data_path("/custom/path");

        assert_eq!(config.data_store_path, Some(PathBuf::from("/custom/path")));
    }
}
