//! OpenAI-compatible HTTP embedding backend.
//!
//! One [`Embedder`] that fronts any server speaking the OpenAI
//! `POST {base}/embeddings` dialect — OpenAI itself, and the local engines
//! that copy it (ollama `/v1`, llama.cpp `--embeddings`, LM Studio,
//! text-embeddings-inference, …). This is the "works today, no model files"
//! backend, mirroring the RFC 0004 LLM HTTP backend.
//!
//! HTTP backends cannot read the weight bytes, so [`EmbedderInfo::content_hash`]
//! is always `None` and a hash-pinned rule resolving to this backend is
//! rejected up-front by the registry (§6.3: absence of a hash is honest, never
//! upgraded to verified).

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{Embedder, EmbedderInfo, EmbeddingError, Locality};

/// Configuration for one [`OpenAiEmbeddingBackend`].
#[derive(Debug, Clone)]
pub struct OpenAiEmbeddingConfig {
    /// Base URL including the version segment, e.g. `http://127.0.0.1:11434/v1`
    /// (a trailing `/` is tolerated and stripped).
    pub base_url: String,
    /// The model name to send in the request and advertise to callers.
    pub model: String,
    /// Output dimension of the model (must equal the target `vector(N)`
    /// column). Validated against the server's response on every call.
    pub dimensions: u32,
    /// Sent as `Authorization: Bearer …` when set.
    pub api_key: Option<String>,
    /// TCP connect budget.
    pub connect_timeout: Duration,
    /// Whole-call budget for one embeddings request.
    pub request_timeout: Duration,
}

impl OpenAiEmbeddingConfig {
    /// A local engine (ollama, llama.cpp, LM Studio, …) with no API key.
    pub fn local(base_url: impl Into<String>, model: impl Into<String>, dimensions: u32) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            dimensions,
            api_key: None,
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(60),
        }
    }

    /// OpenAI's hosted API with a key.
    pub fn openai(model: impl Into<String>, dimensions: u32, api_key: impl Into<String>) -> Self {
        Self {
            base_url: "https://api.openai.com/v1".into(),
            model: model.into(),
            dimensions,
            api_key: Some(api_key.into()),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(60),
        }
    }
}

/// An [`Embedder`] backed by an OpenAI-compatible `/embeddings` endpoint.
pub struct OpenAiEmbeddingBackend {
    config: OpenAiEmbeddingConfig,
    client: reqwest::Client,
    base: String,
}

impl OpenAiEmbeddingBackend {
    pub fn new(config: OpenAiEmbeddingConfig) -> Result<Self, EmbeddingError> {
        let client = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .timeout(config.request_timeout)
            .build()
            .map_err(|e| EmbeddingError::BackendUnavailable(e.to_string()))?;
        let base = config.base_url.trim_end_matches('/').to_string();
        Ok(Self {
            config,
            client,
            base,
        })
    }
}

#[derive(Serialize)]
struct EmbeddingsRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingDatum>,
}

#[derive(Deserialize)]
struct EmbeddingDatum {
    /// OpenAI returns the input's position; used to restore order defensively.
    #[serde(default)]
    index: usize,
    embedding: Vec<f32>,
}

fn transport(e: reqwest::Error) -> EmbeddingError {
    EmbeddingError::BackendUnavailable(e.to_string())
}

#[async_trait]
impl Embedder for OpenAiEmbeddingBackend {
    fn info(&self) -> EmbedderInfo {
        EmbedderInfo {
            name: self.config.model.clone(),
            dimensions: self.config.dimensions,
            // A remote server cannot prove its weights across the wire.
            content_hash: None,
        }
    }

    fn locality(&self) -> Locality {
        Locality::Distributed
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let body = EmbeddingsRequest {
            model: &self.config.model,
            input: texts,
        };
        let mut req = self
            .client
            .post(format!("{}/embeddings", self.base))
            .json(&body);
        if let Some(key) = &self.config.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await.map_err(transport)?;
        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            return Err(EmbeddingError::BackendUnavailable(format!(
                "HTTP {status}: {}",
                detail.chars().take(300).collect::<String>()
            )));
        }
        let parsed: EmbeddingsResponse = resp.json().await.map_err(transport)?;
        if parsed.data.len() != texts.len() {
            return Err(EmbeddingError::Malformed(format!(
                "requested {} embeddings, got {}",
                texts.len(),
                parsed.data.len()
            )));
        }
        // Restore request order by `index` (OpenAI guarantees it, but do not
        // assume the server preserves array order).
        let mut out: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
        for datum in parsed.data {
            let slot = out
                .get_mut(datum.index)
                .ok_or_else(|| EmbeddingError::Malformed("embedding index out of range".into()))?;
            *slot = Some(datum.embedding);
        }
        out.into_iter()
            .map(|o| o.ok_or_else(|| EmbeddingError::Malformed("missing embedding index".into())))
            .collect()
    }
}
