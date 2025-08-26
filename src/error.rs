use thiserror::Error;

/// Tipos de erro específicos para guardian-db
#[derive(Error, Debug)]
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
    
    #[error("IPFS error: {0}")]
    Ipfs(String),
    
    #[error("Access control error: {0}")]
    AccessControl(String),
    
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
    Io(#[from] std::io::Error),
    
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    
    #[error("CID error: {0}")]
    Cid(#[from] cid::Error),
    
    #[error("CBOR error: {0}")]
    Cbor(#[from] serde_cbor::Error),
    
    #[error("IPFS API error: {0}")]
    IpfsApi(#[from] ipfs_api_backend_hyper::Error),
    
    #[error("Datastore error: {0}")]
    Datastore(String),
    
    #[error("Other error: {0}")]
    Other(String),
}

// Removendo conversões de anyhow já que não vamos mais usar
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

/// Alias para Result com GuardianError
pub type Result<T> = std::result::Result<T, GuardianError>;
