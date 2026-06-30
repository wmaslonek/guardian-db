use crate::guardian::error::GuardianError;
use thiserror::Error;

/// Errors produced by GuardianDB's optional ODM layer.
#[derive(Debug, Error)]
pub enum OdmError {
    #[error("invalid schema: {0}")]
    InvalidSchema(String),

    #[error("validation failed for field `{field}`: {message}")]
    Validation { field: String, message: String },

    #[error("duplicate value for unique field `{field}`: {value}")]
    DuplicateKey { field: String, value: String },

    #[error("invalid query: {0}")]
    InvalidQuery(String),

    #[error("invalid update: {0}")]
    InvalidUpdate(String),

    #[error("immutable field `{0}` cannot be updated")]
    ImmutableField(String),

    #[error("unsupported consistency level: {0}")]
    UnsupportedConsistency(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error(transparent)]
    Guardian(#[from] GuardianError),

    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
}

/// Convenience `Result` alias for ODM operations.
pub type Result<T> = std::result::Result<T, OdmError>;
