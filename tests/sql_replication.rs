//! Distributed conformance: PostgreSQL-style SQL over two replicating GuardianDB
//! peers. Schema (catalog) and row writes made via SQL on one peer become
//! visible via SQL on the other after replication.
//!
//! Enabled by the `sql` feature: `cargo test --features sql --test sql_replication`.
#![cfg(feature = "sql")]

mod common;

use common::{connect_nodes, wait_for_propagation, TestNode};
use guardian_db::sql::open_sql;
use guardian_db::sql::engine::Session;
use guardian_db::sql::ExecResult;

fn rows(r: ExecResult) -> Vec<Vec<String>> {
    match r {
        ExecResult::Rows { rows, .. } => rows
            .into_iter()
            .map(|row| row.into_iter().map(|v| v.to_text().unwrap_or_default()).collect())
            .collect(),
        ExecResult::Command { tag } => panic!("expected rows, got {tag}"),
    }
}

// Distributed-conformance target. Raw document replication between two peers
// works (see tests/integration_replication.rs), but making the *relational*
// view (catalog + rows) converge deterministically across peers additionally
// requires (a) the two `open_sql` document stores to share an iroh-docs
// namespace and (b) the relational engine to observe background replication
// (the local index updates on `refresh()`/load, not automatically). This is the
// same in-progress distributed-coordination work tracked for strict mode in
// docs/postgres-compat.md. Run with `--ignored` to exercise the intended flow.
#[tokio::test]
#[ignore = "distributed SQL replication: requires shared-namespace stores + background index refresh (in progress)"]
async fn sql_schema_and_rows_replicate_across_two_peers() {
    let node1 = TestNode::new("sql_repl_a").await.expect("node1");
    let node2 = TestNode::new("sql_repl_b").await.expect("node2");
    connect_nodes(&node1, &node2).await.expect("connect");

    // Both peers open the same logical relational database (shared document store).
    let db1 = open_sql(&node1.db, "shared-app").await.expect("open db1");
    let db2 = open_sql(&node2.db, "shared-app").await.expect("open db2");
    wait_for_propagation().await;

    // Peer 1 creates the schema and inserts rows via SQL.
    let mut s1 = Session::new(db1, "guardian");
    s1.execute("CREATE TABLE users (id INT PRIMARY KEY, name TEXT NOT NULL)")
        .await
        .expect("create table");
    s1.execute("INSERT INTO users VALUES (1, 'Alice'), (2, 'Bob')")
        .await
        .expect("insert");

    // Drive bidirectional synchronization.
    node1
        .db
        .connect_to_peer(node2.iroh.node_id())
        .await
        .expect("n1->n2");
    node2
        .db
        .connect_to_peer(node1.iroh.node_id())
        .await
        .expect("n2->n1");
    wait_for_propagation().await;
    wait_for_propagation().await;

    // Re-sync peer 2's local index from the replicated documents, then read.
    db2.storage().refresh().await.expect("refresh peer 2 index");

    // Peer 2 sees the replicated schema and rows through a fresh SQL session.
    let mut s2 = Session::new(db2.clone(), "guardian");
    let mut r = s2
        .execute("SELECT id, name FROM users ORDER BY id")
        .await
        .expect("select on peer 2");
    let grid = rows(r.pop().unwrap());
    assert_eq!(grid.len(), 2, "peer 2 should see both replicated rows");
    assert_eq!(grid[0][1], "Alice");
    assert_eq!(grid[1][1], "Bob");
}
