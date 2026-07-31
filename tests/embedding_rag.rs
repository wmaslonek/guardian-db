#![cfg(feature = "rag")]
//! End-to-end tests for the RAG helper (RFC 0005 phase 3): embed the query,
//! run the phase-1 vector search over a corpus, assemble context, and generate
//! through a mock LLM backend — plus the query/corpus model-consistency guard.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use guardian_db::compute::llm::{
    FinishReason, GenerateEvent, GenerateRequest, GenerateStream, LlmBackend, LlmError,
    LlmRegistry, LlmRouter, Locality, ModelInfo, ProbeConfig, Role, RouterConfig, Usage,
};
use guardian_db::embedding::{Embedder, HashEmbedder, Rag, RagConfig, RagError};
use guardian_db::sql::MemoryStorage;
use guardian_db::sql::engine::{Database, Session};

const DIMS: u32 = 16;

/// A mock LLM backend that echoes the last user message back as the answer —
/// so a test can assert the retrieved context reached the prompt.
struct EchoBackend {
    model: String,
}

#[async_trait]
impl LlmBackend for EchoBackend {
    async fn models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        Ok(vec![ModelInfo {
            name: self.model.clone(),
            content_hash: None,
        }])
    }
    async fn generate(&self, req: GenerateRequest) -> Result<GenerateStream, LlmError> {
        let prompt = req
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .map(|m| m.content.clone())
            .unwrap_or_default();
        Ok(futures::stream::iter(vec![
            Ok(GenerateEvent::Delta { content: prompt }),
            Ok(GenerateEvent::End {
                finish_reason: FinishReason::Stop,
                usage: Usage::default(),
            }),
        ])
        .boxed())
    }
    fn locality(&self) -> Locality {
        Locality::Local
    }
}

async fn ok(s: &mut Session<MemoryStorage>, sql: &str) {
    s.execute(sql)
        .await
        .unwrap_or_else(|e| panic!("`{sql}`: {e}"));
}

/// A corpus of 5 texts, each embedded with `HashEmbedder` and inserted as a
/// vector literal, plus a declared embedding rule naming the model.
async fn corpus(model: &str) -> Arc<Database<MemoryStorage>> {
    let db = Arc::new(Database::new(Arc::new(MemoryStorage::new()), "app"));
    let mut s = Session::new(db.clone(), "guardian");
    ok(&mut s, "CREATE EXTENSION vector").await;
    ok(
        &mut s,
        &format!(
            "CREATE TABLE docs (id INT PRIMARY KEY, body TEXT, \
             embedding vector({DIMS}), _embedding_srchash TEXT)"
        ),
    )
    .await;
    let embedder = HashEmbedder::new(model, DIMS);
    let texts = [
        "the cat sat on the mat",
        "rust is a systems programming language",
        "the quick brown fox jumps",
        "vector databases enable semantic search",
        "guardian db replicates over iroh",
    ];
    for (i, t) in texts.iter().enumerate() {
        let v = embedder
            .embed(&[t.to_string()])
            .await
            .unwrap()
            .pop()
            .unwrap();
        let lit = format!(
            "[{}]",
            v.iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        ok(
            &mut s,
            &format!("INSERT INTO docs VALUES ({i}, '{t}', '{lit}', 'x')"),
        )
        .await;
    }
    // Declare the embedding rule so the RAG helper can verify the model.
    ok(
        &mut s,
        &format!("SELECT guardian_embed('docs', 'body', 'embedding', '{model}')"),
    )
    .await;
    db
}

/// A router with no backends (used by tests that never generate).
fn router() -> Arc<LlmRouter> {
    Arc::new(LlmRouter::new(
        Arc::new(LlmRegistry::new(ProbeConfig::default())),
        RouterConfig::default(),
    ))
}

async fn router_with_echo(model: &str) -> Arc<LlmRouter> {
    let registry = Arc::new(LlmRegistry::new(ProbeConfig::default()));
    registry.register_backend(
        "echo",
        Arc::new(EchoBackend {
            model: model.into(),
        }),
    );
    // Activate it (advertised models come from the first successful probe).
    registry.probe_backend("echo").await;
    Arc::new(LlmRouter::new(registry, RouterConfig::default()))
}

#[tokio::test]
async fn retrieve_returns_nearest_documents() {
    let db = corpus("test-embed").await;
    let embedder = Arc::new(HashEmbedder::new("test-embed", DIMS));
    let rag = Rag::new(
        db,
        embedder,
        router(),
        RagConfig::new("docs", "body", "embedding", "llm")
            .with_id_column("id")
            .with_k(3),
    )
    .await
    .expect("rag construct");

    // A query identical to a stored text embeds to that exact vector → rank 1.
    let hits = rag
        .retrieve("vector databases enable semantic search")
        .await
        .unwrap();
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].text, "vector databases enable semantic search");
    assert_eq!(hits[0].id.as_deref(), Some("3"));
}

#[tokio::test]
async fn answer_grounds_the_prompt_in_retrieved_context() {
    let db = corpus("test-embed").await;
    let embedder = Arc::new(HashEmbedder::new("test-embed", DIMS));
    let rag = Rag::new(
        db,
        embedder,
        router_with_echo("llm").await,
        RagConfig::new("docs", "body", "embedding", "llm").with_k(2),
    )
    .await
    .expect("rag construct");

    // The echo backend returns the prompt; it must contain the retrieved text
    // and the question.
    let answer = rag
        .answer_text("guardian db replicates over iroh")
        .await
        .unwrap();
    assert!(
        answer.contains("guardian db replicates over iroh"),
        "context/question missing from prompt: {answer}"
    );
    assert!(answer.contains("Context:"), "template applied: {answer}");
}

#[tokio::test]
async fn model_mismatch_is_refused() {
    let db = corpus("corpus-model").await;
    // The query embedder names a *different* model than the corpus rule.
    let embedder = Arc::new(HashEmbedder::new("other-model", DIMS));
    let result = Rag::new(
        db,
        embedder,
        router(),
        RagConfig::new("docs", "body", "embedding", "llm"),
    )
    .await;
    assert!(
        matches!(result, Err(RagError::ModelMismatch { .. })),
        "expected the model-consistency guard to fire"
    );
}

#[tokio::test]
async fn matching_model_is_accepted() {
    let db = corpus("corpus-model").await;
    let embedder = Arc::new(HashEmbedder::new("corpus-model", DIMS));
    assert!(
        Rag::new(
            db,
            embedder,
            router(),
            RagConfig::new("docs", "body", "embedding", "llm"),
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn unsupported_operator_is_rejected() {
    let db = corpus("test-embed").await;
    let embedder = Arc::new(HashEmbedder::new("test-embed", DIMS));
    let result = Rag::new(
        db,
        embedder,
        router(),
        RagConfig::new("docs", "body", "embedding", "llm").with_distance_op("<#>"),
    )
    .await;
    assert!(matches!(result, Err(RagError::UnsupportedOperator(_))));
}
