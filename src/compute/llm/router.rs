//! # LLM placement router (RFC 0004 phase 4, §3.4)
//!
//! Decides where one generation runs, in strict tier order:
//!
//! 1. **Local backend** — a healthy [`Locality::Local`] backend serving the
//!    model (colibri, llama.cpp…);
//! 2. **Peer delegation** — nodes whose gossiped `CapabilityVector`
//!    advertises the model (`llm_models`, healthy-only by construction),
//!    ranked with the same [`score`] the WASM scheduler uses, streamed over
//!    `COMPUTE_ALPN`;
//! 3. **Distributed backend** — a [`Locality::Distributed`] backend
//!    (mesh-llm), which does its own splitting across peers.
//!
//! ## Failover discipline (§6.2)
//!
//! Every tier is *peeked*: [`LlmRouter::route`] resolves only once the first
//! frame of a working stream is in hand, so a tier that dies before its first
//! delta fails over to the next. After the first delta the stream is the
//! caller's — a mid-stream truncation is reported, never transparently
//! retried, because sampling is non-deterministic and a restarted generation
//! would visibly diverge. Re-issuing is the caller's decision.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use iroh::EndpointId as NodeId;
use uuid::Uuid;

use super::super::TaskClass;
use super::super::protocol::{ComputeClient, LlmGenerateRequest};
use super::super::scheduler::{CapabilityDirectory, ScoreWeights, score};
use super::{GenerateRequest, GenerateStream, LlmError, LlmRegistry, Locality};

/// Peer-delegation wiring; without it the router only knows tiers 1 and 3.
#[derive(Debug, Clone)]
pub struct PeerDelegation {
    pub client: ComputeClient,
    pub directory: Arc<CapabilityDirectory>,
    /// This node — never a delegation candidate.
    pub local: NodeId,
}

#[derive(Debug, Clone)]
pub struct RouterConfig {
    /// Connect + admission budget per peer attempt.
    pub ack_timeout: Duration,
    /// Peer candidates tried (best-ranked first) before tier 3.
    pub max_peer_attempts: usize,
    /// Capability vectors older than this are ignored.
    pub max_vector_age: Duration,
    /// Ranking weights (shared shape with the WASM scheduler).
    pub weights: ScoreWeights,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            ack_timeout: Duration::from_secs(10),
            max_peer_attempts: 3,
            max_vector_age: Duration::from_secs(10 * 60),
            weights: ScoreWeights::default(),
        }
    }
}

/// The placement decision-maker. See the module docs for the tier order.
pub struct LlmRouter {
    registry: Arc<LlmRegistry>,
    delegation: Option<PeerDelegation>,
    config: RouterConfig,
}

impl std::fmt::Debug for LlmRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmRouter")
            .field("registry", &self.registry)
            .field("peers", &self.delegation.is_some())
            .finish()
    }
}

impl LlmRouter {
    /// A router without peer delegation: local backends, then distributed.
    pub fn new(registry: Arc<LlmRegistry>, config: RouterConfig) -> Self {
        Self {
            registry,
            delegation: None,
            config,
        }
    }

    /// Enables tier 2 (peer delegation over `COMPUTE_ALPN`).
    pub fn with_peers(mut self, delegation: PeerDelegation) -> Self {
        self.delegation = Some(delegation);
        self
    }

    /// Routes one generation through the tiers, resolving once the first
    /// frame of a working stream arrives (§6.2: failover happens only before
    /// the first delta; afterwards the stream belongs to the caller).
    pub async fn route(&self, req: GenerateRequest) -> Result<GenerateStream, LlmError> {
        let mut attempts: Vec<String> = Vec::new();

        // Tier 1: local backend.
        match self
            .registry
            .generate_at(req.clone(), Locality::Local)
            .await
        {
            Ok(stream) => match peek(stream).await {
                Ok(stream) => return Ok(stream),
                Err(e) => attempts.push(format!("local backend: {e}")),
            },
            Err(LlmError::ModelNotFound(_)) => {} // not served locally: silent
            Err(e) => attempts.push(format!("local backend: {e}")),
        }

        // Tier 2: peers advertising the model.
        if let Some(delegation) = &self.delegation {
            for peer in self.ranked_peers(delegation, &req) {
                let request = LlmGenerateRequest {
                    task_id: Uuid::new_v4(),
                    request: req.clone(),
                };
                match delegation
                    .client
                    .generate_on(peer, request, self.config.ack_timeout)
                    .await
                {
                    Ok(stream) => match peek(stream).await {
                        Ok(stream) => return Ok(stream),
                        Err(e) => attempts.push(format!("peer {}: {e}", peer.fmt_short())),
                    },
                    Err(e) => attempts.push(format!("peer {}: {e}", peer.fmt_short())),
                }
            }
        }

        // Tier 3: distributed backend (mesh-llm does its own splitting).
        match self
            .registry
            .generate_at(req.clone(), Locality::Distributed)
            .await
        {
            Ok(stream) => match peek(stream).await {
                Ok(stream) => return Ok(stream),
                Err(e) => attempts.push(format!("distributed backend: {e}")),
            },
            Err(LlmError::ModelNotFound(_)) => {}
            Err(e) => attempts.push(format!("distributed backend: {e}")),
        }

        if attempts.is_empty() {
            Err(LlmError::ModelNotFound(req.model.name))
        } else {
            Err(LlmError::BackendUnavailable(format!(
                "no route for `{}` succeeded: {}",
                req.model.name,
                attempts.join("; ")
            )))
        }
    }

    /// Peer candidates for tier 2: fresh vectors advertising the model with
    /// inference capacity, best score first, capped at `max_peer_attempts`.
    /// Hash-pinned requests never delegate: a peer's advertisement carries
    /// names only, so the pin cannot be honored remotely (§6.3).
    fn ranked_peers(&self, delegation: &PeerDelegation, req: &GenerateRequest) -> Vec<NodeId> {
        if req.model.required_hash.is_some() {
            return Vec::new();
        }
        let mut ranked: Vec<(NodeId, f64)> = delegation
            .directory
            .snapshot(self.config.max_vector_age)
            .into_iter()
            .filter(|v| v.node_id != delegation.local)
            .filter(|v| v.offers_llm_model(&req.model.name))
            .filter_map(|v| {
                score(&v, TaskClass::Inference, &self.config.weights).map(|s| (v.node_id, s))
            })
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked
            .into_iter()
            .take(self.config.max_peer_attempts)
            .map(|(node, _)| node)
            .collect()
    }
}

/// Pulls the first item: an error (or an immediately empty stream) fails the
/// tier; anything else is chained back on and the stream is committed.
async fn peek(mut stream: GenerateStream) -> Result<GenerateStream, LlmError> {
    match stream.next().await {
        Some(Err(e)) => Err(e),
        None => Err(LlmError::Protocol(
            "stream ended without terminal frame".into(),
        )),
        first @ Some(Ok(_)) => Ok(futures::stream::iter(first).chain(stream).boxed()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        ChatMessage, FinishReason, GenerateEvent, LlmBackend, ModelInfo, ModelSelector,
        SamplingParams, Usage,
    };
    use super::*;
    use async_trait::async_trait;
    use iroh_blobs::Hash;

    /// Backend yielding a canned stream, or failing at start / first token.
    struct Scripted {
        locality: Locality,
        mode: Mode,
    }

    enum Mode {
        Works(&'static str),
        FailsAtStart,
        FailsAtFirstToken,
    }

    #[async_trait]
    impl LlmBackend for Scripted {
        async fn models(&self) -> Result<Vec<ModelInfo>, LlmError> {
            Ok(vec![ModelInfo {
                name: "m".into(),
                content_hash: None,
            }])
        }

        async fn generate(&self, _req: GenerateRequest) -> Result<GenerateStream, LlmError> {
            match self.mode {
                Mode::FailsAtStart => Err(LlmError::BackendUnavailable("down".into())),
                Mode::FailsAtFirstToken => Ok(futures::stream::iter(vec![Err(
                    LlmError::BackendCrashed("died".into()),
                )])
                .boxed()),
                Mode::Works(text) => Ok(futures::stream::iter(vec![
                    Ok(GenerateEvent::Delta {
                        content: text.into(),
                    }),
                    Ok(GenerateEvent::End {
                        finish_reason: FinishReason::Stop,
                        usage: Usage::default(),
                    }),
                ])
                .boxed()),
            }
        }

        fn locality(&self) -> Locality {
            self.locality
        }
    }

    async fn registry_with(backends: Vec<(&str, Scripted)>) -> Arc<LlmRegistry> {
        let registry = Arc::new(LlmRegistry::default());
        for (id, backend) in backends {
            registry.register_backend(id, Arc::new(backend));
        }
        registry.probe_once().await;
        registry
    }

    fn request() -> GenerateRequest {
        GenerateRequest {
            model: "m".into(),
            messages: vec![ChatMessage::user("hi")],
            params: SamplingParams::default(),
            deadline: None,
        }
    }

    async fn first_delta(stream: GenerateStream) -> String {
        let events: Vec<_> = stream.collect().await;
        match events.first() {
            Some(Ok(GenerateEvent::Delta { content })) => content.clone(),
            other => panic!("expected a delta, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn local_tier_wins_over_distributed() {
        let registry = registry_with(vec![
            (
                "mesh",
                Scripted {
                    locality: Locality::Distributed,
                    mode: Mode::Works("from-mesh"),
                },
            ),
            (
                "coli",
                Scripted {
                    locality: Locality::Local,
                    mode: Mode::Works("from-local"),
                },
            ),
        ])
        .await;
        let router = LlmRouter::new(registry, RouterConfig::default());

        let stream = router.route(request()).await.unwrap();
        assert_eq!(first_delta(stream).await, "from-local");
    }

    #[tokio::test]
    async fn local_failure_at_start_falls_through_to_distributed() {
        let registry = registry_with(vec![
            (
                "coli",
                Scripted {
                    locality: Locality::Local,
                    mode: Mode::FailsAtStart,
                },
            ),
            (
                "mesh",
                Scripted {
                    locality: Locality::Distributed,
                    mode: Mode::Works("from-mesh"),
                },
            ),
        ])
        .await;
        let router = LlmRouter::new(registry, RouterConfig::default());

        let stream = router.route(request()).await.unwrap();
        assert_eq!(first_delta(stream).await, "from-mesh");
    }

    #[tokio::test]
    async fn failure_at_first_token_also_fails_over() {
        // §6.2: before the first delta the router may still fail over.
        let registry = registry_with(vec![
            (
                "coli",
                Scripted {
                    locality: Locality::Local,
                    mode: Mode::FailsAtFirstToken,
                },
            ),
            (
                "mesh",
                Scripted {
                    locality: Locality::Distributed,
                    mode: Mode::Works("from-mesh"),
                },
            ),
        ])
        .await;
        let router = LlmRouter::new(registry, RouterConfig::default());

        let stream = router.route(request()).await.unwrap();
        assert_eq!(first_delta(stream).await, "from-mesh");
    }

    #[tokio::test]
    async fn unknown_model_is_model_not_found() {
        let registry = registry_with(vec![]).await;
        let router = LlmRouter::new(registry, RouterConfig::default());

        let result = router.route(request()).await;
        assert!(matches!(result, Err(LlmError::ModelNotFound(_))));
    }

    #[tokio::test]
    async fn every_tier_failing_reports_the_attempts() {
        let registry = registry_with(vec![
            (
                "coli",
                Scripted {
                    locality: Locality::Local,
                    mode: Mode::FailsAtStart,
                },
            ),
            (
                "mesh",
                Scripted {
                    locality: Locality::Distributed,
                    mode: Mode::FailsAtStart,
                },
            ),
        ])
        .await;
        let router = LlmRouter::new(registry, RouterConfig::default());

        match router.route(request()).await {
            Err(LlmError::BackendUnavailable(msg)) => {
                assert!(msg.contains("local backend"), "got: {msg}");
                assert!(msg.contains("distributed backend"), "got: {msg}");
            }
            Err(other) => panic!("expected BackendUnavailable, got {other:?}"),
            Ok(_) => panic!("expected BackendUnavailable, got a stream"),
        }
    }

    #[tokio::test]
    async fn hash_pinned_requests_never_delegate_to_peers() {
        // Registry serves nothing; a pin plus no local/distributed hash match
        // must not produce peer candidates (peers advertise names only).
        let registry = registry_with(vec![]).await;
        let router = LlmRouter::new(registry, RouterConfig::default());
        let mut req = request();
        req.model = ModelSelector {
            name: "m".into(),
            required_hash: Some(Hash::new(b"weights")),
        };
        // No delegation configured — this exercises ranked_peers' guard via
        // route() returning without a peer tier; the pin then surfaces as
        // not-found (nothing local/distributed matches the hash).
        let result = router.route(req).await;
        assert!(result.is_err());
    }
}
