//! Engine-level ANN index runtime (RFC 0005 phase 1).
//!
//! [`SecondaryIndex`](crate::relational::SecondaryIndex)es are cheap enough to
//! rebuild inside every statement's [`LoadedTable`]; an HNSW graph is not.
//! `AnnRuntime` therefore lives on the [`Database`](crate::sql::engine::Database)
//! — shared by every session — and keeps one [`HnswIndexState`] per `hnsw`
//! catalog index, *reconciled* on use against the current statement's freshly
//! loaded table view:
//!
//! 1. a commutative fingerprint over the view's `(row_id, version)` pairs
//!    detects the common no-change case in one O(n) pass with no allocation;
//! 2. when the fingerprint moved, a merge pass inserts new/changed vectors
//!    (an unrelated-column update is recognized by vector content hash and
//!    does not churn the graph) and tombstones deleted rows;
//! 3. tombstones past the rebuild threshold trigger a full rebuild from the
//!    loaded rows — always safe, because the graph is derived state.
//!
//! Because reconciliation always runs against the statement's own view, rows
//! that arrived through replication (a peer's writes) are picked up exactly
//! like local writes — no event wiring, no divergence to repair.
//!
//! Snapshots (§6.1): when a snapshot directory is configured, indexes are
//! persisted after a rebuild and whenever the dirty-insert threshold is
//! crossed, and loaded (checksum-verified) on first use. A snapshot may lag
//! the replicated rows — reconciliation fixes the drift — so validity is
//! only *internal* consistency, which the BLAKE3-checksummed meta guarantees.

use crate::relational::catalog::Index;
use crate::relational::hnsw::{HnswIndexState, HnswOptions, VectorOpClass};
use crate::relational::{SqlType, SqlValue};
use crate::sql::store::LoadedTable;
use parking_lot::{Mutex, RwLock};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

/// Inserts since the last snapshot that trigger a background-free,
/// in-line snapshot save on the next reconcile (§6.1 cadence).
const SNAPSHOT_DIRTY_THRESHOLD: usize = 10_000;

/// One top-k ANN search request, as posed by the planner hook
/// (`try_ann_scan` in `crate::sql::select`).
pub struct AnnQuery<'a> {
    /// The catalog record of the `hnsw` index to search.
    pub idx: &'a Index,
    /// The indexed vector column and its declared type.
    pub column: &'a str,
    pub column_ty: &'a SqlType,
    /// The SQL distance operator (`<->`, `<#>`, `<=>`, `<+>`).
    pub op: &'a str,
    /// The query vector (already cast to the column type).
    pub query: &'a [f32],
    /// How many candidates to fetch and the search-time candidate list size.
    pub k: usize,
    pub ef: usize,
}

pub struct AnnRuntime {
    /// Where sidecar snapshots live; `None` disables persistence (§6.1's
    /// rebuild-from-rows anchor still applies unconditionally).
    snapshot_dir: RwLock<Option<PathBuf>>,
    /// Live index states keyed by catalog index oid. Oids are allocated
    /// monotonically and never reused, so a dropped-and-recreated index can
    /// never collide with a stale slot.
    slots: Mutex<HashMap<u32, Arc<Mutex<HnswIndexState>>>>,
}

impl Default for AnnRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl AnnRuntime {
    pub fn new() -> Self {
        Self {
            snapshot_dir: RwLock::new(None),
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// Enable sidecar snapshots under `dir` (a per-database subdirectory is
    /// appended by the caller if desired). Existing snapshots are picked up
    /// lazily as indexes are first used.
    pub fn set_snapshot_dir(&self, dir: Option<PathBuf>) {
        *self.snapshot_dir.write() = dir;
    }

    fn snapshot_base(oid: u32) -> String {
        format!("idx-{oid}")
    }

    /// Drop the in-memory state and snapshot files for a catalog index that
    /// no longer exists (DROP INDEX / DROP TABLE / DROP COLUMN).
    ///
    /// Called eagerly at DDL time; if the enclosing transaction later rolls
    /// back, the next use simply rebuilds — derived state self-heals.
    pub fn forget(&self, oid: u32) {
        self.slots.lock().remove(&oid);
        if let Some(dir) = self.snapshot_dir.read().clone() {
            HnswIndexState::remove_snapshot(&dir, &Self::snapshot_base(oid));
        }
    }

    /// Parse the catalog record of an `hnsw` index into its runtime
    /// parameters. `None` if the record is malformed (e.g. hand-edited
    /// catalog) — the caller then skips ANN planning for it.
    fn index_params(idx: &Index, column_ty: &SqlType) -> Option<(VectorOpClass, HnswOptions, u32)> {
        let opclass = VectorOpClass::from_name(idx.opclasses.first()?)?;
        let dims = match column_ty {
            SqlType::Vector(Some(d)) => *d,
            _ => return None,
        };
        let mut options = HnswOptions::default();
        for (k, v) in &idx.with_options {
            match k.as_str() {
                "m" => options.m = v.parse().ok()?,
                "ef_construction" => options.ef_construction = v.parse().ok()?,
                _ => {}
            }
        }
        Some((opclass, options, dims))
    }

    /// Fetch (or create, loading a snapshot when available) the slot for an
    /// index.
    fn slot(
        &self,
        idx: &Index,
        opclass: VectorOpClass,
        options: HnswOptions,
        dims: u32,
    ) -> Arc<Mutex<HnswIndexState>> {
        let mut slots = self.slots.lock();
        slots
            .entry(idx.oid)
            .or_insert_with(|| {
                let dir = self.snapshot_dir.read().clone();
                let loaded = dir.as_ref().and_then(|d| {
                    HnswIndexState::load_snapshot(
                        d,
                        &Self::snapshot_base(idx.oid),
                        opclass,
                        options,
                        dims,
                    )
                });
                let state = match loaded {
                    Some(s) => {
                        tracing::info!(index = %idx.name, oid = idx.oid, "hnsw: snapshot loaded");
                        s
                    }
                    None => HnswIndexState::new(opclass, options, dims),
                };
                Arc::new(Mutex::new(state))
            })
            .clone()
    }

    /// Answer a top-k ANN scan for the current table view.
    ///
    /// Reconciles the graph with `loaded` first (see module docs), then
    /// searches. Returns ranked `(row_id, index_distance)` pairs, most
    /// similar first, tombstones excluded during traversal.
    pub fn candidates(
        &self,
        req: &AnnQuery<'_>,
        loaded: &LoadedTable,
    ) -> Option<Vec<(String, f32)>> {
        let (opclass, options, dims) = Self::index_params(req.idx, req.column_ty)?;
        // `op` must be the operator the opclass serves — the planner already
        // matched it; this is a defense-in-depth recheck.
        if opclass.distance_op() != req.op || req.query.len() != dims as usize {
            return None;
        }
        let slot = self.slot(req.idx, opclass, options, dims);
        let mut state = slot.lock();
        self.reconcile(&mut state, loaded, req.column, req.idx.oid);
        Some(state.search(req.query, req.k, req.ef))
    }

    /// The number of rows currently represented in the index (post-reconcile
    /// callers only; used by the planner's growth loop to know when the whole
    /// graph has been fetched).
    pub fn live_count(&self, idx: &Index) -> Option<usize> {
        let slots = self.slots.lock();
        slots.get(&idx.oid).map(|s| s.lock().live_count())
    }

    fn reconcile(&self, state: &mut HnswIndexState, loaded: &LoadedTable, column: &str, oid: u32) {
        let fp = HnswIndexState::view_fingerprint(
            loaded.versions.iter().map(|(rid, v)| (rid.as_str(), *v)),
        );
        if state.is_current(fp) {
            return;
        }
        // Initial build of an empty index: take the parallel bulk path
        // (~15× the serial insert rate) instead of row-by-row reconcile.
        // `live_count == 0 && next graph is unbuilt` is exactly the
        // cold-start / just-loaded-empty case.
        if state.live_count() == 0 {
            *state = Self::bulk_from_view(state, loaded, column);
            state.dirty = usize::MAX; // always snapshot a full build
            state.set_fingerprint(fp);
            self.maybe_snapshot(state, oid);
            return;
        }
        let mut live: HashSet<&str> = HashSet::with_capacity(loaded.rows.len());
        for (rid, values) in &loaded.rows {
            let vector = match values.get(column) {
                Some(SqlValue::Vector(v)) => Some(v.as_slice()),
                _ => None,
            };
            state.reconcile_entry(rid, loaded.version_of(rid), vector);
            live.insert(rid.as_str());
        }
        state.retain_rows(&live);
        if state.needs_rebuild() {
            tracing::info!(oid, "hnsw: tombstone threshold crossed; rebuilding index");
            *state = Self::bulk_from_view(state, loaded, column);
            // A rebuild always snapshots (when configured): it is exactly the
            // work a snapshot exists to avoid repeating.
            state.dirty = usize::MAX;
        }
        state.set_fingerprint(fp);
        self.maybe_snapshot(state, oid);
    }

    /// Parallel bulk-build a fresh index from the current table view.
    fn bulk_from_view(
        state: &HnswIndexState,
        loaded: &LoadedTable,
        column: &str,
    ) -> HnswIndexState {
        HnswIndexState::build_bulk(
            state.opclass,
            state.options,
            state.dims,
            loaded.rows.iter().map(|(rid, values)| {
                let v = match values.get(column) {
                    Some(SqlValue::Vector(v)) => Some(v.as_slice()),
                    _ => None,
                };
                (rid.as_str(), loaded.version_of(rid), v)
            }),
        )
    }

    fn maybe_snapshot(&self, state: &mut HnswIndexState, oid: u32) {
        if state.dirty >= SNAPSHOT_DIRTY_THRESHOLD
            && let Some(dir) = self.snapshot_dir.read().clone()
            && let Err(e) = state.save_snapshot(&dir, &Self::snapshot_base(oid))
        {
            // Snapshot failure is never fatal — §6.1: the sidecar is purely
            // an optimization over rebuild-from-rows.
            tracing::warn!(oid, error = %e, "hnsw: snapshot save failed");
        }
    }
}

impl Drop for AnnRuntime {
    /// Graceful-shutdown snapshot (§6.1): persist every dirty index so an
    /// orderly process exit loses no build work. Best-effort by design.
    fn drop(&mut self) {
        let Some(dir) = self.snapshot_dir.get_mut().clone() else {
            return;
        };
        for (oid, slot) in self.slots.get_mut().drain() {
            let mut state = slot.lock();
            if state.dirty > 0
                && let Err(e) = state.save_snapshot(&dir, &Self::snapshot_base(oid))
            {
                tracing::warn!(oid, error = %e, "hnsw: shutdown snapshot failed");
            }
        }
    }
}
