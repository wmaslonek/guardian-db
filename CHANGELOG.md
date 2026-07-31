# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.20.26] - 2026-07-31

### Added
- **Vector search, auto-embedding, and RAG over the SQL engine (RFC 0005).** The three-phase "data → embed → retrieve → generate" stack, built entirely on replicated GuardianDB data, behind opt-in features; default builds are unaffected. Design and as-built record in `docs/rfcs/0005-vector-search.md`.
  - **Phase 1 — engine-native HNSW ANN index** (`vector-index` feature = `["sql", "dep:hnsw_rs"]`). `CREATE INDEX ... USING hnsw (embedding vector_cosine_ops) WITH (m = 16, ef_construction = 64)` — the pgvector surface, exact: the `vector_l2_ops` / `vector_ip_ops` / `vector_cosine_ops` / `vector_l1_ops` opclasses, the `hnsw.ef_search` GUC (plus `hnsw.ef_growth_cap` / `hnsw.selectivity_threshold`), and a top-k planner hook that accelerates `ORDER BY <col> <dist-op> <query> LIMIT k` while any other shape falls through to the exact scan unchanged. `SET enable_indexscan = off` forces exact results; `EXPLAIN` (execute-and-report, EXPLAIN-ANALYZE-style) shows the chosen path. The HNSW graph is **per-node derived state, never replicated** — rebuilt from the replicated rows, mirroring `SecondaryIndex`; a rebuild is always safe, only slow.
    - Filtered search (§6.2) composes two strategies: a *measured* selective-equality cutover to the exact index scan, and adaptive `ef` growth with an exact fallback past the ceiling — never silently returning fewer than `k`.
    - Robustness: tombstone deletes with a 20%-threshold rebuild; a fixed exact-scan floor (≤ 2000 live rows answered by linear scan — deterministic and immune to small-graph recall wobble); parallel bulk build (`rayon`, ~12× the serial rate on 384-d) for the initial build and rebuilds; and persistent, checksum-validated sidecar snapshots (§6.1, `Database::set_ann_snapshot_dir`) for ~25× faster warm starts. Benchmark: `examples/vector_index_bench.rs` (~3 ms p50 top-k at recall ≈ 0.95 on 100k × 384-d realistic data — pgvector parity).
  - **Phase 2 — auto-embedding pipeline** (`embedding` feature = `["vector-index", "dep:reqwest"]`). A pluggable `Embedder` trait + owner-curated `EmbeddingRegistry` (mirroring the LLM layer), with model identity carried as an optional BLAKE3 hash for pin-by-hash (§6.3). Backends: `OpenAiEmbeddingBackend` (any OpenAI-compatible `/v1/embeddings` server — ollama, llama.cpp, LM Studio, OpenAI), a deterministic `HashEmbedder` for tests/offline demos, and `OnnxEmbeddingBackend` (`embedding-onnx` feature: local `ort` + HuggingFace `tokenizers`, mean-pool + L2-normalize, the one backend whose weight identity is verifiable). The `EmbeddingService` watches the SQL engine's committed-change feed and writes embeddings back into a `vector` column idempotently (a `_<col>_srchash` sidecar records the source-text hash + provenance — model, weight hash, executor), the write-back going straight to storage so it emits no change event and structurally cannot loop.
    - **SQL declaration surface (§6.3):** `SELECT guardian_embed('table','text_col','vector_col','model'[,'local'|'delegated'])` and `SELECT guardian_unembed('table','vector_col')`, persisted in the catalog (replicated like any DDL) and read by the service alongside Rust-API rules.
    - **P2P delegation (§6.4, with `compute`):** embed on a GPU peer over `COMPUTE_ALPN`, hash-pinned by default (a peer that cannot prove the exact weights is ineligible; a mismatch is a hard error, never a name-only fallback), with local fallback. The `CapabilityVector` gained an `embed_models` field (gossip topic bumped `guardian-db/compute/capabilities/3 → /4`), and the compute protocol a self-contained `Embed` request/reply on `COMPUTE_ALPN`.
  - **Phase 3 — RAG helper** (`rag` feature = `["embedding", "compute-llm"]`). `Rag::retrieve` / `Rag::answer` tie phase-1 search + a query embedder + the LLM router together, with one guardrail: `Rag::new` reads the corpus column's declared embedding rule and refuses a query embedder whose model differs (the classic silent RAG failure — mismatched query/corpus vector spaces), checking the model rather than assuming it. Example: `examples/rag_demo.rs` (fully offline).
- **SQL decoded-table cache — eliminates the per-statement table reload+decode.** The engine's local-first model re-scanned and re-decoded a whole table view on every statement (O(table) even for `SELECT count(*)`). Backends can now report a per-collection change counter (`RelationalStorage::generation`); when they do, the engine caches the decoded `LoadedTable` behind an `Arc`, keyed by that counter — read-only statements share it zero-copy, writes pay one copy-on-write clone, and row writes self-invalidate via the counter. `MemoryStorage` implements it (~40× on warm reads: a 30k-row `count(*)` baseline dropped from ~350 ms to ~40 ms, and an ANN warm query from ~290 ms to ~7 ms). The replicated `GuardianRelationalStorage` backend deliberately opts out (returns `None`): its local index is updated by a background live-sync task, so a change counter it mediates could not cover peer writes — correctness over the cache, since a stale read in a P2P database is not an acceptable trade. Catalog/DDL changes (including inside a transaction) bypass or clear the cache so a schema change is never served stale.
- **Payload codecs — the extension point for application-supplied encryption of record values.** iroh-docs has no read-side access control: a `DocTicket` is a bearer capability, so whoever holds it replicates the whole namespace and can read every value in it, and there is no `can_read` counterpart to `AccessController::can_append`. For applications needing confidentiality from a ticket holder, encrypting the payload is therefore the only available mechanism, and until now that meant remembering to encrypt at every call site. The new `stores::payload_codec::PayloadCodec` trait gives that a single home inside the store.
  - **Default behaviour is unchanged.** `CreateDBOptions::payload_codec` (and `NewStoreOptions::payload_codec`) default to `None`, which resolves to the no-op `IdentityCodec`: bytes are stored verbatim, byte-for-byte as before. The trait itself pulls in no dependencies, so applications with their own key schedule (ratcheting, per-group sender keys, MLS) implement it directly without paying for an AEAD.
  - **Applied by both iroh-docs-backed stores** (`keyvalue` and `document`) on every write path — `put`, `put_impl`, `put_batch`/`put_all` and `add_operation` — and reversed on every read path (`get`, `all`, `query`). The in-memory index mirrors the *stored* form, so ciphertext never becomes plaintext at rest in the index. Reads that cannot be decoded are skipped with a warning by the bulk accessors (`all`, `query`) rather than failing the whole listing, since a namespace may legitimately hold records encoded with key material this replica does not have; `get` still reports the failure so a caller can distinguish "absent" from "unreadable".
  - **Not covered, by design:** record *keys* (kept plaintext so key enumeration, range scans and the index keep working — choose keys that do not leak), blobs added directly through the client API (iroh-blobs is content-addressed, so the BLAKE3 hash of a plaintext file *is* an identifier for it; encrypt before adding), and replication metadata. A codec provides content confidentiality, not metadata privacy.
  - **Interoperability:** a codec changes the bytes on the wire, so every replica of a store must be configured with a codec able to decode what the others produce. A peer holding a `DocTicket` but not the key replicates the namespace and cannot read it — which is the intent.
- **`encryption` feature (off by default): batteries-included implementations.**
  - `stores::payload_codec::XChaCha20Poly1305Codec`, a reference AEAD codec. Random 24-byte nonce per write (the extended nonce is what makes collisions negligible across uncoordinated writers sharing a key in a multi-writer namespace), a version byte for future migrations, and the store address + record key bound in as associated data, so a ciphertext cannot be relocated to another key or another store undetected. Provides confidentiality and tamper detection; explicitly **not** forward secrecy or post-compromise security, and it does not hide keys, sizes, write times or authorship.
  - **Encryption at rest for `RedbKeystore`.** Secret key material previously sat verbatim in redb, so a stolen device file gave up every private key. `RedbKeystore::new_encrypted` (and `new_encrypted_from_secret`, deriving via BLAKE3's KDF) seals each secret with XChaCha20-Poly1305, binding the key identifier as associated data. Identifiers and lifecycle metadata stay in the clear, so `enumerate_keys` and `key_meta` still work without the master key. Encrypted files carry a format marker: opening one with `new`, or an existing non-empty plaintext one with `new_encrypted`, is a clean error rather than a silent misread, and there is deliberately no automatic migration. The master key is zeroized once the cipher is built; sourcing it (OS keychain, Android Keystore, iOS Secure Enclave, a stretched passphrase) remains the application's decision.
  - GuardianDB deliberately does **not** pick an encryption scheme for you. Key management is application policy, and a fixed scheme would also disable the features that need to read record contents — `sql`, `vector-index`, `embedding`, `rag` and `compute` all operate on plaintext.

### Changed
- **`messaging` is now a Cargo feature (on by default) and a top-level module.** The high-level P2P transport moved from `src/p2p/messaging/` to the single file `src/messaging.rs`, gated by the new `messaging` feature. Default builds are unaffected (the feature is in `default`); `--no-default-features` drops the built-in transport, in which case `GuardianDB::new` requires an explicit `options.direct_channel_factory` (a clear runtime error otherwise, rather than a silent no-op). The default factory (`messaging::init_direct_channel_factory`) is built on the backend's shared gossip layer.
- **The pub/sub trait now owns the mesh operations the store layer needs**, instead of the store reaching them by downcasting to a concrete type. It gained `subscribe_with_peers`, `get_or_create_topic_with_peers`, `publish_to_topic`, and `topic_peers`; `base_store` replication (subscribe the shared log topic, form a mesh with a new peer, wait for it, publish heads) now speaks only through the trait, so any implementation (including a test mock or an alternative transport) works on the replication path. The trait was also renamed `PubSubInterface → PubSub` (the `-Interface` suffix was non-idiomatic and the only one of its kind in `traits.rs`; it now pairs cleanly with its sibling `PubSubTopic`). **Breaking** (0.x): the rename, plus the four new methods have no default body for external implementors.
- **CBOR codec migrated from the unmaintained `serde_cbor` to `ciborium`**, the maintained serde-ecosystem replacement, behind a small `guardian::cbor` helper (`to_vec`/`from_slice`). The encoding is byte-identical for the struct shapes GuardianDB persists (manifests, access lists are strings/bools/sequences/maps), so content-addressed manifest hashes and previously stored data are unaffected — a regression test pins the wire layout.
- **In-process event infrastructure moved out of `p2p` into a top-level `events` module.** `EventBus`, `Emitter`, `PayloadEmitter` and the type-erased `EventEmitter`/`EmitterInterface` facade now live in `src/events/` (paths `crate::events::*`; `p2p` is now purely networking). The sync-observability layer moved from the root `reactive_synchronizer` module to `stores::sync_observer` (next to the store events it describes). **Breaking:** `guardian_db::reactive_synchronizer::*` → `guardian_db::stores::sync_observer::*`, and `guardian_db::p2p::{EventBus, Emitter, PayloadEmitter}` → `guardian_db::events::{…}`.

### Removed
- **Retired the `IrohBridge` direct-channel implementation** (orbit-db-legacy, ~1,600 lines): it spawned a second `Gossip` instance that was never registered on the iroh `Router`. The default transport is now the one-on-one channel built on the backend's shared gossip.
- **Removed `CoreApiPubSub`/`PsTopic` pub/sub wrapper** (~490 lines, orbit-db-legacy): never constructed in production and a duplicate of the core `EpidemicPubSub`, with polling-based peer watching instead of the native push. `EpidemicPubSub` is now the single `PubSub` implementation.

### Fixed
- **Compute LLM streaming: a token delta larger than the 1 MiB per-frame cap was written as one oversized frame**, which the requester's reader rejected — reporting a *false* mid-stream truncation for a generation that actually succeeded. Large deltas are now split into multiple frames on UTF-8 char boundaries (transparent — the requester concatenates deltas).
- **Compute admission acks were read with a 4 KiB cap**, so a rejection carrying a long backend error string overflowed the reader and surfaced as a transport error instead of the intended `Rejected`. The ack read cap is now 64 KiB and reject-reason strings are bounded before encoding, so an ack can never exceed the cap.

## [0.19.0] - 2026-07-12

### Added
- **Guardian Compute. Decentralized edge computing over the P2P fabric**, behind the `compute` feature (default builds are unaffected). Nodes delegate the execution of business logic (compiled to WebAssembly) to other nodes, and a capability-aware scheduler routes each task to the peer with the most spare capacity; results flow back through ordinary GuardianDB replication. Reuses the existing Iroh stack — QUIC + public-key identity, the `Router`'s ALPN multiplexing, `iroh-blobs` for content-addressed code distribution, `iroh-gossip` for telemetry, the store `EventBus` for triggers, and the `AccessController` for permissioning. Design in `docs/rfcs/0002-guardian-compute.md` and `docs/rfcs/0003-guardian-compute-followups.md`; user guide in `docs/compute.md`.
  - **Sandboxed WASM runtime** (`wasmtime`, no WASI by default): every task runs in a fresh store under three hard limits, linear-memory ceiling, CPU budget (fuel), and wall-clock deadline (epoch interruption). No filesystem, network, clock, or randomness unless the executor's owner opts in. Guest-controlled lengths (output size, host-function buffers) are bounds-checked against the guest's own memory before the host allocates, so a hostile module cannot OOM the executor.
  - **Delegation protocol** over the dedicated `/guardian-db/compute/1` ALPN (postcard, length-prefixed frames): direct `execute_on` with a fast admission ack, plus a Contract-Net `Probe` for fresh capacity bids. The executor fetches the task's `.wasm` from the requester by BLAKE3 hash (integrity verified by construction) and caches compiled modules.
  - **Capability-aware scheduler**: telemetry gossiped with hysteresis over the `guardian-db/compute/capabilities/2` topic feeds a directory the scheduler ranks by idle cores, free memory, load, battery, accelerators, model affinity, and reputation. `execute` (best node with automatic failover — node-specific failures fail over, deterministic ones are final), `execute_with_auction` (probe top candidates for a fresh bid), `map` (MapReduce fan-out), and `execute_redundant` (k-of-n majority with reputation penalties for divergence, persisted across rounds).
  - **Opt-in host capabilities** (`HostGrants`): `gdb.log` and `gdb.store_get` (read the executor's local store), granted by the owner and refused at instantiation otherwise.
  - **Reactive triggers + task ledger**: `on_replicated` rules dispatch tasks when data lands, deduplicated across replicas by a deterministic ledger key and an atomic `Pending → Running` claim (no double-dispatch). Lifecycle (`Pending → Running → Done | Failed`) is recorded through a `LedgerStore` abstraction (`MemoryLedger`, or a replicated store), with deadline-based requeue of abandoned/transiently-failed tasks.
  - **Edge AI** (`compute-nn` feature, implies `compute`): the `wasi-nn` API linked into the sandbox as an opt-in grant, backed by ONNX Runtime. Owner-curated named models distributed as iroh blobs (fetched by hash, ONNX sessions cached), model-affinity routing (`required_model`), and GPU via `compute-nn-cuda` (`NnTarget::Gpu` with safe CPU fallback and CUDA availability verified against advertised `Accel::Gpu`).
  - **Task-authoring SDK**: two companion workspace crates, `guardian-compute-sdk` and `guardian-compute-sdk-macros`: write a task as an ordinary function with `#[guardian_task]` (raw `&[u8]` I/O, or typed CBOR I/O behind the `cbor` feature), compile to `wasm32-unknown-unknown`, and publish the `.wasm` as a blob. Host-function bindings (`guardian::log`, `guardian::store_get`) behind the `host` feature.
  - **Trust model:** the sandbox protects the executor; trust in the *result* is addressed by running in permissioned networks (governed by the `AccessController`) or via redundant k-of-n execution. Participation is reciprocal, never paid, and local policy (concurrency, accepted classes, host grants, battery rule) stays sovereign.
  - **Documentation:** `docs/compute.md`.

## [0.18.0] - 2026-07-10

### Added
- **Guardian Sentinel. A terminal UI (TUI) for inspecting, managing, and monitoring GuardianDB**, behind the `sentinel` Cargo feature (default builds are unaffected). GuardianDB was a library operable only from Rust code; Sentinel turns it into a database a non-Rust operator can drive visually, and everything created through it survives a restart.
  - **Admin RPC seam (`AdminSource`).** Inspection/management no longer touches the storage directly. A small JSON-lines RPC (mirroring the `pgwire` gateway model) exposes every operation through an `AdminSource` trait with two backends: `EmbeddedSource` (owns the `data-dir`) and `AdminClient` (socket). The `guardian-sentinel` panel consumes both uniformly — `--data-dir` (embedded) or `--connect <addr>` (attach to a live instance served by `guardian-sentinel-server` **without contending for the redb lock**). Loopback TCP by default; destructive/action ops are gated by a shared token (`--token`) per connection after an `auth` handshake.
  - **Nine inspectors/monitors** (F1–F7 navigation): Store dashboard with metrics and filtering; EventLog inspector (entry list with cursor pagination, entry detail, live search/filter, CRDT heads with divergence, branch diff, and merge timeline); KeyValue inspector (list, value detail/edit, create/delete/search/export); Access Control manager (list, per-role detail, grant/revoke with role selector, creation wizard with manifest hash); P2P Replication monitor (peer list, peer detail with shared stores, real-time sync dashboard with sparkline, duration, in-flight syncs, and aggregate throughput, alerts/diagnostics); Network Topology (ASCII star graph with real connection type, per-edge latency with global and per-peer p95/p99, relay status, known-offline peers); Keystore manager (key list with active/rotated/type metadata, detail with public key, generate/rotate — never exposes the secret); EventBus explorer (live stream with follow/pause, type + search filters, per-type counts, events/s, 60s sparkline, top peers); and Blob browser (list with real size and partial/complete status, sort by hash/size, disk usage, detail + preview, add/export/delete).
  - **Full write/management parity.** Create and persist stores through a wizard (`StoreRegistry`, a redb catalog in `<data-dir>/store_registry`, reopens every store with its `CreateDBOptions` on boot); close/drop stores; append to EventLogs and put/delete Documents (in addition to the existing KV CRUD); attach an ACL at creation; share a store as read/write `DocTicket`s and import a store from a ticket; and surface the node identity for P2P sharing.
  - **Real network metrics** wrapped from iroh 1.0 / iroh-blobs 0.103: real connection type via `remote_info` (`net.topology`), relay status via `home_relay_status` (`net.relay`), global and per-peer latency percentiles (`node.latency`), aggregate throughput (`node.throughput`), real blob size and partial/complete state via `blobs().status` (`blobs.list`), and discovered-not-connected peers (`net.discovered`).
  - **Usability layer for non-Rust operators:** contextual help (`?`) explaining each screen and the underlying concepts in plain language, action-oriented empty states, a 3-step onboarding quickstart on an empty data-dir, inline validation, destructive-action confirmations, and a client-side audit trail (`l`).
  - **Admin RPC ops implemented (all with e2e tests):** `stores.list/create/close/drop/share/import`, `node.info/identity/latency/throughput`, `kv.entries/put/delete`, `eventlog.entries` (cursor-paginated)/`eventlog.heads`/`eventlog.append`, `docs.list/get/put/delete`, `peers.list/force_sync`, `blobs.list`/`blob.get/add/export/delete`, `events.subscribe` (streaming, structured fields), `keystore.list/detail/generate` (metadata + public key only), `acl.list/grant/revoke/create`, `net.topology/relay/discovered`, and `auth`.
  - **Documentation:** `docs/SENTINEL_TUI.md`
  - **Known architectural limitations** (documented, not bugs): network-partition detection and blob provider discovery are outside the iroh model; wall-clock-per-entry timestamps and runtime `replicate` edits require core changes; all ACL controller types currently behave as `SimpleAccessController` in the core.

## [0.17.2] - 2026-07-06

### Added
- **Supabase compatibility layer** — Supabase client libraries (`supabase-js`,
  PostgREST/GoTrue clients) talk to a GuardianDB node with no GuardianDB-specific
  code, through a Kong-shaped HTTP gateway behind the `supabase` Cargo feature
  (`supabase = ["sql", "dep:axum", "dep:tower", "dep:graphql-parser"]`); default
  builds are unaffected. Served by the new `guardian-supabase` binary.
  - **REST** (PostgREST-compatible): `SELECT`/`INSERT`/`UPDATE`/`DELETE` with
    `select=`, filters, `order=`, `limit`/`offset`/`Range`, upsert via
    `Prefer: resolution=…` + `on_conflict=`, `rpc/{fn}`, and PostgREST-shaped
    errors carrying the real SQLSTATE.
  - **Auth** (GoTrue-compatible): signup, password + refresh-token grants with
    rotating refresh tokens, logout, `GET`/`PUT /user`, and `service_role`-gated
    admin user management. HS256 JWTs implemented from scratch on the in-tree
    `hmac`/`sha2`/`base64` (no `jsonwebtoken` dependency); bcrypt passwords.
  - **Row-Level Security enforced end-to-end**: each request opens a session
    bound to the effective Postgres role (bearer role ?? apikey role) with the
    verified JWT claims injected as `request.jwt.claims`; policies use the
    standard `auth.uid()`/`auth.role()`/`auth.jwt()` helpers, `service_role`
    bypasses like Supabase's service key, and `WITH CHECK` violations surface as
    `42501`/403.
  - **Storage** (storage-api-compatible): bucket CRUD, raw-body upload/download
    (with `x-upsert`), public + signed (HS256) URLs, move/copy/list/delete, per-
    bucket size/mime limits — object bytes replicate through the same document
    store as every other row.
  - **postgres-meta** (`/pg-meta`, alias `/platform/pg-meta`): the catalog and
    `pg_catalog` views Supabase Studio needs (schemas, tables, columns, indexes,
    constraints, policies, views, types, roles, extensions) plus a `POST /query`
    SQL-editor path, all `service_role`-gated.
  - **Realtime**: a Phoenix-protocol websocket compatible with
    `@supabase/realtime-js` v2 — `postgres_changes` (fed by the engine's local
    commit hook, authorized per-subscriber against RLS) + `broadcast`, with a
    typed error for every unsupported frame.
  - **GraphQL** (`/graphql/v1`): a pg_graphql-compatible endpoint reflecting the
    `public` schema per request (collections, relationships, filters/order,
    mutations with `atMost`, keyset pagination, introspection), governed by RLS
    exactly like REST.
  - The remaining Kong service (**Edge Functions**) and out-of-subset features
    return typed `501`/GraphQL errors — never a bare 404 and never fake success.
  - **Documentation:** `docs/supabase-compat.md` (routes, RLS semantics, typed-
    error taxonomy, Studio wiring, and deferred slices); in-process gateway,
    storage/realtime, and pg_graphql integration test suites.

## [0.17.1] - 2026-06-29

### Added
- **Read-only replication with a cryptographic write guarantee** for iroh-docs-backed stores (KeyValue/Document), enabling a "one/two writers, many readers" topology where a reader cannot write — even if its software is compromised — because it never receives the namespace write secret.
  - **Per-role ticket capability:** the ticket exchange now hands out a ticket matching the requester's authenticated role — a **read ticket** (`NamespaceId` only, no write secret) to readers and a **write ticket** (namespace secret) to write-authorized peers. The `TicketProvider` holds both pre-generated tickets and authorization returns the granted mode (`GrantedMode::Read`/`Write`), with `write` taking precedence over `read`.
  - **`CreateDBOptions.read_only`:** opening a store read-only refuses local `put`/`delete` (fail-fast on the public `add_operation` write path and the inherent methods) and never creates a namespace (fail-closed when no ticket or cached namespace is available), so a reader cannot mint its own write secret.
  - **Writability tracking:** each store records whether it holds the namespace secret (created locally, imported via a write vs read `DocTicket`, or reopened from a persisted flag); effective writability is `holds_secret && !read_only`.
  - **`CreateAccessControllerOptions::read_only_replication(writers)`** helper to build the recommended ACL (`write: [writers]`, `read: ["*"]`).
  - **Namespace rotation** support to truly revoke a provisioned writer (the namespace secret is symmetric and cannot be retracted via ACL edits): new `guardian_db::rotation::copy_key_value_state` helper to migrate state into a fresh namespace, plus a `docs/NAMESPACE_ROTATION.md` runbook.
  - **Documentation:** `docs/READ_ONLY_REPLICATION.md` explaining the guarantee, the iroh-docs namespace-secret model, the enforcement layers, and usage.
  - **Tests:** ticket-exchange per-role authorization (read-only peer never receives write, write precedence, no write-ticket leak), read-only fail-fast and no-create unit tests, rotation helper tests, and a two real-node integration test (`tests/integration_readonly.rs`) proving a reader replicates the writer's data but cannot write while the writer's state stays intact.

### Changed
- `IrohBackend::register_ticket_provider` now takes both a read and a write ticket (was a single write-capable ticket); KV/Document stores register via a new internal `share_tickets()` that generates both. `share_ticket()` is retained for compatibility and still returns a write-capable ticket.

## [0.17.0] - 2026-06-24

### Added
- **PostgreSQL compatibility layer** — standard PostgreSQL clients (`psql`,
  node-postgres, **TypeORM** with `type: "postgres"`, DBeaver) connect to
  GuardianDB over the PostgreSQL wire protocol and run ordinary SQL with no
  GuardianDB-specific client code.
  - New feature-gated modules inside `guardian-db` (the engine is
    storage-agnostic, driven through the `RelationalStorage` trait):
    `guardian_db::relational` (PostgreSQL type system, value model, serializable
    catalog, storage trait, BTree indexes, SQLSTATE errors) and
    `guardian_db::sql` (sqlparser-based parser/planner/executor for DDL, DML with
    RETURNING/ON CONFLICT, SELECT with joins/aggregates/subqueries/CTEs/set-ops,
    expressions, local-atomic transactions, parameter binding, and
    `information_schema`/`pg_catalog` introspection) behind the `sql` feature;
    `guardian_db::pgwire` (wire-protocol server on `127.0.0.1:15432` with simple
    + extended query, prepared statements and SQLSTATE errors) plus the
    `guardian-pgwire` binary behind the `pgwire` feature.
  - `sql` feature of `guardian-db` adds `guardian_db::sql`, a
    `RelationalStorage` adapter over a replicated GuardianDB document store,
    preserving the local-first / P2P model (verified on a real iroh node).
  - A PostgreSQL-style **lock manager** for the single-node gateway: all eight
    table-lock modes with the exact conflict matrix, row locks (`FOR UPDATE`/
    `FOR SHARE` with `NOWAIT`/`SKIP LOCKED`), advisory locks (session/xact,
    shared, try, two-key), `LOCK TABLE`, blocking waits with `lock_timeout`,
    deadlock detection (`40P01`), transaction-abort semantics (`25P02`), and
    `pg_catalog.pg_locks` monitoring.
  - `examples/postgres-typeorm` (runnable TypeORM app with migration/seed/
    queries/transactions), `packages/guardiandb-postgres-typeorm` (`GuardianDataSource`),
    and `tests/postgres-compat` (node-postgres + TypeORM conformance, 16 tests).
  - `docs/postgres-compat.md` with consistency/transaction/replication semantics
    and a compatibility matrix; `tests/sql_conformance.rs`
    pins documented gaps (clean-failure and `#[ignore]` tests).


- **Optional ODM layer (`odm` feature)** for TypeORM/Mongoose-style document modeling on top of `DocumentStore`, without replacing GuardianDB's decentralized Iroh Docs/Willow storage model.
  - Added `guardian-db-derive` with `#[derive(Model)]`, `#[primary_key]`, `#[unique]`, `#[index]`, `#[model(collection = "...")]`, `#[model(timestamps)]`, flexible schemas, and schema version metadata.
  - Added typed and dynamic collection APIs with `insert_one`, batch `insert`, `find_one`, `find`, `find_by_id`, and first-match `update`.
  - Added MongoDB-style query/update support including equality filters, comparison/logical operators, dot paths, `$set`, `$unset`, and `$inc`.
  - Added local validation for required fields, nullability, field types, strict schemas, immutable primary keys, primary-key uniqueness, unique constraints, and secondary indexes.
  - Added `GuardianDB::init_collection`, `GuardianDB::list_collections`, and `GuardianDB::model_collection::<T>()` helpers under the ODM feature.
  - Added local transaction/consistency API scaffolding (`TransactionContext`, `ConsistencyLevel`) that explicitly rejects unsupported replicated transaction semantics until a distributed coordinator exists.
- **TypeScript ODM SDK scaffold** in `packages/guardiandb-odm-typescript` exposing `GuardianDB.init`, `GuardianDB.listDatabases`, `initCollection`, `listCollections`, and Mongoose-style collection CRUD through a `GuardianTransport` boundary.
  - Includes a process-local reference transport for deterministic SDK tests and future native Node/WASM/mobile bridge development.
- **ODM documentation and tests**, including `docs/odm.md`, Rust ODM integration tests, and TypeScript SDK tests covering the issue #17 usage flow, uniqueness rollback, update operators, collection listing, and version-conflict behavior.

### Changed
- **Upgraded to Iroh 1.0.** Bumped `iroh` 0.92 → **1.0.0**, `iroh-blobs` → **0.103**, `iroh-gossip` → **0.101**, `iroh-docs` → **0.101**, `iroh-io` → 0.6.1, and added **`iroh-mdns-address-lookup` 0.4** for LAN discovery (these crates remain separately versioned in 1.0).
  - Migrated the API surface: `NodeId`→`EndpointId`, `NodeAddr`→`EndpointAddr` (with unified `TransportAddr`), `discovery()`→`address_lookup()` using the `N0` preset + mDNS, async `remote_info()`, `endpoint.id()`, `connection.remote_id()`, and the new `BlobsProtocol`/`Endpoint::builder(preset)` signatures.
- **Unified randomness on a single `rand` crate.** Removed the direct `rand_core` 0.6.4 pin (no longer needed now that `SecretKey::generate()` takes no RNG), updated `rand` → **0.10**, and set `ed25519-dalek` → 2.2 with the `serde` feature.
- `DocumentStore` opening is now idempotent for ODM collection initialization so repeated collection setup can reuse the underlying replicated document store safely.
- Root README now documents the optional ODM layer, Rust model derive usage, TypeScript collection API shape, build/test commands, and the local-vs-replicated consistency boundary.

### Removed
- **Legacy `replicator` module and `ReplicationInfo` type** (OrbitDB lineage). Replication is handled natively by Iroh, so the vestigial progress-tracking surface was removed: the `Store::replication_status()` trait method and all implementations, the `BaseStore` replication field, and the dead `update/recalculate_replication_*` and `replication_load_complete` helpers.

### Fixed
- **ODM `$inc` now preserves integer types** (e.g. `1024 + 1` yields `1025`, not `1025.0`); it only falls back to floating-point arithmetic for fractional operands or i64 overflow. Fixes a failing ODM reliability test.

## [0.16.0] - 2026-03-01

## [0.15.0] - 2026-02-17

## [0.14.0] - 2026-01-08

### Added
- **Access Control Integration Test Suite**: Comprehensive test suite with 15 integration tests in `tests/integration_access_control.rs`
  - All 15 tests passing with complete coverage of access control system
  - Tests for SimpleAccessController: basic operations, permissions, can_append, wildcard access
  - Tests for GuardianDBAccessController: basic operations, persistence with skip_manifest
  - Tests for IrohAccessController: basic operations, permissions, can_append
  - Integration tests: access control with keyvalue stores, multiple controllers, type validation
  - Complete validation of authorization system with cryptographic verification
- **Integration Replication Test Suite**: Comprehensive test suite with 13 integration tests in `tests/integration_replication.rs`
  - All 13 tests passing with proper cache isolation
  - Tests covering two-node replication, concurrent operations, network partition recovery, and store isolation
  - Sequential operations, high-frequency updates, multi-node scenarios
  - Complete validation of P2P replication system using iroh-gossip
- **GuardianDBAccessController Comprehensive Test Suite**: New robust test suite with 33 integration tests in `src/tests/acl_guardian_comprehensive_test.rs`
  - All 33 tests passing when executed sequentially (`--test-threads=1`)
  - Complete coverage of GuardianDBAccessController functionality
  - Permission management: grant, revoke, duplicate permissions, role-based access
  - Access control: can_append with authorized/unauthorized users, wildcard support, admin inheritance
  - Concurrency tests: concurrent grants and mixed grant/revoke operations
  - Persistence: save/load operations and address validation
  - Edge cases: special characters in identities, very long identities, empty roles, stress tests with 100+ identities
  - Mock implementations for testing: MockLogEntry, MockCanAppendContext, MockIdentityProvider
- **IrohAccessController Comprehensive Test Suite**: New robust test suite with 26 integration tests in `src/tests/iroh_access_controller_test.rs`
  - All 26 tests passing with proper isolation using unique temporary directories
  - Complete coverage of IrohAccessController functionality
  - Basic tests: controller creation, default permissions, address validation
  - Permission management: grant, revoke, duplicate permissions, invalid capabilities
  - Access control: can_append with authorized/unauthorized users, wildcard support
  - Role-based access: write, read, admin, and unknown roles
  - Persistence: save controller, save/load round-trip validation
  - Concurrency: concurrent grants, mixed grant/revoke operations
  - CBOR serialization: simple data, empty lists, special characters/unicode
  - Edge cases: close controller, empty identity, many permissions (100+)
  - Test isolation achieved with unique directory generation per test
- **Comprehensive Guardian Module Test Suite**: New robust test suite with 40 integration tests in `src/tests/guardian_mod_test.rs`
  - 33 tests passing
  - Complete coverage of GuardianDB creation and configuration
  - EventLogStore tests: add_operation, get_by_hash, list operations, multiple operations
  - KeyValueStore tests: put, get, delete, all, update, concurrent operations, special characters, large values
  - DocumentStore tests: put, delete, query with filters, batch operations, complex JSON handling
  - Integration tests: access controller registration, event bus integration, multiple stores
  - Edge cases: empty stores, concurrent operations, store load and sync
- **Interior Mutability API Refactoring**: Comprehensive refactoring of trait method signatures from `&mut self` to `&self`
  - Updated 13 trait method signatures across `Store`, `EventLogStore`, `KeyValueStore`, and `DocumentStore` traits
  - Improved ergonomics when using `Arc<dyn Trait>` by exposing existing interior mutability pattern
  - Thread-safety maintained through existing `Arc<RwLock<T>>` implementation in BaseStore
  - Methods affected: `drop()`, `load()`, `sync()`, `load_more_from()`, `load_from_snapshot()`, `add_operation()`, `add()`, `put()`, `delete()`, `put_batch()`, `put_all()`
- **Guardian Wrapper Integration Tests**: New comprehensive test suite with 15 integration tests
  - Tests for GuardianDB creation and configuration
  - Tests for EventLogStore, KeyValueStore, and DocumentStore creation and basic operations
  - Tests for access control registration
  - Tests for multiple stores and different addresses
  - All tests passing with proper isolation
- **Reactive Synchronization System**: New `reactive_synchronizer` module with `SyncObserver` pattern for real-time observability
  - `SyncObserver` allows external components (UI, monitoring) to observe sync operations in real-time
  - `SyncProgress` tracking with completion percentage and state management
  - `SyncEvent` enum for Started, Progress, Ready, Replicated, and Error events
  - Integrated into BaseStore's `load()`, `load_more_from()`, and `sync()` methods
  - Provides `sync_observer()` getter for external access
- New helper method `Entry::payload_str()` for convenient UTF-8 string conversion from binary payload

### Changed
- **BREAKING**: Complete migration from secp256k1 to ed25519 for cryptographic operations
  - `DefaultIdentificator` now uses `ed25519_dalek` for all signing and verification
  - Public keys reduced from 65 bytes (secp256k1 uncompressed) to 32 bytes (ed25519)
  - Signatures now 64 bytes, hex-encoded for storage
  - Alignment with Iroh's native ed25519 usage for better compatibility
  - Identity creation signs `id + type` (e.g., "hash" + "GuardianDB") for verification
  - Fixed `signatures_map()` to properly decode hex signatures to bytes
  - Removed SHA256 message hashing - ed25519 signs raw bytes directly
- **Architecture Simplification**: Removed redundant RawPubSub wrapper layer in P2P messaging
  - BaseStore now uses EpidemicPubSub directly via trait downcast for replication
  - Guardian core instantiates EpidemicPubSub directly from IrohBackend
  - Simplified architecture eliminates unnecessary middleware layer
  - Updated all unit tests to use EpidemicPubSub directly
- **Cache Isolation**: Fixed cache directory isolation for concurrent test execution
  - BaseStore.create_cache() now accepts configurable cache directory parameter
  - Each GuardianDB instance uses isolated cache directory under its data path
  - Eliminates Sled DB file lock conflicts when multiple nodes run simultaneously
  - Enables parallel test execution without cache contention
- **BREAKING**: API method signatures changed from `&mut self` to `&self` for better Arc compatibility
  - All Store trait implementations updated across 6 core files
  - Updated files: `src/traits.rs`, `src/guardian/mod.rs`, `src/access_control/mod.rs`, `src/stores/base_store/mod.rs`, `src/stores/document_store/mod.rs`, `src/stores/event_log_store/mod.rs`, `src/stores/kv_store/mod.rs`
  - No functional changes - interior mutability was already present, now properly exposed
- **BREAKING**: Complete migration from JSON to Postcard binary serialization for all internal CRDT structures
  - Entry.payload type changed from `String` to `Vec<u8>` for efficient binary storage
  - MessageMarshaler now uses Postcard for all message serialization
  - Operation serialization migrated to Postcard
  - Snapshot serialization migrated to Postcard
  - AccessController operations migrated to Postcard
  - ACL Guardian and ACL Iroh permission storage migrated to Postcard
  - Entry.from_hash() deserialization migrated to Postcard
  - Log.snapshot() now uses Postcard for consistent Entry serialization
- Replaced HashMap with BTreeMap in serialized structures for deterministic BLAKE3 hashing
- Added dedicated serialization module (`src/guardian/serializer.rs`) wrapping Postcard with comprehensive size comparison tests
- Updated all test files to work with binary Entry.payload (using `b"..."` byte strings)
- Updated example files to use `.as_bytes()` and `.into_bytes()` for Entry creation

### Removed
- **P2P Messaging**: Removed redundant RawPubSub module from `src/p2p/messaging/raw.rs`
  - Eliminated ~200 lines of unnecessary middleware code
  - Direct use of EpidemicPubSub reduces complexity and improves maintainability
  - No functional changes - all replication features preserved
- **BREAKING**: Removed entire Replicator module (~1000 lines) that duplicated Iroh's native functionality
  - Removed `Replicator` struct and all associated methods from `src/stores/replicator/`
  - Removed `replicator()` method from `Store` trait
  - Removed replicator field and methods from `BaseStore`, `DocumentStore`, `KeyValueStore`, `EventLogStore`
  - Removed 4 replicator method implementations from Guardian wrappers
  - Kept minimal `Replcache isolation issue preventing concurrent test execution
  - Modified BaseStore to accept cache directory as parameter instead of hardcoded "./cache"
  - Each node now uses unique cache path based on its configured directory
  - Resolves "could not acquire lock" errors from Sled DB file conflicts
- **Critical**: Fixed "RawPubSub requires mutable access" error in BaseStore replication
  - BaseStore now correctly downcasts to EpidemicPubSub for topic subscription
  - Enables proper replication via iroh-gossip protocol
- **Critical**: Fixed ReplicationInfo` struct for compatibility with existing progress tracking
  - All replication now handled natively by Iroh's gossipsub and docs protocols

### Fixed
- **Critical**: Fixed GuardianDBAccessController initialization using hash address instead of name
  - Changed from using `params.address().to_string()` (64-char hex) to `params.get_name()`
  - Prevents "O nome do banco de dados fornecido já é um endereço válido" error
  - Generates unique timestamp-based names when no name is provided
  - Properly configures EventBus in CreateDBOptions to prevent "EventBus is a required option" errors
- **Critical**: Fixed identity signature verification in access control system
  - Aligned signature creation in `DefaultIdentificator::create()` with verification in `verify_identity()`
  - Signatures now verify `id + type` format consistently across the system
  - Enables cryptographic verification in can_append operations
  - All access controller tests now pass with ed25519 verification
- **Critical**: Fixed DocumentStore DELETE operation not being respected in oplog fallback queries
  - Modified `search_documents_from_oplog` in `src/guardian/mod.rs` to properly handle DELETE operations
  - Implemented reverse iteration through oplog (newest to oldest) to process only the most recent operation per key
  - Added tracking of processed keys to prevent duplicate processing
  - DELETE operations now correctly filter out deleted documents from query results
  - Ensures consistency between index-based queries and oplog fallback queries
- **Critical**: Fixed manifest loading issue when creating stores with `overwrite` option
  - `GuardianDB::open()` now checks if `overwrite` is true and uses `store_type` directly instead of trying to read non-existent manifests
  - `GuardianDBAccessController::new()` now properly passes `skip_manifest` flag as `overwrite` to store creation options
  - Fixes "entity not found" errors when creating new stores in tests and production scenarios
  - Enables proper test execution with skip_manifest flag
- **Critical**: Fixed CBOR/Postcard serialization inconsistency in IrohAccessController
  - Removed incorrect binary-to-string conversion using `String::from_utf8_lossy` that could corrupt data
  - Changed `CborWriteAccess.write` field from `String` to `Vec<String>` for native CBOR serialization
  - Eliminated unnecessary double serialization (Postcard wrapped in CBOR)
  - `save()` method now serializes permissions directly to CBOR without intermediate Postcard step
  - `load()` method now deserializes directly from CBOR, extracting `Vec<String>` natively
  - Ensures data integrity and consistency with manifest serialization format
  - Maintains separation of concerns: CBOR for protocol/metadata, Postcard for application data
- **Critical**: Removed 55+ unnecessary downcast blocks in `src/guardian/mod.rs` that were causing runtime errors
  - Fixed "Não foi possível fazer downcast para BaseStore" errors in EventLogStoreWrapper, KeyValueStoreWrapper, DocumentStoreWrapper, and KeyValueStoreBoxWrapper
  - Wrappers now delegate directly to trait methods instead of downcasting to BaseStore
  - Improved code reliability and eliminated runtime panics in store operations
- **Critical**: Fixed `load_more_from` method signature in all Store implementations
  - Method now correctly passes both `amount: u64` and `entries` parameters
- **Critical**: Fixed EventBus loss bug in `core.rs` where event_bus was not preserved when recreating CreateDBOptions
  - EventBus is now explicitly preserved when creating new options in the `open()` method
  - Ensures all stores receive proper EventBus configuration
- Fixed EventBus propagation in GuardianDB wrapper methods (`log()`, `key_value()`, `docs()`)
  - Methods now explicitly pass EventBus from GuardianDB to store creation options
  - Prevents "EventBus is a required option" errors during store creation
- Fixed test isolation issues by using unique store names per test
- Fixed `io::Error` to `GuardianError` conversion to properly handle `ErrorKind::NotFound` and `ErrorKind::TimedOut`
- Fixed `IrohClient` node_id synchronization - now uses backend's persistent secret_key instead of generating separate key
- Added `secret_key()` getter to `IrohBackend` for key consistency

### Performance
- **65-84% reduction** in serialized data size compared to JSON
- **~6x faster** serialization/deserialization performance
- **Deterministic hashing**: Consistent BLAKE3 hashes across all operations

### Security
- Deterministic serialization prevents hash collision attacks
- Binary format reduces attack surface compared to JSON parsing

## [0.11.18] - 2025-11-18

### Changed
- Integrated the batch processor with the Iroh backend, enabling more efficient batch operations.
- Fully implemented the create_controller function in ac/utils.rs, improving internal component orchestration.
- Refactored the former pubsub module, now renamed to p2p, including:

Peer connection system with verified handshake in DirectChannel.
Discovery beacon mechanism for automatic peer discovery.
Connection retry logic with proper timeout management.
Comprehensive peer identity verification during handshake.

- Renamed modules for improved architectural clarity:
src/iface.rs → traits.rs
ipfs_log/iface.rs → traits.rs

- General system improvements, including better stability, organization, and performance.

### Fixed
- Architecture improvements

## [0.10.15] - 2025-10-15

### Added
- New documentations
- Migration to Tracing complete
- Introducing the Embedded Iroh IPFS Node

### Fixed
- Fixed design error in the implementation of the trait BaseGuardianDB for GuardianDB
- Architecture improvements

## [0.9.13] - 2025-09-13

### Added
- Event system improvements and cleanup
- Protocol Buffer support for all workflows
- Multi-platform CI/CD pipeline

### Fixed
- Removed unused DummyEmitterInterface
- Fixed GitHub Actions compilation issues with libp2p-core

### Security
- Comprehensive security audit integration
- Dependency vulnerability scanning

## How to Release

1. Update version in `Cargo.toml`
2. Update this CHANGELOG.md
3. Commit changes: `git commit -am "chore: release v0.X.Y"`
4. Create and push tag: `git tag v0.X.Y && git push origin v0.X.Y`
5. GitHub Actions will automatically:
   - Create GitHub release
   - Build multi-platform binaries
   - Publish to crates.io (for stable releases)

## Version Strategy - Supported Release Types

- **Major (X.0.0)**: Breaking API changes (Automatically publishes to crates.io)
- **Minor (0.X.0)**: New features, backward compatible (Automatically publishes to crates.io)
- **Patch (0.0.X)**: Bug fixes, backward compatible (Automatically publishes to crates.io)
- **Pre-release**: `v1.0.0-alpha.1` (Publishes only on GitHub)
- **Beta**:  `v1.0.0-beta.1` (Publishes only on GitHub)
- **Release Candidate**: `v1.0.0-rc.1` (Publishes only on GitHub)
