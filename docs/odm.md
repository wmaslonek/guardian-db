# Optional ODM layer

GuardianDB's ODM sits above the existing `DocumentStore`; it does not replace Iroh Docs/Willow or turn GuardianDB into a relational database. Enable it explicitly:

```toml
[dependencies]
guardian-db = { version = "0.16", features = ["odm"] }
```

## Rust models

Derive a typed model with declarative primary, unique, secondary-index, timestamp, and strict-schema metadata:

```rust
use guardian_db::odm::Model;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Model)]
#[model(collection = "employees", timestamps)]
struct Employee {
    #[primary_key]
    ssn: String,
    #[unique]
    email: String,
    #[index]
    department: String,
    name: String,
    created_at: Option<String>,
    updated_at: Option<String>,
}

let employees = db.model_collection::<Employee>().await?;
```

The derive supports `#[model(collection = "...")]`, `#[model(timestamps)]`, timestamp field overrides, `#[model(flexible)]`, and schema versions. It honors common Serde field behavior including `rename`, `rename_all`, `skip`, `skip_serializing`, `skip_serializing_if`, and `flatten`.

Runtime schemas are available through `ModelSchema`, `FieldDefinition`, and `Collection::new`. `GuardianDB::init_collection` creates a permissive collection with an automatically generated `_id`. Dynamic and typed collections expose `insert_one`, batch `insert`, `find_one`, `find`, `find_by_id`, and first-match `update` with `$set`, `$unset`, and `$inc`.

## Validation and indexes

Every write validates field types, required/nullability rules, strict schemas, immutable primary keys, and unique constraints before persistence. Primary, unique, and declared secondary indexes are rebuilt from the current local document view and used to narrow equality queries. Batch insert validates the complete candidate state before calling storage.

## Consistency boundary

Each collection instance uses one asynchronous mutation lock. Before a write it refreshes the current local `DocumentStore` index, validates the candidate state, rebuilds unique and secondary indexes, and persists while the lock remains held. This provides atomic validation and mutation ordering inside that collection instance.

GuardianDB remains decentralized and eventually convergent. A unique constraint accepted concurrently on disconnected peers can conflict when replicas merge; there is no hidden global lock or consensus protocol. `TransactionContext` and `ConsistencyLevel` reserve a stable API shape for a future coordinator. `LocalAtomic` is implemented now, while `Replicated` is rejected explicitly rather than implying cross-peer ACID guarantees.

The current `DocumentStore::put_all` implementation writes documents individually, so batch insertion guarantees all-or-nothing *validation before persistence*, not rollback after a lower-level partial I/O failure.

## TypeScript transport

The SDK in `packages/guardiandb-odm` exposes `GuardianDB.init`, `GuardianDB.listDatabases`, `initCollection`, `listCollections`, and Mongoose-style CRUD. Native Node/WASM/mobile bindings implement `GuardianTransport`; a process-local reference transport is included for deterministic tests and SDK development.
