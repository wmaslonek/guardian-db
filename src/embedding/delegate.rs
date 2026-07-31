//! Delegated embedding execution (feature `compute`, RFC 0005 §6.4).
//!
//! The seam through which the [`EmbeddingService`](super::service::EmbeddingService)
//! sends embedding work to a GPU peer instead of running it locally. The
//! service owns the *policy* (hash-pin default, local fallback, provenance);
//! an [`EmbeddingDelegate`] owns the *transport* (which peer, how to reach it).
//!
//! Keeping this a trait means the service's delegation logic is exercised in
//! tests with a mock transport, and the real over-the-network implementation
//! ([`ComputeEmbeddingDelegate`], built on `COMPUTE_ALPN`) is one
//! implementation rather than a hard dependency baked into the pipeline.

use async_trait::async_trait;
use iroh::EndpointId as NodeId;
use iroh_blobs::Hash;

use super::{EmbeddingError, ModelSelector};

/// A vector produced by a peer, with the identity needed to honor a §6.3
/// pin and to record provenance.
#[derive(Debug, Clone)]
pub struct DelegatedEmbedding {
    /// The embedding.
    pub vector: Vec<f32>,
    /// The peer's advertised model weight hash, if it could prove it. When a
    /// request pins a hash, a delegate MUST reject a peer whose returned
    /// hash does not match (never silently accept name-only), so by the time
    /// this is returned the pin — if any — is already satisfied.
    pub model_hash: Option<Hash>,
    /// The peer that produced it (for provenance: `peer:<id>`).
    pub peer: NodeId,
}

/// Transport for delegating one embedding to a peer.
#[async_trait]
pub trait EmbeddingDelegate: Send + Sync {
    /// Embed `text` with `model` on some eligible peer.
    ///
    /// `pin_hash` requires the chosen peer to serve the exact pinned weights;
    /// with it set, a peer advertising the name but not the hash is not
    /// eligible and a returned mismatch is a hard [`EmbeddingError::HashMismatch`]
    /// — the delegate must never fall back to name-only matching. A peer that
    /// cannot be reached or has no candidates is [`EmbeddingError::Delegation`],
    /// which the service turns into a local fallback when the rule allows it.
    async fn embed_delegated(
        &self,
        model: &ModelSelector,
        pin_hash: bool,
        text: &str,
    ) -> Result<DelegatedEmbedding, EmbeddingError>;
}

// ---------------------------------------------------------------------------
// The real transport: delegate over COMPUTE_ALPN to a gossip-discovered peer.
// ---------------------------------------------------------------------------

use std::sync::Arc;
use std::time::Duration;

use crate::compute::TaskClass;
use crate::compute::protocol::{ComputeClient, EmbedReply, EmbedRequest};
use crate::compute::scheduler::{CapabilityDirectory, ScoreWeights, score};

/// Tuning for a [`ComputeEmbeddingDelegate`].
#[derive(Debug, Clone)]
pub struct EmbedDelegateConfig {
    /// Peer candidates tried (best-ranked first) before giving up.
    pub max_attempts: usize,
    /// Connect + round-trip budget per peer attempt.
    pub timeout: Duration,
    /// Capability vectors older than this are ignored.
    pub max_vector_age: Duration,
    /// Ranking weights (shared shape with the compute scheduler).
    pub weights: ScoreWeights,
}

impl Default for EmbedDelegateConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            timeout: Duration::from_secs(30),
            max_vector_age: Duration::from_secs(10 * 60),
            weights: ScoreWeights::default(),
        }
    }
}

/// The over-the-network [`EmbeddingDelegate`]: picks a peer advertising the
/// model in the gossip [`CapabilityDirectory`] and sends it an `Embed`
/// request on `COMPUTE_ALPN`, honoring the §6.3 pin end to end.
pub struct ComputeEmbeddingDelegate {
    client: ComputeClient,
    directory: Arc<CapabilityDirectory>,
    /// This node — never a delegation candidate.
    local: NodeId,
    config: EmbedDelegateConfig,
}

impl ComputeEmbeddingDelegate {
    pub fn new(
        client: ComputeClient,
        directory: Arc<CapabilityDirectory>,
        local: NodeId,
        config: EmbedDelegateConfig,
    ) -> Self {
        Self {
            client,
            directory,
            local,
            config,
        }
    }

    /// Peers advertising `model`, best-ranked first for `Inference` work.
    fn ranked_peers(&self, model: &str) -> Vec<NodeId> {
        let mut ranked: Vec<(NodeId, f64)> = self
            .directory
            .snapshot(self.config.max_vector_age)
            .into_iter()
            .filter(|v| v.node_id != self.local && v.offers_embed_model(model))
            .filter_map(|v| {
                score(&v, TaskClass::Inference, &self.config.weights).map(|s| (v.node_id, s))
            })
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked.into_iter().map(|(n, _)| n).collect()
    }
}

#[async_trait]
impl EmbeddingDelegate for ComputeEmbeddingDelegate {
    async fn embed_delegated(
        &self,
        model: &ModelSelector,
        pin_hash: bool,
        text: &str,
    ) -> Result<DelegatedEmbedding, EmbeddingError> {
        // Resolve what we require of the peer's weights. A pin without a
        // concrete hash cannot be satisfied by any honest peer — surface it
        // rather than silently downgrading to name-only.
        let required_hash = if pin_hash {
            match model.required_hash {
                Some(h) => Some(h),
                None => {
                    return Err(EmbeddingError::Delegation(
                        "hash pin requested but the model selector carries no hash".into(),
                    ));
                }
            }
        } else {
            None
        };

        let peers = self.ranked_peers(&model.name);
        if peers.is_empty() {
            return Err(EmbeddingError::Delegation(format!(
                "no peer advertises embedding model `{}`",
                model.name
            )));
        }

        let mut last: Option<EmbeddingError> = None;
        for peer in peers.into_iter().take(self.config.max_attempts) {
            let request = EmbedRequest {
                task_id: uuid::Uuid::new_v4(),
                model: model.name.clone(),
                required_hash,
                text: text.to_string(),
            };
            match self
                .client
                .embed_on(peer, request, self.config.timeout)
                .await
            {
                Ok(EmbedReply::Ok { vector, model_hash }) => {
                    // Defense in depth: never accept a mismatched hash even if
                    // the executor should already have rejected it.
                    if let Some(want) = required_hash
                        && model_hash != Some(want)
                    {
                        last = Some(EmbeddingError::HashMismatch {
                            name: model.name.clone(),
                        });
                        continue;
                    }
                    return Ok(DelegatedEmbedding {
                        vector,
                        model_hash,
                        peer,
                    });
                }
                Ok(EmbedReply::Rejected(msg)) => {
                    last = Some(EmbeddingError::Delegation(format!(
                        "peer {} rejected: {msg}",
                        peer.fmt_short()
                    )));
                }
                Err(e) => {
                    // An unreachable peer's advertisement is stale; forget it.
                    self.directory.remove(&peer);
                    last = Some(EmbeddingError::Delegation(format!(
                        "peer {} unreachable: {e}",
                        peer.fmt_short()
                    )));
                }
            }
        }
        Err(last.unwrap_or_else(|| EmbeddingError::Delegation("no eligible peer".into())))
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::embedding::{Embedder, HashEmbedder};

    /// A delegate that "runs" on a fake peer using a local [`HashEmbedder`],
    /// counting calls — enough to test the service's delegation policy
    /// (pin enforcement, fallback, provenance) without a network.
    pub struct MockDelegate {
        embedder: HashEmbedder,
        peer: NodeId,
        pub calls: Arc<AtomicUsize>,
        /// When set, every delegation fails, to exercise fallback.
        pub always_fail: bool,
        /// When set, the peer returns a hash that never matches a pin.
        pub wrong_hash: bool,
    }

    impl MockDelegate {
        pub fn new(name: &str, dims: u32) -> Self {
            Self {
                embedder: HashEmbedder::new(name, dims),
                peer: iroh::SecretKey::generate().public(),
                calls: Arc::new(AtomicUsize::new(0)),
                always_fail: false,
                wrong_hash: false,
            }
        }

        pub fn failing(mut self) -> Self {
            self.always_fail = true;
            self
        }

        pub fn with_wrong_hash(mut self) -> Self {
            self.wrong_hash = true;
            self
        }
    }

    #[async_trait]
    impl EmbeddingDelegate for MockDelegate {
        async fn embed_delegated(
            &self,
            model: &ModelSelector,
            pin_hash: bool,
            text: &str,
        ) -> Result<DelegatedEmbedding, EmbeddingError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.always_fail {
                return Err(EmbeddingError::Delegation("mock peer unreachable".into()));
            }
            let info = self.embedder.info();
            let advertised = if self.wrong_hash {
                Some(Hash::from_bytes([9u8; 32]))
            } else {
                info.content_hash
            };
            // Enforce the pin exactly as the real transport must.
            if pin_hash {
                match (model.required_hash, advertised) {
                    (Some(want), Some(got)) if want == got => {}
                    _ => {
                        return Err(EmbeddingError::HashMismatch {
                            name: model.name.clone(),
                        });
                    }
                }
            }
            let v = self
                .embedder
                .embed(std::slice::from_ref(&text.to_string()))
                .await?
                .pop()
                .unwrap();
            Ok(DelegatedEmbedding {
                vector: v,
                model_hash: advertised,
                peer: self.peer,
            })
        }
    }
}
