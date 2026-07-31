//! # Capability telemetry (Phase 3)
//!
//! Samples this node's capacity (CPU, RAM, load) and gossips it to the
//! network as a [`CapabilityVector`], while collecting the vectors of other
//! nodes into a [`CapabilityDirectory`](super::scheduler::CapabilityDirectory)
//! for the scheduler to rank.
//!
//! Publication is **passive with hysteresis** (RFC §5.2): a vector is only
//! re-broadcast when a dynamic field crosses a threshold or a slow heartbeat
//! elapses, so capability traffic stays negligible.
//!
//! Vectors are *hints, not contracts*: they are unauthenticated gossip
//! payloads, and a wrong (or forged) vector costs at most one failed-over
//! delegation attempt — admission on the executor always has the final word.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use iroh::EndpointId as NodeId;
use iroh_gossip::api::GossipSender;
use iroh_gossip::net::Gossip;
use iroh_gossip::proto::TopicId;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use super::protocol::ComputeProtocolHandler;
use super::scheduler::CapabilityDirectory;
use super::{Accel, CapabilityVector, CpuArch};

/// Well-known gossip topic where compute-enabled nodes exchange capability
/// vectors. Hashed with blake3 into the [`TopicId`], the same convention the
/// backend's pubsub uses.
///
/// `/2`: the vector gained `nn_models` (phase NN-3) — postcard cannot decode
/// across that change, so the topic was bumped (same discipline as the ALPN).
/// `/3`: the vector gained `llm_models` (RFC 0004 phase 4), same rule.
/// `/4`: the vector gained `embed_models` (RFC 0005 §6.4 phase 2), same rule.
pub const CAPABILITY_TOPIC: &str = "guardian-db/compute/capabilities/4";

/// The gossip [`TopicId`] of [`CAPABILITY_TOPIC`].
pub fn capability_topic_id() -> TopicId {
    TopicId::from_bytes(blake3::hash(CAPABILITY_TOPIC.as_bytes()).into())
}

/// Tuning of the sampling/publication loop.
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// How often the local capacity is sampled.
    pub sample_interval: Duration,
    /// Maximum silence: re-publish even without changes after this long.
    pub heartbeat: Duration,
    /// Hysteresis: re-publish when CPU load moved at least this many points.
    pub cpu_delta_pct: u8,
    /// Hysteresis: re-publish when free RAM moved at least this fraction of
    /// total RAM (in percent points).
    pub ram_delta_pct: u8,
    /// Battery state to advertise. Automatic detection is platform-specific
    /// and deliberately out of scope; `None` advertises "not on battery"
    /// (the honest default for the desktops/servers that actually accept
    /// work — a node that *is* on battery should set `Some(true)`, which
    /// zeroes its advertised concurrency by default).
    pub on_battery: Option<bool>,
    /// Accelerators to advertise (Phase 5). Owner-declared: reliable
    /// automatic GPU/NPU detection would drag in a graphics stack, so the
    /// node owner states what the machine has; the scheduler rewards it
    /// only for `Inference`-class tasks.
    pub accelerators: Vec<Accel>,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            sample_interval: Duration::from_secs(20),
            heartbeat: Duration::from_secs(180),
            cpu_delta_pct: 15,
            ram_delta_pct: 10,
            on_battery: None,
            accelerators: Vec::new(),
        }
    }
}

/// Samples the local machine into [`CapabilityVector`]s.
///
/// Keeps the `sysinfo::System` alive between samples because CPU usage is a
/// delta between two refreshes — the first sample of a fresh sampler reads 0%.
pub struct TelemetrySampler {
    system: sysinfo::System,
}

impl TelemetrySampler {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            system: sysinfo::System::new(),
        }
    }

    /// Takes one sample, merging machine facts with the executor's live
    /// policy (accepted classes, concurrency, tasks currently running).
    pub fn sample(
        &mut self,
        node_id: NodeId,
        handler: &ComputeProtocolHandler,
        config: &TelemetryConfig,
    ) -> CapabilityVector {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();

        let policy = handler.policy();
        let on_battery = config.on_battery.unwrap_or(false);
        // A node on battery does not advertise capacity unless its owner set
        // a policy saying otherwise is fine — here we advertise 0 slots and
        // let the local admission policy stay untouched (RFC §5.2).
        let max_concurrent = if on_battery { 0 } else { policy.max_concurrent };

        CapabilityVector {
            node_id,
            cpu_cores: self.system.cpus().len().min(u16::MAX as usize) as u16,
            cpu_arch: local_cpu_arch(),
            ram_total_mb: (self.system.total_memory() / (1024 * 1024)).min(u32::MAX as u64) as u32,
            cpu_load_pct: (self.system.global_cpu_usage().round().clamp(0.0, 100.0)) as u8,
            ram_free_mb: (self.system.available_memory() / (1024 * 1024)).min(u32::MAX as u64)
                as u32,
            on_battery,
            battery_pct: None,
            tasks_running: handler.tasks_running().min(u8::MAX as u32) as u8,
            max_concurrent: max_concurrent.min(u8::MAX as u32) as u8,
            accepts: policy.accepts,
            nn_models: handler.nn_model_names(),
            llm_models: handler.llm_model_names(),
            embed_models: handler.embed_model_names(),
            accelerators: advertised_accelerators(&config.accelerators),
            issued_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }
}

/// The hysteresis rule: is `next` different enough from the last published
/// vector (or has the heartbeat elapsed) to justify a broadcast?
pub fn should_publish(
    previous: Option<&CapabilityVector>,
    next: &CapabilityVector,
    since_last_publish: Duration,
    config: &TelemetryConfig,
) -> bool {
    let Some(prev) = previous else {
        return true; // first sample: announce ourselves
    };
    if since_last_publish >= config.heartbeat {
        return true;
    }
    let cpu_moved = prev.cpu_load_pct.abs_diff(next.cpu_load_pct) >= config.cpu_delta_pct;
    let ram_threshold_mb = (u64::from(next.ram_total_mb) * u64::from(config.ram_delta_pct)) / 100;
    let ram_moved =
        u64::from(prev.ram_free_mb.abs_diff(next.ram_free_mb)) >= ram_threshold_mb.max(1);

    cpu_moved
        || ram_moved
        || prev.on_battery != next.on_battery
        || prev.tasks_running != next.tasks_running
        || prev.max_concurrent != next.max_concurrent
        || prev.accepts != next.accepts
        || prev.nn_models != next.nn_models
        // A backend tripping unhealthy (or recovering) must reach the
        // directory on the next gossip round (RFC 0004 §6.1).
        || prev.llm_models != next.llm_models
        // An embedding model becoming (un)available must propagate too.
        || prev.embed_models != next.embed_models
}

/// Background service that joins the capability topic, publishes this node's
/// vector (with hysteresis) and feeds received vectors into the directory.
///
/// Dropping it stops both loops.
pub struct CapabilityGossip {
    directory: Arc<CapabilityDirectory>,
    sender: Arc<tokio::sync::RwLock<GossipSender>>,
    publisher: JoinHandle<()>,
    receiver: JoinHandle<()>,
}

impl std::fmt::Debug for CapabilityGossip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapabilityGossip").finish_non_exhaustive()
    }
}

impl CapabilityGossip {
    /// Subscribes to [`CAPABILITY_TOPIC`] and spawns the publish/receive loops.
    ///
    /// `bootstrap` peers seed the gossip mesh; more can join later via
    /// [`CapabilityGossip::join_peers`] as connections are made.
    pub async fn spawn(
        gossip: Gossip,
        local: NodeId,
        handler: ComputeProtocolHandler,
        directory: Arc<CapabilityDirectory>,
        bootstrap: Vec<NodeId>,
        config: TelemetryConfig,
    ) -> Result<Self, String> {
        let topic = gossip
            .subscribe(capability_topic_id(), bootstrap)
            .await
            .map_err(|e| format!("capability topic subscribe: {e}"))?;
        let (sender, mut events) = topic.split();
        let sender = Arc::new(tokio::sync::RwLock::new(sender));

        // Receive loop: every valid vector goes into the directory.
        let receiver = {
            let directory = directory.clone();
            tokio::spawn(async move {
                while let Some(event) = events.next().await {
                    match event {
                        Ok(iroh_gossip::api::Event::Received(msg)) => {
                            match postcard::from_bytes::<CapabilityVector>(&msg.content) {
                                Ok(vector) if vector.node_id != local => {
                                    debug!(peer = %vector.node_id.fmt_short(),
                                           load = vector.cpu_load_pct,
                                           "compute: capability vector received");
                                    directory.upsert(vector);
                                }
                                Ok(_) => {} // our own echo, ignore
                                Err(e) => {
                                    debug!("compute: undecodable capability vector: {e}")
                                }
                            }
                        }
                        Ok(iroh_gossip::api::Event::NeighborDown(peer)) => {
                            // A vanished neighbor's vector is stale by definition.
                            directory.remove(&peer);
                        }
                        Ok(_) => {}
                        Err(e) => {
                            warn!("compute: capability gossip stream error: {e}");
                            break;
                        }
                    }
                }
            })
        };

        // Publish loop: sample on a timer, broadcast only when worth it.
        let publisher = {
            let sender = sender.clone();
            tokio::spawn(async move {
                let mut sampler = TelemetrySampler::new();
                let mut last_published: Option<CapabilityVector> = None;
                let mut last_publish_at = Instant::now();
                let mut ticker = tokio::time::interval(config.sample_interval);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    let vector = sampler.sample(local, &handler, &config);
                    if !should_publish(
                        last_published.as_ref(),
                        &vector,
                        last_publish_at.elapsed(),
                        &config,
                    ) {
                        continue;
                    }
                    let Ok(payload) = postcard::to_stdvec(&vector) else {
                        continue;
                    };
                    let result = sender.read().await.broadcast(payload.into()).await;
                    match result {
                        Ok(()) => {
                            debug!(
                                load = vector.cpu_load_pct,
                                free_mb = vector.ram_free_mb,
                                "compute: capability vector published"
                            );
                            last_published = Some(vector);
                            last_publish_at = Instant::now();
                        }
                        Err(e) => debug!("compute: capability broadcast failed: {e}"),
                    }
                }
            })
        };

        Ok(Self {
            directory,
            sender,
            publisher,
            receiver,
        })
    }

    /// Adds peers to the capability gossip mesh (e.g. right after connecting
    /// to them for replication).
    pub async fn join_peers(&self, peers: Vec<NodeId>) -> Result<(), String> {
        self.sender
            .read()
            .await
            .join_peers(peers)
            .await
            .map_err(|e| format!("capability join_peers: {e}"))
    }

    /// The directory this service feeds.
    pub fn directory(&self) -> Arc<CapabilityDirectory> {
        self.directory.clone()
    }
}

impl Drop for CapabilityGossip {
    fn drop(&mut self) {
        self.publisher.abort();
        self.receiver.abort();
    }
}

/// What the vector actually advertises for accelerators (phase NN-4).
///
/// Without `compute-nn-cuda` the owner's declaration is passed through
/// unchanged (declarative mode). With it, the GPU claim is **verified**: a
/// declared `Accel::Gpu` is dropped when CUDA is not actually available, and
/// a detected CUDA setup is advertised even if the owner forgot to declare it.
/// NPUs stay declarative — there is no portable detection for them.
fn advertised_accelerators(declared: &[Accel]) -> Vec<Accel> {
    #[cfg(feature = "compute-nn-cuda")]
    {
        verified_accelerators(declared, crate::compute::nn::cuda_available())
    }
    #[cfg(not(feature = "compute-nn-cuda"))]
    {
        declared.to_vec()
    }
}

/// The pure verification rule behind [`advertised_accelerators`]: the GPU
/// entry mirrors detection; everything else passes through.
#[cfg_attr(not(feature = "compute-nn-cuda"), allow(dead_code))]
fn verified_accelerators(declared: &[Accel], gpu_detected: bool) -> Vec<Accel> {
    let mut advertised: Vec<Accel> = declared
        .iter()
        .copied()
        .filter(|accel| *accel != Accel::Gpu)
        .collect();
    if gpu_detected {
        advertised.push(Accel::Gpu);
    }
    advertised
}

/// Maps the compile-time target architecture to the advertised [`CpuArch`].
fn local_cpu_arch() -> CpuArch {
    match std::env::consts::ARCH {
        "x86_64" => CpuArch::X86_64,
        "aarch64" => CpuArch::Aarch64,
        _ => CpuArch::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compute::TaskClass;

    fn test_node_id() -> NodeId {
        iroh::SecretKey::generate().public()
    }

    fn vector(cpu_load: u8, ram_free: u32, tasks: u8) -> CapabilityVector {
        CapabilityVector {
            node_id: test_node_id(),
            cpu_cores: 8,
            cpu_arch: CpuArch::X86_64,
            ram_total_mb: 16_000,
            accelerators: vec![],
            cpu_load_pct: cpu_load,
            ram_free_mb: ram_free,
            on_battery: false,
            battery_pct: None,
            tasks_running: tasks,
            max_concurrent: 4,
            accepts: vec![TaskClass::General],
            nn_models: vec![],
            llm_models: vec![],
            embed_models: vec![],
            issued_at: 0,
        }
    }

    #[test]
    fn gpu_claim_mirrors_detection() {
        // Declared GPU without real CUDA: dropped. Detected CUDA without a
        // declaration: advertised anyway. NPU passes through untouched.
        assert_eq!(
            verified_accelerators(&[Accel::Gpu, Accel::Npu], false),
            vec![Accel::Npu]
        );
        assert_eq!(
            verified_accelerators(&[Accel::Npu], true),
            vec![Accel::Npu, Accel::Gpu]
        );
        assert_eq!(verified_accelerators(&[], false), vec![]);
    }

    #[test]
    fn model_catalog_change_publishes() {
        let cfg = TelemetryConfig::default();
        let prev = vector(30, 8_000, 0);
        let mut next = vector(30, 8_000, 0);
        next.nn_models = vec!["whisper-tiny".into()];
        assert!(should_publish(
            Some(&prev),
            &next,
            Duration::from_secs(5),
            &cfg
        ));
    }

    #[test]
    fn first_sample_is_always_published() {
        let cfg = TelemetryConfig::default();
        assert!(should_publish(
            None,
            &vector(10, 8_000, 0),
            Duration::ZERO,
            &cfg
        ));
    }

    #[test]
    fn small_wobble_is_suppressed() {
        let cfg = TelemetryConfig::default();
        let prev = vector(30, 8_000, 0);
        // 10 points of CPU and ~3% of RAM: below both thresholds.
        let next = vector(40, 8_500, 0);
        assert!(!should_publish(
            Some(&prev),
            &next,
            Duration::from_secs(5),
            &cfg
        ));
    }

    #[test]
    fn threshold_crossings_publish() {
        let cfg = TelemetryConfig::default();
        let prev = vector(30, 8_000, 0);

        // CPU moved 20 points.
        assert!(should_publish(
            Some(&prev),
            &vector(50, 8_000, 0),
            Duration::from_secs(5),
            &cfg
        ));
        // RAM moved > 10% of total (1.6 GB of 16 GB).
        assert!(should_publish(
            Some(&prev),
            &vector(30, 6_000, 0),
            Duration::from_secs(5),
            &cfg
        ));
        // A task started/finished.
        assert!(should_publish(
            Some(&prev),
            &vector(30, 8_000, 1),
            Duration::from_secs(5),
            &cfg
        ));
    }

    #[test]
    fn heartbeat_publishes_even_without_changes() {
        let cfg = TelemetryConfig::default();
        let prev = vector(30, 8_000, 0);
        let same = vector(30, 8_000, 0);
        assert!(should_publish(Some(&prev), &same, cfg.heartbeat, &cfg));
    }

    #[test]
    fn sampler_reports_real_machine_facts() {
        let handler = ComputeProtocolHandler::new(
            Arc::new(NoFetch),
            crate::compute::ExecutorPolicy::default(),
        )
        .expect("handler");
        let cfg = TelemetryConfig::default();
        let mut sampler = TelemetrySampler::new();
        let v = sampler.sample(test_node_id(), &handler, &cfg);

        assert!(v.cpu_cores > 0);
        assert!(v.ram_total_mb > 0);
        assert!(v.cpu_load_pct <= 100);
        assert_eq!(v.tasks_running, 0);
        assert_eq!(v.max_concurrent, 2); // default policy
    }

    #[test]
    fn battery_advertises_zero_capacity() {
        let handler = ComputeProtocolHandler::new(
            Arc::new(NoFetch),
            crate::compute::ExecutorPolicy::default(),
        )
        .expect("handler");
        let cfg = TelemetryConfig {
            on_battery: Some(true),
            ..TelemetryConfig::default()
        };
        let mut sampler = TelemetrySampler::new();
        let v = sampler.sample(test_node_id(), &handler, &cfg);
        assert!(v.on_battery);
        assert_eq!(v.max_concurrent, 0, "battery nodes advertise no capacity");
    }

    struct NoFetch;

    #[async_trait::async_trait]
    impl crate::compute::WasmFetcher for NoFetch {
        async fn fetch_wasm(
            &self,
            _hash: &iroh_blobs::Hash,
            _p: NodeId,
        ) -> Result<Vec<u8>, String> {
            Err("test fetcher".into())
        }
    }
}
