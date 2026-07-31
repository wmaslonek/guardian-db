//! RAG over a GuardianDB SQL table (RFC 0005 phase 3).
//!
//! Ties phases 1–3 together, fully offline: a `HashEmbedder` embeds a small
//! corpus and the query, the phase-1 vector search retrieves the nearest rows,
//! and a local echo "LLM" stands in for a real backend so the demo needs no
//! model download or network. Swap `HashEmbedder` for `OpenAiEmbeddingBackend`
//! / `OnnxEmbeddingBackend` and the echo backend for `OpenAiHttpBackend` to
//! run it for real.
//!
//!   cargo run --release --features rag --example rag_demo

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use guardian_db::compute::llm::{
    FinishReason, GenerateEvent, GenerateRequest, GenerateStream, LlmBackend, LlmError,
    LlmRegistry, LlmRouter, Locality, ModelInfo, ProbeConfig, Role, RouterConfig, Usage,
};
use guardian_db::embedding::{Embedder, HashEmbedder, Rag, RagConfig};
use guardian_db::sql::MemoryStorage;
use guardian_db::sql::engine::{Database, Session};

const DIMS: u32 = 32;
const MODEL: &str = "demo-embed";

/// Stand-in LLM: returns a canned answer that quotes the grounded prompt, so
/// the demo shows the retrieved context reaching the model.
struct EchoLlm;

#[async_trait]
impl LlmBackend for EchoLlm {
    async fn models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        Ok(vec![ModelInfo {
            name: "demo-llm".into(),
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
        let answer = format!(
            "[echo-llm] a real model would answer here, grounded in this prompt:\n\n{prompt}"
        );
        Ok(futures::stream::iter(vec![
            Ok(GenerateEvent::Delta { content: answer }),
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

#[tokio::main]
async fn main() {
    let db = Arc::new(Database::new(Arc::new(MemoryStorage::new()), "app"));
    let mut s = Session::new(db.clone(), "guardian");
    run(&mut s, "CREATE EXTENSION vector").await;
    run(
        &mut s,
        &format!(
            "CREATE TABLE notes (id INT PRIMARY KEY, body TEXT, \
             embedding vector({DIMS}), _embedding_srchash TEXT)"
        ),
    )
    .await;

    let embedder = Arc::new(HashEmbedder::new(MODEL, DIMS));
    let corpus = [
        "GuardianDB stores replicate over iroh using CRDT last-writer-wins.",
        "The HNSW vector index answers ORDER BY embedding <=> query LIMIT k.",
        "Auto-embedding writes vectors back into a column off the change feed.",
        "Compute delegation routes embeddings to a GPU peer over COMPUTE_ALPN.",
        "The SQL surface declares rules with the guardian_embed function.",
    ];
    for (i, text) in corpus.iter().enumerate() {
        let v = embedder.embed(&[text.to_string()]).await.unwrap();
        let lit = format!(
            "[{}]",
            v[0].iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        run(
            &mut s,
            &format!("INSERT INTO notes VALUES ({i}, '{text}', '{lit}', 'x')"),
        )
        .await;
    }
    // Declare the rule so the RAG helper can verify query/corpus model match.
    run(
        &mut s,
        &format!("SELECT guardian_embed('notes', 'body', 'embedding', '{MODEL}')"),
    )
    .await;
    println!("seeded {} notes\n", corpus.len());

    // Wire the LLM router with the echo backend.
    let registry = Arc::new(LlmRegistry::new(ProbeConfig::default()));
    registry.register_backend("echo", Arc::new(EchoLlm));
    registry.probe_backend("echo").await;
    let router = Arc::new(LlmRouter::new(registry, RouterConfig::default()));

    let rag = Rag::new(
        db.clone(),
        embedder,
        router,
        RagConfig::new("notes", "body", "embedding", "demo-llm")
            .with_id_column("id")
            .with_k(3),
    )
    .await
    .expect("model-consistency check passes");

    let question = "How does the vector index serve nearest-neighbour queries?";
    println!("Q: {question}\n");

    println!("-- retrieved --");
    for hit in rag.retrieve(question).await.unwrap() {
        println!(
            "  [{}] d={:.4}  {}",
            hit.id.as_deref().unwrap_or("?"),
            hit.distance,
            hit.text
        );
    }

    println!("\n-- answer --");
    let mut stream = rag.answer(question).await.unwrap();
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            GenerateEvent::Delta { content } => print!("{content}"),
            GenerateEvent::End { .. } => println!(),
        }
    }
}

async fn run(s: &mut Session<MemoryStorage>, sql: &str) {
    s.execute(sql)
        .await
        .unwrap_or_else(|e| panic!("`{sql}`: {e}"));
}
