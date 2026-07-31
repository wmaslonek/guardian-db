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
    s.execute(sql)
        .await
        .unwrap_or_else(|e| panic!("`{sql}` failed: {e}"))
}

/// First column of the first row, as i64 (for `SELECT count(*) ...`).
async fn scalar_i64(s: &mut Session<MemoryStorage>, sql: &str) -> i64 {
    let r = ok(s, sql).await;
    match &r[0] {
        ExecResult::Rows { rows, .. } => rows[0][0]
            .to_text()
            .and_then(|t| t.parse().ok())
            .unwrap_or_else(|| panic!("`{sql}` did not return an integer scalar")),
        _ => panic!("expected rows from `{sql}`"),
    }
}

/// All result rows rendered as text (NULL → "NULL").
async fn rows_text(s: &mut Session<MemoryStorage>, sql: &str) -> Vec<Vec<String>> {
    let r = ok(s, sql).await;
    match r.into_iter().next() {
        Some(ExecResult::Rows { rows, .. }) => rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|v| v.to_text().unwrap_or_else(|| "NULL".into()))
                    .collect()
            })
            .collect(),
        _ => panic!("expected rows from `{sql}`"),
    }
}

/// Execute SQL and return (SQLSTATE, message) of the resulting error.
async fn err_info(s: &mut Session<MemoryStorage>, sql: &str) -> (String, String) {
    match s.execute(sql).await {
        Ok(_) => panic!("expected `{sql}` to fail, but it succeeded"),
        Err(e) => (e.sqlstate().to_string(), e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Clean-failure gaps (these tests PASS — the feature fails with a clear code).
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Window functions (OVER)
// ---------------------------------------------------------------------------

async fn window_session() -> Session<MemoryStorage> {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE t (v INT)").await;
    ok(&mut s, "INSERT INTO t VALUES (10), (20), (20), (30)").await;
    ok(&mut s, "CREATE TABLE emp (dept TEXT, sal INT)").await;
    ok(
        &mut s,
        "INSERT INTO emp VALUES ('a', 100), ('a', 200), ('b', 300)",
    )
    .await;
    s
}

#[tokio::test]
async fn window_ranking_with_ties() {
    let mut s = window_session().await;
    let rows = rows_text(
        &mut s,
        "SELECT v, row_number() OVER (ORDER BY v), rank() OVER (ORDER BY v), \
         dense_rank() OVER (ORDER BY v) FROM t ORDER BY v, 2",
    )
    .await;
    assert_eq!(
        rows,
        vec![
            vec!["10", "1", "1", "1"],
            vec!["20", "2", "2", "2"],
            vec!["20", "3", "2", "2"],
            vec!["30", "4", "4", "3"],
        ]
    );
    // percent_rank/cume_dist over two distinct rows: clean fractions.
    let rows = rows_text(
        &mut s,
        "SELECT sal, percent_rank() OVER (ORDER BY sal), cume_dist() OVER (ORDER BY sal) \
         FROM emp WHERE dept = 'a' ORDER BY sal",
    )
    .await;
    assert_eq!(rows, vec![vec!["100", "0", "0.5"], vec!["200", "1", "1"]]);
}

#[tokio::test]
async fn window_partition_boundaries() {
    let mut s = window_session().await;
    let rows = rows_text(
        &mut s,
        "SELECT dept, sal, row_number() OVER (PARTITION BY dept ORDER BY sal), \
         sum(sal) OVER (PARTITION BY dept) FROM emp ORDER BY dept, sal",
    )
    .await;
    assert_eq!(
        rows,
        vec![
            vec!["a", "100", "1", "300"],
            vec!["a", "200", "2", "300"],
            vec!["b", "300", "1", "300"],
        ]
    );
    // Window calls also work inside a derived subquery.
    assert_eq!(
        scalar_i64(
            &mut s,
            "SELECT count(*) FROM (SELECT row_number() OVER (ORDER BY v) AS rn FROM t) q \
             WHERE rn <= 2",
        )
        .await,
        2
    );
}

#[tokio::test]
async fn window_lag_lead_offset_default() {
    let mut s = window_session().await;
    let rows = rows_text(
        &mut s,
        "SELECT v, lag(v) OVER (ORDER BY v, v), lead(v) OVER (ORDER BY v, v), \
         lag(v, 2, -1) OVER (ORDER BY v, v), lead(v, 2) OVER (ORDER BY v, v) \
         FROM t ORDER BY v",
    )
    .await;
    assert_eq!(
        rows,
        vec![
            vec!["10", "NULL", "20", "-1", "20"],
            vec!["20", "10", "20", "-1", "30"],
            vec!["20", "20", "30", "10", "NULL"],
            vec!["30", "20", "NULL", "20", "NULL"],
        ]
    );
}

#[tokio::test]
async fn window_running_sum_default_frame_includes_peers() {
    let mut s = window_session().await;
    // Default frame = RANGE UNBOUNDED PRECEDING..CURRENT ROW: peers of the
    // current row are included, so the running sum jumps by peer groups
    // (10,20,20,30 → 10,50,50,80) — the classic PostgreSQL behaviour.
    let rows = rows_text(
        &mut s,
        "SELECT v, sum(v) OVER (ORDER BY v) FROM t ORDER BY v",
    )
    .await;
    assert_eq!(
        rows,
        vec![
            vec!["10", "10"],
            vec!["20", "50"],
            vec!["20", "50"],
            vec!["30", "80"],
        ]
    );
    // ROWS mode does not include peers: a true row-by-row running sum.
    let rows = rows_text(
        &mut s,
        "SELECT v, sum(v) OVER (ORDER BY v ROWS UNBOUNDED PRECEDING) FROM t ORDER BY v, 2",
    )
    .await;
    assert_eq!(
        rows,
        vec![
            vec!["10", "10"],
            vec!["20", "30"],
            vec!["20", "50"],
            vec!["30", "80"],
        ]
    );
}

#[tokio::test]
async fn window_rows_between_moving_frame() {
    let mut s = window_session().await;
    let rows = rows_text(
        &mut s,
        "SELECT v, sum(v) OVER (ORDER BY v ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) \
         FROM t ORDER BY v, 2",
    )
    .await;
    assert_eq!(
        rows,
        vec![
            vec!["10", "30"],
            vec!["20", "50"],
            vec!["20", "70"],
            vec!["30", "50"],
        ]
    );
}

#[tokio::test]
async fn window_last_value_frame_gotcha() {
    let mut s = window_session().await;
    // Default frame ends at the current row's peer group, so last_value
    // returns the current peer value — not the partition max (PG gotcha).
    let rows = rows_text(
        &mut s,
        "SELECT v, first_value(v) OVER (ORDER BY v), last_value(v) OVER (ORDER BY v) \
         FROM t ORDER BY v",
    )
    .await;
    assert_eq!(
        rows,
        vec![
            vec!["10", "10", "10"],
            vec!["20", "10", "20"],
            vec!["20", "10", "20"],
            vec!["30", "10", "30"],
        ]
    );
    // The explicit whole-partition frame gives the partition max.
    let rows = rows_text(
        &mut s,
        "SELECT v, last_value(v) OVER (ORDER BY v ROWS BETWEEN UNBOUNDED PRECEDING AND \
         UNBOUNDED FOLLOWING) FROM t ORDER BY v",
    )
    .await;
    assert!(rows.iter().all(|r| r[1] == "30"), "{rows:?}");
    // nth_value honours the frame too (NULL before the frame reaches row n).
    let rows = rows_text(
        &mut s,
        "SELECT v, nth_value(v, 2) OVER (ORDER BY v) FROM t ORDER BY v",
    )
    .await;
    assert_eq!(
        rows,
        vec![
            vec!["10", "NULL"],
            vec!["20", "20"],
            vec!["20", "20"],
            vec!["30", "20"],
        ]
    );
}

#[tokio::test]
async fn window_ntile_uneven() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE n5 (v INT)").await;
    ok(&mut s, "INSERT INTO n5 VALUES (1), (2), (3), (4), (5)").await;
    let rows = rows_text(
        &mut s,
        "SELECT v, ntile(2) OVER (ORDER BY v), ntile(3) OVER (ORDER BY v) FROM n5 ORDER BY v",
    )
    .await;
    assert_eq!(
        rows,
        vec![
            vec!["1", "1", "1"],
            vec!["2", "1", "1"],
            vec!["3", "1", "2"],
            vec!["4", "2", "2"],
            vec!["5", "2", "3"],
        ]
    );
    assert_eq!(
        err_code(&mut s, "SELECT ntile(0) OVER (ORDER BY v) FROM n5").await,
        "22023"
    );
}

#[tokio::test]
async fn window_in_order_by() {
    let mut s = window_session().await;
    let rows = rows_text(
        &mut s,
        "SELECT v FROM t ORDER BY row_number() OVER (ORDER BY v DESC)",
    )
    .await;
    assert_eq!(rows, vec![vec!["30"], vec!["20"], vec!["20"], vec!["10"]]);
}

#[tokio::test]
async fn window_multiple_windows() {
    let mut s = window_session().await;
    let rows = rows_text(
        &mut s,
        "SELECT dept, sal, row_number() OVER (PARTITION BY dept ORDER BY sal), \
         rank() OVER (ORDER BY sal DESC), count(*) OVER () FROM emp ORDER BY dept, sal",
    )
    .await;
    assert_eq!(
        rows,
        vec![
            vec!["a", "100", "1", "3", "3"],
            vec!["a", "200", "2", "2", "3"],
            vec!["b", "300", "1", "1", "3"],
        ]
    );
}

#[tokio::test]
async fn window_named_window_clause() {
    let mut s = window_session().await;
    // OVER w + refinement OVER (w ORDER BY ...) inheriting the partition.
    let rows = rows_text(
        &mut s,
        "SELECT dept, sal, sum(sal) OVER w, row_number() OVER (w ORDER BY sal) \
         FROM emp WINDOW w AS (PARTITION BY dept) ORDER BY dept, sal",
    )
    .await;
    assert_eq!(
        rows,
        vec![
            vec!["a", "100", "300", "1"],
            vec!["a", "200", "300", "2"],
            vec!["b", "300", "300", "1"],
        ]
    );
    // Refinement may not override an existing ORDER BY.
    let (code, msg) = err_info(
        &mut s,
        "SELECT row_number() OVER (w ORDER BY sal) FROM emp WINDOW w AS (ORDER BY dept)",
    )
    .await;
    assert_eq!(code, "42P20");
    assert!(msg.contains("cannot override ORDER BY"), "{msg}");
    // Unknown named window.
    assert_eq!(
        err_code(&mut s, "SELECT row_number() OVER wnope FROM emp").await,
        "42704"
    );
}

#[tokio::test]
async fn window_null_ordering_and_empty_input() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE tn (v INT)").await;
    ok(&mut s, "INSERT INTO tn VALUES (1), (NULL), (2)").await;
    // ASC → NULLS LAST (PostgreSQL default); count(v) skips the NULL.
    let rows = rows_text(
        &mut s,
        "SELECT v, row_number() OVER (ORDER BY v), count(v) OVER (ORDER BY v) \
         FROM tn ORDER BY 2",
    )
    .await;
    assert_eq!(
        rows,
        vec![
            vec!["1", "1", "1"],
            vec!["2", "2", "2"],
            vec!["NULL", "3", "2"],
        ]
    );
    // DESC → NULLS FIRST.
    let rows = rows_text(
        &mut s,
        "SELECT v, row_number() OVER (ORDER BY v DESC) FROM tn ORDER BY 2",
    )
    .await;
    assert_eq!(
        rows,
        vec![vec!["NULL", "1"], vec!["2", "2"], vec!["1", "3"]]
    );
    // Zero input rows → zero output rows, no error.
    let rows = rows_text(
        &mut s,
        "SELECT row_number() OVER (ORDER BY v) FROM tn WHERE v > 100",
    )
    .await;
    assert!(rows.is_empty());
}

#[tokio::test]
async fn window_aggregates_and_filter() {
    let mut s = window_session().await;
    let rows = rows_text(
        &mut s,
        "SELECT v, min(v) OVER (), max(v) OVER (), avg(v) OVER (), \
         count(*) FILTER (WHERE v > 10) OVER () FROM t ORDER BY v LIMIT 1",
    )
    .await;
    assert_eq!(rows, vec![vec!["10", "10", "30", "20", "3"]]);
    // string_agg as a window aggregate over the whole partition.
    let rows = rows_text(
        &mut s,
        "SELECT string_agg(dept, ',') OVER (ORDER BY dept ROWS BETWEEN UNBOUNDED PRECEDING \
         AND UNBOUNDED FOLLOWING) FROM emp LIMIT 1",
    )
    .await;
    assert_eq!(rows, vec![vec!["a,a,b"]]);
}

#[tokio::test]
async fn window_over_grouped_query() {
    let mut s = window_session().await;
    // Windows evaluate after GROUP BY/HAVING: sum of the per-group counts.
    let rows = rows_text(
        &mut s,
        "SELECT dept, count(*), sum(count(*)) OVER (ORDER BY dept) FROM emp \
         GROUP BY dept ORDER BY dept",
    )
    .await;
    assert_eq!(rows, vec![vec!["a", "2", "2"], vec!["b", "1", "3"]]);
}

#[tokio::test]
async fn window_out_of_subset_constructs_fail_typed() {
    let mut s = window_session().await;
    // 42P20: misplaced window functions.
    for sql in [
        "SELECT v FROM t WHERE row_number() OVER () = 1",
        "SELECT v FROM t GROUP BY row_number() OVER ()",
        "SELECT v FROM t GROUP BY v HAVING count(*) OVER () > 0",
        // Nested window calls.
        "SELECT sum(row_number() OVER ()) OVER () FROM t",
        // Invalid frames.
        "SELECT sum(v) OVER (ORDER BY v ROWS BETWEEN CURRENT ROW AND 1 PRECEDING) FROM t",
        "SELECT sum(v) OVER (ORDER BY v ROWS BETWEEN UNBOUNDED FOLLOWING AND CURRENT ROW) FROM t",
    ] {
        assert_eq!(err_code(&mut s, sql).await, "42P20", "for `{sql}`");
    }
    // 0A000 with a message naming the construct.
    let (code, msg) = err_info(
        &mut s,
        "SELECT sum(v) OVER (ORDER BY v RANGE BETWEEN 1 PRECEDING AND CURRENT ROW) FROM t",
    )
    .await;
    assert_eq!(code, "0A000");
    assert!(msg.contains("RANGE with offset"), "{msg}");
    let (code, msg) = err_info(
        &mut s,
        "SELECT sum(v) OVER (ORDER BY v GROUPS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM t",
    )
    .await;
    assert_eq!(code, "0A000");
    assert!(msg.contains("GROUPS"), "{msg}");
    let (code, msg) = err_info(&mut s, "SELECT count(DISTINCT v) OVER () FROM t").await;
    assert_eq!(code, "0A000");
    assert!(msg.contains("DISTINCT"), "{msg}");
    // 42809: OVER on a function that is not a window function or aggregate.
    assert_eq!(
        err_code(&mut s, "SELECT abs(v) OVER () FROM t").await,
        "42809"
    );
}

// ---------------------------------------------------------------------------
// WITH RECURSIVE
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recursive_sequence_generation() {
    let mut s = session().await;
    assert_eq!(
        scalar_i64(
            &mut s,
            "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 10) \
             SELECT sum(n) FROM c",
        )
        .await,
        55
    );
    // WITH RECURSIVE without an actual self-reference is plain WITH.
    assert_eq!(
        scalar_i64(
            &mut s,
            "WITH RECURSIVE c AS (SELECT 1 AS n) SELECT n FROM c"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn recursive_transitive_closure_with_cycle() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE edges (src INT, dst INT)").await;
    ok(&mut s, "INSERT INTO edges VALUES (1, 2), (2, 3), (3, 1)").await;
    // The graph is a cycle; UNION dedup against the accumulation terminates.
    assert_eq!(
        scalar_i64(
            &mut s,
            "WITH RECURSIVE reach(node) AS (SELECT 2 UNION \
             SELECT e.dst FROM edges e JOIN reach r ON e.src = r.node) \
             SELECT count(*) FROM reach",
        )
        .await,
        3
    );
}

#[tokio::test]
async fn recursive_union_all_with_limit() {
    let mut s = session().await;
    let rows = rows_text(
        &mut s,
        "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 100) \
         SELECT n FROM c ORDER BY n LIMIT 5",
    )
    .await;
    assert_eq!(
        rows,
        vec![vec!["1"], vec!["2"], vec!["3"], vec!["4"], vec!["5"]]
    );
}

#[tokio::test]
async fn recursive_working_table_semantics() {
    let mut s = session().await;
    // Fibonacci: each iteration must see ONLY the previous iteration's row.
    // If the recursive term were fed the full accumulation, every earlier row
    // would spawn again each round and the row count would explode.
    let rows = rows_text(
        &mut s,
        "WITH RECURSIVE fib(a, b) AS (SELECT 0, 1 UNION ALL SELECT b, a + b FROM fib \
         WHERE b < 100) SELECT count(*), max(b) FROM fib",
    )
    .await;
    assert_eq!(rows, vec![vec!["12", "144"]]);
}

#[tokio::test]
async fn recursive_infinite_recursion_guard_errors() {
    let mut s = session().await;
    // Lower the session iteration cap so the guard fires fast; the query has
    // no termination condition and must error (54001), not hang.
    ok(
        &mut s,
        "SELECT set_config('guardian.recursive_max_iterations', '50', false)",
    )
    .await;
    let (code, msg) = err_info(
        &mut s,
        "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c) \
         SELECT count(*) FROM c",
    )
    .await;
    assert_eq!(code, "54001");
    assert!(msg.contains("50 iterations"), "{msg}");
}

#[tokio::test]
async fn recursive_with_extra_plain_cte() {
    let mut s = session().await;
    assert_eq!(
        scalar_i64(
            &mut s,
            "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 5), \
             doubled AS (SELECT n * 2 AS m FROM seq) SELECT sum(m) FROM doubled",
        )
        .await,
        30
    );
}

#[tokio::test]
async fn recursive_column_types_fixed_by_base_term() {
    let mut s = session().await;
    // Recursive-term rows coerce to the base term's types; an uncoercible
    // value is a typed error, never a silently mistyped row.
    assert_eq!(
        err_code(
            &mut s,
            "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT 'x' FROM c WHERE n = 1) \
             SELECT * FROM c",
        )
        .await,
        "22P02"
    );
}

#[tokio::test]
async fn recursive_invalid_forms_fail_typed() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE edges (src INT, dst INT)").await;
    // 42P19: invalid recursion shapes.
    for sql in [
        // Self-reference more than once.
        "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT c1.n FROM c c1, c c2) \
         SELECT * FROM c",
        // Self-reference inside a subquery.
        "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c \
         WHERE n < (SELECT max(n) FROM c)) SELECT * FROM c",
        // Self-reference in the non-recursive term.
        "WITH RECURSIVE c(n) AS (SELECT n FROM c UNION ALL SELECT 1) SELECT * FROM c",
        // Aggregate over the recursive reference.
        "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT max(n) + 1 FROM c WHERE n < 5) \
         SELECT * FROM c",
        // Self-reference on the nullable side of an outer join.
        "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL \
         SELECT c.n + 1 FROM edges e LEFT JOIN c ON e.src = c.n) SELECT * FROM c",
        // Self-reference without the UNION shape.
        "WITH RECURSIVE c(n) AS (SELECT n FROM c) SELECT * FROM c",
    ] {
        assert_eq!(err_code(&mut s, sql).await, "42P19", "for `{sql}`");
    }
    // 0A000: recognized-but-unsupported recursive constructs.
    let (code, msg) = err_info(
        &mut s,
        "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 3 \
         ORDER BY n) SELECT * FROM c",
    )
    .await;
    assert_eq!(code, "0A000");
    assert!(msg.contains("ORDER BY in a recursive query"), "{msg}");
    let (code, msg) = err_info(
        &mut s,
        "WITH RECURSIVE a(n) AS (SELECT m FROM b UNION ALL SELECT n FROM a WHERE n < 2), \
         b(m) AS (SELECT 1) SELECT * FROM a",
    )
    .await;
    assert_eq!(code, "0A000");
    assert!(msg.contains("mutual recursion"), "{msg}");
    // WITH (recursive or not) inside a subquery is still rejected.
    assert_eq!(
        err_code(
            &mut s,
            "SELECT * FROM (WITH RECURSIVE c AS (SELECT 1 AS n) SELECT * FROM c) q",
        )
        .await,
        "0A000"
    );
}

#[tokio::test]
async fn set_returning_function_in_from_unsupported() {
    let mut s = session().await;
    assert_eq!(
        err_code(&mut s, "SELECT * FROM generate_series(1, 5)").await,
        "0A000"
    );
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
async fn create_function_basic_forms_now_supported() {
    // `CREATE FUNCTION` is implemented (see `guardian_db::sql::udf` and
    // `tests/sql_functions.rs` for the full behavioral test suite); this test
    // only pins that the basic forms parse and execute, since this file used
    // to pin them as a blanket 0A000 rejection.
    let mut s = session().await;
    ok(
        &mut s,
        "CREATE FUNCTION add(a int, b int) RETURNS int AS 'select a + b' LANGUAGE sql",
    )
    .await;
    assert_eq!(scalar_i64(&mut s, "SELECT add(2, 3)").await, 5);
    ok(
        &mut s,
        "CREATE OR REPLACE FUNCTION add(a int, b int) RETURNS int AS 'select a + b' LANGUAGE sql",
    )
    .await;
}

#[tokio::test]
async fn create_function_trigger_return_type_supported() {
    // Trigger functions (`RETURNS trigger`, PL/pgSQL) are implemented — see
    // `tests/sql_triggers.rs` for the full behavioral suite. The SQL-language
    // form stays rejected with PostgreSQL's own 42P13, and calling a trigger
    // function as a scalar is PostgreSQL's 0A000.
    let mut s = session().await;
    ok(
        &mut s,
        "CREATE FUNCTION f() RETURNS trigger AS $$ BEGIN RETURN NEW; END; $$ LANGUAGE plpgsql",
    )
    .await;
    assert_eq!(err_code(&mut s, "SELECT f()").await, "0A000");
    assert_eq!(
        err_code(
            &mut s,
            "CREATE FUNCTION g() RETURNS trigger AS $$ SELECT 1 $$ LANGUAGE sql"
        )
        .await,
        "42P13"
    );
}

#[tokio::test]
async fn create_procedure_unsupported() {
    let mut s = session().await;
    // sqlparser 0.62 cannot parse the PostgreSQL form of CREATE PROCEDURE at
    // all — without prefix detection this would leak a 42601 syntax error
    // instead of the stable feature rejection.
    for sql in [
        "CREATE PROCEDURE p() LANGUAGE sql AS 'SELECT 1'",
        "CREATE OR REPLACE PROCEDURE p() LANGUAGE sql AS 'SELECT 1'",
        "CREATE PROCEDURE p2() AS BEGIN SELECT 1; END",
    ] {
        assert_eq!(err_code(&mut s, sql).await, "0A000", "for `{sql}`");
    }
}

#[tokio::test]
async fn create_trigger_supported_with_typed_exclusions() {
    // Triggers are implemented (see `tests/sql_triggers.rs` for the full
    // behavioral suite), including CONSTRAINT, INSTEAD OF, and TRUNCATE variants.
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE t (id INT)").await;
    ok(
        &mut s,
        "CREATE FUNCTION f() RETURNS trigger AS $$ BEGIN RETURN NEW; END; $$ LANGUAGE plpgsql",
    )
    .await;
    ok(
        &mut s,
        "CREATE TRIGGER trg BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f()",
    )
    .await;
    ok(
        &mut s,
        "CREATE OR REPLACE TRIGGER trg AFTER UPDATE ON t FOR EACH ROW \
         WHEN (NEW.id IS DISTINCT FROM OLD.id) EXECUTE PROCEDURE f()",
    )
    .await;
    ok(&mut s, "DROP TRIGGER trg ON t").await;
    ok(
        &mut s,
        "CREATE CONSTRAINT TRIGGER trg AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f()",
    )
    .await;
    ok(&mut s, "DROP TRIGGER trg ON t").await;
    ok(&mut s, "CREATE TABLE t2 (id INT)").await;
    ok(&mut s, "CREATE VIEW v AS SELECT * FROM t2").await;
    ok(
        &mut s,
        "CREATE TRIGGER trg INSTEAD OF INSERT ON v FOR EACH ROW EXECUTE FUNCTION f()",
    )
    .await;
    ok(&mut s, "DROP TRIGGER trg ON v").await;
    ok(
        &mut s,
        "CREATE TRIGGER trg AFTER TRUNCATE ON t FOR EACH STATEMENT EXECUTE FUNCTION f()",
    )
    .await;
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
async fn full_text_search_subset_and_exclusions() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE t (body TEXT)").await;
    ok(&mut s, "INSERT INTO t VALUES ('a cat sat')").await;
    // The core subset works (details in tests/sql_fts.rs): the constructor
    // functions, @@ in every PostgreSQL argument order, ts_rank, and the
    // tsvector/tsquery types with raw-parse casts.
    assert_eq!(
        scalar_i64(
            &mut s,
            "SELECT count(*) FROM t WHERE to_tsvector(body) @@ to_tsquery('cat')"
        )
        .await,
        1
    );
    assert_eq!(
        rows_text(&mut s, "SELECT to_tsvector('a cat sat')").await,
        vec![vec!["'cat':2 'sat':3".to_string()]]
    );
    assert_eq!(
        rows_text(&mut s, "SELECT body @@ 'cat' FROM t").await,
        vec![vec!["t".to_string()]]
    );
    ok(&mut s, "SELECT 'a'::tsvector").await;
    ok(&mut s, "SELECT 'a'::tsquery").await;
    // ts_headline is now supported; test it works
    assert_eq!(
        rows_text(&mut s, "SELECT ts_headline('a cat sat', 'cat')").await,
        vec![vec!["a <b>cat</b> sat".to_string()]]
    );
    // Out-of-subset FTS constructs are *named*-unsupported: stable 0A000. It
    // must never be 42883/"does not exist" — these are PostgreSQL core
    // functions, and 42883 is also what triggers sidecar fallback-routing,
    // which would make FTS semantics differ per deployment.
    for sql in [
        "SELECT phraseto_tsquery('the cat')",
        "SELECT websearch_to_tsquery('cat -dog')",
        "SELECT ts_rank_cd('a', 'b')",
        "SELECT setweight('a', 'A')",
        "SELECT ts_delete('a', 'b')",
        "SELECT tsvector_to_array('a')",
        "SELECT to_tsvector('cat') || to_tsvector('dog')",
        "SELECT to_tsquery('cat <-> dog')",
    ] {
        assert_eq!(err_code(&mut s, sql).await, "0A000", "for `{sql}`");
    }
    // Unknown text search configurations are 42704, like PostgreSQL.
    assert_eq!(
        err_code(&mut s, "SELECT to_tsvector('unknown_config_xyz', 'test')").await,
        "42704"
    );
}

// ---------------------------------------------------------------------------
// Foreign keys: declared, introspectable AND enforced (MATCH SIMPLE).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn foreign_keys_enforced_on_insert_and_child_update() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE parent (id INT PRIMARY KEY)").await;
    ok(
        &mut s,
        "CREATE TABLE child (id INT PRIMARY KEY, pid INT REFERENCES parent(id))",
    )
    .await;

    // INSERT with no matching parent: 23503 with the PostgreSQL message shape.
    let err = s
        .execute("INSERT INTO child VALUES (1, 999)")
        .await
        .unwrap_err();
    assert_eq!(err.sqlstate(), "23503");
    assert_eq!(
        err.to_string(),
        "insert or update on table \"child\" violates foreign key constraint \"child_pid_fkey\""
    );

    // MATCH SIMPLE: a NULL FK column satisfies the constraint.
    ok(&mut s, "INSERT INTO child VALUES (1, NULL)").await;

    ok(&mut s, "INSERT INTO parent VALUES (1)").await;
    ok(&mut s, "INSERT INTO child VALUES (2, 1)").await;

    // UPDATE of the referencing column re-checks: dangling value fails ...
    assert_eq!(
        err_code(&mut s, "UPDATE child SET pid = 12345 WHERE id = 2").await,
        "23503"
    );
    // ... NULL and an existing parent pass.
    ok(&mut s, "UPDATE child SET pid = NULL WHERE id = 2").await;
    ok(&mut s, "UPDATE child SET pid = 1 WHERE id = 2").await;
}

#[tokio::test]
async fn foreign_keys_restrict_and_no_action_block_parent_delete() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE parent (id INT PRIMARY KEY)").await;
    ok(
        &mut s,
        "CREATE TABLE child (id INT PRIMARY KEY, pid INT REFERENCES parent(id))",
    )
    .await;
    ok(
        &mut s,
        "CREATE TABLE child_r (id INT PRIMARY KEY, \
         pid INT REFERENCES parent(id) ON DELETE RESTRICT)",
    )
    .await;
    ok(&mut s, "INSERT INTO parent VALUES (1), (2)").await;
    ok(&mut s, "INSERT INTO child VALUES (1, 1)").await;
    ok(&mut s, "INSERT INTO child_r VALUES (1, 2)").await;

    // Default NO ACTION: deleting a referenced parent is 23503 with the
    // PostgreSQL message shape.
    let err = s
        .execute("DELETE FROM parent WHERE id = 1")
        .await
        .unwrap_err();
    assert_eq!(err.sqlstate(), "23503");
    assert_eq!(
        err.to_string(),
        "update or delete on table \"parent\" violates foreign key constraint \
         \"child_pid_fkey\" on table \"child\""
    );
    // RESTRICT: same outcome (per-statement checking; no deferral).
    assert_eq!(
        err_code(&mut s, "DELETE FROM parent WHERE id = 2").await,
        "23503"
    );
    // Removing the referencing rows unblocks the delete — including within
    // one statement (child rows deleted by the same DELETE's cascade set).
    ok(&mut s, "DELETE FROM child WHERE id = 1").await;
    ok(&mut s, "DELETE FROM parent WHERE id = 1").await;
    // UPDATE of the referenced key is guarded the same way.
    assert_eq!(
        err_code(&mut s, "UPDATE parent SET id = 9 WHERE id = 2").await,
        "23503"
    );
}

#[tokio::test]
async fn foreign_key_on_delete_cascade_multi_level_and_self_referential() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE a (id INT PRIMARY KEY)").await;
    ok(
        &mut s,
        "CREATE TABLE b (id INT PRIMARY KEY, aid INT REFERENCES a(id) ON DELETE CASCADE)",
    )
    .await;
    // The second level declares its own action; cascading into `b` applies
    // b's children's actions recursively.
    ok(
        &mut s,
        "CREATE TABLE c (id INT PRIMARY KEY, bid INT REFERENCES b(id) ON DELETE CASCADE)",
    )
    .await;
    ok(&mut s, "INSERT INTO a VALUES (1), (2)").await;
    ok(&mut s, "INSERT INTO b VALUES (10, 1), (20, 2)").await;
    ok(&mut s, "INSERT INTO c VALUES (100, 10), (200, 20)").await;

    ok(&mut s, "DELETE FROM a WHERE id = 1").await;
    assert_eq!(scalar_i64(&mut s, "SELECT count(*) FROM a").await, 1);
    assert_eq!(scalar_i64(&mut s, "SELECT count(*) FROM b").await, 1);
    assert_eq!(scalar_i64(&mut s, "SELECT count(*) FROM c").await, 1);

    // Self-referential CASCADE terminates (chain and even mutual references).
    ok(
        &mut s,
        "CREATE TABLE tree (id INT PRIMARY KEY, \
         parent_id INT REFERENCES tree(id) ON DELETE CASCADE)",
    )
    .await;
    ok(
        &mut s,
        "INSERT INTO tree VALUES (1, NULL), (2, 1), (3, 2), (4, NULL)",
    )
    .await;
    ok(&mut s, "DELETE FROM tree WHERE id = 1").await;
    assert_eq!(scalar_i64(&mut s, "SELECT count(*) FROM tree").await, 1);
}

#[tokio::test]
async fn foreign_key_on_delete_set_null() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE parent (id INT PRIMARY KEY)").await;
    ok(
        &mut s,
        "CREATE TABLE child (id INT PRIMARY KEY, \
         pid INT REFERENCES parent(id) ON DELETE SET NULL)",
    )
    .await;
    ok(&mut s, "INSERT INTO parent VALUES (1)").await;
    ok(&mut s, "INSERT INTO child VALUES (1, 1)").await;
    ok(&mut s, "DELETE FROM parent WHERE id = 1").await;
    let r = ok(&mut s, "SELECT pid FROM child WHERE id = 1").await;
    match &r[0] {
        ExecResult::Rows { rows, .. } => assert_eq!(rows[0][0].to_text(), None),
        _ => panic!("expected rows"),
    }

    // SET NULL into a NOT NULL column surfaces the usual 23502.
    ok(
        &mut s,
        "CREATE TABLE strict_child (id INT PRIMARY KEY, \
         pid INT NOT NULL REFERENCES parent(id) ON DELETE SET NULL)",
    )
    .await;
    ok(&mut s, "INSERT INTO parent VALUES (2)").await;
    ok(&mut s, "INSERT INTO strict_child VALUES (1, 2)").await;
    assert_eq!(
        err_code(&mut s, "DELETE FROM parent WHERE id = 2").await,
        "23502"
    );
}

#[tokio::test]
async fn foreign_key_on_delete_set_default() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE parent (id INT PRIMARY KEY)").await;
    ok(
        &mut s,
        "CREATE TABLE child (id INT PRIMARY KEY, \
         pid INT DEFAULT 1 REFERENCES parent(id) ON DELETE SET DEFAULT)",
    )
    .await;
    ok(&mut s, "INSERT INTO parent VALUES (1), (2)").await;
    ok(&mut s, "INSERT INTO child VALUES (10, 2)").await;

    // Re-satisfying: the default (1) references a surviving parent.
    ok(&mut s, "DELETE FROM parent WHERE id = 2").await;
    assert_eq!(
        scalar_i64(&mut s, "SELECT pid FROM child WHERE id = 10").await,
        1
    );
    // Violating, default == deleted key: the internal update is a no-op, so
    // the post-SET DEFAULT re-check fires (PostgreSQL raises the parent-side
    // shape here).
    let err = s
        .execute("DELETE FROM parent WHERE id = 1")
        .await
        .unwrap_err();
    assert_eq!(err.sqlstate(), "23503");
    assert_eq!(
        err.to_string(),
        "update or delete on table \"parent\" violates foreign key constraint \
         \"child_pid_fkey\" on table \"child\""
    );

    // Violating, default != any parent: the internal update re-checks the FK
    // and fails with the insert-or-update shape.
    ok(
        &mut s,
        "CREATE TABLE child_bad (id INT PRIMARY KEY, \
         pid INT DEFAULT 999 REFERENCES parent(id) ON DELETE SET DEFAULT)",
    )
    .await;
    ok(&mut s, "DELETE FROM child WHERE id = 10").await;
    ok(&mut s, "INSERT INTO child_bad VALUES (1, 1)").await;
    let err = s
        .execute("DELETE FROM parent WHERE id = 1")
        .await
        .unwrap_err();
    assert_eq!(err.sqlstate(), "23503");
    assert_eq!(
        err.to_string(),
        "insert or update on table \"child_bad\" violates foreign key constraint \
         \"child_bad_pid_fkey\""
    );
    ok(&mut s, "DELETE FROM child_bad").await;

    // A column without a default becomes NULL, which satisfies MATCH SIMPLE.
    ok(
        &mut s,
        "CREATE TABLE loose_child (id INT PRIMARY KEY, \
         pid INT REFERENCES parent(id) ON DELETE SET DEFAULT)",
    )
    .await;
    ok(&mut s, "DELETE FROM child WHERE id = 10").await;
    ok(&mut s, "INSERT INTO loose_child VALUES (1, 1)").await;
    ok(&mut s, "DELETE FROM parent WHERE id = 1").await;
    let r = ok(&mut s, "SELECT pid FROM loose_child WHERE id = 1").await;
    match &r[0] {
        ExecResult::Rows { rows, .. } => assert_eq!(rows[0][0].to_text(), None),
        _ => panic!("expected rows"),
    }
}

#[tokio::test]
async fn foreign_key_on_update_actions() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE parent (id INT PRIMARY KEY)").await;
    ok(
        &mut s,
        "CREATE TABLE child (id INT PRIMARY KEY, \
         pid INT REFERENCES parent(id) ON UPDATE CASCADE)",
    )
    .await;
    ok(&mut s, "INSERT INTO parent VALUES (1), (2)").await;
    ok(&mut s, "INSERT INTO child VALUES (1, 1)").await;

    // CASCADE rewrites the child's FK value to the new key.
    ok(&mut s, "UPDATE parent SET id = 5 WHERE id = 1").await;
    assert_eq!(
        scalar_i64(&mut s, "SELECT pid FROM child WHERE id = 1").await,
        5
    );
    // Updating an unreferenced parent row (or a non-key column) fires nothing.
    ok(&mut s, "UPDATE parent SET id = 7 WHERE id = 2").await;
    assert_eq!(
        scalar_i64(&mut s, "SELECT pid FROM child WHERE id = 1").await,
        5
    );

    // SET NULL on update.
    ok(
        &mut s,
        "CREATE TABLE child_null (id INT PRIMARY KEY, \
         pid INT REFERENCES parent(id) ON UPDATE SET NULL)",
    )
    .await;
    ok(&mut s, "INSERT INTO child_null VALUES (1, 7)").await;
    ok(&mut s, "UPDATE parent SET id = 8 WHERE id = 7").await;
    let r = ok(&mut s, "SELECT pid FROM child_null WHERE id = 1").await;
    match &r[0] {
        ExecResult::Rows { rows, .. } => assert_eq!(rows[0][0].to_text(), None),
        _ => panic!("expected rows"),
    }
}

#[tokio::test]
async fn foreign_key_cascade_is_atomic_and_rolls_back() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE parent (id INT PRIMARY KEY)").await;
    ok(
        &mut s,
        "CREATE TABLE c_cascade (id INT PRIMARY KEY, \
         pid INT REFERENCES parent(id) ON DELETE CASCADE)",
    )
    .await;
    ok(
        &mut s,
        "CREATE TABLE c_restrict (id INT PRIMARY KEY, \
         pid INT REFERENCES parent(id) ON DELETE RESTRICT)",
    )
    .await;
    ok(&mut s, "INSERT INTO parent VALUES (1)").await;
    ok(&mut s, "INSERT INTO c_cascade VALUES (1, 1)").await;
    ok(&mut s, "INSERT INTO c_restrict VALUES (1, 1)").await;

    // The RESTRICT sibling aborts the whole statement: the cascade into
    // c_cascade must not be half-applied.
    assert_eq!(
        err_code(&mut s, "DELETE FROM parent WHERE id = 1").await,
        "23503"
    );
    assert_eq!(scalar_i64(&mut s, "SELECT count(*) FROM parent").await, 1);
    assert_eq!(
        scalar_i64(&mut s, "SELECT count(*) FROM c_cascade").await,
        1
    );

    // A rolled-back transaction undoes the cascade with the delete.
    ok(&mut s, "DELETE FROM c_restrict").await;
    ok(&mut s, "BEGIN").await;
    ok(&mut s, "DELETE FROM parent WHERE id = 1").await;
    assert_eq!(
        scalar_i64(&mut s, "SELECT count(*) FROM c_cascade").await,
        0
    );
    ok(&mut s, "ROLLBACK").await;
    assert_eq!(scalar_i64(&mut s, "SELECT count(*) FROM parent").await, 1);
    assert_eq!(
        scalar_i64(&mut s, "SELECT count(*) FROM c_cascade").await,
        1
    );
}

#[tokio::test]
async fn composite_foreign_key_match_simple() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE cp (a INT, b INT, PRIMARY KEY (a, b))").await;
    ok(
        &mut s,
        "CREATE TABLE cc (id INT PRIMARY KEY, x INT, y INT, \
         FOREIGN KEY (x, y) REFERENCES cp (a, b))",
    )
    .await;
    ok(&mut s, "INSERT INTO cp VALUES (1, 2)").await;
    ok(&mut s, "INSERT INTO cc VALUES (1, 1, 2)").await;
    // MATCH SIMPLE: one NULL component passes, even if the rest dangles.
    ok(&mut s, "INSERT INTO cc VALUES (2, 9, NULL)").await;
    // A fully non-NULL key must match a parent row.
    assert_eq!(
        err_code(&mut s, "INSERT INTO cc VALUES (3, 1, 3)").await,
        "23503"
    );
    // The composite key is guarded on the parent side too.
    assert_eq!(err_code(&mut s, "DELETE FROM cp").await, "23503");
}

#[tokio::test]
async fn match_partial_rejected_deferrable_and_match_full_accepted() {
    // FK `DEFERRABLE`/`INITIALLY DEFERRED` and `MATCH FULL` are implemented
    // (see `tests/sql_fk_advanced.rs` for their behavioral tests); real
    // PostgreSQL parity gaps that remain typed `0A000`:
    //   * `MATCH PARTIAL` — PostgreSQL itself has never implemented it either
    //     ("MATCH PARTIAL not yet implemented"), so this *is* parity.
    //   * `DEFERRABLE` on `UNIQUE`/`PRIMARY KEY` — this engine only runs
    //     deferred checking for foreign keys, not the unique-index-based
    //     mechanism PostgreSQL uses for deferrable UNIQUE/PK.
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE dp (id INT PRIMARY KEY)").await;
    for sql in [
        "CREATE TABLE dc (id INT PRIMARY KEY, pid INT REFERENCES dp(id) MATCH PARTIAL)",
        "CREATE TABLE dc (id INT PRIMARY KEY, pid INT, \
         FOREIGN KEY (pid) REFERENCES dp(id) MATCH PARTIAL)",
        "CREATE TABLE dc (id INT PRIMARY KEY, pid INT UNIQUE DEFERRABLE)",
        "CREATE TABLE dc (id INT PRIMARY KEY DEFERRABLE)",
    ] {
        assert_eq!(err_code(&mut s, sql).await, "0A000", "for `{sql}`");
    }
    // DEFERRABLE / INITIALLY DEFERRED / MATCH FULL on a foreign key are all
    // accepted now (typed, enforced — not silently downgraded).
    for sql in [
        "CREATE TABLE dc1 (id INT PRIMARY KEY, pid INT REFERENCES dp(id) DEFERRABLE)",
        "CREATE TABLE dc2 (id INT PRIMARY KEY, \
         pid INT REFERENCES dp(id) DEFERRABLE INITIALLY DEFERRED)",
        "CREATE TABLE dc3 (id INT PRIMARY KEY, pid INT REFERENCES dp(id) INITIALLY DEFERRED)",
        "CREATE TABLE dc4 (id INT PRIMARY KEY, pid INT, \
         FOREIGN KEY (pid) REFERENCES dp(id) DEFERRABLE)",
        "CREATE TABLE dc5 (id INT PRIMARY KEY, pid INT REFERENCES dp(id) MATCH FULL)",
        "CREATE TABLE dc6 (id INT PRIMARY KEY, pid INT REFERENCES dp(id) NOT DEFERRABLE)",
        // `NOT DEFERRABLE` + `INITIALLY DEFERRED` is a specific PostgreSQL
        // syntax error ("constraint declared INITIALLY DEFERRED must be
        // DEFERRABLE"), distinct from the generic 0A000 rejections above.
    ] {
        ok(&mut s, sql).await;
    }
    assert_eq!(
        err_code(
            &mut s,
            "CREATE TABLE dc_conflict (id INT PRIMARY KEY, \
             pid INT REFERENCES dp(id) NOT DEFERRABLE INITIALLY DEFERRED)"
        )
        .await,
        "42601"
    );
    // The defaults the engine implements are accepted.
    ok(
        &mut s,
        "CREATE TABLE dc_ok (id INT PRIMARY KEY, \
         pid INT REFERENCES dp(id) NOT DEFERRABLE INITIALLY IMMEDIATE)",
    )
    .await;
}

#[tokio::test]
async fn referenced_parent_guarded_on_drop_and_truncate() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE parent (id INT PRIMARY KEY)").await;
    ok(
        &mut s,
        "CREATE TABLE child (id INT PRIMARY KEY, pid INT REFERENCES parent(id))",
    )
    .await;
    // TRUNCATE of a referenced parent is rejected (PostgreSQL 0A000) unless
    // the referencing table is truncated in the same statement.
    assert_eq!(err_code(&mut s, "TRUNCATE parent").await, "0A000");
    ok(&mut s, "TRUNCATE parent, child").await;
    // A referencing constraint blocks DROP TABLE (PostgreSQL 2BP01) ...
    assert_eq!(err_code(&mut s, "DROP TABLE parent").await, "2BP01");
    // ... and CASCADE drops the dependent constraint instead, after which the
    // child accepts previously-dangling values.
    ok(&mut s, "DROP TABLE parent CASCADE").await;
    ok(&mut s, "INSERT INTO child VALUES (1, 999)").await;
    // Dropping parent and child together also works.
    ok(&mut s, "CREATE TABLE p2 (id INT PRIMARY KEY)").await;
    ok(
        &mut s,
        "CREATE TABLE c2 (id INT PRIMARY KEY, pid INT REFERENCES p2(id))",
    )
    .await;
    ok(&mut s, "DROP TABLE p2, c2").await;
    // Foreign keys referencing a missing table are rejected at DDL time now
    // that they are enforced (PostgreSQL 42P01).
    assert_eq!(
        err_code(
            &mut s,
            "CREATE TABLE orphan (id INT PRIMARY KEY, pid INT REFERENCES nowhere(id))"
        )
        .await,
        "42P01"
    );
}

#[tokio::test]
async fn foreign_keys_introspectable_with_declared_actions() {
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE parent (id INT PRIMARY KEY)").await;
    ok(
        &mut s,
        "CREATE TABLE child (id INT PRIMARY KEY, pid INT REFERENCES parent(id))",
    )
    .await;
    ok(
        &mut s,
        "CREATE TABLE child_cascade (id INT PRIMARY KEY, \
         pid INT REFERENCES parent(id) ON DELETE CASCADE)",
    )
    .await;
    // pg_constraint reports the declared (and enforced) referential actions:
    // contype 'f'; confdeltype 'c' for the declared CASCADE and 'a'
    // (NO ACTION) otherwise; confupdtype 'a'.
    let r = ok(
        &mut s,
        "SELECT conname, contype, confupdtype, confdeltype \
         FROM pg_constraint WHERE contype = 'f' ORDER BY conname",
    )
    .await;
    let rows = match &r[0] {
        ExecResult::Rows { rows, .. } => rows,
        _ => panic!("expected rows"),
    };
    let text = |row: &[guardian_db::relational::SqlValue]| -> Vec<String> {
        row.iter()
            .map(|v| v.to_text().unwrap_or_default())
            .collect()
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(
        text(&rows[0]),
        ["child_cascade_pid_fkey", "f", "a", "c"]
            .map(str::to_string)
            .to_vec()
    );
    assert_eq!(
        text(&rows[1]),
        ["child_pid_fkey", "f", "a", "a"]
            .map(str::to_string)
            .to_vec()
    );
}

// ---------------------------------------------------------------------------
// Intended-but-unimplemented features (ignored; encode the target behaviour).
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Decoded-table cache (generation-validated) — correctness of the reload
// optimization: repeated reads must still observe writes, cross-session
// writes must be visible, and DDL (which the storage change counter does not
// track) must invalidate cached views.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cache_reflects_writes_and_ddl() {
    let db = Arc::new(Database::new(Arc::new(MemoryStorage::new()), "app"));
    let mut s = Session::new(db.clone(), "guardian");
    ok(
        &mut s,
        "CREATE TABLE t (id INT PRIMARY KEY, v INT, tag TEXT)",
    )
    .await;
    ok(&mut s, "INSERT INTO t VALUES (1, 10, 'a'), (2, 20, 'b')").await;
    // Warm the cache, then confirm a subsequent write is observed (the
    // generation bump invalidates the cached view).
    assert_eq!(scalar_i64(&mut s, "SELECT count(*) FROM t").await, 2);
    assert_eq!(scalar_i64(&mut s, "SELECT count(*) FROM t").await, 2);
    ok(&mut s, "INSERT INTO t VALUES (3, 30, 'c')").await;
    assert_eq!(scalar_i64(&mut s, "SELECT count(*) FROM t").await, 3);
    ok(&mut s, "UPDATE t SET v = 99 WHERE id = 1").await;
    assert_eq!(scalar_i64(&mut s, "SELECT v FROM t WHERE id = 1").await, 99);
    ok(&mut s, "DELETE FROM t WHERE id = 2").await;
    assert_eq!(scalar_i64(&mut s, "SELECT count(*) FROM t").await, 2);

    // A second session over the same Database sees the first session's
    // committed writes (shared cache, generation-keyed).
    let mut s2 = Session::new(db.clone(), "guardian");
    assert_eq!(scalar_i64(&mut s2, "SELECT count(*) FROM t").await, 2);
    ok(&mut s2, "INSERT INTO t VALUES (4, 40, 'd')").await;
    assert_eq!(scalar_i64(&mut s, "SELECT count(*) FROM t").await, 3);

    // DDL that leaves rows untouched (CREATE INDEX) must invalidate the cache
    // so index-dependent planning sees the new index. Prove it functionally:
    // a unique index created now must reject a duplicate.
    ok(&mut s, "CREATE UNIQUE INDEX t_tag ON t (tag)").await;
    assert_eq!(
        &err_code(&mut s, "INSERT INTO t VALUES (5, 50, 'c')").await,
        "23505"
    );
    // TRUNCATE is a schema-independent row change; still observed.
    ok(&mut s, "TRUNCATE t").await;
    assert_eq!(scalar_i64(&mut s, "SELECT count(*) FROM t").await, 0);
}

#[tokio::test]
async fn cache_transaction_isolation_of_own_writes() {
    let db = Arc::new(Database::new(Arc::new(MemoryStorage::new()), "app"));
    let mut s = Session::new(db, "guardian");
    ok(&mut s, "CREATE TABLE t (id INT PRIMARY KEY, v INT)").await;
    ok(&mut s, "INSERT INTO t VALUES (1, 10)").await;
    assert_eq!(scalar_i64(&mut s, "SELECT count(*) FROM t").await, 1);
    // Inside a transaction, the statement must see its own staged writes
    // (the overlay path, which bypasses the committed cache).
    ok(&mut s, "BEGIN").await;
    ok(&mut s, "INSERT INTO t VALUES (2, 20)").await;
    assert_eq!(scalar_i64(&mut s, "SELECT count(*) FROM t").await, 2);
    ok(&mut s, "UPDATE t SET v = 11 WHERE id = 1").await;
    assert_eq!(scalar_i64(&mut s, "SELECT v FROM t WHERE id = 1").await, 11);
    ok(&mut s, "ROLLBACK").await;
    // After rollback the staged writes vanish and the cached committed view
    // is correct again.
    assert_eq!(scalar_i64(&mut s, "SELECT count(*) FROM t").await, 1);
    assert_eq!(scalar_i64(&mut s, "SELECT v FROM t WHERE id = 1").await, 10);
}

// ---------------------------------------------------------------------------
// Regression: decoded-table cache correctness under DDL-in-transaction, and
// EXPLAIN as a side-effect-free inspection (code-review findings, 2026-07-24).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cache_reflects_ddl_within_a_transaction() {
    // A metadata-only DDL inside a transaction must be visible to the next
    // statement in the same transaction — the cache must not serve the
    // pre-DDL table meta.
    let mut s = session().await;
    ok(&mut s, "CREATE TABLE t (a INT PRIMARY KEY)").await;
    ok(&mut s, "INSERT INTO t VALUES (1)").await;
    // Warm the shared cache for t's collection.
    assert_eq!(scalar_i64(&mut s, "SELECT count(*) FROM t").await, 1);

    ok(&mut s, "BEGIN").await;
    ok(&mut s, "ALTER TABLE t ADD COLUMN b INT").await;
    // The new column must be *visible* within the transaction: before the fix
    // the stale cached table meta made this fail with "column b does not
    // exist" (42703). It reads NULL for the pre-existing row — the point is
    // that it resolves, not what value it has.
    assert_eq!(
        rows_text(&mut s, "SELECT b FROM t WHERE a = 1").await,
        vec![vec!["NULL".to_string()]]
    );
    // And it is fully usable within the same transaction: write then read.
    ok(&mut s, "UPDATE t SET b = 7 WHERE a = 1").await;
    assert_eq!(scalar_i64(&mut s, "SELECT b FROM t WHERE a = 1").await, 7);
    // A second DDL in the same transaction is likewise immediately visible.
    ok(&mut s, "ALTER TABLE t ADD COLUMN c TEXT").await;
    ok(&mut s, "UPDATE t SET c = 'x' WHERE a = 1").await;
    assert_eq!(
        rows_text(&mut s, "SELECT c FROM t WHERE a = 1").await,
        vec![vec!["x".to_string()]]
    );
    ok(&mut s, "COMMIT").await;
    // Visible after commit to a fresh statement too.
    assert_eq!(scalar_i64(&mut s, "SELECT b FROM t WHERE a = 1").await, 7);

    // Rollback of a DDL leaves the committed schema intact (no cache poison
    // from the transaction's uncommitted meta).
    ok(&mut s, "BEGIN").await;
    ok(&mut s, "ALTER TABLE t ADD COLUMN d INT").await;
    ok(&mut s, "SELECT d FROM t WHERE a = 1").await; // visible inside the txn
    ok(&mut s, "ROLLBACK").await;
    // `d` must be gone now — and no stale cached view resurrects it.
    assert_eq!(&err_code(&mut s, "SELECT d FROM t").await, "42703");
}

#[tokio::test]
async fn explain_select_for_update_takes_no_row_locks() {
    use std::sync::Arc;
    // Two sessions over one Database; a lock taken by session A blocks B.
    let db = Arc::new(Database::new(Arc::new(MemoryStorage::new()), "app"));
    let mut a = Session::new(db.clone(), "guardian");
    ok(&mut a, "CREATE TABLE t (id INT PRIMARY KEY)").await;
    ok(&mut a, "INSERT INTO t VALUES (1)").await;

    // EXPLAIN of a FOR UPDATE query must NOT acquire the row lock (inspection
    // only). If it did, a competing NOWAIT lock in another session would fail.
    let plan = rows_text(&mut a, "EXPLAIN SELECT id FROM t WHERE id = 1 FOR UPDATE").await;
    assert!(!plan.is_empty(), "EXPLAIN produced a plan");

    // A second session locking the same row NOWAIT must succeed — proving the
    // EXPLAIN above left no lock behind.
    let mut b = Session::new(db, "guardian");
    ok(&mut b, "BEGIN").await;
    ok(&mut b, "SELECT id FROM t WHERE id = 1 FOR UPDATE NOWAIT").await;
    ok(&mut b, "ROLLBACK").await;
}
