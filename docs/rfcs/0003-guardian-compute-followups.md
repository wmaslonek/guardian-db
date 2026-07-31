# Guardian Compute — Edge AI (`wasi-nn`) and the Task-authoring SDK

**Status:** implemented. Part A (Edge AI via `wasi-nn`) complete: opt-in grant, ONNX
backend, blob-backed models with session cache, model-affinity routing, and GPU via
`compute-nn-cuda` with real `Accel::Gpu` verification. Part B (SDK) complete except for
publication to crates.io.
**Scope:** the two extensions from [RFC 0002](0002-guardian-compute.md) §9 — the
`wasi-nn` backend for Edge AI and the `guardian-compute-sdk` crate with the
`#[guardian_task]` macro.
**Base:** the Guardian Compute core (RFC 0002).

---

## Part A — `wasi-nn` backend (Edge AI)

Part A wires the `wasi-nn` API into the Guardian Compute sandbox, enabling neural-network
inference delegated to the most capable node. It reuses the plumbing already present in
the core: the `TaskClass::Inference` class (refused by default by `ExecutorPolicy`), the
opt-in host-functions mechanism (`HostGrants` + Linker in `runtime.rs`), the declarative
accelerator announcement (`TelemetryConfig::accelerators`) and the scoring bonus for
`Inference` (`ScoreWeights::accelerator_bonus`).

### A.1 Backend and feature

- **Cargo feature:** `compute-nn = ["compute", "dep:wasmtime-wasi-nn"]`
  (`wasmtime-wasi-nn 46.0.1`, the same version as `wasmtime`, maintained in the wasmtime
  repository). The ONNX backend is heavy at build time (it downloads native ONNX Runtime
  binaries via `onnx-download`), so it is isolated behind its own feature — anyone who
  wants only basic Guardian Compute does not pay that cost.
- **ONNX via `onnx-download`** is the only portable backend (Windows/Linux/macOS) that
  does not require a manual runtime install by the user; OpenVINO requires Intel's
  install, WinML is Windows-only, `tch` drags in libtorch.
- **Pinning `ort`:** `wasmtime-wasi-nn` 46 declares `ort = "2.0.0-rc.10"`, but cargo
  resolved it to rc.12, which broke the API. A pre-release has no semver guarantee, so
  guardian-db carries a direct dependency `ort = "=2.0.0-rc.10"` purely to pin the
  version in the graph. **Review the pin on every wasmtime upgrade.**

### A.2 Grant and sandbox (`runtime.rs`)

`HostGrants` gains `nn: Option<Arc<NnGrant>>`. When granted, the runtime adds the wasi-nn
context to the Linker (the `HostState` carries the `WasiNnCtx`) registering the named
models. A module that imports `wasi_ephemeral_nn` on an executor without the grant is
refused at instantiation with `HostCapabilityDenied`, like the other capabilities.

The `WasiNnCtx` is built **without generic backends**, so the guest's raw `load`
(arbitrary model bytes) fails with errno — only `load_by_name` over the curated catalog
works. The guest cannot load arbitrary models by bytes: only the ones the executor's
owner offered, keeping resource and content control with the executor.

### A.3 Blob-backed models (`nn.rs`)

The `.onnx` model is distributed as an iroh blob (BLAKE3-hash-addressed — models of tens
or hundreds of MB are exactly the iroh-blobs use case). The `NnModelRegistry` is an
owner-curated `name → hash` catalog, plus a cache of loaded sessions keyed by `(name,
hash)` — re-registering a name with a new hash invalidates the entry. Resolution uses the
same `WasmFetcher` as task code (the `BlobStore` with `get_or_download`, with the
requester as the provider), and the `FsStore` in the data-dir is the on-disk cache
itself.

An `Inference` task **without** a catalog runs in the pure sandbox (small models run
100% in WASM, RFC 0002 §6); the class is a routing label, not a grant requirement. A task
that **imports** wasi-nn without a catalog is refused at instantiation with
`HostCapabilityDenied`.

**Response deadline:** `fuel` does not measure work inside a native host function, and
epoch interruption only acts on WASM code — a long `compute()` call is not abortable
mid-flight. The handler imposes `2×timeout_ms + 1s` over the whole `spawn_blocking`: a
guest stuck in a native call still produces a `DeadlineExceeded` response to the
requester, abandoning the blocked thread until the native call finishes. This holds for
any native host function, not just inference.

### A.4 Model-aware telemetry and scheduler

- `CapabilityVector` carries `nn_models: Vec<String>` (offered names),
  **unconditional on the wire** — nodes without the `compute-nn` feature advertise an
  empty list, because feature-gating the field would break decoding between nodes with
  different builds. The field raised the versioned gossip topic
  (`guardian-db/compute/capabilities/N`); the hysteresis re-publishes when the catalog
  changes.
- `ExecuteRequest::required_model: Option<String>`: the scheduler
  (`rank_for(class, required_model)`) treats model affinity as a **hard constraint** — a
  node without the model is not a candidate, even if stronger — in every mode (execute,
  auction, map, k-of-n). The executor also rejects at admission
  (`RejectReason::ModelNotAvailable`, before the fetch/compile) when a model it does not
  serve is requested, rather than letting `load_by_name` trap later.

### A.5 GPU (`compute-nn-cuda`)

- Feature `compute-nn-cuda = ["compute-nn", "wasmtime-wasi-nn/onnx-cuda"]`.
- **`NnTarget { Cpu, Gpu }`** in the runtime: `NnGrant::new_with_target(...)` and
  `NnModelRegistry::set_execution_target(...)` (changing the target invalidates the
  session cache, reloading on the new target). Asking for GPU is **always safe**: without
  the feature — or without functional CUDA — the upstream backend falls back to CPU with a
  warning and inference stays correct.
- **`Accel::Gpu` verification:** without `compute-nn-cuda`, the accelerator announcement
  stays declarative; with it, the GPU claim **mirrors detection**
  (`CUDAExecutionProvider::is_available()`, consulted once and cached) — a GPU declared
  without real CUDA is removed from the vector, and detected CUDA is announced even
  without a declaration. NPU stays declarative (there is no portable detection).
- **Pending hardware verification:** the `compute-nn-cuda` build compiles, downloads the
  CUDA ONNX Runtime binaries, and the whole suite passes under that binary on a machine
  without a GPU (the GPU target falls back to CPU). Accelerated execution itself and the
  `is_available() == true` path can only be confirmed on a machine with an NVIDIA GPU — no
  new code is expected, only confirmation.

### A.6 Operational notes and limits

- **`Inference` out of k-of-n:** floating-point inference can diverge across
  backends/hardware, so an `Inference` result never enters a redundant round; grants and
  redundancy do not mix.
- **Model provenance** is the executor owner's responsibility: the named registry leaves
  curation (license, origin) with whoever serves the model.
- **Test model without a binary fixture:** the `NnModelRegistry` is validated with a
  minimal ONNX ModelProto (`y = Add(x, x)` over `float[1,2]`) hand-encoded by a ~20-line
  protobuf writer inside the test; the WAT guest speaks the full witx ABI and real
  inference is verified (`[1.5, -2.25]` → `[3.0, -4.5]`). A two-node integration test
  confirms the model download by hash on the first task and cache use on the second, and
  model-based routing (a weak node with the right model beats the strong node without it).

---

## Part B — Task-authoring SDK (`guardian-compute-sdk`)

Writing a task directly against the raw ABI (`gdb_alloc`, entrypoint `(ptr, len) -> i64`
with the output packed) is the biggest adoption barrier. The SDK hides that ABI: the
developer writes an ordinary function, compiles to `wasm32-unknown-unknown`, and
publishes the `.wasm` to the blob store.

```rust
use guardian_compute_sdk::prelude::*;

#[guardian_task]
fn generate_thumbnail(input: &[u8]) -> Result<Vec<u8>, TaskFailure> {
    let img = image::load_from_memory(input)?;
    Ok(img.thumbnail(128, 128).into_bytes())
}
```

### B.1 Structure

Two crates in the workspace, mirroring the `guardian-db-derive` precedent:

```
guardian-compute-sdk/            # lib (compiles to wasm32): ABI runtime + host bindings
guardian-compute-sdk-macros/     # proc-macro: #[guardian_task]
```

The main crate does **not** depend on `guardian-db` (it runs *inside* the sandbox, not
outside).

### B.2 ABI runtime

Usable by hand, without the macro:

- `gdb_alloc` exported (wasm32 only), over Rust's global allocator — the buffers "leak"
  on purpose: the instance dies at the end of execution (RFC 0002 guarantees a fresh store
  per run).
- `abi::input` / `emit` / `pack` / `unpack`: packing the `(ptr << 32) | len` return, with
  `pack`/`unpack` as pure, testable functions.
- The `IntoTaskOutput` trait + `TaskFailure`; a panic crossing `extern "C"` becomes a
  clean trap in wasm (the host maps the trap to `TaskError::Trapped`).

### B.3 The `#[guardian_task]` macro

- Validates the signature (1 parameter, no async/generics/methods) and **does not
  rename** the user's function: it generates a sibling wrapper `__guardian_task_<name>`
  with `#[unsafe(export_name = "<name>")]` — the wasm export matches the `entrypoint` of
  the `ExecuteRequest`/`TaskSpec`, and the original function stays callable.
- Accepts `fn(&[u8]) -> Vec<u8>` and `fn(&[u8]) -> Result<Vec<u8>, TaskFailure>` (the
  `Err` becomes a trap with a message).
- **The `#[guardian_task(cbor)]` variant** (feature `cbor`): a typed signature
  `fn(In) -> Result<Out, E: Display>` with `In: DeserializeOwned, Out: Serialize`,
  serializing with **`ciborium`** (the convention from RFC 0002 §8.2: opaque in the
  protocol, CBOR in the SDK). The `cbor` module does `decode` (invalid CBOR → panic →
  trap, never a phantom value), `encode` and `emit_result`; requiring a `Result` return
  forces error handling and avoids a blanket-impl conflict on the output trait.

### B.4 Host bindings (feature `host`)

A `host` module with `log(&str)` and `store_get(&[u8]) -> Option<Vec<u8>>` — imports
`#[link(wasm_import_module = "gdb")]`. `store_get` implements the two-call protocol (a
4 KiB buffer, retry with the exact size when the value is truncated). On non-wasm targets
the bindings are inert (no-op/`None`), for the hosts builds of the examples. Only whoever
*calls* the bindings imports the functions — a module that does not use them runs on any
executor.

### B.5 Examples and tests

Examples as `[[example]] crate-type = ["cdylib"]`: `echo_task`, `shout_task` (the
`Result`/`Err` path), `word_count_task` (cbor, struct→struct) and `lookup_task` (host,
reads the executor's store).

The golden test (`tests/compute_sdk.rs`, feature `compute`) compiles the examples to
wasm32 via a nested cargo and runs them on the real `WasmRuntime`: echo, empty input,
typed CBOR roundtrip, malformed CBOR becoming a trap, `Err` becoming a trap, fuel
starving, and the host bindings in three situations — executor without the grant
(`HostCapabilityDenied` before running), with the grant (value read) and a missing key
(`Err` → trap). It skips with a clear message when the wasm32 target is not installed.

Having the golden tests run the SDK's `.wasm` against the real `WasmRuntime` is what
guards against the SDK's ABI diverging from the runtime's: any divergence breaks the test.
The SDK only exposes what the runtime already offers — a new capability is born in the
runtime (RFC 0002) before it gets a binding here.
