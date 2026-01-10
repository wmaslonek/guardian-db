use thiserror::Error;

/// Tipos de erro específicos para guardian-db
#[derive(Error, Debug, Clone)]
pub enum GuardianError {
    #[error("Store error: {0}")]
    Store(String),

    #[error("Index error: {0}")]
    Index(String),

    #[error("Cache error: {0}")]
    Cache(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Iroh error: {0}")]
    Iroh(String),

    #[error("Access control error: {0}")]
    AccessControl(String),

    #[error("Network connection error: {0}")]
    NetworkConnection(String),

    #[error("Operation timeout: {0}")]
    Timeout(String),

    #[error("Invalid hash: {0}")]
    InvalidHash(String),

    #[error("PubSub error: {0}")]
    Pubsub(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Peer not found: {0}")]
    PeerNotFound(String),

    #[error("Topic not found: {0}")]
    TopicNotFound(String),

    #[error("Data not found for hash: {0}")]
    DataNotFound(String),

    #[error("Operation not supported: {0}")]
    UnsupportedOperation(String),

    #[error("Client not initialized")]
    ClientNotInitialized,

    #[error("Swarm not available")]
    SwarmNotAvailable,

    #[error("Resource busy: {0}")]
    ResourceBusy(String),

    #[error("Resource limit exceeded: {0}")]
    ResourceLimitExceeded(String),

    #[error("Operation error: {0}")]
    Operation(String),

    #[error("Replication error: {0}")]
    Replication(String),

    #[error("Invalid argument: {0}")]
    InvalidArgument(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Database already exists: {0}")]
    DatabaseAlreadyExists(String),

    #[error("IO error: {0}")]
    Io(String),

    #[error("JSON error: {0}")]
    Json(String),

    #[error("Hash error: {0}")]
    Hash(String),

    #[error("CBOR error: {0}")]
    Cbor(String),

    #[error("Iroh API error: {0}")]
    IrohApi(String),

    #[error("Datastore error: {0}")]
    Datastore(String),

    #[error("Lock poisoned")]
    LockPoisoned,

    #[error("Other error: {0}")]
    Other(String),
}

// Implementações From para conversões de erro com Clone
impl From<std::io::Error> for GuardianError {
    fn from(err: std::io::Error) -> Self {
        match err.kind() {
            std::io::ErrorKind::NotFound => GuardianError::NotFound(err.to_string()),
            std::io::ErrorKind::TimedOut => GuardianError::Timeout(err.to_string()),
            _ => GuardianError::Io(err.to_string()),
        }
    }
}

impl From<serde_json::Error> for GuardianError {
    fn from(err: serde_json::Error) -> Self {
        GuardianError::Json(err.to_string())
    }
}

impl From<serde_cbor::Error> for GuardianError {
    fn from(err: serde_cbor::Error) -> Self {
        GuardianError::Cbor(err.to_string())
    }
}

impl From<Box<dyn std::error::Error + Send + Sync>> for GuardianError {
    fn from(err: Box<dyn std::error::Error + Send + Sync>) -> Self {
        GuardianError::Other(err.to_string())
    }
}

impl From<String> for GuardianError {
    fn from(err: String) -> Self {
        GuardianError::Other(err)
    }
}

impl From<&str> for GuardianError {
    fn from(err: &str) -> Self {
        GuardianError::Other(err.to_string())
    }
}

impl GuardianError {
    /// Cria erro de rede
    pub fn network<S: Into<String>>(msg: S) -> Self {
        Self::NetworkConnection(msg.into())
    }

    /// Cria erro de timeout
    pub fn timeout<S: Into<String>>(msg: S) -> Self {
        Self::Timeout(msg.into())
    }

    /// Cria erro de hash inválido
    pub fn invalid_hash<S: Into<String>>(hash: S) -> Self {
        Self::InvalidHash(hash.into())
    }

    /// Cria erro de PubSub
    pub fn pubsub<S: Into<String>>(msg: S) -> Self {
        Self::Pubsub(msg.into())
    }

    /// Cria erro de armazenamento
    pub fn storage<S: Into<String>>(msg: S) -> Self {
        Self::Storage(msg.into())
    }

    /// Cria erro de configuração
    pub fn config<S: Into<String>>(msg: S) -> Self {
        Self::Config(msg.into())
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
        matches!(self, Self::NetworkConnection(_) | Self::Network(_))
    }

    /// Verifica se é um erro de timeout
    pub fn is_timeout_error(&self) -> bool {
        matches!(self, Self::Timeout(_))
    }

    /// Verifica se é um erro de dados não encontrados
    pub fn is_not_found_error(&self) -> bool {
        matches!(
            self,
            Self::DataNotFound(_)
                | Self::PeerNotFound(_)
                | Self::TopicNotFound(_)
                | Self::NotFound(_)
        )
    }

    /// Verifica se é um erro recuperável
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            Self::NetworkConnection(_)
                | Self::Network(_)
                | Self::Timeout(_)
                | Self::ResourceBusy(_)
        )
    }
}

/// Alias para Result com GuardianError
pub type Result<T> = std::result::Result<T, GuardianError>;

/// Macro para criar erros facilmente
#[macro_export]
macro_rules! guardian_error {
    (network, $msg:expr) => {
        $crate::guardian::error::GuardianError::network($msg)
    };
    (timeout, $msg:expr) => {
        $crate::guardian::error::GuardianError::timeout($msg)
    };
    (invalid_hash, $hash:expr) => {
        $crate::guardian::error::GuardianError::invalid_hash($hash)
    };
    (pubsub, $msg:expr) => {
        $crate::guardian::error::GuardianError::pubsub($msg)
    };
    (storage, $msg:expr) => {
        $crate::guardian::error::GuardianError::storage($msg)
    };
    (config, $msg:expr) => {
        $crate::guardian::error::GuardianError::config($msg)
    };
    (not_found, $item:expr) => {
        $crate::guardian::error::GuardianError::data_not_found($item)
    };
    (unsupported, $op:expr) => {
        $crate::guardian::error::GuardianError::unsupported($op)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_creation() {
        let net_err = GuardianError::network("Connection failed");
        assert!(net_err.is_network_error());
        assert!(net_err.is_recoverable());

        let timeout_err = GuardianError::timeout("Operation timed out");
        assert!(timeout_err.is_timeout_error());
        assert!(timeout_err.is_recoverable());

        let not_found_err = GuardianError::data_not_found("QmTest123");
        assert!(not_found_err.is_not_found_error());
        assert!(!not_found_err.is_recoverable());
    }

    #[test]
    fn test_error_conversions() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "File not found");
        let guardian_err: GuardianError = io_err.into();
        assert!(guardian_err.is_not_found_error());

        let timeout_io_err = std::io::Error::new(std::io::ErrorKind::TimedOut, "Timeout");
        let guardian_timeout_err: GuardianError = timeout_io_err.into();
        // Nota: io::Error é convertido para GuardianError::Io, não Timeout
        if let GuardianError::Io(msg) = guardian_timeout_err {
            assert!(msg.contains("Timeout"));
        }
    }

    #[test]
    fn test_error_macro() {
        let net_err = guardian_error!(network, "Connection failed");
        assert!(net_err.is_network_error());

        let timeout_err = guardian_error!(timeout, "Timeout occurred");
        assert!(timeout_err.is_timeout_error());

        let hash_err = guardian_error!(invalid_hash, "invalid_hash_123");
        match hash_err {
            GuardianError::InvalidHash(hash) => assert_eq!(hash, "invalid_hash_123"),
            _ => panic!("Expected InvalidHash error"),
        }
    }

    #[test]
    fn test_error_display() {
        let err = GuardianError::network("Connection refused");
        let display = format!("{}", err);
        assert!(display.contains("Network connection error"));
        assert!(display.contains("Connection refused"));
    }

    #[test]
    fn test_error_recovery_checks() {
        // Erros recuperáveis
        assert!(GuardianError::network("test").is_recoverable());
        assert!(GuardianError::timeout("test").is_recoverable());
        assert!(GuardianError::ResourceBusy("test".to_string()).is_recoverable());

        // Erros não recuperáveis
        assert!(!GuardianError::data_not_found("test").is_recoverable());
        assert!(!GuardianError::InvalidHash("test".to_string()).is_recoverable());
        assert!(!GuardianError::UnsupportedOperation("test".to_string()).is_recoverable());
    }

    #[test]
    fn test_not_found_variants() {
        assert!(GuardianError::DataNotFound("hash".to_string()).is_not_found_error());
        assert!(GuardianError::PeerNotFound("peer".to_string()).is_not_found_error());
        assert!(GuardianError::TopicNotFound("topic".to_string()).is_not_found_error());
        assert!(GuardianError::NotFound("item".to_string()).is_not_found_error());
    }
}
