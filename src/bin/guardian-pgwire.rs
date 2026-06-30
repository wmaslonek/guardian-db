//! The `guardian-pgwire` gateway binary.
//!
//! Starts a PostgreSQL-compatible server backed by the GuardianDB SQL engine.
//! By default it binds `127.0.0.1:15432` using an in-memory relational store,
//! which is ideal for development, testing, and the TypeORM examples. The
//! GuardianDB-backed (replicated, local-first) gateway lives in the `guardian-db`
//! crate behind the `sql` feature.

use std::sync::Arc;

use guardian_db::pgwire::{serve, DEFAULT_ADDR};
use guardian_db::sql::engine::Database;
use guardian_db::sql::MemoryStorage;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut addr = DEFAULT_ADDR.to_string();
    let mut database = "app".to_string();
    let mut username = "guardian".to_string();
    // Reserved for the GuardianDB-backed gateway (see the `sql` feature in the
    // guardian-db crate). Accepted here so embedders can pass them uniformly;
    // the in-memory gateway ignores them.
    let mut data_path: Option<String> = None;
    let mut consistency = "local".to_string();
    let mut peers: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--addr" | "-a" => addr = args.next().unwrap_or(addr),
            "--database" | "-d" => database = args.next().unwrap_or(database),
            "--username" | "-u" => username = args.next().unwrap_or(username),
            "--path" | "-p" => data_path = args.next(),
            "--consistency" | "-c" => consistency = args.next().unwrap_or(consistency),
            "--peer" => {
                if let Some(p) = args.next() {
                    peers.push(p);
                }
            }
            "--help" | "-h" => {
                println!(
                    "guardian-pgwire — PostgreSQL gateway for GuardianDB\n\n\
                     Usage: guardian-pgwire [--addr 127.0.0.1:15432] [--database app] [--username guardian]\n\n\
                     Connect with:  psql 'postgres://guardian:guardian@127.0.0.1:15432/app'"
                );
                return Ok(());
            }
            other => eprintln!("ignoring unknown argument: {other}"),
        }
    }

    let db = Arc::new(Database::new(Arc::new(MemoryStorage::new()), database.clone()));

    tracing::info!("GuardianDB PostgreSQL gateway listening on {addr} (database \"{database}\")");
    tracing::info!("connect: psql 'postgres://{username}:***@{addr}/{database}'");
    if data_path.is_some() || consistency != "local" || !peers.is_empty() {
        tracing::info!(
            "note: this in-memory gateway ignores --path/--consistency/--peer; \
             use the GuardianDB-backed gateway (guardian-db `sql` feature) for persistence/replication"
        );
    }

    serve(&addr, db, username).await
}
