//! # Auto-embedding pipeline (feature `embedding`, RFC 0005 phase 2)
//!
//! Closes the loop the vector index (phase 1) opened: rows carrying text get
//! their embedding computed and written into a `vector` column automatically,
//! so `ORDER BY embedding <=> $q LIMIT k` just works without the application
//! hand-rolling an embed step.
//!
//! The shape deliberately mirrors the RFC 0004 LLM layer
//! (the `compute::llm` module): a pluggable [`Embedder`] trait, an owner-curated
//! [`EmbeddingRegistry`] keyed by model name, and model identity carried as an
//! optional BLAKE3 [`content_hash`](EmbedderInfo::content_hash) for the §6.3
//! pin-by-hash guarantee. What phase 2 adds on top of that pattern is the
//! *data* side — the [`EmbeddingRule`](rule::EmbeddingRule) that binds a text
//! column to a vector column, and the [`EmbeddingService`](service::EmbeddingService)
//! that drives it off the engine's committed-change feed
//! ([`Database::subscribe_changes`](crate::sql::engine::Database::subscribe_changes)),
//! idempotently (a source-text hash sidecar skips unchanged rows) and with
//! provenance (which model, which hash, which executor produced each vector).
//!
//! ## Execution
//!
//! Where the embedding actually runs is a per-rule policy
//! ([`ExecutionPolicy`](rule::ExecutionPolicy)):
//! - **local** — a registered [`Embedder`] on this node (HTTP backend, ONNX
//!   backend, …);
//! - **delegated** (feature `compute`) — routed to a GPU peer through the
//!   existing Guardian Compute scheduler, hash-pinned by default so a peer
//!   that cannot prove it holds the exact weights is never chosen (§6.4).
//!
//! Backends are process/HTTP/library boundaries, exactly like the LLM layer:
//! [`OpenAiEmbeddingBackend`](openai::OpenAiEmbeddingBackend) fronts any
//! OpenAI-compatible `/v1/embeddings` server (ollama, llama.cpp, LM Studio,
//! OpenAI); the ONNX backend (feature `embedding-onnx`) runs a local model.

pub mod rule;
pub mod service;

/// Delegated embedding execution over Guardian Compute (RFC 0005 §6.4).
/// Available with the `compute` feature.
#[cfg(feature = "compute")]
pub mod delegate;
#[cfg(feature = "compute")]
pub use delegate::{
    ComputeEmbeddingDelegate, DelegatedEmbedding, EmbedDelegateConfig, EmbeddingDelegate,
};

pub mod openai;
pub use openai::{OpenAiEmbeddingBackend, OpenAiEmbeddingConfig};

#[cfg(feature = "embedding-onnx")]
pub mod onnx;
#[cfg(feature = "embedding-onnx")]
pub use onnx::{OnnxEmbeddingBackend, OnnxEmbeddingConfig};

/// Retrieval-augmented generation helper (RFC 0005 phase 3). Available with
/// the `rag` feature (`embedding` + `compute-llm`).
#[cfg(feature = "rag")]
pub mod rag;
#[cfg(feature = "rag")]
pub use rag::{Rag, RagConfig, RagError, Retrieved};

pub use rule::{EmbeddingRule, ExecutionPolicy};
pub use service::{EmbeddingService, EmbeddingStats};

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use iroh_blobs::Hash;
use serde::{Deserialize, Serialize};

/// Where an embedder executes — feeds the router's local-vs-delegated
/// preference, mirroring `compute::llm::Locality`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Locality {
    /// Runs on this machine.
    Local,
    /// The backend itself distributes the work (e.g. a remote pool).
    Distributed,
}

/// Identity and shape of a model an [`Embedder`] serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedderInfo {
    /// The model name callers select by (`text-embedding-3-small`,
    /// `all-MiniLM-L6-v2`, …).
    pub name: String,
    /// Output vector dimension. Must match the target `vector(N)` column.
    pub dimensions: u32,
    /// BLAKE3 hash of the weight bytes — `Some` only when the backend can
    /// actually read them (a local ONNX file); `None` for remote HTTP
    /// backends, which cannot prove their weights across the wire (§6.3).
    /// Absence is the honest "identity by name only" signal, never upgraded
    /// to verified.
    pub content_hash: Option<Hash>,
}

/// A caller's model requirement: a name, optionally pinned to an exact hash
/// (§6.3). A name match whose hash is wrong or absent is a
/// [`EmbeddingError::HashMismatch`], never a silent fallback to name-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSelector {
    pub name: String,
    pub required_hash: Option<Hash>,
}

impl ModelSelector {
    /// Select by name only (no pin).
    pub fn by_name(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            required_hash: None,
        }
    }

    /// Select by name pinned to an exact weight hash.
    pub fn pinned(name: impl Into<String>, hash: Hash) -> Self {
        Self {
            name: name.into(),
            required_hash: Some(hash),
        }
    }

    /// Whether `info` satisfies this selector. Name must match; if a hash is
    /// pinned it must equal the backend's advertised hash (absent hash never
    /// satisfies a pin).
    pub fn matches(&self, info: &EmbedderInfo) -> bool {
        info.name == self.name
            && match self.required_hash {
                None => true,
                Some(h) => info.content_hash == Some(h),
            }
    }
}

/// Errors an embedding operation can produce.
#[derive(Debug, thiserror::Error)]
pub enum EmbeddingError {
    /// No backend serves the requested model name.
    #[error("no embedder registered for model `{0}`")]
    UnknownModel(String),
    /// A backend serves the name but not the pinned hash (§6.3).
    #[error("model `{name}` is available but its weights do not match the pinned hash")]
    HashMismatch { name: String },
    /// The backend is unreachable or failed mid-request.
    #[error("embedding backend unavailable: {0}")]
    BackendUnavailable(String),
    /// The backend returned a vector whose length is not the advertised
    /// dimension, or a batch whose length is not the input length.
    #[error("embedding backend returned a malformed response: {0}")]
    Malformed(String),
    /// The produced dimension does not match the target column.
    #[error("embedding dimension mismatch: model produces {got}, column expects {expected}")]
    DimensionMismatch { got: usize, expected: usize },
    /// Delegated execution failed and no local fallback applies.
    #[error("delegated embedding failed: {0}")]
    Delegation(String),
}

/// A pluggable embedding model. Batch-oriented: callers pass many texts so a
/// backend can amortize per-request overhead (HTTP round-trips, ONNX session
/// dispatch). Implementations must return exactly one vector per input, in
/// order, each of [`EmbedderInfo::dimensions`] length.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Identity and output shape of the served model.
    fn info(&self) -> EmbedderInfo;

    /// Where this embedder runs (local vs distributed).
    fn locality(&self) -> Locality;

    /// Embed a batch of texts. One output vector per input, same order.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError>;

    /// Validate a backend's batch response shape against its contract — one
    /// vector per input, each the advertised dimension. Backends should call
    /// this before returning so a malformed remote response is a typed error,
    /// not a silently short/wrong write-back.
    fn validate_batch(&self, inputs: usize, out: &[Vec<f32>]) -> Result<(), EmbeddingError> {
        if out.len() != inputs {
            return Err(EmbeddingError::Malformed(format!(
                "expected {inputs} vectors, got {}",
                out.len()
            )));
        }
        let dims = self.info().dimensions as usize;
        for v in out {
            if v.len() != dims {
                return Err(EmbeddingError::Malformed(format!(
                    "expected {dims}-dim vectors, got {}",
                    v.len()
                )));
            }
        }
        Ok(())
    }
}

/// Owner-curated catalog of embedders this node serves, keyed by model name —
/// the embedding counterpart to
/// `LlmRegistry` and
/// `NnModelRegistry`. Registration is
/// what makes a model resolvable to the [`EmbeddingService`] and (with
/// `compute`) advertisable to peers.
#[derive(Default, Clone)]
pub struct EmbeddingRegistry {
    inner: Arc<parking_lot::RwLock<HashMap<String, Arc<dyn Embedder>>>>,
}

impl EmbeddingRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer `embedder` under its [`EmbedderInfo::name`].
    pub fn register(&self, embedder: Arc<dyn Embedder>) {
        let name = embedder.info().name;
        self.inner.write().insert(name, embedder);
    }

    /// Stop offering `name`.
    pub fn unregister(&self, name: &str) {
        self.inner.write().remove(name);
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    /// Registered model names, sorted.
    pub fn model_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.inner.read().keys().cloned().collect();
        names.sort();
        names
    }

    /// Resolve a selector to a backend, enforcing the §6.3 pin rule: a name
    /// match with the wrong/absent pinned hash is [`EmbeddingError::HashMismatch`],
    /// never a silent name-only fallback.
    pub fn resolve(&self, selector: &ModelSelector) -> Result<Arc<dyn Embedder>, EmbeddingError> {
        let guard = self.inner.read();
        let Some(embedder) = guard.get(&selector.name) else {
            return Err(EmbeddingError::UnknownModel(selector.name.clone()));
        };
        if selector.matches(&embedder.info()) {
            Ok(embedder.clone())
        } else {
            Err(EmbeddingError::HashMismatch {
                name: selector.name.clone(),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Deterministic embedder (tests, offline demos, CI)
// ---------------------------------------------------------------------------

/// A deterministic, dependency-free embedder: hashes the text into a fixed-
/// dimension unit vector. Not semantically meaningful — identical texts map
/// to identical vectors and near-identical texts do *not* map to nearby
/// vectors — but exact, offline, and reproducible, which is what the pipeline
/// tests and offline demos need. Never use it for real retrieval.
pub struct HashEmbedder {
    name: String,
    dims: u32,
}

impl HashEmbedder {
    pub fn new(name: impl Into<String>, dims: u32) -> Self {
        Self {
            name: name.into(),
            dims,
        }
    }

    fn embed_one(&self, text: &str) -> Vec<f32> {
        // Expand a BLAKE3 XOF over the text into `dims` f32s, then L2-
        // normalize (so cosine distance is well-defined, like real
        // embeddings).
        let mut hasher = blake3::Hasher::new();
        hasher.update(text.as_bytes());
        let mut xof = hasher.finalize_xof();
        let mut v = vec![0f32; self.dims as usize];
        let mut buf = [0u8; 4];
        for x in &mut v {
            xof.fill(&mut buf);
            // Map the 32 bits into [-1, 1).
            *x = (u32::from_le_bytes(buf) as f32 / u32::MAX as f32) * 2.0 - 1.0;
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }
}

#[async_trait]
impl Embedder for HashEmbedder {
    fn info(&self) -> EmbedderInfo {
        // A stable content hash derived from (name, dims): the deterministic
        // embedder *can* prove its "weights" (there are none beyond these
        // parameters), so it advertises a hash and supports pin-by-hash — the
        // one embedder whose identity is fully verifiable, useful for testing
        // the §6.3 path end to end.
        let mut h = blake3::Hasher::new();
        h.update(b"guardian-hash-embedder\0");
        h.update(self.name.as_bytes());
        h.update(&self.dims.to_le_bytes());
        EmbedderInfo {
            name: self.name.clone(),
            dimensions: self.dims,
            content_hash: Some(Hash::from_bytes(*h.finalize().as_bytes())),
        }
    }

    fn locality(&self) -> Locality {
        Locality::Local
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts.iter().map(|t| self.embed_one(t)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_embedder_is_deterministic_and_normalized() {
        let e = HashEmbedder::new("test", 16);
        let a = e.embed(&["hello".into()]).await.unwrap();
        let b = e.embed(&["hello".into()]).await.unwrap();
        assert_eq!(a, b, "same text -> same vector");
        let norm: f32 = a[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "unit-normalized");
        assert_eq!(a[0].len(), 16);
        let c = e.embed(&["world".into()]).await.unwrap();
        assert_ne!(a[0], c[0], "different text -> different vector");
    }

    #[tokio::test]
    async fn batch_preserves_order_and_count() {
        let e = HashEmbedder::new("test", 8);
        let texts: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        let out = e.embed(&texts).await.unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], e.embed(&["a".into()]).await.unwrap()[0]);
        assert_eq!(out[2], e.embed(&["c".into()]).await.unwrap()[0]);
    }

    #[test]
    fn registry_resolves_and_enforces_hash_pin() {
        let reg = EmbeddingRegistry::new();
        let e = Arc::new(HashEmbedder::new("m", 8));
        let hash = e.info().content_hash.unwrap();
        reg.register(e);
        // Name-only resolves.
        assert!(reg.resolve(&ModelSelector::by_name("m")).is_ok());
        // Correct pin resolves.
        assert!(reg.resolve(&ModelSelector::pinned("m", hash)).is_ok());
        // Wrong pin is a typed mismatch, never a name fallback.
        let wrong = Hash::from_bytes([0u8; 32]);
        assert!(matches!(
            reg.resolve(&ModelSelector::pinned("m", wrong)),
            Err(EmbeddingError::HashMismatch { .. })
        ));
        // Unknown name.
        assert!(matches!(
            reg.resolve(&ModelSelector::by_name("nope")),
            Err(EmbeddingError::UnknownModel(_))
        ));
    }

    #[tokio::test]
    async fn validate_batch_catches_malformed_shapes() {
        let e = HashEmbedder::new("m", 8);
        assert!(e.validate_batch(2, &[vec![0.0; 8], vec![0.0; 8]]).is_ok());
        assert!(e.validate_batch(2, &[vec![0.0; 8]]).is_err()); // wrong count
        assert!(e.validate_batch(1, &[vec![0.0; 4]]).is_err()); // wrong dim
    }
}
