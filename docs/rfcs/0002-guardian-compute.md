# Guardian Compute — Distributed Execution of Business Logic (WASM) over GuardianDB

**Status:** implemented (feature `compute`). Core (sandbox runtime, delegation
protocol, telemetry + scheduler, ledger + reactive triggers) and advanced capabilities
(opt-in host functions, Contract-Net auction, declarative accelerators, MapReduce,
k-of-n redundant execution with reputation). Extensions documented in
[0003-guardian-compute-followups.md](0003-guardian-compute-followups.md) (Edge AI via
`wasi-nn`) and [0004-compute-llm.md](0004-compute-llm.md) (LLM inference).
**Scope:** the `compute` feature of the `guardian-db` crate.
**Relates to:** `src/p2p/network/core/` (Router ALPN, gossip, blobs),
`src/messaging.rs` (direct channels), `src/stores/sync_observer.rs` (reactive
observability), `src/access_control/` (permissioned networks),
[RFC 0001](0001-fenced-shard-primaries.md) (ordered log, reusable as a ledger).

---

## 1. Summary

Guardian Compute turns GuardianDB into a **decentralized edge-computing platform**:
nodes on the P2P network delegate the execution of business logic (compiled to
WebAssembly) to other nodes, with a **capacity-aware scheduler** that routes each task
to the node with the most available capacity. Results flow back through GuardianDB's
ordinary replication.

The feature reuses the Iroh stack already in production — QUIC with public-key
identity, ALPN protocol multiplexing on the `Router`, `iroh-blobs` for hash-addressed
code distribution, `iroh-gossip` for telemetry, the stores' `EventBus` for triggers,
and the `AccessController` for permissioning. Three layers are new: capacity telemetry,
a sandboxed WASM runtime, and the scheduler. It is a **decentralized FaaS with
capacity-based scheduling**, behind optional dependencies and with no cost to builds
that do not enable the feature.

---

## 2. Foundation reused from the existing stack

| Need | Reused component |
|---|---|
| Secure P2P connectivity (QUIC + public-key identity) | `iroh 1.0` — `Endpoint` and `Router` (`src/p2p/network/core/mod.rs`) |
| Custom protocols via ALPN | The Router multiplexes `gossip`, `blobs`, `docs` and `ticket`. `ticket_exchange.rs` (`TICKET_ALPN`) served as the template for the compute protocol |
| WASM binary distribution | `iroh-blobs`: BLAKE3-hash-addressed transfer with native integrity verification. A `.wasm` is just a blob |
| Telemetry broadcast | `iroh-gossip` (`src/p2p/network/core/gossip.rs`) |
| Direct node-to-node messaging | `src/messaging.rs` |
| Reactive triggers | `EventBus` + store events (`EventReplicated`, `EventReady`); the observer pattern is in `src/stores/sync_observer.rs` |
| Participant permissioning | `src/access_control/` |
| Auditable, replicated task record | GuardianDB itself (event log / docs); RFC 0001 defines an ordered `(epoch, seq)` log reusable as a ledger |
| Compact protocol serialization | `postcard` (already used in the ticket exchange) |

Layers built specifically for the feature: capacity telemetry (CPU/RAM/battery
collection), the WASM runtime (`wasmtime`), the scheduler, and the task-lifecycle
ledger.

---

## 3. Execution flow

```
┌──────────── Node A (requester) ─────────────┐      ┌──────────── Node B (executor) ───────────┐
│                                            │      │                                          │
│ 1. Publishes task.wasm to iroh-blobs       │      │                                          │
│    (BLAKE3 hash = code identity)           │      │                                          │
│                                            │      │                                          │
│ 2. Scheduler consults CapabilityVectors    │◄─────│ 0. Publishes telemetry via gossip        │
│    received via gossip and picks Node B    │      │    (cores, free RAM, load, battery)      │
│                                            │      │                                          │
│ 3. Opens a QUIC connection on the ALPN     │─────►│ 4. Downloads the task.wasm blob (if not  │
│    /guardian-db/compute/1 and sends        │      │    in local cache, by hash)              │
│    ExecuteRequest{wasm_hash, input, limits}│      │                                          │
│                                            │      │ 5. Runs in a wasmtime sandbox:           │
│                                            │      │    - max memory, fuel (CPU), timeout     │
│                                            │      │    - no filesystem, no network, no env   │
│                                            │      │                                          │
│ 7. Receives ExecuteResult{output, metrics} │◄─────│ 6. Returns the result on the same conn   │
│                                            │      │                                          │
│ 8. Writes the result to a GuardianDB store │      │                                          │
│    → replicates to the network normally    │      │                                          │
└────────────────────────────────────────────┘      └──────────────────────────────────────────┘
```

Steps 1, 4 and 8 need no networking code of their own: they are iroh-blobs and
GuardianDB's ordinary replication.

---

## 4. Components

### 4.1 Capacity telemetry (`telemetry.rs`)

Every node with `compute` enabled periodically publishes a capability vector on the
network's gossip topic:

```rust
#[derive(Serialize, Deserialize)]
pub struct CapabilityVector {
    pub node_id: NodeId,
    // Static (rarely changes)
    pub cpu_cores: u16,
    pub cpu_arch: CpuArch,           // x86_64, aarch64, ...
    pub ram_total_mb: u32,
    pub accelerators: Vec<Accel>,    // declared GPU/NPU
    // Dynamic (sampled; published with hysteresis)
    pub cpu_load_pct: u8,
    pub ram_free_mb: u32,
    pub on_battery: bool,
    pub battery_pct: Option<u8>,
    pub tasks_running: u8,
    // Node owner's policy
    pub max_concurrent: u8,
    pub accepts: TaskClasses,
    pub issued_at: u64,
}
```

- **Collection** via `sysinfo` (CPU, RAM). Automatic battery detection is
  platform-specific; the owner supplies it via `TelemetryConfig::on_battery`, and a node
  on battery advertises `max_concurrent: 0` by default.
- **Passive publication with hysteresis**: `CapabilityGossip` runs two loops — a
  publisher sampling every 20 s that only re-publishes when a dynamic field crosses a
  threshold (CPU ±15 points, RAM ±10% of total, a battery/slots/classes change) or every
  3 min as a heartbeat, and a receiver feeding the `CapabilityDirectory`.
- **Versioned gossip topic**: `guardian-db/compute/capabilities/N` (TopicId by blake3,
  the same convention as pubsub). The version is incremented when the vector gains fields
  (later extensions such as NN and embedding model affinity raised it).
- **Aging by the local reception clock**, never by the sender's `issued_at` (subject to
  clock skew).
- **Vectors are hints, not contracts**: they are unauthenticated data; a forged vector
  costs at most one attempt with failover, since admission at the executor always
  decides.

### 4.2 Delegation protocol (`protocol.rs`, `mod.rs`)

An ALPN registered on the existing Router, alongside `TICKET_ALPN`:

```rust
pub const COMPUTE_ALPN: &[u8] = b"/guardian-db/compute/1";
```

The request frame is the enum `ComputeRequest { Execute, Probe }` (postcard, with a
`u32` length prefix). The `Execute` response uses **two frames**: a fast admission ack
(before the fetch/compile) that distinguishes "queued at a real executor" from
"unreachable", followed by the result. The ack is read with a 64 KiB cap and reject-
reason strings are bounded before encoding, so an ack can never exceed the cap.

```rust
enum ComputeMessage {
    ExecuteRequest {
        task_id: Uuid,
        wasm_hash: Hash,          // BLAKE3 hash of the blob (iroh-blobs)
        entrypoint: String,
        input: Bytes,
        limits: ResourceLimits,
        class: TaskClass,
    },
    ExecuteAccept { task_id: Uuid },
    ExecuteReject { task_id: Uuid, reason: RejectReason },
    ExecuteResult {
        task_id: Uuid,
        outcome: Result<Bytes, TaskError>,
        metrics: ExecMetrics,     // fuel spent, memory peak, duration
    },
    Probe { manifest: TaskManifest },   // Contract-Net auction (§5)
    Proposal { task_id: Uuid, readiness_score: u32 },
}
```

An executor that does not hold the referenced `.wasm` downloads it **from the requester
itself** (authenticated by the QUIC TLS, serving as the provider) via iroh-blobs by
hash, with integrity verification by construction: it is impossible to run a binary
different from the one the hash identifies. Compiled modules sit in a local LRU cache
(32 entries) by hash, so repeated tasks re-transfer and recompile nothing.

### 4.3 Sandbox runtime (`runtime.rs`)

`wasmtime` (v46, no default features — only `runtime` + `cranelift`, no WASI) configured
for maximum security:

- **No WASI by default**: the module only sees `input: &[u8]` and returns `output:
  &[u8]`. No access to filesystem, network, environment variables or clock.
- **Fuel metering**: each instruction consumes fuel; a task that exhausts its CPU budget
  is aborted cleanly (protection against infinite loops).
- **`ResourceLimiter`**: a hard linear-memory ceiling per instance.
- **Epoch interruption**: a wall-clock timeout imposed by the host.
- **Bounds-checking of guest-controlled lengths** (output size, host-function buffers)
  against the guest's own memory before the host allocates, so a hostile module cannot
  OOM the executor.

Minimal guest-module ABI (exported-allocator style):

```
(export "gdb_alloc")  ;; allocates a buffer for the host to write the input into
(export "gdb_run")    ;; fn(ptr, len) -> ptr_len_of_output
```

The `guardian-compute-sdk` crate (macro `#[guardian_task]`) hides this ABI from the
developer.

### 4.4 Scheduler (`scheduler.rs`)

**Local scoring:** the requester keeps the table of `CapabilityVector`s received via
gossip and picks the best node locally, with no extra network round trip:

```
score(node) = w1·estimated_free_cores
            + w2·free_ram
            − w3·current_load
            − w4·battery_penalty (disqualifying if on_battery and policy forbids)
            − w5·known_latency (RTT measured by iroh)
            + w6·cache_affinity (does the node already have the .wasm? the data?)
```

Ties favor the node that already holds the code blob and/or a replica of the input data
(*data gravity*). **Automatic failover**: `Rejected`/`Unreachable`/`Timeout` try the
next-ranked node (an unreachable node is evicted from the directory); an error from the
task itself (`TaskError`) is terminal — running it on another node would not change the
result.

API entry points: `ComputeClient::execute(task)` (the network decides the destination),
`execute_on(node_id, task)` (explicit destination), `IrohBackend::compute_scheduler()`
and `compute_join_capability_mesh(peers)`.

### 4.5 Task ledger (`ledger.rs`)

Each task's lifecycle is recorded through the `LedgerStore` abstraction
(`get`/`put`/`create_if_absent`/`list`), which gives replicated auditing, observability
and recovery after failure:

```
TaskRecord { task_id, wasm_hash, input_hash, class, state, assigned_to,
             deadline, result_hash?, metrics?, attempts }
state: Pending → Running{deadline} → Done{executor} | Failed
```

- `MemoryLedger` gives exact conditional semantics on one node. Over a replicated
  GuardianDB store (the app's implementation), `create_if_absent` is best-effort under
  LWW — tasks should be idempotent; under RFC 0001's `Strict` mode the condition becomes
  exact.
- A **permanent** failure (an error from the task itself) is terminal; a **transient**
  failure (no candidates, all busy) returns to `Pending` and the requeue loop
  (`spawn_requeue_loop`) redispatches until `max_attempts` is exhausted; a `Running` task
  with an expired deadline is treated as abandoned and redispatched.

### 4.6 Reactive triggers (`triggers.rs`)

A **reactive rule** associates a store event with a compute task:

```rust
compute.on_replicated("/photos", TaskSpec {
    wasm: THUMBNAIL_WASM_HASH,
    entrypoint: "generate_thumbnail",
    class: TaskClass::Media,
    placement: Placement::BestAvailable,
})?;
```

The bridge to events is `TriggerEngine::attach_event_bus`, which subscribes to
`EventReplicated` (the entry's hash becomes the `event_id` and the payload becomes the
task's input).

**Trigger deduplication:** `EventReplicated` fires on every replica; without
coordination, N nodes would schedule the same task N times. The trigger is a conditional
write to the ledger (`Pending`), keyed by `blake3(rule_id ␟ event_id)` (deterministic on
every replica) — the ledger's own replication resolves the race, and only whoever "won"
the write actually schedules. Under RFC 0001's `Strict` mode this is exact; under LWW,
rare collisions cause duplicate execution, acceptable for idempotent tasks.

---

## 5. Advanced capabilities

- **Opt-in host functions** (`HostGrants` in the runtime, `set_host_grants` on the
  handler): guest-module imports under `"gdb"` — `log(ptr,len)` and
  `store_get(key,…)->i32` (answered by an app-provided `HostStoreReader`). Everything off
  by default (pure, deterministic sandbox); a module that imports an ungranted capability
  fails at instantiation with `TaskError::HostCapabilityDenied`. Clock and randomness are
  deliberately not offered (they preserve the determinism the k-of-n mode requires).
- **Contract-Net auction** (`execute_with_auction`): direct probing of the top-N of the
  ranking via a `Probe` message on `COMPUTE_ALPN` itself; the executor replies with a
  fresh sample (does it accept the class? free slots? current load/RAM), the requester
  re-scores with the fresh data and delegates — a fresh bid beats a stale gossip vector,
  without burning an attempt. Degrades gracefully to the gossip ranking if nobody
  answers.
- **Declarative accelerators** (`TelemetryConfig::accelerators`): automatic GPU/NPU
  detection would drag in a graphics stack, so the owner declares them; scoring only
  rewards them on inference tasks (`accelerator_bonus`).
- **MapReduce** (`ComputeScheduler::map`): parallel fan-out of the partitions with
  per-task ranking rotation (spreads the load without waiting for `Busy` rejections); the
  Reduce is the caller's.
- **k-of-n redundant execution + reputation** (`execute_redundant` + `ReputationBook`):
  the same task goes to k nodes, results are grouped by the BLAKE3 hash of the output, a
  strict majority wins (a tie = `Divergent`); divergent nodes have their reputation
  penalized (halved, floor 0.05) and the ranking discounts it (`reputation_penalty`) — a
  known liar is deprioritized, never banned. It requires deterministic tasks, so **grants
  and redundancy do not mix**.

---

## 6. Use cases

- **Edge AI — inference on the most capable node.** Small models (quantized
  Llama/Phi/Mistral, Whisper, image classifiers) run via `wasi-nn`, which delegates
  execution to the host's native backend (ONNX Runtime) with real acceleration. A phone
  that needs to transcribe audio or classify a photo delegates to the idle desktop on the
  same network; the result comes back and is written to GuardianDB, replicating to every
  device. Implemented by the `compute-nn` feature (see RFC 0003).
- **Decentralized media processing.** Reactive trigger + blobs: heavy media enters the
  database → the strongest idle node generates thumbnails, transcodes, extracts metadata.
  The media is already a hash-addressed blob; the task references the hash and the
  executor pulls the content straight from whoever holds it. Pure-Rust image libraries
  (`image`, `zune`) compile to WASM.
- **Analytics / P2P MapReduce.** Analytical queries sliced across nodes: each processes
  its partition (Map) and returns the partial aggregate (Reduce), with direct synergy
  with the `sql` layer.
- **Automation and ETL with failover.** Periodic routines recorded in the ledger with a
  lease: if the node that usually runs them disappears, the lease expires and another
  node takes over. Tasks that access external networks require WASI with socket
  permission (an explicit executor opt-in).

---

## 7. Trust and security model

The sandbox protects the **executor** node against malicious code (fuel, memory ceiling,
timeout, no I/O by default). Trust in the **result** — the Byzantine problem, where an
executor returns a wrong or forged result — is addressed by two paths, not by the
sandbox:

1. **Permissioned networks** (the primary target): the `AccessController` restricts
   participation to nodes trusted by identity — the user's own devices, or an
   organization's.
2. **k-of-n redundant execution** (§5) for open networks: the same deterministic task
   goes to k independent nodes, results are compared by hash, and divergence lowers the
   lying node's reputation.

**Determinism** is a requirement only of the redundant mode, not of the base model. In
the "1 requester → 1 executor → result written as ordinary data" flow, the result is
just another replicated datum, and non-determinism (clock, randomness) would be
acceptable — but those host functions are not offered precisely to keep determinism
available for when redundancy is used.

---

## 8. Design decisions

1. **Runtime: `wasmtime`** (v46, no default features — only `runtime` + `cranelift`, no
   WASI). A Bytecode Alliance project, with fuel metering and epoch interruption built in
   and the reference implementation of `wasi-nn`. In `src/compute/runtime.rs`.

2. **`input`/`output` format: opaque in the protocol, CBOR as the SDK convention.** The
   protocol and the runtime treat input and output as opaque bytes (`&[u8]` → `Vec<u8>`);
   the meaning is a contract between the WASM module's author and the requester. The
   `guardian-compute-sdk` adopts CBOR (via `ciborium`) as the parameter-serialization
   convention — SDK sugar, invisible to the protocol and the runtime.

3. **Compensation: no payment; reciprocity as a term of use.** There is no credit economy
   nor charging between nodes. By enabling the `compute` feature and joining a Guardian
   Compute network, the operator agrees that their resources may run other nodes' tasks
   on the same network, to the same extent that they use others' (the BitTorrent spirit:
   who downloads, seeds). The term **does not revoke local control**: `max_concurrent`,
   accepted classes and the battery rule stay sovereign. A contractual term is a social
   mechanism, not a cryptographic one; on an open network, the mitigation against
   free-riders is reputation/measured reciprocity (the fuel reported in `ExecMetrics` is
   the objective metric of work), never payment.

4. **ALPN versioning: freeze by version.** `/guardian-db/compute/1` freezes the message
   format; any incompatible change requires `/guardian-db/compute/2` — never a silent
   change under `/1` (the same discipline as the ticket exchange).

---

## 9. Extensions

Documented in their own RFCs, built on this core:

- **Edge AI via `wasi-nn`** ([RFC 0003](0003-guardian-compute-followups.md), feature
  `compute-nn`): links `wasi_ephemeral_nn` into the sandbox as an opt-in grant
  (`HostGrants::nn`), with owner-curated named models distributed as iroh blobs
  (downloaded by hash, ONNX sessions cached), model-affinity routing (`required_model`),
  and GPU via `compute-nn-cuda` (`NnTarget::Gpu` with safe fallback; `Accel::Gpu`
  verified against real CUDA availability). In `src/compute/nn.rs`.
- **LLM inference** ([RFC 0004](0004-compute-llm.md), feature `compute-llm`): the
  `LlmBackend` trait, an owner-curated `LlmRegistry` with active health-checking, and the
  OpenAI-compatible HTTP backend (SSE token streaming). In `src/compute/llm/`.
- **Task-authoring SDK** (`guardian-compute-sdk` + `guardian-compute-sdk-macros`): write
  a task as an ordinary function with `#[guardian_task]` (raw `&[u8]` I/O or typed CBOR
  behind the `cbor` feature), compile to `wasm32-unknown-unknown`, and publish the
  `.wasm` as a blob. Host-function bindings (`guardian::log`, `guardian::store_get`)
  behind the `host` feature.
