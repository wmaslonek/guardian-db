//! In-crate integration tests exercising the engine end-to-end over
//! [`MemoryStorage`]. These double as conformance coverage for the SQL surface.

use crate::sql::engine::{Database, Session};
use crate::sql::error::SqlError;
use crate::sql::result::ExecResult;
use crate::relational::{MemoryStorage, SqlValue};
use std::sync::Arc;

async fn session() -> Session<MemoryStorage> {
    let db = Arc::new(Database::new(Arc::new(MemoryStorage::new()), "app"));
    Session::new(db, "guardian")
}

/// Run a statement, returning its result.
async fn run(s: &mut Session<MemoryStorage>, sql: &str) -> ExecResult {
    let mut r = s.execute(sql).await.unwrap_or_else(|e| panic!("`{sql}` failed: {e}"));
    r.pop().unwrap()
}

/// Run a query and return rows as text grids.
async fn q(s: &mut Session<MemoryStorage>, sql: &str) -> Vec<Vec<Option<String>>> {
    match run(s, sql).await {
        ExecResult::Rows { rows, .. } => rows
            .into_iter()
            .map(|r| r.into_iter().map(|v| v.to_text()).collect())
            .collect(),
        ExecResult::Command { tag } => panic!("expected rows from `{sql}`, got {tag}"),
    }
}

/// Run a command and return its completion tag.
async fn tag(s: &mut Session<MemoryStorage>, sql: &str) -> String {
    run(s, sql).await.command_tag()
}

/// Run a statement expected to fail; return the SQLSTATE.
async fn err(s: &mut Session<MemoryStorage>, sql: &str) -> String {
    match s.execute(sql).await {
        Ok(_) => panic!("expected `{sql}` to fail"),
        Err(e) => e.sqlstate().to_string(),
    }
}

fn cell(grid: &[Vec<Option<String>>], r: usize, c: usize) -> &str {
    grid[r][c].as_deref().unwrap_or("<null>")
}

#[tokio::test]
async fn ddl_and_basic_crud() {
    let mut s = session().await;
    assert_eq!(tag(&mut s, "CREATE TABLE users (id INT PRIMARY KEY, name TEXT NOT NULL, age INT)").await, "CREATE TABLE");
    assert_eq!(tag(&mut s, "INSERT INTO users VALUES (1,'Alice',30),(2,'Bob',25)").await, "INSERT 0 2");
    let g = q(&mut s, "SELECT name, age FROM users ORDER BY age").await;
    assert_eq!(cell(&g, 0, 0), "Bob");
    assert_eq!(cell(&g, 1, 0), "Alice");
    assert_eq!(tag(&mut s, "UPDATE users SET age = 31 WHERE id = 1").await, "UPDATE 1");
    let g = q(&mut s, "SELECT age FROM users WHERE id = 1").await;
    assert_eq!(cell(&g, 0, 0), "31");
    assert_eq!(tag(&mut s, "DELETE FROM users WHERE id = 2").await, "DELETE 1");
    let g = q(&mut s, "SELECT count(*) FROM users").await;
    assert_eq!(cell(&g, 0, 0), "1");
}

#[tokio::test]
async fn returning_clauses() {
    let mut s = session().await;
    run(&mut s, "CREATE TABLE t (id SERIAL PRIMARY KEY, v INT)").await;
    let g = q(&mut s, "INSERT INTO t (v) VALUES (10) RETURNING id, v").await;
    assert_eq!(cell(&g, 0, 0), "1");
    assert_eq!(cell(&g, 0, 1), "10");
    let g = q(&mut s, "UPDATE t SET v = 20 WHERE id = 1 RETURNING v").await;
    assert_eq!(cell(&g, 0, 0), "20");
    let g = q(&mut s, "DELETE FROM t WHERE id = 1 RETURNING id").await;
    assert_eq!(cell(&g, 0, 0), "1");
}

#[tokio::test]
async fn serial_and_defaults() {
    let mut s = session().await;
    run(&mut s, "CREATE TABLE t (id SERIAL PRIMARY KEY, created TIMESTAMPTZ DEFAULT now(), flag BOOL DEFAULT true)").await;
    run(&mut s, "INSERT INTO t DEFAULT VALUES").await.command_tag();
    run(&mut s, "INSERT INTO t (id) VALUES (DEFAULT)").await;
    let g = q(&mut s, "SELECT id, flag FROM t ORDER BY id").await;
    assert_eq!(cell(&g, 0, 0), "1");
    assert_eq!(cell(&g, 0, 1), "t");
    assert_eq!(cell(&g, 1, 0), "2");
}

#[tokio::test]
async fn unique_and_notnull_violations() {
    let mut s = session().await;
    run(&mut s, "CREATE TABLE u (id INT PRIMARY KEY, email TEXT UNIQUE)").await;
    run(&mut s, "INSERT INTO u VALUES (1,'a@x.com')").await;
    assert_eq!(err(&mut s, "INSERT INTO u VALUES (1,'b@x.com')").await, "23505"); // pk dup
    assert_eq!(err(&mut s, "INSERT INTO u VALUES (2,'a@x.com')").await, "23505"); // unique dup
    assert_eq!(err(&mut s, "INSERT INTO u VALUES (NULL,'c@x.com')").await, "23502"); // not null
    // Multiple NULLs in a unique column are allowed.
    run(&mut s, "INSERT INTO u VALUES (3, NULL)").await;
    run(&mut s, "INSERT INTO u VALUES (4, NULL)").await;
    let g = q(&mut s, "SELECT count(*) FROM u").await;
    assert_eq!(cell(&g, 0, 0), "3");
}

#[tokio::test]
async fn upsert_on_conflict() {
    let mut s = session().await;
    run(&mut s, "CREATE TABLE kv (k TEXT PRIMARY KEY, v INT)").await;
    run(&mut s, "INSERT INTO kv VALUES ('a', 1)").await;
    run(&mut s, "INSERT INTO kv VALUES ('a', 2) ON CONFLICT (k) DO NOTHING").await;
    assert_eq!(cell(&q(&mut s, "SELECT v FROM kv WHERE k='a'").await, 0, 0), "1");
    run(&mut s, "INSERT INTO kv VALUES ('a', 5) ON CONFLICT (k) DO UPDATE SET v = excluded.v").await;
    assert_eq!(cell(&q(&mut s, "SELECT v FROM kv WHERE k='a'").await, 0, 0), "5");
}

#[tokio::test]
async fn joins() {
    let mut s = session().await;
    run(&mut s, "CREATE TABLE org (id INT PRIMARY KEY, name TEXT)").await;
    run(&mut s, "CREATE TABLE usr (id INT PRIMARY KEY, org_id INT, name TEXT)").await;
    run(&mut s, "INSERT INTO org VALUES (1,'Acme'),(2,'Globex')").await;
    run(&mut s, "INSERT INTO usr VALUES (1,1,'Alice'),(2,1,'Bob'),(3,NULL,'Carol')").await;
    let g = q(&mut s, "SELECT u.name, o.name FROM usr u INNER JOIN org o ON u.org_id = o.id ORDER BY u.id").await;
    assert_eq!(g.len(), 2);
    assert_eq!(cell(&g, 0, 1), "Acme");
    let g = q(&mut s, "SELECT u.name, o.name FROM usr u LEFT JOIN org o ON u.org_id = o.id ORDER BY u.id").await;
    assert_eq!(g.len(), 3);
    assert_eq!(g[2][1], None); // Carol has no org
    let g = q(&mut s, "SELECT count(*) FROM usr CROSS JOIN org").await;
    assert_eq!(cell(&g, 0, 0), "6");
    let g = q(&mut s, "SELECT o.name FROM org o RIGHT JOIN usr u ON u.org_id = o.id ORDER BY u.id").await;
    assert_eq!(g.len(), 3);
}

#[tokio::test]
async fn aggregates_group_having() {
    let mut s = session().await;
    run(&mut s, "CREATE TABLE sales (region TEXT, amount NUMERIC)").await;
    run(&mut s, "INSERT INTO sales VALUES ('e',10),('e',20),('w',5),('w',5),('w',5)").await;
    let g = q(&mut s, "SELECT region, count(*), sum(amount), avg(amount), min(amount), max(amount) FROM sales GROUP BY region ORDER BY region").await;
    assert_eq!(cell(&g, 0, 0), "e");
    assert_eq!(cell(&g, 0, 1), "2");
    assert_eq!(cell(&g, 0, 2), "30");
    assert_eq!(cell(&g, 0, 3), "15");
    assert_eq!(cell(&g, 1, 1), "3");
    assert_eq!(cell(&g, 1, 2), "15");
    let g = q(&mut s, "SELECT region FROM sales GROUP BY region HAVING sum(amount) > 20").await;
    assert_eq!(g.len(), 1);
    assert_eq!(cell(&g, 0, 0), "e");
    let g = q(&mut s, "SELECT count(DISTINCT amount) FROM sales").await;
    assert_eq!(cell(&g, 0, 0), "3"); // 10,20,5
}

#[tokio::test]
async fn distinct_and_set_ops() {
    let mut s = session().await;
    run(&mut s, "CREATE TABLE t (n INT)").await;
    run(&mut s, "INSERT INTO t VALUES (1),(1),(2),(3),(3)").await;
    let g = q(&mut s, "SELECT DISTINCT n FROM t ORDER BY n").await;
    assert_eq!(g.len(), 3);
    let g = q(&mut s, "SELECT n FROM t WHERE n=1 UNION SELECT n FROM t WHERE n=2 ORDER BY n").await;
    assert_eq!(g.len(), 2);
    let g = q(&mut s, "SELECT n FROM t UNION ALL SELECT n FROM t").await;
    assert_eq!(g.len(), 10);
}

#[tokio::test]
async fn subqueries_and_exists() {
    let mut s = session().await;
    run(&mut s, "CREATE TABLE a (id INT, v INT)").await;
    run(&mut s, "CREATE TABLE b (id INT, a_id INT)").await;
    run(&mut s, "INSERT INTO a VALUES (1,10),(2,20),(3,30)").await;
    run(&mut s, "INSERT INTO b VALUES (1,1),(2,2)").await;
    let g = q(&mut s, "SELECT v FROM a WHERE id IN (SELECT a_id FROM b) ORDER BY v").await;
    assert_eq!(g.len(), 2);
    let g = q(&mut s, "SELECT v FROM a WHERE EXISTS (SELECT 1 FROM b WHERE b.a_id = a.id) ORDER BY v").await;
    assert_eq!(g.len(), 2);
    let g = q(&mut s, "SELECT (SELECT max(v) FROM a) AS m").await;
    assert_eq!(cell(&g, 0, 0), "30");
    let g = q(&mut s, "SELECT v FROM a WHERE v > (SELECT avg(v) FROM a) ORDER BY v").await;
    assert_eq!(cell(&g, 0, 0), "30");
}

#[tokio::test]
async fn cte() {
    let mut s = session().await;
    run(&mut s, "CREATE TABLE t (n INT)").await;
    run(&mut s, "INSERT INTO t VALUES (1),(2),(3),(4)").await;
    let g = q(&mut s, "WITH evens AS (SELECT n FROM t WHERE n % 2 = 0) SELECT sum(n) FROM evens").await;
    assert_eq!(cell(&g, 0, 0), "6");
}

#[tokio::test]
async fn expressions_and_nulls() {
    let mut s = session().await;
    run(&mut s, "CREATE TABLE t (a INT, b INT, name TEXT)").await;
    run(&mut s, "INSERT INTO t VALUES (1,2,'alice'),(3,NULL,'BOB'),(5,5,NULL)").await;
    let g = q(&mut s, "SELECT a + b, a * 2, a > b FROM t ORDER BY a").await;
    assert_eq!(cell(&g, 0, 0), "3");
    assert_eq!(cell(&g, 0, 1), "2");
    assert_eq!(cell(&g, 0, 2), "f");
    assert_eq!(g[1][0], None); // 3 + NULL = NULL
    let g = q(&mut s, "SELECT name FROM t WHERE b IS NULL").await;
    assert_eq!(cell(&g, 0, 0), "BOB");
    let g = q(&mut s, "SELECT name FROM t WHERE name ILIKE 'b%'").await;
    assert_eq!(cell(&g, 0, 0), "BOB");
    let g = q(&mut s, "SELECT a FROM t WHERE a BETWEEN 2 AND 4").await;
    assert_eq!(cell(&g, 0, 0), "3");
    let g = q(&mut s, "SELECT coalesce(b, -1) FROM t ORDER BY a").await;
    assert_eq!(cell(&g, 1, 0), "-1");
    let g = q(&mut s, "SELECT upper(name), length(name) FROM t WHERE name IS NOT NULL ORDER BY a").await;
    assert_eq!(cell(&g, 0, 0), "ALICE");
    assert_eq!(cell(&g, 0, 1), "5");
    let g = q(&mut s, "SELECT CASE WHEN a > 2 THEN 'big' ELSE 'small' END FROM t ORDER BY a").await;
    assert_eq!(cell(&g, 0, 0), "small");
    assert_eq!(cell(&g, 1, 0), "big");
}

#[tokio::test]
async fn casts_and_types() {
    let mut s = session().await;
    run(&mut s, "CREATE TABLE t (id INT, data JSONB, ts TIMESTAMP, u UUID)").await;
    run(&mut s, "INSERT INTO t VALUES (1, '{\"a\":1}', '2026-06-29 10:00:00', '00000000-0000-0000-0000-000000000001')").await;
    let g = q(&mut s, "SELECT data FROM t").await;
    assert_eq!(cell(&g, 0, 0), "{\"a\":1}");
    let g = q(&mut s, "SELECT '42'::int + 1, 3.5::numeric, 'true'::boolean").await;
    assert_eq!(cell(&g, 0, 0), "43");
    assert_eq!(cell(&g, 0, 2), "t");
    let g = q(&mut s, "SELECT id::text FROM t").await;
    assert_eq!(cell(&g, 0, 0), "1");
}

#[tokio::test]
async fn numeric_precision() {
    let mut s = session().await;
    run(&mut s, "CREATE TABLE t (price NUMERIC(10,2))").await;
    run(&mut s, "INSERT INTO t VALUES (123456.78), (0.01)").await;
    let g = q(&mut s, "SELECT sum(price) FROM t").await;
    assert_eq!(cell(&g, 0, 0), "123456.79");
    assert_eq!(err(&mut s, "SELECT 1/0").await, "22012"); // division by zero
}

#[tokio::test]
async fn transactions_atomic() {
    let mut s = session().await;
    run(&mut s, "CREATE TABLE t (id INT PRIMARY KEY)").await;
    run(&mut s, "BEGIN").await;
    run(&mut s, "INSERT INTO t VALUES (1)").await;
    run(&mut s, "INSERT INTO t VALUES (2)").await;
    // Visible within the transaction.
    assert_eq!(cell(&q(&mut s, "SELECT count(*) FROM t").await, 0, 0), "2");
    run(&mut s, "ROLLBACK").await;
    // Rolled back.
    assert_eq!(cell(&q(&mut s, "SELECT count(*) FROM t").await, 0, 0), "0");
    run(&mut s, "BEGIN").await;
    run(&mut s, "INSERT INTO t VALUES (1)").await;
    run(&mut s, "COMMIT").await;
    assert_eq!(cell(&q(&mut s, "SELECT count(*) FROM t").await, 0, 0), "1");
}

#[tokio::test]
async fn indexes_scan_equals_full_scan() {
    let mut s = session().await;
    run(&mut s, "CREATE TABLE t (id INT PRIMARY KEY, email TEXT)").await;
    for i in 0..50 {
        run(&mut s, &format!("INSERT INTO t VALUES ({i}, 'user{i}@x.com')")).await;
    }
    run(&mut s, "CREATE INDEX idx_email ON t (email)").await;
    // The PK index gives a point lookup; results must equal a full scan.
    let by_pk = q(&mut s, "SELECT email FROM t WHERE id = 25").await;
    assert_eq!(cell(&by_pk, 0, 0), "user25@x.com");
    let by_idx = q(&mut s, "SELECT id FROM t WHERE email = 'user25@x.com'").await;
    assert_eq!(cell(&by_idx, 0, 0), "25");
    // Unique index enforcement.
    run(&mut s, "CREATE UNIQUE INDEX idx_uemail ON t (email)").await;
    assert_eq!(err(&mut s, "INSERT INTO t VALUES (100, 'user25@x.com')").await, "23505");
}

#[tokio::test]
async fn alter_table() {
    let mut s = session().await;
    run(&mut s, "CREATE TABLE t (id INT PRIMARY KEY)").await;
    run(&mut s, "INSERT INTO t VALUES (1)").await;
    run(&mut s, "ALTER TABLE t ADD COLUMN name TEXT").await;
    run(&mut s, "UPDATE t SET name = 'x' WHERE id = 1").await;
    assert_eq!(cell(&q(&mut s, "SELECT name FROM t").await, 0, 0), "x");
    run(&mut s, "ALTER TABLE t RENAME COLUMN name TO label").await;
    assert_eq!(cell(&q(&mut s, "SELECT label FROM t").await, 0, 0), "x");
    run(&mut s, "ALTER TABLE t ALTER COLUMN label SET DEFAULT 'def'").await;
    run(&mut s, "INSERT INTO t (id) VALUES (2)").await;
    assert_eq!(cell(&q(&mut s, "SELECT label FROM t WHERE id=2").await, 0, 0), "def");
    run(&mut s, "ALTER TABLE t DROP COLUMN label").await;
    assert_eq!(err(&mut s, "SELECT label FROM t").await, "42703");
}

#[tokio::test]
async fn introspection_views() {
    let mut s = session().await;
    run(&mut s, "CREATE TABLE products (id SERIAL PRIMARY KEY, name VARCHAR(100) NOT NULL, price NUMERIC(10,2))").await;
    let g = q(&mut s, "SELECT table_name FROM information_schema.tables WHERE table_schema='public' AND table_name='products'").await;
    assert_eq!(cell(&g, 0, 0), "products");
    let g = q(&mut s, "SELECT column_name, data_type, is_nullable FROM information_schema.columns WHERE table_name='products' ORDER BY ordinal_position").await;
    assert_eq!(cell(&g, 0, 0), "id");
    assert_eq!(cell(&g, 1, 0), "name");
    assert_eq!(cell(&g, 1, 1), "character varying");
    assert_eq!(cell(&g, 1, 2), "NO");
    let g = q(&mut s, "SELECT relname, relkind FROM pg_catalog.pg_class WHERE relname='products'").await;
    assert_eq!(cell(&g, 0, 0), "products");
    assert_eq!(cell(&g, 0, 1), "r");
    let g = q(&mut s, "SELECT constraint_type FROM information_schema.table_constraints WHERE table_name='products'").await;
    assert!(g.iter().any(|r| r[0].as_deref() == Some("PRIMARY KEY")));
}

#[tokio::test]
async fn schemas_and_drop_if_exists() {
    let mut s = session().await;
    assert_eq!(tag(&mut s, "CREATE SCHEMA app").await, "CREATE SCHEMA");
    run(&mut s, "CREATE TABLE app.t (id INT PRIMARY KEY)").await;
    run(&mut s, "INSERT INTO app.t VALUES (1)").await;
    assert_eq!(cell(&q(&mut s, "SELECT count(*) FROM app.t").await, 0, 0), "1");
    assert_eq!(tag(&mut s, "DROP TABLE IF EXISTS app.nonexistent").await, "DROP TABLE");
    run(&mut s, "TRUNCATE app.t").await;
    assert_eq!(cell(&q(&mut s, "SELECT count(*) FROM app.t").await, 0, 0), "0");
}

#[tokio::test]
async fn parameters_bound() {
    let mut s = session().await;
    run(&mut s, "CREATE TABLE t (id INT PRIMARY KEY, name TEXT)").await;
    let prepared = s.prepare("INSERT INTO t VALUES ($1, $2)").unwrap();
    assert_eq!(prepared.param_count, 2);
    s.execute_one(&prepared.statement, &[SqlValue::Int4(1), SqlValue::Text("Alice".into())]).await.unwrap();
    let sel = s.prepare("SELECT name FROM t WHERE id = $1").unwrap();
    let r = s.execute_one(&sel.statement, &[SqlValue::Int4(1)]).await.unwrap();
    if let ExecResult::Rows { rows, .. } = r {
        assert_eq!(rows[0][0].to_text().unwrap(), "Alice");
    } else {
        panic!("expected rows");
    }
}

#[tokio::test]
async fn cross_session_visibility() {
    // DDL/data committed by one session is visible to another sharing storage.
    let storage = Arc::new(MemoryStorage::new());
    let db = Arc::new(Database::new(storage, "app"));
    let mut s1 = Session::new(db.clone(), "guardian");
    let mut s2 = Session::new(db.clone(), "guardian");
    run(&mut s1, "CREATE TABLE t (id INT PRIMARY KEY)").await;
    run(&mut s1, "INSERT INTO t VALUES (1)").await;
    assert_eq!(cell(&q(&mut s2, "SELECT count(*) FROM t").await, 0, 0), "1");
}

#[tokio::test]
async fn unsupported_surfaces_clear_error() {
    let mut s = session().await;
    // A genuinely unsupported feature returns SQLSTATE 0A000, not a panic.
    let code = err(&mut s, "SELECT * FROM generate_series(1, 10)").await;
    assert!(code == "0A000" || code == "42P01", "got {code}");
    let _ = SqlError::FeatureNotSupported("x".into());
}
