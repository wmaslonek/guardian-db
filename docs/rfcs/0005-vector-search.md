# Guardian Vector Search — ANN Indexing, Auto-Embedding, and RAG

**Status:** implemented. Feature `vector-index` (+ `embedding` / `embedding-onnx` /
`rag`).
**Scope:** the `sql` feature family; `compute-nn` / `compute-llm` integration for
embedding delegation and RAG.
**Relates to:** [RFC 0002](0002-guardian-compute.md) (Guardian Compute),
[RFC 0003](0003-guardian-compute-followups.md) (blob-backed NN registry),
[RFC 0004](0004-compute-llm.md) (LLM layer) — whose motivation names "RAG over
GuardianDB stores", delivered here.
**Upstream/prior art:** [pgvector](https://github.com/pgvector/pgvector) (the SQL
surface), [hnsw_rs](https://crates.io/crates/hnsw_rs) (pure-Rust HNSW, adopted);
`instant-distance` (build-once, rejected — no incremental insert) and
[usearch](https://github.com/unum-cloud/usearch) (C++ FFI, rejected) were evaluated.

---

## 1. Overview

GuardianDB already had every *ingredient* of a vector search stack: the pgvector value
surface (`SqlType::Vector`, the four distance functions and the `<->` / `<#>` / `<=>` /
`<+>` operators in `src/sql/ext/vector.rs`), ONNX embedding models via `NnModelRegistry`
(RFC 0003), and generative inference via the LLM layer (RFC 0004). What was missing was
the connective tissue:

1. **An ANN index.** `ORDER BY embedding <-> $1 LIMIT k` worked as a full scan plus a
   full sort — O(n log n) per query, unusable beyond ~10⁵ rows.
2. **Auto-embedding.** Nothing connected "row inserted" → "embedding computed" →
   "vector column populated".
3. **A retrieval API for the LLM layer.** RAG had to be assembled by hand from three
   subsystems.

This RFC closes all three. The ANN index makes vector search real; auto-embedding and
RAG are where GuardianDB's P2P compute fabric offers what pgvector and its hosted
derivatives cannot: a weak node delegates embedding work to a GPU peer, and the whole
retrieve-augment-generate loop runs *next to the replicated data*.

## 2. Scope boundaries

- **No replicated index structure.** The HNSW graph is never gossiped, serialized into
  the oplog, or shared. Raw vectors replicate as ordinary row data; each node derives
  its own index locally — the exact model `SecondaryIndex` follows, and the reason index
  divergence between peers can never corrupt data.
- **No new store type.** A row with a vector column plus a local index covers the use
  case; a "VectorStore" would duplicate replication, ACL and event plumbing for no query
  power.
- **No `ivfflat`.** HNSW wins on recall/latency at every scale that fits in memory and
  needs no training step. The catalog's `method` field keeps the door open if a workload
  ever demands `ivfflat`.
- **No Guardian-specific query dialect.** The pgvector SQL surface *is* the API
  (`CREATE INDEX ... USING hnsw`, opclasses, `WITH (m, ef_construction)`, the
  `hnsw.ef_search` GUC), so Postgres+pgvector clients — every ORM and framework — work
  unchanged through pgwire.
- **No chunking/tokenization framework** above what a model intrinsically needs.
  Splitting documents into embeddable chunks is an application concern.

## 3. Architecture

```
src/relational/hnsw.rs   — HnswIndexState: distances, opclasses, options, snapshots
src/sql/
├── ann.rs               — AnnRuntime on Database (per-(table,column) graphs)
├── ddl.rs               — CREATE INDEX ... USING hnsw (validate_hnsw_index)
├── select.rs            — top-k planner hook (try_ann_scan)
├── engine.rs            — EXPLAIN (execute-and-report)
└── ext/vector.rs        — value-level surface (unchanged)
src/embedding/           — auto-embedding pipeline (feature `embedding`)
├── mod.rs                 Embedder trait + EmbeddingRegistry
├── openai.rs / onnx.rs    backends
├── rule.rs                EmbeddingRule + ExecutionPolicy
├── service.rs             change-feed-driven write-back
├── delegate.rs            P2P delegation (feature `compute`)
└── rag.rs                 retrieve → augment → generate (feature `rag`)
```

The three layers are gated independently: `vector-index = ["sql", "dep:hnsw_rs"]`,
`embedding = ["vector-index", "dep:reqwest"]` (with `embedding-onnx` and delegation as
opt-in extensions), and `rag = ["embedding", "compute-llm"]`.

## 4. HNSW ANN index

### 4.1 Index lifecycle

A per-`(table, column)` HNSW graph maintained by the engine alongside the table's
materialized view, exactly where `SecondaryIndex` sits, using **`hnsw_rs`** (pure Rust,
incremental insertion, pluggable distance — no FFI):

- **Lifecycle mirrors `SecondaryIndex`:** built on refresh from the materialized rows,
  updated incrementally on local writes, cleared and rebuilt when the engine's "refresh
  then operate" cycle detects divergence. Because the index is derived state, a rebuild
  is always *safe*, only *slow* — there is no migration or corruption scenario, and
  rebuild-from-rows is the correctness anchor everything else falls back to.
- **Deletes are tombstones.** HNSW has no true removal; deleted row ids go into a
  tombstone set consulted at query time (results over-fetched by the tombstone ratio),
  and a rebuild triggers when tombstones exceed a threshold (default 20% of indexed
  rows).
- **Opclasses and options, pgvector-compatible:**
  `CREATE INDEX ON items USING hnsw (embedding vector_cosine_ops) WITH (m = 16, ef_construction = 64)`,
  with `vector_l2_ops` / `vector_ip_ops` / `vector_cosine_ops` mapping to the distances
  `src/sql/ext/vector.rs` implements. Recall is tuned via the `hnsw.ef_search` GUC
  (default 40); `hnsw.ef_growth_cap` and `hnsw.selectivity_threshold` tune filtered
  search (§4.3).
- **Dimension cap 2000** for indexed columns (pgvector parity); unindexed vector columns
  keep the core type's limits.
- **Custom distance functions.** `anndists`' `DistDot`/`DistCosine` assert on
  unnormalized input, so all four distances are implemented locally. `hnsw_rs`
  additionally asserts distances are non-negative, so the inner-product ordering uses the
  strictly-monotone bounded map `π/2 − atan(dot)` — HNSW decisions are pure distance
  comparisons, so any strictly monotone transform yields identical graph behaviour.
  Cosine clamps at 0 (f32 rounding) and maps zero vectors to 2.0 (never NaN, which would
  silently corrupt neighbour selection).

### 4.2 DDL and planner hook

`validate_hnsw_index` (`src/sql/ddl.rs`) reads sqlparser's `USING` clause and validates
opclass/options; the catalog stores `Index.method = "hnsw"` plus `Index.opclasses` /
`Index.with_options` (serde-defaulted so existing persisted catalogs load unchanged).

On the read side, `try_ann_scan` (`src/sql/select.rs`) pattern-matches, *before* the
generic materialize-and-sort path:

> `ORDER BY <indexed-col> <dist-op> <query-vector> [ASC] LIMIT k` with a matching
> healthy index → HNSW top-k scan.

Anything else — no index, mismatched operator/opclass, `DESC`, missing `LIMIT` — falls
through to the exact path unchanged. Like pgvector, the indexed path is **approximate by
contract**: exactness is available by not creating the index, or via
`SET enable_indexscan = off`.

- **Candidate-set architecture.** The hook does *not* produce final results: it returns
  an ANN-ranked candidate `RowSet` as the statement's FROM input (the same shape as the
  existing `try_index_scan`), and the untouched WHERE / ORDER BY / LIMIT pipeline runs
  over it. NULL-vector rows are always appended (they sort last and fill under-full
  LIMITs, matching PostgreSQL). Approximation lives *only* in candidate selection.
- **Reconcile-on-use, not event wiring.** `AnnRuntime` reconciles each graph against the
  statement's freshly loaded table view: a commutative BLAKE3 fingerprint over
  `(row_id, version)` detects the no-change case in O(n); on change, a merge pass with
  per-row vector content hashes inserts/tombstones only what moved (an unrelated-column
  update does not churn the graph). Replicated writes from peers are picked up
  identically to local ones, because the loaded view is always storage-fresh.
- **DDL drop bookkeeping** is a single post-statement diff of hnsw index oids —
  DROP INDEX/TABLE/COLUMN and schema cascades all funnel through it; eager forgetting
  under a later-rolled-back transaction just costs a rebuild.
- **`<+>` is unreachable from SQL.** sqlparser 0.62 cannot tokenize the L1 operator
  (a pre-existing engine-wide limitation). `vector_l1_ops` remains valid DDL and
  `l1_distance()` works on the exact path; the planner serves `<->`, `<#>`, `<=>`.

### 4.3 Filtered search

`WHERE` combined with ANN ordering composes two strategies rather than picking one:

- **Selective-filter path.** When the residual `WHERE` clause can be answered by exact
  machinery (`SecondaryIndex` point/range lookups) and the candidate set is small
  (default ≤ 10 × `LIMIT` k, `hnsw.selectivity_threshold`), the engine skips ANN entirely
  and brute-forces exact distances over the filtered rows — cheaper *and* exact.
- **Broad-filter path.** Otherwise: HNSW top-k with post-filtering under **adaptive `ef`
  growth** — start at `hnsw.ef_search`, double until k survivors or the growth cap
  (default 10 × initial, `hnsw.ef_growth_cap`), then fall back to an exact scan rather
  than silently returning fewer than k rows.
- Selectivity is **measured**, never estimated: the exact-path candidate set is
  enumerated through the secondary index before the cutover decision — no statistics
  subsystem is invented. The chosen path is reported by `EXPLAIN`.

### 4.4 Persistence: sidecar snapshots

Rebuilding a 10⁶-row × 768-dim graph is minutes of CPU on every start, so the graph is
snapshotted to a **local sidecar file** next to the data directory (never replicated —
it is derived state), written atomically (temp file + rename) so a crash mid-write leaves
the previous snapshot intact.

- The payload is `hnsw_rs`'s own dump format; a postcard meta binds it with a **BLAKE3
  checksum** plus format version, opclass, options, dimension and row bookkeeping. On
  load, *any* mismatch — including a corrupt or truncated file — discards the snapshot
  and falls back to a full rebuild. The sidecar is purely an optimization and can be
  deleted at any time.
- **Cadence:** after every tombstone-triggered rebuild, at a dirty-threshold background
  cadence (default every 10k incremental inserts, so an unclean exit loses bounded work),
  and on `AnnRuntime` drop (graceful shutdown). Enabled via
  `Database::set_ann_snapshot_dir`.
- A reloaded snapshot may lag the rows; reconcile-on-use (§4.2) fixes the drift, so
  snapshot validity is internal consistency only. (`HnswIo` ties the reloaded graph's
  lifetime to its loader, which is deliberately leaked once per load — a few hundred
  bytes — to erase that lifetime.)

### 4.5 Determinism and robustness

- **Brute-force floor.** A corpus at or below `EXACT_ROW_FLOOR` live rows (2000, or the
  effective `ef` if larger) is answered by an exact linear scan: a scan over a few
  thousand vectors is sub-millisecond, so HNSW gives no speedup, while its approximate
  nature and small-graph edge cases (probabilistic layer assignment, or adversarial
  low-intrinsic-dimension data such as collinear points stranding a node) would cost
  recall and make results non-deterministic. Below the floor every answer is exact and
  stable — which is also why parallel vs. serial construction can never change
  small-table results. Above the floor, the growth loop measures exhaustion against the
  index's *live count*, never against `results < requested` (which an ef-bounded filtered
  traversal can legitimately under-fill).
- **Graph connectivity hardening.** The HNSW select-neighbours pruning heuristic can
  disconnect the graph on low-intrinsic-dimension data (points along an arc — observed as
  intermittent partial recall); both paper mitigations (`extend_candidates`,
  `keep_pruned`) are enabled at construction.
- **Parallel bulk build.** Initial build and post-tombstone rebuild take a rayon-parallel
  `parallel_insert` path (`build_bulk`), ~12× the serial rate; live single-row writes
  stay on the serial incremental path. `dirty` counters are saturating so the
  "always snapshot after a full build" sentinel cannot overflow.
- **Boundary ties are approximate** (pgvector parity): rows tied on distance exactly at
  the `LIMIT` boundary may resolve to either row on the ANN path; ties strictly inside
  the candidate set are ordered exactly by the normal sort (including tie-breaker keys).
- **`EXPLAIN` executes** (EXPLAIN ANALYZE semantics): the ANN decision — adaptive growth,
  measured selectivity — exists only at run time, so there is no static planner tree to
  print. Restricted to SELECT so it can never run DML. Reports `Ann Index Scan using ...
  (op, ef, candidates)`, the §4.3 cutover (`ann skipped`), the abandonment
  (`ann abandoned: ef ceiling`), or `Seq Scan`.

## 5. Auto-embedding pipeline

The embedding layer connects the change feed to the vector column. It is a top-level
`src/embedding/` module (feature `embedding`), deliberately *not* under `src/compute/` so
the local pipeline does not pull the heavy `compute`/wasmtime stack; delegation code is
gated on `feature = "compute"` within it.

The original design assumed embeddings would run as `Inference`-class tasks through
`NnModelRegistry`. That registry is **WASM-guest-oriented** — models run *inside* the
sandbox via wasi-nn `load_by_name`, with no host-side "text → vector" primitive — and
real embeddings need a tokenizer. So the pipeline uses a dedicated pluggable `Embedder`
layer (shaped like the RFC 0004 LLM layer, not the NN registry) driven off the SQL
engine's committed-change feed (`Database::subscribe_changes`), a cleaner trigger than
the compute `EventBus` or a SQL `CREATE TRIGGER`.

### 5.1 The `Embedder` layer (`mod.rs`)

- The `Embedder` trait (batch `embed`, `info`, `locality`) and an owner-curated
  `EmbeddingRegistry` (`name → Arc<dyn Embedder>`).
- `EmbedderInfo` / `ModelSelector` carry an optional BLAKE3 `content_hash` for pinning:
  `resolve` rejects a name match with the wrong or absent pinned hash as `HashMismatch`,
  never a name-only fallback.
- `HashEmbedder`, a deterministic unit-normalized embedder for tests/offline demos — it
  can prove its trivial "weights", so it exercises the pin path end to end.

### 5.2 Backends

- **`OpenAiEmbeddingBackend`** (`openai.rs`) fronts any OpenAI-compatible
  `/v1/embeddings` server (ollama, llama.cpp, LM Studio, OpenAI). No model files, so
  `content_hash: None` — a remote cannot prove its weights.
- **`OnnxEmbeddingBackend`** (`onnx.rs`, feature `embedding-onnx`) runs a local model
  with `ort` + a HuggingFace `tokenizers` tokenizer — the standard sentence-transformer
  pipeline (tokenize → transformer → attention-masked mean-pool → L2-normalize) — and
  hashes the model file, so it is the one backend with a verifiable identity.
  (`tokenizers` uses the `onig` C regex engine on native targets, so `embedding-onnx`
  needs a C toolchain; the pure-Rust `fancy-regex` path is wasm-only. The pooling/
  normalize math is unit-tested standalone; real-model inference needs a `model.onnx` +
  `tokenizer.json`.)

### 5.3 Rule, policy, and service

`EmbeddingRule` (`rule.rs`) binds `text_column → vector_column` on a table with a
`ModelSelector` and an `ExecutionPolicy` (`LocalOnly` | `Delegated { pin_hash,
allow_local_fallback }`, the default being hash-pinned with fallback).

The `EmbeddingService` (`service.rs`) subscribes to the change feed and, per matching
insert/update, embeds the text and writes the vector back via `Database::patch_row` — a
**direct storage write**, chosen because it emits no `ChangeEvent` (structurally breaking
the embed → write-back → re-embed loop) and needs neither the primary key nor a
constructed SQL statement.

- **Idempotency:** a sidecar column `_<vec>_srchash` stores the source-text BLAKE3 plus
  provenance (model name, model hash, executor `local`/`peer:<id>`) as JSON; an unchanged
  text hash skips re-embedding, so replays and refresh cycles are no-ops. Embedding lag is
  observable (the row exists before its vector); consumers needing completeness filter on
  `embedding IS NOT NULL`.
- The write-back re-reads the row so a concurrent update is not clobbered, and its
  generation bump invalidates the decoded-table cache. The subscription is registered
  synchronously in `spawn()` so a write issued right after cannot race the listener.

### 5.4 Remote delegation

Delegation to GPU peers is hash-pinned by default: a peer that cannot prove it holds the
exact weights is not eligible, and `HashMismatch` is a hard error — unpinned delegation
requires an explicit per-rule opt-out. The write-back records embedding provenance
(model name, hash, executing peer id) so a suspect corpus is auditable and selectively
re-embeddable, and `LocalOnly` keeps a privacy-sensitive corpus on the node even when
delegation is available.

- `delegate.rs` (feature `compute`): the `EmbeddingDelegate` trait is the transport
  boundary; the service owns the policy (pin enforcement, local fallback, provenance).
  `ComputeEmbeddingDelegate` is the real transport — it picks a peer advertising the
  model in the gossip `CapabilityDirectory`, ranks candidates for `Inference`, and sends
  an `Embed` request over `COMPUTE_ALPN`, honoring the pin end to end.
- **Compute-protocol additions:** `EmbedRequest` / `EmbedReply` + `ComputeRequest::Embed`,
  appended after `Generate` (same postcard forward-compat discipline — a peer without the
  `embedding` feature answers the unknown variant with a clean rejection). The wire types
  are self-contained, so the protocol layer does not depend on the embedding feature —
  only the executor handler (`serve_embed`) and `ComputeClient::embed_on` do.
  `set_embed_registry` opts the node into `Inference` admission and advertises its models;
  `CapabilityVector` gained `embed_models` (always on the wire, empty without a registry),
  bumping the gossip topic to `.../capabilities/4`.

### 5.5 SQL declaration surface

Both a Rust owner API and a SQL surface lower to the same rule engine. `sqlparser` cannot
parse string-literal arguments in `CREATE TRIGGER … EXECUTE FUNCTION f('a','b')` (it
models trigger args as `CREATE FUNCTION`-style type declarations), but it parses a plain
function call cleanly, so the SQL surface is a pair of engine-recognized calls,
intercepted in `Statement::Query` dispatch:

```sql
SELECT guardian_embed('docs', 'body', 'embedding', 'model' [, 'local'|'delegated']);
SELECT guardian_unembed('docs', 'embedding');
```

- The rule is validated (table/columns exist, target is a dimensioned `vector`, the
  `_<col>_srchash` sidecar exists, policy is `local`/`delegated`) and stored as an
  `EmbeddingRuleDef` in the **catalog**, so it persists and replicates like any DDL and
  introspects via `Catalog::embedding_rules`. Identity is `(schema, table,
  vector_column)`; re-declaring replaces (last write wins). `EmbeddingRuleDef` lives in
  the relational core (a dependency-free mirror of `EmbeddingRule`); the service converts
  it.
- The `EmbeddingService` merges its Rust-API rules with the catalog rules, re-reading the
  catalog on a timer (`with_catalog_refresh`, default 5 s), so a rule declared after
  startup takes effect and a dropped rule stops applying. Both front doors lower to the
  same `process_row` path — one engine, no behavioral divergence.

## 6. RAG helper

`src/embedding/rag.rs` (feature `rag = ["embedding", "compute-llm"]`) ties phases 4–5 to
the RFC 0004 LLM layer with a thin `retrieve` / `answer` pair.

- **`Rag::new` enforces query/corpus model consistency.** It reads the corpus table's
  declared embedding rule (§5.5) for the vector column and refuses a query embedder whose
  model name differs (`RagError::ModelMismatch`): embedding the query with a different
  model than the corpus is the classic silent RAG failure (the vectors live in unrelated
  spaces). When no rule is declared the corpus was embedded out of band — consistency is
  unverifiable, so it warns and trusts the caller. The model is *checked, never assumed*.
- **`retrieve`** embeds the query and runs `SELECT … ORDER BY <col> <op> <qvec> LIMIT k`
  — the exact shape §4.2 accelerates with an HNSW index (an exact scan otherwise) —
  returning `Retrieved { id?, text, distance }`. The operator is restricted to `<=>`
  (cosine, default) and `<->`, the two `sqlparser` can tokenize.
- **`answer`** retrieves, assembles a character-budgeted, optionally id-prefixed context
  block, substitutes `{context}` / `{question}` into a caller-supplied prompt template
  (with a grounded, refusal-friendly default), and streams through the RFC 0004
  `LlmRouter` (local → peers → distributed); `answer_text` drains the stream to a
  `String`.
- Agents, memory, re-ranking and chunking live above this crate, by design.

## 7. Integration points

- **Catalog:** `Index.method` stores `"hnsw"`; opclass and `WITH` options join the entry
  (serde-defaulted). `pg_indexes` and the catalog views report the method string as-is —
  pgvector-compatible introspection for free.
- **Extension gate:** the ANN path activates only when `CREATE EXTENSION vector` has run
  (the same check the operators make); `CREATE INDEX … USING hnsw` without the extension
  fails with a typed error naming `pg_available_extensions`.
- **pgwire / Supabase:** nothing to do — both sit on the SQL engine and inherit the
  planner hook; ORMs targeting pgvector work unchanged.
- **Compute admission:** embedding delegation is ordinary `Inference`-class work;
  admission, ledger and failover are RFC 0002/0003 machinery, untouched.

## 8. Benchmark (`examples/vector_index_bench.rs`)

Three parts: the index core (A), end-to-end SQL (B), and the auto-embedding pipeline (C).
Measured on 100k × 384-d vectors, a realistic low-intrinsic-dimension corpus (rank-24,
the structure of transformer embeddings; full-rank uniform noise is the
curse-of-dimensionality worst case and not representative, quantified separately via
`--tune`):

| Metric | Result |
|---|---|
| **Search** (k=10, ef=40, default params) | **~3 ms p50, recall@10 ≈ 0.95** — pgvector parity |
| Search ef=100 / ef=200 | recall ≈ 0.98, ~5.6 / ~8.9 ms p50 |
| **Build** (parallel bulk) | ~3300 rows/s (30 s for 100k), ~12× the serial rate |
| Insert (serial incremental, live writes) | ~2100 rows/s |
| **Snapshot** | save ~0.7 s, load ~1.2 s, ~225 MiB → **~25× faster start** than rebuild |

The `(m, ef_construction)` sweep (`--tune`) confirmed pgvector's defaults (m=16,
ef_construction=64) reach recall ≈ 0.94 at ef_search=40 on realistic data; higher
parameters trade build time for marginal recall and are exposed but not defaulted.

End-to-end through SQL, the ANN vector stage is below timing noise: the per-statement
table reload+decode dominated query latency at the time of this measurement. That floor
was subsequently removed by the decoded-table cache (see the CHANGELOG), which keys a
decoded `LoadedTable` by a per-collection generation counter; the snapshot already avoided
the cold first-query build (~15 s for 50k inside the statement) on restart (~1.5 s, a
~10× warm-start win).

**Part C — auto-embedding pipeline (8000 × 384-d, deterministic `HashEmbedder` so the
numbers isolate the *plumbing* from model inference):**

| Metric | Result |
|---|---|
| **Pipeline throughput** (change-feed → embed → write-back) | **~28k rows/s** |
| Write-back primitive (`patch_row`, storage merge+put+gen-bump) | ~390k rows/s |
| Embed primitive (HashEmbedder, batched) | ~52k rows/s |
| Idempotent re-touch (UPDATE, unchanged text) | provenance byte-identical — the loop terminates |

The pipeline sustains ~28k rows/s of *plumbing*, so with a real HTTP/ONNX embedder
(100–1000× slower per call) the effective rate is model-bound, not pipeline-bound. Two
measurement notes: the burst is seeded with a single multi-row `INSERT` (one statement, N
change events) to avoid the O(n²) per-statement insert cost; and drain completion is
detected via the O(1) storage change counter rather than a `count(*)` poll. The write-back
going straight to storage (not through a SQL `UPDATE`) is what makes it ~390k rows/s and,
by emitting no `ChangeEvent`, structurally prevents the embed → write → re-embed loop.
Batching the per-event embed call to amortize HTTP/ONNX overhead is a documented follow-up
knob (the service is per-event today).
