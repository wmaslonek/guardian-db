//! The auto-embedding service: drives [`EmbeddingRule`]s off the SQL engine's
//! committed-change feed, embedding text into vector columns idempotently.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::embedding::rule::EmbeddingRule;
#[cfg(feature = "compute")]
use crate::embedding::rule::ExecutionPolicy;
use crate::embedding::{EmbeddingError, EmbeddingRegistry};
use crate::relational::{RelationalStorage, SqlType};
use crate::sql::engine::{ChangeOp, Database};

/// Provenance recorded alongside each written embedding, in the rule's
/// sidecar column (RFC 0005 §6.4). Serialized to a JSON string. The
/// `text_hash` field is what the service compares for idempotency; the rest
/// makes a suspect corpus auditable and selectively re-embeddable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingProvenance {
    /// BLAKE3 (hex) of the exact source text that produced the current vector.
    #[serde(rename = "t")]
    pub text_hash: String,
    /// The model name used.
    #[serde(rename = "m")]
    pub model: String,
    /// The model's weight hash, when the backend could prove it (§6.3).
    #[serde(rename = "h", skip_serializing_if = "Option::is_none", default)]
    pub model_hash: Option<String>,
    /// Where the embedding ran: `"local"` or `"peer:<node-id>"`.
    #[serde(rename = "x")]
    pub executor: String,
}

impl EmbeddingProvenance {
    fn to_json_string(&self) -> String {
        serde_json::to_string(self).expect("provenance serializes")
    }

    fn parse(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }
}

/// Running counters for observability.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingStats {
    /// Rows embedded and written back.
    pub embedded: u64,
    /// Rows skipped because the source text was unchanged (idempotent).
    pub skipped_unchanged: u64,
    /// Rows skipped because the source text was NULL/absent.
    pub skipped_no_text: u64,
    /// Rows that errored (model missing, dimension mismatch, backend down).
    pub errored: u64,
}

/// Per-rule resolved context, cached after the first event for the rule's
/// table (storage collection name + the vector column's declared dimension).
#[derive(Clone)]
struct RuleTarget {
    collection: String,
    dims: u32,
    /// The rule's sidecar (source-hash / provenance) column exists in the
    /// catalog. When false the rule cannot be honored (write-back would
    /// silently drop the sidecar), so it is skipped with a logged error.
    has_sidecar: bool,
}

/// Text hash used for idempotency: BLAKE3 of the source text, hex.
fn text_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

/// The auto-embedding service. Construct with [`EmbeddingService::new`], add
/// rules, then [`spawn`](EmbeddingService::spawn) it to run in the background
/// against the database's change feed.
pub struct EmbeddingService<S: RelationalStorage> {
    db: Arc<Database<S>>,
    registry: EmbeddingRegistry,
    /// Rules declared through the Rust API. Merged with catalog-declared rules
    /// (the SQL surface) at run time.
    rules: Vec<EmbeddingRule>,
    /// How often catalog-declared rules (the SQL `guardian_embed(...)` surface)
    /// are re-read, so a rule declared after the service started takes effect.
    catalog_refresh: std::time::Duration,
    #[cfg(feature = "compute")]
    delegate: Option<Arc<dyn crate::embedding::delegate::EmbeddingDelegate>>,
}

impl<S: RelationalStorage + 'static> EmbeddingService<S> {
    pub fn new(db: Arc<Database<S>>, registry: EmbeddingRegistry) -> Self {
        Self {
            db,
            registry,
            rules: Vec::new(),
            catalog_refresh: std::time::Duration::from_secs(5),
            #[cfg(feature = "compute")]
            delegate: None,
        }
    }

    /// Set how often catalog-declared (SQL) rules are re-read. Default 5s.
    pub fn with_catalog_refresh(mut self, every: std::time::Duration) -> Self {
        self.catalog_refresh = every;
        self
    }

    /// Read the catalog's declared embedding rules (the SQL surface) and
    /// convert them to [`EmbeddingRule`]s. Malformed policy strings fall back
    /// to local. Returns empty on a catalog read error (logged).
    async fn load_catalog_rules(&self) -> Vec<EmbeddingRule> {
        use crate::embedding::ModelSelector;
        use crate::embedding::rule::ExecutionPolicy;
        let catalog = match self.db.catalog_snapshot().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "embedding: catalog read failed; keeping current rules");
                return Vec::new();
            }
        };
        catalog
            .embedding_rules()
            .iter()
            .map(|d| {
                let policy = match d.policy.as_str() {
                    "delegated" => ExecutionPolicy::delegated(),
                    _ => ExecutionPolicy::LocalOnly,
                };
                EmbeddingRule::new(
                    d.table.clone(),
                    d.text_column.clone(),
                    d.vector_column.clone(),
                    ModelSelector::by_name(d.model.clone()),
                )
                .with_schema(d.schema.clone())
                .with_policy(policy)
            })
            .collect()
    }

    /// Add a rule. Returns `self` for chaining.
    pub fn with_rule(mut self, rule: EmbeddingRule) -> Self {
        self.rules.push(rule);
        self
    }

    pub fn add_rule(&mut self, rule: EmbeddingRule) {
        self.rules.push(rule);
    }

    /// Attach a delegation transport (feature `compute`) used by rules whose
    /// [`ExecutionPolicy`] is `Delegated`. Without one, delegated rules fall
    /// back to local execution when their policy allows it.
    #[cfg(feature = "compute")]
    pub fn with_delegate(
        mut self,
        delegate: Arc<dyn crate::embedding::delegate::EmbeddingDelegate>,
    ) -> Self {
        self.delegate = Some(delegate);
        self
    }

    /// Process a single already-decoded backlog row (for initial backfill of
    /// rows that existed before the rule was added). `doc` is the stored row
    /// document. Public so an owner can backfill on startup.
    pub async fn embed_row(&self, schema: &str, table: &str, row_id: &str, doc: &JsonValue) {
        let mut targets: HashMap<String, RuleTarget> = HashMap::new();
        let all = self.effective_rules(&self.load_catalog_rules().await);
        self.process_row(
            &all,
            schema,
            table,
            row_id,
            doc,
            &mut targets,
            &mut EmbeddingStats::default(),
        )
        .await;
    }

    /// Run the service in the background, returning a handle. The service
    /// stops when the returned handle is aborted or the database's change
    /// senders are all dropped.
    ///
    /// The change-feed subscription is registered **synchronously here**,
    /// before this returns, so a write issued right after `spawn()` cannot
    /// race ahead of the listener and be missed.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        let rx = self.db.subscribe_changes();
        tokio::spawn(async move { self.run_with(rx).await })
    }

    /// Run the service loop in the current task until the change feed closes,
    /// subscribing now. Prefer [`spawn`](Self::spawn) when a later write must
    /// not race the subscription.
    pub async fn run(self) {
        let rx = self.db.subscribe_changes();
        self.run_with(rx).await
    }

    async fn run_with(
        self,
        mut rx: tokio::sync::mpsc::UnboundedReceiver<crate::sql::engine::ChangeEvent>,
    ) {
        let mut targets: HashMap<String, RuleTarget> = HashMap::new();
        let mut stats = EmbeddingStats::default();
        // Catalog-declared (SQL) rules, re-read on a timer so a rule declared
        // after startup takes effect. `targets` is cleared on refresh so a
        // dropped/changed rule does not keep a stale write-back target.
        let mut catalog_rules = self.load_catalog_rules().await;
        let mut refresh = tokio::time::interval(self.catalog_refresh);
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        refresh.tick().await; // consume the immediate first tick
        loop {
            tokio::select! {
                ev = rx.recv() => {
                    let Some(ev) = ev else { break };
                    if matches!(ev.op, ChangeOp::Delete) {
                        continue;
                    }
                    let Some(doc) = ev.new.as_ref() else { continue };
                    let Some(row_id) = ev.row_id() else { continue };
                    let row_id = row_id.to_string();
                    let all = self.effective_rules(&catalog_rules);
                    self.process_row(&all, &ev.schema, &ev.table, &row_id, doc, &mut targets, &mut stats).await;
                }
                _ = refresh.tick() => {
                    catalog_rules = self.load_catalog_rules().await;
                    targets.clear();
                }
            }
        }
    }

    /// Static (Rust API) rules followed by catalog (SQL) rules. A catalog rule
    /// for the same vector column as a static one is a duplicate the
    /// per-target idempotency absorbs harmlessly.
    fn effective_rules(&self, catalog_rules: &[EmbeddingRule]) -> Vec<EmbeddingRule> {
        let mut all = self.rules.clone();
        all.extend_from_slice(catalog_rules);
        all
    }

    /// Resolve (and cache) a rule's storage collection + vector dimension +
    /// sidecar presence from a fresh catalog snapshot. `None` if the table or
    /// vector column no longer exists / is not a dimensioned vector.
    async fn resolve_target(&self, rule: &EmbeddingRule) -> Option<RuleTarget> {
        let catalog = self.db.catalog_snapshot().await.ok()?;
        let q = catalog.resolve_table_name(Some(&rule.schema), &rule.table)?;
        let table = catalog.get_table(&q)?;
        let col = table.column(&rule.vector_column)?;
        let dims = match &col.ty {
            SqlType::Vector(Some(d)) => *d,
            _ => {
                tracing::error!(
                    rule = %rule.id(),
                    "embedding: target column is not a dimensioned vector; rule disabled"
                );
                return None;
            }
        };
        let has_sidecar = table.column(&rule.source_hash_column()).is_some();
        if !has_sidecar {
            tracing::error!(
                rule = %rule.id(),
                sidecar = %rule.source_hash_column(),
                "embedding: missing sidecar column; add it (TEXT) — rule disabled"
            );
        }
        Some(RuleTarget {
            collection: table.storage_collection.clone(),
            dims,
            has_sidecar,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn process_row(
        &self,
        rules: &[EmbeddingRule],
        schema: &str,
        table: &str,
        row_id: &str,
        doc: &JsonValue,
        targets: &mut HashMap<String, RuleTarget>,
        stats: &mut EmbeddingStats,
    ) {
        for rule in rules.iter().filter(|r| r.matches(schema, table)) {
            // The source text. NULL/absent → nothing to embed.
            let text = match doc.get(&rule.text_column).and_then(JsonValue::as_str) {
                Some(t) if !t.is_empty() => t.to_string(),
                _ => {
                    stats.skipped_no_text += 1;
                    continue;
                }
            };
            let th = text_hash(&text);

            // Idempotency: if the sidecar already records this exact text
            // hash, the current vector is up to date — skip. This is also
            // what terminates the write-back → change → re-embed cycle when a
            // future path routes write-backs through the change feed.
            let sidecar_col = rule.source_hash_column();
            if let Some(prev) = doc
                .get(&sidecar_col)
                .and_then(JsonValue::as_str)
                .and_then(EmbeddingProvenance::parse)
                && prev.text_hash == th
            {
                stats.skipped_unchanged += 1;
                continue;
            }

            // Resolve (cached) the write-back target for this rule's table.
            let target = match targets.get(&rule.id()) {
                Some(t) => t.clone(),
                None => match self.resolve_target(rule).await {
                    Some(t) => {
                        targets.insert(rule.id(), t.clone());
                        t
                    }
                    None => {
                        stats.errored += 1;
                        continue;
                    }
                },
            };
            if !target.has_sidecar {
                stats.errored += 1;
                continue;
            }

            match self
                .embed_and_write(rule, &target, row_id, &text, &th)
                .await
            {
                Ok(()) => stats.embedded += 1,
                Err(e) => {
                    tracing::warn!(rule = %rule.id(), row = row_id, error = %e, "embedding failed");
                    stats.errored += 1;
                }
            }
        }
    }

    async fn embed_and_write(
        &self,
        rule: &EmbeddingRule,
        target: &RuleTarget,
        row_id: &str,
        text: &str,
        text_hash: &str,
    ) -> Result<(), EmbeddingError> {
        let (vector, provenance) = self.compute_embedding(rule, text, text_hash).await?;
        if vector.len() != target.dims as usize {
            return Err(EmbeddingError::DimensionMismatch {
                got: vector.len(),
                expected: target.dims as usize,
            });
        }
        let vec_json = JsonValue::Array(
            vector
                .iter()
                .map(|f| {
                    serde_json::Number::from_f64(*f as f64)
                        .map(JsonValue::Number)
                        .unwrap_or(JsonValue::Null)
                })
                .collect(),
        );
        let updates = vec![
            (rule.vector_column.clone(), vec_json),
            (
                rule.source_hash_column(),
                JsonValue::String(provenance.to_json_string()),
            ),
        ];
        self.db
            .patch_row(&target.collection, row_id, &updates)
            .await
            .map_err(|e| EmbeddingError::BackendUnavailable(e.to_string()))
    }

    /// Compute the embedding per the rule's execution policy, returning the
    /// vector and its provenance.
    async fn compute_embedding(
        &self,
        rule: &EmbeddingRule,
        text: &str,
        text_hash: &str,
    ) -> Result<(Vec<f32>, EmbeddingProvenance), EmbeddingError> {
        // Delegated policy (feature `compute`): try a GPU peer first.
        #[cfg(feature = "compute")]
        if let ExecutionPolicy::Delegated {
            pin_hash,
            allow_local_fallback,
        } = &rule.policy
        {
            if let Some(delegate) = &self.delegate {
                match delegate.embed_delegated(&rule.model, *pin_hash, text).await {
                    Ok(out) => {
                        let prov = EmbeddingProvenance {
                            text_hash: text_hash.to_string(),
                            model: rule.model.name.clone(),
                            model_hash: out.model_hash.map(|h| h.to_string()),
                            executor: format!("peer:{}", out.peer),
                        };
                        return Ok((out.vector, prov));
                    }
                    Err(e) if *allow_local_fallback => {
                        tracing::warn!(
                            rule = %rule.id(),
                            error = %e,
                            "delegated embedding failed; falling back to local"
                        );
                    }
                    Err(e) => return Err(e),
                }
            } else if !*allow_local_fallback {
                return Err(EmbeddingError::Delegation(
                    "no delegate configured and local fallback disabled".into(),
                ));
            }
        }

        // Local execution.
        let embedder = self.registry.resolve(&rule.model)?;
        let mut out = embedder
            .embed(std::slice::from_ref(&text.to_string()))
            .await?;
        embedder.validate_batch(1, &out)?;
        let vector = out.pop().ok_or_else(|| {
            EmbeddingError::Malformed("backend returned no vector for one input".into())
        })?;
        let info = embedder.info();
        let prov = EmbeddingProvenance {
            text_hash: text_hash.to_string(),
            model: info.name,
            model_hash: info.content_hash.map(|h| h.to_string()),
            executor: "local".into(),
        };
        Ok((vector, prov))
    }
}

#[cfg(all(test, feature = "compute"))]
mod delegation_tests {
    use super::*;
    use crate::embedding::delegate::test_support::MockDelegate;
    use crate::embedding::{Embedder, HashEmbedder, ModelSelector};
    use crate::sql::MemoryStorage;

    const DIMS: u32 = 8;

    fn service(
        delegate: Option<Arc<MockDelegate>>,
        register_local: bool,
    ) -> EmbeddingService<MemoryStorage> {
        let db = Arc::new(Database::new(Arc::new(MemoryStorage::new()), "app"));
        let registry = EmbeddingRegistry::new();
        if register_local {
            registry.register(Arc::new(HashEmbedder::new("m", DIMS)));
        }
        let mut svc = EmbeddingService::new(db, registry);
        if let Some(d) = delegate {
            svc = svc.with_delegate(d);
        }
        svc
    }

    fn rule(pin: bool, fallback: bool) -> EmbeddingRule {
        let model = if pin {
            ModelSelector::pinned(
                "m",
                HashEmbedder::new("m", DIMS).info().content_hash.unwrap(),
            )
        } else {
            ModelSelector::by_name("m")
        };
        EmbeddingRule::new("t", "body", "v", model).with_policy(ExecutionPolicy::Delegated {
            pin_hash: pin,
            allow_local_fallback: fallback,
        })
    }

    #[tokio::test]
    async fn delegated_success_records_peer_provenance() {
        let d = Arc::new(MockDelegate::new("m", DIMS));
        let calls = d.calls.clone();
        let svc = service(Some(d), false);
        let r = rule(false, true);
        let (vec, prov) = svc.compute_embedding(&r, "hello", "th").await.unwrap();
        assert_eq!(vec.len(), DIMS as usize);
        assert!(prov.executor.starts_with("peer:"), "{}", prov.executor);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn delegation_failure_falls_back_to_local_when_allowed() {
        let d = Arc::new(MockDelegate::new("m", DIMS).failing());
        let svc = service(Some(d), true); // local embedder registered
        let r = rule(false, true);
        let (_, prov) = svc.compute_embedding(&r, "hello", "th").await.unwrap();
        assert_eq!(prov.executor, "local", "must fall back to local");
    }

    #[tokio::test]
    async fn delegation_failure_without_fallback_is_an_error() {
        let d = Arc::new(MockDelegate::new("m", DIMS).failing());
        let svc = service(Some(d), true);
        let r = rule(false, false);
        assert!(svc.compute_embedding(&r, "hello", "th").await.is_err());
    }

    #[tokio::test]
    async fn pinned_wrong_hash_peer_is_rejected() {
        // A peer advertising the name but the wrong weight hash must not be
        // accepted under a pin; with fallback the service uses local instead.
        let d = Arc::new(MockDelegate::new("m", DIMS).with_wrong_hash());
        let svc = service(Some(d), true);
        let r = rule(true, true);
        let (_, prov) = svc.compute_embedding(&r, "hello", "th").await.unwrap();
        assert_eq!(prov.executor, "local", "pin mismatch must not delegate");
    }

    #[tokio::test]
    async fn no_delegate_without_fallback_errors() {
        let svc = service(None, true);
        let r = rule(false, false);
        assert!(svc.compute_embedding(&r, "hello", "th").await.is_err());
    }
}
