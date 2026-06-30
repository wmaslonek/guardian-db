#![cfg(feature = "odm")]

use guardian_db::odm::{
    Collection, FieldDefinition, FieldType, MemoryStorage, ModelSchema, OdmError,
};
use serde_json::{Value, json};
use std::sync::Arc;

fn stress_schema() -> ModelSchema {
    ModelSchema::new("BenchmarkDocument", "benchmark_documents")
        .field(
            FieldDefinition::new("id", FieldType::String)
                .primary_key()
                .required(),
        )
        .field(
            FieldDefinition::new("email", FieldType::String)
                .unique()
                .required(),
        )
        .field(
            FieldDefinition::new("tenant", FieldType::String)
                .indexed()
                .required(),
        )
        .field(
            FieldDefinition::new("group", FieldType::String)
                .indexed()
                .required(),
        )
        .field(FieldDefinition::new("counter", FieldType::Number).required())
        .field(FieldDefinition::new("payload", FieldType::String).required())
        .field(FieldDefinition::new("metadata", FieldType::Object).required())
        .timestamps("created_at", "updated_at")
}

async fn collection() -> Collection {
    Collection::new(
        "benchmark_documents",
        stress_schema(),
        Arc::new(MemoryStorage::new()),
    )
    .await
    .unwrap()
}

fn payload(size: usize, seed: usize) -> String {
    let pattern = format!("guardian-db-large-doc-{seed:016x}-");
    let mut value = String::with_capacity(size);
    while value.len() < size {
        value.push_str(&pattern);
    }
    value.truncate(size);
    value
}

fn doc(index: usize, payload_bytes: usize) -> Value {
    json!({
        "id": format!("stress-{index:010}"),
        "email": format!("stress-{index:010}@example.test"),
        "tenant": format!("tenant-{}", index % 12),
        "group": format!("group-{}", index % 64),
        "counter": index,
        "payload": payload(payload_bytes, index),
        "metadata": {
            "active": index.is_multiple_of(2),
            "bucket": index % 128,
            "region": format!("region-{}", index % 8)
        }
    })
}

fn docs(count: usize, payload_bytes: usize) -> Vec<Value> {
    (0..count).map(|index| doc(index, payload_bytes)).collect()
}

#[tokio::test]
async fn bulk_query_update_and_constraint_reliability_workload() {
    let collection = collection().await;
    let count = 2_000;

    let inserted = collection.insert(docs(count, 256)).await.unwrap();
    assert_eq!(inserted.len(), count);

    assert_eq!(
        collection
            .find_one(json!({ "email": "stress-0000001024@example.test" }))
            .await
            .unwrap()
            .unwrap()["id"],
        "stress-0000001024",
    );
    assert_eq!(
        collection
            .find(json!({ "group": "group-7" }))
            .await
            .unwrap()
            .len(),
        32
    );
    assert!(
        collection
            .find_by_id("stress-0000001999")
            .await
            .unwrap()
            .is_some()
    );

    let updated = collection
        .update(
            json!({ "email": "stress-0000001024@example.test" }),
            json!({ "$set": { "tenant": "tenant-hot" }, "$inc": { "counter": 1 } }),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated["tenant"], "tenant-hot");
    assert_eq!(updated["counter"], 1025);

    let duplicate = collection
        .insert(vec![
            json!({
                "id": "duplicate-a",
                "email": "duplicate@example.test",
                "tenant": "tenant-x",
                "group": "group-x",
                "counter": 1,
                "payload": "a",
                "metadata": {}
            }),
            json!({
                "id": "duplicate-b",
                "email": "duplicate@example.test",
                "tenant": "tenant-x",
                "group": "group-x",
                "counter": 2,
                "payload": "b",
                "metadata": {}
            }),
        ])
        .await
        .unwrap_err();
    assert!(matches!(duplicate, OdmError::DuplicateKey { .. }));
    assert!(
        collection
            .find_by_id("duplicate-a")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        collection
            .find_by_id("duplicate-b")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn multi_megabyte_document_round_trip_default_stress() {
    let collection = collection().await;
    let payload_bytes = 2 * 1024 * 1024;
    let inserted = collection.insert_one(doc(1, payload_bytes)).await.unwrap();
    assert_eq!(inserted["payload"].as_str().unwrap().len(), payload_bytes);

    let found = collection
        .find_by_id("stress-0000000001")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found["payload"].as_str().unwrap().len(), payload_bytes);

    let updated = collection
        .update(
            json!({ "id": "stress-0000000001" }),
            json!({ "$set": { "metadata.large_doc_round_trip": true } }),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated["metadata"]["large_doc_round_trip"], true);
}

#[tokio::test]
#[ignore = "explicit limit probe; set GUARDIANDB_ODM_LARGE_DOC_MB to test above MongoDB's 16 MiB BSON document limit"]
async fn explicit_large_document_limit_probe() {
    let collection = collection().await;
    let mib = std::env::var("GUARDIANDB_ODM_LARGE_DOC_MB")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(17);
    let payload_bytes = mib * 1024 * 1024;

    let inserted = collection.insert_one(doc(42, payload_bytes)).await.unwrap();
    assert_eq!(inserted["payload"].as_str().unwrap().len(), payload_bytes);
    assert!(
        collection
            .find_by_id("stress-0000000042")
            .await
            .unwrap()
            .is_some()
    );
}
