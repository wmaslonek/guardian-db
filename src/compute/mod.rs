//! # Guardian Compute
//!
//! Delegation of business-logic execution (WebAssembly) between peers of the
//! GuardianDB network, with capability-aware routing: the node with the most
//! spare processing power runs the task, and the result flows back through
//! ordinary GuardianDB replication.
//!
//! Design and phasing: `docs/rfcs/0002-guardian-compute.md`.
//!
//! ## Phase status
//!
//! All RFC phases (0–5) have their core implemented: sandbox runtime with
//! opt-in host capabilities, direct 1-to-1 delegation, capability-aware
//! orchestration (gossip telemetry + scoring + failover + Contract-Net
//! auction), the reactive layer (task ledger + `on_replicated` triggers),
//! MapReduce fan-out, and k-of-n redundant execution with reputation.
//! Remaining Phase 5 follow-ups (see RFC §7): a real `wasi-nn` backend for
//! Edge AI, and the `guardian-compute-sdk` crate with `#[guardian_task]`.
//!
//! - [`runtime`] (Phase 1, **implemented**) — the wasmtime sandbox enforcing
//!   [`ResourceLimits`]
//! - [`protocol`] (Phase 2, **implemented**) — the [`COMPUTE_ALPN`]
//!   request/response protocol, modeled on
//!   `p2p::network::core::ticket_exchange`; registered on the Router when the
//!   feature is enabled, with [`ComputeClient::execute_on`] as the entry point
//! - [`telemetry`] (Phase 3, **implemented**) — samples and gossips the
//!   [`CapabilityVector`] with hysteresis, feeding the directory
//! - [`scheduler`] (Phase 3, **implemented**) — capability scoring, ranking
//!   and delegation with automatic failover
//! - [`ledger`] (Phase 4, **implemented**) — task lifecycle with conditional
//!   claims, over any [`ledger::LedgerStore`] (in-memory, or a replicated
//!   GuardianDB store supplied by the app)
//! - [`triggers`] (Phase 4, **implemented**) — reactive rules with
//!   replica-safe deduplication and deadline-based requeue, bridged to the
//!   store `EventBus` via [`triggers::TriggerEngine::attach_event_bus`]
//! - `llm` (RFC 0004 phases 1–5, **implemented**, features `compute-llm` /
//!   `compute-llm-colibri`) — generative LLM inference through pluggable
//!   backends (OpenAI-compatible HTTP: mesh-llm, colibri's server,
//!   llama.cpp…; plus the supervised native colibri process driver), with
//!   active health-checking, streamed `Generate` responses on
//!   [`COMPUTE_ALPN`], the local → peers → distributed placement router, and
//!   the `gdb.llm_generate` host grant for WASM tasks (SDK:
//!   `guardian_compute_sdk::host::llm_generate`)
//!
//! Decisions recorded in RFC §8: wasmtime (no WASI, fuel + epoch); task
//! input/output are opaque bytes at protocol level (CBOR is an SDK-level
//! convention); participation is reciprocal by license term — no payments.

pub mod ledger;
pub mod protocol;
pub mod runtime;
pub mod scheduler;
pub mod telemetry;
pub mod triggers;

pub use ledger::{LedgerStore, MemoryLedger, TaskLedger, TaskRecord, TaskState};
pub use protocol::{
    CompletedTask, ComputeCallError, ComputeClient, ComputeProtocolHandler, ExecuteRequest,
    ExecutorPolicy, ProbeReply, RejectReason, WasmFetcher,
};
pub use runtime::{
    CompiledTask, ExecMetrics, Execution, HostGrants, HostStoreReader, TaskError, WasmRuntime,
};
pub use triggers::{DispatchError, TaskDispatcher, TriggerConfig, TriggerEngine, TriggerRule};
#[cfg(feature = "compute-nn")]
pub mod nn;
#[cfg(feature = "compute-nn")]
pub use nn::NnModelRegistry;
#[cfg(feature = "compute-llm")]
pub mod llm;
#[cfg(feature = "compute-llm-colibri")]
pub use llm::{ColibriConfig, ColibriProcessBackend};
#[cfg(feature = "compute-llm")]
pub use llm::{
    GenerateEvent, GenerateRequest, GenerateStream, LlmBackend, LlmError, LlmRegistry, LlmRouter,
    ModelInfo, ModelSelector, OpenAiConfig, OpenAiHttpBackend, PeerDelegation, RouterConfig,
};
#[cfg(feature = "compute-llm")]
pub use protocol::{LlmGenerateRequest, LlmStreamFrame};
#[cfg(feature = "compute-llm")]
pub use runtime::LlmGrant;
#[cfg(feature = "compute-nn")]
pub use runtime::{NnGrant, NnTarget};
pub use scheduler::{
    CapabilityDirectory, ComputeScheduler, Delegated, RedundantOutcome, ReputationBook,
    ScheduleError, SchedulerConfig, ScoreWeights,
};
pub use telemetry::{CAPABILITY_TOPIC, CapabilityGossip, TelemetryConfig, TelemetrySampler};

use iroh::EndpointId as NodeId;
use iroh_blobs::Hash;
use serde::{Deserialize, Serialize};

/// Dedicated ALPN for the Guardian Compute execution protocol (Phase 2).
///
/// Registered on the same iroh `Router` that multiplexes gossip/blobs/docs and
/// the ticket exchange. Bumping the message format incompatibly requires a new
/// ALPN (`/guardian-db/compute/2`), never a silent change under `/1`.
pub const COMPUTE_ALPN: &[u8] = b"/guardian-db/compute/1";

/// CPU architecture advertised in a [`CapabilityVector`].
///
/// Coarse-grained on purpose: the scheduler only needs it to reason about
/// rough performance classes, not to gate execution (WASM runs anywhere).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CpuArch {
    X86_64,
    Aarch64,
    /// Anything else; carried for diagnostics, scored neutrally.
    Other,
}

/// Hardware accelerator advertised in a [`CapabilityVector`].
///
/// Only meaningful once host functions such as `wasi-nn` exist (Phase 5);
/// until then peers may advertise it but schedulers ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Accel {
    Gpu,
    Npu,
}

/// Coarse class of a task, used for the executor's admission policy.
///
/// An executor declares which classes it accepts (see
/// [`CapabilityVector::accepts`]); a request whose class is not accepted is
/// rejected before any bytes of code are fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskClass {
    /// General-purpose business logic (default).
    General,
    /// Media processing: thumbnails, transcoding, metadata extraction.
    Media,
    /// Analytics / query fan-out (MapReduce-style partials).
    Analytics,
    /// AI inference (requires opt-in host functions; Phase 5).
    Inference,
}

/// Hard resource ceiling the executor enforces on a single task run.
///
/// All three limits are mandatory: the wasmtime sandbox (Phase 1) aborts the
/// task cleanly when any of them is exceeded, so a hostile or buggy module can
/// spin, allocate, or stall without harming the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// Ceiling on the module's linear memory, in bytes.
    pub max_memory_bytes: u64,
    /// CPU budget in wasmtime fuel units (roughly proportional to executed
    /// instructions); exhaustion traps the module.
    pub fuel: u64,
    /// Wall-clock deadline enforced by the host via epoch interruption.
    pub timeout_ms: u64,
}

impl Default for ResourceLimits {
    /// Conservative defaults for small business-logic functions:
    /// 64 MiB of memory, 1 billion fuel units, 10 s wall clock.
    fn default() -> Self {
        Self {
            max_memory_bytes: 64 * 1024 * 1024,
            fuel: 1_000_000_000,
            timeout_ms: 10_000,
        }
    }
}

/// Where the scheduler may place a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Placement {
    /// Let the scheduler pick the best node from known capability vectors
    /// (falling back to local execution if no peer qualifies).
    BestAvailable,
    /// Pin the task to one specific peer (the Phase 2 `execute_on` path).
    Node(NodeId),
    /// Run on the local node only; never delegate.
    Local,
}

/// Reusable description of a task: which code to run, how, and within which
/// limits.
///
/// A `TaskSpec` is the *template* — per-invocation input bytes travel in the
/// Phase 2 `ExecuteRequest`, not here, so the same spec can back both direct
/// calls and reactive trigger rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSpec {
    /// BLAKE3 hash identifying the `.wasm` blob in iroh-blobs. The executor
    /// fetches and verifies the code by this hash, so it is impossible to run
    /// a binary other than the one the requester named.
    pub wasm_hash: Hash,
    /// Name of the exported function to invoke.
    pub entrypoint: String,
    /// Admission class (matched against the executor's accepted classes).
    pub class: TaskClass,
    /// Resource ceiling for one run.
    pub limits: ResourceLimits,
    /// Placement constraint for the scheduler.
    pub placement: Placement,
    /// NN model this task needs (`load_by_name`); routes only to nodes serving
    /// it (phase NN-3). `None` for non-inference tasks.
    pub required_model: Option<String>,
}

/// Capability advertisement a compute-enabled node publishes over gossip.
///
/// Vectors are *hints, not contracts*: the scheduler ranks candidates by them,
/// but the executor's local admission policy always has the final word. Nodes
/// publish with hysteresis (on threshold crossings plus a slow heartbeat)
/// rather than continuously, to keep gossip traffic negligible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityVector {
    /// Identity of the advertising node.
    pub node_id: NodeId,
    // -- Static (changes rarely) --
    /// Logical CPU cores.
    pub cpu_cores: u16,
    pub cpu_arch: CpuArch,
    pub ram_total_mb: u32,
    /// Detected accelerators (informational until Phase 5).
    pub accelerators: Vec<Accel>,
    // -- Dynamic (sampled; published with hysteresis) --
    /// Current CPU load, 0–100.
    pub cpu_load_pct: u8,
    pub ram_free_mb: u32,
    /// `true` while running on battery. A node on battery advertises
    /// `max_concurrent: 0` by default — it must opt in to accept work.
    pub on_battery: bool,
    pub battery_pct: Option<u8>,
    /// Compute tasks currently running on this node.
    pub tasks_running: u8,
    // -- Owner policy --
    /// Maximum tasks this node is willing to run concurrently; `0` means the
    /// node is advertising presence but not accepting work.
    pub max_concurrent: u8,
    /// Task classes this node accepts.
    pub accepts: Vec<TaskClass>,
    /// NN model names this node serves via wasi-nn (RFC 0003 phase NN-3).
    /// Always present in the wire format so nodes with and without the
    /// `compute-nn` feature interoperate; nodes without it advertise none.
    pub nn_models: Vec<String>,
    /// Generative LLM model names this node serves (RFC 0004 phase 4). Only
    /// models of **healthy** backends appear here — the registry's prober
    /// withdraws unresponsive ones (RFC 0004 §6.1). Same interop rule as
    /// `nn_models`: always on the wire, empty without `compute-llm`.
    pub llm_models: Vec<String>,
    /// Embedding model names this node serves (RFC 0005 §6.4 phase 2). Same
    /// interop rule: always on the wire, empty unless an `EmbeddingRegistry`
    /// is attached to the executor. A peer delegating an embedding picks a
    /// node that advertises the model here.
    pub embed_models: Vec<String>,
    /// Unix timestamp (seconds) when this vector was sampled, so schedulers
    /// can discard stale vectors.
    pub issued_at: u64,
}

impl CapabilityVector {
    /// Whether this node currently advertises room for a task of `class`.
    pub fn is_candidate_for(&self, class: TaskClass) -> bool {
        self.max_concurrent > 0
            && self.tasks_running < self.max_concurrent
            && self.accepts.contains(&class)
    }

    /// Whether this node advertises the named NN model (phase NN-3).
    pub fn offers_model(&self, name: &str) -> bool {
        self.nn_models.iter().any(|m| m == name)
    }

    /// Whether this node advertises the named generative LLM model
    /// (RFC 0004 phase 4).
    pub fn offers_llm_model(&self, name: &str) -> bool {
        self.llm_models.iter().any(|m| m == name)
    }

    /// Whether this node advertises the named embedding model
    /// (RFC 0005 §6.4 phase 2).
    pub fn offers_embed_model(&self, name: &str) -> bool {
        self.embed_models.iter().any(|m| m == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_vector(node_id: NodeId) -> CapabilityVector {
        CapabilityVector {
            node_id,
            cpu_cores: 8,
            cpu_arch: CpuArch::X86_64,
            ram_total_mb: 16_384,
            accelerators: vec![],
            cpu_load_pct: 12,
            ram_free_mb: 9_000,
            on_battery: false,
            battery_pct: None,
            tasks_running: 1,
            max_concurrent: 4,
            accepts: vec![TaskClass::General, TaskClass::Media],
            nn_models: vec!["doubler".to_string()],
            llm_models: vec![],
            embed_models: vec![],
            issued_at: 1_760_000_000,
        }
    }

    fn test_node_id() -> NodeId {
        iroh::SecretKey::generate().public()
    }

    #[test]
    fn candidate_check_honors_class_and_slots() {
        let mut v = sample_vector(test_node_id());
        assert!(v.is_candidate_for(TaskClass::Media));
        assert!(!v.is_candidate_for(TaskClass::Inference));

        v.tasks_running = v.max_concurrent;
        assert!(!v.is_candidate_for(TaskClass::Media));

        v.tasks_running = 0;
        v.max_concurrent = 0;
        assert!(!v.is_candidate_for(TaskClass::General));
    }

    #[test]
    fn capability_vector_roundtrips_through_postcard() {
        let v = sample_vector(test_node_id());
        let bytes = postcard::to_stdvec(&v).expect("serialize");
        let back: CapabilityVector = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(v, back);
    }

    #[test]
    fn task_spec_roundtrips_through_postcard() {
        let spec = TaskSpec {
            wasm_hash: Hash::new(b"fake wasm module bytes"),
            entrypoint: "generate_thumbnail".into(),
            class: TaskClass::Media,
            limits: ResourceLimits::default(),
            placement: Placement::BestAvailable,
            required_model: None,
        };
        let bytes = postcard::to_stdvec(&spec).expect("serialize");
        let back: TaskSpec = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(spec, back);
    }
}
