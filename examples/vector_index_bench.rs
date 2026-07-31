//! Benchmark for the engine-native HNSW vector index + auto-embedding
//! pipeline (RFC 0005 phases 1–2).
//!
//! Three layers, measured separately on purpose:
//!
//! - **Part A — index core** (`HnswIndexState`, no SQL): graph build rate,
//!   search latency and recall@10 at several `ef`, snapshot save/load. This
//!   is where the RFC's value milestone ("top-k in milliseconds at large n")
//!   lives.
//! - **Part B — end-to-end SQL**: the same corpus queried through the full
//!   statement pipeline. The engine reloads and decodes the whole table view
//!   per statement (its local-first "refresh then operate" model), so this
//!   part attributes cost between the ANN scan itself and the general
//!   statement overhead — measured honestly via a `count(*)` baseline that
//!   pays the pipeline without any vector work.
//! - **Part C — auto-embedding pipeline** (feature `embedding`): change-feed →
//!   embed → write-back throughput, with the deterministic `HashEmbedder` so
//!   the numbers isolate the pipeline plumbing from model inference.
//!
//! Usage:
//!   cargo run --release --features embedding --example vector_index_bench
//!   cargo run --release --features embedding --example vector_index_bench -- \
//!       <core_rows> <dims> <sql_rows> <embed_rows>   (defaults: 100000 384 50000 5000)
//!
//! With only `--features vector-index`, Parts A+B run and Part C is skipped.

use guardian_db::relational::hnsw::{HnswIndexState, HnswOptions, VectorOpClass};
use guardian_db::sql::engine::{Database, Session};
use guardian_db::sql::{ExecResult, MemoryStorage};
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--tune") {
        let rows: usize = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(20_000);
        let dims: usize = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(384);
        tune(rows, dims);
        return;
    }
    let core_rows: usize = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(100_000);
    let dims: usize = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(384);
    let sql_rows: usize = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(50_000);
    let embed_rows: usize = args.get(4).and_then(|a| a.parse().ok()).unwrap_or(5_000);

    println!("== vector-index benchmark (RFC 0005 phases 1–2) ==");
    println!(
        "core rows: {core_rows}   dims: {dims}   sql rows: {sql_rows}   embed rows: {embed_rows}\n"
    );

    part_a_core(core_rows, dims);

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async move {
            part_b_sql(sql_rows, dims).await;
            #[cfg(feature = "embedding")]
            part_c_embedding(embed_rows, dims).await;
            #[cfg(not(feature = "embedding"))]
            {
                let _ = embed_rows;
                println!(
                    "\n(Part C — auto-embedding pipeline — skipped; \
                     rebuild with --features embedding to include it)"
                );
            }
        });
}

// ---------------------------------------------------------------------------
// --tune: compare hnsw_rs construction-heuristic configs directly.
// ---------------------------------------------------------------------------

/// Low-rank corpus: `rank` latent gaussian coords lifted into `dims` by a
/// fixed random basis, then L2-normalized. This is the realistic structure
/// of transformer embeddings — nominal dimension 384 but intrinsic
/// dimension ~`rank` (typically 10-30 for sentence encoders) — as opposed
/// to `gen_vectors`, whose full-rank uniform noise is the
/// curse-of-dimensionality worst case where ANN (any ANN) cannot separate
/// the true top-k from thousands of near-ties.
fn gen_lowrank(n: usize, dims: usize, rank: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut rng = fastrand::Rng::with_seed(seed);
    let gauss = |rng: &mut fastrand::Rng| {
        let (u1, u2) = (rng.f32().max(1e-7), rng.f32());
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f32::consts::PI * u2).cos()
    };
    // Fixed basis: `rank` directions in `dims` space.
    let basis: Vec<Vec<f32>> = (0..rank)
        .map(|_| (0..dims).map(|_| gauss(&mut rng)).collect())
        .collect();
    (0..n)
        .map(|_| {
            let coords: Vec<f32> = (0..rank).map(|_| gauss(&mut rng)).collect();
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

fn tune(n: usize, dims: usize) {
    use guardian_db::relational::hnsw::DistVecL2Sq;
    use hnsw_rs::prelude::*;
    println!("== (m, ef_construction) tuning ({n} rows, {dims} dims) ==");
    println!("goal: recall@10 ≥ ~0.95 at ef_search 40-100, the pgvector expectation");
    println!("corpus: low-rank (intrinsic dim 24) — realistic embedding structure\n");
    let corpus = gen_lowrank(n, dims, 24, 42);
    let queries = gen_lowrank(50, dims, 24, 7);
    let truths: Vec<Vec<usize>> = queries.iter().map(|q| exact_topk(&corpus, q, 10)).collect();

    // (m, ef_construction): pgvector default is (16, 64); sweep upward.
    for (m, efc) in [(16usize, 64usize), (16, 200), (32, 200)] {
        // parallel_insert (rayon) — also measures the build-speed lever.
        let mut g: Hnsw<'static, f32, DistVecL2Sq> = Hnsw::new(m, n, 16, efc, DistVecL2Sq);
        g.set_extend_candidates(true);
        g.set_keeping_pruned(true);
        let t = Instant::now();
        let data: Vec<(&Vec<f32>, usize)> = corpus.iter().zip(0..).collect();
        g.parallel_insert(&data);
        let build = t.elapsed().as_secs_f64();
        for ef in [40usize, 100, 200] {
            let t = Instant::now();
            let mut hit = 0usize;
            for (q, truth) in queries.iter().zip(&truths) {
                let got = g.search(q, 10, ef);
                hit += got.iter().filter(|r| truth.contains(&r.d_id)).count();
            }
            let lat = t.elapsed().as_secs_f64() * 1000.0 / queries.len() as f64;
            println!(
                "m={m:<2} efc={efc:<3} build {build:6.1}s ({:>6.0}/s ‖)  ef_search={ef:<3} \
                 {lat:6.2} ms/q  recall@10 {:.3}",
                n as f64 / build,
                hit as f64 / (queries.len() * 10) as f64
            );
        }
        println!();
    }
}

// ---------------------------------------------------------------------------
// Corpus generation: deterministic, L2-normalized (realistic embeddings).
// ---------------------------------------------------------------------------

/// Full-rank uniform vectors — the curse-of-dimensionality worst case, kept
/// for reference / experimentation (not used by the default run, which uses
/// the realistic low-rank corpus).
#[allow(dead_code)]
fn gen_vectors(n: usize, dims: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut rng = fastrand::Rng::with_seed(seed);
    (0..n)
        .map(|_| {
            let mut v: Vec<f32> = (0..dims).map(|_| rng.f32() * 2.0 - 1.0).collect();
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

fn l2sq(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| {
            let d = x - y;
            d * d
        })
        .sum()
}

/// Exact top-k row indexes by linear scan (ground truth for recall).
fn exact_topk(corpus: &[Vec<f32>], query: &[f32], k: usize) -> Vec<usize> {
    let mut all: Vec<(usize, f32)> = corpus
        .iter()
        .enumerate()
        .map(|(i, v)| (i, l2sq(query, v)))
        .collect();
    all.sort_by(|a, b| a.1.total_cmp(&b.1));
    all.truncate(k);
    all.into_iter().map(|(i, _)| i).collect()
}

fn pcts(mut samples: Vec<f64>) -> (f64, f64) {
    samples.sort_by(|a, b| a.total_cmp(b));
    let p = |q: f64| samples[((samples.len() - 1) as f64 * q) as usize];
    (p(0.50), p(0.95))
}

// ---------------------------------------------------------------------------
// Part A — index core
// ---------------------------------------------------------------------------

fn part_a_core(n: usize, dims: usize) {
    println!("-- Part A: index core (HnswIndexState, no SQL) --");
    // Low-rank corpus (intrinsic dim 24): the realistic structure of
    // transformer embeddings. Full-rank uniform noise in 384-d is the
    // curse-of-dimensionality worst case and not representative — see the
    // --tune mode for that comparison.
    let corpus = gen_lowrank(n, dims, 24, 42);
    let queries = gen_lowrank(100, dims, 24, 7);

    // Bulk build — the production initial-build / rebuild path (parallel).
    let ids: Vec<String> = (0..n).map(|i| i.to_string()).collect();
    let t = Instant::now();
    let mut state = HnswIndexState::build_bulk(
        VectorOpClass::L2,
        HnswOptions::default(),
        dims as u32,
        ids.iter()
            .zip(&corpus)
            .map(|(id, v)| (id.as_str(), 1i64, Some(v))),
    );
    let build = t.elapsed();
    println!(
        "build (parallel bulk): {n} rows in {:.2}s  ({:.0} rows/s, m=16 efc=64)",
        build.as_secs_f64(),
        n as f64 / build.as_secs_f64()
    );

    // Incremental single-insert rate (the live-write path), sampled.
    let sample = 2000.min(n);
    let mut inc = HnswIndexState::new(VectorOpClass::L2, HnswOptions::default(), dims as u32);
    let t = Instant::now();
    for (i, v) in corpus.iter().take(sample).enumerate() {
        inc.reconcile_entry(&i.to_string(), 1, Some(v));
    }
    println!(
        "insert (serial incremental): {:.0} rows/s  ← live-write path",
        sample as f64 / t.elapsed().as_secs_f64()
    );

    // Search latency + recall at several ef.
    let k = 10;
    // Ground truth on a subset (linear scan is the expensive part here).
    let gt_queries = 25.min(queries.len());
    let truths: Vec<Vec<usize>> = queries[..gt_queries]
        .iter()
        .map(|q| exact_topk(&corpus, q, k))
        .collect();
    for ef in [40usize, 100, 200] {
        let mut lat = Vec::new();
        for q in &queries {
            let t = Instant::now();
            let _ = state.search(q, k, ef);
            lat.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let (p50, p95) = pcts(lat);
        let mut hit = 0usize;
        for (q, truth) in queries[..gt_queries].iter().zip(&truths) {
            let got = state.search(q, k, ef);
            hit += got
                .iter()
                .filter(|(rid, _)| truth.contains(&rid.parse::<usize>().unwrap()))
                .count();
        }
        let recall = hit as f64 / (gt_queries * k) as f64;
        println!(
            "search k={k} ef={ef:>3}: p50 {p50:.3} ms  p95 {p95:.3} ms  recall@10 {:.3}",
            recall
        );
    }

    // Snapshot save / load.
    let dir = std::env::temp_dir().join(format!("gdb-vec-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let t = Instant::now();
    state.save_snapshot(&dir, "bench").expect("snapshot save");
    let save = t.elapsed();
    let bytes: u64 = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok()?.metadata().ok())
        .map(|m| m.len())
        .sum();
    let t = Instant::now();
    let loaded = HnswIndexState::load_snapshot(
        &dir,
        "bench",
        VectorOpClass::L2,
        HnswOptions::default(),
        dims as u32,
    )
    .expect("snapshot load");
    let load = t.elapsed();
    println!(
        "snapshot: save {:.2}s  load {:.2}s  size {:.1} MiB  (vs rebuild {:.2}s → {:.1}x faster start)",
        save.as_secs_f64(),
        load.as_secs_f64(),
        bytes as f64 / (1024.0 * 1024.0),
        build.as_secs_f64(),
        build.as_secs_f64() / load.as_secs_f64()
    );
    drop(loaded);
    let _ = std::fs::remove_dir_all(&dir);
    println!();
}

// ---------------------------------------------------------------------------
// Part B — end-to-end SQL
// ---------------------------------------------------------------------------

async fn exec(s: &mut Session<MemoryStorage>, sql: &str) -> Vec<ExecResult> {
    s.execute(sql)
        .await
        .unwrap_or_else(|e| panic!("`{sql}` failed: {e}"))
}

async fn rows_of(s: &mut Session<MemoryStorage>, sql: &str) -> Vec<String> {
    match exec(s, sql).await.pop() {
        Some(ExecResult::Rows { rows, .. }) => rows
            .into_iter()
            .map(|r| r.first().and_then(|v| v.to_text()).unwrap_or_default())
            .collect(),
        other => panic!("`{sql}` did not produce rows: {other:?}"),
    }
}

fn vec_literal(v: &[f32]) -> String {
    let mut out = String::with_capacity(v.len() * 10);
    out.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("{x}"));
    }
    out.push(']');
    out
}

async fn timed_query(s: &mut Session<MemoryStorage>, sql: &str) -> f64 {
    let t = Instant::now();
    let _ = exec(s, sql).await;
    t.elapsed().as_secs_f64() * 1000.0
}

async fn part_b_sql(n: usize, dims: usize) {
    println!("-- Part B: end-to-end SQL ({n} rows through the statement pipeline) --");
    let corpus = gen_lowrank(n, dims, 24, 42);
    let queries = gen_lowrank(40, dims, 24, 7);
    let dir = std::env::temp_dir().join(format!("gdb-vec-bench-sql-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let storage = Arc::new(MemoryStorage::new());
    let database = Arc::new(Database::new(storage.clone(), "bench"));
    database.set_ann_snapshot_dir(&dir);
    let mut s = Session::new(database.clone(), "guardian");
    exec(&mut s, "CREATE EXTENSION vector").await;
    exec(
        &mut s,
        &format!("CREATE TABLE items (id int PRIMARY KEY, v vector({dims}))"),
    )
    .await;
    exec(&mut s, "CREATE INDEX ON items USING hnsw (v)").await;

    // Bulk-seed the storage directly, the same path replicated writes take.
    // (Statement-level INSERTs reload the whole table view each time —
    // O(n²) for a seed loop — so they would benchmark the DML pipeline, not
    // the index.)
    let oid = rows_of(
        &mut s,
        "SELECT oid FROM pg_class WHERE relname = 'items' AND relkind = 'r'",
    )
    .await
    .pop()
    .expect("table oid");
    let collection = format!("__gdb_sql_rows_{oid}");
    let t = Instant::now();
    for (i, v) in corpus.iter().enumerate() {
        let doc = serde_json::json!({
            "_id": i.to_string(),
            "__schema": "public",
            "__table": "items",
            "__version": 1,
            "__deleted": false,
            "id": i,
            "v": v,
        });
        guardian_db::relational::RelationalStorage::put(
            storage.as_ref(),
            &collection,
            &i.to_string(),
            &doc,
        )
        .await
        .expect("seed put");
    }
    println!(
        "seed: {n} docs in {:.2}s (direct storage puts)",
        t.elapsed().as_secs_f64()
    );

    let topk = |q: &[f32]| {
        format!(
            "SELECT id FROM items ORDER BY v <-> '{}'::vector LIMIT 10",
            vec_literal(q)
        )
    };

    // Statement-pipeline baseline: pays load_table + decode, no vector work.
    let mut base = Vec::new();
    for _ in 0..5 {
        base.push(timed_query(&mut s, "SELECT count(*) FROM items").await);
    }
    let (base_p50, _) = pcts(base);
    println!("pipeline baseline (count(*)): {base_p50:.0} ms  ← per-statement table load+decode");

    // Cold ANN query: includes the full graph build.
    let cold = timed_query(&mut s, &topk(&queries[0])).await;
    println!("ANN cold (first query, builds graph): {cold:.0} ms");

    // Warm ANN queries: fingerprint reconcile + search + exact re-sort.
    let mut warm = Vec::new();
    for q in &queries[1..31] {
        warm.push(timed_query(&mut s, &topk(q)).await);
    }
    let (p50, p95) = pcts(warm);
    let ann_share = (p50 - base_p50).max(0.0);
    println!(
        "ANN warm: p50 {p50:.0} ms  p95 {p95:.0} ms  (vector stage ≈{ann_share:.0} ms; \
         rest is the per-statement pipeline)"
    );

    // Exact seq scan for the same queries.
    exec(&mut s, "SET enable_indexscan = off").await;
    let mut ex = Vec::new();
    for q in &queries[1..6] {
        ex.push(timed_query(&mut s, &topk(q)).await);
    }
    exec(&mut s, "SET enable_indexscan = on").await;
    let (ex_p50, _) = pcts(ex);
    let exact_share = (ex_p50 - base_p50).max(0.0);
    if ann_share >= 1.0 {
        println!(
            "exact seq scan: p50 {ex_p50:.0} ms  (vector stage {exact_share:.0} ms vs \
             ANN {ann_share:.0} ms → {:.0}x on the vector stage)",
            exact_share / ann_share
        );
    } else {
        println!(
            "exact seq scan: p50 {ex_p50:.0} ms  (vector stage {exact_share:.0} ms; ANN's \
             share is below timing noise)"
        );
    }

    // Result sanity + plan proof.
    let plan = rows_of(&mut s, &format!("EXPLAIN {}", topk(&queries[1]))).await;
    println!("plan: {}", plan.first().map(String::as_str).unwrap_or("?"));

    // Incremental write visibility + timing. Insert a row whose vector
    // equals a query, then (a) time the ANN query and (b) confirm the row is
    // reconciled into the index. Visibility is verified through the exact
    // path (`enable_indexscan = off`), which scans every row and so always
    // ranks the new distance-0 row first — a deterministic check. The ANN
    // path is approximate: a single fresh node in a large graph can sit in
    // the recall tail even at high ef, so its top-1 is timed, not asserted.
    let newv = vec_literal(&queries[35]);
    exec(&mut s, &format!("INSERT INTO items VALUES ({n}, '{newv}')")).await;
    let inc = timed_query(&mut s, &topk(&queries[35])).await;
    exec(&mut s, "SET enable_indexscan = off").await;
    let exact_first = rows_of(&mut s, &topk(&queries[35])).await;
    exec(&mut s, "SET enable_indexscan = on").await;
    println!(
        "incremental: insert + ANN query {inc:.0} ms; row reconciled & visible \
         (exact-path nearest = {}, expected {n})",
        exact_first.first().map(String::as_str).unwrap_or("?")
    );

    // Snapshot restart: new Database over the same storage.
    drop(s);
    drop(database);
    let database2 = Arc::new(Database::new(storage, "bench"));
    database2.set_ann_snapshot_dir(&dir);
    let mut s2 = Session::new(database2, "guardian");
    let restart = timed_query(&mut s2, &topk(&queries[2])).await;
    println!("restart with snapshot: first query {restart:.0} ms (vs {cold:.0} ms cold build)");

    let _ = std::fs::remove_dir_all(&dir);
    println!();
}

// ---------------------------------------------------------------------------
// Part C — auto-embedding pipeline (feature `embedding`)
// ---------------------------------------------------------------------------

/// Measures the RFC 0005 phase-2 pipeline. The deterministic `HashEmbedder`
/// is near-free, so these numbers isolate the *plumbing* (change-feed
/// delivery, idempotency check, write-back) from model inference — with a
/// real HTTP/ONNX embedder the model dominates and the effective rate is
/// `min(plumbing, model_rate)`, so a high plumbing rate means the pipeline
/// adds little of its own.
#[cfg(feature = "embedding")]
async fn part_c_embedding(n: usize, dims: usize) {
    use guardian_db::embedding::{
        Embedder, EmbeddingRegistry, EmbeddingRule, EmbeddingService, HashEmbedder, ModelSelector,
    };

    println!("-- Part C: auto-embedding pipeline ({n} rows, {dims}-d) --");

    // Embed primitive: the (fast) model cost alone, batched.
    let embedder = HashEmbedder::new("bench-embed", dims as u32);
    let texts: Vec<String> = (0..n)
        .map(|i| format!("benchmark document number {i}"))
        .collect();
    let t = Instant::now();
    for chunk in texts.chunks(256) {
        let _ = embedder.embed(chunk).await.unwrap();
    }
    println!(
        "embed primitive (HashEmbedder, batched): {:.0} rows/s  ← a real model is 100–1000x slower",
        n as f64 / t.elapsed().as_secs_f64()
    );

    // Set up the corpus table + the service with a rule.
    let db = Arc::new(Database::new(Arc::new(MemoryStorage::new()), "app"));
    let mut s = Session::new(db.clone(), "guardian");
    exec(&mut s, "CREATE EXTENSION vector").await;
    exec(
        &mut s,
        &format!(
            "CREATE TABLE docs (id int PRIMARY KEY, body text, \
             embedding vector({dims}), _embedding_srchash text)"
        ),
    )
    .await;
    let registry = EmbeddingRegistry::new();
    registry.register(Arc::new(HashEmbedder::new("bench-embed", dims as u32)));
    let rule = EmbeddingRule::new(
        "docs",
        "body",
        "embedding",
        ModelSelector::by_name("bench-embed"),
    );
    let service = EmbeddingService::new(db.clone(), registry).with_rule(rule);
    let handle = service.spawn();

    // The storage collection backing the table — for an O(1) completion
    // probe (see below).
    let oid = rows_of(
        &mut s,
        "SELECT oid FROM pg_class WHERE relname = 'docs' AND relkind = 'r'",
    )
    .await
    .pop()
    .expect("table oid");
    let collection = format!("__gdb_sql_rows_{oid}");
    use guardian_db::relational::RelationalStorage;
    let gen0 = db.storage().generation(&collection).unwrap_or(0);

    // End-to-end pipeline throughput: one multi-row INSERT emits N change
    // events in a single statement (avoiding the O(n^2) per-statement insert
    // cost), then the service drains them.
    let mut values = String::with_capacity(n * 48);
    for i in 0..n {
        if i > 0 {
            values.push(',');
        }
        values.push_str(&format!(
            "({i}, 'benchmark document number {i}', NULL, NULL)"
        ));
    }
    let t = Instant::now();
    exec(&mut s, &format!("INSERT INTO docs VALUES {values}")).await;
    let insert_time = t.elapsed();

    // Completion probe via the storage change counter (O(1), no table scan —
    // a `count(*)` poll would reload the whole table each time and contend
    // with the service, badly skewing the number). The insert bumped the
    // counter N times (one per row); each write-back bumps it once more, so
    // the drain is done when the counter has advanced by 2N from `gen0`.
    let t_drain = Instant::now();
    let target = gen0 + 2 * n as u64;
    while db.storage().generation(&collection).unwrap_or(0) < target {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    let drain = t_drain.elapsed();
    println!(
        "insert {n} rows (1 statement): {:.2}s   ({:.0} rows/s DML)",
        insert_time.as_secs_f64(),
        n as f64 / insert_time.as_secs_f64()
    );
    println!(
        "pipeline drain (change-feed → embed → write-back): {:.2}s   ({:.0} rows/s plumbing)",
        drain.as_secs_f64(),
        n as f64 / drain.as_secs_f64()
    );

    // Idempotency: re-touch every row without changing its text. The service
    // sees N change events, recomputes the source hash, and skips all of them
    // — the provenance sidecar stays byte-identical. Measure how fast the
    // skip path clears the backlog (compared to embedding it fresh).
    let before: String = rows_of(&mut s, "SELECT _embedding_srchash FROM docs WHERE id = 0")
        .await
        .pop()
        .unwrap_or_default();
    let t = Instant::now();
    exec(&mut s, "UPDATE docs SET body = body").await;
    // Give the service time to process (and skip) the re-touch backlog.
    tokio::time::sleep(std::time::Duration::from_millis(50 + (n / 200) as u64)).await;
    let after: String = rows_of(&mut s, "SELECT _embedding_srchash FROM docs WHERE id = 0")
        .await
        .pop()
        .unwrap_or_default();
    println!(
        "idempotent re-touch (UPDATE, unchanged text): {:.2}s to submit; provenance unchanged = {}",
        t.elapsed().as_secs_f64(),
        before == after && !before.is_empty()
    );

    handle.abort();

    // Write-back primitive in isolation: patch_row (storage merge + put + the
    // generation bump that invalidates the table cache), the dominant cost
    // inside the plumbing. Reuses the now-seeded rows and the `collection`
    // resolved above.
    let vec_json = serde_json::Value::Array(
        (0..dims)
            .map(|i| serde_json::Value::from(i as f64 / dims as f64))
            .collect(),
    );
    let sample = n.min(5_000);
    let t = Instant::now();
    for i in 0..sample {
        db.patch_row(
            &collection,
            &i.to_string(),
            &[("embedding".to_string(), vec_json.clone())],
        )
        .await
        .expect("patch_row");
    }
    println!(
        "write-back primitive (patch_row): {:.0} rows/s  ← dominant cost inside plumbing",
        sample as f64 / t.elapsed().as_secs_f64()
    );

    println!("\ndone.");
}
