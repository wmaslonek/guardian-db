//! # Native colibri process driver (RFC 0004 phase 3, feature `compute-llm-colibri`)
//!
//! A [`LlmBackend`] that owns a [colibri](https://github.com/JustVugg/colibri)
//! `coli` child process and speaks its stdin/stdout serve protocol directly —
//! the same protocol colibri's own `openai_server.py` speaks — removing the
//! Python dependency. The `coli` binary itself stays upstream-built; only the
//! driver is ours (RFC 0004 §2: no rewrite, no fork).
//!
//! ## The protocol (as spoken by `openai_server.py`)
//!
//! - spawn: `coli <cap>` with `SNAP=<model> SERVE=1 SERVE_BATCH=1 NGEN=<n>
//!   KV_SLOTS=<n>` in the environment;
//! - warmup: the engine prints arbitrary output, then the READY sentinel
//!   (`\x01\x01READY\x01\x01\n`) followed by a `STAT …` line;
//! - request: `SUBMIT <id> <slot> <len> <max> <temp> <top_p>\n` + prompt
//!   bytes + `\n`;
//! - response lines: `DATA <id> <size>\n<bytes>\n` (UTF-8 fragments that may
//!   split multi-byte characters), `DONE <id> STAT …`, `ERROR <id> <msg…>`;
//!   info lines (`HWINFO`/`TIERS`/`PROF`/…) may arrive at any time;
//! - cancel: `CANCEL <id>\n`, acknowledged as `ERROR <id> CANCELLED`.
//!
//! One deliberate deviation: unknown stdout lines are logged and ignored
//! rather than treated as fatal (the Python server errors). This keeps the
//! driver robust against engine-version drift and lets the supervisor tests
//! run against a scripted fake engine.
//!
//! ## Supervision (RFC 0004 §6.4, decided: the robust version outright)
//!
//! One supervisor task owns the child: spawn → warmup (deadline) → `Ready` →
//! serve. Crashes fail the in-flight request with
//! [`LlmError::BackendCrashed`] (never silently retried — generation is
//! non-deterministic) and restart with exponential backoff + jitter under a
//! restart budget; exceeding the budget trips the backend until
//! [`ColibriProcessBackend::reset`] — repeated crashes usually mean a bad
//! model file or OOM, which retrying cannot fix. Requests flow through a
//! bounded queue with per-request deadlines covering queue wait +
//! generation; queue-full and deadline-exceeded fail fast and distinctly.
//! `stderr` is drained into tracing. [`LlmBackend::models`] reads the
//! supervisor state, so the §6.1 prober needs no separate probe for a child
//! we already own.
//!
//! ## Model identity (RFC 0004 §6.3)
//!
//! The weight file is BLAKE3-hashed at spawn, cached in a
//! `<model>.gdb-b3` sidecar keyed by `(size, mtime)` so restarts do not
//! re-hash hundreds of gigabytes. The hash is advertised in [`ModelInfo`]
//! and hash-pinned requests are verified against it.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use iroh_blobs::Hash;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::{
    ChatMessage, FinishReason, GenerateEvent, GenerateRequest, GenerateStream, LlmBackend,
    LlmError, Locality, ModelInfo, Role, Usage,
};

/// The engine's warmup sentinel (`openai_server.py`'s `READY`).
const READY_SENTINEL: &[u8] = b"\x01\x01READY\x01\x01\n";
/// Engine `DATA` payloads are bounded (the Python server enforces the same).
const MAX_DATA_BYTES: usize = 65536;
/// After a cancel/deadline, how long to wait for the engine's terminal frame
/// before declaring it wedged and restarting the child (§6.4: freeing
/// capacity beats preserving a doomed decode).
const CANCEL_GRACE: Duration = Duration::from_secs(10);

/// How chat messages become the engine's prompt string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptTemplate {
    /// The text-only subset of the official GLM chat template (what colibri
    /// models are trained on; mirrors `openai_server.py::render_chat`).
    Glm,
    /// Message contents joined by newlines — for tests and raw models.
    Raw,
}

#[derive(Debug, Clone)]
pub struct ColibriConfig {
    /// The upstream-built `coli` executable.
    pub executable: PathBuf,
    /// The model snapshot file (`SNAP`).
    pub model_path: PathBuf,
    /// The name guests select by; defaults to the model file stem.
    pub model_name: String,
    /// Engine cache cap, `coli`'s single positional argument.
    pub cap: u32,
    /// Server-side generation ceiling (`NGEN`); requests are clamped to it.
    pub max_tokens: u32,
    /// KV cache slots (`KV_SLOTS`). The driver serializes requests, so 1.
    pub kv_slots: u32,
    pub template: PromptTemplate,
    /// Bounded request queue depth (§6.4); overflow is [`LlmError::QueueFull`].
    pub queue_depth: usize,
    /// Deadline applied when a request carries none (queue wait + generation).
    pub default_deadline: Duration,
    /// Budget for spawn + model load up to the READY sentinel.
    pub warmup_timeout: Duration,
    /// Crashes tolerated within [`budget_window`](Self::budget_window)
    /// before the backend trips (§6.4).
    pub restart_budget: u32,
    pub budget_window: Duration,
    /// Exponential restart backoff (jittered), reset on a successful warmup.
    pub backoff_base: Duration,
    pub backoff_cap: Duration,
    /// BLAKE3-hash the model file at spawn (§6.3). Disable only for tests.
    pub verify_hash: bool,
    /// Extra environment for the child.
    pub extra_env: Vec<(String, String)>,
    /// Replaces the default `[cap]` argv entirely (testing/advanced hook —
    /// the supervisor e2e tests point this at a scripted fake engine).
    pub custom_args: Option<Vec<String>>,
}

impl ColibriConfig {
    pub fn new(executable: impl Into<PathBuf>, model_path: impl Into<PathBuf>) -> Self {
        let model_path: PathBuf = model_path.into();
        let model_name = model_path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "colibri-model".into());
        Self {
            executable: executable.into(),
            model_path,
            model_name,
            cap: 8,
            max_tokens: 1024,
            kv_slots: 1,
            template: PromptTemplate::Glm,
            queue_depth: 32,
            default_deadline: Duration::from_secs(10 * 60),
            warmup_timeout: Duration::from_secs(15 * 60),
            restart_budget: 5,
            budget_window: Duration::from_secs(10 * 60),
            backoff_base: Duration::from_secs(1),
            backoff_cap: Duration::from_secs(60),
            verify_hash: true,
            extra_env: Vec::new(),
            custom_args: None,
        }
    }
}

// ─── Supervisor state ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum State {
    Starting,
    Ready,
    Restarting { attempt: u32 },
    Tripped { reason: String },
}

struct StateInner {
    state: State,
    /// Crash instants within the budget window (§6.4).
    crashes: VecDeque<Instant>,
}

type Shared = Arc<parking_lot::Mutex<StateInner>>;

fn set_state(shared: &Shared, state: State) {
    shared.lock().state = state;
}

/// Records a crash; returns `Some(reason)` when the budget tripped.
fn record_crash(shared: &Shared, config: &ColibriConfig, reason: &str) -> Option<String> {
    let mut inner = shared.lock();
    let now = Instant::now();
    inner.crashes.push_back(now);
    while let Some(first) = inner.crashes.front() {
        if now.duration_since(*first) > config.budget_window {
            inner.crashes.pop_front();
        } else {
            break;
        }
    }
    if inner.crashes.len() as u32 >= config.restart_budget {
        let tripped = format!(
            "tripped after {} crashes within {:?} (last: {reason}); call reset() after fixing the cause",
            inner.crashes.len(),
            config.budget_window,
        );
        inner.state = State::Tripped {
            reason: tripped.clone(),
        };
        Some(tripped)
    } else {
        None
    }
}

// ─── Engine wire types ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
struct EngineStats {
    completion_tokens: u32,
    prompt_tokens: u32,
    length_limited: bool,
}

#[derive(Debug)]
enum EngineEvent {
    Ready,
    Data { id: String, bytes: Vec<u8> },
    Done { id: String, stats: EngineStats },
    EngineError { id: String, message: String },
    Crashed(String),
}

/// `DONE`/warmup status: `STAT <completion> <tps> <hit%> <rss> [prompt] [ll]`.
fn parse_stats(fields: &[&str]) -> Result<EngineStats, String> {
    if fields.len() < 5 || fields[0] != "STAT" {
        return Err(format!("invalid engine status: {}", fields.join(" ")));
    }
    let completion = fields[1]
        .parse()
        .map_err(|_| format!("bad completion count: {}", fields[1]))?;
    let prompt = fields
        .get(5)
        .and_then(|f| f.parse().ok())
        .unwrap_or_default();
    let length_limited = fields.get(6).map(|f| *f == "1").unwrap_or(false);
    Ok(EngineStats {
        completion_tokens: completion,
        prompt_tokens: prompt,
        length_limited,
    })
}

// ─── Buffered engine reader ──────────────────────────────────────────────────

/// Minimal buffered reader for the engine's mixed line/binary stdout: line
/// framing for control lines, exact reads for `DATA` payloads, and the
/// warmup sentinel scan. EOF is always an error (the engine exited).
struct EngineReader<R> {
    reader: R,
    buf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> EngineReader<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            buf: Vec::new(),
        }
    }

    async fn fill(&mut self) -> Result<(), String> {
        let mut chunk = [0u8; 8192];
        let n = self
            .reader
            .read(&mut chunk)
            .await
            .map_err(|e| format!("engine stdout read: {e}"))?;
        if n == 0 {
            return Err("colibri engine exited unexpectedly".into());
        }
        self.buf.extend_from_slice(&chunk[..n]);
        Ok(())
    }

    /// Discards everything up to and including `sentinel`.
    async fn wait_sentinel(&mut self, sentinel: &[u8]) -> Result<(), String> {
        loop {
            if let Some(pos) = find_sub(&self.buf, sentinel) {
                self.buf.drain(..pos + sentinel.len());
                return Ok(());
            }
            // Bound memory while skipping arbitrary warmup output: only a
            // sentinel-sized tail can still contain a partial match.
            if self.buf.len() > 64 * 1024 {
                let keep = sentinel.len() - 1;
                self.buf.drain(..self.buf.len() - keep);
            }
            self.fill().await?;
        }
    }

    /// One `\n`-terminated line (without the terminator, `\r` tolerated).
    async fn line(&mut self) -> Result<String, String> {
        loop {
            if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(String::from_utf8_lossy(&line).into_owned());
            }
            self.fill().await?;
        }
    }

    async fn exact(&mut self, n: usize) -> Result<Vec<u8>, String> {
        while self.buf.len() < n {
            self.fill().await?;
        }
        Ok(self.buf.drain(..n).collect())
    }
}

fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Owns the child's stdout: warmup, then the dispatch loop. All exits are a
/// `Crashed` event; unknown lines are ignored (see the module docs).
async fn read_stdout(stdout: ChildStdout, events: mpsc::UnboundedSender<EngineEvent>) {
    let mut reader = EngineReader::new(stdout);
    let crash = |m: String| EngineEvent::Crashed(m);

    if let Err(e) = reader.wait_sentinel(READY_SENTINEL).await {
        let _ = events.send(crash(e));
        return;
    }
    match reader.line().await {
        Ok(line) => {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if let Err(e) = parse_stats(&fields) {
                debug!(target: "colibri", "unexpected warmup status: {e}");
            }
        }
        Err(e) => {
            let _ = events.send(crash(e));
            return;
        }
    }
    if events.send(EngineEvent::Ready).is_err() {
        return;
    }

    loop {
        let line = match reader.line().await {
            Ok(line) => line,
            Err(e) => {
                let _ = events.send(crash(e));
                return;
            }
        };
        let fields: Vec<&str> = line.split_whitespace().collect();
        let event = match fields.as_slice() {
            ["DATA", id, size] => {
                let Ok(size) = size.parse::<usize>() else {
                    let _ = events.send(crash(format!("invalid DATA size: {size}")));
                    return;
                };
                if size > MAX_DATA_BYTES {
                    let _ = events.send(crash(format!("DATA payload of {size} bytes")));
                    return;
                }
                // Payload plus the trailing '\n' terminator.
                let mut payload = match reader.exact(size + 1).await {
                    Ok(payload) => payload,
                    Err(e) => {
                        let _ = events.send(crash(e));
                        return;
                    }
                };
                if payload.pop() != Some(b'\n') {
                    let _ = events.send(crash("invalid DATA terminator".into()));
                    return;
                }
                EngineEvent::Data {
                    id: id.to_string(),
                    bytes: payload,
                }
            }
            ["DONE", id, rest @ ..] => match parse_stats(rest) {
                Ok(stats) => EngineEvent::Done {
                    id: id.to_string(),
                    stats,
                },
                Err(e) => {
                    let _ = events.send(crash(e));
                    return;
                }
            },
            ["ERROR", id, message @ ..] => EngineEvent::EngineError {
                id: id.to_string(),
                message: if message.is_empty() {
                    "engine request failed".into()
                } else {
                    message.join(" ")
                },
            },
            [] => continue,
            other => {
                // HWINFO/EMAP/HITS/PROF/TIERS and future engine chatter.
                debug!(target: "colibri", "ignoring engine line: {}", other.join(" "));
                continue;
            }
        };
        if events.send(event).is_err() {
            return;
        }
    }
}

// ─── Prompt rendering ────────────────────────────────────────────────────────

/// Renders the request's messages for the engine (RFC 0004 §3.1 keeps the
/// chat dialect at the trait level; the template is a backend concern).
fn render_prompt(template: PromptTemplate, messages: &[ChatMessage]) -> Result<String, LlmError> {
    if messages.is_empty() {
        return Err(LlmError::Protocol("messages must not be empty".into()));
    }
    let prompt = match template {
        PromptTemplate::Glm => {
            // Text-only subset of the GLM template (openai_server.py
            // render_chat); tool blocks are out of scope for the driver.
            let mut prompt = String::from("[gMASK]<sop>");
            for message in messages {
                match message.role {
                    Role::System => {
                        prompt.push_str("<|system|>");
                        prompt.push_str(&message.content);
                    }
                    Role::User => {
                        prompt.push_str("<|user|>");
                        prompt.push_str(&message.content);
                    }
                    Role::Assistant => {
                        prompt.push_str("<|assistant|><think></think>");
                        prompt.push_str(message.content.trim());
                    }
                }
            }
            prompt.push_str("<|assistant|><think></think>");
            prompt
        }
        PromptTemplate::Raw => {
            let contents: Vec<&str> = messages.iter().map(|m| m.content.as_str()).collect();
            contents.join("\n")
        }
    };
    if prompt.as_bytes().contains(&0) {
        return Err(LlmError::Protocol(
            "NUL bytes are not supported in prompts".into(),
        ));
    }
    Ok(prompt)
}

// ─── Incremental UTF-8 ───────────────────────────────────────────────────────

/// `DATA` fragments may split multi-byte characters; this carries the
/// incomplete tail between fragments (the Python server uses an incremental
/// codec for the same reason).
#[derive(Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(valid) => {
                    out.push_str(valid);
                    self.pending.clear();
                    return out;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    out.push_str(
                        std::str::from_utf8(&self.pending[..valid]).expect("valid prefix"),
                    );
                    match e.error_len() {
                        // Incomplete trailing sequence: keep it for the next
                        // fragment.
                        None => {
                            self.pending.drain(..valid);
                            return out;
                        }
                        // Truly invalid bytes: replace and continue.
                        Some(bad) => {
                            out.push('\u{FFFD}');
                            self.pending.drain(..valid + bad);
                        }
                    }
                }
            }
        }
    }

    fn flush(&mut self) -> String {
        let tail = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        tail
    }
}

// ─── Model hashing (§6.3) ────────────────────────────────────────────────────

/// BLAKE3 of the weight file, cached in a `<model>.gdb-b3` sidecar keyed by
/// `(size, mtime)` so restarts do not re-hash huge files.
fn model_hash_blocking(path: &std::path::Path) -> Result<Hash, LlmError> {
    let meta = std::fs::metadata(path)
        .map_err(|e| LlmError::BackendUnavailable(format!("model file {path:?}: {e}")))?;
    let size = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or_default();

    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(".gdb-b3");
    let sidecar = PathBuf::from(sidecar);

    if let Ok(cached) = std::fs::read_to_string(&sidecar) {
        let fields: Vec<&str> = cached.split_whitespace().collect();
        if let [cached_size, cached_mtime, hex] = fields.as_slice()
            && cached_size.parse() == Ok(size)
            && cached_mtime.parse() == Ok(mtime)
            && let Ok(hash) = hex.parse::<Hash>()
        {
            return Ok(hash);
        }
    }

    let file = std::fs::File::open(path)
        .map_err(|e| LlmError::BackendUnavailable(format!("model file {path:?}: {e}")))?;
    let mut reader = std::io::BufReader::with_capacity(1024 * 1024, file);
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut reader, &mut hasher)
        .map_err(|e| LlmError::BackendUnavailable(format!("hashing {path:?}: {e}")))?;
    let hash = Hash::from_bytes(*hasher.finalize().as_bytes());

    // Best-effort cache; a failed write only costs a future re-hash.
    if let Err(e) = std::fs::write(&sidecar, format!("{size} {mtime} {hash}")) {
        debug!(target: "colibri", "sidecar write failed ({sidecar:?}): {e}");
    }
    Ok(hash)
}

// ─── The backend ─────────────────────────────────────────────────────────────

struct Job {
    request: GenerateRequest,
    deadline_at: tokio::time::Instant,
    events: mpsc::Sender<Result<GenerateEvent, LlmError>>,
}

/// Supervised `coli` child process as a [`LlmBackend`]. See the module docs.
///
/// Dropping the backend closes the job queue; the supervisor finishes the
/// in-flight generation (graceful drain), then exits and kills the child.
pub struct ColibriProcessBackend {
    model: ModelInfo,
    jobs: mpsc::Sender<Job>,
    state: Shared,
    config: Arc<ColibriConfig>,
    /// Wakes a tripped supervisor after [`reset`](Self::reset).
    reset_signal: Arc<tokio::sync::Notify>,
}

impl std::fmt::Debug for ColibriProcessBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColibriProcessBackend")
            .field("model", &self.model)
            .field("state", &self.state.lock().state)
            .finish()
    }
}

impl ColibriProcessBackend {
    /// Hashes the model (§6.3, off the async pool) and starts the supervisor.
    /// Returns immediately: the backend reports itself unavailable until the
    /// child reaches READY, which the §6.1 prober picks up.
    pub async fn spawn(config: ColibriConfig) -> Result<Arc<Self>, LlmError> {
        let content_hash = if config.verify_hash {
            let path = config.model_path.clone();
            Some(
                tokio::task::spawn_blocking(move || model_hash_blocking(&path))
                    .await
                    .map_err(|e| LlmError::Protocol(format!("hash task: {e}")))??,
            )
        } else {
            None
        };
        let model = ModelInfo {
            name: config.model_name.clone(),
            content_hash,
        };
        let config = Arc::new(config);
        let state: Shared = Arc::new(parking_lot::Mutex::new(StateInner {
            state: State::Starting,
            crashes: VecDeque::new(),
        }));
        let (jobs_tx, jobs_rx) = mpsc::channel(config.queue_depth);
        let reset_signal = Arc::new(tokio::sync::Notify::new());
        tokio::spawn(supervise(
            config.clone(),
            state.clone(),
            jobs_rx,
            reset_signal.clone(),
        ));
        Ok(Arc::new(Self {
            model,
            jobs: jobs_tx,
            state,
            config,
            reset_signal,
        }))
    }

    /// Un-trips a tripped backend after the owner fixed the underlying cause
    /// (§6.4): clears the crash history and wakes the supervisor for a fresh
    /// spawn. A no-op in any other state.
    pub fn reset(&self) {
        let mut inner = self.state.lock();
        if !matches!(inner.state, State::Tripped { .. }) {
            return;
        }
        inner.crashes.clear();
        inner.state = State::Starting;
        drop(inner);
        info!(target: "colibri", "reset: restarting supervision");
        self.reset_signal.notify_one();
    }

    fn state_error(&self) -> Option<LlmError> {
        match &self.state.lock().state {
            State::Ready => None,
            State::Starting => Some(LlmError::BackendUnavailable(
                "colibri engine is starting".into(),
            )),
            State::Restarting { attempt } => Some(LlmError::BackendUnavailable(format!(
                "colibri engine is restarting (attempt {attempt})"
            ))),
            State::Tripped { reason } => Some(LlmError::BackendUnavailable(reason.clone())),
        }
    }
}

#[async_trait]
impl LlmBackend for ColibriProcessBackend {
    /// §6.4 health integration: the supervisor state *is* the probe — no
    /// separate handshake with a child we already own.
    async fn models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        match self.state_error() {
            None => Ok(vec![self.model.clone()]),
            Some(err) => Err(err),
        }
    }

    async fn generate(&self, request: GenerateRequest) -> Result<GenerateStream, LlmError> {
        if request.model.name != self.model.name {
            return Err(LlmError::ModelNotFound(request.model.name.clone()));
        }
        // §6.3: this backend *can* verify weights — enforce the pin.
        if let Some(pinned) = request.model.required_hash
            && self.model.content_hash != Some(pinned)
        {
            return Err(LlmError::HashMismatch {
                name: request.model.name.clone(),
                pinned,
            });
        }
        // Tripped fails fast; Starting/Restarting requests may queue — their
        // deadline covers the wait (§6.4).
        if let State::Tripped { reason } = &self.state.lock().state {
            return Err(LlmError::BackendUnavailable(reason.clone()));
        }
        render_prompt(self.config.template, &request.messages)?; // validate early

        let deadline_at =
            tokio::time::Instant::now() + request.deadline.unwrap_or(self.config.default_deadline);
        let (tx, rx) = mpsc::channel(256);
        let job = Job {
            request,
            deadline_at,
            events: tx,
        };
        self.jobs.try_send(job).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => LlmError::QueueFull,
            mpsc::error::TrySendError::Closed(_) => {
                LlmError::BackendUnavailable("colibri supervisor stopped".into())
            }
        })?;
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    fn locality(&self) -> Locality {
        Locality::Local
    }
}

// ─── Supervisor ──────────────────────────────────────────────────────────────

async fn supervise(
    config: Arc<ColibriConfig>,
    state: Shared,
    mut jobs: mpsc::Receiver<Job>,
    reset_signal: Arc<tokio::sync::Notify>,
) {
    let mut backoff = config.backoff_base;
    let mut attempt: u32 = 0;
    loop {
        set_state(&state, State::Starting);
        let outcome = run_child(&config, &state, &mut jobs).await;
        match outcome {
            RunOutcome::Shutdown => {
                debug!(target: "colibri", "supervisor: clean shutdown");
                return;
            }
            RunOutcome::Crashed { reason, was_ready } => {
                warn!(target: "colibri", "engine crashed: {reason}");
                if was_ready {
                    // A full warmup resets the backoff ladder; only crash
                    // loops keep climbing it.
                    backoff = config.backoff_base;
                    attempt = 0;
                }
                if let Some(tripped) = record_crash(&state, &config, &reason) {
                    warn!(target: "colibri", "{tripped}");
                    fail_queued(&mut jobs, &tripped).await;
                    // Park until reset() (fresh jobs fail fast meanwhile) or
                    // shutdown; the crash history was cleared by reset().
                    loop {
                        tokio::select! {
                            _ = reset_signal.notified() => break,
                            job = jobs.recv() => match job {
                                None => return,
                                Some(job) => {
                                    let _ = job.events
                                        .send(Err(LlmError::BackendUnavailable(tripped.clone())))
                                        .await;
                                }
                            },
                        }
                    }
                    backoff = config.backoff_base;
                    attempt = 0;
                    continue;
                }
                attempt += 1;
                set_state(&state, State::Restarting { attempt });
                let jitter = 0.5 + fastrand::f64(); // 0.5x..1.5x
                tokio::time::sleep(backoff.mul_f64(jitter)).await;
                backoff = (backoff * 2).min(config.backoff_cap);
            }
        }
    }
}

/// Fails every queued job when the backend trips (§6.4).
async fn fail_queued(jobs: &mut mpsc::Receiver<Job>, reason: &str) {
    while let Ok(job) = jobs.try_recv() {
        let _ = job
            .events
            .send(Err(LlmError::BackendUnavailable(reason.to_string())))
            .await;
    }
}

enum RunOutcome {
    /// The job queue closed: the backend was dropped.
    Shutdown,
    Crashed {
        reason: String,
        /// Whether warmup had completed (resets the backoff ladder).
        was_ready: bool,
    },
}

/// One child lifetime: spawn → warmup → serve jobs until crash or shutdown.
async fn run_child(
    config: &ColibriConfig,
    state: &Shared,
    jobs: &mut mpsc::Receiver<Job>,
) -> RunOutcome {
    let crashed = |reason: String, was_ready: bool| RunOutcome::Crashed { reason, was_ready };

    let mut command = Command::new(&config.executable);
    match &config.custom_args {
        Some(args) => {
            command.args(args);
        }
        None => {
            command.arg(config.cap.to_string());
        }
    }
    command
        .env("SNAP", &config.model_path)
        .env("SERVE", "1")
        .env("SERVE_BATCH", "1")
        .env("NGEN", config.max_tokens.to_string())
        .env("KV_SLOTS", config.kv_slots.to_string())
        .envs(config.extra_env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child: Child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return crashed(format!("spawn {:?}: {e}", config.executable), false),
    };
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let mut stdin = child.stdin.take().expect("stdin piped");

    // Drain stderr into tracing so a wedged child is diagnosable (§6.4).
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            debug!(target: "colibri-stderr", "{line}");
        }
    });

    let (events_tx, mut events) = mpsc::unbounded_channel();
    let stdout_task = tokio::spawn(read_stdout(stdout, events_tx));

    let result = drive_child(config, state, jobs, &mut stdin, &mut events).await;

    stdout_task.abort();
    stderr_task.abort();
    drop(stdin);
    let _ = child.kill().await;
    result
}

/// Waits for READY, then serves jobs; returns on crash or queue close.
async fn drive_child(
    config: &ColibriConfig,
    state: &Shared,
    jobs: &mut mpsc::Receiver<Job>,
    stdin: &mut ChildStdin,
    events: &mut mpsc::UnboundedReceiver<EngineEvent>,
) -> RunOutcome {
    let crashed = |reason: String, was_ready: bool| RunOutcome::Crashed { reason, was_ready };

    // Warmup under its deadline.
    match tokio::time::timeout(config.warmup_timeout, events.recv()).await {
        Ok(Some(EngineEvent::Ready)) => {}
        Ok(Some(EngineEvent::Crashed(reason))) => return crashed(reason, false),
        Ok(Some(other)) => {
            return crashed(format!("unexpected pre-ready event: {other:?}"), false);
        }
        Ok(None) => return crashed("stdout reader stopped".into(), false),
        Err(_) => {
            return crashed(
                format!("warmup exceeded {:?}", config.warmup_timeout),
                false,
            );
        }
    }
    set_state(state, State::Ready);
    info!(target: "colibri", model = %config.model_name, "engine ready");

    let mut next_request_id: u64 = 1;
    loop {
        tokio::select! {
            job = jobs.recv() => match job {
                None => return RunOutcome::Shutdown,
                Some(job) => {
                    let id = next_request_id.to_string();
                    next_request_id += 1;
                    if let Err(reason) = serve_job(config, stdin, events, job, &id).await {
                        return crashed(reason, true);
                    }
                }
            },
            event = events.recv() => match event {
                Some(EngineEvent::Crashed(reason)) => return crashed(reason, true),
                Some(stale) => debug!(target: "colibri", "idle engine event ignored: {stale:?}"),
                None => return crashed("stdout reader stopped".into(), true),
            },
        }
    }
}

/// Serves one job end-to-end. `Err` means the *child* is gone/wedged and the
/// supervisor must restart it; job-level failures are delivered on the job's
/// stream and return `Ok`.
async fn serve_job(
    config: &ColibriConfig,
    stdin: &mut ChildStdin,
    events: &mut mpsc::UnboundedReceiver<EngineEvent>,
    job: Job,
    id: &str,
) -> Result<(), String> {
    // Deadline may have burned in the queue (§6.4: distinct, immediate).
    if tokio::time::Instant::now() >= job.deadline_at {
        let _ = job.events.send(Err(LlmError::DeadlineExceeded)).await;
        return Ok(());
    }
    let prompt = match render_prompt(config.template, &job.request.messages) {
        Ok(prompt) => prompt,
        Err(e) => {
            let _ = job.events.send(Err(e)).await;
            return Ok(());
        }
    };
    let params = &job.request.params;
    let max_tokens = params
        .max_tokens
        .unwrap_or(config.max_tokens)
        .clamp(1, config.max_tokens);
    let temperature = params.temperature.unwrap_or(0.7);
    let top_p = params.top_p.unwrap_or(0.9);
    let payload = prompt.as_bytes();
    let header = format!(
        "SUBMIT {id} 0 {} {max_tokens} {temperature} {top_p}\n",
        payload.len()
    );

    let mut message = Vec::with_capacity(header.len() + payload.len() + 1);
    message.extend_from_slice(header.as_bytes());
    message.extend_from_slice(payload);
    message.push(b'\n');
    if let Err(e) = stdin.write_all(&message).await {
        let _ = job
            .events
            .send(Err(LlmError::BackendCrashed(format!("engine stdin: {e}"))))
            .await;
        return Err(format!("engine stdin: {e}"));
    }
    let _ = stdin.flush().await;

    let mut decoder = Utf8Decoder::default();
    let mut cancelled: Option<LlmError> = None;
    let mut hard_stop = job.deadline_at;
    loop {
        let event = match tokio::time::timeout_at(hard_stop, events.recv()).await {
            Ok(Some(event)) => event,
            Ok(None) => {
                let _ = job
                    .events
                    .send(Err(LlmError::BackendCrashed(
                        "stdout reader stopped".into(),
                    )))
                    .await;
                return Err("stdout reader stopped".into());
            }
            Err(_elapsed) => {
                if cancelled.is_none() {
                    // Deadline hit: cancel and give the engine a short grace
                    // to acknowledge before declaring it wedged (§6.4).
                    cancelled = Some(LlmError::DeadlineExceeded);
                    hard_stop = tokio::time::Instant::now() + CANCEL_GRACE;
                    if let Err(e) = stdin.write_all(format!("CANCEL {id}\n").as_bytes()).await {
                        let _ = job.events.send(Err(LlmError::DeadlineExceeded)).await;
                        return Err(format!("engine stdin: {e}"));
                    }
                    let _ = stdin.flush().await;
                    continue;
                }
                let _ = job.events.send(Err(LlmError::DeadlineExceeded)).await;
                return Err("cancel unacknowledged: engine wedged".into());
            }
        };
        match event {
            EngineEvent::Data {
                id: event_id,
                bytes,
            } if event_id == id => {
                if cancelled.is_some() {
                    continue; // draining a cancelled generation
                }
                let text = decoder.push(&bytes);
                if text.is_empty() {
                    continue;
                }
                if job
                    .events
                    .send(Ok(GenerateEvent::Delta { content: text }))
                    .await
                    .is_err()
                {
                    // Receiver dropped: the caller cancelled (§6.4). Free
                    // the engine and drain to the terminal frame.
                    cancelled = Some(LlmError::Cancelled);
                    hard_stop = tokio::time::Instant::now() + CANCEL_GRACE;
                    if let Err(e) = stdin.write_all(format!("CANCEL {id}\n").as_bytes()).await {
                        return Err(format!("engine stdin: {e}"));
                    }
                    let _ = stdin.flush().await;
                }
            }
            EngineEvent::Done {
                id: event_id,
                stats,
            } if event_id == id => {
                if cancelled.is_none() {
                    let tail = decoder.flush();
                    if !tail.is_empty() {
                        let _ = job
                            .events
                            .send(Ok(GenerateEvent::Delta { content: tail }))
                            .await;
                    }
                    let finish_reason = if stats.length_limited {
                        FinishReason::Length
                    } else {
                        FinishReason::Stop
                    };
                    let _ = job
                        .events
                        .send(Ok(GenerateEvent::End {
                            finish_reason,
                            usage: Usage {
                                prompt_tokens: stats.prompt_tokens,
                                completion_tokens: stats.completion_tokens,
                            },
                        }))
                        .await;
                }
                return Ok(());
            }
            EngineEvent::EngineError {
                id: event_id,
                message,
            } if event_id == id => {
                match &cancelled {
                    // The engine acknowledged our CANCEL.
                    Some(LlmError::Cancelled) if message == "CANCELLED" => {}
                    Some(reason) => {
                        let _ = job.events.send(Err(clone_error(reason))).await;
                    }
                    None => {
                        let _ = job.events.send(Err(LlmError::Protocol(message))).await;
                    }
                }
                return Ok(());
            }
            EngineEvent::Crashed(reason) => {
                let _ = job
                    .events
                    .send(Err(LlmError::BackendCrashed(reason.clone())))
                    .await;
                return Err(reason);
            }
            stale => debug!(target: "colibri", "stale engine event ignored: {stale:?}"),
        }
    }
}

/// `LlmError` is not `Clone` (it may wrap streams upstream); the two variants
/// the cancel path needs are value-only.
fn clone_error(err: &LlmError) -> LlmError {
    match err {
        LlmError::Cancelled => LlmError::Cancelled,
        LlmError::DeadlineExceeded => LlmError::DeadlineExceeded,
        other => LlmError::Protocol(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ModelSelector, SamplingParams};
    use super::*;
    use futures::StreamExt;

    // ── pure pieces ─────────────────────────────────────────────────────────

    #[test]
    fn glm_template_matches_the_python_renderer() {
        let messages = vec![
            ChatMessage::system("be brief"),
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello "),
            ChatMessage::user("bye"),
        ];
        let prompt = render_prompt(PromptTemplate::Glm, &messages).unwrap();
        assert_eq!(
            prompt,
            "[gMASK]<sop><|system|>be brief<|user|>hi\
             <|assistant|><think></think>hello<|user|>bye\
             <|assistant|><think></think>"
        );
    }

    #[test]
    fn nul_bytes_in_prompts_are_rejected() {
        let messages = vec![ChatMessage::user("a\0b")];
        assert!(matches!(
            render_prompt(PromptTemplate::Raw, &messages),
            Err(LlmError::Protocol(_))
        ));
    }

    #[test]
    fn utf8_decoder_survives_split_multibyte_chars() {
        let mut decoder = Utf8Decoder::default();
        // "é" = 0xC3 0xA9, split across two DATA fragments.
        assert_eq!(decoder.push(b"caf\xC3"), "caf");
        assert_eq!(decoder.push(b"\xA9!"), "é!");
        // Truly invalid byte becomes U+FFFD, decoding continues.
        assert_eq!(decoder.push(b"a\xFFb"), "a\u{FFFD}b");
        assert_eq!(decoder.flush(), "");
    }

    #[test]
    fn stats_parse_matches_the_engine_line() {
        let stats = parse_stats(&["STAT", "42", "10.5", "0.9", "1.2", "7", "1"]).unwrap();
        assert_eq!(
            stats,
            EngineStats {
                completion_tokens: 42,
                prompt_tokens: 7,
                length_limited: true,
            }
        );
        // The short form (no prompt/length fields) still parses.
        let stats = parse_stats(&["STAT", "3", "1.0", "0.0", "0.5"]).unwrap();
        assert_eq!(stats.prompt_tokens, 0);
        assert!(!stats.length_limited);
        assert!(parse_stats(&["NOPE", "1"]).is_err());
    }

    #[tokio::test]
    async fn engine_reader_frames_lines_data_and_sentinel() {
        // Sentinel split across the banner, then a STAT line, a DATA frame
        // and a control line — all in one buffer read in small chunks.
        let mut wire = Vec::new();
        wire.extend_from_slice(b"warmup banner noise\n\x01\x01READY");
        wire.extend_from_slice(b"\x01\x01\nSTAT 0 0 0 0\nDATA 1 5\nHello\nDONE 1 STAT 1 1 0 0\n");
        let mut reader = EngineReader::new(std::io::Cursor::new(wire));

        reader.wait_sentinel(READY_SENTINEL).await.unwrap();
        assert_eq!(reader.line().await.unwrap(), "STAT 0 0 0 0");
        assert_eq!(reader.line().await.unwrap(), "DATA 1 5");
        assert_eq!(reader.exact(6).await.unwrap(), b"Hello\n");
        assert_eq!(reader.line().await.unwrap(), "DONE 1 STAT 1 1 0 0");
        // EOF is a crash, never a clean end.
        assert!(reader.line().await.is_err());
    }

    #[test]
    fn crash_budget_trips_only_inside_the_window() {
        let config = {
            let mut c = ColibriConfig::new("coli", "model.snap");
            c.restart_budget = 3;
            c.budget_window = Duration::from_secs(600);
            c
        };
        let state: Shared = Arc::new(parking_lot::Mutex::new(StateInner {
            state: State::Starting,
            crashes: VecDeque::new(),
        }));
        assert!(record_crash(&state, &config, "one").is_none());
        assert!(record_crash(&state, &config, "two").is_none());
        let tripped = record_crash(&state, &config, "three").expect("third crash trips");
        assert!(tripped.contains("three"));
        assert!(matches!(state.lock().state, State::Tripped { .. }));
    }

    #[test]
    fn model_hash_uses_and_refreshes_the_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("model.snap");
        std::fs::write(&model, b"weights-v1").unwrap();

        let first = model_hash_blocking(&model).unwrap();
        assert_eq!(first, Hash::new(b"weights-v1"));
        // Second call is served from the sidecar (same result either way;
        // corrupt the sidecar hash to prove it is being *read*).
        let sidecar = dir.path().join("model.snap.gdb-b3");
        let cached = std::fs::read_to_string(&sidecar).unwrap();
        assert!(cached.ends_with(&first.to_string()));
        let forged = cached.replace(&first.to_string(), &Hash::new(b"forged").to_string());
        std::fs::write(&sidecar, forged).unwrap();
        assert_eq!(model_hash_blocking(&model).unwrap(), Hash::new(b"forged"));

        // A content change invalidates the (size,mtime) key → re-hash.
        std::fs::write(&model, b"weights-v2-longer").unwrap();
        assert_eq!(
            model_hash_blocking(&model).unwrap(),
            Hash::new(b"weights-v2-longer")
        );
    }

    // ── fake-engine e2e (supervisor, §6.4) ──────────────────────────────────
    //
    // The child is this very test binary re-invoked with `--exact` on
    // `fake_engine_process` and `GDB_FAKE_COLI` set: libtest's banner lines
    // are ignored by the tolerant parser, and the warmup scan skips
    // everything before the sentinel.

    /// Not a test: the scripted engine the e2e tests spawn. Inert unless
    /// `GDB_FAKE_COLI` is set (so `--include-ignored` runs stay safe).
    #[test]
    #[ignore = "helper process for the supervisor e2e tests"]
    fn fake_engine_process() {
        let Ok(mode) = std::env::var("GDB_FAKE_COLI") else {
            return;
        };
        fake_engine(&mode);
    }

    fn fake_engine(mode: &str) -> ! {
        use std::io::{BufRead, BufReader, Read, Write};
        if mode == "die_at_start" {
            std::process::exit(3);
        }
        let mut out = std::io::stdout();
        out.write_all(READY_SENTINEL).unwrap();
        out.write_all(b"STAT 0 0 0 0\n").unwrap();
        out.flush().unwrap();
        let mut input = BufReader::new(std::io::stdin());
        loop {
            let mut line = String::new();
            if input.read_line(&mut line).unwrap_or(0) == 0 {
                std::process::exit(0);
            }
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields.as_slice() {
                ["SUBMIT", id, _slot, len, ..] => {
                    let len: usize = len.parse().unwrap();
                    let mut payload = vec![0u8; len + 1]; // + trailing '\n'
                    input.read_exact(&mut payload).unwrap();

                    let crash_now = mode == "crash_once" && {
                        let marker = std::env::var("GDB_FAKE_COLI_MARKER").unwrap();
                        let path = std::path::Path::new(&marker);
                        if path.exists() {
                            false
                        } else {
                            std::fs::write(path, b"crashed").unwrap();
                            true
                        }
                    };
                    out.write_all(format!("DATA {id} 5\nHello\n").as_bytes())
                        .unwrap();
                    out.flush().unwrap();
                    if crash_now {
                        std::process::exit(9);
                    }
                    // Split multi-byte UTF-8 across two DATA frames on
                    // purpose: " é!" as 0x20 0xC3 | 0xA9 0x21.
                    out.write_all(b"DATA ").unwrap();
                    out.write_all(id.as_bytes()).unwrap();
                    out.write_all(b" 2\n \xC3\n").unwrap();
                    out.write_all(b"DATA ").unwrap();
                    out.write_all(id.as_bytes()).unwrap();
                    out.write_all(b" 2\n\xA9!\n").unwrap();
                    out.write_all(b"TIERS 1 2 3 0.5 0.5\n").unwrap(); // must be ignored
                    out.write_all(format!("DONE {id} STAT 4 12.5 0.0 1.0 3 0\n").as_bytes())
                        .unwrap();
                    out.flush().unwrap();
                }
                ["CANCEL", id] => {
                    out.write_all(format!("ERROR {id} CANCELLED\n").as_bytes())
                        .unwrap();
                    out.flush().unwrap();
                }
                _ => {}
            }
        }
    }

    fn fake_config(mode: &str, dir: &tempfile::TempDir) -> ColibriConfig {
        let model = dir.path().join("model.snap");
        std::fs::write(&model, b"tiny weights").unwrap();
        let mut config = ColibriConfig::new(std::env::current_exe().unwrap(), model);
        config.custom_args = Some(vec![
            "--exact".into(),
            "compute::llm::colibri::tests::fake_engine_process".into(),
            "--nocapture".into(),
            "--include-ignored".into(),
        ]);
        config.extra_env = vec![
            ("GDB_FAKE_COLI".into(), mode.into()),
            (
                "GDB_FAKE_COLI_MARKER".into(),
                dir.path().join("marker").display().to_string(),
            ),
        ];
        config.warmup_timeout = Duration::from_secs(30);
        config.default_deadline = Duration::from_secs(20);
        config.backoff_base = Duration::from_millis(50);
        config.backoff_cap = Duration::from_millis(200);
        config.restart_budget = 2;
        config.budget_window = Duration::from_secs(60);
        config
    }

    async fn wait_ready(backend: &ColibriProcessBackend) {
        tokio::time::timeout(Duration::from_secs(30), async {
            while backend.models().await.is_err() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("engine became ready");
    }

    fn request(model: &str) -> GenerateRequest {
        GenerateRequest {
            model: model.into(),
            messages: vec![ChatMessage::user("hello")],
            params: SamplingParams::default(),
            deadline: None,
        }
    }

    #[tokio::test]
    async fn supervised_engine_generates_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let backend = ColibriProcessBackend::spawn(fake_config("ok", &dir))
            .await
            .unwrap();
        wait_ready(&backend).await;

        // §6.3: the advertised hash is the model file's BLAKE3.
        let models = backend.models().await.unwrap();
        assert_eq!(models[0].content_hash, Some(Hash::new(b"tiny weights")));

        let stream = backend.generate(request("model")).await.unwrap();
        let events: Vec<GenerateEvent> = stream.map(|e| e.expect("event")).collect().await;
        assert_eq!(
            events,
            vec![
                GenerateEvent::Delta {
                    content: "Hello".into()
                },
                GenerateEvent::Delta {
                    content: " ".into()
                },
                // The split "é" arrives whole once its second byte lands.
                GenerateEvent::Delta {
                    content: "é!".into()
                },
                GenerateEvent::End {
                    finish_reason: FinishReason::Stop,
                    usage: Usage {
                        prompt_tokens: 3,
                        completion_tokens: 4
                    },
                },
            ]
        );

        // A hash-pinned request against the wrong weights fails offline.
        let mut pinned = request("model");
        pinned.model = ModelSelector {
            name: "model".into(),
            required_hash: Some(Hash::new(b"other weights")),
        };
        assert!(matches!(
            backend.generate(pinned).await,
            Err(LlmError::HashMismatch { .. })
        ));
        // And the wrong name is not served.
        assert!(matches!(
            backend.generate(request("ghost")).await,
            Err(LlmError::ModelNotFound(_))
        ));
    }

    #[tokio::test]
    async fn crash_fails_the_job_and_the_supervisor_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let backend = ColibriProcessBackend::spawn(fake_config("crash_once", &dir))
            .await
            .unwrap();
        wait_ready(&backend).await;

        // First generation: one delta, then the engine dies → the job fails
        // with BackendCrashed and is NOT silently retried (§6.4).
        let stream = backend.generate(request("model")).await.unwrap();
        let events: Vec<Result<GenerateEvent, LlmError>> = stream.collect().await;
        assert!(matches!(
            events.first(),
            Some(Ok(GenerateEvent::Delta { .. }))
        ));
        assert!(matches!(
            events.last(),
            Some(Err(LlmError::BackendCrashed(_)))
        ));

        // The supervisor restarts (marker file flips the fake to healthy);
        // the next generation succeeds.
        wait_ready(&backend).await;
        let stream = backend.generate(request("model")).await.unwrap();
        let events: Vec<GenerateEvent> = stream.map(|e| e.expect("event")).collect().await;
        assert!(matches!(events.last(), Some(GenerateEvent::End { .. })));
    }

    #[tokio::test]
    async fn crash_loop_trips_the_budget_and_reset_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let backend = ColibriProcessBackend::spawn(fake_config("die_at_start", &dir))
            .await
            .unwrap();

        // budget=2: two failed spawns trip it.
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if matches!(backend.state.lock().state, State::Tripped { .. }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("backend tripped");

        // Tripped fails fast on both probe and generate (§6.4).
        let probe = backend.models().await;
        assert!(matches!(probe, Err(LlmError::BackendUnavailable(_))));
        assert!(matches!(
            backend.generate(request("model")).await,
            Err(LlmError::BackendUnavailable(_))
        ));

        // reset() restarts supervision; the fake still dies at start, so it
        // leaves Tripped (Starting/Restarting) — proving the supervisor woke.
        backend.reset();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if !matches!(backend.state.lock().state, State::Tripped { .. }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("supervision resumed after reset");
    }
}
