//! End-to-end wire-protocol tests using a real PostgreSQL client
//! (`tokio-postgres`) against the GuardianDB gateway.
//!
//! These exercise startup/auth, the simple query protocol, row descriptions,
//! data rows, command tags, multi-statement queries, transactions and
//! SQLSTATE error propagation over an actual TCP socket.

use std::sync::Arc;

use guardian_pgwire::serve_on;
use guardian_sql::engine::Database;
use guardian_sql::MemoryStorage;
use tokio::net::TcpListener;
use tokio_postgres::{NoTls, SimpleQueryMessage};

/// Start a gateway on an ephemeral port and return a connected client.
async fn connect() -> tokio_postgres::Client {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let db = Arc::new(Database::new(Arc::new(MemoryStorage::new()), "app"));
    tokio::spawn(async move {
        let _ = serve_on(listener, db, "guardian").await;
    });

    let conninfo = format!("host=127.0.0.1 port={port} user=guardian password=guardian dbname=app");
    let (client, connection) = tokio_postgres::connect(&conninfo, NoTls).await.unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// Collect the textual rows from a simple query.
fn rows(messages: &[SimpleQueryMessage]) -> Vec<Vec<Option<String>>> {
    messages
        .iter()
        .filter_map(|m| match m {
            SimpleQueryMessage::Row(r) => {
                let cols = (0..r.len()).map(|i| r.get(i).map(|s| s.to_string())).collect();
                Some(cols)
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn simple_query_crud() {
    let client = connect().await;
    client
        .simple_query("CREATE TABLE users (id INT PRIMARY KEY, name TEXT NOT NULL)")
        .await
        .unwrap();
    client
        .simple_query("INSERT INTO users VALUES (1, 'Alice'), (2, 'Bob')")
        .await
        .unwrap();
    let res = client
        .simple_query("SELECT id, name FROM users ORDER BY id")
        .await
        .unwrap();
    let grid = rows(&res);
    assert_eq!(grid.len(), 2);
    assert_eq!(grid[0][0].as_deref(), Some("1"));
    assert_eq!(grid[0][1].as_deref(), Some("Alice"));
    assert_eq!(grid[1][1].as_deref(), Some("Bob"));
}

#[tokio::test]
async fn command_tags_and_counts() {
    let client = connect().await;
    client.simple_query("CREATE TABLE t (id INT PRIMARY KEY)").await.unwrap();
    let res = client.simple_query("INSERT INTO t VALUES (1),(2),(3)").await.unwrap();
    let tag = res.iter().find_map(|m| match m {
        SimpleQueryMessage::CommandComplete(n) => Some(*n),
        _ => None,
    });
    assert_eq!(tag, Some(3));
}

#[tokio::test]
async fn sqlstate_error_propagates() {
    let client = connect().await;
    let err = client.simple_query("SELECT * FROM does_not_exist").await.unwrap_err();
    let code = err.code().map(|c| c.code().to_string());
    assert_eq!(code.as_deref(), Some("42P01"));
}

#[tokio::test]
async fn unique_violation_over_wire() {
    let client = connect().await;
    client.simple_query("CREATE TABLE u (id INT PRIMARY KEY)").await.unwrap();
    client.simple_query("INSERT INTO u VALUES (1)").await.unwrap();
    let err = client.simple_query("INSERT INTO u VALUES (1)").await.unwrap_err();
    assert_eq!(err.code().map(|c| c.code().to_string()).as_deref(), Some("23505"));
}

#[tokio::test]
async fn transaction_rollback_over_wire() {
    let client = connect().await;
    client.simple_query("CREATE TABLE t (id INT PRIMARY KEY)").await.unwrap();
    client
        .simple_query("BEGIN; INSERT INTO t VALUES (1); ROLLBACK")
        .await
        .unwrap();
    let res = client.simple_query("SELECT count(*) FROM t").await.unwrap();
    assert_eq!(rows(&res)[0][0].as_deref(), Some("0"));
}

#[tokio::test]
async fn introspection_over_wire() {
    let client = connect().await;
    client
        .simple_query("CREATE TABLE products (id SERIAL PRIMARY KEY, name VARCHAR(100))")
        .await
        .unwrap();
    let res = client
        .simple_query(
            "SELECT table_name FROM information_schema.tables WHERE table_name = 'products'",
        )
        .await
        .unwrap();
    assert_eq!(rows(&res)[0][0].as_deref(), Some("products"));
}

#[tokio::test]
async fn expressions_over_wire() {
    let client = connect().await;
    let res = client.simple_query("SELECT 1 + 1 AS two, upper('hi') AS greeting").await.unwrap();
    let grid = rows(&res);
    assert_eq!(grid[0][0].as_deref(), Some("2"));
    assert_eq!(grid[0][1].as_deref(), Some("HI"));
}
