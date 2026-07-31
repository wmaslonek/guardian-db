//! Host-side ONNX embedding backend (feature `embedding-onnx`).
//!
//! Runs a local sentence-embedding model with ONNX Runtime ([`ort`]) and a
//! HuggingFace tokenizer ([`tokenizers`]) — the self-contained backend that
//! needs no external service. The pipeline is the standard sentence-
//! transformer shape: tokenize → run the transformer → **mean-pool** the last
//! hidden state weighted by the attention mask → **L2-normalize**.
//!
//! Unlike the HTTP backend, this one reads the weight bytes, so it advertises
//! a real BLAKE3 [`content_hash`](super::EmbedderInfo::content_hash) and can
//! honor a §6.3 pin — the one embedding backend whose identity is verifiable.
//!
//! The model and tokenizer are the usual `model.onnx` + `tokenizer.json` pair
//! exported by `optimum`/`sentence-transformers`. The model must take the
//! `input_ids` / `attention_mask` (and optional `token_type_ids`) inputs and
//! produce a `last_hidden_state` (or single) output — the near-universal BERT
//! family shape.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use iroh_blobs::Hash;
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::Tokenizer;

use super::{Embedder, EmbedderInfo, EmbeddingError, Locality};

/// Pooling strategy over the transformer's token outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pooling {
    /// Mean of token vectors weighted by the attention mask (sentence-
    /// transformers default).
    Mean,
    /// The `[CLS]` token vector (position 0).
    Cls,
}

/// Configuration for one [`OnnxEmbeddingBackend`].
#[derive(Debug, Clone)]
pub struct OnnxEmbeddingConfig {
    /// The model name callers select by.
    pub model: String,
    /// Path to the ONNX model file.
    pub model_path: PathBuf,
    /// Path to the `tokenizer.json`.
    pub tokenizer_path: PathBuf,
    /// Output vector dimension (the model's hidden size).
    pub dimensions: u32,
    /// Pooling strategy (default [`Pooling::Mean`]).
    pub pooling: Pooling,
    /// L2-normalize outputs (default true; sentence-transformers do).
    pub normalize: bool,
    /// Truncate token sequences to at most this length (0 = the model's
    /// default / no explicit cap).
    pub max_tokens: usize,
}

impl OnnxEmbeddingConfig {
    pub fn new(
        model: impl Into<String>,
        model_path: impl Into<PathBuf>,
        tokenizer_path: impl Into<PathBuf>,
        dimensions: u32,
    ) -> Self {
        Self {
            model: model.into(),
            model_path: model_path.into(),
            tokenizer_path: tokenizer_path.into(),
            dimensions,
            pooling: Pooling::Mean,
            normalize: true,
            max_tokens: 256,
        }
    }
}

/// An [`Embedder`] running a local ONNX model. The `ort` session is single-
/// threaded per call (`run` takes `&mut`), so it is behind a `Mutex`; batch
/// embedding amortizes the lock over the whole batch.
pub struct OnnxEmbeddingBackend {
    config: OnnxEmbeddingConfig,
    tokenizer: Tokenizer,
    session: Mutex<Session>,
    content_hash: Hash,
}

impl OnnxEmbeddingBackend {
    /// Load the model + tokenizer from disk.
    pub fn load(config: OnnxEmbeddingConfig) -> Result<Self, EmbeddingError> {
        let bytes = std::fs::read(&config.model_path).map_err(|e| {
            EmbeddingError::BackendUnavailable(format!(
                "reading model {}: {e}",
                config.model_path.display()
            ))
        })?;
        let content_hash = Hash::from_bytes(*blake3::hash(&bytes).as_bytes());
        let session = Session::builder()
            .and_then(|b| b.commit_from_memory(&bytes))
            .map_err(|e| EmbeddingError::BackendUnavailable(format!("loading ONNX model: {e}")))?;
        let tokenizer = Tokenizer::from_file(&config.tokenizer_path)
            .map_err(|e| EmbeddingError::BackendUnavailable(format!("loading tokenizer: {e}")))?;
        Ok(Self {
            config,
            tokenizer,
            session: Mutex::new(session),
            content_hash,
        })
    }

    /// The names of the model's declared inputs (so we only feed
    /// `token_type_ids` when the model actually wants it).
    fn input_names(session: &Session) -> Vec<String> {
        session.inputs.iter().map(|i| i.name.clone()).collect()
    }

    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| EmbeddingError::Malformed(format!("tokenization failed: {e}")))?;

        // Pad to a common length so the batch is one rectangular tensor.
        let cap = self.config.max_tokens;
        let mut seq_len = encodings
            .iter()
            .map(|e| e.get_ids().len())
            .max()
            .unwrap_or(1)
            .max(1);
        if cap > 0 {
            seq_len = seq_len.min(cap);
        }
        let batch = texts.len();

        let mut input_ids = vec![0i64; batch * seq_len];
        let mut attention = vec![0i64; batch * seq_len];
        let mut type_ids = vec![0i64; batch * seq_len];
        for (b, enc) in encodings.iter().enumerate() {
            let ids = enc.get_ids();
            let mask = enc.get_attention_mask();
            let types = enc.get_type_ids();
            let take = ids.len().min(seq_len);
            for t in 0..take {
                input_ids[b * seq_len + t] = ids[t] as i64;
                attention[b * seq_len + t] = mask[t] as i64;
                type_ids[b * seq_len + t] = *types.get(t).unwrap_or(&0) as i64;
            }
        }

        let shape = [batch, seq_len];
        let mut session = self.session.lock().unwrap();
        let wants_types = Self::input_names(&session)
            .iter()
            .any(|n| n == "token_type_ids");

        let ids_t =
            Tensor::from_array((shape, input_ids.clone().into_boxed_slice())).map_err(map_ort)?;
        let mask_t =
            Tensor::from_array((shape, attention.clone().into_boxed_slice())).map_err(map_ort)?;

        let outputs = if wants_types {
            let types_t =
                Tensor::from_array((shape, type_ids.into_boxed_slice())).map_err(map_ort)?;
            session
                .run(ort::inputs![
                    "input_ids" => ids_t,
                    "attention_mask" => mask_t,
                    "token_type_ids" => types_t,
                ])
                .map_err(map_ort)?
        } else {
            session
                .run(ort::inputs![
                    "input_ids" => ids_t,
                    "attention_mask" => mask_t,
                ])
                .map_err(map_ort)?
        };

        // Take the first output (last_hidden_state), shape [batch, seq, hidden].
        let (_name, first) = outputs
            .iter()
            .next()
            .ok_or_else(|| EmbeddingError::Malformed("model produced no output".into()))?;
        let (out_shape, data) = first.try_extract_tensor::<f32>().map_err(map_ort)?;
        let dims: Vec<usize> = out_shape.iter().map(|&d| d as usize).collect();
        if dims.len() != 3 || dims[0] != batch || dims[1] != seq_len {
            return Err(EmbeddingError::Malformed(format!(
                "unexpected model output shape {dims:?} (expected [{batch}, {seq_len}, hidden])"
            )));
        }
        let hidden = dims[2];
        if hidden != self.config.dimensions as usize {
            return Err(EmbeddingError::DimensionMismatch {
                got: hidden,
                expected: self.config.dimensions as usize,
            });
        }

        let mut out = Vec::with_capacity(batch);
        for b in 0..batch {
            let token_vecs = &data[b * seq_len * hidden..(b + 1) * seq_len * hidden];
            let mask = &attention[b * seq_len..(b + 1) * seq_len];
            let mut v = match self.config.pooling {
                Pooling::Mean => mean_pool(token_vecs, mask, hidden),
                Pooling::Cls => token_vecs[..hidden].to_vec(),
            };
            if self.config.normalize {
                l2_normalize(&mut v);
            }
            out.push(v);
        }
        Ok(out)
    }
}

fn map_ort(e: ort::Error) -> EmbeddingError {
    EmbeddingError::BackendUnavailable(format!("onnx: {e}"))
}

/// Attention-weighted mean of `seq_len` token vectors of width `hidden`.
/// Tokens whose mask is 0 (padding) are excluded; an all-padding row yields
/// a zero vector.
fn mean_pool(token_vecs: &[f32], mask: &[i64], hidden: usize) -> Vec<f32> {
    let mut sum = vec![0f32; hidden];
    let mut count = 0f32;
    for (t, &m) in mask.iter().enumerate() {
        if m == 0 {
            continue;
        }
        count += 1.0;
        let row = &token_vecs[t * hidden..(t + 1) * hidden];
        for (s, x) in sum.iter_mut().zip(row) {
            *s += *x;
        }
    }
    if count > 0.0 {
        for s in &mut sum {
            *s /= count;
        }
    }
    sum
}

/// In-place L2 normalization; a zero vector is left unchanged.
fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v {
            *x /= norm;
        }
    }
}

#[async_trait]
impl Embedder for OnnxEmbeddingBackend {
    fn info(&self) -> EmbedderInfo {
        EmbedderInfo {
            name: self.config.model.clone(),
            dimensions: self.config.dimensions,
            // Local weights are readable, so identity is verifiable (§6.3).
            content_hash: Some(self.content_hash),
        }
    }

    fn locality(&self) -> Locality {
        Locality::Local
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        // ONNX Runtime is blocking; keep it off the async executor.
        let texts = texts.to_vec();
        let this = SendPtr(self as *const Self);
        tokio::task::block_in_place(move || {
            // Safety: `this` is only used within this synchronous closure,
            // which cannot outlive `&self`'s borrow of the call.
            let backend = unsafe { &*this.0 };
            backend.embed_batch(&texts)
        })
    }
}

/// A `Send` wrapper for the `*const Self` handed to `block_in_place`. Sound
/// because the closure runs synchronously within the borrow of `&self`.
struct SendPtr(*const OnnxEmbeddingBackend);
unsafe impl Send for SendPtr {}

impl OnnxEmbeddingBackend {
    /// Hash a model file the same way [`load`](Self::load) does, without
    /// loading it — for a caller that wants to pin before constructing.
    pub fn hash_model_file(path: &Path) -> std::io::Result<Hash> {
        let bytes = std::fs::read(path)?;
        Ok(Hash::from_bytes(*blake3::hash(&bytes).as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_pool_excludes_padding() {
        // 2 tokens, hidden 2. Second token is padding (mask 0).
        let token_vecs = [1.0, 2.0, 100.0, 100.0];
        let mask = [1i64, 0];
        let pooled = mean_pool(&token_vecs, &mask, 2);
        assert_eq!(pooled, vec![1.0, 2.0], "padding token must not contribute");
    }

    #[test]
    fn mean_pool_averages_active_tokens() {
        let token_vecs = [2.0, 0.0, 4.0, 0.0];
        let mask = [1i64, 1];
        let pooled = mean_pool(&token_vecs, &mask, 2);
        assert_eq!(pooled, vec![3.0, 0.0]);
    }

    #[test]
    fn all_padding_yields_zero_vector() {
        let token_vecs = [1.0, 2.0];
        let mask = [0i64];
        assert_eq!(mean_pool(&token_vecs, &mask, 2), vec![0.0, 0.0]);
    }

    #[test]
    fn normalize_makes_unit_length() {
        let mut v = vec![3.0, 4.0];
        l2_normalize(&mut v);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
        l2_normalize(&mut [0.0, 0.0]); // must not divide by zero
    }
}
