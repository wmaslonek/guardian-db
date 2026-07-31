//! # WASM sandbox runtime (Phase 1)
//!
//! Executes untrusted business-logic modules under the hard ceilings of a
//! [`ResourceLimits`]: linear-memory cap (via a `ResourceLimiter`), CPU budget
//! (wasmtime fuel) and wall-clock deadline (epoch interruption driven by a
//! background ticker thread). No WASI is linked: a guest sees nothing but its
//! own memory — no filesystem, no network, no clock, no environment.
//!
//! ## Guest ABI
//!
//! A task module must export:
//!
//! - `memory` — its linear memory;
//! - `gdb_alloc: (len: i32) -> i32` — returns an offset where the host writes
//!   the input bytes;
//! - the entrypoint named in the [`TaskSpec`](super::TaskSpec), with signature
//!   `(ptr: i32, len: i32) -> i64` — receives the input location and returns
//!   the output location packed as `(out_ptr << 32) | out_len`.
//!
//! Input and output are opaque byte strings by decision §8.2 of the RFC: the
//! protocol and this runtime attach no meaning to them (the future SDK adopts
//! CBOR as its convention, invisible at this layer).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use wasmtime::{Caller, Config, Engine, Linker, Module, ResourceLimiter, Store, Trap};

use super::ResourceLimits;

/// Export name of the guest allocator (fixed by the ABI).
pub const ABI_ALLOC_EXPORT: &str = "gdb_alloc";
/// Export name of the guest linear memory (fixed by the ABI).
pub const ABI_MEMORY_EXPORT: &str = "memory";

/// Granularity of the wall-clock deadline: the epoch ticker fires at this
/// interval, so timeouts are enforced within roughly one tick.
const EPOCH_TICK: Duration = Duration::from_millis(10);

/// Why a task run failed. Serializable so Phase 2 can carry it verbatim in
/// `ExecuteResult` back to the requester.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum TaskError {
    #[error("invalid WebAssembly module: {0}")]
    InvalidModule(String),
    #[error("wasm module unavailable: {0}")]
    WasmUnavailable(String),
    #[error("module does not export `{0}` with the expected ABI signature")]
    MissingExport(String),
    #[error("CPU budget exhausted (fuel limit)")]
    FuelExhausted,
    #[error("wall-clock deadline exceeded")]
    DeadlineExceeded,
    #[error("memory limit exceeded")]
    MemoryLimitExceeded,
    #[error("guest violated the ABI: {0}")]
    AbiViolation(String),
    #[error("module imports a host capability the executor did not grant: {0}")]
    HostCapabilityDenied(String),
    #[error("guest trapped: {0}")]
    Trapped(String),
    #[error("runtime error: {0}")]
    Runtime(String),
}

impl TaskError {
    /// Whether this failure is *node-specific* — a different executor might
    /// well succeed — so the scheduler should fail over to the next candidate
    /// rather than treat the task as permanently broken.
    ///
    /// Deterministic failures (fuel/deadline/memory limits, a guest trap, a
    /// bad module, an ABI violation) would recur identically on any node, so
    /// they are final. Transient/node-local ones are:
    /// - [`WasmUnavailable`](Self::WasmUnavailable): this node couldn't fetch
    ///   the blob; another provider may have it.
    /// - [`HostCapabilityDenied`](Self::HostCapabilityDenied): this node lacks
    ///   a granted capability; another may have granted it.
    /// - [`Runtime`](Self::Runtime): executor-side infrastructure hiccup
    ///   (e.g. a panicked worker), not the task's fault.
    /// - [`MissingExport`](Self::MissingExport): almost always deterministic,
    ///   but cheap to re-try elsewhere and harmless if it recurs.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            TaskError::WasmUnavailable(_)
                | TaskError::HostCapabilityDenied(_)
                | TaskError::Runtime(_)
                | TaskError::MissingExport(_)
        )
    }
}

/// What one run actually cost, reported alongside the output (and, from
/// Phase 2 on, sent back to the requester in `ExecuteResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecMetrics {
    /// Fuel units consumed (objective measure of CPU work done).
    pub fuel_consumed: u64,
    /// Wall-clock duration of the run.
    pub duration_ms: u64,
    /// Highest linear-memory size the guest reached, in bytes.
    pub peak_memory_bytes: u64,
}

/// Successful result of one task run.
#[derive(Debug, Clone)]
pub struct Execution {
    /// Opaque output bytes produced by the guest (§8.2: meaning is a contract
    /// between module author and requester, not the runtime's business).
    pub output: Vec<u8>,
    pub metrics: ExecMetrics,
}

/// A validated, compiled task module, reusable across many runs.
///
/// Compilation (Cranelift JIT) is the expensive step; executors cache these
/// keyed by the blob hash so repeated tasks skip it.
#[derive(Debug, Clone)]
pub struct CompiledTask {
    module: Module,
}

/// Read access to local data a guest may be granted (RFC §5.4 / Phase 5).
///
/// Synchronous by design: host functions run inside the (blocking) sandbox
/// call, so implementations should answer from memory or a local cache, not
/// from the network.
pub trait HostStoreReader: Send + Sync + 'static {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>>;
}

/// The host capabilities an executor grants to a task run (Phase 5).
///
/// Everything defaults to **off**: an ungranted sandbox exposes nothing but
/// input/output bytes, which also keeps runs deterministic — the property the
/// k-of-n redundant mode depends on. A module that imports a host function
/// which was not granted fails instantiation with
/// [`TaskError::HostCapabilityDenied`]. Clock and randomness are deliberately
/// not offered at all.
///
/// Guest imports live under the `"gdb"` module:
/// - `log(ptr, len)` — emits a UTF-8 message into the host's tracing.
/// - `store_get(key_ptr, key_len, dest_ptr, dest_cap) -> i32` — copies the
///   value for the key into the destination buffer, returning the value's
///   full length (`-1` when absent); the guest re-calls with a bigger buffer
///   if the value was truncated.
/// - `llm_generate(model_ptr, model_len, prompt_ptr, prompt_len) -> i64`
///   (feature `compute-llm`, RFC 0004 phase 5) — buffered LLM generation:
///   the response is written into a guest buffer obtained by re-entrantly
///   calling the guest's own `gdb_alloc`, and its location is returned
///   packed as `(ptr << 32) | len`; `-1` on failure.
///
/// With the `compute-nn` feature, [`HostGrants::nn`] additionally links the
/// `wasi_ephemeral_nn` API (Edge AI, RFC 0003 Part A).
#[derive(Clone, Default)]
pub struct HostGrants {
    /// Grant the `gdb.log` import.
    pub log: bool,
    /// Grant the `gdb.store_get` import, answered by this reader.
    pub store: Option<Arc<dyn HostStoreReader>>,
    /// Grant the `wasi_ephemeral_nn` imports, serving these named models.
    #[cfg(feature = "compute-nn")]
    pub nn: Option<Arc<NnGrant>>,
    /// Grant the `gdb.llm_generate` import (RFC 0004 phase 5).
    #[cfg(feature = "compute-llm")]
    pub llm: Option<Arc<LlmGrant>>,
}

impl std::fmt::Debug for HostGrants {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut dbg = f.debug_struct("HostGrants");
        dbg.field("log", &self.log)
            .field("store", &self.store.is_some());
        #[cfg(feature = "compute-nn")]
        dbg.field("nn", &self.nn.is_some());
        #[cfg(feature = "compute-llm")]
        dbg.field("llm", &self.llm.is_some());
        dbg.finish()
    }
}

/// The LLM grant (feature `compute-llm`, RFC 0004 phase 5): guests reach the
/// executor's [`LlmRegistry`](crate::compute::llm::LlmRegistry) through
/// `gdb.llm_generate`.
///
/// Deliberate constraints:
/// - **Buffered.** Guests are synchronous; the host collects the token
///   stream and hands the complete response back in one call. Streaming to
///   *requesters* is the protocol's job (RFC 0004 §6.2), not the guest's.
/// - **Non-deterministic.** Tasks importing this are ineligible for k-of-n
///   redundant execution, like any `Inference`-class work.
/// - Generation blocks inside a native host call, which fuel does not meter
///   and epoch interruption cannot abort mid-call; the grant's own
///   [`deadline`](Self::with_limits) bounds it, and the protocol layer's
///   response deadline is the outer net. Tasks calling this should set
///   `ResourceLimits::timeout_ms` generously — the epoch clock keeps ticking
///   while the host call runs.
#[cfg(feature = "compute-llm")]
pub struct LlmGrant {
    registry: Arc<crate::compute::llm::LlmRegistry>,
    /// Bridges the async registry into the synchronous host call. Captured
    /// at construction; the blocking `execute_with_host` thread must NOT be
    /// a tokio worker (the protocol layer runs it in `spawn_blocking`).
    handle: tokio::runtime::Handle,
    /// Per-call generation deadline.
    deadline: Duration,
    /// Cap on response bytes written into the guest; generation past it is
    /// cancelled (dropping the stream) and the text truncated at a char
    /// boundary.
    max_response_bytes: usize,
}

#[cfg(feature = "compute-llm")]
impl LlmGrant {
    /// Grant over `registry` with the defaults (5 min deadline, 1 MiB
    /// response cap). Must be called from async context (captures the
    /// current tokio runtime handle).
    pub fn new(registry: Arc<crate::compute::llm::LlmRegistry>) -> Self {
        Self::with_limits(registry, Duration::from_secs(5 * 60), 1024 * 1024)
    }

    /// [`new`](Self::new) with explicit per-call limits.
    pub fn with_limits(
        registry: Arc<crate::compute::llm::LlmRegistry>,
        deadline: Duration,
        max_response_bytes: usize,
    ) -> Self {
        Self {
            registry,
            handle: tokio::runtime::Handle::current(),
            deadline,
            max_response_bytes,
        }
    }

    /// Runs one buffered generation, blocking the calling (non-async) thread.
    fn generate_blocking(
        &self,
        model: String,
        prompt: String,
    ) -> Result<String, crate::compute::llm::LlmError> {
        use crate::compute::llm::{ChatMessage, GenerateEvent, GenerateRequest, SamplingParams};
        use futures::StreamExt;

        let registry = self.registry.clone();
        let deadline = self.deadline;
        let max = self.max_response_bytes;
        self.handle.block_on(async move {
            let request = GenerateRequest {
                model: model.into(),
                messages: vec![ChatMessage::user(prompt)],
                params: SamplingParams::default(),
                deadline: Some(deadline),
            };
            let mut stream = registry.generate(request).await?;
            let mut text = String::new();
            while let Some(event) = stream.next().await {
                match event? {
                    GenerateEvent::Delta { content } => {
                        text.push_str(&content);
                        if text.len() >= max {
                            // Cap reached: dropping the stream cancels the
                            // generation (§6.4); truncate on a boundary.
                            let mut cut = max.min(text.len());
                            while !text.is_char_boundary(cut) {
                                cut -= 1;
                            }
                            text.truncate(cut);
                            return Ok(text);
                        }
                    }
                    GenerateEvent::End { .. } => return Ok(text),
                }
            }
            Err(crate::compute::llm::LlmError::Protocol(
                "stream ended without terminal frame".into(),
            ))
        })
    }
}

#[cfg(feature = "compute-llm")]
impl std::fmt::Debug for LlmGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmGrant")
            .field("registry", &self.registry)
            .field("deadline", &self.deadline)
            .finish()
    }
}

/// The wasi-nn grant (feature `compute-nn`, RFC 0003 phases NN-1/NN-2): the
/// ONNX models this executor offers, referenced by the guest via
/// `load_by_name`.
///
/// Deliberate constraints:
/// - **Named models only.** No backend is linked for the guest's raw `load`
///   call (it fails with an errno); only the curated registry is reachable —
///   the executor's owner decides what runs, consistent with the §8.3
///   sovereignty rule.
/// - **CPU execution.** GPU execution providers arrive with `compute-nn-cuda`
///   (phase NN-4).
/// - Inference runs inside a native host call, which fuel does not meter and
///   epoch interruption cannot abort mid-call; the protocol layer adds a
///   response-level deadline as the safety net (see RFC 0003 §A.3).
///
/// Loaded graphs (ONNX Runtime sessions) are cached inside the grant after
/// the first run — share one `Arc<NnGrant>` across executions to reuse them.
#[cfg(feature = "compute-nn")]
pub struct NnGrant {
    /// `(name, ONNX model bytes)` pairs served via `load_by_name`.
    models: Vec<(String, Vec<u8>)>,
    /// Where the models execute (phase NN-4); CPU by default.
    target: NnTarget,
    /// Lazily loaded graphs; built once, shared by later runs.
    prepared:
        parking_lot::Mutex<Option<std::collections::HashMap<String, wasmtime_wasi_nn::Graph>>>,
}

#[cfg(feature = "compute-nn")]
impl NnGrant {
    /// Grant serving these in-memory models (loaded on first use, on CPU).
    pub fn new(models: Vec<(String, Vec<u8>)>) -> Self {
        Self::new_with_target(models, NnTarget::Cpu)
    }

    /// Grant serving these in-memory models on the given execution target
    /// (phase NN-4).
    pub fn new_with_target(models: Vec<(String, Vec<u8>)>, target: NnTarget) -> Self {
        Self {
            models,
            target,
            prepared: parking_lot::Mutex::new(None),
        }
    }

    /// Grant over graphs that are already loaded (the
    /// [`NnModelRegistry`](super::nn::NnModelRegistry) path, phase NN-2).
    pub(crate) fn from_graphs(
        graphs: std::collections::HashMap<String, wasmtime_wasi_nn::Graph>,
    ) -> Self {
        Self {
            models: Vec::new(),
            target: NnTarget::Cpu, // irrelevant: graphs are already loaded
            prepared: parking_lot::Mutex::new(Some(graphs)),
        }
    }

    /// Whether this grant serves the named model.
    pub fn has_model(&self, name: &str) -> bool {
        if self.models.iter().any(|(n, _)| n == name) {
            return true;
        }
        self.prepared
            .lock()
            .as_ref()
            .is_some_and(|graphs| graphs.contains_key(name))
    }

    /// The loaded graphs, building (and caching) them on first use.
    fn graphs(
        &self,
    ) -> Result<std::collections::HashMap<String, wasmtime_wasi_nn::Graph>, TaskError> {
        let mut prepared = self.prepared.lock();
        if let Some(graphs) = prepared.as_ref() {
            return Ok(graphs.clone());
        }
        let mut graphs = std::collections::HashMap::new();
        for (name, bytes) in &self.models {
            graphs.insert(name.clone(), load_onnx_graph(name, bytes, self.target)?);
        }
        *prepared = Some(graphs.clone());
        Ok(graphs)
    }
}

#[cfg(feature = "compute-nn")]
impl std::fmt::Debug for NnGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NnGrant")
            .field(
                "models",
                &self.models.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            )
            .field("prepared", &self.prepared.lock().is_some())
            .finish()
    }
}

/// Where NN models execute (RFC 0003 phase NN-4).
///
/// `Gpu` selects the CUDA execution provider when the `compute-nn-cuda`
/// feature is enabled; without it (or without a working CUDA setup) the
/// backend falls back to CPU with a warning — requesting GPU is always safe.
#[cfg(feature = "compute-nn")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NnTarget {
    #[default]
    Cpu,
    Gpu,
}

#[cfg(feature = "compute-nn")]
impl NnTarget {
    fn to_wit(self) -> wasmtime_wasi_nn::wit::ExecutionTarget {
        match self {
            NnTarget::Cpu => wasmtime_wasi_nn::wit::ExecutionTarget::Cpu,
            NnTarget::Gpu => wasmtime_wasi_nn::wit::ExecutionTarget::Gpu,
        }
    }
}

/// Loads one ONNX model into a backend graph (an ONNX Runtime session).
#[cfg(feature = "compute-nn")]
pub(crate) fn load_onnx_graph(
    name: &str,
    bytes: &[u8],
    target: NnTarget,
) -> Result<wasmtime_wasi_nn::Graph, TaskError> {
    let mut backend =
        wasmtime_wasi_nn::Backend::from(wasmtime_wasi_nn::backend::onnx::OnnxBackend::default());
    backend
        .load(&[bytes], target.to_wit())
        .map_err(|e| TaskError::Runtime(format!("loading NN model `{name}`: {e}")))
}

/// Per-store state: the memory ceiling enforced during execution, plus the
/// wasi-nn context when that capability was granted.
struct HostState {
    ceiling: MemoryCeiling,
    #[cfg(feature = "compute-nn")]
    nn: Option<wasmtime_wasi_nn::witx::WasiNnCtx>,
}

/// `ResourceLimiter` that refuses linear-memory growth past the task's cap,
/// remembering both the peak reached and whether a request was denied (so a
/// subsequent guest trap can be attributed to the memory limit).
struct MemoryCeiling {
    max_bytes: usize,
    peak_bytes: usize,
    denied: bool,
}

impl ResourceLimiter for MemoryCeiling {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > self.max_bytes {
            self.denied = true;
            Ok(false)
        } else {
            self.peak_bytes = self.peak_bytes.max(desired);
            Ok(true)
        }
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        // Tables hold function references, not data; a generous fixed cap
        // keeps pathological modules from ballooning host memory.
        Ok(desired <= 100_000)
    }
}

/// Sandboxed executor of task modules.
///
/// Owns one wasmtime [`Engine`] plus the background thread that advances its
/// epoch every [`EPOCH_TICK`], which is what makes wall-clock deadlines
/// enforceable on non-cooperative guests. Cheap to share (`Engine` is
/// internally reference-counted); create it once per process.
///
/// Execution is synchronous and CPU-bound — from async code, wrap calls in
/// `tokio::task::spawn_blocking`.
pub struct WasmRuntime {
    engine: Engine,
    ticker_stop: Arc<AtomicBool>,
}

impl WasmRuntime {
    pub fn new() -> Result<Self, TaskError> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.epoch_interruption(true);
        let engine =
            Engine::new(&config).map_err(|e| TaskError::Runtime(format!("engine init: {e}")))?;

        let ticker_stop = Arc::new(AtomicBool::new(false));
        {
            let engine = engine.clone();
            let stop = ticker_stop.clone();
            std::thread::Builder::new()
                .name("gdb-compute-epoch".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(EPOCH_TICK);
                        engine.increment_epoch();
                    }
                })
                .map_err(|e| TaskError::Runtime(format!("epoch ticker spawn: {e}")))?;
        }

        Ok(Self {
            engine,
            ticker_stop,
        })
    }

    /// Compiles and validates a module from raw `.wasm` bytes.
    pub fn compile(&self, wasm: &[u8]) -> Result<CompiledTask, TaskError> {
        let module =
            Module::new(&self.engine, wasm).map_err(|e| TaskError::InvalidModule(e.to_string()))?;
        Ok(CompiledTask { module })
    }

    /// Runs `entrypoint` of a compiled task with `input`, enforcing `limits`,
    /// with no host capabilities granted (the deterministic default).
    pub fn execute(
        &self,
        task: &CompiledTask,
        entrypoint: &str,
        input: &[u8],
        limits: &ResourceLimits,
    ) -> Result<Execution, TaskError> {
        self.execute_with_host(task, entrypoint, input, limits, &HostGrants::default())
    }

    /// Runs `entrypoint` of a compiled task with `input`, enforcing `limits`
    /// and linking exactly the host functions in `grants` (Phase 5).
    ///
    /// Every run gets a fresh `Store` (fresh guest memory and globals): tasks
    /// share compiled code but never state.
    pub fn execute_with_host(
        &self,
        task: &CompiledTask,
        entrypoint: &str,
        input: &[u8],
        limits: &ResourceLimits,
        grants: &HostGrants,
    ) -> Result<Execution, TaskError> {
        let started = Instant::now();

        // NN-1 note: the wasi-nn context (and its ONNX sessions) is built per
        // run. Fine for small models; the blob-backed model registry of phase
        // NN-2 is where session caching lands.
        #[cfg(feature = "compute-nn")]
        let nn_ctx = match &grants.nn {
            Some(grant) => Some(build_wasi_nn_ctx(grant)?),
            None => None,
        };

        let mut store = Store::new(
            &self.engine,
            HostState {
                ceiling: MemoryCeiling {
                    max_bytes: usize::try_from(limits.max_memory_bytes).unwrap_or(usize::MAX),
                    peak_bytes: 0,
                    denied: false,
                },
                #[cfg(feature = "compute-nn")]
                nn: nn_ctx,
            },
        );
        store.limiter(|state| &mut state.ceiling);
        store
            .set_fuel(limits.fuel)
            .map_err(|e| TaskError::Runtime(format!("set_fuel: {e}")))?;
        // Deadline in ticks of the background ticker; at least one tick so a
        // zero timeout still means "almost immediately", never "forever".
        store.set_epoch_deadline(
            limits
                .timeout_ms
                .div_ceil(EPOCH_TICK.as_millis() as u64)
                .max(1),
        );

        let mut linker: Linker<HostState> = Linker::new(&self.engine);
        if grants.log {
            linker
                .func_wrap(
                    "gdb",
                    "log",
                    |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| {
                        if let Some(message) = read_guest_bytes(&mut caller, ptr, len) {
                            tracing::info!(target: "guardian_compute_guest",
                                           "{}", String::from_utf8_lossy(&message));
                        }
                    },
                )
                .map_err(|e| TaskError::Runtime(format!("link gdb.log: {e}")))?;
        }
        if let Some(reader) = grants.store.clone() {
            linker
                .func_wrap(
                    "gdb",
                    "store_get",
                    move |mut caller: Caller<'_, HostState>,
                          key_ptr: i32,
                          key_len: i32,
                          dest_ptr: i32,
                          dest_cap: i32|
                          -> i32 {
                        let Some(key) = read_guest_bytes(&mut caller, key_ptr, key_len) else {
                            return -1;
                        };
                        let Some(value) = reader.get(&key) else {
                            return -1;
                        };
                        let full_len = value.len().min(i32::MAX as usize) as i32;
                        let writable = value.len().min(dest_cap.max(0) as usize);
                        if writable > 0
                            && !write_guest_bytes(&mut caller, dest_ptr, &value[..writable])
                        {
                            return -1;
                        }
                        full_len
                    },
                )
                .map_err(|e| TaskError::Runtime(format!("link gdb.store_get: {e}")))?;
        }

        // gdb.llm_generate (feature `compute-llm`, RFC 0004 phase 5): linked
        // only when granted; the response buffer is allocated by re-entrantly
        // calling the guest's own `gdb_alloc`.
        #[cfg(feature = "compute-llm")]
        if let Some(grant) = grants.llm.clone() {
            linker
                .func_wrap(
                    "gdb",
                    "llm_generate",
                    move |mut caller: Caller<'_, HostState>,
                          model_ptr: i32,
                          model_len: i32,
                          prompt_ptr: i32,
                          prompt_len: i32|
                          -> i64 {
                        let Some(model) = read_guest_bytes(&mut caller, model_ptr, model_len)
                        else {
                            return -1;
                        };
                        let Some(prompt) = read_guest_bytes(&mut caller, prompt_ptr, prompt_len)
                        else {
                            return -1;
                        };
                        let model = String::from_utf8_lossy(&model).into_owned();
                        let prompt = String::from_utf8_lossy(&prompt).into_owned();
                        let response = match grant.generate_blocking(model, prompt) {
                            Ok(text) => text,
                            Err(e) => {
                                tracing::warn!(target: "guardian_compute_guest",
                                               "llm_generate failed: {e}");
                                return -1;
                            }
                        };
                        write_into_guest_alloc(&mut caller, response.as_bytes())
                    },
                )
                .map_err(|e| TaskError::Runtime(format!("link gdb.llm_generate: {e}")))?;
        }

        // wasi-nn (feature `compute-nn`): linked only when granted, so a
        // module importing `wasi_ephemeral_nn` on an ungranted executor is
        // refused at instantiation like any other capability.
        #[cfg(feature = "compute-nn")]
        if store.data().nn.is_some() {
            wasmtime_wasi_nn::witx::add_to_linker(&mut linker, |state: &mut HostState| {
                state.nn.as_mut().expect("nn context present when granted")
            })
            .map_err(|e| TaskError::Runtime(format!("link wasi-nn: {e}")))?;
        }

        // Detect ungranted capabilities deterministically, before instantiating:
        // any import the linker cannot satisfy is a host capability the policy
        // withheld. This replaces matching on wasmtime's error text, which would
        // break silently if the wording changed across versions.
        for import in task.module.imports() {
            if linker.get_by_import(&mut store, &import).is_none() {
                return Err(TaskError::HostCapabilityDenied(format!(
                    "{}::{}",
                    import.module(),
                    import.name()
                )));
            }
        }

        let instance = linker
            .instantiate(&mut store, &task.module)
            .map_err(|e| classify_error(&store, e))?;

        let memory = instance
            .get_memory(&mut store, ABI_MEMORY_EXPORT)
            .ok_or_else(|| TaskError::MissingExport(ABI_MEMORY_EXPORT.into()))?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, ABI_ALLOC_EXPORT)
            .map_err(|_| TaskError::MissingExport(ABI_ALLOC_EXPORT.into()))?;
        let run = instance
            .get_typed_func::<(i32, i32), i64>(&mut store, entrypoint)
            .map_err(|_| TaskError::MissingExport(entrypoint.into()))?;

        // Hand the input to the guest through its own allocator.
        let input_len = i32::try_from(input.len())
            .map_err(|_| TaskError::AbiViolation("input too large".into()))?;
        let input_ptr = alloc
            .call(&mut store, input_len)
            .map_err(|e| classify_error(&store, e))?;
        if !input.is_empty() {
            let offset = u64::try_from(input_ptr).map_err(|_| {
                TaskError::AbiViolation("allocator returned a negative offset".into())
            })?;
            memory
                .write(&mut store, offset as usize, input)
                .map_err(|_| {
                    TaskError::AbiViolation("allocator returned an out-of-bounds buffer".into())
                })?;
        }

        let packed = run
            .call(&mut store, (input_ptr, input_len))
            .map_err(|e| classify_error(&store, e))?;

        // Unpack `(out_ptr << 32) | out_len` and copy the output out.
        // `out_len` is guest-controlled (up to 4 GiB); validate the range
        // against the guest's linear memory BEFORE allocating the host buffer,
        // or a hostile module returning a huge length would OOM the executor
        // (the default no-grant path, so this guards the reciprocity surface).
        let out_ptr = (packed >> 32) as u32 as usize;
        let out_len = packed as u32 as usize;
        if out_ptr
            .checked_add(out_len)
            .is_none_or(|end| end > memory.data_size(&store))
        {
            return Err(TaskError::AbiViolation(
                "entrypoint returned an out-of-bounds output".into(),
            ));
        }
        let mut output = vec![0u8; out_len];
        if out_len > 0 {
            memory.read(&store, out_ptr, &mut output).map_err(|_| {
                TaskError::AbiViolation("entrypoint returned an out-of-bounds output".into())
            })?;
        }

        let fuel_left = store.get_fuel().unwrap_or(0);
        Ok(Execution {
            output,
            metrics: ExecMetrics {
                fuel_consumed: limits.fuel.saturating_sub(fuel_left),
                duration_ms: started.elapsed().as_millis() as u64,
                peak_memory_bytes: store.data().ceiling.peak_bytes as u64,
            },
        })
    }
}

impl Drop for WasmRuntime {
    fn drop(&mut self) {
        self.ticker_stop.store(true, Ordering::Relaxed);
    }
}

/// Builds the wasi-nn context for one run: a registry serving exactly the
/// models the executor's owner granted, by name. **No backend is linked**, so
/// the guest's raw `load` (arbitrary model bytes) fails with an errno and
/// only `load_by_name` over the curated registry works.
#[cfg(feature = "compute-nn")]
fn build_wasi_nn_ctx(grant: &NnGrant) -> Result<wasmtime_wasi_nn::witx::WasiNnCtx, TaskError> {
    Ok(wasmtime_wasi_nn::witx::WasiNnCtx::new(
        std::iter::empty::<wasmtime_wasi_nn::Backend>(),
        NamedModels(grant.graphs()?).into(),
    ))
}

/// Registry mapping owner-granted names to loaded graphs — the only models a
/// guest's `load_by_name` can reach.
#[cfg(feature = "compute-nn")]
struct NamedModels(std::collections::HashMap<String, wasmtime_wasi_nn::Graph>);

#[cfg(feature = "compute-nn")]
impl wasmtime_wasi_nn::GraphRegistry for NamedModels {
    fn get(&self, name: &str) -> Option<&wasmtime_wasi_nn::Graph> {
        self.0.get(name)
    }

    fn get_mut(&mut self, name: &str) -> Option<&mut wasmtime_wasi_nn::Graph> {
        self.0.get_mut(name)
    }
}

/// Reads `len` bytes at `ptr` from the calling guest's exported memory.
///
/// The `[ptr, ptr+len)` range is validated against the guest's linear-memory
/// size **before** allocating the host buffer: `len` is a guest-controlled
/// i32 (up to ~2 GiB), so allocating first would let a hostile guest exhaust
/// host memory with a single `log`/`store_get` call naming a huge length.
fn read_guest_bytes(caller: &mut Caller<'_, HostState>, ptr: i32, len: i32) -> Option<Vec<u8>> {
    let memory = caller.get_export(ABI_MEMORY_EXPORT)?.into_memory()?;
    let (ptr, len) = (usize::try_from(ptr).ok()?, usize::try_from(len).ok()?);
    if ptr.checked_add(len)? > memory.data_size(&caller) {
        return None; // out of bounds — don't allocate on the guest's word
    }
    let mut buffer = vec![0u8; len];
    memory.read(caller, ptr, &mut buffer).ok()?;
    Some(buffer)
}

/// Allocates via the guest's own `gdb_alloc` (a re-entrant call back into the
/// instance) and writes `bytes` there, returning the location packed as
/// `(ptr << 32) | len` — the same convention as the entrypoint's return
/// value. Returns `-1` on any failure (missing allocator, trap, OOB write).
#[cfg(feature = "compute-llm")]
fn write_into_guest_alloc(caller: &mut Caller<'_, HostState>, bytes: &[u8]) -> i64 {
    if bytes.is_empty() {
        return 0;
    }
    let Ok(len) = i32::try_from(bytes.len()) else {
        return -1;
    };
    let Some(alloc) = caller
        .get_export(ABI_ALLOC_EXPORT)
        .and_then(|e| e.into_func())
    else {
        return -1;
    };
    let Ok(alloc) = alloc.typed::<i32, i32>(&mut *caller) else {
        return -1;
    };
    let Ok(ptr) = alloc.call(&mut *caller, len) else {
        return -1;
    };
    if ptr < 0 || !write_guest_bytes(caller, ptr, bytes) {
        return -1;
    }
    ((ptr as u32 as i64) << 32) | (len as u32 as i64)
}

/// Writes `bytes` at `ptr` into the calling guest's exported memory.
fn write_guest_bytes(caller: &mut Caller<'_, HostState>, ptr: i32, bytes: &[u8]) -> bool {
    let Some(memory) = caller
        .get_export(ABI_MEMORY_EXPORT)
        .and_then(|e| e.into_memory())
    else {
        return false;
    };
    let Ok(ptr) = usize::try_from(ptr) else {
        return false;
    };
    memory.write(caller, ptr, bytes).is_ok()
}

/// Maps a wasmtime error from instantiation or a guest call to the [`TaskError`]
/// the requester should see: which *limit* fired matters more than the raw trap.
fn classify_error(store: &Store<HostState>, err: wasmtime::Error) -> TaskError {
    // Fuel/deadline are unambiguous — check them first. A guest can survive a
    // denied `memory.grow` (it returns -1, execution continues) and only later
    // exhaust fuel or the wall clock; that later trap must not be mislabeled as
    // a memory-limit failure just because the `denied` flag is still set.
    match err.downcast_ref::<Trap>() {
        Some(Trap::OutOfFuel) => return TaskError::FuelExhausted,
        Some(Trap::Interrupt) => return TaskError::DeadlineExceeded,
        _ => {}
    }
    // For any other trap, a denied memory growth is the most likely true cause
    // (an allocator abort after being refused more linear memory).
    if store.data().ceiling.denied {
        return TaskError::MemoryLimitExceeded;
    }
    match err.downcast_ref::<Trap>() {
        Some(trap) => TaskError::Trapped(trap.to_string()),
        None => TaskError::Runtime(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Well-behaved guest: a bump allocator starting at offset 1024, and a
    /// `gdb_run` that echoes the input back (returns the very same ptr/len).
    const ECHO_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (global $next (mut i32) (i32.const 1024))
          (func (export "gdb_alloc") (param $len i32) (result i32)
            (local $ptr i32)
            (local.set $ptr (global.get $next))
            (global.set $next (i32.add (global.get $next) (local.get $len)))
            (local.get $ptr))
          (func (export "gdb_run") (param $ptr i32) (param $len i32) (result i64)
            (i64.or
              (i64.shl (i64.extend_i32_u (local.get $ptr)) (i64.const 32))
              (i64.extend_i32_u (local.get $len)))))
    "#;

    /// Hostile guest: spins forever. Must be stopped by fuel or deadline.
    const SPIN_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "gdb_alloc") (param i32) (result i32) (i32.const 1024))
          (func (export "gdb_run") (param i32 i32) (result i64)
            (loop $spin (br $spin))
            (i64.const 0)))
    "#;

    /// Greedy guest: tries to grow memory by 100 pages (~6.4 MiB) and writes
    /// the result of `memory.grow` (-1 when denied) as its 4-byte output.
    const HOG_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "gdb_alloc") (param i32) (result i32) (i32.const 1024))
          (func (export "gdb_run") (param i32 i32) (result i64)
            (i32.store (i32.const 1024) (memory.grow (i32.const 100)))
            (i64.or (i64.shl (i64.const 1024) (i64.const 32)) (i64.const 4))))
    "#;

    fn runtime() -> WasmRuntime {
        WasmRuntime::new().expect("runtime")
    }

    fn compile(rt: &WasmRuntime, wat: &str) -> CompiledTask {
        rt.compile(&wat::parse_str(wat).expect("valid wat"))
            .expect("compile")
    }

    #[test]
    fn echo_module_returns_input_and_reports_metrics() {
        let rt = runtime();
        let task = compile(&rt, ECHO_WAT);
        let input = b"ola guardian compute";

        let exec = rt
            .execute(&task, "gdb_run", input, &ResourceLimits::default())
            .expect("execution");

        assert_eq!(exec.output, input);
        assert!(exec.metrics.fuel_consumed > 0);
        assert!(exec.metrics.peak_memory_bytes >= 65_536); // at least one page
    }

    #[test]
    fn empty_input_is_valid() {
        let rt = runtime();
        let task = compile(&rt, ECHO_WAT);
        let exec = rt
            .execute(&task, "gdb_run", b"", &ResourceLimits::default())
            .expect("execution");
        assert!(exec.output.is_empty());
    }

    #[test]
    fn infinite_loop_is_stopped_by_fuel_limit() {
        let rt = runtime();
        let task = compile(&rt, SPIN_WAT);
        let limits = ResourceLimits {
            fuel: 100_000,
            timeout_ms: 60_000,
            ..ResourceLimits::default()
        };

        let err = rt.execute(&task, "gdb_run", b"", &limits).unwrap_err();
        assert_eq!(err, TaskError::FuelExhausted);
    }

    #[test]
    fn infinite_loop_is_stopped_by_wall_clock_deadline() {
        let rt = runtime();
        let task = compile(&rt, SPIN_WAT);
        let limits = ResourceLimits {
            fuel: u64::MAX,
            timeout_ms: 100,
            ..ResourceLimits::default()
        };

        let started = Instant::now();
        let err = rt.execute(&task, "gdb_run", b"", &limits).unwrap_err();
        assert_eq!(err, TaskError::DeadlineExceeded);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn memory_growth_past_the_cap_is_denied() {
        let rt = runtime();
        let task = compile(&rt, HOG_WAT);
        // 2 pages: enough to instantiate (1 page), far below the 101 pages
        // the guest asks for.
        let limits = ResourceLimits {
            max_memory_bytes: 2 * 65_536,
            ..ResourceLimits::default()
        };

        let exec = rt
            .execute(&task, "gdb_run", b"", &limits)
            .expect("execution");
        let grow_result = i32::from_le_bytes(exec.output.try_into().expect("4 bytes"));
        assert_eq!(grow_result, -1, "memory.grow must have been denied");
        assert!(exec.metrics.peak_memory_bytes <= limits.max_memory_bytes);
    }

    /// Greedy-then-spinning guest: does one over-cap `memory.grow` (denied,
    /// returns -1, execution continues) and then loops forever. The later
    /// fuel-exhaustion trap must be reported as `FuelExhausted`, not
    /// misattributed to the earlier denied grow (the sticky `denied` flag).
    const HOG_THEN_SPIN_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "gdb_alloc") (param i32) (result i32) (i32.const 1024))
          (func (export "gdb_run") (param i32 i32) (result i64)
            (drop (memory.grow (i32.const 100)))
            (loop $spin (br $spin))
            (i64.const 0)))
    "#;

    #[test]
    fn fuel_trap_after_denied_grow_is_not_misreported_as_memory() {
        let rt = runtime();
        let task = compile(&rt, HOG_THEN_SPIN_WAT);
        let limits = ResourceLimits {
            max_memory_bytes: 2 * 65_536, // denies the 100-page grow
            fuel: 100_000,                // then the spin exhausts fuel
            timeout_ms: 60_000,
        };
        let err = rt.execute(&task, "gdb_run", b"", &limits).unwrap_err();
        assert_eq!(
            err,
            TaskError::FuelExhausted,
            "the fuel trap must win over the stale denied-grow flag"
        );
    }

    #[test]
    fn transient_vs_deterministic_task_errors() {
        // Node-specific failures fail over; deterministic ones are final.
        assert!(TaskError::WasmUnavailable("x".into()).is_transient());
        assert!(TaskError::HostCapabilityDenied("gdb::log".into()).is_transient());
        assert!(TaskError::Runtime("panic".into()).is_transient());
        assert!(!TaskError::FuelExhausted.is_transient());
        assert!(!TaskError::DeadlineExceeded.is_transient());
        assert!(!TaskError::MemoryLimitExceeded.is_transient());
        assert!(!TaskError::Trapped("x".into()).is_transient());
        assert!(!TaskError::AbiViolation("x".into()).is_transient());
        assert!(!TaskError::InvalidModule("x".into()).is_transient());
    }

    #[test]
    fn memory_growth_under_the_cap_is_allowed() {
        let rt = runtime();
        let task = compile(&rt, HOG_WAT);
        let limits = ResourceLimits {
            max_memory_bytes: 16 * 1024 * 1024,
            ..ResourceLimits::default()
        };

        let exec = rt
            .execute(&task, "gdb_run", b"", &limits)
            .expect("execution");
        let grow_result = i32::from_le_bytes(exec.output.try_into().expect("4 bytes"));
        assert_eq!(grow_result, 1, "grow returns the previous page count");
        assert!(exec.metrics.peak_memory_bytes >= 101 * 65_536);
    }

    /// Hostile guest: returns a packed output pointer/length claiming a
    /// gigantic (out-of-bounds) length. The host must reject it as an ABI
    /// violation WITHOUT allocating that many bytes (else it OOMs).
    const HUGE_OUTPUT_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "gdb_alloc") (param i32) (result i32) (i32.const 1024))
          (func (export "gdb_run") (param i32 i32) (result i64)
            ;; out_ptr = 0, out_len = 0xFFFFFFFF (~4 GiB, far past 1 page)
            (i64.const 0xFFFFFFFF)))
    "#;

    #[test]
    fn out_of_bounds_output_length_is_rejected_not_allocated() {
        let rt = runtime();
        let task = compile(&rt, HUGE_OUTPUT_WAT);
        let err = rt
            .execute(&task, "gdb_run", b"", &ResourceLimits::default())
            .unwrap_err();
        assert!(
            matches!(err, TaskError::AbiViolation(_)),
            "expected AbiViolation, got: {err:?}"
        );
    }

    #[test]
    fn missing_entrypoint_is_reported_by_name() {
        let rt = runtime();
        let task = compile(&rt, ECHO_WAT);
        let err = rt
            .execute(&task, "no_such_fn", b"", &ResourceLimits::default())
            .unwrap_err();
        assert_eq!(err, TaskError::MissingExport("no_such_fn".into()));
    }

    #[test]
    fn garbage_bytes_are_rejected_at_compile_time() {
        let rt = runtime();
        assert!(matches!(
            rt.compile(b"definitely not wasm"),
            Err(TaskError::InvalidModule(_))
        ));
    }

    /// Guest that logs its input and answers with the store value for the
    /// key given as input (empty output when the key is absent).
    const STORE_READER_WAT: &str = r#"
        (module
          (import "gdb" "log" (func $log (param i32 i32)))
          (import "gdb" "store_get" (func $get (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (func (export "gdb_alloc") (param i32) (result i32) (i32.const 4096))
          (func (export "gdb_run") (param $ptr i32) (param $len i32) (result i64)
            (local $n i32)
            (call $log (local.get $ptr) (local.get $len))
            (local.set $n
              (call $get (local.get $ptr) (local.get $len) (i32.const 8192) (i32.const 1024)))
            (if (i32.lt_s (local.get $n) (i32.const 0))
              (then (return (i64.const 0))))
            (i64.or
              (i64.shl (i64.const 8192) (i64.const 32))
              (i64.extend_i32_u (local.get $n)))))
    "#;

    struct MapReader(std::collections::HashMap<Vec<u8>, Vec<u8>>);

    impl HostStoreReader for MapReader {
        fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
            self.0.get(key).cloned()
        }
    }

    // `..default()` covers the wasi-nn field under `compute-nn`; without that
    // feature `log` + `store` are already all the fields.
    #[cfg_attr(not(feature = "compute-nn"), allow(clippy::needless_update))]
    fn grants_with_store() -> HostGrants {
        let mut map = std::collections::HashMap::new();
        map.insert(b"foto:1".to_vec(), b"conteudo da foto".to_vec());
        HostGrants {
            log: true,
            store: Some(Arc::new(MapReader(map))),
            ..HostGrants::default()
        }
    }

    #[test]
    fn granted_store_read_returns_the_value() {
        let rt = runtime();
        let task = compile(&rt, STORE_READER_WAT);
        let exec = rt
            .execute_with_host(
                &task,
                "gdb_run",
                b"foto:1",
                &ResourceLimits::default(),
                &grants_with_store(),
            )
            .expect("execution with grants");
        assert_eq!(exec.output, b"conteudo da foto");
    }

    #[test]
    fn granted_store_read_of_absent_key_is_empty() {
        let rt = runtime();
        let task = compile(&rt, STORE_READER_WAT);
        let exec = rt
            .execute_with_host(
                &task,
                "gdb_run",
                b"nao-existe",
                &ResourceLimits::default(),
                &grants_with_store(),
            )
            .expect("execution");
        assert!(exec.output.is_empty());
    }

    #[test]
    fn ungranted_import_is_denied_at_instantiation() {
        let rt = runtime();
        let task = compile(&rt, STORE_READER_WAT);
        // Default grants: nothing linked → the module's imports are refused.
        let err = rt
            .execute(&task, "gdb_run", b"foto:1", &ResourceLimits::default())
            .unwrap_err();
        assert!(
            matches!(err, TaskError::HostCapabilityDenied(_)),
            "expected HostCapabilityDenied, got: {err:?}"
        );
    }

    #[test]
    fn plain_modules_are_unaffected_by_grants() {
        // A module with no imports runs identically with or without grants.
        let rt = runtime();
        let task = compile(&rt, ECHO_WAT);
        let exec = rt
            .execute_with_host(
                &task,
                "gdb_run",
                b"oi",
                &ResourceLimits::default(),
                &grants_with_store(),
            )
            .expect("execution");
        assert_eq!(exec.output, b"oi");
    }

    /// gdb.llm_generate end to end (feature `compute-llm`, RFC 0004 phase 5):
    /// a WAT guest asks the granted registry for a generation and returns the
    /// response as its task output.
    #[cfg(feature = "compute-llm")]
    mod llm_grant {
        use super::*;
        use crate::compute::llm::{
            GenerateEvent, GenerateRequest, GenerateStream, LlmBackend, LlmError, LlmRegistry,
            Locality, ModelInfo, Usage,
        };
        use futures::StreamExt;

        /// Guest importing `gdb.llm_generate`: model = "test-model" (from its
        /// data segment), prompt = the task input; the packed response
        /// location is returned directly as the task output (or empty on -1).
        const LLM_WAT: &str = r#"
            (module
              (import "gdb" "llm_generate"
                (func $llm (param i32 i32 i32 i32) (result i64)))
              (memory (export "memory") 1)
              (data (i32.const 64) "test-model")
              (global $next (mut i32) (i32.const 4096))
              (func (export "gdb_alloc") (param $len i32) (result i32)
                (local $ptr i32)
                (local.set $ptr (global.get $next))
                (global.set $next (i32.add (global.get $next) (local.get $len)))
                (local.get $ptr))
              (func (export "gdb_run") (param $ptr i32) (param $len i32) (result i64)
                (local $packed i64)
                (local.set $packed
                  (call $llm (i32.const 64) (i32.const 10)
                             (local.get $ptr) (local.get $len)))
                (if (i64.lt_s (local.get $packed) (i64.const 0))
                  (then (return (i64.const 0))))
                (local.get $packed)))
        "#;

        /// Backend answering "echo: <prompt>" as two deltas plus End.
        struct EchoBackend;

        #[async_trait::async_trait]
        impl LlmBackend for EchoBackend {
            async fn models(&self) -> Result<Vec<ModelInfo>, LlmError> {
                Ok(vec![ModelInfo {
                    name: "test-model".into(),
                    content_hash: None,
                }])
            }

            async fn generate(&self, req: GenerateRequest) -> Result<GenerateStream, LlmError> {
                let prompt = req
                    .messages
                    .last()
                    .map(|m| m.content.clone())
                    .unwrap_or_default();
                Ok(futures::stream::iter(vec![
                    Ok(GenerateEvent::Delta {
                        content: "echo: ".into(),
                    }),
                    Ok(GenerateEvent::Delta { content: prompt }),
                    Ok(GenerateEvent::End {
                        finish_reason: crate::compute::llm::FinishReason::Stop,
                        usage: Usage::default(),
                    }),
                ])
                .boxed())
            }

            fn locality(&self) -> Locality {
                Locality::Local
            }
        }

        /// A probed registry + grant, plus the tokio runtime that must stay
        /// alive to serve the grant's `block_on`.
        fn llm_grants() -> (tokio::runtime::Runtime, HostGrants) {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            let registry = Arc::new(LlmRegistry::default());
            registry.register_backend("mock", Arc::new(EchoBackend));
            rt.block_on(registry.probe_once());
            let grant = rt.block_on(async { LlmGrant::new(registry) });
            let grants = HostGrants {
                llm: Some(Arc::new(grant)),
                ..HostGrants::default()
            };
            (rt, grants)
        }

        #[test]
        fn granted_guest_generates_and_returns_the_response() {
            let rt = runtime();
            let task = compile(&rt, LLM_WAT);
            let (_tokio, grants) = llm_grants();
            let exec = rt
                .execute_with_host(
                    &task,
                    "gdb_run",
                    b"hello",
                    &ResourceLimits::default(),
                    &grants,
                )
                .expect("execution with llm grant");
            assert_eq!(exec.output, b"echo: hello");
        }

        #[test]
        fn ungranted_executor_refuses_llm_modules_before_running() {
            let rt = runtime();
            let task = compile(&rt, LLM_WAT);
            let err = rt
                .execute(&task, "gdb_run", b"hello", &ResourceLimits::default())
                .unwrap_err();
            assert!(
                matches!(err, TaskError::HostCapabilityDenied(_)),
                "expected HostCapabilityDenied, got: {err:?}"
            );
        }

        #[test]
        fn unknown_model_surfaces_as_failure_not_trap() {
            let rt = runtime();
            let task = compile(&rt, LLM_WAT);
            // Registry with no backends: resolution fails, the host fn
            // returns -1, and the guest maps that to an empty output.
            let tokio_rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            let registry = Arc::new(LlmRegistry::default());
            let grant = tokio_rt.block_on(async { LlmGrant::new(registry) });
            let grants = HostGrants {
                llm: Some(Arc::new(grant)),
                ..HostGrants::default()
            };
            let exec = rt
                .execute_with_host(
                    &task,
                    "gdb_run",
                    b"hello",
                    &ResourceLimits::default(),
                    &grants,
                )
                .expect("execution");
            assert!(exec.output.is_empty());
        }
    }

    /// wasi-nn end to end (feature `compute-nn`, RFC 0003 phase NN-1): a
    /// minimal ONNX model is hand-encoded in the test (no binary fixture),
    /// granted under the name "doubler", and a WAT guest runs real inference
    /// through `load_by_name → init_execution_context → set_input → compute
    /// → get_output`.
    #[cfg(feature = "compute-nn")]
    mod nn {
        use super::*;

        /// Guest speaking the `wasi_ephemeral_nn` witx ABI. Input: a tensor
        /// of two f32 (LE bytes). Output: the raw bytes of output tensor 0.
        /// Any wasi-nn errno != success traps (`unreachable`).
        const NN_WAT: &str = r#"
            (module
              (import "wasi_ephemeral_nn" "load_by_name"
                (func $load_by_name (param i32 i32 i32) (result i32)))
              (import "wasi_ephemeral_nn" "init_execution_context"
                (func $init_ctx (param i32 i32) (result i32)))
              (import "wasi_ephemeral_nn" "set_input"
                (func $set_input (param i32 i32 i32) (result i32)))
              (import "wasi_ephemeral_nn" "compute"
                (func $compute (param i32) (result i32)))
              (import "wasi_ephemeral_nn" "get_output"
                (func $get_output (param i32 i32 i32 i32 i32) (result i32)))
              (memory (export "memory") 2)
              (data (i32.const 128) "doubler")
              (global $next (mut i32) (i32.const 4096))
              (func (export "gdb_alloc") (param $len i32) (result i32)
                (local $ptr i32)
                (local.set $ptr (global.get $next))
                (global.set $next (i32.add (global.get $next) (local.get $len)))
                (local.get $ptr))
              (func $check (param $errno i32)
                (if (i32.ne (local.get $errno) (i32.const 0)) (then unreachable)))
              (func (export "gdb_run") (param $ptr i32) (param $len i32) (result i64)
                (local $graph i32) (local $ctx i32) (local $outsize i32)
                ;; graph <- load_by_name("doubler")
                (call $check
                  (call $load_by_name (i32.const 128) (i32.const 7) (i32.const 256)))
                (local.set $graph (i32.load (i32.const 256)))
                ;; ctx <- init_execution_context(graph)
                (call $check (call $init_ctx (local.get $graph) (i32.const 260)))
                (local.set $ctx (i32.load (i32.const 260)))
                ;; dims = [1, 2] at 320
                (i32.store (i32.const 320) (i32.const 1))
                (i32.store (i32.const 324) (i32.const 2))
                ;; tensor record at 300:
                ;;   dims ptr, dims len, type (1 = f32), data ptr, data len
                (i32.store (i32.const 300) (i32.const 320))
                (i32.store (i32.const 304) (i32.const 2))
                (i32.store8 (i32.const 308) (i32.const 1))
                (i32.store (i32.const 312) (local.get $ptr))
                (i32.store (i32.const 316) (local.get $len))
                (call $check
                  (call $set_input (local.get $ctx) (i32.const 0) (i32.const 300)))
                (call $check (call $compute (local.get $ctx)))
                ;; output tensor 0 into buffer at 2048 (cap 1024); size at 280
                (call $check
                  (call $get_output (local.get $ctx) (i32.const 0)
                        (i32.const 2048) (i32.const 1024) (i32.const 280)))
                (local.set $outsize (i32.load (i32.const 280)))
                (i64.or
                  (i64.shl (i64.const 2048) (i64.const 32))
                  (i64.extend_i32_u (local.get $outsize)))))
        "#;

        // ── Minimal protobuf writer (varint + length-delimited fields) ──

        fn varint(mut value: u64, out: &mut Vec<u8>) {
            loop {
                let byte = (value & 0x7f) as u8;
                value >>= 7;
                if value == 0 {
                    out.push(byte);
                    return;
                }
                out.push(byte | 0x80);
            }
        }

        fn varint_field(field: u64, value: u64, out: &mut Vec<u8>) {
            varint(field << 3, out); // wire type 0
            varint(value, out);
        }

        fn bytes_field(field: u64, bytes: &[u8], out: &mut Vec<u8>) {
            varint((field << 3) | 2, out); // wire type 2
            varint(bytes.len() as u64, out);
            out.extend_from_slice(bytes);
        }

        /// `y = Add(x, x)` over float[1,2] — a valid, minimal ONNX ModelProto
        /// that doubles its input.
        fn doubler_onnx_model() -> Vec<u8> {
            // TypeProto { tensor_type { elem_type: FLOAT, shape { dim: 1, dim: 2 } } }
            let mut shape = Vec::new();
            for extent in [1u64, 2] {
                let mut dim = Vec::new();
                varint_field(1, extent, &mut dim); // Dimension.dim_value
                bytes_field(1, &dim, &mut shape); // TensorShapeProto.dim
            }
            let mut tensor_type = Vec::new();
            varint_field(1, 1, &mut tensor_type); // elem_type = FLOAT
            bytes_field(2, &shape, &mut tensor_type); // shape
            let mut type_proto = Vec::new();
            bytes_field(1, &tensor_type, &mut type_proto); // TypeProto.tensor_type

            let value_info = |name: &str| {
                let mut vi = Vec::new();
                bytes_field(1, name.as_bytes(), &mut vi); // ValueInfoProto.name
                bytes_field(2, &type_proto, &mut vi); // ValueInfoProto.type
                vi
            };

            // NodeProto { input: ["x", "x"], output: ["y"], op_type: "Add" }
            let mut node = Vec::new();
            bytes_field(1, b"x", &mut node);
            bytes_field(1, b"x", &mut node);
            bytes_field(2, b"y", &mut node);
            bytes_field(4, b"Add", &mut node);

            // GraphProto { node, name, input, output }
            let mut graph = Vec::new();
            bytes_field(1, &node, &mut graph);
            bytes_field(2, b"doubler", &mut graph);
            bytes_field(11, &value_info("x"), &mut graph);
            bytes_field(12, &value_info("y"), &mut graph);

            // OperatorSetIdProto { domain: "", version: 13 }
            let mut opset = Vec::new();
            varint_field(2, 13, &mut opset);

            // ModelProto { ir_version: 8, graph, opset_import }
            let mut model = Vec::new();
            varint_field(1, 8, &mut model);
            bytes_field(7, &graph, &mut model);
            bytes_field(8, &opset, &mut model);
            model
        }

        fn nn_grants() -> HostGrants {
            HostGrants {
                nn: Some(Arc::new(NnGrant::new(vec![(
                    "doubler".to_string(),
                    doubler_onnx_model(),
                )]))),
                ..HostGrants::default()
            }
        }

        fn f32s(bytes: &[u8]) -> Vec<f32> {
            bytes
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect()
        }

        #[test]
        fn granted_model_runs_real_inference() {
            let rt = runtime();
            let task = compile(&rt, NN_WAT);

            let mut input = Vec::new();
            input.extend_from_slice(&1.5f32.to_le_bytes());
            input.extend_from_slice(&(-2.25f32).to_le_bytes());

            let exec = rt
                .execute_with_host(
                    &task,
                    "gdb_run",
                    &input,
                    &ResourceLimits::default(),
                    &nn_grants(),
                )
                .expect("inference execution");
            assert_eq!(f32s(&exec.output), vec![3.0, -4.5], "y = x + x");
        }

        /// Phase NN-4: requesting the GPU target is always safe — without the
        /// `compute-nn-cuda` feature (or without CUDA hardware) the backend
        /// falls back to CPU and inference still runs correctly.
        #[test]
        fn gpu_target_is_safe_and_falls_back_when_unavailable() {
            let rt = runtime();
            let task = compile(&rt, NN_WAT);
            let grants = HostGrants {
                nn: Some(Arc::new(NnGrant::new_with_target(
                    vec![("doubler".to_string(), doubler_onnx_model())],
                    NnTarget::Gpu,
                ))),
                ..HostGrants::default()
            };
            let mut input = Vec::new();
            input.extend_from_slice(&2.0f32.to_le_bytes());
            input.extend_from_slice(&0.5f32.to_le_bytes());
            let exec = rt
                .execute_with_host(
                    &task,
                    "gdb_run",
                    &input,
                    &ResourceLimits::default(),
                    &grants,
                )
                .expect("gpu-target execution");
            assert_eq!(f32s(&exec.output), vec![4.0, 1.0]);
        }

        #[test]
        fn ungranted_executor_refuses_nn_modules_before_running() {
            let rt = runtime();
            let task = compile(&rt, NN_WAT);
            let err = rt
                .execute(&task, "gdb_run", b"", &ResourceLimits::default())
                .unwrap_err();
            assert!(
                matches!(err, TaskError::HostCapabilityDenied(_)),
                "expected HostCapabilityDenied, got: {err:?}"
            );
        }

        #[test]
        fn unknown_model_name_traps_cleanly() {
            let rt = runtime();
            let task = compile(&rt, NN_WAT);
            // Grant exists, but under a different name than the guest asks for:
            // load_by_name fails with an errno, which the guest turns into a trap.
            let grants = HostGrants {
                nn: Some(Arc::new(NnGrant::new(vec![(
                    "outro-modelo".to_string(),
                    doubler_onnx_model(),
                )]))),
                ..HostGrants::default()
            };
            let mut input = Vec::new();
            input.extend_from_slice(&1.0f32.to_le_bytes());
            input.extend_from_slice(&2.0f32.to_le_bytes());
            let err = rt
                .execute_with_host(
                    &task,
                    "gdb_run",
                    &input,
                    &ResourceLimits::default(),
                    &grants,
                )
                .unwrap_err();
            assert!(matches!(err, TaskError::Trapped(_)), "got: {err:?}");
        }
    }
}
