//! # Guardian Compute LLM demo (RFC 0004)
//!
//! Registers an OpenAI-compatible backend, health-probes it, and streams one
//! chat completion through the placement router — the local → peers →
//! distributed decision of RFC 0004 §3.4 (peers need a running GuardianDB
//! network, so this demo exercises tiers 1 and 3).
//!
//! Point it at any OpenAI-compatible server:
//!
//! ```text
//! # mesh-llm (default): distributed inference over iroh
//! mesh-llm serve --auto
//! cargo run --example llm_inference_demo --features compute-llm
//!
//! # colibri's server, llama.cpp, ollama, vLLM…
//! LLM_BASE_URL=http://127.0.0.1:8000/v1 \
//! LLM_LOCALITY=local \
//! cargo run --example llm_inference_demo --features compute-llm
//!
//! # pick a model and prompt
//! LLM_MODEL=GLM-4.7-Flash LLM_PROMPT="explique iroh em uma frase" ...
//! ```

use std::sync::Arc;

use futures::StreamExt;
use guardian_db::compute::llm::{ChatMessage, GenerateEvent, GenerateRequest, SamplingParams};
use guardian_db::compute::{LlmRegistry, LlmRouter, OpenAiConfig, OpenAiHttpBackend, RouterConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // ── 1. One backend behind the OpenAI dialect ────────────────────────────
    let base_url =
        std::env::var("LLM_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:9337/v1".into());
    let config = match std::env::var("LLM_LOCALITY").as_deref() {
        // A single-machine engine (colibri's server, llama.cpp, ollama…).
        Ok("local") => OpenAiConfig::local(&base_url),
        // Default shape: mesh-llm — the mesh splits work across peers, this
        // node just talks to the local API (Locality::Distributed).
        _ => OpenAiConfig {
            base_url: base_url.clone(),
            ..OpenAiConfig::mesh_llm()
        },
    };
    let backend = Arc::new(OpenAiHttpBackend::new(config)?);

    // ── 2. Owner-curated registry + active health-checking (§6.1) ──────────
    let registry = Arc::new(LlmRegistry::default());
    registry.register_backend("demo", backend);
    // Registration alone advertises nothing: the first successful probe does.
    registry.probe_once().await;
    let _prober = registry.spawn_prober(); // §6.1 loop; stops with the registry

    let models = registry.advertised_model_names();
    if models.is_empty() {
        eprintln!("nenhum modelo anunciado — há um servidor em {base_url}?");
        eprintln!("  mesh-llm:  mesh-llm serve --auto");
        eprintln!("  colibri:   coli serve   (e LLM_BASE_URL/LLM_LOCALITY=local)");
        std::process::exit(1);
    }
    println!("modelos anunciados (CapabilityVector `llm_models`): {models:?}");

    // ── 3. Route one generation and stream the tokens ──────────────────────
    let model = std::env::var("LLM_MODEL").unwrap_or_else(|_| models[0].clone());
    let prompt = std::env::var("LLM_PROMPT")
        .unwrap_or_else(|_| "Explique em uma frase o que é um banco de dados P2P.".into());
    println!("modelo: {model}\nprompt: {prompt}\n---");

    // In a full node, `.with_peers(PeerDelegation { .. })` adds tier 2 —
    // delegation to peers advertising the model over COMPUTE_ALPN.
    let router = LlmRouter::new(registry.clone(), RouterConfig::default());
    let request = GenerateRequest {
        model: model.into(),
        messages: vec![
            ChatMessage::system("Responda em português, de forma direta."),
            ChatMessage::user(prompt),
        ],
        params: SamplingParams {
            max_tokens: Some(256),
            ..SamplingParams::default()
        },
        deadline: Some(std::time::Duration::from_secs(120)),
    };

    let mut stream = router.route(request).await?;
    while let Some(event) = stream.next().await {
        match event? {
            GenerateEvent::Delta { content } => {
                use std::io::Write;
                print!("{content}");
                std::io::stdout().flush().ok();
            }
            GenerateEvent::End {
                finish_reason,
                usage,
            } => {
                println!(
                    "\n---\nfim: {finish_reason:?} · {} tokens de prompt, {} gerados",
                    usage.prompt_tokens, usage.completion_tokens
                );
            }
        }
    }
    // Backends of Locality::Local are preferred by the router; had a colibri
    // driver been registered (feature `compute-llm-colibri`), tier 1 would
    // have won:
    //
    //   let coli = ColibriProcessBackend::spawn(
    //       ColibriConfig::new("./coli", "./model.snap")).await?;
    //   registry.register_backend("colibri", coli);
    Ok(())
}
