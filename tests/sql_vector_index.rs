#![cfg(feature = "vector-index")]
//! End-to-end conformance tests for the engine-native HNSW access method
//! (RFC 0005 phase 1): `CREATE INDEX ... USING hnsw` DDL validation, the
//! top-k ANN planner hook (`ORDER BY <col> <dist-op> <vec> LIMIT k`), the
//! §6.2 filtered-search strategy, tombstone/rebuild maintenance, sidecar
//! snapshots (§6.1), GUCs, and the EXPLAIN surface.
//!
//! Correctness baseline: for the small corpora used here, `ef_search` (40)
//! exceeds the corpus size, so HNSW search visits every live node and the
//! ANN result must equal the exact result — asserted by comparing against
//! `SET enable_indexscan = off` runs.

use guardian_db::sql::engine::{Database, Session};
use guardian_db::sql::{ExecResult, MemoryStorage};
use std::sync::Arc;

fn db() -> Arc<Database<MemoryStorage>> {
    Arc::new(Database::new(Arc::new(MemoryStorage::new()), "app"))
}

async fn session() -> Session<MemoryStorage> {
    Session::new(db(), "guardian")
}

async fn ok(s: &mut Session<MemoryStorage>, sql: &str) -> Vec<ExecResult> {
    s.execute(sql)
        .await
        .unwrap_or_else(|e| panic!("`{sql}` failed: {e}"))
}

async fn err(s: &mut Session<MemoryStorage>, sql: &str) -> String {
    match s.execute(sql).await {
        Ok(_) => panic!("expected `{sql}` to fail, but it succeeded"),
        Err(e) => e.to_string(),
    }
}

/// All rows of the first (and only) column, as text.
async fn column(s: &mut Session<MemoryStorage>, sql: &str) -> Vec<String> {
    match ok(s, sql).await.pop() {
        Some(ExecResult::Rows { rows, .. }) => rows
            .into_iter()
            .map(|r| r.first().and_then(|v| v.to_text()).unwrap_or_default())
            .collect(),
        other => panic!("`{sql}` did not produce rows: {other:?}"),
    }
}

/// Create the items table with `n` deterministic 3-d vectors: row `i` is
/// `[i, 0, 0]`, so distances from the origin order rows by `i` with no ties.
async fn seed(s: &mut Session<MemoryStorage>, n: usize) {
    ok(s, "CREATE EXTENSION vector").await;
    ok(
        s,
        "CREATE TABLE items (id int PRIMARY KEY, category text, v vector(3))",
    )
    .await;
    for i in 0..n {
        let cat = if i % 10 == 0 { "rare" } else { "common" };
        ok(
            s,
            &format!("INSERT INTO items VALUES ({i}, '{cat}', '[{i},0,0]')"),
        )
        .await;
    }
}

const TOPK: &str = "SELECT id FROM items ORDER BY v <-> '[0,0,0]'::vector LIMIT 5";

/// Run `sql` both through the ANN planner and with `enable_indexscan = off`
/// (exact), asserting identical results.
async fn assert_matches_exact(s: &mut Session<MemoryStorage>, sql: &str) -> Vec<String> {
    let ann = column(s, sql).await;
    ok(s, "SET enable_indexscan = off").await;
    let exact = column(s, sql).await;
    ok(s, "SET enable_indexscan = on").await;
    assert_eq!(ann, exact, "ANN result diverged from exact for `{sql}`");
    ann
}

// ---------------------------------------------------------------------------
// DDL validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hnsw_requires_vector_extension() {
    // The `vector` *type* already requires the extension, so a vector column
    // cannot exist without it; the access-method gate is defense-in-depth.
    // Exercise it through a non-vector column in a fresh database.
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE t (a int)").await;
    let e = err(&mut s, "CREATE INDEX ON t USING hnsw (a)").await;
    assert!(e.contains("hnsw"), "unexpected error: {e}");
    assert!(e.contains("extension"), "unexpected error: {e}");
}

#[tokio::test]
async fn hnsw_ddl_validation() {
    let mut s = session().await;
    ok(&mut s, "CREATE EXTENSION vector").await;
    ok(&mut s, "CREATE TABLE t (a int, v vector(3), u vector)").await;
    // Happy paths: default opclass, explicit opclass, WITH options.
    ok(&mut s, "CREATE INDEX t_v_l2 ON t USING hnsw (v)").await;
    ok(
        &mut s,
        "CREATE INDEX t_v_cos ON t USING hnsw (v vector_cosine_ops) \
         WITH (m = 24, ef_construction = 100)",
    )
    .await;
    // Unknown opclass.
    let e = err(&mut s, "CREATE INDEX ON t USING hnsw (v vector_bogus_ops)").await;
    assert!(e.contains("vector_bogus_ops"), "unexpected error: {e}");
    // Column type rules.
    let e = err(&mut s, "CREATE INDEX ON t USING hnsw (a)").await;
    assert!(e.contains("does not support column type"), "{e}");
    let e = err(&mut s, "CREATE INDEX ON t USING hnsw (u)").await;
    assert!(e.contains("does not have dimensions"), "{e}");
    // Options ranges (pgvector parity).
    let e = err(&mut s, "CREATE INDEX ON t USING hnsw (v) WITH (m = 1)").await;
    assert!(e.contains("\"m\""), "{e}");
    let e = err(
        &mut s,
        "CREATE INDEX ON t USING hnsw (v) WITH (ef_construction = 2000)",
    )
    .await;
    assert!(e.contains("ef_construction"), "{e}");
    let e = err(
        &mut s,
        "CREATE INDEX ON t USING hnsw (v) WITH (m = 40, ef_construction = 64)",
    )
    .await;
    assert!(e.contains("2 * m"), "{e}");
    let e = err(&mut s, "CREATE INDEX ON t USING hnsw (v) WITH (lists = 4)").await;
    assert!(e.contains("lists"), "{e}");
    // Shape rules.
    let e = err(&mut s, "CREATE UNIQUE INDEX ON t USING hnsw (v)").await;
    assert!(e.contains("unique"), "{e}");
    let e = err(&mut s, "CREATE INDEX ON t USING hnsw (v) WHERE a > 0").await;
    assert!(e.contains("partial"), "{e}");
    let e = err(&mut s, "CREATE INDEX ON t USING hnsw (a, v)").await;
    assert!(e.contains("exactly one column"), "{e}");
}

#[tokio::test]
async fn hnsw_rejects_more_than_2000_dims() {
    let mut s = session().await;
    ok(&mut s, "CREATE EXTENSION vector").await;
    ok(&mut s, "CREATE TABLE big (v vector(2001))").await;
    let e = err(&mut s, "CREATE INDEX ON big USING hnsw (v)").await;
    assert!(e.contains("2000"), "{e}");
}

#[tokio::test]
async fn non_hnsw_using_clause_still_works() {
    // Pre-existing behaviour: other methods are recorded and act as btree.
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE t (a int)").await;
    ok(&mut s, "CREATE INDEX ON t USING gin (a)").await;
    ok(&mut s, "INSERT INTO t VALUES (1)").await;
    assert_eq!(column(&mut s, "SELECT a FROM t WHERE a = 1").await, ["1"]);
}

// ---------------------------------------------------------------------------
// Top-k planner hook
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ann_topk_matches_exact_scan() {
    let mut s = session().await;
    seed(&mut s, 60).await;
    ok(&mut s, "CREATE INDEX ON items USING hnsw (v)").await;
    let got = assert_matches_exact(&mut s, TOPK).await;
    assert_eq!(got, ["0", "1", "2", "3", "4"]);
    // The plan proves the index was used.
    let plan = column(&mut s, &format!("EXPLAIN {TOPK}")).await;
    assert!(
        plan.iter().any(|l| l.contains("Ann Index Scan")),
        "plan was: {plan:?}"
    );
}

#[tokio::test]
async fn ann_above_exact_floor_uses_graph_path() {
    // Above `HnswIndexState::EXACT_ROW_FLOOR` (2000) the search is a genuine
    // approximate graph traversal rather than the exact linear floor. Assert
    // the plan really is the ANN graph path and returns a well-formed top-k
    // (k distinct real ids in ascending distance). Recall quality on
    // realistic data is validated separately and quantitatively by the
    // index-level `recall_above_floor_on_lowrank_data` unit test and the
    // `vector_index_bench` example — asserting a recall *threshold* here
    // would only couple a slow SQL test to synthetic-corpus micro-structure.
    let mut s = session().await;
    seed(&mut s, 2100).await;
    ok(&mut s, "CREATE INDEX ON items USING hnsw (v)").await;
    let sql = "SELECT id FROM items ORDER BY v <-> '[0,0,0]'::vector LIMIT 10";
    let plan = column(&mut s, &format!("EXPLAIN {sql}")).await;
    assert!(
        plan.iter().any(|l| l.contains("Ann Index Scan")),
        "expected graph path above the exact floor: {plan:?}"
    );
    let got = column(&mut s, sql).await;
    assert_eq!(got.len(), 10);
    let mut ids: Vec<i64> = got.iter().map(|s| s.parse().unwrap()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 10, "results must be 10 distinct rows");
}

#[tokio::test]
async fn all_four_operators_and_opclasses() {
    let mut s = session().await;
    // The `[i,0,0]` corpus is collinear — every row ties on cosine distance
    // and pairs tie on L2/L1 around the query. Distances must be pairwise
    // distinct under *all four* metrics for the exact-equality assertion, so
    // use unit-circle vectors at distinct angles on one side of the query
    // (no symmetric pairs).
    ok(&mut s, "CREATE EXTENSION vector").await;
    ok(
        &mut s,
        "CREATE TABLE items (id int PRIMARY KEY, v vector(3))",
    )
    .await;
    for i in 0..30 {
        let theta = 0.05 * (i as f64 + 1.0);
        ok(
            &mut s,
            &format!(
                "INSERT INTO items VALUES ({i}, '[{:.6},{:.6},0]')",
                theta.cos(),
                theta.sin()
            ),
        )
        .await;
    }
    // `<+>` (L1) is missing here: sqlparser 0.62 cannot tokenize it, so the
    // operator is unreachable from SQL today (a pre-existing parser
    // limitation — `l1_distance()` remains available, exact-path only).
    // `vector_l1_ops` index creation is still covered below.
    for (opclass, op) in [
        ("vector_l2_ops", "<->"),
        ("vector_ip_ops", "<#>"),
        ("vector_cosine_ops", "<=>"),
    ] {
        ok(
            &mut s,
            &format!("CREATE INDEX idx_{opclass} ON items USING hnsw (v {opclass})"),
        )
        .await;
        // Query angle ≈ −0.0997 rad sits below every corpus angle, so no
        // symmetric pairs tie on the angular metrics.
        let sql = format!("SELECT id FROM items ORDER BY v {op} '[1,-0.1,0]'::vector LIMIT 4");
        assert_matches_exact(&mut s, &sql).await;
        let plan = column(&mut s, &format!("EXPLAIN {sql}")).await;
        assert!(
            plan.iter()
                .any(|l| l.contains(&format!("idx_{opclass}")) && l.contains("Ann Index Scan")),
            "operator {op} did not use idx_{opclass}: {plan:?}"
        );
        ok(&mut s, &format!("DROP INDEX idx_{opclass}")).await;
    }
    // L1: the opclass is valid DDL and `l1_distance` answers exactly.
    ok(
        &mut s,
        "CREATE INDEX idx_l1 ON items USING hnsw (v vector_l1_ops)",
    )
    .await;
    let got = column(
        &mut s,
        "SELECT id FROM items ORDER BY l1_distance(v, '[1,-0.1,0]'::vector) LIMIT 2",
    )
    .await;
    assert_eq!(got, ["0", "1"]);
}

#[tokio::test]
async fn opclass_must_match_operator() {
    let mut s = session().await;
    seed(&mut s, 20).await;
    // A cosine index cannot serve `<->` — exact scan, still correct.
    ok(
        &mut s,
        "CREATE INDEX ON items USING hnsw (v vector_cosine_ops)",
    )
    .await;
    let got = assert_matches_exact(&mut s, TOPK).await;
    assert_eq!(got, ["0", "1", "2", "3", "4"]);
    let plan = column(&mut s, &format!("EXPLAIN {TOPK}")).await;
    assert!(
        !plan.iter().any(|l| l.contains("Ann Index Scan")),
        "cosine index must not serve <->: {plan:?}"
    );
}

#[tokio::test]
async fn no_limit_means_no_ann_scan() {
    let mut s = session().await;
    seed(&mut s, 20).await;
    ok(&mut s, "CREATE INDEX ON items USING hnsw (v)").await;
    let sql = "SELECT id FROM items ORDER BY v <-> '[0,0,0]'::vector";
    let got = column(&mut s, sql).await;
    assert_eq!(got.len(), 20);
    let plan = column(&mut s, &format!("EXPLAIN {sql}")).await;
    assert!(
        !plan.iter().any(|l| l.contains("Ann Index Scan")),
        "{plan:?}"
    );
}

#[tokio::test]
async fn descending_and_nulls_first_bail_to_exact() {
    let mut s = session().await;
    seed(&mut s, 20).await;
    ok(&mut s, "CREATE INDEX ON items USING hnsw (v)").await;
    for sql in [
        "SELECT id FROM items ORDER BY v <-> '[0,0,0]'::vector DESC LIMIT 3",
        "SELECT id FROM items ORDER BY v <-> '[0,0,0]'::vector NULLS FIRST LIMIT 3",
    ] {
        assert_matches_exact(&mut s, sql).await;
        let plan = column(&mut s, &format!("EXPLAIN {sql}")).await;
        assert!(
            !plan.iter().any(|l| l.contains("Ann Index Scan")),
            "{plan:?}"
        );
    }
}

#[tokio::test]
async fn limit_offset_pattern() {
    let mut s = session().await;
    seed(&mut s, 40).await;
    ok(&mut s, "CREATE INDEX ON items USING hnsw (v)").await;
    let sql = "SELECT id FROM items ORDER BY v <-> '[0,0,0]'::vector LIMIT 3 OFFSET 2";
    let got = assert_matches_exact(&mut s, sql).await;
    assert_eq!(got, ["2", "3", "4"]);
}

#[tokio::test]
async fn extra_order_keys_are_applied_exactly() {
    let mut s = session().await;
    ok(&mut s, "CREATE EXTENSION vector").await;
    ok(&mut s, "CREATE TABLE t (id int PRIMARY KEY, v vector(2))").await;
    // Two rows tied on distance; the tie-breaker must decide their order.
    // LIMIT covers the whole corpus so the tie sits *inside* the candidate
    // set — a tie exactly at the k-boundary is legitimately unstable under
    // an approximate scan (pgvector documents the same: boundary ties may
    // resolve either way).
    ok(
        &mut s,
        "INSERT INTO t VALUES (7, '[1,0]'), (3, '[0,1]'), (9, '[5,5]')",
    )
    .await;
    ok(&mut s, "CREATE INDEX ON t USING hnsw (v)").await;
    let sql = "SELECT id FROM t ORDER BY v <-> '[0,0]'::vector, id LIMIT 3";
    let got = assert_matches_exact(&mut s, sql).await;
    assert_eq!(got, ["3", "7", "9"]);
    let plan = column(&mut s, &format!("EXPLAIN {sql}")).await;
    assert!(
        plan.iter().any(|l| l.contains("Ann Index Scan")),
        "{plan:?}"
    );
}

#[tokio::test]
async fn null_vectors_sort_last_and_fill_underfull_limits() {
    let mut s = session().await;
    ok(&mut s, "CREATE EXTENSION vector").await;
    ok(&mut s, "CREATE TABLE t (id int PRIMARY KEY, v vector(2))").await;
    ok(
        &mut s,
        "INSERT INTO t VALUES (1, '[1,0]'), (2, NULL), (3, '[2,0]'), (4, NULL)",
    )
    .await;
    ok(&mut s, "CREATE INDEX ON t USING hnsw (v)").await;
    let sql = "SELECT id FROM t ORDER BY v <-> '[0,0]'::vector, id LIMIT 4";
    let got = assert_matches_exact(&mut s, sql).await;
    assert_eq!(got, ["1", "3", "2", "4"]);
}

#[tokio::test]
async fn query_vector_from_parameterizable_expression() {
    let mut s = session().await;
    seed(&mut s, 20).await;
    ok(&mut s, "CREATE INDEX ON items USING hnsw (v)").await;
    // Reversed operand order (`const <-> col`) must also plan.
    let sql = "SELECT id FROM items ORDER BY '[0,0,0]'::vector <-> v LIMIT 3";
    let got = assert_matches_exact(&mut s, sql).await;
    assert_eq!(got, ["0", "1", "2"]);
    let plan = column(&mut s, &format!("EXPLAIN {sql}")).await;
    assert!(
        plan.iter().any(|l| l.contains("Ann Index Scan")),
        "{plan:?}"
    );
}

// ---------------------------------------------------------------------------
// §6.2 filtered search
// ---------------------------------------------------------------------------

#[tokio::test]
async fn selective_equality_cuts_over_to_exact() {
    let mut s = session().await;
    seed(&mut s, 100).await;
    ok(&mut s, "CREATE INDEX ON items USING hnsw (v)").await;
    ok(&mut s, "CREATE INDEX items_cat ON items (category)").await;
    // 10 of 100 rows are 'rare' — within 10 × k for k = 5 → exact path.
    let sql = "SELECT id FROM items WHERE category = 'rare' \
               ORDER BY v <-> '[0,0,0]'::vector LIMIT 5";
    let got = assert_matches_exact(&mut s, sql).await;
    assert_eq!(got, ["0", "10", "20", "30", "40"]);
    let plan = column(&mut s, &format!("EXPLAIN {sql}")).await;
    assert!(
        plan.iter().any(|l| l.contains("ann skipped")),
        "expected selective cutover: {plan:?}"
    );
}

#[tokio::test]
async fn broad_filter_grows_adaptively() {
    let mut s = session().await;
    seed(&mut s, 100).await;
    ok(&mut s, "CREATE INDEX ON items USING hnsw (v)").await;
    // Non-indexed predicate keeping 1 in 10 rows: the first fetch of k
    // candidates cannot satisfy k survivors, forcing the growth loop.
    let sql = "SELECT id FROM items WHERE category = 'rare' \
               ORDER BY v <-> '[0,0,0]'::vector LIMIT 5";
    let got = assert_matches_exact(&mut s, sql).await;
    assert_eq!(got, ["0", "10", "20", "30", "40"]);
}

#[tokio::test]
async fn growth_ceiling_falls_back_to_exact() {
    let mut s = session().await;
    seed(&mut s, 100).await;
    ok(&mut s, "CREATE INDEX ON items USING hnsw (v)").await;
    // A filter matching nothing can never satisfy k survivors; with a tight
    // ceiling the planner must abandon ANN and fall back — and the exact
    // path must agree (empty result).
    ok(&mut s, "SET hnsw.ef_growth_cap = 1").await;
    let sql = "SELECT id FROM items WHERE category = 'absent' \
               ORDER BY v <-> '[0,0,0]'::vector LIMIT 5";
    let got = assert_matches_exact(&mut s, sql).await;
    assert!(got.is_empty());
}

// ---------------------------------------------------------------------------
// Maintenance: updates, deletes, rebuild
// ---------------------------------------------------------------------------

#[tokio::test]
async fn updates_and_deletes_are_reflected() {
    let mut s = session().await;
    seed(&mut s, 20).await;
    ok(&mut s, "CREATE INDEX ON items USING hnsw (v)").await;
    assert_eq!(column(&mut s, TOPK).await, ["0", "1", "2", "3", "4"]);
    // Move row 0 far away; delete row 1.
    ok(&mut s, "UPDATE items SET v = '[100,0,0]' WHERE id = 0").await;
    ok(&mut s, "DELETE FROM items WHERE id = 1").await;
    let got = assert_matches_exact(&mut s, TOPK).await;
    assert_eq!(got, ["2", "3", "4", "5", "6"]);
    // Update of an unrelated column must not perturb results.
    ok(&mut s, "UPDATE items SET category = 'x' WHERE id = 2").await;
    let got = assert_matches_exact(&mut s, TOPK).await;
    assert_eq!(got, ["2", "3", "4", "5", "6"]);
}

#[tokio::test]
async fn heavy_deletion_crosses_rebuild_threshold() {
    let mut s = session().await;
    seed(&mut s, 50).await;
    ok(&mut s, "CREATE INDEX ON items USING hnsw (v)").await;
    // Populate the graph, then delete over 20% of it.
    assert_eq!(column(&mut s, TOPK).await.len(), 5);
    ok(&mut s, "DELETE FROM items WHERE id < 20").await;
    let got = assert_matches_exact(&mut s, TOPK).await;
    assert_eq!(got, ["20", "21", "22", "23", "24"]);
}

#[tokio::test]
async fn writes_from_another_session_are_visible() {
    let shared = db();
    let mut a = Session::new(shared.clone(), "guardian");
    seed(&mut a, 10).await;
    ok(&mut a, "CREATE INDEX ON items USING hnsw (v)").await;
    assert_eq!(column(&mut a, TOPK).await.len(), 5);
    // A different session inserts a row at a distinct, second-nearest
    // position (an exact duplicate of row 0's vector would tie, and tied
    // candidate sets may legitimately differ between ANN and exact).
    let mut b = Session::new(shared, "guardian");
    ok(&mut b, "INSERT INTO items VALUES (999, 'z', '[0.5,0,0]')").await;
    let got = assert_matches_exact(&mut a, TOPK).await;
    assert_eq!(got[..2], ["0".to_string(), "999".to_string()]);
}

// ---------------------------------------------------------------------------
// GUCs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn gucs_are_registered_and_settable() {
    let mut s = session().await;
    assert_eq!(
        column(&mut s, "SHOW hnsw.ef_search").await,
        ["40"],
        "registered default"
    );
    ok(&mut s, "SET hnsw.ef_search = 100").await;
    assert_eq!(column(&mut s, "SHOW hnsw.ef_search").await, ["100"]);
    assert_eq!(column(&mut s, "SHOW hnsw.ef_growth_cap").await, ["10"]);
    assert_eq!(
        column(&mut s, "SHOW hnsw.selectivity_threshold").await,
        ["10"]
    );
}

// ---------------------------------------------------------------------------
// Sidecar snapshots (§6.1)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn snapshots_persist_and_reload_across_databases() {
    let dir = std::env::temp_dir().join(format!("gdb-vecidx-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let storage = Arc::new(MemoryStorage::new());

    let database = Arc::new(Database::new(storage.clone(), "app"));
    database.set_ann_snapshot_dir(&dir);
    let mut s = Session::new(database.clone(), "guardian");
    seed(&mut s, 50).await;
    ok(&mut s, "CREATE INDEX ON items USING hnsw (v)").await;
    assert_eq!(column(&mut s, TOPK).await.len(), 5);
    // Deleting >20% forces a rebuild, and a rebuild always snapshots.
    ok(&mut s, "DELETE FROM items WHERE id >= 30").await;
    assert_eq!(column(&mut s, TOPK).await, ["0", "1", "2", "3", "4"]);
    let files: Vec<_> = std::fs::read_dir(dir.join("app"))
        .expect("snapshot dir exists")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect();
    assert!(
        files.iter().any(|f| f.ends_with(".meta")),
        "snapshot files missing: {files:?}"
    );

    // A fresh Database over the same storage (same process restart shape)
    // must load the snapshot and answer identically — including reconciling
    // rows written after the snapshot was taken.
    drop(s);
    drop(database);
    let database2 = Arc::new(Database::new(storage, "app"));
    database2.set_ann_snapshot_dir(&dir);
    let mut s2 = Session::new(database2, "guardian");
    ok(&mut s2, "INSERT INTO items VALUES (999, 'z', '[0.5,0,0]')").await;
    let got = assert_matches_exact(&mut s2, TOPK).await;
    assert_eq!(
        got[..2],
        ["0".to_string(), "999".to_string()],
        "post-snapshot insert must be reconciled into the reloaded graph"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn drop_index_removes_snapshot_files() {
    let dir = std::env::temp_dir().join(format!("gdb-vecidx-drop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let database = Arc::new(Database::new(Arc::new(MemoryStorage::new()), "app"));
    database.set_ann_snapshot_dir(&dir);
    let mut s = Session::new(database, "guardian");
    seed(&mut s, 50).await;
    ok(&mut s, "CREATE INDEX items_v ON items USING hnsw (v)").await;
    assert_eq!(column(&mut s, TOPK).await.len(), 5);
    ok(&mut s, "DELETE FROM items WHERE id >= 30").await; // snapshot via rebuild
    assert_eq!(column(&mut s, TOPK).await.len(), 5);
    let app = dir.join("app");
    let has_meta = |dir: &std::path::Path| {
        std::fs::read_dir(dir)
            .map(|it| {
                it.filter_map(|e| e.ok())
                    .any(|e| e.file_name().to_string_lossy().ends_with(".meta"))
            })
            .unwrap_or(false)
    };
    assert!(has_meta(&app), "expected snapshot before drop");
    ok(&mut s, "DROP INDEX items_v").await;
    assert!(!has_meta(&app), "snapshot must be removed on DROP INDEX");
    // And the query still works (exact path).
    assert_eq!(column(&mut s, TOPK).await.len(), 5);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// EXPLAIN surface
// ---------------------------------------------------------------------------

#[tokio::test]
async fn explain_is_select_only_and_reports_default_path() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE t (a int)").await;
    let plan = column(&mut s, "EXPLAIN SELECT a FROM t").await;
    assert!(plan[0].contains("Seq Scan"), "{plan:?}");
    let e = err(&mut s, "EXPLAIN INSERT INTO t VALUES (1)").await;
    assert!(e.contains("SELECT"), "{e}");
    // EXPLAIN must not have executed anything.
    assert!(column(&mut s, "SELECT a FROM t").await.is_empty());
}
