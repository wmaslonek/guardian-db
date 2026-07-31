#![cfg(feature = "embedding")]
//! End-to-end tests for the auto-embedding pipeline (RFC 0005 phase 2):
//! change-feed → embed → idempotent write-back with provenance, over the
//! deterministic `HashEmbedder` and `MemoryStorage`.

use std::sync::Arc;
use std::time::Duration;

use guardian_db::embedding::{
    EmbeddingRegistry, EmbeddingRule, EmbeddingService, HashEmbedder, ModelSelector,
};
use guardian_db::sql::engine::{Database, Session};
use guardian_db::sql::{ExecResult, MemoryStorage};

const DIMS: u32 = 16;

fn db() -> Arc<Database<MemoryStorage>> {
    Arc::new(Database::new(Arc::new(MemoryStorage::new()), "app"))
}

async fn ok(s: &mut Session<MemoryStorage>, sql: &str) -> Vec<ExecResult> {
    s.execute(sql)
        .await
        .unwrap_or_else(|e| panic!("`{sql}` failed: {e}"))
}

async fn column(s: &mut Session<MemoryStorage>, sql: &str) -> Vec<String> {
    match ok(s, sql).await.pop() {
        Some(ExecResult::Rows { rows, .. }) => rows
            .into_iter()
            .map(|r| r.first().and_then(|v| v.to_text()).unwrap_or_default())
            .collect(),
        other => panic!("`{sql}` did not produce rows: {other:?}"),
    }
}

async fn err(s: &mut Session<MemoryStorage>, sql: &str) -> String {
    match s.execute(sql).await {
        Ok(_) => panic!("expected `{sql}` to fail, but it succeeded"),
        Err(e) => e.to_string(),
    }
}

async fn count_where(s: &mut Session<MemoryStorage>, pred: &str) -> i64 {
    column(s, &format!("SELECT count(*) FROM docs WHERE {pred}"))
        .await
        .pop()
        .and_then(|t| t.parse().ok())
        .unwrap()
}

async fn setup() -> (Arc<Database<MemoryStorage>>, EmbeddingRegistry) {
    let database = db();
    let mut s = Session::new(database.clone(), "guardian");
    ok(&mut s, "CREATE EXTENSION vector").await;
    // The vector column plus the required source-hash sidecar (TEXT).
    ok(
        &mut s,
        &format!(
            "CREATE TABLE docs (id INT PRIMARY KEY, body TEXT, \
             embedding vector({DIMS}), _embedding_srchash TEXT)"
        ),
    )
    .await;
    let registry = EmbeddingRegistry::new();
    registry.register(Arc::new(HashEmbedder::new("test-embed", DIMS)));
    (database, registry)
}

#[tokio::test]
async fn embeds_on_insert_and_is_idempotent() {
    let (database, registry) = setup().await;
    let rule = EmbeddingRule::new(
        "docs",
        "body",
        "embedding",
        ModelSelector::by_name("test-embed"),
    );
    let service = EmbeddingService::new(database.clone(), registry).with_rule(rule);
    let handle = service.spawn();

    let mut s = Session::new(database.clone(), "guardian");
    ok(
        &mut s,
        "INSERT INTO docs VALUES (1, 'hello world', NULL, NULL)",
    )
    .await;

    // The service embeds asynchronously off the change feed; wait for it.
    let mut checker = Session::new(database.clone(), "guardian");
    let mut embedded = 0;
    for _ in 0..200 {
        embedded = count_where(&mut checker, "embedding IS NOT NULL").await;
        if embedded == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(embedded, 1, "row should be embedded");

    // The provenance sidecar records the model + local executor.
    let prov = column(
        &mut checker,
        "SELECT _embedding_srchash FROM docs WHERE id = 1",
    )
    .await
    .pop()
    .unwrap();
    assert!(
        prov.contains("test-embed"),
        "provenance names the model: {prov}"
    );
    assert!(
        prov.contains("local"),
        "provenance names the executor: {prov}"
    );

    // A vector was written in the pgvector text form.
    let got = column(&mut checker, "SELECT embedding FROM docs WHERE id = 1")
        .await
        .pop()
        .unwrap();
    assert!(got.starts_with('['), "vector text form: {got}");

    handle.abort();
}

#[tokio::test]
async fn unchanged_text_is_not_reembedded() {
    let (database, registry) = setup().await;
    let rule = EmbeddingRule::new(
        "docs",
        "body",
        "embedding",
        ModelSelector::by_name("test-embed"),
    );
    let service = EmbeddingService::new(database.clone(), registry).with_rule(rule);
    let handle = service.spawn();

    let mut s = Session::new(database.clone(), "guardian");
    ok(
        &mut s,
        "INSERT INTO docs VALUES (1, 'stable text', NULL, NULL)",
    )
    .await;

    // Wait for the first embedding.
    for _ in 0..200 {
        if count_where(&mut s, "embedding IS NOT NULL").await == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let prov1 = column(&mut s, "SELECT _embedding_srchash FROM docs WHERE id = 1")
        .await
        .pop()
        .unwrap();

    // An UPDATE that does not touch the text column must not re-embed: the
    // provenance (hence the sidecar text) stays byte-identical.
    ok(&mut s, "UPDATE docs SET id = id WHERE id = 1").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let prov2 = column(&mut s, "SELECT _embedding_srchash FROM docs WHERE id = 1")
        .await
        .pop()
        .unwrap();
    assert_eq!(prov1, prov2, "unchanged text must not re-embed");

    // Changing the text DOES re-embed (new source hash).
    ok(
        &mut s,
        "UPDATE docs SET body = 'different text' WHERE id = 1",
    )
    .await;
    for _ in 0..200 {
        let p = column(&mut s, "SELECT _embedding_srchash FROM docs WHERE id = 1")
            .await
            .pop()
            .unwrap();
        if p != prov1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let prov3 = column(&mut s, "SELECT _embedding_srchash FROM docs WHERE id = 1")
        .await
        .pop()
        .unwrap();
    assert_ne!(prov1, prov3, "changed text must re-embed");

    handle.abort();
}

#[tokio::test]
async fn null_text_is_skipped() {
    let (database, registry) = setup().await;
    let rule = EmbeddingRule::new(
        "docs",
        "body",
        "embedding",
        ModelSelector::by_name("test-embed"),
    );
    let service = EmbeddingService::new(database.clone(), registry).with_rule(rule);
    let handle = service.spawn();

    let mut s = Session::new(database.clone(), "guardian");
    ok(&mut s, "INSERT INTO docs VALUES (1, NULL, NULL, NULL)").await;
    // Give the service time to (not) act.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(count_where(&mut s, "embedding IS NOT NULL").await, 0);
    handle.abort();
}

#[tokio::test]
async fn embedded_vector_is_searchable() {
    let (database, registry) = setup().await;
    let rule = EmbeddingRule::new(
        "docs",
        "body",
        "embedding",
        ModelSelector::by_name("test-embed"),
    );
    let service = EmbeddingService::new(database.clone(), registry).with_rule(rule);
    let handle = service.spawn();

    let mut s = Session::new(database.clone(), "guardian");
    for i in 1..=5 {
        ok(
            &mut s,
            &format!("INSERT INTO docs VALUES ({i}, 'document number {i}', NULL, NULL)"),
        )
        .await;
    }
    // Wait for all five embeddings.
    for _ in 0..300 {
        if count_where(&mut s, "embedding IS NOT NULL").await == 5 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(count_where(&mut s, "embedding IS NOT NULL").await, 5);

    // A nearest-neighbour query over the auto-populated column: the row whose
    // text matches the query embeds to the same vector, so it ranks first.
    let query_vec = {
        // Reproduce the embedder's output for a known text as a pgvector
        // literal by round-tripping through the stored column.
        column(&mut s, "SELECT embedding FROM docs WHERE id = 3")
            .await
            .pop()
            .unwrap()
    };
    let top = column(
        &mut s,
        &format!("SELECT id FROM docs ORDER BY embedding <=> '{query_vec}'::vector LIMIT 1"),
    )
    .await;
    assert_eq!(top, ["3"], "the matching document ranks first");

    handle.abort();
}

// ---------------------------------------------------------------------------
// SQL declaration surface (RFC 0005 §6.3): guardian_embed / guardian_unembed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sql_declared_rule_embeds_and_drops() {
    let (database, registry) = setup().await;
    // No static Rust rule — the rule comes entirely from SQL. Refresh fast so
    // the test does not wait 5s.
    let service = EmbeddingService::new(database.clone(), registry)
        .with_catalog_refresh(Duration::from_millis(50));
    let handle = service.spawn();

    let mut s = Session::new(database.clone(), "guardian");
    // Declare via the SQL surface.
    let status = column(
        &mut s,
        "SELECT guardian_embed('docs', 'body', 'embedding', 'test-embed')",
    )
    .await;
    assert_eq!(status, ["embedding rule created"]);

    // Give the service a moment to pick the catalog rule up, then insert.
    tokio::time::sleep(Duration::from_millis(120)).await;
    ok(
        &mut s,
        "INSERT INTO docs VALUES (1, 'hello sql', NULL, NULL)",
    )
    .await;
    for _ in 0..200 {
        if count_where(&mut s, "embedding IS NOT NULL").await == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        count_where(&mut s, "embedding IS NOT NULL").await,
        1,
        "SQL-declared rule must embed"
    );

    // Drop the rule; new inserts are no longer embedded.
    let dropped = column(&mut s, "SELECT guardian_unembed('docs', 'embedding')").await;
    assert_eq!(dropped, ["embedding rule dropped"]);
    tokio::time::sleep(Duration::from_millis(120)).await;
    ok(
        &mut s,
        "INSERT INTO docs VALUES (2, 'after drop', NULL, NULL)",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        count_where(&mut s, "id = 2 AND embedding IS NULL").await,
        1,
        "dropped rule must not embed new rows"
    );

    handle.abort();
}

#[tokio::test]
async fn sql_surface_validates_arguments() {
    let (database, _registry) = setup().await;
    let mut s = Session::new(database, "guardian");
    // Missing sidecar column.
    ok(
        &mut s,
        "CREATE TABLE nosc (id INT PRIMARY KEY, body TEXT, v vector(16))",
    )
    .await;
    let e = err(&mut s, "SELECT guardian_embed('nosc', 'body', 'v', 'm')").await;
    assert!(e.contains("sidecar"), "{e}");
    // Non-vector target column.
    let e = err(&mut s, "SELECT guardian_embed('docs', 'body', 'body', 'm')").await;
    assert!(e.contains("not a dimensioned vector"), "{e}");
    // Unknown table.
    let e = err(&mut s, "SELECT guardian_embed('ghost', 'a', 'b', 'm')").await;
    assert!(e.contains("ghost") || e.contains("does not exist"), "{e}");
    // Bad policy.
    let e = err(
        &mut s,
        "SELECT guardian_embed('docs', 'body', 'embedding', 'm', 'wrongpolicy')",
    )
    .await;
    assert!(e.contains("policy"), "{e}");
    // Unembed of a nonexistent rule reports gracefully (no error).
    let r = column(&mut s, "SELECT guardian_unembed('docs', 'embedding')").await;
    assert_eq!(r, ["no such embedding rule"]);
}

#[tokio::test]
async fn sql_rule_persists_in_catalog() {
    let storage = Arc::new(MemoryStorage::new());
    {
        let database = Arc::new(Database::new(storage.clone(), "app"));
        let mut s = Session::new(database, "guardian");
        ok(&mut s, "CREATE EXTENSION vector").await;
        ok(
            &mut s,
            &format!(
                "CREATE TABLE docs (id INT PRIMARY KEY, body TEXT, \
                 embedding vector({DIMS}), _embedding_srchash TEXT)"
            ),
        )
        .await;
        ok(
            &mut s,
            "SELECT guardian_embed('docs', 'body', 'embedding', 'test-embed', 'delegated')",
        )
        .await;
    }
    // A fresh Database over the same storage sees the declared rule (it was
    // persisted in the replicated catalog).
    let database2 = Arc::new(Database::new(storage, "app"));
    let catalog = database2.catalog_snapshot().await.unwrap();
    let rules = catalog.embedding_rules();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].table, "docs");
    assert_eq!(rules[0].vector_column, "embedding");
    assert_eq!(rules[0].model, "test-embed");
    assert_eq!(rules[0].policy, "delegated");
}
