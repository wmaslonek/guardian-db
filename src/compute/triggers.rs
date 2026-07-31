//! # Reactive triggers (Phase 4)
//!
//! "Whenever new data lands in this store, run that task on the most capable
//! node" (RFC §5.7). A [`TriggerRule`] binds a store address (prefix) to a
//! [`TaskSpec`]; replication events fire the matching rules.
//!
//! ## Deduplication
//!
//! `EventReplicated` fires on **every** replica, so N nodes would schedule
//! the same task N times. The firing itself is therefore a *conditional
//! write* in the [`TaskLedger`]: the task key is
//! `blake3(rule id ␟ event id)`, deterministic on every replica, and only
//! the node whose `claim` wins actually dispatches. Over an LWW-replicated
//! ledger this is best-effort (RFC §5.7: rare races duplicate execution, so
//! triggered tasks should be idempotent); over [`MemoryLedger`]
//! (single-node) it is exact.
//!
//! ## Requeue
//!
//! [`TriggerEngine::requeue_due`] re-dispatches `Pending` tasks and `Running`
//! tasks whose deadline passed (their dispatcher died mid-flight), until the
//! retry budget is spent. Run it from [`TriggerEngine::spawn_requeue_loop`].
//!
//! [`MemoryLedger`]: super::ledger::MemoryLedger

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tracing::{debug, warn};
use uuid::Uuid;

use super::TaskSpec;
use super::ledger::{TaskKey, TaskLedger, TaskRecord, TaskState, unix_now};
use super::protocol::ExecuteRequest;
use super::scheduler::{ComputeScheduler, Delegated, ScheduleError};

/// A reactive rule: events on `store_address` (prefix match) run `spec`.
#[derive(Debug, Clone)]
pub struct TriggerRule {
    /// Stable rule name — part of the dedup key, so renaming a rule re-fires
    /// it for already-seen events.
    pub id: String,
    /// Store address this rule watches (prefix match).
    pub store_address: String,
    /// What to run. The event payload becomes the task input.
    pub spec: TaskSpec,
}

/// Where triggered tasks go to be executed. The production dispatcher is the
/// capability-aware [`ComputeScheduler`]; tests inject mocks.
#[async_trait]
pub trait TaskDispatcher: Send + Sync + 'static {
    async fn dispatch(&self, request: ExecuteRequest) -> Result<Delegated, DispatchError>;
}

/// Why a dispatch failed, split by whether retrying can help.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct DispatchError {
    /// Permanent failures (the task itself is broken) go straight to
    /// `Failed`; transient ones (no candidates, everyone busy) stay
    /// `Pending` for the requeue loop.
    pub permanent: bool,
    pub message: String,
}

#[async_trait]
impl TaskDispatcher for ComputeScheduler {
    async fn dispatch(&self, request: ExecuteRequest) -> Result<Delegated, DispatchError> {
        self.execute(request).await.map_err(|e| DispatchError {
            permanent: matches!(e, ScheduleError::TaskFailed { .. }),
            message: e.to_string(),
        })
    }
}

/// Tuning of the trigger engine.
#[derive(Debug, Clone)]
pub struct TriggerConfig {
    /// Dispatch attempts per task before it is marked `Failed`.
    pub max_attempts: u32,
    /// How long a dispatched task may stay `Running` before another pass
    /// considers it abandoned and requeues it.
    pub task_deadline: Duration,
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            task_deadline: Duration::from_secs(300),
        }
    }
}

/// The reactive engine: rules + ledger + dispatcher.
pub struct TriggerEngine {
    rules: parking_lot::RwLock<Vec<TriggerRule>>,
    ledger: TaskLedger,
    dispatcher: Arc<dyn TaskDispatcher>,
    config: TriggerConfig,
}

impl std::fmt::Debug for TriggerEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TriggerEngine")
            .field("rules", &self.rules.read().len())
            .finish_non_exhaustive()
    }
}

impl TriggerEngine {
    pub fn new(
        ledger: TaskLedger,
        dispatcher: Arc<dyn TaskDispatcher>,
        config: TriggerConfig,
    ) -> Self {
        Self {
            rules: parking_lot::RwLock::new(Vec::new()),
            ledger,
            dispatcher,
            config,
        }
    }

    /// Registers a rule: events on `store_address` run `spec` (RFC §5.7's
    /// `on_replicated`). Returns the rule id.
    pub fn on_replicated(&self, store_address: impl Into<String>, spec: TaskSpec) -> String {
        let store_address = store_address.into();
        let id = format!("rule-{}", Uuid::new_v4());
        self.rules.write().push(TriggerRule {
            id: id.clone(),
            store_address,
            spec,
        });
        id
    }

    /// Registers a rule with a caller-chosen stable id (needed when several
    /// replicas must agree on the dedup key — give them the same id).
    pub fn add_rule(&self, rule: TriggerRule) {
        self.rules.write().push(rule);
    }

    pub fn remove_rule(&self, id: &str) {
        self.rules.write().retain(|r| r.id != id);
    }

    /// The ledger this engine writes through (for inspection/UI).
    pub fn ledger(&self) -> &TaskLedger {
        &self.ledger
    }

    /// Fires the rules matching a replication event. `event_id` must be
    /// unique and replica-independent (an entry hash is ideal); `payload`
    /// becomes the task input. Returns the keys of the tasks *this* call
    /// claimed (deduped ones are silently skipped).
    pub async fn notify_replicated(
        self: &Arc<Self>,
        store_address: &str,
        event_id: &[u8],
        payload: &[u8],
    ) -> Vec<TaskKey> {
        let matching: Vec<TriggerRule> = self
            .rules
            .read()
            .iter()
            .filter(|rule| store_address.starts_with(&rule.store_address))
            .cloned()
            .collect();

        let mut claimed = Vec::new();
        for rule in matching {
            let key = dedup_key(&rule.id, event_id);
            let record = TaskRecord::pending(
                key.clone(),
                rule.spec.wasm_hash,
                rule.spec.entrypoint.clone(),
                rule.spec.class,
                rule.spec.limits,
                payload.to_vec(),
                rule.spec.required_model.clone(),
            );
            match self.ledger.claim(&record).await {
                Ok(true) => {
                    debug!(rule = %rule.id, task = %key, store = store_address,
                           "compute trigger: task claimed");
                    let engine = self.clone();
                    let task_key = key.clone();
                    tokio::spawn(async move {
                        engine.run_task(&task_key).await;
                    });
                    claimed.push(key);
                }
                Ok(false) => {
                    debug!(rule = %rule.id, task = %key,
                           "compute trigger: already claimed elsewhere, skipping");
                }
                Err(e) => warn!(rule = %rule.id, "compute trigger: ledger claim failed: {e}"),
            }
        }
        claimed
    }

    /// Dispatches one claimed task, walking the ledger through
    /// `Running → Done | Failed | Pending(retry)`.
    ///
    /// The transition to `Running` is an atomic claim: if another dispatcher
    /// (a concurrent requeue pass, or the reactive path) already owns the
    /// task, this returns without dispatching — preventing double execution.
    async fn run_task(&self, key: &str) {
        let deadline = unix_now() + self.config.task_deadline.as_secs();
        let record = match self.ledger.claim_for_dispatch(key, deadline).await {
            Ok(Some(record)) => record,
            Ok(None) => return, // absent, terminal, or already claimed elsewhere
            Err(e) => {
                warn!(task = %key, "compute trigger: claim failed: {e}");
                return;
            }
        };

        let request = ExecuteRequest {
            task_id: Uuid::new_v4(),
            wasm_hash: record.wasm_hash,
            entrypoint: record.entrypoint.clone(),
            class: record.class,
            limits: record.limits,
            input: record.input.clone(),
            required_model: record.required_model.clone(),
        };

        match self.dispatcher.dispatch(request).await {
            Ok(delegated) => {
                debug!(task = %key, executor = %delegated.executor.fmt_short(),
                       "compute trigger: task done");
                let _ = self
                    .ledger
                    .update(key, |r| {
                        r.state = TaskState::Done {
                            executor: delegated.executor,
                        };
                        r.result = Some(delegated.completed.clone());
                        // The input is only needed to (re)dispatch; a terminal
                        // task never will. Dropping it keeps the requeue scan
                        // cheap even as Done/Failed records accumulate (media
                        // payloads would otherwise be re-parsed every tick).
                        r.input = Vec::new();
                    })
                    .await;
            }
            Err(e) => {
                let spent = record.attempts >= self.config.max_attempts;
                let final_failure = e.permanent || spent;
                warn!(task = %key, attempts = record.attempts, permanent = e.permanent,
                      "compute trigger: dispatch failed: {}", e.message);
                let _ = self
                    .ledger
                    .update(key, |r| {
                        if final_failure {
                            r.state = TaskState::Failed { error: e.message };
                            r.input = Vec::new(); // terminal: input no longer needed
                        } else {
                            // Back to Pending: the requeue loop retries it, so
                            // the input must stay.
                            r.state = TaskState::Pending;
                        }
                    })
                    .await;
            }
        }
    }

    /// One requeue pass: re-dispatches every task that is `Pending` or
    /// `Running` past deadline. Returns how many were re-dispatched.
    pub async fn requeue_due(self: &Arc<Self>) -> usize {
        let due = match self.ledger.needing_dispatch().await {
            Ok(due) => due,
            Err(e) => {
                warn!("compute trigger: requeue scan failed: {e}");
                return 0;
            }
        };
        let mut dispatched = 0;
        for record in due {
            if record.attempts >= self.config.max_attempts {
                let _ = self
                    .ledger
                    .update(&record.key, |r| {
                        r.state = TaskState::Failed {
                            error: "retry budget exhausted".into(),
                        };
                        r.input = Vec::new(); // terminal: input no longer needed
                    })
                    .await;
                continue;
            }
            let engine = self.clone();
            let key = record.key.clone();
            tokio::spawn(async move {
                engine.run_task(&key).await;
            });
            dispatched += 1;
        }
        dispatched
    }

    /// Spawns the periodic requeue loop. Abort the handle to stop it.
    pub fn spawn_requeue_loop(self: &Arc<Self>, every: Duration) -> tokio::task::JoinHandle<()> {
        let engine = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(every);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                engine.requeue_due().await;
            }
        })
    }

    /// Bridges the store event bus into this engine: every entry of every
    /// `EventReplicated` fires [`TriggerEngine::notify_replicated`] with the
    /// entry hash as the event id and the entry payload as the task input.
    /// Abort the handle to detach.
    pub fn attach_event_bus(
        self: &Arc<Self>,
        event_bus: Arc<crate::events::EventBus>,
    ) -> tokio::task::JoinHandle<()> {
        let engine = self.clone();
        tokio::spawn(async move {
            let mut receiver = match event_bus
                .subscribe::<crate::stores::events::EventReplicated>()
                .await
            {
                Ok(receiver) => receiver,
                Err(e) => {
                    warn!("compute trigger: EventReplicated subscription failed: {e}");
                    return;
                }
            };
            while let Ok(event) = receiver.recv().await {
                let address = event.address.to_string();
                for entry in &event.entries {
                    engine
                        .notify_replicated(&address, entry.hash().as_bytes(), entry.payload())
                        .await;
                }
            }
        })
    }
}

/// Deterministic task key for (rule, event): identical on every replica, so
/// the ledger's conditional write arbitrates who runs it.
fn dedup_key(rule_id: &str, event_id: &[u8]) -> TaskKey {
    let mut hasher = blake3::Hasher::new();
    hasher.update(rule_id.as_bytes());
    hasher.update(&[0x1f]);
    hasher.update(event_id);
    hex::encode(hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compute::{CompletedTask, ExecMetrics, Placement, ResourceLimits, TaskClass};
    use iroh_blobs::Hash;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn spec() -> TaskSpec {
        TaskSpec {
            wasm_hash: Hash::new(b"thumbnailer"),
            entrypoint: "generate_thumbnail".into(),
            class: TaskClass::Media,
            limits: ResourceLimits::default(),
            placement: Placement::BestAvailable,
            required_model: None,
        }
    }

    /// Dispatcher that succeeds, counting invocations.
    struct CountingDispatcher {
        calls: AtomicU32,
    }

    #[async_trait]
    impl TaskDispatcher for CountingDispatcher {
        async fn dispatch(&self, request: ExecuteRequest) -> Result<Delegated, DispatchError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Delegated {
                executor: iroh::SecretKey::from_bytes(&[7u8; 32]).public(),
                completed: CompletedTask {
                    output: request.input, // echo
                    metrics: ExecMetrics {
                        fuel_consumed: 1,
                        duration_ms: 1,
                        peak_memory_bytes: 65_536,
                    },
                },
            })
        }
    }

    /// Dispatcher that always fails, transiently or permanently.
    struct FailingDispatcher {
        permanent: bool,
        calls: AtomicU32,
    }

    #[async_trait]
    impl TaskDispatcher for FailingDispatcher {
        async fn dispatch(&self, _request: ExecuteRequest) -> Result<Delegated, DispatchError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(DispatchError {
                permanent: self.permanent,
                message: "boom".into(),
            })
        }
    }

    async fn wait_for_state<F>(engine: &Arc<TriggerEngine>, key: &str, matches: F)
    where
        F: Fn(&TaskState) -> bool,
    {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(Some(record)) = engine.ledger().get(key).await
                && matches(&record.state)
            {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "task {key} never reached the expected state"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn event_fires_rule_once_and_completes() {
        let dispatcher = Arc::new(CountingDispatcher {
            calls: AtomicU32::new(0),
        });
        let engine = Arc::new(TriggerEngine::new(
            TaskLedger::in_memory(),
            dispatcher.clone(),
            TriggerConfig::default(),
        ));
        engine.add_rule(TriggerRule {
            id: "thumbnails".into(),
            store_address: "/fotos".into(),
            spec: spec(),
        });

        // The same replication event arrives twice (e.g. two sync rounds):
        // only the first claim dispatches.
        let claimed = engine
            .notify_replicated("/fotos/album1", b"entry-hash-1", b"jpeg bytes")
            .await;
        assert_eq!(claimed.len(), 1);
        let key = claimed[0].clone();
        let again = engine
            .notify_replicated("/fotos/album1", b"entry-hash-1", b"jpeg bytes")
            .await;
        assert!(again.is_empty(), "duplicate event must be deduped");

        wait_for_state(&engine, &key, |s| matches!(s, TaskState::Done { .. })).await;
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
        let record = engine.ledger().get(&key).await.unwrap().unwrap();
        assert!(
            record.input.is_empty(),
            "a terminal record drops its input so the requeue scan stays cheap"
        );
        assert_eq!(record.result.unwrap().output, b"jpeg bytes");
    }

    #[tokio::test]
    async fn unmatched_address_fires_nothing() {
        let engine = Arc::new(TriggerEngine::new(
            TaskLedger::in_memory(),
            Arc::new(CountingDispatcher {
                calls: AtomicU32::new(0),
            }),
            TriggerConfig::default(),
        ));
        engine.add_rule(TriggerRule {
            id: "thumbnails".into(),
            store_address: "/fotos".into(),
            spec: spec(),
        });
        let claimed = engine.notify_replicated("/documentos", b"e1", b"pdf").await;
        assert!(claimed.is_empty());
    }

    #[tokio::test]
    async fn permanent_failure_is_terminal() {
        let dispatcher = Arc::new(FailingDispatcher {
            permanent: true,
            calls: AtomicU32::new(0),
        });
        let engine = Arc::new(TriggerEngine::new(
            TaskLedger::in_memory(),
            dispatcher.clone(),
            TriggerConfig::default(),
        ));
        engine.add_rule(TriggerRule {
            id: "r".into(),
            store_address: "/fotos".into(),
            spec: spec(),
        });

        let key = engine
            .notify_replicated("/fotos", b"e1", b"x")
            .await
            .remove(0);
        wait_for_state(&engine, &key, |s| matches!(s, TaskState::Failed { .. })).await;

        // A requeue pass must NOT resurrect it.
        assert_eq!(engine.requeue_due().await, 0);
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn transient_failure_retries_until_budget_then_fails() {
        let dispatcher = Arc::new(FailingDispatcher {
            permanent: false,
            calls: AtomicU32::new(0),
        });
        let engine = Arc::new(TriggerEngine::new(
            TaskLedger::in_memory(),
            dispatcher.clone(),
            TriggerConfig {
                max_attempts: 2,
                ..TriggerConfig::default()
            },
        ));
        engine.add_rule(TriggerRule {
            id: "r".into(),
            store_address: "/fotos".into(),
            spec: spec(),
        });

        let key = engine
            .notify_replicated("/fotos", b"e1", b"x")
            .await
            .remove(0);
        // Attempt 1 fails transiently → back to Pending.
        wait_for_state(&engine, &key, |s| matches!(s, TaskState::Pending)).await;

        // Requeue pass runs attempt 2 — the budget's last — which also fails:
        // now the failure is final.
        assert_eq!(engine.requeue_due().await, 1);
        wait_for_state(&engine, &key, |s| matches!(s, TaskState::Failed { .. })).await;
        assert_eq!(engine.requeue_due().await, 0, "failed tasks stay failed");
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn abandoned_running_task_is_requeued() {
        let dispatcher = Arc::new(CountingDispatcher {
            calls: AtomicU32::new(0),
        });
        let engine = Arc::new(TriggerEngine::new(
            TaskLedger::in_memory(),
            dispatcher.clone(),
            TriggerConfig::default(),
        ));

        // Simulate a task another node claimed and then abandoned: Running
        // with an expired deadline, written straight into the ledger.
        let mut record = TaskRecord::pending(
            "abandoned".into(),
            Hash::new(b"wasm"),
            "gdb_run".into(),
            TaskClass::Media,
            ResourceLimits::default(),
            b"input".to_vec(),
            None,
        );
        record.attempts = 1;
        record.state = TaskState::Running {
            deadline_unix: unix_now() - 10,
        };
        engine.ledger().claim(&record).await.unwrap();

        assert_eq!(engine.requeue_due().await, 1);
        wait_for_state(&engine, "abandoned", |s| {
            matches!(s, TaskState::Done { .. })
        })
        .await;
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
    }
}
