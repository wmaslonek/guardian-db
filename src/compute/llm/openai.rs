//! # OpenAI-compatible HTTP backend (RFC 0004 phase 2, §3.3)
//!
//! One [`LlmBackend`] that fronts *any* OpenAI-compatible server:
//!
//! - **mesh-llm** (`http://127.0.0.1:9337/v1`) — distributed inference; the
//!   mesh does its own peer splitting, this node just talks to the local API
//!   ([`OpenAiConfig::mesh_llm`]);
//! - **colibri's server**, llama.cpp `--server`, ollama, vLLM — local
//!   engines behind the same dialect.
//!
//! `GET {base}/models` doubles as the §6.1 health probe;
//! `POST {base}/chat/completions` with `stream: true` yields SSE chunks that
//! are mapped 1:1 onto [`GenerateEvent`]s. Dropping the returned stream
//! drops the HTTP connection — that is the cancellation path (§6.4).
//!
//! HTTP backends cannot read the weight bytes, so every advertised
//! [`ModelInfo::content_hash`] is `None` and a hash-pinned request is
//! rejected with [`LlmError::HashMismatch`] before any network call
//! (§6.3: absence of a hash is explicit, never upgraded to verified).

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use super::{
    ChatMessage, FinishReason, GenerateEvent, GenerateRequest, GenerateStream, LlmBackend,
    LlmError, Locality, ModelInfo, Usage,
};

/// Configuration for one [`OpenAiHttpBackend`].
#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    /// Base URL including the version segment, e.g. `http://127.0.0.1:9337/v1`
    /// (a trailing `/` is tolerated and stripped).
    pub base_url: String,
    /// Sent as `Authorization: Bearer …` when set.
    pub api_key: Option<String>,
    /// How the registry/router should treat this backend. mesh-llm is
    /// [`Locality::Distributed`]; single-machine engines are `Local`.
    pub locality: Locality,
    /// TCP connect budget.
    pub connect_timeout: Duration,
    /// Whole-call budget for the `/models` probe (never applied to
    /// generation streams — those follow [`GenerateRequest::deadline`]).
    pub probe_timeout: Duration,
    /// Budget for the `/chat/completions` response *headers* to arrive when
    /// the request carries no deadline of its own. Generation length is
    /// unbounded; time-to-first-byte is not.
    pub first_byte_timeout: Duration,
}

impl OpenAiConfig {
    /// A local single-machine engine (colibri's server, llama.cpp, ollama…).
    pub fn local(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: None,
            locality: Locality::Local,
            connect_timeout: Duration::from_secs(5),
            probe_timeout: Duration::from_secs(10),
            first_byte_timeout: Duration::from_secs(120),
        }
    }

    /// mesh-llm's default local API (`mesh-llm serve` on this machine); the
    /// mesh behind it is what makes the backend [`Locality::Distributed`].
    pub fn mesh_llm() -> Self {
        Self {
            locality: Locality::Distributed,
            ..Self::local("http://127.0.0.1:9337/v1")
        }
    }
}

/// [`LlmBackend`] over an OpenAI-compatible HTTP API. See the module docs.
#[derive(Debug)]
pub struct OpenAiHttpBackend {
    client: reqwest::Client,
    config: OpenAiConfig,
}

impl OpenAiHttpBackend {
    pub fn new(mut config: OpenAiConfig) -> Result<Self, LlmError> {
        while config.base_url.ends_with('/') {
            config.base_url.pop();
        }
        let client = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .build()
            .map_err(|e| LlmError::Protocol(format!("HTTP client init: {e}")))?;
        Ok(Self { client, config })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut builder = self
            .client
            .request(method, format!("{}{path}", self.config.base_url));
        if let Some(key) = &self.config.api_key {
            builder = builder.bearer_auth(key);
        }
        builder
    }
}

/// Maps a transport-level reqwest error (no HTTP status yet).
fn transport_error(err: reqwest::Error) -> LlmError {
    if err.is_connect() || err.is_timeout() {
        LlmError::BackendUnavailable(err.to_string())
    } else {
        LlmError::Protocol(err.to_string())
    }
}

/// Maps a non-success HTTP status (RFC 0004 §6.4: 429 must surface as
/// queue-full so the scheduler routes around a saturated node).
async fn status_error(response: reqwest::Response, model: &str) -> LlmError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let snippet: String = body.chars().take(200).collect();
    match status.as_u16() {
        404 => LlmError::ModelNotFound(model.to_string()),
        429 => LlmError::QueueFull,
        500..=599 => LlmError::BackendUnavailable(format!("HTTP {status}: {snippet}")),
        _ => LlmError::Protocol(format!("HTTP {status}: {snippet}")),
    }
}

// ---------- wire types (OpenAI dialect) ----------

#[derive(Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
    /// Asks for a final usage chunk; servers that predate the field ignore it.
    stream_options: StreamOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stop: Vec<String>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    #[serde(default)]
    delta: ChunkDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        "length" => FinishReason::Length,
        "stop" => FinishReason::Stop,
        other => {
            tracing::debug!(reason = other, "unknown finish_reason; treating as stop");
            FinishReason::Stop
        }
    }
}

// ---------- SSE ----------

/// Incremental SSE parser: feed raw bytes, get completed event payloads
/// (the joined `data:` lines of each event). Comments and non-`data` fields
/// are ignored, per the SSE spec's client rules.
#[derive(Default)]
struct SseParser {
    buf: Vec<u8>,
    data_lines: Vec<String>,
}

impl SseParser {
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop(); // the '\n'
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line);
            if line.is_empty() {
                if !self.data_lines.is_empty() {
                    events.push(self.data_lines.join("\n"));
                    self.data_lines.clear();
                }
            } else if let Some(data) = line.strip_prefix("data:") {
                self.data_lines
                    .push(data.strip_prefix(' ').unwrap_or(data).to_string());
            }
        }
        events
    }
}

#[async_trait]
impl LlmBackend for OpenAiHttpBackend {
    async fn models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        let call = async {
            let response = self
                .request(reqwest::Method::GET, "/models")
                .send()
                .await
                .map_err(transport_error)?;
            if !response.status().is_success() {
                return Err(status_error(response, "").await);
            }
            let parsed: ModelsResponse = response
                .json()
                .await
                .map_err(|e| LlmError::Protocol(format!("/models response: {e}")))?;
            Ok(parsed
                .data
                .into_iter()
                .map(|entry| ModelInfo {
                    name: entry.id,
                    content_hash: None, // §6.3: HTTP cannot read the weights
                })
                .collect())
        };
        tokio::time::timeout(self.config.probe_timeout, call)
            .await
            .map_err(|_| {
                LlmError::BackendUnavailable(format!(
                    "/models probe timed out after {:?}",
                    self.config.probe_timeout
                ))
            })?
    }

    async fn generate(&self, req: GenerateRequest) -> Result<GenerateStream, LlmError> {
        // §6.3: this backend cannot verify weights, so a pinned request can
        // never be satisfied here — fail before touching the network.
        if let Some(pinned) = req.model.required_hash {
            return Err(LlmError::HashMismatch {
                name: req.model.name.clone(),
                pinned,
            });
        }

        let deadline_at = req
            .deadline
            .map(|budget| tokio::time::Instant::now() + budget);
        let body = ChatCompletionRequest {
            model: &req.model.name,
            messages: &req.messages,
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
            temperature: req.params.temperature,
            top_p: req.params.top_p,
            max_tokens: req.params.max_tokens,
            stop: req.params.stop.clone(),
        };

        // Response headers must arrive within the request deadline or, when
        // none is set, within first_byte_timeout — generation length is
        // unbounded, time-to-first-byte is not.
        let headers_deadline = deadline_at
            .unwrap_or_else(|| tokio::time::Instant::now() + self.config.first_byte_timeout);
        let send = self
            .request(reqwest::Method::POST, "/chat/completions")
            .json(&body)
            .send();
        let response = match tokio::time::timeout_at(headers_deadline, send).await {
            Ok(sent) => sent.map_err(transport_error)?,
            Err(_) if deadline_at.is_some() => return Err(LlmError::DeadlineExceeded),
            Err(_) => {
                return Err(LlmError::BackendUnavailable(format!(
                    "no response headers within {:?}",
                    self.config.first_byte_timeout
                )));
            }
        };
        if !response.status().is_success() {
            return Err(status_error(response, &req.model.name).await);
        }

        let stream = async_stream::stream! {
            let mut bytes = response.bytes_stream();
            let mut parser = SseParser::default();
            let mut finish: Option<FinishReason> = None;
            let mut usage = Usage::default();
            loop {
                let next = match deadline_at {
                    Some(at) => match tokio::time::timeout_at(at, bytes.next()).await {
                        Ok(next) => next,
                        Err(_) => {
                            yield Err(LlmError::DeadlineExceeded);
                            return;
                        }
                    },
                    None => bytes.next().await,
                };
                match next {
                    Some(Ok(chunk)) => {
                        for payload in parser.push(&chunk) {
                            if payload == "[DONE]" {
                                yield Ok(GenerateEvent::End {
                                    finish_reason: finish.unwrap_or(FinishReason::Stop),
                                    usage,
                                });
                                return;
                            }
                            let parsed: StreamChunk = match serde_json::from_str(&payload) {
                                Ok(parsed) => parsed,
                                Err(e) => {
                                    yield Err(LlmError::Protocol(format!("bad SSE chunk: {e}")));
                                    return;
                                }
                            };
                            if let Some(wire) = parsed.usage {
                                usage = Usage {
                                    prompt_tokens: wire.prompt_tokens,
                                    completion_tokens: wire.completion_tokens,
                                };
                            }
                            for choice in parsed.choices {
                                if let Some(reason) = &choice.finish_reason {
                                    finish = Some(map_finish_reason(reason));
                                }
                                if let Some(content) = choice.delta.content
                                    && !content.is_empty()
                                {
                                    yield Ok(GenerateEvent::Delta { content });
                                }
                            }
                        }
                    }
                    Some(Err(e)) => {
                        // Truncated stream (§6.2): report, never auto-retry.
                        yield Err(LlmError::BackendCrashed(e.to_string()));
                        return;
                    }
                    None => {
                        // EOF without [DONE]: tolerate servers that just
                        // close after the final chunk, but only once a
                        // finish_reason proves the generation completed.
                        match finish {
                            Some(finish_reason) => {
                                yield Ok(GenerateEvent::End { finish_reason, usage })
                            }
                            None => {
                                yield Err(LlmError::Protocol(
                                    "stream ended without terminal frame".into(),
                                ))
                            }
                        }
                        return;
                    }
                }
            }
        };
        Ok(stream.boxed())
    }

    fn locality(&self) -> Locality {
        self.config.locality
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ModelSelector, SamplingParams};
    use super::*;
    use iroh_blobs::Hash;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Serves one canned HTTP response on a fresh port; resolves to the
    /// request it received (for body assertions).
    async fn one_shot_server(response: Vec<u8>) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&buf[..n]);
                if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&req[..pos]).to_lowercase();
                    let content_length = headers
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if req.len() >= pos + 4 + content_length {
                        break;
                    }
                }
            }
            sock.write_all(&response).await.unwrap();
            sock.shutdown().await.ok();
            String::from_utf8_lossy(&req).into_owned()
        });
        (format!("http://{addr}/v1"), handle)
    }

    fn http_response(content_type: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn backend(base_url: &str) -> OpenAiHttpBackend {
        OpenAiHttpBackend::new(OpenAiConfig::local(base_url)).unwrap()
    }

    fn request(model: &str) -> GenerateRequest {
        GenerateRequest {
            model: model.into(),
            messages: vec![ChatMessage::user("hello")],
            params: SamplingParams::default(),
            deadline: None,
        }
    }

    async fn collect(stream: GenerateStream) -> Vec<Result<GenerateEvent, LlmError>> {
        stream.collect().await
    }

    #[tokio::test]
    async fn models_parses_the_openai_list() {
        let body =
            r#"{"object":"list","data":[{"id":"GLM-4.7-Flash","object":"model"},{"id":"qwen3"}]}"#;
        let (base, _server) = one_shot_server(http_response("application/json", body)).await;

        let models = backend(&base).models().await.unwrap();
        let names: Vec<&str> = models.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["GLM-4.7-Flash", "qwen3"]);
        assert!(models.iter().all(|m| m.content_hash.is_none()));
    }

    #[tokio::test]
    async fn generate_streams_sse_deltas_then_end() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            ": keep-alive comment, must be ignored\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n",
        );
        let (base, server) = one_shot_server(http_response("text/event-stream", sse)).await;

        let stream = backend(&base).generate(request("m")).await.unwrap();
        let events: Vec<GenerateEvent> = collect(stream)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            events,
            vec![
                GenerateEvent::Delta {
                    content: "Hel".into()
                },
                GenerateEvent::Delta {
                    content: "lo".into()
                },
                GenerateEvent::End {
                    finish_reason: FinishReason::Stop,
                    usage: Usage {
                        prompt_tokens: 3,
                        completion_tokens: 2
                    },
                },
            ]
        );

        let sent = server.await.unwrap();
        assert!(sent.contains("POST /v1/chat/completions"));
        assert!(sent.contains("\"stream\":true"));
        assert!(sent.contains("\"include_usage\":true"));
    }

    #[tokio::test]
    async fn eof_after_finish_reason_still_ends_cleanly() {
        // Some servers close without [DONE]; a seen finish_reason proves the
        // generation completed, so this is a clean End, not a truncation.
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
        );
        let (base, _server) = one_shot_server(http_response("text/event-stream", sse)).await;

        let events = collect(backend(&base).generate(request("m")).await.unwrap()).await;
        assert!(matches!(
            events.last(),
            Some(Ok(GenerateEvent::End {
                finish_reason: FinishReason::Length,
                ..
            }))
        ));
    }

    #[tokio::test]
    async fn truncation_without_finish_reason_is_a_protocol_error() {
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
        let (base, _server) = one_shot_server(http_response("text/event-stream", sse)).await;

        let events = collect(backend(&base).generate(request("m")).await.unwrap()).await;
        assert!(matches!(events.last(), Some(Err(LlmError::Protocol(_)))));
    }

    #[tokio::test]
    async fn connection_refused_maps_to_backend_unavailable() {
        // Bind, learn the port, drop — nothing listens there anymore.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        drop(listener);

        let err = backend(&base).models().await.unwrap_err();
        assert!(matches!(err, LlmError::BackendUnavailable(_)), "got: {err}");
    }

    #[tokio::test]
    async fn http_429_maps_to_queue_full() {
        let response =
            b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_vec();
        let (base, _server) = one_shot_server(response).await;

        let result = backend(&base).generate(request("m")).await;
        assert!(matches!(result, Err(LlmError::QueueFull)));
    }

    #[tokio::test]
    async fn hash_pinned_requests_are_rejected_offline() {
        // No server needed: the rejection must happen before any I/O (§6.3).
        let backend = backend("http://127.0.0.1:9/v1");
        let mut req = request("m");
        req.model = ModelSelector {
            name: "m".into(),
            required_hash: Some(Hash::new(b"weights")),
        };
        let result = backend.generate(req).await;
        assert!(matches!(result, Err(LlmError::HashMismatch { .. })));
    }

    #[tokio::test]
    async fn deadline_cuts_a_stalled_stream() {
        // Server sends one delta then stalls with the socket open.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let head =
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
            let delta = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
            sock.write_all(head.as_bytes()).await.unwrap();
            sock.write_all(delta.as_bytes()).await.unwrap();
            // Stall: keep the socket open, never finish.
            tokio::time::sleep(Duration::from_secs(60)).await;
            drop(sock);
        });

        let mut req = request("m");
        req.deadline = Some(Duration::from_millis(300));
        let events = collect(backend(&base).generate(req).await.unwrap()).await;
        assert!(matches!(
            events.first(),
            Some(Ok(GenerateEvent::Delta { .. }))
        ));
        assert!(matches!(
            events.last(),
            Some(Err(LlmError::DeadlineExceeded))
        ));
        server.abort();
    }

    #[test]
    fn sse_parser_handles_split_chunks_and_crlf() {
        let mut parser = SseParser::default();
        // One event arriving in three fragments, CRLF line endings.
        assert!(parser.push(b"data: {\"a\"").is_empty());
        assert!(parser.push(b":1}\r\n").is_empty());
        let events = parser.push(b"\r\ndata: [DONE]\r\n\r\n");
        assert_eq!(events, vec![r#"{"a":1}"#.to_string(), "[DONE]".to_string()]);
    }
}
