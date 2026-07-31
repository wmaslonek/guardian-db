//! Embedding rules: what to embed, where to write it, and how to run it.

use crate::embedding::ModelSelector;
use serde::{Deserialize, Serialize};

/// Where a rule's embeddings are computed (RFC 0005 §6.4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionPolicy {
    /// Embed on this node only, using a registered local [`Embedder`]. A
    /// privacy-sensitive corpus that must never leave the machine uses this.
    ///
    /// [`Embedder`]: crate::embedding::Embedder
    #[default]
    LocalOnly,
    /// Prefer delegating to a GPU peer through the Guardian Compute scheduler
    /// (feature `compute`), falling back to local execution when no eligible
    /// peer is available and `allow_local_fallback` is set.
    ///
    /// `pin_hash` defaults to `true`: a delegated request pins the model's
    /// BLAKE3 hash, so a peer that cannot prove it holds the exact weights is
    /// ineligible and a mismatch is a hard error — the unsafe direction
    /// (accepting name-only delegation, which permits silent corpus
    /// poisoning) is the one that needs an explicit opt-out.
    Delegated {
        pin_hash: bool,
        allow_local_fallback: bool,
    },
}

impl ExecutionPolicy {
    /// The robust delegation default: hash-pinned, with local fallback so a
    /// transient lack of peers degrades to correct local execution rather
    /// than stalling the pipeline.
    pub fn delegated() -> Self {
        Self::Delegated {
            pin_hash: true,
            allow_local_fallback: true,
        }
    }
}

/// A declarative binding: "for table `schema.table`, embed the text in
/// `text_column` with `model` and write the vector into `vector_column`".
///
/// The service maintains idempotency with a sidecar column named
/// [`source_hash_column`](Self::source_hash_column) (`_<vector_column>_srchash`
/// by convention): it stores a hash of the exact text that produced the
/// current vector, so replays, refreshes, and unrelated-column updates never
/// re-embed unchanged text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingRule {
    pub schema: String,
    pub table: String,
    /// The text column to read (`NULL`/absent text → the row is skipped and
    /// its vector left `NULL`).
    pub text_column: String,
    /// The `vector(N)` column to populate. `N` must equal the model's
    /// dimension or the service reports a `DimensionMismatch` and skips.
    pub vector_column: String,
    /// The model to embed with, optionally hash-pinned.
    pub model: ModelSelector,
    /// Execution policy (local vs delegated).
    #[serde(default)]
    pub policy: ExecutionPolicy,
}

impl EmbeddingRule {
    /// Build a local-only rule in the `public` schema.
    pub fn new(
        table: impl Into<String>,
        text_column: impl Into<String>,
        vector_column: impl Into<String>,
        model: ModelSelector,
    ) -> Self {
        Self {
            schema: "public".into(),
            table: table.into(),
            text_column: text_column.into(),
            vector_column: vector_column.into(),
            model,
            policy: ExecutionPolicy::LocalOnly,
        }
    }

    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.schema = schema.into();
        self
    }

    pub fn with_policy(mut self, policy: ExecutionPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// The sidecar column carrying the source-text hash for idempotency.
    pub fn source_hash_column(&self) -> String {
        format!("_{}_srchash", self.vector_column)
    }

    /// Whether a change to `schema.table` is governed by this rule.
    pub fn matches(&self, schema: &str, table: &str) -> bool {
        self.schema == schema && self.table == table
    }

    /// Stable identifier for logging / dedup within a service.
    pub fn id(&self) -> String {
        format!(
            "{}.{}({}->{})",
            self.schema, self.table, self.text_column, self.vector_column
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_helpers() {
        let r = EmbeddingRule::new("docs", "body", "embedding", ModelSelector::by_name("m"));
        assert_eq!(r.schema, "public");
        assert_eq!(r.policy, ExecutionPolicy::LocalOnly);
        assert_eq!(r.source_hash_column(), "_embedding_srchash");
        assert!(r.matches("public", "docs"));
        assert!(!r.matches("public", "other"));
    }

    #[test]
    fn delegated_default_is_pinned_with_fallback() {
        assert_eq!(
            ExecutionPolicy::delegated(),
            ExecutionPolicy::Delegated {
                pin_hash: true,
                allow_local_fallback: true
            }
        );
    }
}
