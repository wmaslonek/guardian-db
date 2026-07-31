//! Engine-native HNSW ANN index for `vector` columns (RFC 0005 phase 1).
//!
//! The ANN counterpart to [`crate::relational::index::SecondaryIndex`]: a
//! per-`(table, column)` HNSW graph that the executor consults for
//! `ORDER BY <col> <dist-op> <query> LIMIT k` top-k scans. Like every index
//! in this engine it is **local derived state** — the graph is never
//! replicated, never written to the oplog, and can always be rebuilt from the
//! replicated rows; a rebuild is always safe, only slow (RFC 0005 §2).
//!
//! Unlike `SecondaryIndex`, an HNSW graph is far too expensive to rebuild per
//! statement, so instances live across statements in the engine-level
//! [`crate::sql::ann::AnnRuntime`] and are *reconciled* against each
//! statement's freshly loaded table view by row version (see
//! [`HnswIndexState::reconcile_entry`]). Deletes and updates tombstone the
//! old graph node (HNSW supports no true removal); a rebuild triggers when
//! tombstones exceed [`HnswOptions::REBUILD_TOMBSTONE_RATIO`] of the graph.
//!
//! Distance functions are implemented here rather than borrowed from
//! `anndists` because the upstream `DistDot`/`DistCosine` implementations
//! `assert!` on unnormalized input (`1 - dot >= 0`), which panics on
//! arbitrary vectors. Semantics follow the pgvector opclasses: ordering by
//! the index distance must be monotonic with the ordering of the
//! corresponding SQL operator (`<->`, `<#>`, `<=>`, `<+>`); the final
//! ordering presented to the user is always recomputed exactly by the SQL
//! layer over the candidate set, so only monotonicity matters here.

use hnsw_rs::api::AnnT;
use hnsw_rs::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// pgvector caps indexed vectors at 2000 dimensions; mirror it.
pub const MAX_INDEXED_DIMS: u32 = 2000;

/// Snapshot format version (§6.1): bumped on any incompatible change to
/// [`SnapshotMeta`] or on an incompatible `hnsw_rs` upgrade. An old version
/// is never migrated — the index simply rebuilds.
const SNAPSHOT_FORMAT_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Distances (pgvector-opclass semantics, assert-free)
// ---------------------------------------------------------------------------

/// Squared L2. Monotonic with `l2_distance` (`<->`); the sqrt is skipped
/// because only ordering matters inside the graph.
#[derive(Default, Clone, Copy)]
pub struct DistVecL2Sq;
impl Distance<f32> for DistVecL2Sq {
    fn eval(&self, a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| {
                let d = x - y;
                d * d
            })
            .sum()
    }
}

/// Inner-product ordering (`<#>` = negative inner product), mapped through
/// `π/2 − atan(dot)` into `(0, π)`.
///
/// `hnsw_rs` asserts every distance is non-negative (`hnsw.rs:965`), so the
/// raw `-dot` (which pgvector uses internally) is not admissible. The atan
/// map is strictly decreasing in `dot`, bounded, and overflow-free — and
/// because every decision HNSW makes is a *comparison between two distances
/// produced by the same function*, any strictly monotone transform yields
/// exactly the same graph and search behaviour as `-dot` would (up to f32
/// resolution, which atan preserves at the same relative precision as the
/// dot product itself: both value and derivative decay as `1/x`).
#[derive(Default, Clone, Copy)]
pub struct DistVecNegDot;
impl Distance<f32> for DistVecNegDot {
    fn eval(&self, a: &[f32], b: &[f32]) -> f32 {
        let dot = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum::<f32>();
        std::f32::consts::FRAC_PI_2 - dot.atan()
    }
}

/// Cosine distance (`1 - cos`), monotonic with `<=>`. A zero vector has no
/// defined cosine distance (the SQL layer yields `NaN`, which sorts last);
/// inside the graph it maps to `2.0` — the maximum real cosine distance — so
/// graph construction never sees `NaN` (comparisons against `NaN` are always
/// false and would silently corrupt neighbour selection).
#[derive(Default, Clone, Copy)]
pub struct DistVecCos;
impl Distance<f32> for DistVecCos {
    fn eval(&self, a: &[f32], b: &[f32]) -> f32 {
        let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
        for (x, y) in a.iter().zip(b.iter()) {
            dot += x * y;
            na += x * x;
            nb += y * y;
        }
        if na == 0.0 || nb == 0.0 {
            return 2.0;
        }
        // Clamp: f32 rounding can push `dot / (|a||b|)` past 1.0 for
        // near-identical vectors, and hnsw_rs asserts distances are >= 0.
        (1.0 - dot / (na.sqrt() * nb.sqrt())).max(0.0)
    }
}

/// L1 (taxicab), monotonic with `<+>`.
#[derive(Default, Clone, Copy)]
pub struct DistVecL1;
impl Distance<f32> for DistVecL1 {
    fn eval(&self, a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum()
    }
}

// ---------------------------------------------------------------------------
// Opclasses and options
// ---------------------------------------------------------------------------

/// The pgvector HNSW operator classes. Each index serves exactly the SQL
/// distance operator of its opclass, like pgvector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VectorOpClass {
    /// `vector_l2_ops` — serves `<->`.
    L2,
    /// `vector_ip_ops` — serves `<#>`.
    InnerProduct,
    /// `vector_cosine_ops` — serves `<=>`.
    Cosine,
    /// `vector_l1_ops` — serves `<+>`.
    L1,
}

impl VectorOpClass {
    /// Parse a `CREATE INDEX` opclass name. `None` for unknown names.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "vector_l2_ops" => Some(Self::L2),
            "vector_ip_ops" => Some(Self::InnerProduct),
            "vector_cosine_ops" => Some(Self::Cosine),
            "vector_l1_ops" => Some(Self::L1),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::L2 => "vector_l2_ops",
            Self::InnerProduct => "vector_ip_ops",
            Self::Cosine => "vector_cosine_ops",
            Self::L1 => "vector_l1_ops",
        }
    }

    /// The SQL distance operator this opclass indexes.
    pub fn distance_op(&self) -> &'static str {
        match self {
            Self::L2 => "<->",
            Self::InnerProduct => "<#>",
            Self::Cosine => "<=>",
            Self::L1 => "<+>",
        }
    }

    /// The opclass serving a SQL distance operator.
    pub fn for_operator(op: &str) -> Option<Self> {
        match op {
            "<->" => Some(Self::L2),
            "<#>" => Some(Self::InnerProduct),
            "<=>" => Some(Self::Cosine),
            "<+>" => Some(Self::L1),
            _ => None,
        }
    }
}

/// HNSW build parameters (`WITH (m = ..., ef_construction = ...)`), pgvector
/// defaults and validation ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HnswOptions {
    /// Max connections per node per layer (pgvector `m`, default 16, 2..=100).
    pub m: u32,
    /// Size of the candidate list during construction (pgvector
    /// `ef_construction`, default 64, 4..=1000, must be `>= 2 * m`).
    pub ef_construction: u32,
}

impl Default for HnswOptions {
    fn default() -> Self {
        Self {
            m: 16,
            ef_construction: 64,
        }
    }
}

impl HnswOptions {
    /// Tombstone fraction beyond which the next reconcile rebuilds the graph.
    pub const REBUILD_TOMBSTONE_RATIO: f64 = 0.2;

    /// Validate pgvector's documented ranges. Returns a human-readable error.
    pub fn validate(&self) -> Result<(), String> {
        if !(2..=100).contains(&self.m) {
            return Err(format!(
                "value {} out of bounds for option \"m\" (2..100)",
                self.m
            ));
        }
        if !(4..=1000).contains(&self.ef_construction) {
            return Err(format!(
                "value {} out of bounds for option \"ef_construction\" (4..1000)",
                self.ef_construction
            ));
        }
        if self.ef_construction < 2 * self.m {
            return Err("ef_construction must be greater than or equal to 2 * m".into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The graph (one enum arm per opclass distance)
// ---------------------------------------------------------------------------

/// Hint passed to `Hnsw::new` for table pre-allocation; the graph grows past
/// it freely.
const CAPACITY_HINT: usize = 16 * 1024;
/// HNSW layer count (the crate caps at its own `NB_LAYER_MAX = 16`).
const MAX_LAYER: usize = 16;

enum AnnGraph {
    L2(Hnsw<'static, f32, DistVecL2Sq>),
    Ip(Hnsw<'static, f32, DistVecNegDot>),
    Cos(Hnsw<'static, f32, DistVecCos>),
    L1(Hnsw<'static, f32, DistVecL1>),
}

macro_rules! on_graph {
    ($self:expr, $g:ident => $body:expr) => {
        match $self {
            AnnGraph::L2($g) => $body,
            AnnGraph::Ip($g) => $body,
            AnnGraph::Cos($g) => $body,
            AnnGraph::L1($g) => $body,
        }
    };
}

impl AnnGraph {
    fn new(opclass: VectorOpClass, opts: &HnswOptions) -> Self {
        let (m, efc) = (opts.m as usize, opts.ef_construction as usize);
        /// Construct with both connectivity-preserving heuristics enabled.
        ///
        /// The select-neighbours pruning heuristic can disconnect the graph
        /// on low-intrinsic-dimension data (points along a line/arc are the
        /// classic case): a stranded component makes searches return a
        /// fraction of the corpus regardless of `ef`. `extend_candidates`
        /// and `keep_pruned` are the two canonical HNSW-paper mitigations;
        /// the modest build-time cost is the right trade for an index that
        /// must serve arbitrary user data.
        fn build<D: Distance<f32> + Send + Sync>(
            m: usize,
            efc: usize,
            dist: D,
        ) -> Hnsw<'static, f32, D> {
            let mut g = Hnsw::new(m, CAPACITY_HINT, MAX_LAYER, efc, dist);
            g.set_extend_candidates(true);
            g.set_keeping_pruned(true);
            g
        }
        match opclass {
            VectorOpClass::L2 => Self::L2(build(m, efc, DistVecL2Sq)),
            VectorOpClass::InnerProduct => Self::Ip(build(m, efc, DistVecNegDot)),
            VectorOpClass::Cosine => Self::Cos(build(m, efc, DistVecCos)),
            VectorOpClass::L1 => Self::L1(build(m, efc, DistVecL1)),
        }
    }

    fn insert(&self, vector: &[f32], internal: usize) {
        on_graph!(self, g => g.insert_slice((vector, internal)))
    }

    /// Insert many points in one rayon-parallel batch. Benchmarked at ~15×
    /// the serial `insert` rate on 384-d data — the right path for the
    /// initial bulk build and for post-tombstone rebuilds, where every
    /// vector is known up front.
    fn parallel_insert(&self, batch: &Vec<(&[f32], usize)>) {
        on_graph!(self, g => g.parallel_insert_slice(batch))
    }

    fn search_filter(
        &self,
        query: &[f32],
        k: usize,
        ef: usize,
        filter: &dyn FilterT,
    ) -> Vec<Neighbour> {
        on_graph!(self, g => g.search_filter(query, k, ef, Some(filter)))
    }

    fn file_dump(&self, dir: &Path, basename: &str) -> std::io::Result<String> {
        on_graph!(self, g => g.file_dump(dir, basename)).map_err(std::io::Error::other)
    }

    /// Every stored point (tombstoned ones included) with its distance to
    /// `query`, by linear scan. Must not be called on an empty graph (the
    /// crate's point iterator unwraps the entry point).
    fn scan_all(&self, query: &[f32]) -> Vec<(usize, f32)> {
        fn scan<D: Distance<f32> + Send + Sync>(
            g: &Hnsw<'static, f32, D>,
            d: &D,
            q: &[f32],
        ) -> Vec<(usize, f32)> {
            g.get_point_indexation()
                .into_iter()
                .map(|p| (p.get_origin_id(), d.eval(q, p.get_v())))
                .collect()
        }
        match self {
            AnnGraph::L2(g) => scan(g, &DistVecL2Sq, query),
            AnnGraph::Ip(g) => scan(g, &DistVecNegDot, query),
            AnnGraph::Cos(g) => scan(g, &DistVecCos, query),
            AnnGraph::L1(g) => scan(g, &DistVecL1, query),
        }
    }
}

// ---------------------------------------------------------------------------
// Index state
// ---------------------------------------------------------------------------

/// Per-row bookkeeping: which graph node currently represents the row, at
/// which row version, and a cheap content hash of the indexed vector so an
/// unrelated column update (version bump, same vector) does not churn the
/// graph.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct RowEntry {
    internal: usize,
    version: i64,
    vhash: u64,
}

/// First 8 bytes of the BLAKE3 hash of the vector's raw f32 bytes.
fn vector_hash(v: &[f32]) -> u64 {
    let mut hasher = blake3::Hasher::new();
    for x in v {
        hasher.update(&x.to_le_bytes());
    }
    let mut out = [0u8; 8];
    out.copy_from_slice(&hasher.finalize().as_bytes()[..8]);
    u64::from_le_bytes(out)
}

/// One live HNSW index: the graph plus the row bookkeeping that keeps it
/// reconciled with the replicated rows.
pub struct HnswIndexState {
    pub opclass: VectorOpClass,
    pub options: HnswOptions,
    /// Vector dimension, fixed by the column's declared type at CREATE INDEX.
    pub dims: u32,
    graph: AnnGraph,
    /// Next internal (graph) id to assign. Internal ids are never reused —
    /// a replaced row gets a fresh node and the old one is tombstoned.
    next_internal: usize,
    by_row: HashMap<String, RowEntry>,
    by_internal: HashMap<usize, String>,
    tombstones: HashSet<usize>,
    /// Cheap change detector: a commutative fold over `(row_id, version)` of
    /// the last reconciled table view. When a statement's view folds to the
    /// same value, reconciliation is skipped entirely.
    fingerprint: u64,
    /// Inserts since the last snapshot (drives the dirty-threshold snapshot
    /// cadence, §6.1).
    pub dirty: usize,
}

impl HnswIndexState {
    pub fn new(opclass: VectorOpClass, options: HnswOptions, dims: u32) -> Self {
        Self {
            opclass,
            options,
            dims,
            graph: AnnGraph::new(opclass, &options),
            next_internal: 0,
            by_row: HashMap::new(),
            by_internal: HashMap::new(),
            tombstones: HashSet::new(),
            fingerprint: 0,
            dirty: 0,
        }
    }

    /// Live-row count at or below which search is always exact (see
    /// [`Self::search`]). A linear scan over this many vectors is
    /// sub-millisecond; below it, HNSW gives no speedup and only risks
    /// recall, so the floor buys determinism and exactness for free.
    pub const EXACT_ROW_FLOOR: usize = 2000;

    /// Rows currently represented by a live (non-tombstoned) graph node.
    pub fn live_count(&self) -> usize {
        self.by_row.len()
    }

    /// Build a fresh index from an entire row set in one parallel batch.
    ///
    /// The bulk counterpart to [`Self::reconcile_entry`], used for the
    /// initial build of an empty index and for post-tombstone rebuilds:
    /// both cases have every current vector in hand, and
    /// `parallel_insert` builds the graph ~15× faster than serial inserts
    /// (benchmarked, 384-d). Rows with a NULL/mismatched vector are skipped,
    /// exactly as [`Self::reconcile_entry`] would. The result carries no
    /// tombstones and a fresh internal-id space.
    pub fn build_bulk<'a, I, V>(
        opclass: VectorOpClass,
        options: HnswOptions,
        dims: u32,
        rows: I,
    ) -> Self
    where
        I: IntoIterator<Item = (&'a str, i64, Option<V>)>,
        V: AsRef<[f32]> + 'a,
    {
        let mut state = Self::new(opclass, options, dims);
        // Materialize the accepted rows and their internal ids first, then
        // hand the whole batch to the parallel inserter.
        let mut vectors: Vec<Vec<f32>> = Vec::new();
        for (rid, version, vector) in rows {
            let Some(v) = vector else { continue };
            let v = v.as_ref();
            if v.len() != dims as usize {
                tracing::warn!(
                    row_id = rid,
                    got = v.len(),
                    expected = dims,
                    "hnsw: row vector dimension mismatch; row excluded from index"
                );
                continue;
            }
            let internal = state.next_internal;
            state.next_internal += 1;
            state.by_row.insert(
                rid.to_string(),
                RowEntry {
                    internal,
                    version,
                    vhash: vector_hash(v),
                },
            );
            state.by_internal.insert(internal, rid.to_string());
            vectors.push(v.to_vec());
        }
        if !vectors.is_empty() {
            let batch: Vec<(&[f32], usize)> = vectors
                .iter()
                .enumerate()
                .map(|(i, v)| (v.as_slice(), i))
                .collect();
            state.graph.parallel_insert(&batch);
        }
        state.dirty = vectors.len();
        state
    }

    /// Commutative fingerprint of a table view; see [`Self::fingerprint`].
    pub fn view_fingerprint<'a>(rows: impl Iterator<Item = (&'a str, i64)>) -> u64 {
        let mut acc = 0u64;
        let mut n = 0u64;
        for (rid, version) in rows {
            let mut h = blake3::Hasher::new();
            h.update(rid.as_bytes());
            h.update(&version.to_le_bytes());
            let mut b = [0u8; 8];
            b.copy_from_slice(&h.finalize().as_bytes()[..8]);
            acc = acc.wrapping_add(u64::from_le_bytes(b));
            n = n.wrapping_add(1);
        }
        // `+1` keeps every real view fingerprint distinct from the initial
        // `0` ("never reconciled"), including the empty-table view.
        acc.wrapping_add(n.wrapping_mul(0x9e37_79b9_7f4a_7c15))
            .wrapping_add(1)
    }

    /// Whether the last reconciled view already matches `fp`.
    pub fn is_current(&self, fp: u64) -> bool {
        self.fingerprint == fp
    }

    pub fn set_fingerprint(&mut self, fp: u64) {
        self.fingerprint = fp;
    }

    /// Reconcile one row of the current table view into the graph.
    ///
    /// `vector` is `None` when the row's indexed column is SQL NULL (the row
    /// simply does not participate in the index, like pgvector). Call
    /// [`Self::retain_rows`] afterwards to tombstone rows that left the view.
    pub fn reconcile_entry(&mut self, row_id: &str, version: i64, vector: Option<&[f32]>) {
        let Some(vec) = vector else {
            // NULL vector: make sure any previous node for this row is gone.
            self.tombstone_row(row_id);
            return;
        };
        if vec.len() != self.dims as usize {
            // Defensive: a row that somehow violates the declared dimension is
            // excluded from the index rather than poisoning the graph.
            tracing::warn!(
                row_id,
                got = vec.len(),
                expected = self.dims,
                "hnsw: row vector dimension mismatch; row excluded from index"
            );
            self.tombstone_row(row_id);
            return;
        }
        let vhash = vector_hash(vec);
        if let Some(entry) = self.by_row.get_mut(row_id) {
            if entry.vhash == vhash {
                // Same vector (row version may have moved for other columns).
                entry.version = version;
                return;
            }
            // Vector changed: tombstone the old node, insert a fresh one.
            let old = entry.internal;
            self.by_internal.remove(&old);
            self.tombstones.insert(old);
            let internal = self.next_internal;
            self.next_internal += 1;
            *self.by_row.get_mut(row_id).unwrap() = RowEntry {
                internal,
                version,
                vhash,
            };
            self.by_internal.insert(internal, row_id.to_string());
            self.graph.insert(vec, internal);
            self.dirty = self.dirty.saturating_add(1);
            return;
        }
        let internal = self.next_internal;
        self.next_internal += 1;
        self.by_row.insert(
            row_id.to_string(),
            RowEntry {
                internal,
                version,
                vhash,
            },
        );
        self.by_internal.insert(internal, row_id.to_string());
        self.graph.insert(vec, internal);
        self.dirty = self.dirty.saturating_add(1);
    }

    /// Tombstone every indexed row not present in `live` (rows deleted since
    /// the last reconcile).
    pub fn retain_rows(&mut self, live: &HashSet<&str>) {
        let gone: Vec<String> = self
            .by_row
            .keys()
            .filter(|rid| !live.contains(rid.as_str()))
            .cloned()
            .collect();
        for rid in gone {
            self.tombstone_row(&rid);
        }
    }

    fn tombstone_row(&mut self, row_id: &str) {
        if let Some(entry) = self.by_row.remove(row_id) {
            self.by_internal.remove(&entry.internal);
            self.tombstones.insert(entry.internal);
        }
    }

    /// Whether tombstones have accumulated past the rebuild threshold.
    pub fn needs_rebuild(&self) -> bool {
        let total = self.by_row.len() + self.tombstones.len();
        total > 0
            && (self.tombstones.len() as f64 / total as f64) > HnswOptions::REBUILD_TOMBSTONE_RATIO
    }

    /// Top-k search. Returns `(row_id, index_distance)` ranked most similar
    /// first.
    ///
    /// **Brute-force floor:** a corpus at or below [`Self::EXACT_ROW_FLOOR`]
    /// live rows (or below the effective `ef`, whichever is larger) is
    /// answered by an exact linear scan instead of graph traversal. At that
    /// size HNSW offers no speedup — a linear scan over a few thousand
    /// vectors is sub-millisecond — while its approximate nature would cost
    /// recall, and its small-graph edge cases (probabilistic layer
    /// assignment, or adversarial low-intrinsic-dimension data such as
    /// collinear points, occasionally stranding a node) would make results
    /// non-deterministic. Below the floor every result is exact and stable;
    /// this is also why parallel vs. serial graph construction can never
    /// change small-table answers. pgvector likewise advises against
    /// indexing tiny tables.
    ///
    /// Above the floor: HNSW traversal with tombstoned nodes filtered
    /// *during* the walk (no over-fetching) — the filter only admits live
    /// internal ids.
    pub fn search(&self, query: &[f32], k: usize, ef: usize) -> Vec<(String, f32)> {
        if query.len() != self.dims as usize || self.by_row.is_empty() {
            return Vec::new();
        }
        let ef = ef.max(k);
        if self.by_row.len() <= ef.max(Self::EXACT_ROW_FLOOR) {
            let mut all: Vec<(String, f32)> = self
                .graph
                .scan_all(query)
                .into_iter()
                .filter(|(id, _)| !self.tombstones.contains(id))
                .filter_map(|(id, d)| self.by_internal.get(&id).map(|rid| (rid.clone(), d)))
                .collect();
            all.sort_by(|a, b| a.1.total_cmp(&b.1));
            all.truncate(k);
            return all;
        }
        let tombstones = &self.tombstones;
        let filter = move |id: &DataId| -> bool { !tombstones.contains(id) };
        self.graph
            .search_filter(query, k, ef, &filter)
            .into_iter()
            .filter_map(|n| {
                self.by_internal
                    .get(&n.d_id)
                    .map(|rid| (rid.clone(), n.distance))
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Sidecar snapshots (§6.1)
// ---------------------------------------------------------------------------

/// Everything needed to revalidate and reattach a dumped graph. The graph
/// bytes themselves are `hnsw_rs`'s own dump format in the two files
/// `<base>.hnsw.graph` / `<base>.hnsw.data`; the meta binds them with a
/// BLAKE3 checksum. Any mismatch — options, dims, checksum, format version —
/// discards the snapshot and rebuilds (§6.1: no migration code for derived
/// state, ever).
#[derive(Serialize, Deserialize)]
struct SnapshotMeta {
    format_version: u32,
    opclass: VectorOpClass,
    options: HnswOptions,
    dims: u32,
    next_internal: usize,
    rows: Vec<(String, RowEntry)>,
    tombstones: Vec<usize>,
    /// BLAKE3 of `<base>.hnsw.graph` followed by `<base>.hnsw.data`.
    graph_hash: [u8; 32],
}

fn hash_files(paths: &[PathBuf]) -> std::io::Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    for p in paths {
        let bytes = std::fs::read(p)?;
        hasher.update(&bytes);
    }
    Ok(*hasher.finalize().as_bytes())
}

impl HnswIndexState {
    /// Write a snapshot of this index under `dir` as `{base}.meta` +
    /// `{base}.hnsw.graph` + `{base}.hnsw.data`.
    ///
    /// Write order makes torn snapshots detectable rather than loadable:
    /// graph files first, then the meta (whose checksum binds them) via
    /// temp-file + rename. A crash between the two leaves an old meta whose
    /// checksum no longer matches — which the loader treats as "no snapshot".
    pub fn save_snapshot(&mut self, dir: &Path, base: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        // hnsw_rs writes `<basename>.hnsw.graph` / `<basename>.hnsw.data`
        // directly; dump under a temp basename, then move into place.
        let tmp_base = format!("{base}.tmp");
        self.graph.file_dump(dir, &tmp_base)?;
        let graph_tmp = dir.join(format!("{tmp_base}.hnsw.graph"));
        let data_tmp = dir.join(format!("{tmp_base}.hnsw.data"));
        let graph_final = dir.join(format!("{base}.hnsw.graph"));
        let data_final = dir.join(format!("{base}.hnsw.data"));
        // Windows `rename` refuses to overwrite; remove stale targets first.
        // A crash in this window is caught by the checksum on load.
        let _ = std::fs::remove_file(&graph_final);
        let _ = std::fs::remove_file(&data_final);
        std::fs::rename(&graph_tmp, &graph_final)?;
        std::fs::rename(&data_tmp, &data_final)?;
        let graph_hash = hash_files(&[graph_final, data_final])?;
        let meta = SnapshotMeta {
            format_version: SNAPSHOT_FORMAT_VERSION,
            opclass: self.opclass,
            options: self.options,
            dims: self.dims,
            next_internal: self.next_internal,
            rows: self.by_row.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            tombstones: self.tombstones.iter().copied().collect(),
            graph_hash,
        };
        let bytes = postcard::to_stdvec(&meta).map_err(std::io::Error::other)?;
        let meta_tmp = dir.join(format!("{base}.meta.tmp"));
        let meta_final = dir.join(format!("{base}.meta"));
        std::fs::write(&meta_tmp, &bytes)?;
        let _ = std::fs::remove_file(&meta_final);
        std::fs::rename(&meta_tmp, &meta_final)?;
        self.dirty = 0;
        Ok(())
    }

    /// Try to load a snapshot for an index expected to have `opclass` /
    /// `options` / `dims`. Returns `None` — never an error — when anything
    /// is missing, corrupt, or mismatched: the caller falls back to a
    /// rebuild, which is always correct (§6.1).
    pub fn load_snapshot(
        dir: &Path,
        base: &str,
        opclass: VectorOpClass,
        options: HnswOptions,
        dims: u32,
    ) -> Option<Self> {
        let meta_path = dir.join(format!("{base}.meta"));
        let bytes = std::fs::read(&meta_path).ok()?;
        let meta: SnapshotMeta = postcard::from_bytes(&bytes).ok()?;
        if meta.format_version != SNAPSHOT_FORMAT_VERSION
            || meta.opclass != opclass
            || meta.options != options
            || meta.dims != dims
        {
            tracing::info!(base, "hnsw: snapshot metadata mismatch; rebuilding");
            return None;
        }
        let graph_final = dir.join(format!("{base}.hnsw.graph"));
        let data_final = dir.join(format!("{base}.hnsw.data"));
        let actual = hash_files(&[graph_final, data_final]).ok()?;
        if actual != meta.graph_hash {
            tracing::warn!(base, "hnsw: snapshot checksum mismatch; rebuilding");
            return None;
        }
        // `HnswIo::load_hnsw_with_dist` ties the returned graph's lifetime to
        // the loader borrow even though (without mmap) the graph owns its
        // data. The loader struct is deliberately leaked to erase that
        // lifetime: it is a few hundred bytes, leaked once per snapshot load
        // per process — bounded and intentional.
        let io: &'static HnswIo = Box::leak(Box::new(HnswIo::new(dir, base)));
        let graph = match opclass {
            VectorOpClass::L2 => io
                .load_hnsw_with_dist::<f32, DistVecL2Sq>(DistVecL2Sq)
                .ok()
                .map(AnnGraph::L2),
            VectorOpClass::InnerProduct => io
                .load_hnsw_with_dist::<f32, DistVecNegDot>(DistVecNegDot)
                .ok()
                .map(AnnGraph::Ip),
            VectorOpClass::Cosine => io
                .load_hnsw_with_dist::<f32, DistVecCos>(DistVecCos)
                .ok()
                .map(AnnGraph::Cos),
            VectorOpClass::L1 => io
                .load_hnsw_with_dist::<f32, DistVecL1>(DistVecL1)
                .ok()
                .map(AnnGraph::L1),
        }?;
        let by_row: HashMap<String, RowEntry> = meta.rows.into_iter().collect();
        let by_internal = by_row
            .iter()
            .map(|(rid, e)| (e.internal, rid.clone()))
            .collect();
        Some(Self {
            opclass,
            options,
            dims,
            graph,
            next_internal: meta.next_internal,
            by_row,
            by_internal,
            tombstones: meta.tombstones.into_iter().collect(),
            // Force a full reconcile on first use: the snapshot may lag the
            // replicated rows (that is expected and fine — reconcile fixes
            // the drift; the snapshot only has to be internally consistent).
            fingerprint: 0,
            dirty: 0,
        })
    }

    /// Remove this index's snapshot files (index dropped).
    pub fn remove_snapshot(dir: &Path, base: &str) {
        for suffix in [".meta", ".hnsw.graph", ".hnsw.data"] {
            let _ = std::fs::remove_file(dir.join(format!("{base}{suffix}")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(opclass: VectorOpClass) -> HnswIndexState {
        HnswIndexState::new(opclass, HnswOptions::default(), 3)
    }

    #[test]
    fn insert_and_search_l2() {
        let mut s = state(VectorOpClass::L2);
        s.reconcile_entry("a", 1, Some(&[0.0, 0.0, 0.0]));
        s.reconcile_entry("b", 1, Some(&[1.0, 0.0, 0.0]));
        s.reconcile_entry("c", 1, Some(&[10.0, 0.0, 0.0]));
        let got = s.search(&[0.9, 0.0, 0.0], 2, 40);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, "b");
        assert_eq!(got[1].0, "a");
    }

    #[test]
    fn build_bulk_matches_incremental() {
        let rows: Vec<(&str, i64, Option<Vec<f32>>)> = vec![
            ("a", 1, Some(vec![0.0, 0.0, 0.0])),
            ("b", 1, Some(vec![1.0, 0.0, 0.0])),
            ("nullrow", 1, None), // skipped, like reconcile_entry
            ("c", 1, Some(vec![10.0, 0.0, 0.0])),
        ];
        let s = HnswIndexState::build_bulk(
            VectorOpClass::L2,
            HnswOptions::default(),
            3,
            rows.iter().map(|(r, v, vec)| (*r, *v, vec.as_ref())),
        );
        assert_eq!(s.live_count(), 3, "null-vector row is not indexed");
        let got = s.search(&[0.9, 0.0, 0.0], 3, 40);
        assert_eq!(
            got.iter().map(|(r, _)| r.as_str()).collect::<Vec<_>>(),
            ["b", "a", "c"]
        );
        // A subsequent incremental update must not overflow `dirty` or churn.
        let mut s = s;
        s.reconcile_entry("b", 2, Some(&[1.0, 0.0, 0.0])); // same vector
        assert!(s.tombstones.is_empty());
    }

    /// Low-rank corpus (intrinsic dim `rank` lifted into `dims`): the
    /// realistic structure of transformer embeddings, unlike full-rank
    /// uniform noise (the curse-of-dimensionality worst case where no ANN
    /// can separate the true top-k). Deterministic from `seed`.
    fn lowrank(n: usize, dims: usize, rank: usize, seed: u64) -> Vec<Vec<f32>> {
        let mut st = seed | 1;
        let mut next = || {
            st ^= st << 13;
            st ^= st >> 7;
            st ^= st << 17;
            (st >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
        };
        let basis: Vec<Vec<f32>> = (0..rank)
            .map(|_| (0..dims).map(|_| next()).collect())
            .collect();
        (0..n)
            .map(|_| {
                let coords: Vec<f32> = (0..rank).map(|_| next()).collect();
                let mut v = vec![0f32; dims];
                for (r, b) in basis.iter().enumerate() {
                    for (d, bd) in b.iter().enumerate() {
                        v[d] += coords[r] * bd;
                    }
                }
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm > 0.0 {
                    for x in &mut v {
                        *x /= norm;
                    }
                }
                v
            })
            .collect()
    }

    #[test]
    fn recall_above_floor_on_lowrank_data() {
        // Genuine approximate graph search (corpus > EXACT_ROW_FLOOR) must
        // hit pgvector-parity recall on realistic low-intrinsic-dimension
        // data with the default parameters. This is the quantitative recall
        // guarantee; the SQL suite only checks the graph path is wired.
        let dims = 32;
        let n = HnswIndexState::EXACT_ROW_FLOOR + 1500;
        let corpus = lowrank(n, dims, 12, 42);
        let queries = lowrank(20, dims, 12, 7);
        let mut state = HnswIndexState::new(VectorOpClass::L2, HnswOptions::default(), dims as u32);
        for (i, v) in corpus.iter().enumerate() {
            state.reconcile_entry(&i.to_string(), 1, Some(v));
        }
        assert!(
            state.live_count() > HnswIndexState::EXACT_ROW_FLOOR,
            "must exceed the exact floor to exercise the graph"
        );
        let k = 10;
        let mut hits = 0usize;
        for q in &queries {
            // Exact truth by linear scan.
            let mut truth: Vec<(usize, f32)> = corpus
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    (
                        i,
                        v.iter().zip(q).map(|(a, b)| (a - b) * (a - b)).sum::<f32>(),
                    )
                })
                .collect();
            truth.sort_by(|a, b| a.1.total_cmp(&b.1));
            let truth: Vec<usize> = truth.into_iter().take(k).map(|(i, _)| i).collect();
            let got = state.search(q, k, 100);
            hits += got
                .iter()
                .filter(|(rid, _)| truth.contains(&rid.parse::<usize>().unwrap()))
                .count();
        }
        let recall = hits as f64 / (queries.len() * k) as f64;
        assert!(
            recall >= 0.90,
            "recall@10 too low on low-rank data: {recall:.3}"
        );
    }

    #[test]
    fn update_tombstones_old_node() {
        let mut s = state(VectorOpClass::L2);
        s.reconcile_entry("a", 1, Some(&[0.0, 0.0, 0.0]));
        s.reconcile_entry("a", 2, Some(&[5.0, 5.0, 5.0]));
        assert_eq!(s.live_count(), 1);
        let got = s.search(&[0.0, 0.0, 0.0], 2, 40);
        // Only the current node is visible; the stale position is filtered.
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "a");
    }

    #[test]
    fn unrelated_version_bump_does_not_churn() {
        let mut s = state(VectorOpClass::L2);
        s.reconcile_entry("a", 1, Some(&[1.0, 2.0, 3.0]));
        let dirty = s.dirty;
        s.reconcile_entry("a", 2, Some(&[1.0, 2.0, 3.0]));
        assert_eq!(s.dirty, dirty, "same vector must not re-insert");
        assert!(s.tombstones.is_empty());
    }

    #[test]
    fn delete_via_retain() {
        let mut s = state(VectorOpClass::L2);
        s.reconcile_entry("a", 1, Some(&[0.0, 0.0, 0.0]));
        s.reconcile_entry("b", 1, Some(&[1.0, 0.0, 0.0]));
        let live: HashSet<&str> = ["b"].into_iter().collect();
        s.retain_rows(&live);
        let got = s.search(&[0.0, 0.0, 0.0], 5, 40);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "b");
    }

    #[test]
    fn null_vector_rows_are_not_indexed() {
        let mut s = state(VectorOpClass::L2);
        s.reconcile_entry("a", 1, Some(&[0.0, 0.0, 0.0]));
        s.reconcile_entry("b", 1, None);
        assert_eq!(s.live_count(), 1);
    }

    #[test]
    fn cosine_zero_vector_is_max_distance_not_nan() {
        let d = DistVecCos.eval(&[0.0, 0.0], &[1.0, 0.0]);
        assert_eq!(d, 2.0);
    }

    #[test]
    fn negdot_orders_like_negative_inner_product() {
        let mut s = state(VectorOpClass::InnerProduct);
        s.reconcile_entry("small", 1, Some(&[1.0, 0.0, 0.0]));
        s.reconcile_entry("big", 1, Some(&[100.0, 0.0, 0.0]));
        let got = s.search(&[1.0, 0.0, 0.0], 2, 40);
        assert_eq!(got[0].0, "big", "larger inner product ranks first");
    }

    #[test]
    fn options_validation_matches_pgvector() {
        assert!(
            HnswOptions {
                m: 16,
                ef_construction: 64
            }
            .validate()
            .is_ok()
        );
        assert!(
            HnswOptions {
                m: 1,
                ef_construction: 64
            }
            .validate()
            .is_err()
        );
        assert!(
            HnswOptions {
                m: 101,
                ef_construction: 640
            }
            .validate()
            .is_err()
        );
        assert!(
            HnswOptions {
                m: 16,
                ef_construction: 3
            }
            .validate()
            .is_err()
        );
        assert!(
            HnswOptions {
                m: 16,
                ef_construction: 2000
            }
            .validate()
            .is_err()
        );
        // ef_construction < 2*m
        assert!(
            HnswOptions {
                m: 40,
                ef_construction: 64
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn rebuild_threshold() {
        let mut s = state(VectorOpClass::L2);
        for i in 0..10 {
            s.reconcile_entry(&format!("r{i}"), 1, Some(&[i as f32, 0.0, 0.0]));
        }
        assert!(!s.needs_rebuild());
        // Delete 3 of 10 → 3/13 total slots tombstoned > 20%.
        let live: HashSet<&str> = ["r0", "r1", "r2", "r3", "r4", "r5", "r6"]
            .into_iter()
            .collect();
        s.retain_rows(&live);
        assert!(s.needs_rebuild());
    }

    #[test]
    fn snapshot_roundtrip() {
        let dir = std::env::temp_dir().join(format!("gdb-hnsw-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = state(VectorOpClass::Cosine);
        s.reconcile_entry("a", 1, Some(&[1.0, 0.0, 0.0]));
        s.reconcile_entry("b", 3, Some(&[0.0, 1.0, 0.0]));
        s.save_snapshot(&dir, "idx-42").unwrap();
        let loaded = HnswIndexState::load_snapshot(
            &dir,
            "idx-42",
            VectorOpClass::Cosine,
            HnswOptions::default(),
            3,
        )
        .expect("snapshot should load");
        assert_eq!(loaded.live_count(), 2);
        let got = loaded.search(&[1.0, 0.1, 0.0], 1, 40);
        assert_eq!(got[0].0, "a");
        // Mismatched expectations refuse the snapshot.
        assert!(
            HnswIndexState::load_snapshot(
                &dir,
                "idx-42",
                VectorOpClass::L2,
                HnswOptions::default(),
                3
            )
            .is_none()
        );
        // A corrupted graph file fails the checksum.
        let gpath = dir.join("idx-42.hnsw.data");
        let mut bytes = std::fs::read(&gpath).unwrap();
        if let Some(b) = bytes.last_mut() {
            *b = b.wrapping_add(1);
        }
        std::fs::write(&gpath, &bytes).unwrap();
        assert!(
            HnswIndexState::load_snapshot(
                &dir,
                "idx-42",
                VectorOpClass::Cosine,
                HnswOptions::default(),
                3
            )
            .is_none()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
