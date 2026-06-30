#![cfg(feature = "odm")]

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use guardian_db::odm::{Collection, FieldDefinition, FieldType, MemoryStorage, ModelSchema};
use serde_json::{Value, json};
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::runtime::Runtime;

const DEFAULT_DOCS: usize = 5_000;
const DEFAULT_BATCH_DOCS: usize = 1_000;
const DEFAULT_PAYLOAD_BYTES: usize = 512;
static COLLECTION_COUNTER: AtomicU64 = AtomicU64::new(1);

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("1" | "true" | "TRUE" | "yes" | "YES")
    )
}

fn benchmark_schema() -> ModelSchema {
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
        .field(
            FieldDefinition::new("metadata", FieldType::Object)
                .indexed()
                .required(),
        )
        .timestamps("created_at", "updated_at")
}

fn new_collection_name(prefix: &str) -> String {
    let id = COLLECTION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{id}")
}

async fn new_collection(prefix: &str) -> Collection {
    Collection::new(
        new_collection_name(prefix),
        benchmark_schema(),
        Arc::new(MemoryStorage::new()),
    )
    .await
    .unwrap()
}

fn payload(size: usize, seed: usize) -> String {
    let pattern = format!("guardian-db-odm-benchmark-{seed:016x}-");
    let mut value = String::with_capacity(size);
    while value.len() < size {
        value.push_str(&pattern);
    }
    value.truncate(size);
    value
}

fn benchmark_document(index: usize, payload_bytes: usize) -> Value {
    json!({
        "id": format!("bench-{index:010}"),
        "email": format!("bench-{index:010}@example.test"),
        "tenant": format!("tenant-{}", index % 16),
        "group": format!("group-{}", index % 64),
        "counter": index,
        "payload": payload(payload_bytes, index),
        "metadata": {
            "region": format!("region-{}", index % 8),
            "active": index.is_multiple_of(2),
            "bucket": index % 128
        }
    })
}

fn documents(start: usize, count: usize, payload_bytes: usize) -> Vec<Value> {
    (start..start + count)
        .map(|index| benchmark_document(index, payload_bytes))
        .collect()
}

async fn seeded_collection(count: usize, payload_bytes: usize) -> Collection {
    let collection = new_collection("seeded").await;
    for chunk_start in (0..count).step_by(1_000) {
        let chunk_len = (count - chunk_start).min(1_000);
        collection
            .insert(documents(chunk_start, chunk_len, payload_bytes))
            .await
            .unwrap();
    }
    collection
}

fn benchmark_insert_workloads(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let batch_docs = env_usize("GUARDIANDB_ODM_BENCH_BATCH_DOCS", DEFAULT_BATCH_DOCS);
    let payload_bytes = env_usize("GUARDIANDB_ODM_BENCH_PAYLOAD_BYTES", DEFAULT_PAYLOAD_BYTES);

    let mut group = c.benchmark_group("odm_insert");
    group.sample_size(10);

    let counter = AtomicU64::new(0);
    let single_collection = rt.block_on(new_collection("insert_one"));
    group.throughput(Throughput::Elements(1));
    group.bench_function("insert_one_indexed_document", |b| {
        b.iter(|| {
            let index = counter.fetch_add(1, Ordering::Relaxed) as usize;
            rt.block_on(async {
                single_collection
                    .insert_one(std::hint::black_box(benchmark_document(
                        index,
                        payload_bytes,
                    )))
                    .await
                    .unwrap()
            })
        })
    });

    group.throughput(Throughput::Elements(batch_docs as u64));
    group.bench_function(
        format!("batch_insert_{batch_docs}_documents").as_str(),
        |b| {
            let batch_counter = AtomicU64::new(0);
            b.iter_batched(
                || {
                    let start =
                        batch_counter.fetch_add(batch_docs as u64, Ordering::Relaxed) as usize;
                    (
                        rt.block_on(new_collection("batch_insert")),
                        documents(start, batch_docs, payload_bytes),
                    )
                },
                |(collection, docs)| {
                    rt.block_on(async move {
                        collection.insert(std::hint::black_box(docs)).await.unwrap()
                    })
                },
                BatchSize::LargeInput,
            )
        },
    );

    group.finish();
}

fn benchmark_query_workloads(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let doc_count = env_usize("GUARDIANDB_ODM_BENCH_DOCS", DEFAULT_DOCS);
    let payload_bytes = env_usize("GUARDIANDB_ODM_BENCH_PAYLOAD_BYTES", DEFAULT_PAYLOAD_BYTES);
    let collection = rt.block_on(seeded_collection(doc_count, payload_bytes));
    let counter = AtomicU64::new(0);

    let mut group = c.benchmark_group("odm_query");
    group.sample_size(10);
    group.throughput(Throughput::Elements(1));

    group.bench_function(format!("find_by_id_{doc_count}_docs").as_str(), |b| {
        b.iter(|| {
            let index = (counter.fetch_add(1, Ordering::Relaxed) as usize) % doc_count;
            rt.block_on(async {
                collection
                    .find_by_id(std::hint::black_box(format!("bench-{index:010}")))
                    .await
                    .unwrap()
                    .unwrap()
            })
        })
    });

    group.bench_function(
        format!("find_one_unique_index_{doc_count}_docs").as_str(),
        |b| {
            b.iter(|| {
                let index = (counter.fetch_add(1, Ordering::Relaxed) as usize) % doc_count;
                rt.block_on(async {
                    collection
                        .find_one(std::hint::black_box(json!({
                            "email": format!("bench-{index:010}@example.test")
                        })))
                        .await
                        .unwrap()
                        .unwrap()
                })
            })
        },
    );

    group.bench_function(
        format!("find_secondary_index_{doc_count}_docs").as_str(),
        |b| {
            b.iter(|| {
                let bucket = (counter.fetch_add(1, Ordering::Relaxed) as usize) % 64;
                rt.block_on(async {
                    collection
                        .find(std::hint::black_box(
                            json!({ "group": format!("group-{bucket}") }),
                        ))
                        .await
                        .unwrap()
                })
            })
        },
    );

    group.bench_function(
        format!("find_scan_comparison_{doc_count}_docs").as_str(),
        |b| {
            b.iter(|| {
                let threshold = (counter.fetch_add(1, Ordering::Relaxed) as usize) % doc_count;
                rt.block_on(async {
                    collection
                        .find(std::hint::black_box(
                            json!({ "counter": { "$gte": threshold } }),
                        ))
                        .await
                        .unwrap()
                })
            })
        },
    );

    group.finish();
}

fn benchmark_update_workloads(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let doc_count = env_usize("GUARDIANDB_ODM_BENCH_DOCS", DEFAULT_DOCS);
    let payload_bytes = env_usize("GUARDIANDB_ODM_BENCH_PAYLOAD_BYTES", DEFAULT_PAYLOAD_BYTES);
    let collection = rt.block_on(seeded_collection(doc_count, payload_bytes));
    let counter = AtomicU64::new(0);

    let mut group = c.benchmark_group("odm_update");
    group.sample_size(10);
    group.throughput(Throughput::Elements(1));

    group.bench_function(
        format!("update_by_unique_index_{doc_count}_docs").as_str(),
        |b| {
            b.iter(|| {
                let index = (counter.fetch_add(1, Ordering::Relaxed) as usize) % doc_count;
                rt.block_on(async {
                    collection
                        .update(
                            std::hint::black_box(json!({
                                "email": format!("bench-{index:010}@example.test")
                            })),
                            std::hint::black_box(json!({ "$set": { "tenant": "tenant-updated" } })),
                        )
                        .await
                        .unwrap()
                        .unwrap()
                })
            })
        },
    );

    group.bench_function(format!("update_inc_by_id_{doc_count}_docs").as_str(), |b| {
        b.iter(|| {
            let index = (counter.fetch_add(1, Ordering::Relaxed) as usize) % doc_count;
            rt.block_on(async {
                collection
                    .update(
                        std::hint::black_box(json!({ "id": format!("bench-{index:010}") })),
                        std::hint::black_box(json!({ "$inc": { "counter": 1 } })),
                    )
                    .await
                    .unwrap()
                    .unwrap()
            })
        })
    });

    group.finish();
}

fn benchmark_large_document_workloads(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let sizes = large_payload_sizes();
    let mut group = c.benchmark_group("odm_large_document");
    group.sample_size(10);

    for size in sizes {
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(
            format!("insert_read_update_payload_{size}_bytes").as_str(),
            |b| {
                let counter = AtomicU64::new(0);
                b.iter_batched(
                    || {
                        let index = counter.fetch_add(1, Ordering::Relaxed) as usize;
                        (rt.block_on(new_collection("large_doc")), index)
                    },
                    |(collection, index)| {
                        rt.block_on(async move {
                            let inserted = collection
                                .insert_one(std::hint::black_box(benchmark_document(index, size)))
                                .await
                                .unwrap();
                            let id = inserted["id"].as_str().unwrap().to_string();
                            let found = collection
                                .find_by_id(std::hint::black_box(id.clone()))
                                .await
                                .unwrap();
                            assert!(found.is_some());
                            collection
                                .update(
                                    std::hint::black_box(json!({ "id": id })),
                                    std::hint::black_box(
                                        json!({ "$set": { "metadata.status": "updated" } }),
                                    ),
                                )
                                .await
                                .unwrap()
                                .unwrap()
                        })
                    },
                    BatchSize::LargeInput,
                )
            },
        );
    }

    group.finish();
}

fn benchmark_reliability_guards(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let doc_count = env_usize("GUARDIANDB_ODM_BENCH_DOCS", DEFAULT_DOCS);
    let payload_bytes = env_usize("GUARDIANDB_ODM_BENCH_PAYLOAD_BYTES", DEFAULT_PAYLOAD_BYTES);

    let mut group = c.benchmark_group("odm_reliability");
    group.sample_size(10);
    group.throughput(Throughput::Elements(doc_count as u64));

    group.bench_function(
        format!("validate_unique_index_rebuild_{doc_count}_docs").as_str(),
        |b| {
            b.iter_batched(
                || documents(0, doc_count, payload_bytes),
                |docs| {
                    rt.block_on(async {
                        let collection = new_collection("reliability").await;
                        collection.insert(std::hint::black_box(docs)).await.unwrap();
                        let duplicate = collection
                            .insert_one(benchmark_document(0, payload_bytes))
                            .await
                            .unwrap_err();
                        std::hint::black_box(duplicate)
                    })
                },
                BatchSize::LargeInput,
            )
        },
    );

    group.finish();
}

fn large_payload_sizes() -> Vec<usize> {
    if let Ok(value) = std::env::var("GUARDIANDB_ODM_BENCH_LARGE_BYTES") {
        return value
            .split(',')
            .filter_map(|part| part.trim().parse::<usize>().ok())
            .filter(|value| *value > 0)
            .collect();
    }

    let mut sizes = vec![64 * 1024, 1024 * 1024];
    if env_flag("GUARDIANDB_ODM_BENCH_EXTREME") {
        sizes.extend([8 * 1024 * 1024, 17 * 1024 * 1024, 32 * 1024 * 1024]);
    }
    sizes
}

criterion_group!(
    odm_benches,
    benchmark_insert_workloads,
    benchmark_query_workloads,
    benchmark_update_workloads,
    benchmark_large_document_workloads,
    benchmark_reliability_guards
);
criterion_main!(odm_benches);
