use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Consistency intent attached to an ODM operation.
///
/// `LocalAtomic` is implemented today: validation, index maintenance, and the
/// write are serialized within one collection instance. `Replicated` reserves
/// the API shape for a future distributed transaction coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConsistencyLevel {
    #[default]
    LocalAtomic,
    Replicated,
}

/// Metadata identifying a single ODM transaction (id, start time, consistency).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionContext {
    pub id: Uuid,
    pub started_at: DateTime<Utc>,
    pub consistency: ConsistencyLevel,
}

impl TransactionContext {
    /// Creates a new local-atomic transaction context.
    pub fn local() -> Self {
        Self {
            id: Uuid::new_v4(),
            started_at: Utc::now(),
            consistency: ConsistencyLevel::LocalAtomic,
        }
    }

    /// Creates a transaction context with an explicit consistency level.
    pub fn with_consistency(consistency: ConsistencyLevel) -> Self {
        Self {
            consistency,
            ..Self::local()
        }
    }
}

/// Per-write options; carries an optional transaction context.
#[derive(Debug, Clone, Default)]
pub struct WriteOptions {
    pub transaction: Option<TransactionContext>,
}
