# GuardianDB ODM Benchmarks

This benchmark suite is modeled after MongoDB-style workload tools such as `mongo-bench`: configurable insert, query, update, reliability, and large-document workloads with operation-rate reporting.

The benchmarks intentionally exercise the ODM layer rather than the full p2p replication stack. They are useful for measuring schema validation, index-maintenance costs, indexed query paths, scan query paths, atomic batch rollback, and large JSON document round trips.

## Rust Criterion benchmarks

Run the default benchmark suite:

```bash
cargo bench --features odm --bench odm_benchmark
```

Tune workload size with environment variables:

```bash
GUARDIANDB_ODM_BENCH_DOCS=50000 \
GUARDIANDB_ODM_BENCH_BATCH_DOCS=5000 \
GUARDIANDB_ODM_BENCH_PAYLOAD_BYTES=1024 \
cargo bench --features odm --bench odm_benchmark
```

Probe large-document sizes. By default, the Criterion run includes 64 KiB and 1 MiB payloads. Use either an explicit comma-separated byte list or the extreme preset:

```bash
GUARDIANDB_ODM_BENCH_LARGE_BYTES=1048576,8388608,17825792,33554432 \
cargo bench --features odm --bench odm_benchmark odm_large_document

GUARDIANDB_ODM_BENCH_EXTREME=1 \
cargo bench --features odm --bench odm_benchmark odm_large_document
```

The `17 MiB` case is included because MongoDB's maximum BSON document size is 16 MiB. GuardianDB's ODM does not encode documents as BSON and does not add a BSON-specific cap. The practical ceiling is determined by JSON serialization, memory pressure, storage backend behavior, and replication/networking costs. A successful `MemoryStorage` result establishes the ODM/JSON path's behavior only; it is not a hard limit measurement for the Iroh-backed `DocumentStoreStorage` transport.

## Rust reliability tests

Run the normal reliability workload:

```bash
cargo test --features odm --test odm_benchmark_reliability
```

Run the explicit large-document limit probe. The default payload is 17 MiB; override it as needed:

```bash
cargo test --features odm --test odm_benchmark_reliability -- --ignored

GUARDIANDB_ODM_LARGE_DOC_MB=64 \
cargo test --features odm --test odm_benchmark_reliability explicit_large_document_limit_probe -- --ignored
```

## TypeScript SDK benchmark runner

The TypeScript runner targets the SDK API and its deterministic `MemoryTransport` reference backend. It is useful for client-surface regression checks before a native Iroh/WASM transport is wired in.

```bash
cd packages/guardiandb-odm
npm run bench -- --mode=runAll --docs=10000 --batch-size=1000 --queries=2500 --updates=2500
```

Run only the large-document probe:

```bash
npm run bench -- --mode=large --large-mb=17
npm run bench -- --mode=large --large-mb=64
```

Include the large-document probe inside `runAll`:

```bash
npm run bench -- --mode=runAll --include-large --large-mb=17
```

The runner prints phase and batch progress to stderr and emits the final JSON result to stdout. This keeps interactive runs visibly active while still allowing machine-readable output to be redirected independently:

```bash
npm run bench -- --mode=runAll 2>benchmark-progress.log >benchmark-results.json
```

Progress is enabled by default. Disable it or change the heartbeat interval when running in CI:

```bash
npm run bench -- --mode=runAll --progress=false
npm run bench -- --mode=runAll --heartbeat-ms=10000
```

Document batches are generated lazily so increasing `--docs` does not first allocate the entire workload in memory.

## What is measured

| Workload | Purpose |
| --- | --- |
| `odm_insert` / `insert.batch` | Batch insertion throughput under schema validation and index creation. |
| `odm_query` / `query.*` | Primary-key, unique-index, secondary-index, and scan-style query paths. |
| `odm_update` / `update.*` | MongoDB-style `$set` and `$inc` operations with index maintenance and validation. |
| `odm_reliability` / `reliability.*` | Unique-constraint rejection, atomic batch rollback, and serialized concurrent unique writes. |
| `odm_large_document` / `large.*` | Insert, read, and update behavior with multi-megabyte JSON documents. |

## Interpreting results

The Rust ODM currently refreshes the local replicated view before read/write operations and rebuilds its in-process indexes from the storage snapshot. That is deliberately conservative for consistency, so the Criterion results include refresh and validation costs rather than measuring only map lookup latency. The TypeScript `MemoryTransport` maintains indexes incrementally and is intended as a fast reference backend; it does not measure Iroh replication. Treat both suites as regression signals, and benchmark a native `GuardianTransport`/`DocumentStoreStorage` adapter before drawing conclusions about decentralized end-to-end limits.
