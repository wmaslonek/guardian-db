// Tratamento de erros específicos da API IPFS
//
// Estende o sistema de erros do GuardianDB com erros específicos do IPFS

use thiserror::Error;

/// Erros específicos da API IPFS Core
#[derive(Error, Debug)]
pub enum IpfsError {
    /// Erro de conexão de rede
    #[error("Network connection error: {0}")]
    NetworkError(String),

    /// Erro de timeout em operação
    #[error("Operation timeout: {0}")]
    TimeoutError(String),

    /// Erro de validação de CID/hash
    #[error("Invalid CID or hash: {0}")]
    InvalidCid(String),

    /// Erro de PubSub
    #[error("PubSub error: {0}")]
    PubsubError(String),

    /// Erro de armazenamento
    #[error("Storage error: {0}")]
    StorageError(String),

    /// Erro de configuração
    #[error("Configuration error: {0}")]
    ConfigError(String),

    /// Peer não encontrado
    #[error("Peer not found: {0}")]
    PeerNotFound(String),

    /// Tópico não encontrado
    #[error("Topic not found: {0}")]
    TopicNotFound(String),

    /// Dados não encontrados
    #[error("Data not found for hash: {0}")]
    DataNotFound(String),

    /// Operação não suportada
    #[error("Operation not supported: {0}")]
    UnsupportedOperation(String),

    /// Erro de serialização/deserialização
    #[error("Serialization error: {0}")]
    SerializationError(String),

    /// Cliente não inicializado
    #[error("Client not initialized")]
    ClientNotInitialized,

    /// Swarm não disponível
    #[error("Swarm not available")]
    SwarmNotAvailable,

    /// Recurso ocupado/em uso
    #[error("Resource busy: {0}")]
    ResourceBusy(String),

    /// Limite de recursos atingido
    #[error("Resource limit exceeded: {0}")]
    ResourceLimitExceeded(String),
}

impl IpfsError {
    /// Cria erro de rede
    pub fn network<S: Into<String>>(msg: S) -> Self {
        Self::NetworkError(msg.into())
    }

    /// Cria erro de timeout
    pub fn timeout<S: Into<String>>(msg: S) -> Self {
        Self::TimeoutError(msg.into())
    }

    /// Cria erro de CID inválido
    pub fn invalid_cid<S: Into<String>>(cid: S) -> Self {
        Self::InvalidCid(cid.into())
    }

    /// Cria erro de PubSub
    pub fn pubsub<S: Into<String>>(msg: S) -> Self {
        Self::PubsubError(msg.into())
    }

    /// Cria erro de armazenamento
    pub fn storage<S: Into<String>>(msg: S) -> Self {
        Self::StorageError(msg.into())
    }

    /// Cria erro de configuração
    pub fn config<S: Into<String>>(msg: S) -> Self {
        Self::ConfigError(msg.into())
    }

    /// Cria erro de peer não encontrado
    pub fn peer_not_found<S: Into<String>>(peer: S) -> Self {
        Self::PeerNotFound(peer.into())
    }

    /// Cria erro de tópico não encontrado
    pub fn topic_not_found<S: Into<String>>(topic: S) -> Self {
        Self::TopicNotFound(topic.into())
    }

    /// Cria erro de dados não encontrados
    pub fn data_not_found<S: Into<String>>(hash: S) -> Self {
        Self::DataNotFound(hash.into())
    }

    /// Cria erro de operação não suportada
    pub fn unsupported<S: Into<String>>(operation: S) -> Self {
        Self::UnsupportedOperation(operation.into())
    }

    /// Verifica se é um erro de rede
    pub fn is_network_error(&self) -> bool {
        matches!(self, Self::NetworkError(_))
    }

    /// Verifica se é um erro de timeout
    pub fn is_timeout_error(&self) -> bool {
        matches!(self, Self::TimeoutError(_))
    }

    /// Verifica se é um erro de dados não encontrados
    pub fn is_not_found_error(&self) -> bool {
        matches!(
            self,
            Self::DataNotFound(_) | Self::PeerNotFound(_) | Self::TopicNotFound(_)
        )
    }

    /// Verifica se é um erro recuperável
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            Self::NetworkError(_) | Self::TimeoutError(_) | Self::ResourceBusy(_)
        )
    }

    /// Converte para GuardianError
    pub fn into_guardian_error(self) -> crate::error::GuardianError {
        crate::error::GuardianError::Other(self.to_string())
    }
}

/// Implementa conversão para GuardianError
impl From<IpfsError> for crate::error::GuardianError {
    fn from(err: IpfsError) -> Self {
        Self::Other(err.to_string())
    }
}

/// Conversões de erros externos comuns
impl From<cid::Error> for IpfsError {
    fn from(err: cid::Error) -> Self {
        Self::InvalidCid(err.to_string())
    }
}

impl From<std::io::Error> for IpfsError {
    fn from(err: std::io::Error) -> Self {
        match err.kind() {
            std::io::ErrorKind::NotFound => Self::DataNotFound(err.to_string()),
            std::io::ErrorKind::TimedOut => Self::TimeoutError(err.to_string()),
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionAborted => {
                Self::NetworkError(err.to_string())
            }
            _ => Self::StorageError(err.to_string()),
        }
    }
}

impl From<serde_json::Error> for IpfsError {
    fn from(err: serde_json::Error) -> Self {
        Self::SerializationError(err.to_string())
    }
}

impl From<serde_cbor::Error> for IpfsError {
    fn from(err: serde_cbor::Error) -> Self {
        Self::SerializationError(err.to_string())
    }
}

/// Resultado específico para operações IPFS
pub type IpfsResult<T> = Result<T, IpfsError>;

/// Macro para criar erros IPFS facilmente
#[macro_export]
macro_rules! ipfs_error {
    (network, $msg:expr) => {
        $crate::ipfs_core_api::errors::IpfsError::network($msg)
    };
    (timeout, $msg:expr) => {
        $crate::ipfs_core_api::errors::IpfsError::timeout($msg)
    };
    (invalid_cid, $cid:expr) => {
        $crate::ipfs_core_api::errors::IpfsError::invalid_cid($cid)
    };
    (pubsub, $msg:expr) => {
        $crate::ipfs_core_api::errors::IpfsError::pubsub($msg)
    };
    (storage, $msg:expr) => {
        $crate::ipfs_core_api::errors::IpfsError::storage($msg)
    };
    (config, $msg:expr) => {
        $crate::ipfs_core_api::errors::IpfsError::config($msg)
    };
    (not_found, $item:expr) => {
        $crate::ipfs_core_api::errors::IpfsError::data_not_found($item)
    };
    (unsupported, $op:expr) => {
        $crate::ipfs_core_api::errors::IpfsError::unsupported($op)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_creation() {
        let net_err = IpfsError::network("Connection failed");
        assert!(net_err.is_network_error());
        assert!(net_err.is_recoverable());

        let timeout_err = IpfsError::timeout("Operation timed out");
        assert!(timeout_err.is_timeout_error());
        assert!(timeout_err.is_recoverable());

        let not_found_err = IpfsError::data_not_found("QmTest123");
        assert!(not_found_err.is_not_found_error());
        assert!(!not_found_err.is_recoverable());
    }

    #[test]
    fn test_error_conversions() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "File not found");
        let ipfs_err: IpfsError = io_err.into();
        assert!(ipfs_err.is_not_found_error());

        let timeout_io_err = std::io::Error::new(std::io::ErrorKind::TimedOut, "Timeout");
        let ipfs_timeout_err: IpfsError = timeout_io_err.into();
        assert!(ipfs_timeout_err.is_timeout_error());
    }

    #[test]
    fn test_guardian_error_conversion() {
        let ipfs_err = IpfsError::network("Test error");
        let guardian_err: crate::error::GuardianError = ipfs_err.into();

        match guardian_err {
            crate::error::GuardianError::Other(msg) => {
                assert!(msg.contains("Network connection error"));
            }
            _ => panic!("Expected GuardianError::Other"),
        }
    }

    #[test]
    fn test_error_macro() {
        let net_err = ipfs_error!(network, "Connection failed");
        assert!(net_err.is_network_error());

        let timeout_err = ipfs_error!(timeout, "Timeout occurred");
        assert!(timeout_err.is_timeout_error());

        let cid_err = ipfs_error!(invalid_cid, "QmInvalid");
        match cid_err {
            IpfsError::InvalidCid(cid) => assert_eq!(cid, "QmInvalid"),
            _ => panic!("Expected InvalidCid error"),
        }
    }

    #[test]
    fn test_error_display() {
        let err = IpfsError::network("Connection refused");
        let display = format!("{}", err);
        assert!(display.contains("Network connection error"));
        assert!(display.contains("Connection refused"));
    }
}
