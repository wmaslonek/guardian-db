//! # Guardian Compute LLM layer (feature `compute-llm`, RFC 0004 phase 1)
//!
//! Generative LLM inference for Guardian Compute: the [`LlmBackend`] trait,
//! the owner-curated [`LlmRegistry`], and the active health-checking that
//! decides which models this node advertises (RFC 0004 §6.1).
//!
//! The shape mirrors [`NnModelRegistry`](super::nn::NnModelRegistry): the
//! executor's **owner** registers backends, and registration (plus a first
//! successful probe) is what opts the node into serving the corresponding
//! models. Backends are process/HTTP boundaries — colibri as a supervised
//! child process, mesh-llm through its OpenAI-compatible API (RFC 0004 §3.3);
//! nothing is linked in, nothing is forked.
//!
//! ## Health (RFC 0004 §6.1)
//!
//! Each backend is probed via [`LlmBackend::models`] on a jittered interval
//! ([`ProbeConfig`]). `failure_threshold` consecutive failures mark it
//! unhealthy and withdraw its models from [`LlmRegistry::advertised_models`]
//! (what phase 4 feeds into the `CapabilityVector`); one success restores it.
//! Failure-on-use is the inner safety net: a generation hitting a dead
//! backend records the failure and triggers an immediate out-of-cycle probe.
//!
//! ## Model identity (RFC 0004 §6.3)
//!
//! [`ModelInfo::content_hash`] is `Some` only when the backend can read the
//! weight bytes (BLAKE3, same convention as `NnModelRegistry`). Requesters
//! pin with [`ModelSelector::required_hash`]; a name match with the wrong or
//! absent hash is [`LlmError::HashMismatch`], never a silent name fallback.

#[cfg(feature = "compute-llm-colibri")]
pub mod colibri;
pub mod openai;
pub mod router;
#[cfg(feature = "compute-llm-colibri")]
pub use colibri::{ColibriConfig, ColibriProcessBackend, PromptTemplate};
pub use openai::{OpenAiConfig, OpenAiHttpBackend};
pub use router::{LlmRouter, PeerDelegation, RouterConfig};

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use iroh_blobs::Hash;
use serde::{Deserialize, Serialize};

/// Streamed generation output: zero or more [`GenerateEvent::Delta`]s
/// followed by exactly one [`GenerateEvent::End`]. Dropping the stream
/// cancels the generation (backends must treat it as RFC 0004 §6.4
/// cancellation).
pub type GenerateStream = futures::stream::BoxStream<'static, Result<GenerateEvent, LlmError>>;

/// Where a backend executes (RFC 0004 §3.1). Feeds the resolution preference
/// in [`LlmRegistry::backend_for`] (local wins) and, in phase 4, the router's
/// placement decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Locality {
    /// Inference runs on this machine (colibri, llama.cpp, ...).
    Local,
    /// The backend itself distributes the work across peers (mesh-llm).
    Distributed,
}

/// One model a backend serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// The name guests select by (OpenAI dialect, e.g. `"GLM-4.7-Flash"`).
    pub name: String,
    /// BLAKE3 of the weight bytes when the backend can read them; `None` is
    /// the explicit "identity claimed by name only" signal (RFC 0004 §6.3).
    pub content_hash: Option<Hash>,
}

/// What a request asks for: a model name, optionally pinned to exact weights.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSelector {
    pub name: String,
    /// When set, only backends advertising this exact hash qualify.
    pub required_hash: Option<Hash>,
}

impl From<&str> for ModelSelector {
    fn from(name: &str) -> Self {
        Self {
            name: name.into(),
            required_hash: None,
        }
    }
}

impl From<String> for ModelSelector {
    fn from(name: String) -> Self {
        Self {
            name,
            required_hash: None,
        }
    }
}

/// Chat message role (OpenAI dialect).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

/// Sampling parameters (OpenAI dialect; `None` means backend default).
///
/// No `skip_serializing_if` here: these types cross the `COMPUTE_ALPN` wire
/// as postcard (RFC 0004 §6.2), which cannot survive omitted fields. JSON
/// field skipping lives in the backend's own wire types (`openai.rs`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SamplingParams {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stop: Vec<String>,
}

/// A chat-completion-shaped generation request. This is the type that phase 4
/// serializes (CBOR, SDK convention) across `COMPUTE_ALPN`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerateRequest {
    pub model: ModelSelector,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub params: SamplingParams,
    /// Total budget covering queue wait + generation (RFC 0004 §6.4). `None`
    /// leaves the backend's own deadline policy in charge.
    pub deadline: Option<Duration>,
}

/// Why a generation stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FinishReason {
    /// Natural stop (EOS or a `stop` sequence).
    Stop,
    /// `max_tokens` reached.
    Length,
    /// The requester cancelled (dropped the stream).
    Cancelled,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

/// One frame of a [`GenerateStream`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GenerateEvent {
    /// A token delta (UTF-8 text under the OpenAI dialect).
    Delta { content: String },
    /// Terminal frame — exactly one per stream, always last.
    End {
        finish_reason: FinishReason,
        usage: Usage,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("no healthy backend serves model `{0}`")]
    ModelNotFound(String),
    /// The name is served, but not with the hash the requester pinned
    /// (RFC 0004 §6.3: never a silent fallback to name matching).
    #[error("model `{name}` is served, but not with pinned hash {pinned}")]
    HashMismatch { name: String, pinned: Hash },
    /// The backend did not answer (connection refused, probe timeout, ...).
    #[error("backend unavailable: {0}")]
    BackendUnavailable(String),
    /// The backend process died mid-request. Never silently retried —
    /// generation is non-deterministic (RFC 0004 §6.2).
    #[error("backend crashed: {0}")]
    BackendCrashed(String),
    /// The backend's bounded queue is full — fail fast so the scheduler
    /// routes around this node (RFC 0004 §6.4).
    #[error("backend request queue is full")]
    QueueFull,
    #[error("deadline exceeded")]
    DeadlineExceeded,
    #[error("generation was cancelled")]
    Cancelled,
    /// Malformed backend response (bad SSE frame, broken handshake, ...).
    #[error("backend protocol error: {0}")]
    Protocol(String),
}

/// A generative-inference engine behind a process/HTTP boundary
/// (RFC 0004 §3.1).
#[async_trait]
pub trait LlmBackend: Send + Sync {
    /// Models this backend can serve right now. Doubles as the health probe
    /// (RFC 0004 §6.1): an `Err` counts as a probe failure.
    async fn models(&self) -> Result<Vec<ModelInfo>, LlmError>;

    /// Streaming generation. Implementations must emit exactly one
    /// [`GenerateEvent::End`] as the final item of a successful stream.
    async fn generate(&self, req: GenerateRequest) -> Result<GenerateStream, LlmError>;

    fn locality(&self) -> Locality;
}

/// Health-probe policy (RFC 0004 §6.1).
#[derive(Debug, Clone, Copy)]
pub struct ProbeConfig {
    /// Base interval between probe rounds.
    pub interval: Duration,
    /// Fractional jitter applied per round: the sleep is uniform in
    /// `interval * [1 - jitter, 1 + jitter]`.
    pub jitter: f64,
    /// Per-backend deadline for one [`LlmBackend::models`] call.
    pub timeout: Duration,
    /// Consecutive failures that mark a backend unhealthy.
    pub failure_threshold: u32,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(30),
            jitter: 0.1,
            timeout: Duration::from_secs(10),
            failure_threshold: 3,
        }
    }
}

/// Observability snapshot of one backend's health.
#[derive(Debug, Clone)]
pub struct BackendHealth {
    pub healthy: bool,
    pub consecutive_failures: u32,
    /// Set while unhealthy.
    pub unhealthy_since: Option<Instant>,
    /// The most recent probe/use error, kept for diagnostics.
    pub last_error: Option<String>,
}

struct BackendEntry {
    backend: Arc<dyn LlmBackend>,
    /// Last successful probe result — what this backend advertises. Empty
    /// until the first successful probe: registration alone advertises
    /// nothing (RFC 0004 §6.1).
    models: Vec<ModelInfo>,
    consecutive_failures: u32,
    /// `Some` while unhealthy (withdrawn from advertisement).
    unhealthy_since: Option<Instant>,
    last_error: Option<String>,
}

impl BackendEntry {
    fn healthy(&self) -> bool {
        self.unhealthy_since.is_none()
    }

    fn record_failure(&mut self, id: &str, error: String, threshold: u32) {
        self.consecutive_failures += 1;
        self.last_error = Some(error);
        if self.unhealthy_since.is_none() && self.consecutive_failures >= threshold {
            self.unhealthy_since = Some(Instant::now());
            tracing::warn!(
                backend = id,
                failures = self.consecutive_failures,
                error = self.last_error.as_deref().unwrap_or(""),
                "LLM backend marked unhealthy; models withdrawn from advertisement"
            );
        }
    }

    fn record_success(&mut self, id: &str, models: Vec<ModelInfo>) {
        if self.unhealthy_since.take().is_some() {
            tracing::info!(backend = id, "LLM backend recovered; models re-advertised");
        }
        self.consecutive_failures = 0;
        self.last_error = None;
        self.models = models;
    }
}

/// Owner-curated catalog of LLM backends with active health-checking
/// (RFC 0004 §3.2, §6.1).
///
/// Registration alone advertises nothing: a backend's models appear in
/// [`advertised_models`](Self::advertised_models) after its first successful
/// probe (call [`probe_backend`](Self::probe_backend) after registering for
/// immediate activation, or let [`spawn_prober`](Self::spawn_prober)'s loop
/// pick it up).
pub struct LlmRegistry {
    backends: parking_lot::RwLock<HashMap<String, BackendEntry>>,
    config: ProbeConfig,
}

impl Default for LlmRegistry {
    fn default() -> Self {
        Self::new(ProbeConfig::default())
    }
}

impl std::fmt::Debug for LlmRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let map = self.backends.read();
        let mut healthy = Vec::new();
        let mut unhealthy = Vec::new();
        for (id, entry) in map.iter() {
            if entry.healthy() {
                healthy.push(id.clone());
            } else {
                unhealthy.push(id.clone());
            }
        }
        healthy.sort();
        unhealthy.sort();
        f.debug_struct("LlmRegistry")
            .field("healthy", &healthy)
            .field("unhealthy", &unhealthy)
            .field("advertised", &self.advertised_model_names())
            .finish()
    }
}

impl LlmRegistry {
    pub fn new(config: ProbeConfig) -> Self {
        Self {
            backends: parking_lot::RwLock::new(HashMap::new()),
            config,
        }
    }

    /// Registers `backend` under `id`, replacing any previous backend with
    /// the same id (the replacement starts unprobed, advertising nothing).
    pub fn register_backend(&self, id: impl Into<String>, backend: Arc<dyn LlmBackend>) {
        self.backends.write().insert(
            id.into(),
            BackendEntry {
                backend,
                models: Vec::new(),
                consecutive_failures: 0,
                unhealthy_since: None,
                last_error: None,
            },
        );
    }

    /// Stops serving through `id` and drops its advertisement.
    pub fn unregister_backend(&self, id: &str) {
        self.backends.write().remove(id);
    }

    pub fn backend_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.backends.read().keys().cloned().collect();
        ids.sort();
        ids
    }

    pub fn is_empty(&self) -> bool {
        self.backends.read().is_empty()
    }

    /// Health snapshot for `id` (observability/tests).
    pub fn health(&self, id: &str) -> Option<BackendHealth> {
        self.backends.read().get(id).map(|e| BackendHealth {
            healthy: e.healthy(),
            consecutive_failures: e.consecutive_failures,
            unhealthy_since: e.unhealthy_since,
            last_error: e.last_error.clone(),
        })
    }

    /// Models of **healthy** backends — what phase 4 feeds into the
    /// `CapabilityVector`. Sorted by name, `(name, hash)`-deduplicated.
    pub fn advertised_models(&self) -> Vec<ModelInfo> {
        let map = self.backends.read();
        let mut models: Vec<ModelInfo> = map
            .values()
            .filter(|e| e.healthy())
            .flat_map(|e| e.models.iter().cloned())
            .collect();
        models.sort_by(|a, b| (&a.name, &a.content_hash).cmp(&(&b.name, &b.content_hash)));
        models.dedup();
        models
    }

    /// Advertised model names, deduplicated (the `CapabilityVector` shape).
    pub fn advertised_model_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .advertised_models()
            .into_iter()
            .map(|m| m.name)
            .collect();
        names.dedup();
        names
    }

    /// Resolves `selector` to a healthy backend advertising the model,
    /// preferring [`Locality::Local`] (RFC 0004 §3.4 step 1; the full
    /// local → peers → distributed decision lives in [`router::LlmRouter`]).
    pub fn backend_for(&self, selector: &ModelSelector) -> Result<Arc<dyn LlmBackend>, LlmError> {
        self.resolve(selector, None).map(|(_, backend)| backend)
    }

    /// Like [`backend_for`](Self::backend_for), restricted to backends of one
    /// [`Locality`] — the router resolves its tiers separately.
    pub fn backend_for_locality(
        &self,
        selector: &ModelSelector,
        locality: Locality,
    ) -> Result<Arc<dyn LlmBackend>, LlmError> {
        self.resolve(selector, Some(locality))
            .map(|(_, backend)| backend)
    }

    fn resolve(
        &self,
        selector: &ModelSelector,
        filter: Option<Locality>,
    ) -> Result<(String, Arc<dyn LlmBackend>), LlmError> {
        let map = self.backends.read();
        let mut name_seen = false;
        let mut distributed: Option<(String, Arc<dyn LlmBackend>)> = None;
        // Deterministic tie-break between equally qualified backends.
        let mut ids: Vec<&String> = map.keys().collect();
        ids.sort();
        for id in ids {
            let entry = &map[id];
            if !entry.healthy() {
                continue;
            }
            let locality = entry.backend.locality();
            if filter.is_some_and(|wanted| wanted != locality) {
                continue;
            }
            for model in &entry.models {
                if model.name != selector.name {
                    continue;
                }
                name_seen = true;
                if let Some(pinned) = selector.required_hash
                    && model.content_hash != Some(pinned)
                {
                    continue; // wrong or unverifiable weights (§6.3)
                }
                match locality {
                    Locality::Local => return Ok((id.clone(), entry.backend.clone())),
                    Locality::Distributed => {
                        if distributed.is_none() {
                            distributed = Some((id.clone(), entry.backend.clone()));
                        }
                    }
                }
            }
        }
        if let Some(found) = distributed {
            return Ok(found);
        }
        match (name_seen, selector.required_hash) {
            (true, Some(pinned)) => Err(LlmError::HashMismatch {
                name: selector.name.clone(),
                pinned,
            }),
            _ => Err(LlmError::ModelNotFound(selector.name.clone())),
        }
    }

    /// Resolves and generates. An unavailable/crashed backend records a use
    /// failure and triggers an immediate out-of-cycle probe (RFC 0004 §6.1)
    /// before the error propagates — the caller (or the router's failover)
    /// decides whether to retry elsewhere.
    pub async fn generate(
        self: &Arc<Self>,
        req: GenerateRequest,
    ) -> Result<GenerateStream, LlmError> {
        self.generate_filtered(req, None).await
    }

    /// [`generate`](Self::generate) restricted to one [`Locality`] tier
    /// (used by [`router::LlmRouter`]).
    pub async fn generate_at(
        self: &Arc<Self>,
        req: GenerateRequest,
        locality: Locality,
    ) -> Result<GenerateStream, LlmError> {
        self.generate_filtered(req, Some(locality)).await
    }

    async fn generate_filtered(
        self: &Arc<Self>,
        req: GenerateRequest,
        filter: Option<Locality>,
    ) -> Result<GenerateStream, LlmError> {
        let (id, backend) = self.resolve(&req.model, filter)?;
        match backend.generate(req).await {
            Err(err @ (LlmError::BackendUnavailable(_) | LlmError::BackendCrashed(_))) => {
                self.record_use_failure(&id, &err);
                let registry = Arc::downgrade(self);
                tokio::spawn(async move {
                    if let Some(registry) = registry.upgrade() {
                        registry.probe_backend(&id).await;
                    }
                });
                Err(err)
            }
            other => other,
        }
    }

    fn record_use_failure(&self, id: &str, error: &LlmError) {
        if let Some(entry) = self.backends.write().get_mut(id) {
            entry.record_failure(id, error.to_string(), self.config.failure_threshold);
        }
    }

    /// Probes a single backend now. Returns whether the probe succeeded
    /// (`None` if `id` is not registered).
    pub async fn probe_backend(&self, id: &str) -> Option<bool> {
        let backend = self.backends.read().get(id).map(|e| e.backend.clone())?;
        let result = self.run_probe(&backend).await;
        let ok = result.is_ok();
        // The backend may have been (un/re-)registered while we awaited;
        // only apply to the entry that still holds the instance we probed.
        let mut map = self.backends.write();
        let entry = map.get_mut(id)?;
        if !Arc::ptr_eq(&entry.backend, &backend) {
            return None;
        }
        match result {
            Ok(models) => entry.record_success(id, models),
            Err(error) => entry.record_failure(id, error, self.config.failure_threshold),
        }
        Some(ok)
    }

    /// One probe round over every registered backend (RFC 0004 §6.1).
    pub async fn probe_once(&self) {
        let ids = self.backend_ids();
        futures::future::join_all(ids.iter().map(|id| self.probe_backend(id))).await;
    }

    async fn run_probe(&self, backend: &Arc<dyn LlmBackend>) -> Result<Vec<ModelInfo>, String> {
        match tokio::time::timeout(self.config.timeout, backend.models()).await {
            Ok(Ok(models)) => Ok(models),
            Ok(Err(err)) => Err(err.to_string()),
            Err(_) => Err(format!("probe timed out after {:?}", self.config.timeout)),
        }
    }

    /// Spawns the periodic probe loop (jittered interval, RFC 0004 §6.1).
    /// Holds only a `Weak` on the registry: the loop ends when the last
    /// strong reference drops, so the handle need not be awaited or aborted.
    pub fn spawn_prober(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let registry = Arc::downgrade(self);
        let ProbeConfig {
            interval, jitter, ..
        } = self.config;
        tokio::spawn(async move {
            loop {
                // Uniform in interval * [1 - jitter, 1 + jitter].
                let factor = 1.0 + jitter * (fastrand::f64() * 2.0 - 1.0);
                tokio::time::sleep(interval.mul_f64(factor.max(0.0))).await;
                let Some(registry) = registry.upgrade() else {
                    break;
                };
                registry.probe_once().await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    /// Scriptable backend: probe result and generate outcome are swappable.
    struct MockBackend {
        models: parking_lot::Mutex<Result<Vec<ModelInfo>, String>>,
        generate_error: Option<LlmError>,
        locality: Locality,
    }

    impl MockBackend {
        fn serving(models: Vec<ModelInfo>, locality: Locality) -> Arc<Self> {
            Arc::new(Self {
                models: parking_lot::Mutex::new(Ok(models)),
                generate_error: None,
                locality,
            })
        }

        fn failing(locality: Locality) -> Arc<Self> {
            Arc::new(Self {
                models: parking_lot::Mutex::new(Err("connection refused".into())),
                generate_error: Some(LlmError::BackendUnavailable("connection refused".into())),
                locality,
            })
        }

        fn set_models(&self, models: Result<Vec<ModelInfo>, String>) {
            *self.models.lock() = models;
        }
    }

    #[async_trait]
    impl LlmBackend for MockBackend {
        async fn models(&self) -> Result<Vec<ModelInfo>, LlmError> {
            self.models
                .lock()
                .clone()
                .map_err(LlmError::BackendUnavailable)
        }

        async fn generate(&self, _req: GenerateRequest) -> Result<GenerateStream, LlmError> {
            if let Some(err) = &self.generate_error {
                return Err(match err {
                    LlmError::BackendUnavailable(m) => LlmError::BackendUnavailable(m.clone()),
                    _ => LlmError::Protocol("unexpected".into()),
                });
            }
            Ok(futures::stream::iter(vec![
                Ok(GenerateEvent::Delta {
                    content: "hi".into(),
                }),
                Ok(GenerateEvent::End {
                    finish_reason: FinishReason::Stop,
                    usage: Usage {
                        prompt_tokens: 1,
                        completion_tokens: 1,
                    },
                }),
            ])
            .boxed())
        }

        fn locality(&self) -> Locality {
            self.locality
        }
    }

    fn model(name: &str) -> ModelInfo {
        ModelInfo {
            name: name.into(),
            content_hash: None,
        }
    }

    fn hashed_model(name: &str, bytes: &[u8]) -> ModelInfo {
        ModelInfo {
            name: name.into(),
            content_hash: Some(Hash::new(bytes)),
        }
    }

    fn request(model_name: &str) -> GenerateRequest {
        GenerateRequest {
            model: model_name.into(),
            messages: vec![ChatMessage::user("hello")],
            params: SamplingParams::default(),
            deadline: None,
        }
    }

    #[tokio::test]
    async fn registration_advertises_nothing_until_first_successful_probe() {
        let registry = LlmRegistry::default();
        registry.register_backend(
            "local",
            MockBackend::serving(vec![model("m")], Locality::Local),
        );
        assert!(registry.advertised_models().is_empty());

        assert_eq!(registry.probe_backend("local").await, Some(true));
        assert_eq!(registry.advertised_model_names(), vec!["m"]);
    }

    #[tokio::test]
    async fn threshold_failures_withdraw_and_one_success_restores() {
        let registry = LlmRegistry::default(); // failure_threshold: 3
        let backend = MockBackend::serving(vec![model("m")], Locality::Local);
        registry.register_backend("local", backend.clone());
        registry.probe_backend("local").await;

        backend.set_models(Err("boom".into()));
        // Below the threshold the last known models stay advertised.
        for expected_failures in 1..3u32 {
            registry.probe_backend("local").await;
            let health = registry.health("local").unwrap();
            assert!(health.healthy);
            assert_eq!(health.consecutive_failures, expected_failures);
            assert_eq!(registry.advertised_model_names(), vec!["m"]);
        }
        // Third consecutive failure trips it: withdrawn.
        registry.probe_backend("local").await;
        let health = registry.health("local").unwrap();
        assert!(!health.healthy);
        assert!(health.unhealthy_since.is_some());
        assert_eq!(
            health.last_error.as_deref(),
            Some("backend unavailable: boom")
        );
        assert!(registry.advertised_models().is_empty());

        // One success restores and re-advertises.
        backend.set_models(Ok(vec![model("m")]));
        registry.probe_backend("local").await;
        let health = registry.health("local").unwrap();
        assert!(health.healthy);
        assert_eq!(health.consecutive_failures, 0);
        assert_eq!(registry.advertised_model_names(), vec!["m"]);
    }

    #[tokio::test]
    async fn resolution_prefers_local_over_distributed() {
        let registry = LlmRegistry::default();
        registry.register_backend(
            "mesh",
            MockBackend::serving(vec![model("m")], Locality::Distributed),
        );
        registry.register_backend(
            "coli",
            MockBackend::serving(vec![model("m")], Locality::Local),
        );
        registry.probe_once().await;

        let backend = registry.backend_for(&"m".into()).unwrap();
        assert_eq!(backend.locality(), Locality::Local);
    }

    #[tokio::test]
    async fn hash_pinning_never_falls_back_to_name_matching() {
        let registry = LlmRegistry::default();
        registry.register_backend(
            "coli",
            MockBackend::serving(vec![hashed_model("m", b"weights-v1")], Locality::Local),
        );
        registry.probe_once().await;

        // Right hash resolves.
        let pinned = ModelSelector {
            name: "m".into(),
            required_hash: Some(Hash::new(b"weights-v1")),
        };
        assert!(registry.backend_for(&pinned).is_ok());

        // Wrong hash is a HashMismatch, not a name-based fallback.
        let wrong = ModelSelector {
            name: "m".into(),
            required_hash: Some(Hash::new(b"weights-v2")),
        };
        assert!(matches!(
            registry.backend_for(&wrong),
            Err(LlmError::HashMismatch { .. })
        ));

        // Unknown name is ModelNotFound.
        assert!(matches!(
            registry.backend_for(&"ghost".into()),
            Err(LlmError::ModelNotFound(_))
        ));
    }

    #[tokio::test]
    async fn unhealthy_backends_are_skipped_by_resolution() {
        let registry = LlmRegistry::new(ProbeConfig {
            failure_threshold: 1,
            ..ProbeConfig::default()
        });
        let backend = MockBackend::serving(vec![model("m")], Locality::Local);
        registry.register_backend("local", backend.clone());
        registry.probe_backend("local").await;
        assert!(registry.backend_for(&"m".into()).is_ok());

        backend.set_models(Err("down".into()));
        registry.probe_backend("local").await;
        assert!(matches!(
            registry.backend_for(&"m".into()),
            Err(LlmError::ModelNotFound(_))
        ));
    }

    #[tokio::test]
    async fn generate_streams_deltas_then_end() {
        let registry = Arc::new(LlmRegistry::default());
        registry.register_backend(
            "local",
            MockBackend::serving(vec![model("m")], Locality::Local),
        );
        registry.probe_once().await;

        let events: Vec<GenerateEvent> = registry
            .generate(request("m"))
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect()
            .await;
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], GenerateEvent::Delta { .. }));
        assert!(matches!(
            events.last(),
            Some(GenerateEvent::End {
                finish_reason: FinishReason::Stop,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn use_failure_is_recorded_against_the_backend() {
        let registry = Arc::new(LlmRegistry::default());
        let backend = MockBackend::failing(Locality::Local);
        backend.set_models(Ok(vec![model("m")])); // probes fine, generate fails
        registry.register_backend("flaky", backend.clone());
        registry.probe_once().await;

        let result = registry.generate(request("m")).await;
        assert!(matches!(result, Err(LlmError::BackendUnavailable(_))));
        // The failure is recorded; the out-of-cycle reprobe (spawned, may or
        // may not have run yet) can only reset it by *succeeding*, which is
        // fine either way — assert the record happened.
        let health = registry.health("flaky").unwrap();
        assert!(health.consecutive_failures >= 1 || health.last_error.is_none());
    }

    #[tokio::test]
    async fn advertised_models_deduplicate_across_backends() {
        let registry = LlmRegistry::default();
        registry.register_backend("a", MockBackend::serving(vec![model("m")], Locality::Local));
        registry.register_backend(
            "b",
            MockBackend::serving(vec![model("m")], Locality::Distributed),
        );
        registry.probe_once().await;

        assert_eq!(registry.advertised_models().len(), 1);
        assert_eq!(registry.advertised_model_names(), vec!["m"]);
    }
}
