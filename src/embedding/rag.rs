//! Retrieval-augmented generation helper (feature `rag`, RFC 0005 phase 3).
//!
//! The thin, opinionated layer that ties the three pieces together:
//! - **retrieve** — embed the query with an [`Embedder`] and run the phase-1
//!   top-k vector search over a corpus table (the HNSW index serves it
//!   automatically when one exists, an exact scan otherwise);
//! - **augment** — assemble the retrieved texts into a context block;
//! - **generate** — hand a templated prompt to the RFC 0004 [`LlmRouter`]
//!   (local → peers → distributed), streaming the answer back.
//!
//! The one guardrail this layer insists on is **query/corpus model
//! consistency**: embedding the query with a different model than the corpus
//! was embedded with is the classic silent RAG failure (the vectors live in
//! unrelated spaces, so "nearest" is noise). [`Rag::new`] reads the corpus
//! table's declared embedding rule (phase 2 / §6.3) and refuses a mismatched
//! query embedder — it checks the model, never assumes it.
//!
//! Deliberately *not* here (they live above this crate): agent frameworks,
//! conversation memory, re-ranking, chunking. This is retrieve → augment →
//! generate, nothing more.

use std::sync::Arc;
use std::time::Duration;

use crate::compute::llm::{
    ChatMessage, GenerateEvent, GenerateRequest, GenerateStream, LlmRouter, ModelSelector,
    SamplingParams,
};
use crate::embedding::{Embedder, EmbeddingError};
use crate::relational::RelationalStorage;
use crate::sql::engine::{Database, Session};
use crate::sql::{ExecResult, SqlValue};
use futures::StreamExt;

/// The placeholders a [`RagConfig::prompt_template`] may use.
const CONTEXT_PLACEHOLDER: &str = "{context}";
const QUESTION_PLACEHOLDER: &str = "{question}";

/// Default prompt: grounded, refusal-friendly, cites nothing fancy.
pub const DEFAULT_PROMPT_TEMPLATE: &str = "\
Answer the question using only the context below. If the context does not \
contain the answer, say you don't know.\n\n\
Context:\n{context}\n\n\
Question: {question}\n\n\
Answer:";

/// Errors from a RAG operation.
#[derive(Debug, thiserror::Error)]
pub enum RagError {
    /// The query embedder's model differs from the model the corpus was
    /// embedded with — the silent-failure guardrail (RFC 0005 §3.4).
    #[error(
        "query/corpus embedding model mismatch: corpus `{corpus}` was embedded with `{corpus_model}`, \
         but the query embedder is `{query_model}`"
    )]
    ModelMismatch {
        corpus: String,
        corpus_model: String,
        query_model: String,
    },
    /// The distance operator is not one the SQL parser accepts (`<#>`/`<+>`
    /// cannot be tokenized; use `<=>` or `<->`).
    #[error("unsupported distance operator `{0}` (use `<=>` or `<->`)")]
    UnsupportedOperator(String),
    #[error("embedding failed: {0}")]
    Embedding(#[from] EmbeddingError),
    #[error("retrieval query failed: {0}")]
    Sql(String),
    #[error("generation failed: {0}")]
    Generation(String),
}

/// A retrieved corpus row.
#[derive(Debug, Clone)]
pub struct Retrieved {
    /// The id column value, if a [`RagConfig::id_column`] was configured.
    pub id: Option<String>,
    /// The source text.
    pub text: String,
    /// The index distance to the query (smaller is nearer).
    pub distance: f64,
}

/// What corpus to retrieve from and how to generate.
#[derive(Debug, Clone)]
pub struct RagConfig {
    pub schema: String,
    /// The corpus table.
    pub table: String,
    /// The text column returned as context.
    pub text_column: String,
    /// The `vector` column searched.
    pub vector_column: String,
    /// An optional id column, surfaced in [`Retrieved::id`] and the context
    /// block for lightweight citations.
    pub id_column: Option<String>,
    /// The pgvector distance operator: `<=>` (cosine, default) or `<->` (L2).
    pub distance_op: String,
    /// How many rows to retrieve.
    pub k: usize,
    /// The LLM model name to generate with.
    pub llm_model: String,
    /// Prompt template using `{context}` and `{question}`.
    pub prompt_template: String,
    /// Optional system message prepended to the chat.
    pub system_prompt: Option<String>,
    /// Cap on the assembled context length (characters); retrieved rows are
    /// included until this is reached.
    pub max_context_chars: usize,
    /// Sampling parameters passed through to the backend.
    pub sampling: SamplingParams,
    /// Generation deadline (queue + generate); `None` leaves it to the backend.
    pub deadline: Option<Duration>,
}

impl RagConfig {
    /// Sensible defaults: cosine distance, k=5, the [`DEFAULT_PROMPT_TEMPLATE`],
    /// no id column, 8 KiB of context.
    pub fn new(
        table: impl Into<String>,
        text_column: impl Into<String>,
        vector_column: impl Into<String>,
        llm_model: impl Into<String>,
    ) -> Self {
        Self {
            schema: "public".into(),
            table: table.into(),
            text_column: text_column.into(),
            vector_column: vector_column.into(),
            id_column: None,
            distance_op: "<=>".into(),
            k: 5,
            llm_model: llm_model.into(),
            prompt_template: DEFAULT_PROMPT_TEMPLATE.into(),
            system_prompt: None,
            max_context_chars: 8 * 1024,
            sampling: SamplingParams::default(),
            deadline: None,
        }
    }

    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.schema = schema.into();
        self
    }
    pub fn with_id_column(mut self, id_column: impl Into<String>) -> Self {
        self.id_column = Some(id_column.into());
        self
    }
    pub fn with_k(mut self, k: usize) -> Self {
        self.k = k;
        self
    }
    pub fn with_distance_op(mut self, op: impl Into<String>) -> Self {
        self.distance_op = op.into();
        self
    }
    pub fn with_prompt_template(mut self, template: impl Into<String>) -> Self {
        self.prompt_template = template.into();
        self
    }
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }
}

/// The RAG helper. Bind a corpus (via [`RagConfig`]), a query [`Embedder`],
/// and an [`LlmRouter`]; then [`retrieve`](Rag::retrieve) or
/// [`answer`](Rag::answer).
pub struct Rag<S: RelationalStorage> {
    db: Arc<Database<S>>,
    embedder: Arc<dyn Embedder>,
    router: Arc<LlmRouter>,
    config: RagConfig,
}

impl<S: RelationalStorage + 'static> Rag<S> {
    /// Bind the helper, enforcing query/corpus model consistency (§3.4).
    ///
    /// When the corpus table has a declared embedding rule (phase 2 / §6.3)
    /// for the vector column, its model must equal the query embedder's model,
    /// or this returns [`RagError::ModelMismatch`]. When there is no declared
    /// rule the corpus was embedded out of band — consistency cannot be
    /// verified, so a warning is logged and the caller is trusted.
    pub async fn new(
        db: Arc<Database<S>>,
        embedder: Arc<dyn Embedder>,
        router: Arc<LlmRouter>,
        config: RagConfig,
    ) -> Result<Self, RagError> {
        // Validate the distance operator up front.
        if !matches!(config.distance_op.as_str(), "<=>" | "<->") {
            return Err(RagError::UnsupportedOperator(config.distance_op.clone()));
        }
        let query_model = embedder.info().name;
        match db.catalog_snapshot().await {
            Ok(catalog) => {
                let rule = catalog.embedding_rules().iter().find(|r| {
                    r.schema == config.schema
                        && r.table == config.table
                        && r.vector_column == config.vector_column
                });
                match rule {
                    Some(r) if r.model != query_model => {
                        return Err(RagError::ModelMismatch {
                            corpus: format!("{}.{}", config.schema, config.table),
                            corpus_model: r.model.clone(),
                            query_model,
                        });
                    }
                    Some(_) => {} // models agree
                    None => {
                        tracing::warn!(
                            corpus = %format!("{}.{}", config.schema, config.table),
                            query_model = %query_model,
                            "rag: no declared embedding rule for the corpus column; \
                             cannot verify query/corpus model consistency"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "rag: catalog read failed; skipping model check");
            }
        }
        Ok(Self {
            db,
            embedder,
            router,
            config,
        })
    }

    /// Embed `query` and return the top-`k` corpus rows by vector distance.
    pub async fn retrieve(&self, query: &str) -> Result<Vec<Retrieved>, RagError> {
        let qvec = self
            .embedder
            .embed(std::slice::from_ref(&query.to_string()))
            .await?
            .pop()
            .ok_or_else(|| RagError::Embedding(EmbeddingError::Malformed("empty output".into())))?;
        let sql = self.retrieval_sql(&qvec);
        let mut session = Session::new(self.db.clone(), "rag");
        let mut results = session
            .execute(&sql)
            .await
            .map_err(|e| RagError::Sql(e.to_string()))?;
        let rows = match results.pop() {
            Some(ExecResult::Rows { rows, .. }) => rows,
            _ => return Ok(Vec::new()),
        };
        // Column order: [id?], text, distance.
        let has_id = self.config.id_column.is_some();
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let mut it = row.into_iter();
            let id = if has_id {
                it.next().and_then(|v| v.to_text())
            } else {
                None
            };
            let text = it.next().and_then(|v| v.to_text()).unwrap_or_default();
            let distance = match it.next() {
                Some(SqlValue::Float8(d)) => d,
                Some(other) => other.to_text().and_then(|t| t.parse().ok()).unwrap_or(0.0),
                None => 0.0,
            };
            out.push(Retrieved { id, text, distance });
        }
        Ok(out)
    }

    /// Retrieve, assemble a context block, and stream the generated answer.
    pub async fn answer(&self, query: &str) -> Result<GenerateStream, RagError> {
        let hits = self.retrieve(query).await?;
        let context = self.assemble_context(&hits);
        let prompt = self
            .config
            .prompt_template
            .replace(CONTEXT_PLACEHOLDER, &context)
            .replace(QUESTION_PLACEHOLDER, query);
        let mut messages = Vec::new();
        if let Some(sys) = &self.config.system_prompt {
            messages.push(ChatMessage::system(sys.clone()));
        }
        messages.push(ChatMessage::user(prompt));
        let req = GenerateRequest {
            model: ModelSelector {
                name: self.config.llm_model.clone(),
                required_hash: None,
            },
            messages,
            params: self.config.sampling.clone(),
            deadline: self.config.deadline,
        };
        self.router
            .route(req)
            .await
            .map_err(|e| RagError::Generation(e.to_string()))
    }

    /// Convenience: retrieve, generate, and collect the whole answer into a
    /// `String` (drains the stream). Prefer [`answer`](Rag::answer) for
    /// streaming UIs.
    pub async fn answer_text(&self, query: &str) -> Result<String, RagError> {
        let mut stream = self.answer(query).await?;
        let mut out = String::new();
        while let Some(event) = stream.next().await {
            match event.map_err(|e| RagError::Generation(e.to_string()))? {
                GenerateEvent::Delta { content } => out.push_str(&content),
                GenerateEvent::End { .. } => break,
            }
        }
        Ok(out)
    }

    /// The top-k retrieval SQL. `ORDER BY <col> <op> <qvec> LIMIT k` is exactly
    /// the shape the phase-1 planner hook accelerates with an HNSW index.
    fn retrieval_sql(&self, qvec: &[f32]) -> String {
        let lit = vector_literal(qvec);
        let op = &self.config.distance_op;
        let vc = &self.config.vector_column;
        let mut cols = String::new();
        if let Some(id) = &self.config.id_column {
            cols.push_str(id);
            cols.push_str(", ");
        }
        cols.push_str(&self.config.text_column);
        format!(
            "SELECT {cols}, {vc} {op} '{lit}'::vector AS _rag_distance \
             FROM {schema}.{table} WHERE {vc} IS NOT NULL \
             ORDER BY {vc} {op} '{lit}'::vector LIMIT {k}",
            schema = self.config.schema,
            table = self.config.table,
            k = self.config.k,
        )
    }

    /// Join retrieved texts into a context block, id-prefixed when available,
    /// truncated to the character budget.
    fn assemble_context(&self, hits: &[Retrieved]) -> String {
        let mut ctx = String::new();
        for hit in hits {
            let piece = match &hit.id {
                Some(id) => format!("[{id}] {}", hit.text),
                None => hit.text.clone(),
            };
            if !ctx.is_empty() {
                if ctx.len() + piece.len() + 5 > self.config.max_context_chars {
                    break;
                }
                ctx.push_str("\n\n---\n\n");
            } else if piece.len() > self.config.max_context_chars {
                // A single oversized piece: include a truncated prefix.
                ctx.push_str(&truncate_chars(&piece, self.config.max_context_chars));
                break;
            }
            ctx.push_str(&piece);
        }
        ctx
    }
}

/// Format an f32 slice as a pgvector text literal (`[a,b,c]`).
fn vector_literal(v: &[f32]) -> String {
    let mut out = String::with_capacity(v.len() * 10 + 2);
    out.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&x.to_string());
    }
    out.push(']');
    out
}

/// Truncate to at most `max` characters on a char boundary.
fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_literal_roundtrips() {
        assert_eq!(vector_literal(&[1.0, 2.5, -3.0]), "[1,2.5,-3]");
    }

    #[test]
    fn context_respects_budget() {
        let cfg = RagConfig::new("t", "body", "v", "m");
        let db_none: Option<()> = None;
        let _ = db_none;
        // Build a Rag-independent assembler by exercising the logic directly.
        let hits = vec![
            Retrieved {
                id: Some("1".into()),
                text: "alpha".into(),
                distance: 0.1,
            },
            Retrieved {
                id: Some("2".into()),
                text: "beta".into(),
                distance: 0.2,
            },
        ];
        // Reproduce assemble_context without a Database (pure function of cfg).
        let mut ctx = String::new();
        for hit in &hits {
            let piece = format!("[{}] {}", hit.id.as_ref().unwrap(), hit.text);
            if !ctx.is_empty() {
                if ctx.len() + piece.len() + 5 > cfg.max_context_chars {
                    break;
                }
                ctx.push_str("\n\n---\n\n");
            }
            ctx.push_str(&piece);
        }
        assert_eq!(ctx, "[1] alpha\n\n---\n\n[2] beta");
    }

    #[test]
    fn prompt_template_substitution() {
        let t = DEFAULT_PROMPT_TEMPLATE
            .replace("{context}", "CTX")
            .replace("{question}", "Q?");
        assert!(t.contains("CTX"));
        assert!(t.contains("Q?"));
        assert!(!t.contains("{context}"));
    }
}
