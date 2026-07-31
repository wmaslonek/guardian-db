//! # Capability-aware scheduler (Phase 3, stage 1: local scoring)
//!
//! The requester-side half of the RFC's "intelligent orchestrator": ranks the
//! [`CapabilityVector`]s collected by the telemetry gossip and delegates a
//! task to the best node, failing over down the ranking when a candidate
//! rejects, times out or is unreachable (RFC §5.5).
//!
//! Scoring is deliberately local — no extra network round trip. The auction
//! (`CallForProposals`) is a Phase 5 refinement for tasks where a wrong pick
//! is expensive.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::EndpointId as NodeId;
use tracing::{debug, warn};

use super::protocol::{CompletedTask, ComputeCallError, ComputeClient, ExecuteRequest};
use super::{CapabilityVector, TaskClass};

/// Live table of the capability vectors this node has heard over gossip,
/// keyed by the advertising peer. Entries age by *local receive time* (never
/// by the sender's clock, which may be skewed).
#[derive(Debug, Default)]
pub struct CapabilityDirectory {
    inner: parking_lot::RwLock<HashMap<NodeId, SeenVector>>,
}

#[derive(Debug, Clone)]
struct SeenVector {
    vector: CapabilityVector,
    received_at: Instant,
}

impl CapabilityDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or refreshes a peer's vector.
    pub fn upsert(&self, vector: CapabilityVector) {
        self.inner.write().insert(
            vector.node_id,
            SeenVector {
                vector,
                received_at: Instant::now(),
            },
        );
    }

    /// Forgets a peer (e.g. its gossip neighbor went down, or it proved
    /// unreachable during a delegation attempt).
    pub fn remove(&self, node: &NodeId) {
        self.inner.write().remove(node);
    }

    /// The vector of one peer, if known and not older than `max_age`.
    pub fn get(&self, node: &NodeId, max_age: Duration) -> Option<CapabilityVector> {
        self.inner
            .read()
            .get(node)
            .filter(|seen| seen.received_at.elapsed() <= max_age)
            .map(|seen| seen.vector.clone())
    }

    /// All vectors not older than `max_age`.
    pub fn snapshot(&self, max_age: Duration) -> Vec<CapabilityVector> {
        self.inner
            .read()
            .values()
            .filter(|seen| seen.received_at.elapsed() <= max_age)
            .map(|seen| seen.vector.clone())
            .collect()
    }

    /// Number of peers currently known (regardless of staleness).
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }
}

/// Weights of the scoring formula (RFC §5.5). Defaults favor idle CPU first,
/// then free memory, with a battery penalty that in practice disqualifies
/// laptops running on battery even when they slipped past the advertising
/// side's own zeroing.
#[derive(Debug, Clone)]
pub struct ScoreWeights {
    /// Points per estimated free core.
    pub free_cores: f64,
    /// Points per GiB of free RAM.
    pub ram_free_gib: f64,
    /// Penalty per point of CPU load (0-100).
    pub load_penalty: f64,
    /// Flat penalty applied to nodes on battery.
    pub battery_penalty: f64,
    /// Bonus for advertised accelerators, applied only to
    /// [`TaskClass::Inference`] tasks (Phase 5).
    pub accelerator_bonus: f64,
    /// Penalty scale for bad reputation: a node at reputation 0 loses this
    /// many points; at 1.0 (default) it loses none (Phase 5).
    pub reputation_penalty: f64,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        Self {
            free_cores: 10.0,
            ram_free_gib: 1.0,
            load_penalty: 0.2,
            battery_penalty: 1_000.0,
            accelerator_bonus: 50.0,
            reputation_penalty: 500.0,
        }
    }
}

/// Reputation earned through redundant execution (RFC §6.5, Phase 5): nodes
/// whose results diverge from the majority lose standing; agreeing nodes
/// slowly recover. Consulted by [`ComputeScheduler::rank`] as a score
/// penalty — a known liar is de-prioritized, never hard-banned (its vector
/// may still be the only candidate).
#[derive(Debug, Default)]
pub struct ReputationBook {
    scores: parking_lot::RwLock<HashMap<NodeId, f64>>,
}

impl ReputationBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Standing in `[0, 1]`; unknown nodes start at 1.0.
    pub fn factor(&self, node: &NodeId) -> f64 {
        self.scores.read().get(node).copied().unwrap_or(1.0)
    }

    /// Halves the node's standing (divergent result in a k-of-n round).
    pub fn penalize(&self, node: &NodeId) {
        let mut scores = self.scores.write();
        let entry = scores.entry(*node).or_insert(1.0);
        *entry = (*entry * 0.5).max(0.05);
    }

    /// Small recovery for a node that agreed with the majority.
    pub fn reward(&self, node: &NodeId) {
        let mut scores = self.scores.write();
        let entry = scores.entry(*node).or_insert(1.0);
        *entry = (*entry + 0.05).min(1.0);
    }
}

/// Scheduler tuning.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Vectors older than this (by local receive time) are not candidates.
    pub max_vector_age: Duration,
    /// How many candidates to try before giving up (ranking order).
    pub max_attempts: usize,
    /// Time budget for each individual delegation attempt.
    pub attempt_timeout: Duration,
    /// How many top candidates an auction probes for fresh bids (Phase 5).
    pub auction_probe_count: usize,
    /// Time budget for each auction probe (Phase 5).
    pub probe_timeout: Duration,
    pub weights: ScoreWeights,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_vector_age: Duration::from_secs(300),
            max_attempts: 3,
            attempt_timeout: Duration::from_secs(60),
            auction_probe_count: 3,
            probe_timeout: Duration::from_secs(5),
            weights: ScoreWeights::default(),
        }
    }
}

/// Readiness score of one candidate for one task class; `None` when the node
/// is not a candidate at all (class refused, no slots, on-battery default).
pub fn score(vector: &CapabilityVector, class: TaskClass, weights: &ScoreWeights) -> Option<f64> {
    if !vector.is_candidate_for(class) {
        return None;
    }
    let load = f64::from(vector.cpu_load_pct.min(100));
    let free_cores = f64::from(vector.cpu_cores) * (100.0 - load) / 100.0;
    let ram_gib = f64::from(vector.ram_free_mb) / 1024.0;
    let battery = if vector.on_battery {
        weights.battery_penalty
    } else {
        0.0
    };
    // Accelerators only matter for inference workloads (Phase 5).
    let accel = if class == TaskClass::Inference && !vector.accelerators.is_empty() {
        weights.accelerator_bonus
    } else {
        0.0
    };
    Some(
        weights.free_cores * free_cores + weights.ram_free_gib * ram_gib + accel
            - weights.load_penalty * load
            - battery,
    )
}

/// A delegation that succeeded somewhere on the network.
#[derive(Debug, Clone)]
pub struct Delegated {
    /// The node that actually ran the task.
    pub executor: NodeId,
    pub completed: CompletedTask,
}

/// Why the scheduler could not get the task executed.
#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    /// No fresh capability vector accepts this task class. The caller may
    /// fall back to running locally.
    #[error("no candidate node advertises capacity for this task class")]
    NoCandidates,
    /// The task itself failed on an executor (sandbox verdict, bad wasm…).
    /// Deterministic failures are not retried elsewhere — the same task would
    /// fail the same way.
    #[error("task failed on {executor}: {error}")]
    TaskFailed {
        executor: NodeId,
        error: super::runtime::TaskError,
    },
    /// Every attempted candidate was unavailable (rejected/unreachable/timeout).
    #[error("all {} delegation attempts failed", attempts.len())]
    AllAttemptsFailed {
        attempts: Vec<(NodeId, ComputeCallError)>,
    },
    /// Redundant execution produced no majority result (Phase 5): either the
    /// task is not deterministic, or too many executors lied/failed.
    #[error("redundant execution diverged with no majority")]
    Divergent,
}

/// Outcome of a k-of-n redundant execution (RFC §6.5, Phase 5).
#[derive(Debug, Clone)]
pub struct RedundantOutcome {
    /// The majority result (executor = first agreeing node).
    pub delegated: Delegated,
    /// How many executors agreed on the winning output.
    pub agreements: usize,
    /// How many executors returned any result at all.
    pub responded: usize,
    /// Nodes whose output diverged from the majority (reputation-penalized).
    pub divergent: Vec<NodeId>,
}

/// The capability-aware orchestrator: `execute` with no explicit destination.
#[derive(Debug, Clone)]
pub struct ComputeScheduler {
    client: ComputeClient,
    directory: Arc<CapabilityDirectory>,
    reputation: Arc<ReputationBook>,
    local: NodeId,
    config: SchedulerConfig,
}

impl ComputeScheduler {
    pub fn new(
        client: ComputeClient,
        directory: Arc<CapabilityDirectory>,
        local: NodeId,
        config: SchedulerConfig,
    ) -> Self {
        Self::with_reputation(
            client,
            directory,
            Arc::new(ReputationBook::new()),
            local,
            config,
        )
    }

    /// Like [`ComputeScheduler::new`] but sharing an existing
    /// [`ReputationBook`]. The wired backend uses this so reputation earned in
    /// redundant rounds survives across the ephemeral schedulers handed out by
    /// `compute_scheduler()` — otherwise a penalized node's standing resets on
    /// every call and RFC §6.5 reputation never accumulates.
    pub fn with_reputation(
        client: ComputeClient,
        directory: Arc<CapabilityDirectory>,
        reputation: Arc<ReputationBook>,
        local: NodeId,
        config: SchedulerConfig,
    ) -> Self {
        Self {
            client,
            directory,
            reputation,
            local,
            config,
        }
    }

    /// The reputation book this scheduler consults (fed by redundant runs).
    pub fn reputation(&self) -> Arc<ReputationBook> {
        self.reputation.clone()
    }

    /// Candidates for `class`, best first, from the fresh vectors known now.
    /// The local node is never a candidate — this is a *delegation* ranking.
    /// Reputation earned in redundant rounds discounts the score.
    pub fn rank(&self, class: TaskClass) -> Vec<(NodeId, f64)> {
        self.rank_for(class, None)
    }

    /// Like [`ComputeScheduler::rank`], additionally constrained to nodes
    /// that advertise `required_model` (phase NN-3): model affinity is the
    /// data-gravity rule of RFC 0002 §5.5 applied to NN models — the task
    /// goes to where the model already is.
    pub fn rank_for(&self, class: TaskClass, required_model: Option<&str>) -> Vec<(NodeId, f64)> {
        let mut ranked: Vec<(NodeId, f64)> = self
            .directory
            .snapshot(self.config.max_vector_age)
            .into_iter()
            .filter(|v| v.node_id != self.local)
            .filter(|v| required_model.is_none_or(|model| v.offers_model(model)))
            .filter_map(|v| {
                let base = score(&v, class, &self.config.weights)?;
                let standing = self.reputation.factor(&v.node_id);
                Some((
                    v.node_id,
                    base - (1.0 - standing) * self.config.weights.reputation_penalty,
                ))
            })
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked
    }

    /// Runs `request` on the best available node, failing over down the
    /// ranking (up to `max_attempts`) when a candidate rejects the task, is
    /// unreachable or times out. Task-level failures are final (§5.5).
    ///
    /// Dialing is by bare node id: the endpoint resolves the address from its
    /// active paths or configured lookup services.
    pub async fn execute(&self, request: ExecuteRequest) -> Result<Delegated, ScheduleError> {
        let order: Vec<NodeId> = self
            .rank_for(request.class, request.required_model.as_deref())
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        self.attempt_in_order(request, order).await
    }

    /// Contract-Net auction (Phase 5): probes the top-ranked candidates for a
    /// *fresh* bid before committing, so an expensive task is not sent on the
    /// word of a possibly stale gossip vector. Falls back to the gossip
    /// ranking when nobody answers the probes.
    pub async fn execute_with_auction(
        &self,
        request: ExecuteRequest,
    ) -> Result<Delegated, ScheduleError> {
        let ranked = self.rank_for(request.class, request.required_model.as_deref());
        if ranked.is_empty() {
            return Err(ScheduleError::NoCandidates);
        }
        let probe_count = self.config.auction_probe_count.clamp(1, ranked.len());
        let class = request.class;

        let bids =
            futures::future::join_all(ranked[..probe_count].iter().map(|(node, _)| async move {
                let reply = self
                    .client
                    .probe(*node, class, self.config.probe_timeout)
                    .await;
                (*node, reply)
            }))
            .await;

        // Prior gossip scores, used as a fallback so a node that bids
        // positively is never dropped just because its gossip vector aged out
        // (or scored `None`) during the probe window — the auction's whole
        // point is to trust the fresh bid over stale/absent gossip.
        let ranked_scores: std::collections::HashMap<NodeId, f64> =
            ranked.iter().copied().collect();
        let mut fresh: Vec<(NodeId, f64)> = bids
            .into_iter()
            .filter_map(|(node, reply)| match reply {
                Ok(bid) if bid.accepts_class => {
                    // Prefer re-scoring the fresh dynamic fields patched into
                    // the last gossiped vector; fall back to the prior gossip
                    // score when the vector is gone or no longer a candidate.
                    let base = self
                        .directory
                        .get(&node, self.config.max_vector_age)
                        .and_then(|mut vector| {
                            vector.cpu_load_pct = bid.cpu_load_pct;
                            vector.ram_free_mb = bid.ram_free_mb;
                            vector.tasks_running = vector
                                .max_concurrent
                                .saturating_sub(bid.free_slots.min(u8::MAX as u32) as u8);
                            score(&vector, class, &self.config.weights)
                        })
                        .or_else(|| ranked_scores.get(&node).copied())?;
                    let standing = self.reputation.factor(&node);
                    Some((
                        node,
                        base - (1.0 - standing) * self.config.weights.reputation_penalty,
                    ))
                }
                Ok(_) => None, // valid bid, but declines this class right now
                Err(err) => {
                    debug!(executor = %node.fmt_short(), error = %err,
                           "compute auction: probe failed");
                    None
                }
            })
            .collect();
        fresh.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let order: Vec<NodeId> = if fresh.is_empty() {
            // No bids: degrade gracefully to the gossip ranking.
            ranked.into_iter().map(|(n, _)| n).collect()
        } else {
            fresh.into_iter().map(|(n, _)| n).collect()
        };
        self.attempt_in_order(request, order).await
    }

    /// MapReduce's map half (RFC §6.3, Phase 5): fans `requests` out across
    /// the candidates in parallel, rotating the ranking per task so the load
    /// spreads instead of piling on the top node (its `Busy` rejections plus
    /// failover would sort it out anyway, but rotation avoids the churn).
    /// The reduce half belongs to the caller. Results keep request order.
    pub async fn map(
        &self,
        requests: Vec<ExecuteRequest>,
    ) -> Vec<Result<Delegated, ScheduleError>> {
        futures::future::join_all(requests.into_iter().enumerate().map(|(i, request)| {
            let mut order: Vec<NodeId> = self
                .rank_for(request.class, request.required_model.as_deref())
                .into_iter()
                .map(|(n, _)| n)
                .collect();
            if !order.is_empty() {
                let shift = i % order.len();
                order.rotate_left(shift);
            }
            self.attempt_in_order(request, order)
        }))
        .await
    }

    /// Redundant k-of-n execution (RFC §6.5, Phase 5): runs the same task on
    /// up to `k` distinct nodes and returns the majority result, penalizing
    /// the reputation of any node whose output diverges.
    ///
    /// **The task must be deterministic** — same input, same output on every
    /// honest node. The default sandbox guarantees this (no clock, no
    /// randomness, no host functions); an executor granting host functions
    /// breaks the property, which is why grants and redundancy don't mix.
    pub async fn execute_redundant(
        &self,
        request: ExecuteRequest,
        k: usize,
    ) -> Result<RedundantOutcome, ScheduleError> {
        let candidates: Vec<NodeId> = self
            .rank_for(request.class, request.required_model.as_deref())
            .into_iter()
            .map(|(n, _)| n)
            .take(k.max(1))
            .collect();
        if candidates.is_empty() {
            return Err(ScheduleError::NoCandidates);
        }

        let runs = futures::future::join_all(candidates.iter().map(|node| {
            let request = request.clone();
            async move {
                (
                    *node,
                    self.client
                        .execute_on(*node, request, self.config.attempt_timeout)
                        .await,
                )
            }
        }))
        .await;

        let mut attempts: Vec<(NodeId, ComputeCallError)> = Vec::new();
        let mut successes: Vec<(NodeId, super::protocol::CompletedTask)> = Vec::new();
        for (node, result) in runs {
            match result {
                Ok(completed) => successes.push((node, completed)),
                Err(err) => {
                    if matches!(
                        err,
                        ComputeCallError::Unreachable(_) | ComputeCallError::Timeout
                    ) {
                        self.directory.remove(&node);
                    }
                    attempts.push((node, err));
                }
            }
        }
        if successes.is_empty() {
            return Err(ScheduleError::AllAttemptsFailed { attempts });
        }

        let Some((winners, divergent)) = majority_by_output(&successes) else {
            return Err(ScheduleError::Divergent);
        };
        for node in &winners {
            self.reputation.reward(node);
        }
        for node in &divergent {
            warn!(executor = %node.fmt_short(),
                  "compute redundant: divergent result, reputation penalized");
            self.reputation.penalize(node);
        }

        let winner = winners[0];
        let completed = successes
            .iter()
            .find(|(node, _)| *node == winner)
            .map(|(_, completed)| completed.clone())
            .expect("winner comes from successes");
        Ok(RedundantOutcome {
            delegated: Delegated {
                executor: winner,
                completed,
            },
            agreements: winners.len(),
            responded: successes.len(),
            divergent,
        })
    }

    /// Tries the candidates in the given order (shared by every execution
    /// mode). Task-level failures are final; availability failures fail over.
    async fn attempt_in_order(
        &self,
        request: ExecuteRequest,
        order: Vec<NodeId>,
    ) -> Result<Delegated, ScheduleError> {
        if order.is_empty() {
            return Err(ScheduleError::NoCandidates);
        }
        let mut attempts: Vec<(NodeId, ComputeCallError)> = Vec::new();
        for node in order.into_iter().take(self.config.max_attempts.max(1)) {
            debug!(task = %request.task_id, executor = %node.fmt_short(),
                   "compute: delegating to ranked candidate");
            match self
                .client
                .execute_on(node, request.clone(), self.config.attempt_timeout)
                .await
            {
                Ok(completed) => {
                    return Ok(Delegated {
                        executor: node,
                        completed,
                    });
                }
                // A deterministic task failure would recur identically on any
                // node — final. A node-specific one (blob unavailable here,
                // capability not granted here, executor infra hiccup) may well
                // succeed elsewhere, so fail over.
                Err(ComputeCallError::Task(error)) if !error.is_transient() => {
                    return Err(ScheduleError::TaskFailed {
                        executor: node,
                        error,
                    });
                }
                Err(err) => {
                    warn!(task = %request.task_id, executor = %node.fmt_short(),
                          error = %err, "compute: attempt failed, trying next candidate");
                    match &err {
                        // A vanished node's vector is a lie; drop it so the
                        // next scheduling round doesn't rank it again.
                        ComputeCallError::Unreachable(_) | ComputeCallError::Timeout => {
                            self.directory.remove(&node);
                        }
                        _ => {}
                    }
                    attempts.push((node, err));
                }
            }
        }
        Err(ScheduleError::AllAttemptsFailed { attempts })
    }
}

/// Groups successful results by output hash and returns
/// `(majority nodes, divergent nodes)`, or `None` when two groups tie for
/// the lead (no majority to trust).
fn majority_by_output(
    successes: &[(NodeId, super::protocol::CompletedTask)],
) -> Option<(Vec<NodeId>, Vec<NodeId>)> {
    let mut groups: HashMap<[u8; 32], Vec<NodeId>> = HashMap::new();
    for (node, completed) in successes {
        groups
            .entry(*blake3::hash(&completed.output).as_bytes())
            .or_default()
            .push(*node);
    }
    let best_size = groups.values().map(Vec::len).max()?;
    let leaders: Vec<_> = groups.values().filter(|g| g.len() == best_size).collect();
    if leaders.len() != 1 {
        return None; // tie: no trustworthy majority
    }
    let winners = leaders[0].clone();
    let divergent = groups
        .values()
        .filter(|g| g.len() != best_size)
        .flatten()
        .copied()
        .collect();
    Some((winners, divergent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compute::{CpuArch, ResourceLimits};

    fn node(seed: u8) -> NodeId {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        for (i, b) in bytes.iter_mut().enumerate().skip(1) {
            *b = seed.wrapping_add(i as u8);
        }
        iroh::SecretKey::from_bytes(&bytes).public()
    }

    fn vector(id: NodeId, cores: u16, load: u8, ram_free: u32) -> CapabilityVector {
        CapabilityVector {
            node_id: id,
            cpu_cores: cores,
            cpu_arch: CpuArch::X86_64,
            ram_total_mb: ram_free * 2,
            accelerators: vec![],
            cpu_load_pct: load,
            ram_free_mb: ram_free,
            on_battery: false,
            battery_pct: None,
            tasks_running: 0,
            max_concurrent: 4,
            accepts: vec![TaskClass::General, TaskClass::Media],
            nn_models: vec![],
            llm_models: vec![],
            embed_models: vec![],
            issued_at: 0,
        }
    }

    fn scheduler_with(directory: Arc<CapabilityDirectory>, local: NodeId) -> ComputeScheduler {
        // The endpoint is never dialed in these tests; rank() is pure.
        let endpoint = futures::executor::block_on(async {
            iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .bind()
                .await
        });
        // rank() does not touch the endpoint, but constructing the scheduler
        // requires a client; tests that cannot bind a socket would need a
        // refactor — bind() on localhost is available in the test env.
        let endpoint = endpoint.expect("bind test endpoint");
        ComputeScheduler::new(
            ComputeClient::new(endpoint),
            directory,
            local,
            SchedulerConfig::default(),
        )
    }

    #[test]
    fn stronger_idle_node_outscores_busy_small_one() {
        let w = ScoreWeights::default();
        let strong = vector(node(1), 16, 10, 32_000);
        let weak = vector(node(2), 4, 80, 4_000);
        assert!(score(&strong, TaskClass::General, &w) > score(&weak, TaskClass::General, &w));
    }

    #[test]
    fn battery_penalty_disqualifies_in_practice() {
        let w = ScoreWeights::default();
        let mut on_battery = vector(node(1), 16, 0, 32_000);
        on_battery.on_battery = true;
        let modest = vector(node(2), 2, 50, 2_000);
        // The battery node still scores (its owner opted in with slots), but
        // far below any plugged-in machine.
        assert!(
            score(&on_battery, TaskClass::General, &w) < score(&modest, TaskClass::General, &w)
        );
    }

    #[test]
    fn accelerator_bonus_applies_only_to_inference() {
        let w = ScoreWeights::default();
        let mut plain = vector(node(1), 8, 10, 8_000);
        plain.accepts = vec![TaskClass::General, TaskClass::Inference];
        let mut gpu = plain.clone();
        gpu.node_id = node(2);
        gpu.accelerators = vec![crate::compute::Accel::Gpu];

        // Same machine otherwise: the GPU only helps for Inference.
        let bonus = score(&gpu, TaskClass::Inference, &w).unwrap()
            - score(&plain, TaskClass::Inference, &w).unwrap();
        assert!((bonus - w.accelerator_bonus).abs() < 1e-9);
        let general_delta = score(&gpu, TaskClass::General, &w).unwrap()
            - score(&plain, TaskClass::General, &w).unwrap();
        assert!(general_delta.abs() < 1e-9);
    }

    #[tokio::test]
    async fn bad_reputation_demotes_a_stronger_node() {
        let local = node(9);
        let directory = Arc::new(CapabilityDirectory::new());
        directory.upsert(vector(node(1), 16, 10, 32_000)); // strong
        directory.upsert(vector(node(2), 4, 50, 4_000)); // weak
        let scheduler = scheduler_with(directory, local);

        assert_eq!(scheduler.rank(TaskClass::General)[0].0, node(1));
        // One divergence halves the strong node's standing → big penalty.
        scheduler.reputation().penalize(&node(1));
        assert_eq!(
            scheduler.rank(TaskClass::General)[0].0,
            node(2),
            "the known liar must rank below the honest weak node"
        );
    }

    #[tokio::test]
    async fn required_model_constrains_the_ranking() {
        let local = node(9);
        let directory = Arc::new(CapabilityDirectory::new());
        // The stronger node lacks the model; the weaker one serves it.
        let mut strong = vector(node(1), 16, 10, 32_000);
        strong.accepts.push(TaskClass::Inference);
        let mut weak_with_model = vector(node(2), 4, 50, 4_000);
        weak_with_model.accepts.push(TaskClass::Inference);
        weak_with_model.nn_models = vec!["whisper-tiny".into()];
        directory.upsert(strong);
        directory.upsert(weak_with_model);

        let scheduler = scheduler_with(directory, local);
        // Without a model requirement, raw capacity wins.
        assert_eq!(scheduler.rank(TaskClass::Inference)[0].0, node(1));
        // With one, only the node that has the model is a candidate.
        let ranked = scheduler.rank_for(TaskClass::Inference, Some("whisper-tiny"));
        assert_eq!(
            ranked.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            vec![node(2)],
            "model affinity is a hard constraint"
        );
        assert!(
            scheduler
                .rank_for(TaskClass::Inference, Some("nao-existe"))
                .is_empty()
        );
    }

    #[test]
    fn majority_grouping_picks_winner_and_flags_divergents() {
        let completed = |bytes: &[u8]| crate::compute::CompletedTask {
            output: bytes.to_vec(),
            metrics: crate::compute::ExecMetrics {
                fuel_consumed: 1,
                duration_ms: 1,
                peak_memory_bytes: 0,
            },
        };
        // Two agree, one lies.
        let successes = vec![
            (node(1), completed(b"correct")),
            (node(2), completed(b"correct")),
            (node(3), completed(b"forged")),
        ];
        let (winners, divergent) = majority_by_output(&successes).expect("majority");
        assert_eq!(winners.len(), 2);
        assert_eq!(divergent, vec![node(3)]);

        // 1 vs 1: a tie is no majority.
        let tied = vec![(node(1), completed(b"a")), (node(2), completed(b"b"))];
        assert!(majority_by_output(&tied).is_none());

        // Single response: trivially the majority.
        let single = vec![(node(1), completed(b"only"))];
        let (winners, divergent) = majority_by_output(&single).expect("majority");
        assert_eq!(winners, vec![node(1)]);
        assert!(divergent.is_empty());
    }

    #[test]
    fn non_candidates_score_none() {
        let w = ScoreWeights::default();
        let mut v = vector(node(1), 8, 10, 8_000);
        assert!(
            score(&v, TaskClass::Inference, &w).is_none(),
            "class not accepted"
        );
        v.tasks_running = v.max_concurrent;
        assert!(score(&v, TaskClass::General, &w).is_none(), "no free slot");
    }

    #[tokio::test]
    async fn rank_orders_by_score_and_excludes_local_and_stale() {
        let local = node(9);
        let directory = Arc::new(CapabilityDirectory::new());
        directory.upsert(vector(node(1), 16, 10, 32_000)); // strong
        directory.upsert(vector(node(2), 4, 80, 4_000)); // weak
        directory.upsert(vector(local, 64, 0, 128_000)); // ourselves: excluded

        let scheduler = scheduler_with(directory.clone(), local);
        let ranked = scheduler.rank(TaskClass::General);
        assert_eq!(
            ranked.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            vec![node(1), node(2)]
        );

        // Stale entries drop out of the ranking.
        directory.remove(&node(1));
        let ranked = scheduler.rank(TaskClass::General);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].0, node(2));
    }

    #[tokio::test]
    async fn empty_directory_yields_no_candidates() {
        let local = node(9);
        let scheduler = scheduler_with(Arc::new(CapabilityDirectory::new()), local);
        let request = ExecuteRequest {
            task_id: uuid::Uuid::new_v4(),
            wasm_hash: iroh_blobs::Hash::new(b"x"),
            entrypoint: "gdb_run".into(),
            class: TaskClass::General,
            limits: ResourceLimits::default(),
            input: vec![],
            required_model: None,
        };
        assert!(matches!(
            scheduler.execute(request).await,
            Err(ScheduleError::NoCandidates)
        ));
    }
}
