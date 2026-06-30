#![cfg(feature = "sql")]
//! Conformance tests that pin down GuardianDB's documented PostgreSQL gaps.
//!
//! Two kinds of test live here:
//!   * **Clean-failure tests** assert that an unsupported feature fails with a
//!     precise SQLSTATE rather than silently misbehaving. These pass today and
//!     guard against accidental "fake success".
//!   * **`#[ignore]` tests** describe features that are intentionally not yet
//!     implemented; they encode the intended behaviour for when they are. Run
//!     them with `cargo test -- --ignored` to see what remains.
//!
//! Every gap listed in `docs/postgres-compat.md` has a corresponding test here.

use guardian_db::sql::engine::{Database, Session};
use guardian_db::sql::{ExecResult, MemoryStorage};
use std::sync::Arc;

async fn session() -> Session<MemoryStorage> {
    let db = Arc::new(Database::new(Arc::new(MemoryStorage::new()), "app"));
    Session::new(db, "guardian")
}

/// Execute SQL and return the SQLSTATE of the resulting error (panics if it
/// unexpectedly succeeds).
async fn err_code(s: &mut Session<MemoryStorage>, sql: &str) -> String {
    match s.execute(sql).await {
        Ok(_) => panic!("expected `{sql}` to fail, but it succeeded"),
        Err(e) => e.sqlstate().to_string(),
    }
}

async fn ok(s: &mut Session<MemoryStorage>, sql: &str) -> Vec<ExecResult> {
    s.execute(sql).await.unwrap_or_else(|e| panic!("`{sql}` failed: {e}"))
}

// ---------------------------------------------------------------------------
// Clean-failure gaps (these tests PASS — the feature fails with a clear code).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn window_functions_unsupported() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE t (id INT)").await;
    // 0A000 = feature_not_supported.
    assert_eq!(err_code(&mut s, "SELECT row_number() OVER (ORDER BY id) FROM t").await, "0A000");
}

#[tokio::test]
async fn set_returning_function_in_from_unsupported() {
    let mut s = session().await;
    assert_eq!(err_code(&mut s, "SELECT * FROM generate_series(1, 5)").await, "0A000");
}

#[tokio::test]
async fn nested_with_in_subquery_unsupported() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE t (n INT)").await;
    let code = err_code(
        &mut s,
        "SELECT * FROM (WITH x AS (SELECT 1) SELECT * FROM x) q",
    )
    .await;
    assert_eq!(code, "0A000");
}

#[tokio::test]
async fn copy_not_supported_by_engine() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE t (id INT)").await;
    // COPY requires wire-protocol CopyIn/CopyOut framing, which is not
    // implemented. The engine rejects it rather than pretending.
    assert_eq!(err_code(&mut s, "COPY t FROM STDIN").await, "0A000");
}

#[tokio::test]
async fn create_function_unsupported() {
    let mut s = session().await;
    let code = err_code(
        &mut s,
        "CREATE FUNCTION add(a int, b int) RETURNS int AS 'select a + b' LANGUAGE sql",
    )
    .await;
    assert_eq!(code, "0A000");
}

#[tokio::test]
async fn materialized_view_unsupported() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE t (id INT)").await;
    let code = err_code(&mut s, "CREATE MATERIALIZED VIEW mv AS SELECT * FROM t").await;
    // Either feature-not-supported or a parser-level rejection is acceptable;
    // what matters is that it does not silently "succeed".
    assert!(code == "0A000" || code == "42601", "got {code}");
}

#[tokio::test]
async fn full_text_search_unsupported() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE t (body TEXT)").await;
    ok(&mut s, "INSERT INTO t VALUES ('a cat sat')").await;
    let code = err_code(
        &mut s,
        "SELECT * FROM t WHERE to_tsvector(body) @@ to_tsquery('cat')",
    )
    .await;
    assert!(code == "0A000" || code == "42601", "got {code}");
}

// ---------------------------------------------------------------------------
// Intended-but-unimplemented features (ignored; encode the target behaviour).
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "WITH RECURSIVE is not implemented; CTEs are materialized non-recursively"]
async fn recursive_cte() {
    let mut s = session().await;
    let r = ok(
        &mut s,
        "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 5) SELECT sum(n) FROM c",
    )
    .await;
    match &r[0] {
        ExecResult::Rows { rows, .. } => assert_eq!(rows[0][0].to_text().unwrap(), "15"),
        _ => panic!("expected rows"),
    }
}

#[tokio::test]
#[ignore = "SAVEPOINT partial rollback is not implemented; ROLLBACK TO collapses to a full rollback"]
async fn savepoint_partial_rollback() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE t (id INT PRIMARY KEY)").await;
    ok(&mut s, "BEGIN").await;
    ok(&mut s, "INSERT INTO t VALUES (1)").await;
    ok(&mut s, "SAVEPOINT sp1").await;
    ok(&mut s, "INSERT INTO t VALUES (2)").await;
    ok(&mut s, "ROLLBACK TO SAVEPOINT sp1").await;
    ok(&mut s, "COMMIT").await;
    // Intended: only row 1 survives.
    let r = ok(&mut s, "SELECT count(*) FROM t").await;
    match &r[0] {
        ExecResult::Rows { rows, .. } => assert_eq!(rows[0][0].to_text().unwrap(), "1"),
        _ => panic!("expected rows"),
    }
}

#[tokio::test]
#[ignore = "SERIALIZABLE isolation is not implemented; local-atomic / read-committed only"]
async fn serializable_isolation() {
    // Intended: two concurrent transactions that would create a write skew are
    // serialized, with one aborting with 40001 (serialization_failure). The
    // strict-mode coordinator (single-writer) is the planned mechanism.
    let mut s = session().await;
    ok(&mut s, "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE").await;
}

#[tokio::test]
#[ignore = "Generated/computed columns (GENERATED ALWAYS AS) are not implemented"]
async fn generated_columns() {
    let mut s = session().await;
    ok(
        &mut s,
        "CREATE TABLE t (a INT, b INT GENERATED ALWAYS AS (a * 2) STORED)",
    )
    .await;
    ok(&mut s, "INSERT INTO t (a) VALUES (5)").await;
    let r = ok(&mut s, "SELECT b FROM t").await;
    match &r[0] {
        ExecResult::Rows { rows, .. } => assert_eq!(rows[0][0].to_text().unwrap(), "10"),
        _ => panic!("expected rows"),
    }
}
