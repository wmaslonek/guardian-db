# Guardian Compute — LLM Layer (Generative Inference via Pluggable Backends)

**Status:** implemented. Features `compute-llm` / `compute-llm-colibri`.
**Scope:** the `compute` feature family of the `guardian-db` crate.
**Relates to:** [RFC 0002](0002-guardian-compute.md) (Guardian Compute core),
[RFC 0003](0003-guardian-compute-followups.md) (blob-backed NN registry,
`src/compute/nn.rs`).
**Upstream systems consumed:** [colibri](https://github.com/JustVugg/colibri)
(single-machine MoE runtime in C, Apache-2.0),
[mesh-llm](https://github.com/Mesh-LLM/mesh-llm) (distributed inference over iroh,
Apache-2.0). Both are consumed, not forked.

---

## 1. Overview

The Guardian Compute core (RFC 0002) routes `TaskClass::Inference` work — the
scheduler scores accelerators and ranks peers by required model
(`rank_for`, `src/compute/scheduler.rs`), and admission gates `Inference` behind an
explicit opt-in. RFC 0003 gave that class session-based wasi-nn/ONNX graphs
(classification, embeddings, fixed-shape tensors). This layer adds the remaining
workload class: **generative LLM inference** — token streaming, chat completion,
long-running autoregressive decode. It is the piece that lets agents run near
replicated data, RAG over GuardianDB stores, and LLM-assisted triggers.

Two mature systems solve the two halves of the execution problem, and the layer
consumes both rather than reinventing either:

- **colibri** — a single-machine MoE runtime in C that runs very large MoE models on
  consumer hardware by streaming experts from disk. Answers *"how do I run this LLM on
  this machine as efficiently as possible?"* Consumed as an upstream-built binary
  driven across a process boundary (§7).
- **mesh-llm** — distributed inference over iroh that pools GPUs across machines and
  splits models too large for one box, behind one OpenAI-compatible API. Answers
  *"where in a network of machines should each part of this LLM run?"* Consumed through
  its HTTP API.

Both expose an OpenAI-compatible HTTP surface, which is why the generic HTTP backend
covers both on day one.

## 2. Scope boundaries

Deliberately out of scope:

- **No rewrite of colibri in Rust.** The core is heavily optimized C under active
  upstream development, with CUDA (`.cu`) and Metal (`.mm`) kernels that do not
  translate to Rust. It is driven as an upstream-built binary across a process
  boundary (§7).
- **No fork or vendoring of mesh-llm.** It is consumed through its OpenAI-compatible
  API (`http://localhost:9337/v1`).
- **No port of colibri's Python tooling.** The one runtime-relevant piece — the
  stdin/stdout driver inside `openai_server.py` — is reimplemented as a small native
  driver (§7); the rest is development tooling.
- **No separate `guardian-compute` repository.** The compute module is wired into
  guardian-db (protocol modeled on `p2p::network::core::ticket_exchange`, triggers
  bridged to the store `EventBus`); the LLM layer is `src/compute/llm/` behind feature
  flags, following the `compute-nn` pattern.

## 3. Architecture

```
src/compute/
├── (core) runtime, protocol, scheduler, ledger, triggers, telemetry
├── nn.rs          — session-based inference (ONNX/wasi-nn), unchanged
└── llm/           — generative inference
    ├── mod.rs         LlmBackend trait + LlmRegistry
    ├── openai.rs      generic OpenAI-compatible HTTP backend
    ├── colibri.rs     native `coli` child-process driver
    └── router.rs      local vs. distributed placement decision
```

The shape mirrors `NnModelRegistry`: the executor's **owner** curates which models the
node serves and which backend serves each, and registration is what opts the node into
accepting the corresponding work — exactly like `set_nn_models` unlocking
`TaskClass::Inference`.

### 3.1 The `LlmBackend` trait

```rust
#[async_trait]
pub trait LlmBackend: Send + Sync {
    /// Models this backend can serve right now.
    async fn models(&self) -> Result<Vec<ModelInfo>, LlmError>;
    /// Streaming generation (chat-completion shaped).
    async fn generate(&self, req: GenerateRequest) -> Result<GenerateStream, LlmError>;
    /// Where execution happens — feeds the router and the CapabilityVector.
    fn locality(&self) -> Locality; // Local | Distributed
}
```

`GenerateRequest`/`GenerateStream` follow OpenAI chat-completion semantics (messages,
sampling params, SSE-style token deltas) because every candidate backend already speaks
that dialect; a Guardian-specific dialect would only add translation layers.

### 3.2 `LlmRegistry`

Owner-curated `model name → Arc<dyn LlmBackend>` catalog, `parking_lot`-guarded like
`NnModelRegistry`. Registered names are advertised in the node's `CapabilityVector` so
`rank_for(class, required_model)` routes model-specific tasks to nodes that serve them.
Only models of **healthy** backends are advertised (§5).

### 3.3 Backends

| Backend | Kind | Transport |
|---|---|---|
| `OpenAiHttpBackend` | generic | HTTP + SSE to any OpenAI-compatible URL |
| → pointed at mesh-llm (`:9337/v1`) | distributed | same |
| → pointed at colibri's server, llama.cpp, ollama, vLLM | local | same |
| `ColibriProcessBackend` | local | child process, stdin/stdout token protocol |

The generic HTTP backend (`openai.rs`) covers both target systems. The native colibri
driver (`colibri.rs`) removes the Python dependency: colibri's `openai_server.py`
drives the `coli` binary via `subprocess.Popen` with stdin/stdout pipes; the driver
reimplements that protocol (spawn, warmup, prompt submit, token read, crash restart) in
Rust. The `coli` binary itself stays upstream-built.

### 3.4 Routing (`router.rs`)

Placement for an incoming generation request:

1. model registered locally → local backend (colibri et al.);
2. model advertised by a peer → delegate via the existing scheduler (`rank_for` +
   failover);
3. model too large for any single node, with a `Distributed` backend registered → hand
   to mesh-llm, which does its own splitting.

This keeps Guardian Compute out of mesh-llm's specialty (layer-splitting a model across
peers is *their* problem) while owning the placement decision. Every tier is *peeked*:
`route()` resolves only once the first frame of a working stream is in hand, so failover
happens before the caller sees any token. Hash-pinned requests never delegate to peers
(advertisements carry names only — see §6).

## 4. Integration with the compute core

- **Admission:** `ComputeProtocolHandler::set_llm_registry(...)`, analogous to
  `set_nn_models` — registering a non-empty registry pushes `TaskClass::Inference` into
  the accepted classes.
- **Telemetry:** served model names join the gossiped `CapabilityVector`
  (`src/compute/telemetry.rs`); only healthy backends' models are advertised (§5), and
  the existing gossip hysteresis absorbs flapping. The vector's `llm_models` field
  bumped `CAPABILITY_TOPIC` (postcard cannot decode across the field change; the topic
  is versioned like the ALPN).
- **Protocol:** the streamed variant landed as `ComputeRequest::Generate`, appended as
  the **last** postcard variant on the existing `COMPUTE_ALPN` — no ALPN bump. Older
  variants keep their indexes; a peer without `compute-llm` answers the unknown variant
  with a clean `Malformed` rejection, which the router treats as a failed attempt
  (failover before first delta). Frames are `LlmStreamFrame::{Delta, End, Error}`.
- **Executor generation ceiling:** the executor caps (or imposes) the requester's
  deadline at 30 min so a remote peer cannot hold an inference slot forever; the slot is
  held for the whole stream, so a saturated node acks itself busy.
- **Features (`Cargo.toml`):** `compute-llm = ["compute", "dep:reqwest"]` and
  `compute-llm-colibri = ["compute-llm"]` (process driver, no extra deps).

## 5. Backend liveness: active health-checking

The registry actively health-checks its backends and withdraws dead ones from the
advertised `CapabilityVector`:

- A per-backend prober calls `models()` (for HTTP backends, `GET /v1/models`) on a
  configurable interval (default 30 s, jittered).
- **N consecutive failures** (default 3) mark the backend `Unhealthy`: its models are
  removed from the advertised set, the telemetry sampler picks the change up on its next
  gossip round (hysteresis prevents flapping from thrashing the directory), and the
  local router stops selecting it.
- **One successful probe** restores `Healthy` and re-advertises. Transitions are logged;
  the last probe error is kept for diagnostics.
- Failure-on-use is the inner safety net: a request hitting a backend that died between
  probes surfaces `LlmError::BackendUnavailable`, which triggers an immediate
  out-of-cycle probe and lets the scheduler's failover retry elsewhere.

## 6. Model identity: verified hash where readable, explicit absence where not

A name string is never treated as evidence of *which weights* answered a request; a
BLAKE3 content hash is — and only where a backend can actually read the bytes.

- `ModelInfo` carries `content_hash: Option<Hash>` (BLAKE3, the convention
  `NnModelRegistry` already uses for blob-backed models).
- **Backends with local weight files advertise the hash.** The `ColibriProcessBackend`
  hashes the model file at startup, cached by `(size, mtime)` so restarts don't re-hash
  hundreds of GB.
- **Remote HTTP backends advertise `None`.** mesh-llm cannot prove its weights across
  the wire; absence of a hash is the explicit, honest signal "identity claimed by name
  only" — surfaced as such, never upgraded to verified.
- **Requesters may pin.** `ModelSelector { name, required_hash: Option<Hash> }`: a
  pinned request only resolves to backends advertising that exact hash; a name match
  with the wrong (or absent) hash is a distinct `HashMismatch` error, never a silent
  fallback. The HTTP backend rejects hash-pinned requests outright (it cannot verify
  weights); the colibri driver verifies the pin against the model file's BLAKE3.
- Scheduler matching follows the same rule: name-based by default, hash-based when the
  task pins. No global model-name registry is invented; the hash boundary is the whole
  reproducibility story until a backend can verify remote weights.

## 7. colibri process supervision

The `ColibriProcessBackend` is a full supervisor, not a spawn-and-hope wrapper:
correctness and resilience over simplicity.

- **Lifecycle.** One supervisor task owns the `coli` child: spawn → warmup (readiness
  detected from the child's startup handshake, with a deadline) → `Ready` → serving.
  `stderr` is continuously drained into tracing logs (tagged, rate-limited) so a wedged
  child is diagnosable.
- **Crash handling.** Child exit or a broken pipe fails the in-flight request with
  `LlmError::BackendCrashed` — never silently retried, since generation is
  non-deterministic — and schedules a restart with **exponential backoff + jitter**
  (base 1 s, cap 60 s). A **restart budget** (default 5 failures per 10-minute window)
  trips the backend into `Unhealthy` permanently until the owner intervenes: repeated
  crashes usually mean a bad model file or OOM, which retrying cannot fix. `reset()`
  un-trips a tripped supervisor.
- **Request discipline.** The `coli` process is single-stream; the driver serializes
  requests through a **bounded queue** (default depth 32) with per-request deadlines
  covering queue wait + generation. Queue-full and deadline-exceeded are distinct,
  immediate errors — a saturated node fails fast so the scheduler routes around it
  rather than accumulating latency. Cancellation (requester drops the stream) kills the
  current generation: the driver sends colibri's interrupt if the protocol supports it,
  else restarts the child.
- **Health integration.** Supervisor state (`Starting`/`Ready`/`Restarting`/`Tripped`)
  is what §5's prober reads for this backend — no separate HTTP probe for a child the
  node already owns.
- **Shutdown.** Graceful drain on node shutdown: stop accepting, let the in-flight
  request finish within a drain deadline, then SIGTERM/kill.
- **Protocol.** Speaks the `SUBMIT`/`DATA`/`DONE`/`ERROR`/`CANCEL` stdin/stdout protocol
  of `openai_server.py` directly, GLM prompt template included. Unknown engine stdout
  lines are logged and ignored rather than fatal (robustness to engine-version drift).
- Concurrency beyond one child (a worker pool over multiple `coli` processes) is a
  config knob the queue design makes additive.

## 8. Guest surface: the `gdb.llm_generate` host grant

An opt-in host grant `gdb.llm` (in `HostGrants`) exposes generation to WASM tasks;
`guardian-compute-sdk` gains a matching `host::llm_generate` helper.

- **ABI:** `gdb.llm_generate(model_ptr, model_len, prompt_ptr, prompt_len) -> i64` —
  buffered (guests are synchronous; streaming is the protocol's job). The response is
  written into a buffer obtained by re-entrantly calling the guest's own `gdb_alloc`,
  its location returned as `(ptr << 32) | len`, `-1` on failure.
- **Grant-level limits:** 5 min deadline, 1 MiB response cap (over-cap generation is
  cancelled by dropping the stream). Attached automatically to `Inference`-class tasks
  when `set_llm_registry` is configured.
- **Determinism:** LLM-using tasks are non-deterministic and therefore ineligible for
  k-of-n redundant execution — the same rule RFC 0002 applies to `Inference`-class
  work. The sandbox epoch clock keeps ticking during the native call, so LLM-using
  tasks need a generous `ResourceLimits::timeout_ms` (documented in the grant).

Files: `src/compute/llm/{mod,openai,colibri,router}.rs`, the grant in
`src/compute/runtime.rs` (`LlmGrant`), protocol additions in `src/compute/protocol.rs`,
the SDK helper `guardian_compute_sdk::host::llm_generate`, and the demo
`examples/llm_inference_demo.rs`.
