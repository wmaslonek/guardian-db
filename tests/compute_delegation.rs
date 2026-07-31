//! End-to-end integration tests for Guardian Compute Phase 2 (RFC 0002):
//! direct 1-to-1 task delegation between two real iroh endpoints, in-process.
//!
//! The full RFC flow is exercised: the requester publishes the `.wasm` to its
//! own iroh-blobs store, the executor fetches it *from the requester* by hash
//! (integrity verified by construction), runs it in the wasmtime sandbox and
//! replies with output + metrics over the compute ALPN.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;

use guardian_db::compute::{
    COMPUTE_ALPN, CapabilityDirectory, CapabilityGossip, ComputeCallError, ComputeClient,
    ComputeProtocolHandler, ComputeScheduler, ExecuteRequest, ExecutorPolicy, RejectReason,
    ResourceLimits, ScheduleError, SchedulerConfig, TaskClass, TaskError, TelemetryConfig,
};
use guardian_db::p2p::network::core::blobs::BlobStore;
use iroh::endpoint::{Endpoint, presets};
use iroh::protocol::Router;
use iroh::{EndpointAddr, TransportAddr};
use iroh_blobs::BlobsProtocol;
use iroh_blobs::store::fs::FsStore;
use iroh_gossip::net::Gossip;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Echo guest: bump allocator + entrypoint that returns the input unchanged.
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

/// Hostile guest: spins forever; only the executor's limits can stop it.
const SPIN_WAT: &str = r#"
    (module
      (memory (export "memory") 1)
      (func (export "gdb_alloc") (param i32) (result i32) (i32.const 1024))
      (func (export "gdb_run") (param i32 i32) (result i64)
        (loop $spin (br $spin))
        (i64.const 0)))
"#;

/// A dialable address for an endpoint bound on unspecified interfaces.
fn dialable_addr(endpoint: &Endpoint) -> EndpointAddr {
    let addrs: Vec<TransportAddr> = endpoint
        .bound_sockets()
        .into_iter()
        .map(|mut socket| {
            if socket.ip().is_unspecified() {
                let loopback = if socket.is_ipv4() {
                    IpAddr::V4(Ipv4Addr::LOCALHOST)
                } else {
                    IpAddr::V6(Ipv6Addr::LOCALHOST)
                };
                socket.set_ip(loopback);
            }
            TransportAddr::Ip(socket)
        })
        .collect();
    EndpointAddr::from_parts(endpoint.id(), addrs)
}

/// Registers `peer`'s address on `endpoint` so it can be dialed by bare id
/// (no discovery services in the Minimal preset).
fn seed_peer_addr(endpoint: &Endpoint, peer: EndpointAddr) {
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    lookup.add_endpoint_info(peer);
    endpoint
        .address_lookup()
        .expect("address lookup available")
        .add(lookup);
}

/// A full test node: endpoint, blob store, gossip, and a compute executor.
struct TestNode {
    endpoint: Endpoint,
    addr: EndpointAddr,
    blob_store: BlobStore,
    gossip: Gossip,
    handler: ComputeProtocolHandler,
    _router: Router,
    _dir: tempfile::TempDir,
}

async fn spawn_node(policy: ExecutorPolicy) -> TestNode {
    let dir = tempfile::tempdir().expect("temp dir");
    let fs_store = FsStore::load(dir.path()).await.expect("fs store");
    let endpoint = Endpoint::builder(presets::Minimal)
        .bind()
        .await
        .expect("bind endpoint");

    let blob_store =
        BlobStore::new_with_endpoint(Arc::new(RwLock::new(fs_store.clone())), endpoint.clone());
    let gossip = Gossip::builder().spawn(endpoint.clone());
    let handler =
        ComputeProtocolHandler::new(Arc::new(blob_store.clone()), policy).expect("compute handler");

    let router = Router::builder(endpoint.clone())
        .accept(
            iroh_blobs::ALPN,
            BlobsProtocol::new(fs_store.as_ref(), None),
        )
        .accept(iroh_gossip::ALPN, gossip.clone())
        .accept(COMPUTE_ALPN, handler.clone())
        .spawn();

    let addr = dialable_addr(&endpoint);
    TestNode {
        endpoint,
        addr,
        blob_store,
        gossip,
        handler,
        _router: router,
        _dir: dir,
    }
}

/// Spins up a connected requester/executor pair with the given executor policy.
async fn spawn_pair(policy: ExecutorPolicy) -> (TestNode, TestNode) {
    let requester = spawn_node(ExecutorPolicy::default()).await;
    let executor = spawn_node(policy).await;
    // The executor must be able to dial the requester back (blob fetch) even
    // though the Minimal preset has no discovery.
    seed_peer_addr(&executor.endpoint, requester.addr.clone());
    (requester, executor)
}

fn request(wasm_hash: iroh_blobs::Hash, input: &[u8]) -> ExecuteRequest {
    ExecuteRequest {
        task_id: Uuid::new_v4(),
        wasm_hash,
        entrypoint: "gdb_run".into(),
        class: TaskClass::General,
        limits: ResourceLimits::default(),
        input: input.to_vec(),
        required_model: None,
    }
}

const CALL_TIMEOUT: Duration = Duration::from_secs(60);

#[tokio::test]
async fn delegated_task_runs_on_the_executor_and_echoes() {
    let (requester, executor) = spawn_pair(ExecutorPolicy::default()).await;

    // 1. Requester publishes the task code to its own blob store.
    let wasm = wat::parse_str(ECHO_WAT).expect("valid wat");
    let hash = requester
        .blob_store
        .add_document(wasm.into())
        .await
        .expect("publish wasm");

    // 2. Direct delegation: "run this on that node".
    let client = ComputeClient::new(requester.endpoint.clone());
    let input = b"processado no no vizinho";
    let done = client
        .execute_on(executor.addr.clone(), request(hash, input), CALL_TIMEOUT)
        .await
        .expect("delegated execution");

    // 3. The result comes back with the executor's metrics.
    assert_eq!(done.output, input);
    assert!(done.metrics.fuel_consumed > 0);
    assert_eq!(executor.handler.tasks_running(), 0, "slot released");

    // 4. Second call: the executor now has the blob and the compiled module
    //    cached — must work without re-fetching from the requester.
    let done_again = client
        .execute_on(
            executor.addr.clone(),
            request(hash, b"segunda rodada"),
            CALL_TIMEOUT,
        )
        .await
        .expect("cached execution");
    assert_eq!(done_again.output, b"segunda rodada");
}

#[tokio::test]
async fn executor_policy_rejects_class_not_accepted() {
    let (requester, executor) = spawn_pair(ExecutorPolicy::default()).await;

    let wasm = wat::parse_str(ECHO_WAT).expect("valid wat");
    let hash = requester
        .blob_store
        .add_document(wasm.into())
        .await
        .expect("publish wasm");

    let client = ComputeClient::new(requester.endpoint.clone());
    let mut req = request(hash, b"x");
    req.class = TaskClass::Inference; // default policy keeps Inference off
    let err = client
        .execute_on(executor.addr.clone(), req, CALL_TIMEOUT)
        .await
        .unwrap_err();

    assert_eq!(
        err,
        ComputeCallError::Rejected(RejectReason::ClassNotAccepted)
    );
}

#[tokio::test]
async fn hostile_task_fails_remotely_with_the_sandbox_error() {
    let (requester, executor) = spawn_pair(ExecutorPolicy::default()).await;

    let wasm = wat::parse_str(SPIN_WAT).expect("valid wat");
    let hash = requester
        .blob_store
        .add_document(wasm.into())
        .await
        .expect("publish wasm");

    let client = ComputeClient::new(requester.endpoint.clone());
    let mut req = request(hash, b"");
    req.limits = ResourceLimits {
        fuel: 100_000,
        ..ResourceLimits::default()
    };
    let err = client
        .execute_on(executor.addr.clone(), req, CALL_TIMEOUT)
        .await
        .unwrap_err();

    // The sandbox verdict crosses the wire intact.
    assert_eq!(err, ComputeCallError::Task(TaskError::FuelExhausted));
    assert_eq!(
        executor.handler.tasks_running(),
        0,
        "slot released on failure"
    );
}

#[tokio::test]
async fn unknown_wasm_hash_fails_as_unavailable() {
    let (requester, executor) = spawn_pair(ExecutorPolicy::default()).await;

    // Hash of content nobody ever published.
    let bogus = iroh_blobs::Hash::new(b"never published anywhere");
    let client = ComputeClient::new(requester.endpoint.clone());
    let err = client
        .execute_on(
            executor.addr.clone(),
            request(bogus, b""),
            Duration::from_secs(30),
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, ComputeCallError::Task(TaskError::WasmUnavailable(_))),
        "expected WasmUnavailable, got: {err:?}"
    );
    let _ = executor;
}

#[tokio::test]
async fn owner_policy_change_applies_immediately() {
    let (requester, executor) = spawn_pair(ExecutorPolicy::default()).await;

    let wasm = wat::parse_str(ECHO_WAT).expect("valid wat");
    let hash = requester
        .blob_store
        .add_document(wasm.into())
        .await
        .expect("publish wasm");
    let client = ComputeClient::new(requester.endpoint.clone());

    // Owner turns execution off (RFC §8.3: local policy is sovereign).
    executor.handler.set_policy(ExecutorPolicy {
        max_concurrent: 0,
        ..ExecutorPolicy::default()
    });
    let err = client
        .execute_on(executor.addr.clone(), request(hash, b"x"), CALL_TIMEOUT)
        .await
        .unwrap_err();
    assert_eq!(err, ComputeCallError::Rejected(RejectReason::Busy));

    // Owner turns it back on.
    executor.handler.set_policy(ExecutorPolicy::default());
    let done = client
        .execute_on(executor.addr.clone(), request(hash, b"y"), CALL_TIMEOUT)
        .await
        .expect("execution after re-enable");
    assert_eq!(done.output, b"y");
}

// ─── Phase 3: capability telemetry + scheduler ───────────────────────────────

/// Fast telemetry for tests: sample often, publish eagerly.
fn fast_telemetry() -> TelemetryConfig {
    TelemetryConfig {
        sample_interval: Duration::from_millis(200),
        ..TelemetryConfig::default()
    }
}

/// Waits until `directory` knows at least `n` peers, or panics after 30 s.
async fn wait_for_peers(directory: &Arc<CapabilityDirectory>, n: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while directory.len() < n {
        assert!(
            tokio::time::Instant::now() < deadline,
            "capability directory never reached {n} peers (has {})",
            directory.len()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn scheduler_picks_the_available_node_via_capability_gossip() {
    // Requester + two executors: one available, one advertising zero slots.
    let requester = spawn_node(ExecutorPolicy::default()).await;
    let available = spawn_node(ExecutorPolicy::default()).await;
    let unavailable = spawn_node(ExecutorPolicy {
        max_concurrent: 0,
        ..ExecutorPolicy::default()
    })
    .await;

    // Executors can dial the requester back (blob fetch), the requester can
    // dial the executors (delegation), and both executors can reach the
    // requester's gossip mesh.
    seed_peer_addr(&available.endpoint, requester.addr.clone());
    seed_peer_addr(&unavailable.endpoint, requester.addr.clone());
    seed_peer_addr(&requester.endpoint, available.addr.clone());
    seed_peer_addr(&requester.endpoint, unavailable.addr.clone());

    // Capability telemetry on all three nodes; executors bootstrap off the
    // requester so the mesh forms without any discovery service.
    let requester_gossip = CapabilityGossip::spawn(
        requester.gossip.clone(),
        requester.endpoint.id(),
        requester.handler.clone(),
        Arc::new(CapabilityDirectory::new()),
        vec![],
        fast_telemetry(),
    )
    .await
    .expect("requester capability gossip");
    let _available_gossip = CapabilityGossip::spawn(
        available.gossip.clone(),
        available.endpoint.id(),
        available.handler.clone(),
        Arc::new(CapabilityDirectory::new()),
        vec![requester.endpoint.id()],
        fast_telemetry(),
    )
    .await
    .expect("available capability gossip");
    let _unavailable_gossip = CapabilityGossip::spawn(
        unavailable.gossip.clone(),
        unavailable.endpoint.id(),
        unavailable.handler.clone(),
        Arc::new(CapabilityDirectory::new()),
        vec![requester.endpoint.id()],
        fast_telemetry(),
    )
    .await
    .expect("unavailable capability gossip");

    // The requester hears both executors over gossip.
    let directory = requester_gossip.directory();
    wait_for_peers(&directory, 2).await;

    let scheduler = ComputeScheduler::new(
        ComputeClient::new(requester.endpoint.clone()),
        directory,
        requester.endpoint.id(),
        SchedulerConfig::default(),
    );

    // Ranking: only the node with free slots is a candidate.
    let ranked = scheduler.rank(TaskClass::General);
    assert_eq!(
        ranked.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        vec![available.endpoint.id()],
        "the zero-slot node must not be a candidate"
    );

    // `execute` with no destination: the network decides — and it decides
    // for the available executor.
    let wasm = wat::parse_str(ECHO_WAT).expect("valid wat");
    let hash = requester
        .blob_store
        .add_document(wasm.into())
        .await
        .expect("publish wasm");
    let delegated = scheduler
        .execute(request(hash, b"orquestrado pela rede"))
        .await
        .expect("scheduled execution");

    assert_eq!(delegated.executor, available.endpoint.id());
    assert_eq!(delegated.completed.output, b"orquestrado pela rede");
}

#[tokio::test]
async fn scheduler_fails_over_to_the_next_candidate() {
    let (requester, executor) = spawn_pair(ExecutorPolicy::default()).await;
    seed_peer_addr(&requester.endpoint, executor.addr.clone());

    // Directory fed by hand: a phantom "stronger" node that does not exist,
    // ranked above the real executor.
    let directory = Arc::new(CapabilityDirectory::new());
    let phantom = iroh::SecretKey::generate().public();
    directory.upsert(guardian_db::compute::CapabilityVector {
        node_id: phantom,
        cpu_cores: 64,
        cpu_arch: guardian_db::compute::CpuArch::X86_64,
        ram_total_mb: 256_000,
        accelerators: vec![],
        cpu_load_pct: 0,
        ram_free_mb: 200_000,
        on_battery: false,
        battery_pct: None,
        tasks_running: 0,
        max_concurrent: 8,
        accepts: vec![TaskClass::General],
        nn_models: vec![],
        llm_models: vec![],
        embed_models: vec![],
        issued_at: 0,
    });
    directory.upsert(guardian_db::compute::CapabilityVector {
        node_id: executor.endpoint.id(),
        cpu_cores: 4,
        cpu_arch: guardian_db::compute::CpuArch::X86_64,
        ram_total_mb: 8_000,
        accelerators: vec![],
        cpu_load_pct: 50,
        ram_free_mb: 4_000,
        on_battery: false,
        battery_pct: None,
        tasks_running: 0,
        max_concurrent: 2,
        accepts: vec![TaskClass::General],
        nn_models: vec![],
        llm_models: vec![],
        embed_models: vec![],
        issued_at: 0,
    });

    let scheduler = ComputeScheduler::new(
        ComputeClient::new(requester.endpoint.clone()),
        directory.clone(),
        requester.endpoint.id(),
        SchedulerConfig {
            attempt_timeout: Duration::from_secs(5),
            ..SchedulerConfig::default()
        },
    );

    // Sanity: the phantom ranks first.
    let ranked = scheduler.rank(TaskClass::General);
    assert_eq!(ranked[0].0, phantom);

    let wasm = wat::parse_str(ECHO_WAT).expect("valid wat");
    let hash = requester
        .blob_store
        .add_document(wasm.into())
        .await
        .expect("publish wasm");
    let delegated = scheduler
        .execute(request(hash, b"failover automatico"))
        .await
        .expect("failover execution");

    // The phantom failed; the real executor got the task; the phantom's
    // vector was evicted so it is not ranked again.
    assert_eq!(delegated.executor, executor.endpoint.id());
    assert_eq!(delegated.completed.output, b"failover automatico");
    assert!(directory.get(&phantom, Duration::from_secs(600)).is_none());
}

#[tokio::test]
async fn scheduler_reports_no_candidates_on_empty_network() {
    let requester = spawn_node(ExecutorPolicy::default()).await;
    let scheduler = ComputeScheduler::new(
        ComputeClient::new(requester.endpoint.clone()),
        Arc::new(CapabilityDirectory::new()),
        requester.endpoint.id(),
        SchedulerConfig::default(),
    );
    let err = scheduler
        .execute(request(iroh_blobs::Hash::new(b"x"), b""))
        .await
        .unwrap_err();
    assert!(matches!(err, ScheduleError::NoCandidates));
}

/// Guest that imports `gdb.log` (calls it, then echoes). It only instantiates
/// on an executor that granted the log capability.
const LOG_ECHO_WAT: &str = r#"
    (module
      (import "gdb" "log" (func $log (param i32 i32)))
      (memory (export "memory") 1)
      (global $next (mut i32) (i32.const 1024))
      (func (export "gdb_alloc") (param $len i32) (result i32)
        (local $ptr i32)
        (local.set $ptr (global.get $next))
        (global.set $next (i32.add (global.get $next) (local.get $len)))
        (local.get $ptr))
      (func (export "gdb_run") (param $ptr i32) (param $len i32) (result i64)
        (call $log (local.get $ptr) (local.get $len))
        (i64.or
          (i64.shl (i64.extend_i32_u (local.get $ptr)) (i64.const 32))
          (i64.extend_i32_u (local.get $len)))))
"#;

/// A transient (node-specific) task failure must fail over to the next
/// candidate, not abort the whole delegation. The top-ranked executor lacks
/// the `gdb.log` capability the task needs, so it returns
/// `HostCapabilityDenied` — a node-specific failure; the scheduler must then
/// succeed on the node that granted it.
#[tokio::test]
async fn transient_task_failure_fails_over_to_next_candidate() {
    let requester = spawn_node(ExecutorPolicy::default()).await;
    let broken = spawn_node(ExecutorPolicy::default()).await; // ranked first, no grant
    let healthy = spawn_node(ExecutorPolicy::default()).await;
    // Only `healthy` grants the log capability the task imports.
    healthy
        .handler
        .set_host_grants(guardian_db::compute::HostGrants {
            log: true,
            ..guardian_db::compute::HostGrants::default()
        });

    // The requester can dial both; both can dial the requester back to fetch
    // the blob (the incoming compute connection already provides a path).
    seed_peer_addr(&requester.endpoint, broken.addr.clone());
    seed_peer_addr(&requester.endpoint, healthy.addr.clone());

    let wasm = wat::parse_str(LOG_ECHO_WAT).expect("valid wat");
    let hash = requester
        .blob_store
        .add_document(wasm.into())
        .await
        .expect("publish wasm");

    // Rank `broken` above `healthy` (stronger vector) so it is tried first.
    let directory = Arc::new(CapabilityDirectory::new());
    feed_vector(&directory, broken.endpoint.id(), 64, 8);
    feed_vector(&directory, healthy.endpoint.id(), 4, 2);
    let scheduler = ComputeScheduler::new(
        ComputeClient::new(requester.endpoint.clone()),
        directory,
        requester.endpoint.id(),
        SchedulerConfig {
            attempt_timeout: Duration::from_secs(10),
            ..SchedulerConfig::default()
        },
    );
    assert_eq!(
        scheduler.rank(TaskClass::General)[0].0,
        broken.endpoint.id()
    );

    let delegated = scheduler
        .execute(request(hash, b"failover em erro transiente"))
        .await
        .expect("must fail over to the healthy node");
    assert_eq!(delegated.executor, healthy.endpoint.id());
    assert_eq!(delegated.completed.output, b"failover em erro transiente");
}

// ─── Phase 4: reactive triggers + task ledger ────────────────────────────────

/// The RFC §6.2 media flow, end to end: a "new photo replicated" event fires
/// a trigger rule; the engine claims the task in the ledger (deduplicating
/// the event that fires on every replica), dispatches it through the
/// capability-aware scheduler, and records the executor + result.
#[tokio::test]
async fn replication_event_triggers_processing_on_the_best_node() {
    let (requester, executor) = spawn_pair(ExecutorPolicy::default()).await;
    seed_peer_addr(&requester.endpoint, executor.addr.clone());

    // The executor is known to the scheduler (vector fed directly; the full
    // gossip path is covered by the Phase 3 test).
    let directory = Arc::new(CapabilityDirectory::new());
    directory.upsert(guardian_db::compute::CapabilityVector {
        node_id: executor.endpoint.id(),
        cpu_cores: 8,
        cpu_arch: guardian_db::compute::CpuArch::X86_64,
        ram_total_mb: 16_000,
        accelerators: vec![],
        cpu_load_pct: 10,
        ram_free_mb: 8_000,
        on_battery: false,
        battery_pct: None,
        tasks_running: 0,
        max_concurrent: 2,
        accepts: vec![TaskClass::General, TaskClass::Media],
        nn_models: vec![],
        llm_models: vec![],
        embed_models: vec![],
        issued_at: 0,
    });
    let scheduler = ComputeScheduler::new(
        ComputeClient::new(requester.endpoint.clone()),
        directory,
        requester.endpoint.id(),
        SchedulerConfig::default(),
    );

    // The "thumbnailer" is published once to the requester's blob store.
    let wasm = wat::parse_str(ECHO_WAT).expect("valid wat");
    let wasm_hash = requester
        .blob_store
        .add_document(wasm.into())
        .await
        .expect("publish wasm");

    let engine = Arc::new(guardian_db::compute::TriggerEngine::new(
        guardian_db::compute::TaskLedger::in_memory(),
        Arc::new(scheduler),
        guardian_db::compute::TriggerConfig::default(),
    ));
    engine.on_replicated(
        "/fotos",
        guardian_db::compute::TaskSpec {
            wasm_hash,
            entrypoint: "gdb_run".into(),
            class: TaskClass::Media,
            limits: ResourceLimits::default(),
            placement: guardian_db::compute::Placement::BestAvailable,
            required_model: None,
        },
    );

    // A new photo lands in the store — and the same event arrives twice, as
    // it would on a network where every replica observes the replication.
    let photo = b"bytes da foto pesada";
    let claimed = engine
        .notify_replicated("/fotos/ferias", b"entry-abc", photo)
        .await;
    assert_eq!(claimed.len(), 1);
    let key = claimed[0].clone();
    assert!(
        engine
            .notify_replicated("/fotos/ferias", b"entry-abc", photo)
            .await
            .is_empty(),
        "the duplicate firing must lose the ledger claim"
    );

    // The task completes on the executor, recorded in the ledger.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let record = loop {
        let record = engine.ledger().get(&key).await.unwrap().unwrap();
        if matches!(
            record.state,
            guardian_db::compute::TaskState::Done { .. }
                | guardian_db::compute::TaskState::Failed { .. }
        ) {
            break record;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "triggered task never finished"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    match record.state {
        guardian_db::compute::TaskState::Done { executor: ran_on } => {
            assert_eq!(ran_on, executor.endpoint.id());
        }
        other => panic!("expected Done, got {other:?}"),
    }
    assert_eq!(record.result.unwrap().output, photo);
    assert_eq!(record.attempts, 1);
}

// ─── Phase 5: auction, map fan-out, k-of-n redundancy, host functions ────────

/// Guest that answers with the local-store value for the key given as input
/// (imports the `gdb.log` and `gdb.store_get` host capabilities).
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

fn feed_vector(directory: &CapabilityDirectory, node: iroh::EndpointId, cores: u16, slots: u8) {
    directory.upsert(guardian_db::compute::CapabilityVector {
        node_id: node,
        cpu_cores: cores,
        cpu_arch: guardian_db::compute::CpuArch::X86_64,
        ram_total_mb: 16_000,
        accelerators: vec![],
        cpu_load_pct: 10,
        ram_free_mb: 8_000,
        on_battery: false,
        battery_pct: None,
        tasks_running: 0,
        max_concurrent: slots,
        accepts: vec![TaskClass::General, TaskClass::Media],
        nn_models: vec![],
        llm_models: vec![],
        embed_models: vec![],
        issued_at: 0,
    });
}

/// The point of the auction: a stale gossip vector says the "strong" node is
/// available, but its fresh probe bid says otherwise — the auction routes to
/// the truly available node without burning a failed delegation attempt.
#[tokio::test]
async fn auction_trusts_fresh_bids_over_stale_gossip() {
    let requester = spawn_node(ExecutorPolicy::default()).await;
    let available = spawn_node(ExecutorPolicy::default()).await;
    let stale_strong = spawn_node(ExecutorPolicy {
        max_concurrent: 0, // reality: accepts nothing
        ..ExecutorPolicy::default()
    })
    .await;
    seed_peer_addr(&requester.endpoint, available.addr.clone());
    seed_peer_addr(&requester.endpoint, stale_strong.addr.clone());
    seed_peer_addr(&available.endpoint, requester.addr.clone());

    let directory = Arc::new(CapabilityDirectory::new());
    feed_vector(&directory, available.endpoint.id(), 4, 2);
    // The stale lie: gossip still claims the strong node has slots.
    feed_vector(&directory, stale_strong.endpoint.id(), 64, 8);

    let scheduler = ComputeScheduler::new(
        ComputeClient::new(requester.endpoint.clone()),
        directory,
        requester.endpoint.id(),
        SchedulerConfig::default(),
    );
    // Sanity: gossip ranking would try the stale node first.
    assert_eq!(
        scheduler.rank(TaskClass::General)[0].0,
        stale_strong.endpoint.id()
    );

    let wasm = wat::parse_str(ECHO_WAT).expect("valid wat");
    let hash = requester
        .blob_store
        .add_document(wasm.into())
        .await
        .expect("publish wasm");
    let delegated = scheduler
        .execute_with_auction(request(hash, b"leilao"))
        .await
        .expect("auction execution");

    assert_eq!(delegated.executor, available.endpoint.id());
    assert_eq!(delegated.completed.output, b"leilao");
}

#[tokio::test]
async fn map_fans_out_and_preserves_order() {
    let requester = spawn_node(ExecutorPolicy::default()).await;
    let a = spawn_node(ExecutorPolicy::default()).await;
    let b = spawn_node(ExecutorPolicy::default()).await;
    for executor in [&a, &b] {
        seed_peer_addr(&requester.endpoint, executor.addr.clone());
        seed_peer_addr(&executor.endpoint, requester.addr.clone());
    }

    let directory = Arc::new(CapabilityDirectory::new());
    feed_vector(&directory, a.endpoint.id(), 8, 2);
    feed_vector(&directory, b.endpoint.id(), 8, 2);
    let scheduler = ComputeScheduler::new(
        ComputeClient::new(requester.endpoint.clone()),
        directory,
        requester.endpoint.id(),
        SchedulerConfig::default(),
    );

    let wasm = wat::parse_str(ECHO_WAT).expect("valid wat");
    let hash = requester
        .blob_store
        .add_document(wasm.into())
        .await
        .expect("publish wasm");

    let partitions: Vec<&[u8]> = vec![b"fatia-0", b"fatia-1", b"fatia-2", b"fatia-3"];
    let requests = partitions.iter().map(|p| request(hash, p)).collect();
    let results = scheduler.map(requests).await;

    assert_eq!(results.len(), 4);
    let known = [a.endpoint.id(), b.endpoint.id()];
    for (i, result) in results.into_iter().enumerate() {
        let delegated = result.expect("map partition");
        assert_eq!(delegated.completed.output, partitions[i], "order preserved");
        assert!(known.contains(&delegated.executor));
    }
}

#[tokio::test]
async fn redundant_execution_reaches_agreement() {
    let requester = spawn_node(ExecutorPolicy::default()).await;
    let a = spawn_node(ExecutorPolicy::default()).await;
    let b = spawn_node(ExecutorPolicy::default()).await;
    for executor in [&a, &b] {
        seed_peer_addr(&requester.endpoint, executor.addr.clone());
        seed_peer_addr(&executor.endpoint, requester.addr.clone());
    }

    let directory = Arc::new(CapabilityDirectory::new());
    feed_vector(&directory, a.endpoint.id(), 8, 2);
    feed_vector(&directory, b.endpoint.id(), 8, 2);
    let scheduler = ComputeScheduler::new(
        ComputeClient::new(requester.endpoint.clone()),
        directory,
        requester.endpoint.id(),
        SchedulerConfig::default(),
    );

    let wasm = wat::parse_str(ECHO_WAT).expect("valid wat");
    let hash = requester
        .blob_store
        .add_document(wasm.into())
        .await
        .expect("publish wasm");

    let outcome = scheduler
        .execute_redundant(request(hash, b"verificado em dobro"), 2)
        .await
        .expect("redundant execution");

    assert_eq!(outcome.responded, 2);
    assert_eq!(outcome.agreements, 2, "deterministic echo must agree");
    assert!(outcome.divergent.is_empty());
    assert_eq!(outcome.delegated.completed.output, b"verificado em dobro");
    // Both honest nodes keep (or regain) full standing.
    assert_eq!(scheduler.reputation().factor(&a.endpoint.id()), 1.0);
    assert_eq!(scheduler.reputation().factor(&b.endpoint.id()), 1.0);
}

struct TestStoreReader;

impl guardian_db::compute::HostStoreReader for TestStoreReader {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        (key == b"config:tema").then(|| b"escuro".to_vec())
    }
}

#[tokio::test]
async fn host_functions_run_end_to_end_only_when_granted() {
    let (requester, executor) = spawn_pair(ExecutorPolicy::default()).await;

    let wasm = wat::parse_str(STORE_READER_WAT).expect("valid wat");
    let hash = requester
        .blob_store
        .add_document(wasm.into())
        .await
        .expect("publish wasm");
    let client = ComputeClient::new(requester.endpoint.clone());

    // Without grants (the default), the module imports are refused.
    let err = client
        .execute_on(
            executor.addr.clone(),
            request(hash, b"config:tema"),
            CALL_TIMEOUT,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            ComputeCallError::Task(TaskError::HostCapabilityDenied(_))
        ),
        "expected HostCapabilityDenied, got: {err:?}"
    );

    // The owner opts in: log + read access to the local store.
    // (`..default()` covers the wasi-nn field under `compute-nn`.)
    #[cfg_attr(not(feature = "compute-nn"), allow(clippy::needless_update))]
    let grants = guardian_db::compute::HostGrants {
        log: true,
        store: Some(Arc::new(TestStoreReader)),
        ..guardian_db::compute::HostGrants::default()
    };
    executor.handler.set_host_grants(grants);
    let done = client
        .execute_on(
            executor.addr.clone(),
            request(hash, b"config:tema"),
            CALL_TIMEOUT,
        )
        .await
        .expect("granted execution");
    assert_eq!(
        done.output, b"escuro",
        "guest read the executor local store"
    );
}

// ─── RFC 0003 phase NN-2: blob-backed NN models over two nodes ───────────────

/// Edge AI end to end (feature `compute-nn`): the requester publishes both
/// the task wasm *and* the ONNX model as blobs; the executor's owner
/// registers the model by hash; the first Inference task makes the executor
/// download the model from the requester and load a session, the second is
/// served entirely from cache.
#[cfg(feature = "compute-nn")]
mod nn_delegation {
    use super::*;
    use guardian_db::compute::NnModelRegistry;

    /// Guest speaking the `wasi_ephemeral_nn` witx ABI (same as the runtime
    /// unit test): doubles a two-f32 tensor via the "doubler" named model.
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
            (call $check
              (call $load_by_name (i32.const 128) (i32.const 7) (i32.const 256)))
            (local.set $graph (i32.load (i32.const 256)))
            (call $check (call $init_ctx (local.get $graph) (i32.const 260)))
            (local.set $ctx (i32.load (i32.const 260)))
            (i32.store (i32.const 320) (i32.const 1))
            (i32.store (i32.const 324) (i32.const 2))
            (i32.store (i32.const 300) (i32.const 320))
            (i32.store (i32.const 304) (i32.const 2))
            (i32.store8 (i32.const 308) (i32.const 1))
            (i32.store (i32.const 312) (local.get $ptr))
            (i32.store (i32.const 316) (local.get $len))
            (call $check
              (call $set_input (local.get $ctx) (i32.const 0) (i32.const 300)))
            (call $check (call $compute (local.get $ctx)))
            (call $check
              (call $get_output (local.get $ctx) (i32.const 0)
                    (i32.const 2048) (i32.const 1024) (i32.const 280)))
            (local.set $outsize (i32.load (i32.const 280)))
            (i64.or
              (i64.shl (i64.const 2048) (i64.const 32))
              (i64.extend_i32_u (local.get $outsize)))))
    "#;

    // Minimal protobuf writer + `y = Add(x, x)` ONNX model (same construction
    // as the runtime unit test — kept local, integration tests cannot share
    // unit-test code).
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
        varint(field << 3, out);
        varint(value, out);
    }

    fn bytes_field(field: u64, bytes: &[u8], out: &mut Vec<u8>) {
        varint((field << 3) | 2, out);
        varint(bytes.len() as u64, out);
        out.extend_from_slice(bytes);
    }

    fn doubler_onnx_model() -> Vec<u8> {
        let mut shape = Vec::new();
        for extent in [1u64, 2] {
            let mut dim = Vec::new();
            varint_field(1, extent, &mut dim);
            bytes_field(1, &dim, &mut shape);
        }
        let mut tensor_type = Vec::new();
        varint_field(1, 1, &mut tensor_type);
        bytes_field(2, &shape, &mut tensor_type);
        let mut type_proto = Vec::new();
        bytes_field(1, &tensor_type, &mut type_proto);

        let value_info = |name: &str| {
            let mut vi = Vec::new();
            bytes_field(1, name.as_bytes(), &mut vi);
            bytes_field(2, &type_proto, &mut vi);
            vi
        };

        let mut node = Vec::new();
        bytes_field(1, b"x", &mut node);
        bytes_field(1, b"x", &mut node);
        bytes_field(2, b"y", &mut node);
        bytes_field(4, b"Add", &mut node);

        let mut graph = Vec::new();
        bytes_field(1, &node, &mut graph);
        bytes_field(2, b"doubler", &mut graph);
        bytes_field(11, &value_info("x"), &mut graph);
        bytes_field(12, &value_info("y"), &mut graph);

        let mut opset = Vec::new();
        varint_field(2, 13, &mut opset);

        let mut model = Vec::new();
        varint_field(1, 8, &mut model);
        bytes_field(7, &graph, &mut model);
        bytes_field(8, &opset, &mut model);
        model
    }

    fn f32s(bytes: &[u8]) -> Vec<f32> {
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }

    fn inference_policy() -> ExecutorPolicy {
        ExecutorPolicy {
            accepts: vec![TaskClass::General, TaskClass::Inference],
            ..ExecutorPolicy::default()
        }
    }

    #[tokio::test]
    async fn executor_fetches_the_model_by_hash_and_serves_from_cache() {
        let (requester, executor) = spawn_pair(inference_policy()).await;

        // The requester publishes both the task code and the ONNX model.
        let wasm = wat::parse_str(NN_WAT).expect("valid wat");
        let wasm_hash = requester
            .blob_store
            .add_document(wasm.into())
            .await
            .expect("publish wasm");
        let model_hash = requester
            .blob_store
            .add_document(doubler_onnx_model().into())
            .await
            .expect("publish model");

        // The executor's owner curates the catalog: name → blob hash.
        let registry = Arc::new(NnModelRegistry::new());
        registry.register_model("doubler", model_hash);
        executor.handler.set_nn_models(registry.clone());
        assert!(
            registry.cached_model_names().is_empty(),
            "nothing loaded yet"
        );

        let client = ComputeClient::new(requester.endpoint.clone());
        let mut req = request(wasm_hash, &{
            let mut input = Vec::new();
            input.extend_from_slice(&1.5f32.to_le_bytes());
            input.extend_from_slice(&(-2.25f32).to_le_bytes());
            input
        });
        req.class = TaskClass::Inference;

        // First task: the executor downloads the model blob from the
        // requester, loads a session and runs real inference.
        let done = client
            .execute_on(executor.addr.clone(), req.clone(), CALL_TIMEOUT)
            .await
            .expect("first inference");
        assert_eq!(f32s(&done.output), vec![3.0, -4.5], "y = x + x");
        assert_eq!(
            registry.cached_model_names(),
            vec!["doubler"],
            "session cached after the first run"
        );

        // Second task: served from the cached session (and the blob is local
        // now — no dependency on the requester's copy).
        req.task_id = Uuid::new_v4();
        let done = client
            .execute_on(executor.addr.clone(), req, CALL_TIMEOUT)
            .await
            .expect("second inference");
        assert_eq!(f32s(&done.output), vec![3.0, -4.5]);
    }

    #[tokio::test]
    async fn inference_without_a_catalog_still_runs_pure_wasm() {
        // The Inference class is a label, not a wasi-nn obligation (RFC 0002
        // §6.1: small models can run entirely inside WASM). An executor that
        // accepts the class but has no catalog runs the task in the pure
        // sandbox — and a task that *does* import wasi-nn is then refused.
        let (requester, executor) = spawn_pair(inference_policy()).await;
        let client = ComputeClient::new(requester.endpoint.clone());

        // Pure-wasm task under the Inference label: runs fine.
        let echo = wat::parse_str(ECHO_WAT).expect("valid wat");
        let echo_hash = requester
            .blob_store
            .add_document(echo.into())
            .await
            .expect("publish echo");
        let mut req = request(echo_hash, b"modelo pequeno em wasm puro");
        req.class = TaskClass::Inference;
        let done = client
            .execute_on(executor.addr.clone(), req, CALL_TIMEOUT)
            .await
            .expect("pure-wasm inference");
        assert_eq!(done.output, b"modelo pequeno em wasm puro");

        // wasi-nn-importing task without any catalog/grant: capability denied.
        let nn_wasm = wat::parse_str(NN_WAT).expect("valid wat");
        let nn_hash = requester
            .blob_store
            .add_document(nn_wasm.into())
            .await
            .expect("publish nn wasm");
        let mut req = request(nn_hash, b"");
        req.class = TaskClass::Inference;
        let err = client
            .execute_on(executor.addr.clone(), req, CALL_TIMEOUT)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                ComputeCallError::Task(TaskError::HostCapabilityDenied(_))
            ),
            "got: {err:?}"
        );
    }

    /// A task naming a `required_model` gets the NN grant attached even when
    /// its class is not `Inference` — admission already vetted the model via
    /// `serves_model`, so the wasi-nn imports must be linked or the task would
    /// fail at instantiation with HostCapabilityDenied.
    #[tokio::test]
    async fn required_model_attaches_nn_grant_regardless_of_class() {
        let (requester, executor) = spawn_pair(inference_policy()).await;

        let wasm = wat::parse_str(NN_WAT).expect("valid wat");
        let wasm_hash = requester
            .blob_store
            .add_document(wasm.into())
            .await
            .expect("publish wasm");
        let model_hash = requester
            .blob_store
            .add_document(doubler_onnx_model().into())
            .await
            .expect("publish model");
        let registry = Arc::new(NnModelRegistry::new());
        registry.register_model("doubler", model_hash);
        executor.handler.set_nn_models(registry);

        let mut input = Vec::new();
        input.extend_from_slice(&3.0f32.to_le_bytes());
        input.extend_from_slice(&(-1.0f32).to_le_bytes());
        // General class (not Inference) but names a required model.
        let mut req = request(wasm_hash, &input);
        req.class = TaskClass::General;
        req.required_model = Some("doubler".into());

        let client = ComputeClient::new(requester.endpoint.clone());
        let done = client
            .execute_on(executor.addr.clone(), req, CALL_TIMEOUT)
            .await
            .expect("general-class task with a required model must run");
        assert_eq!(f32s(&done.output), vec![6.0, -2.0]);
    }

    /// RFC 0003 phase NN-3: two executors advertise different models; the
    /// scheduler routes each task to the node that has its model (model
    /// affinity as a hard constraint), and an executor asked directly for a
    /// model it does not serve rejects cleanly at admission.
    #[tokio::test]
    async fn scheduler_routes_by_model_affinity() {
        let requester = spawn_node(ExecutorPolicy::default()).await;
        let node_a = spawn_node(inference_policy()).await;
        let node_b = spawn_node(inference_policy()).await;
        for executor in [&node_a, &node_b] {
            seed_peer_addr(&requester.endpoint, executor.addr.clone());
            seed_peer_addr(&executor.endpoint, requester.addr.clone());
        }

        // The requester publishes the task wasm and TWO models.
        let wasm = wat::parse_str(NN_WAT).expect("valid wat");
        let wasm_hash = requester
            .blob_store
            .add_document(wasm.into())
            .await
            .expect("publish wasm");
        let model = doubler_onnx_model();
        let model_hash = requester
            .blob_store
            .add_document(model.into())
            .await
            .expect("publish model");

        // A serves "doubler"; B serves "outro" (same bytes, different name —
        // what matters for routing is the advertised catalog).
        let registry_a = Arc::new(NnModelRegistry::new());
        registry_a.register_model("doubler", model_hash);
        node_a.handler.set_nn_models(registry_a);
        let registry_b = Arc::new(NnModelRegistry::new());
        registry_b.register_model("outro", model_hash);
        node_b.handler.set_nn_models(registry_b);

        // Capability vectors as the telemetry would advertise them — B is the
        // "stronger" machine, so without model affinity it would win.
        let directory = Arc::new(CapabilityDirectory::new());
        let vector = |node: &TestNode, cores: u16, models: Vec<String>| {
            guardian_db::compute::CapabilityVector {
                node_id: node.endpoint.id(),
                cpu_cores: cores,
                cpu_arch: guardian_db::compute::CpuArch::X86_64,
                ram_total_mb: 16_000,
                accelerators: vec![],
                cpu_load_pct: 10,
                ram_free_mb: 8_000,
                on_battery: false,
                battery_pct: None,
                tasks_running: 0,
                max_concurrent: 2,
                accepts: vec![TaskClass::General, TaskClass::Inference],
                nn_models: models,
                llm_models: vec![],
                embed_models: vec![],
                issued_at: 0,
            }
        };
        directory.upsert(vector(&node_a, 4, vec!["doubler".into()]));
        directory.upsert(vector(&node_b, 64, vec!["outro".into()]));

        let scheduler = ComputeScheduler::new(
            ComputeClient::new(requester.endpoint.clone()),
            directory,
            requester.endpoint.id(),
            SchedulerConfig::default(),
        );

        let mut input = Vec::new();
        input.extend_from_slice(&1.5f32.to_le_bytes());
        input.extend_from_slice(&(-2.25f32).to_le_bytes());
        let request_for = |model: &str| {
            let mut req = request(wasm_hash, &input);
            req.class = TaskClass::Inference;
            req.required_model = Some(model.to_string());
            req
        };

        // The task needing "doubler" lands on A even though B is stronger.
        let delegated = scheduler
            .execute(request_for("doubler"))
            .await
            .expect("doubler task");
        assert_eq!(delegated.executor, node_a.endpoint.id());
        assert_eq!(f32s(&delegated.completed.output), vec![3.0, -4.5]);

        // Asking A directly for a model it does not serve: clean admission
        // rejection, not a trap.
        let client = ComputeClient::new(requester.endpoint.clone());
        let err = client
            .execute_on(node_a.addr.clone(), request_for("outro"), CALL_TIMEOUT)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            ComputeCallError::Rejected(RejectReason::ModelNotAvailable("outro".into()))
        );
    }
}

/// RFC 0004 phase 4: streamed LLM generation over `COMPUTE_ALPN` between two
/// real iroh endpoints (§6.2), and the router's peer-delegation tier.
#[cfg(feature = "compute-llm")]
mod llm_streaming {
    use super::*;
    use async_trait::async_trait;
    use futures::StreamExt;
    use guardian_db::compute::llm::{
        ChatMessage, FinishReason, GenerateEvent, GenerateRequest, GenerateStream, LlmBackend,
        LlmError, Locality, ModelInfo, SamplingParams, Usage,
    };
    use guardian_db::compute::{
        LlmGenerateRequest, LlmRegistry, LlmRouter, PeerDelegation, RouterConfig,
    };

    /// Local backend on the executor: streams two deltas then End — or one
    /// delta then a crash, to exercise the truncation reporting rule.
    struct TokenBackend {
        crash_mid_stream: bool,
    }

    #[async_trait]
    impl LlmBackend for TokenBackend {
        async fn models(&self) -> Result<Vec<ModelInfo>, LlmError> {
            Ok(vec![ModelInfo {
                name: "test-model".into(),
                content_hash: None,
            }])
        }

        async fn generate(&self, _req: GenerateRequest) -> Result<GenerateStream, LlmError> {
            let events: Vec<Result<GenerateEvent, LlmError>> = if self.crash_mid_stream {
                vec![
                    Ok(GenerateEvent::Delta {
                        content: "Hel".into(),
                    }),
                    Err(LlmError::BackendCrashed("engine died".into())),
                ]
            } else {
                vec![
                    Ok(GenerateEvent::Delta {
                        content: "Hel".into(),
                    }),
                    Ok(GenerateEvent::Delta {
                        content: "lo".into(),
                    }),
                    Ok(GenerateEvent::End {
                        finish_reason: FinishReason::Stop,
                        usage: Usage {
                            prompt_tokens: 3,
                            completion_tokens: 2,
                        },
                    }),
                ]
            };
            Ok(futures::stream::iter(events).boxed())
        }

        fn locality(&self) -> Locality {
            Locality::Local
        }
    }

    fn generate_request(model: &str) -> GenerateRequest {
        GenerateRequest {
            model: model.into(),
            messages: vec![ChatMessage::user("hello")],
            params: SamplingParams::default(),
            deadline: Some(Duration::from_secs(30)),
        }
    }

    /// Registers a probed LLM registry on the node's handler.
    async fn serve_llm(node: &TestNode, crash_mid_stream: bool) -> Arc<LlmRegistry> {
        let registry = Arc::new(LlmRegistry::default());
        registry.register_backend("test", Arc::new(TokenBackend { crash_mid_stream }));
        registry.probe_once().await;
        node.handler.set_llm_registry(registry.clone());
        registry
    }

    #[tokio::test]
    async fn generation_streams_deltas_across_real_endpoints() {
        let executor = spawn_node(ExecutorPolicy::default()).await;
        let requester = spawn_node(ExecutorPolicy::default()).await;
        serve_llm(&executor, false).await;
        assert_eq!(executor.handler.llm_model_names(), vec!["test-model"]);

        let client = ComputeClient::new(requester.endpoint.clone());
        let stream = client
            .generate_on(
                executor.addr.clone(),
                LlmGenerateRequest {
                    task_id: Uuid::new_v4(),
                    request: generate_request("test-model"),
                },
                Duration::from_secs(10),
            )
            .await
            .expect("accepted");

        let events: Vec<GenerateEvent> = stream.map(|e| e.expect("stream event")).collect().await;
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
    }

    #[tokio::test]
    async fn unknown_model_is_rejected_at_admission() {
        let executor = spawn_node(ExecutorPolicy::default()).await;
        let requester = spawn_node(ExecutorPolicy::default()).await;
        serve_llm(&executor, false).await;

        let client = ComputeClient::new(requester.endpoint.clone());
        let result = client
            .generate_on(
                executor.addr.clone(),
                LlmGenerateRequest {
                    task_id: Uuid::new_v4(),
                    request: generate_request("ghost-model"),
                },
                Duration::from_secs(10),
            )
            .await;
        match result {
            Err(ComputeCallError::Rejected(RejectReason::ModelNotAvailable(_))) => {}
            Err(other) => panic!("expected ModelNotAvailable rejection, got {other:?}"),
            Ok(_) => panic!("expected rejection, got an accepted stream"),
        }
    }

    #[tokio::test]
    async fn mid_stream_backend_crash_reaches_the_requester_as_error() {
        let executor = spawn_node(ExecutorPolicy::default()).await;
        let requester = spawn_node(ExecutorPolicy::default()).await;
        serve_llm(&executor, true).await;

        let client = ComputeClient::new(requester.endpoint.clone());
        let stream = client
            .generate_on(
                executor.addr.clone(),
                LlmGenerateRequest {
                    task_id: Uuid::new_v4(),
                    request: generate_request("test-model"),
                },
                Duration::from_secs(10),
            )
            .await
            .expect("accepted");

        let events: Vec<Result<GenerateEvent, LlmError>> = stream.collect().await;
        // The delta that made it through arrives, then the truncation is
        // reported as an error — never silently retried (RFC 0004 §6.2).
        assert!(matches!(
            events.first(),
            Some(Ok(GenerateEvent::Delta { .. }))
        ));
        assert!(matches!(events.last(), Some(Err(_))));
    }

    /// Tier 2 end-to-end: the requester serves no local backend, discovers
    /// the executor through a capability vector advertising `llm_models`,
    /// and the router delegates the stream over the wire.
    #[tokio::test]
    async fn router_delegates_to_a_peer_advertising_the_model() {
        let executor = spawn_node(ExecutorPolicy::default()).await;
        let requester = spawn_node(ExecutorPolicy::default()).await;
        serve_llm(&executor, false).await;
        seed_peer_addr(&requester.endpoint, executor.addr.clone());

        let directory = Arc::new(CapabilityDirectory::new());
        directory.upsert(guardian_db::compute::CapabilityVector {
            node_id: executor.endpoint.id(),
            cpu_cores: 8,
            cpu_arch: guardian_db::compute::CpuArch::X86_64,
            ram_total_mb: 16_000,
            accelerators: vec![],
            cpu_load_pct: 10,
            ram_free_mb: 8_000,
            on_battery: false,
            battery_pct: None,
            tasks_running: 0,
            max_concurrent: 2,
            accepts: vec![TaskClass::Inference],
            nn_models: vec![],
            llm_models: vec!["test-model".into()],
            embed_models: vec![],
            issued_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        });

        // The requester's registry is empty: tiers 1 and 3 have nothing.
        let empty_registry = Arc::new(LlmRegistry::default());
        let router =
            LlmRouter::new(empty_registry, RouterConfig::default()).with_peers(PeerDelegation {
                client: ComputeClient::new(requester.endpoint.clone()),
                directory,
                local: requester.endpoint.id(),
            });

        let stream = router
            .route(generate_request("test-model"))
            .await
            .expect("routed to peer");
        let events: Vec<GenerateEvent> = stream.map(|e| e.expect("stream event")).collect().await;
        assert_eq!(events.len(), 3);
        assert!(matches!(
            events.last(),
            Some(GenerateEvent::End {
                finish_reason: FinishReason::Stop,
                ..
            })
        ));
    }
}
