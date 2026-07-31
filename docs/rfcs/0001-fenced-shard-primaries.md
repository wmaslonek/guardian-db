# RFC: Fenced Shard Primaries — Sharding and Read/Write Replication for GuardianDB's PostgreSQL Layer

**Status:** Proposed — no part of this design is implemented yet. `Consistency::Strict`
is currently a flag with no call sites and `tests/sql_replication.rs` is `#[ignore]`d;
this RFC is the plan to make strict mode real.
**Scope:** `sql` / `pgwire` features of the `guardian-db` crate
**Relates to:** [docs/postgres-compat.md](../postgres-compat.md) §7 (consistency modes), §9 (replication semantics); `tests/sql_replication.rs`
**Core design:** one write-once iroh-docs log document per commit, keyed by `(epoch, seq)`
and written by a single lease-fenced primary per shard, so per-key LWW never arbitrates
SQL state and wall clocks leave the data plane entirely. §11 records how the design
resists the known failure modes of distributed SQL over an AP substrate, and the
alternatives it rejects.

---

## 1. Motivation

The SQL engine is already storage-agnostic: `Database<S: RelationalStorage>` (src/sql/engine.rs:18) touches persistence only through the seven async methods of `RelationalStorage` (src/relational/storage.rs:25), and `GuardianRelationalStorage` (src/sql/guardian_storage.rs:56) maps tables to key-prefixed documents in one replicated iroh-docs namespace. But everything above that boundary assumes one process:

- **Concurrency control is process-local.** The `LockManager` (src/sql/lock.rs:268) is a `Mutex<Inner>` + `tokio::Notify`, self-described as the "single-node gateway" coordinator (lock.rs:1-12). Two engine processes have disjoint lock spaces; `FOR UPDATE`, `LOCK TABLE`, and unique enforcement mean nothing across nodes.
- **Commit is not atomic in storage.** `Session::commit` (src/sql/engine.rs:425-449) replays the transaction overlay as sequential `put`/`delete` calls in HashMap order plus a separate catalog save. A crash mid-flush leaves a torn transaction; a remote peer syncing mid-flush observes one.
- **The catalog is one blind-written LWW document** (`__gdb_sql_catalog\u{1f}catalog`, guardian_storage.rs:170-176). Concurrent DDL clobbers the entire schema; `Catalog::allocate_oid` (src/relational/catalog.rs:247) and `next_sequence_value` (catalog.rs:480) collide across writers.
- **Uniqueness is check-then-write against a local view** (`LoadedTable::check_unique`, src/sql/store.rs:95). Two peers insert the same PK; iroh-docs wall-clock LWW silently drops one.
- **Reads see a local mirror** refreshed by `refresh()`/`store.load(0)` (guardian_storage.rs:86) or a background LiveEvent loop; the example gateway papers over this with a 2-second poll (examples/postgres_iroh_gateway.rs:107).
- **`Consistency::Strict` is a flag with no call sites** (guardian_storage.rs:41-53); docs/postgres-compat.md:314-322 promises "a single-writer leader per database" that does not exist; tests/sql_replication.rs is `#[ignore]`d.

This RFC makes `Consistency::Strict` real. The central move: **the commit point becomes a single write-once iroh-docs document keyed by `(epoch, seq)`, written by exactly one lease-fenced primary per shard.** Because no two legitimate writers ever write the same log key, iroh-docs per-key LWW never arbitrates SQL state, and wall clocks are removed from the data plane entirely. Replicas consume the log in order and therefore always expose a transaction-consistent prefix. `LocalFirst` mode remains byte-identical in semantics, gaining only diagnostics.

---

## 2. Design Overview

- A **shard** is a catalog-declared colocation group of whole tables, backed by one iroh-docs namespace (one `GuardianDBDocumentStore`). Shard 0 is the **system shard**: catalog, shard map, witness configuration, sequence allocation, move state machine.
- Each shard has exactly one **primary** at a time, holding a witness-quorum **lease** with a monotone **epoch**. All writes for the shard's tables route to it (pgwire forwarding). The primary runs today's engine unchanged — the `LockManager` becomes globally sound *by topology*.
- The primary reifies each committed transaction as one **LogEntry document** at key `__gdb_sql_log\u{1f}{epoch:016x}\u{1f}{seq:016x}`. That single `set_bytes` is the commit point. Row documents are materialized afterward as an idempotent, LSN-stamped cache.
- **Replicas never derive SQL state from wall-clock LWW.** They apply log entries in strict `(epoch, seq)` order, gated by a quorum-ack watermark; row-doc arbitration (bootstrap/restart only) selects per key by *fencing-aware LSN*, never by iroh timestamp.
- **Epoch adoption** makes fencing total: the first entry of epoch e+1 records exactly where epoch e legitimately ends. Everything in epoch e beyond that watermark is fenced — deterministically discarded by every replica, witness, and bootstrap, forever.
- `LocalFirst` mode: unchanged AP/LWW semantics, plus a `guardian_conflicts` diagnostic view and dual-author audit.

---

## 3. Consistency Model

Terminology: `Lsn = (epoch, seq)`. The **valid log** of a shard is defined by the adoption chain: entry `(e, q)` is valid iff `e` is in the chain and `q ≤ watermark(e)` recorded by the `Adopt` entry of `e+1`, or `e` is the current epoch. `stable_upto` is the highest Lsn the primary has acknowledged as durable under the shard's ack tier; replicas apply only entries `≤ stable_upto`.

### 3.1 Strict mode, per operation class

| Class | Guarantee |
|---|---|
| **Single-shard write (autocommit or txn)** | Totally ordered per shard by Lsn. Atomic: the log append is the commit; materialization is idempotent replay. Durable per ack tier (§6.4): `quorum` (default when witnesses exist) — an acked commit survives any single failover and any minority loss; `replicated(k)` — survives k−1 log-holder losses, survives failover iff the ack set intersects the election quorum; `local` — node-durable only. Isolation at `READ COMMITTED` is exactly today's engine (per-statement re-read, no MVCC). `SERIALIZABLE` = table-granular strict 2PL via the existing `LockMode` conflict matrix (lock.rs:124): read tables take `ShareLock`, written tables `RowExclusive`+, held to commit → **per-shard strict serializability**, with blocking and `40P01` deadlocks rather than `40001` aborts. |
| **PK / UNIQUE enforcement** | Immediate and total per shard. All writes to a table serialize through one process, so `check_unique` (store.rs:95) is sound; across failover the new primary replays the full valid log before serving, so its view is complete. Tables are never split across shards, so cross-shard uniqueness does not arise. |
| **Reads at the primary** | Linearizable with respect to that shard's commits; read-your-writes; monotonic. |
| **Strict replica reads** (`guardian.read_freshness='strict'`) | Linearizable per shard: seq-barrier against the current lease holder, or transparent routing to it. If the primary is unreachable, fail `57P03` — never silent downgrade. |
| **Bounded / prefix replica reads** | A **transaction-consistent prefix** of the shard's commit order. Torn transactions are *structurally excluded*: replicas apply whole LogEntries in order, never raw row docs. Monotonic reads per session pinned to a replica. Staleness ≤ bound for `bounded_<ms>`, else per `guardian.staleness_action`: `error` (57P03 `GDB_STALENESS_EXCEEDED`) or `warn` (proceed + SQLSTATE 01000 warning). |
| **Sync-token reads** | Opt-in cross-endpoint read-your-writes: `SHOW guardian.sync_token` after commit returns the session's per-shard Lsn vector; `SET guardian.wait_for_token = '<token>'` on a replica blocks until `applied ≥ token` per shard or deadline (`57P03 GDB_TOKEN_WAIT_TIMEOUT`). **Sound here where it was not in the read/write-split design**: the token is a position in a totally ordered log that replicas apply gaplessly, so `applied ≥ token` implies *all* of the session's writes are visible — no reliance on per-author delivery order from Willow reconciliation. |
| **Cross-shard read-only queries** | Permitted. Each shard contributes a consistent prefix; prefixes are not mutually aligned — a query may see T2 on shard B but not earlier T1 on shard A. **Named permitted anomaly: cross-shard fractured read / causal reverse.** `strict` freshness narrows the window via per-shard barriers but forms no global snapshot. |
| **Cross-shard write transactions** | Rejected with `0A000 GDB_CROSS_SHARD_TXN` through Phase 5. Phase 6 (optional) adds 2PC: atomic and durable, per-shard serializable, **not** globally strictly serializable — the docs are revised to say exactly that. |
| **DDL, OIDs, sequences** | DDL executes only as logged transactions on the system-shard primary: totally ordered, no LWW clobber, no OID collision. `nextval` allocates from ranges leased to shard primaries through the system-shard log; crash forfeits an unused range remainder — **gaps possible, duplicates impossible** (PostgreSQL-compatible). |

### 3.2 LocalFirst mode

Byte-identical semantics to today, as docs/postgres-compat.md:299-313 promises: any write-ticket holder writes locally; per-key LWW convergence; eventual (not immediate) uniqueness; no cross-key atomicity across replication. Additive diagnostics only: `guardian_conflicts` view surfacing per-key LWW losers (iroh-docs retains the latest entry per `(key, author)`, so the view reports divergent author-latest entries — latest-per-author only, not full history; this limitation is documented), and the dual-author audit. The two modes never mix within one database.

### 3.3 Anomaly ledger

**Excluded (strict mode):** torn transactional reads on replicas; wall-clock LWW arbitration of any SQL state; dual-primary data divergence; acked-commit loss (at `quorum` tier); duplicate PKs; concurrent-DDL schema clobber; OID/sequence duplication; silently incomplete scans; silent strict→local degradation of any kind.

**Permitted and named (strict mode):** stale reads at `prefix`/`bounded` freshness; cross-shard fractured reads; non-repeatable reads at `READ COMMITTED`; blocked-writer-overwrites-from-original-snapshot within a shard (pre-existing engine behavior, docs/postgres-compat.md:369-371); `08007` commit-outcome-unknown when a connection drops racing the ack; sequence gaps; **fenced-read** — at ack tiers below `quorum` only, a replica may briefly serve a deposed primary's never-quorum-acked tail during the failover window, retracted deterministically on adoption (see §6.3); at `quorum` tier this anomaly is excluded because `stable_upto` cannot advance without witness acks that fencing forbids.

**Permitted (LocalFirst):** everything docs/postgres-compat.md already documents, now with visibility.

---

## 4. Sharding Design

### 4.1 Shard unit and key

The shard unit is a **colocation group of whole tables**, not row ranges. This is dictated by the engine, not preference: every statement full-scans referenced tables into a `LoadedTable` (src/sql/store.rs:28; `Session::load_table`, engine.rs:473) and `check_unique` validates against that materialized view. Splitting one table's rows across nodes would force scatter-gather and distributed uniqueness on every statement — an executor rewrite, not a storage swap. Table-group sharding also keeps the ORM compatibility that PK-hash routing would break: criteria `UPDATE`s, cascades, and TypeORM `save()` read-before-write patterns are all single-table and therefore single-shard — they route whole and work unchanged. Only multi-shard *write transactions* are rejected.

- `shard_of(table) = table.shard_group`, a new catalog field beside `Table.storage_collection` (catalog.rs:137 — the atlas's designated placement seam).
- DDL: `CREATE TABLE ... WITH (shard_group = 'g')`; `ALTER TABLE t SET (shard_group = 'g2')` triggers the fenced move (§4.4). Default group `'default'` — a one-shard database is exactly the documented strict-mode promise, so Phase 1 is the degenerate deployment.
- **Storage collection naming:** `storage_collection` is assigned `__gdb_sql_rows_{uuidv7}` at `CREATE TABLE` (the field is already opaque and defaulted-only-when-empty, catalog.rs:377-379). This decouples data identity from OIDs — cheap insurance so that even an operator-error dual-writer window can never cross-wire row data or `LockObject::Table(oid)` identity.

### 4.2 Shard → storage mapping

One shard group = one iroh-docs namespace, opened via the existing `GuardianDB::docs()` create-or-open path (src/guardian/mod.rs:194). In-shard key layout is unchanged: `"{storage_collection}\u{1f}{row_id}"` (gkey, guardian_storage.rs:90), row ids from `derive_row_id` (store.rs:235). Each namespace mints read/write `DocTicket`s via `share_tickets` (document_store/mod.rs:431), distributed over the existing TICKET_ALPN exchange gated by the `AccessController` (ticket_exchange.rs:33-179): **write ticket = primary-candidate capability, read ticket = replica capability**. The default docs-store ACL for SQL namespaces is tightened from `write:'*'` (document_store/mod.rs:573-577) to the configured primary-candidate set.

A `ShardedRelationalStorage` router implements `RelationalStorage` by resolving `collection → table oid → shard` through a cached, versioned shard map and delegating to the per-shard backend. `table_lock_plan` (engine.rs:535) already yields each statement's table set before any data loads — that set determines the statement's shard(s).

### 4.3 Cross-shard queries and transactions

- Read-only multi-shard `SELECT`s: allowed; the executing gateway holds read tickets for every namespace and its own engine executes over per-shard consistent prefixes (§3.1). No SQL rewriting, no merge operator — the executor already materializes whole tables.
- Write transactions are **pinned** to the shard of their first write. Any statement binding a different shard fails `0A000 GDB_CROSS_SHARD_TXN` and aborts the transaction via the existing abort-on-error path (engine.rs:140-142).
- **Epoch/shard-map pinning:** `Transaction` (engine.rs:44) additionally snapshots the shard-map version and pinned shard's epoch at `BEGIN`, alongside its existing catalog clone. If a table move or failover invalidates the pin before `COMMIT`, the commit fails with retryable `40001 GDB_EPOCH_CHANGED` — transactions never straddle epochs or map versions.
- DDL and `nextval` route to the system-shard primary.

### 4.4 Rebalancing: fenced table moves

Moving a table between groups is a logged state machine on the system shard: (1) `MoveIntent{table, src, dst, move_epoch}`; (2) src primary takes `AccessExclusive` via its unchanged `LockManager`, writes a final per-table checkpoint entry; (3) bulk copy into the dst namespace — documents are self-describing (`__collection` wrapper, guardian_storage.rs:99-102; `__schema`/`__table`, store.rs:222-223), so the mover needs no catalog joins; (4) dst primary logs `Import{table, upto: Lsn}`; (5) system shard logs `MoveCommit` flipping `shard_group`; (6) src logs tombstone-truncate. Every step is idempotent and resumable from the recorded state; the table stays readable at src until `MoveCommit` and is never writable in two shards (`AccessExclusive` + `move_epoch` fencing). Writes to a table in its move-critical section fail retryable `40001 GDB_TABLE_MOVING`.

---

## 5. Log, Materialization, and Row-Doc Arbitration

```
LogEntry doc key:  __gdb_sql_log \u{1f} {epoch:016x} \u{1f} {seq:016x}     (write-once)
LogEntry body:     { txid, stable_upto: Lsn, batch: CommitBatch }
Adopt entry:       seq 0 of every epoch e+1: Adopt { prev: Lsn(e, watermark) }
Watermark doc:     __gdb_sql_wm \u{1f} {epoch:016x}   (single author per epoch; heartbeats stable_upto)
Checkpoint doc:    __gdb_sql_ckpt  = { lsn, adoption_chain, sweep_complete: true }
Row doc wrapper:   { _id: gkey, __collection, __lsn: Lsn, doc }              (additive field)
```

- **Commit point** = one `set_bytes` of the LogEntry. Row docs are materialized afterward, each stamped with the entry's `__lsn`.
- **Row-doc arbitration rule:** row docs are a *cache*, consulted only by a restarting primary or a bootstrapping replica, and **never selected by wall-clock LWW**. iroh-docs retains the latest entry per `(key, author)`; the index-build query fetches all authors' latest entries per key and selects the one with the **greatest Lsn that is valid under the adoption chain**, discarding invalid-Lsn docs regardless of iroh timestamp.
- **Sweep**: on winning epoch e+1, before writing any checkpoint, the new primary enumerates fenced log entries (`(e, q > watermark)` present in the namespace), collects their touched keys, and re-materializes those keys from replayed valid state under its own epoch's Lsn. The sweep re-runs whenever fenced entries arrive later via reconciliation (LiveEvent-triggered). Invariant: **any checkpoint with epoch ≥ e+1 implies the epoch-e fence sweep completed**, so bootstrap "trust row docs ≤ checkpoint" can never absorb fenced state.
- **Log compaction**: the primary prunes entries below min(checkpoint, replica-acked watermarks) minus a retention floor. A replica behind the pruned horizon performs checkpoint-based full resync (fencing-aware row-doc base ≤ checkpoint, then log tail).

---

## 6. Replication Design

### 6.1 Write path

Client → any gateway (pgwire `serve_on`, src/pgwire/mod.rs:178). `Session::execute_one`/`table_lock_plan` (engine.rs:126, :535) resolves the statement's shard. If this process is not the shard's primary, the session's statements forward verbatim over a new authenticated QUIC ALPN `gdb/sql-fwd/1` (handler plumbing modeled on `TicketProtocolHandler`, ticket_exchange.rs:75; the *protocol* is a plain request/response stream, nothing consensus-shaped), with the transaction pinned to one stream for its lifetime. On the primary: locks via the unchanged `LockManager`; execution exactly as today; at (auto)commit, the overlay is reified into a `CommitBatch` (ordered `Vec<Mutation>`, store.rs:258, plus optional catalog delta) and appended as one LogEntry — replacing the sequential put loop of engine.rs:425-449 for this backend. Then row docs materialize (grouped per shard through `DocumentStore::put_all`, src/traits.rs:759) and the checkpoint advances lazily.

### 6.2 Read path

Primaries read their own local index as today. Replicas run a **log-apply loop** hung off the existing LiveEvent subscription (`spawn_live_index_sync`, document_store/mod.rs:484): new log entries apply in strict Lsn order into the replica's relational index, gated by `stable_upto`; a seq gap stalls apply and triggers explicit reconciliation — never out-of-order apply. Each apply publishes a typed `ShardApplied{shard, lsn}` event on the existing `EventBus`/`SyncObserver` spine (src/p2p/mod.rs:20; reactive_synchronizer.rs:183) — this drives freshness GUCs and replaces the example gateway's 2s poll. Because LiveEvent/EventBus channels are bounded and lossy (gossip.rs:312), a **periodic 30s `refresh()`/`load(0)` pull** (guardian_storage.rs:86 → sync_index_from_docs, document_store/mod.rs:478) runs as an idempotent net under the push path: event loss degrades freshness by at most one period, never permanently.

### 6.3 Catch-up, bootstrap, adoption

New/lagging replica: obtain the read ticket over TICKET_ALPN; Willow-reconcile the namespace; **refuse to serve reads until first `SyncFinished`** (typed startup error `57P03 GDB_REPLICA_SYNCING`); build base state from row docs `≤ checkpoint` under the fencing-aware arbitration rule; apply the valid log tail in order. On receiving an `Adopt(e+1)` entry that fences entries it already applied (possible only at ack tiers below `quorum`), the replica discards its materialized view and deterministically replays the valid log from its last valid checkpoint — bounded by log retention, and the transient exposure is the named fenced-read anomaly (§3.3). `resolve_shared_ticket`'s timed create-local fallback (core/mod.rs:839-844) is **removed for all SQL namespaces in all modes**: opening a SQL database without an explicit ticket or resolvable shard map fails hard with `GuardianError::NamespaceUnresolved` — the gateway refuses to start, because a forked namespace is the one split-brain nothing downstream can heal.

### 6.4 Leases, fencing, durability

**LeaseAuthority protocol** (per shard; witnesses default to the shard's replica set — no new process type):

1. Witness persistent state at `--path`: `promised_epoch`, held log entries, lease record.
2. `Acquire(shard, candidate, e)`: witness grants iff `e > promised_epoch`, persisting the promise **before** replying; the reply carries the witness's highest held Lsn and adoption chain.
3. On majority grant, the candidate syncs log state from that majority, sets `watermark(e_prev)` = the highest valid `(e_prev, seq)` held across the majority, appends `Adopt` as `(e, 0)`, runs the sweep (§5), then serves. Lease `{shard, epoch, holder: EndpointId, expires_at}` is announced on the gossip control topic (`EpidemicPubSub`, gossip.rs:405) as a *hint only* — safety never depends on lossy gossip.
4. Renewal is periodic against the majority; the holder self-demotes to read-only at `expires_at − ε` on its **monotonic** clock; witnesses treat the lease as expired at `expires_at + ρ`, where ρ is the single explicitly stated timing assumption (bounded monotonic drift; lease TTL ≫ ρ).
5. **Fencing at the ack plane:** a witness that has promised `e+1` refuses log-entry acks for epoch ≤ e. Under the `quorum` ack tier, a deposed primary therefore cannot advance `stable_upto`, cannot ack clients, and its unfenced tail is invisible to replicas.

**Durability tiers** (`guardian.commit_ack`): `quorum` — acks from a majority of witnesses; **default whenever witnesses are configured**; guarantees no acked-commit loss across any failover (quorum intersection with the election majority). `replicated(k)` — k witness acks; survives k−1 log-holder losses; failover-loss possible iff the ack set misses the election quorum (named). `local` — allowed only by explicit configuration on witness-less single-node deployments. The effective tier is reported in the pgwire `ParameterStatus` at session start and via `SHOW guardian.commit_ack`.

**Dual-author audit:** every entry carries its `AuthorId`; `guardian_replication_status` flags any strict-mode namespace containing entries from two authors with overlapping timestamps, naming affected keys — a tripwire that surfaces "impossible" fencing bugs as an explicit, named alert rather than silent divergence.

**Witness reconfiguration:** a witness set permanently below majority halts shard writes by design (`57P03`). Remediation is `guardian-pgwire witness reconfigure` — an explicit configuration transaction logged through the system shard, shipped with a tested runbook and a chaos test (§12) *before* Phase 3 ships. Never an automatic quorum shrink.

---

## 7. Rust API Sketch

All capability-passing; no ambient state. Real atlas types throughout.

```rust
// ── src/relational/storage.rs — additive, default-impl'd; MemoryStorage untouched ──
#[async_trait]
pub trait RelationalStorage: Send + Sync {
    /* existing: scan / get / put / delete / truncate / load_catalog / save_catalog */
    /// Atomically commit an ordered batch. Default = today's sequential loop.
    async fn apply_batch(&self, batch: &CommitBatch) -> Result<CommitReceipt> { /* seq loop */ }
}
pub struct CommitBatch { pub mutations: Vec<Mutation>, pub catalog_delta: Option<serde_json::Value> }

// ── src/sql/shard.rs ──
pub struct ShardId(pub u16);
pub struct Epoch(pub u64);
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Lsn { pub epoch: Epoch, pub seq: u64 }

pub trait ShardMap: Send + Sync {                    // built from the catalog; versioned
    fn shard_of_table(&self, oid: u32) -> ShardId;
    fn descriptor(&self, s: ShardId) -> &ShardDesc;  // namespace id, witness set, DocTickets
    fn version(&self) -> u64;
}

/// Proof of primaryship. Required by every write-side API; dropping it demotes.
pub struct LeaseGuard { shard: ShardId, epoch: Epoch, /* renewer handle */ }
impl LeaseGuard { pub fn check(&self) -> Result<(), LeaseLost>; }

pub enum LogEntryBody {
    Adopt { prev: Lsn },                              // seq 0 of every epoch
    Commit { txid: u64, batch: CommitBatch },
    Move(MoveStep), SequenceLease { oid: u32, range: Range<i64> }, WitnessReconfig(WitnessSet),
}
pub struct LogEntry { pub lsn: Lsn, pub stable_upto: Lsn, pub body: LogEntryBody }

pub trait ShardLog: Send + Sync {
    async fn append(&self, lease: &LeaseGuard, body: LogEntryBody) -> Result<Lsn, AppendError>;
    fn subscribe(&self, from: Lsn) -> LogStream;      // strict order; gap ⇒ stall + resync
    async fn checkpoint(&self, lease: &LeaseGuard, at: Lsn) -> Result<(), AppendError>;
}
pub enum AppendError { Fenced { promised: Epoch }, QuorumUnavailable, Storage(GuardianError) }

// ── election over authenticated QUIC witnesses; protocol per §6.4, model-checked ──
pub trait LeaseAuthority: Send + Sync {
    async fn acquire(&self, shard: ShardId, me: EndpointId) -> Result<LeaseGuard, AcquireError>;
    // AcquireError::{ NoQuorum, HigherEpochPromised(Epoch), Io } — total, no silent fallback
}

pub enum CommitAck { Local, Replicated(u8), Quorum }  // default Quorum when witnesses exist

// ── router impl of RelationalStorage; per-shard role is an explicit value ──
pub enum ShardBackend {
    Primary { store: GuardianRelationalStorage, log: Arc<dyn ShardLog>, lease: LeaseGuard },
    Replica { view: ReplicaView },                    // log-applied index + freshness API
    Remote  { client: PrimaryClient },                // QUIC "gdb/sql-fwd/1"
}
pub struct ShardedRelationalStorage { map: Arc<dyn ShardMap>, backends: Vec<ShardBackend> }
#[async_trait] impl RelationalStorage for ShardedRelationalStorage { /* route by table set */ }

impl ReplicaView {
    pub fn applied(&self) -> Lsn;
    pub async fn wait_for(&self, at: Lsn, deadline: Duration) -> Result<(), StalenessError>;
    pub fn freshness(&self) -> FreshnessEstimate;     // drives bounded_<ms> + 01000 warnings
}

pub struct SyncToken(pub Vec<(ShardId, Lsn)>);        // Display/FromStr for wire transport

// ── factory (sibling of open_sql_with, guardian_storage.rs:191) ──
pub async fn open_sql_sharded(db: &GuardianDB, name: &str, cfg: StrictConfig)
    -> GuardianResult<(Arc<Database<ShardedRelationalStorage>>, ShardTickets)>;
pub struct StrictConfig {
    pub role: NodeRole,           // StaticPrimary | PrimaryCandidate | Replica
    pub commit_ack: CommitAck,
    pub witnesses: Vec<EndpointId>,
    pub consistency: Consistency, // existing enum, finally interpreted
}
```

`Database<S>`, `Session`, `LockManager`, `Mutation`, `derive_row_id`, `LoadedTable`, and the pgwire layer are unchanged. `MemoryStorage` compiles untouched.

---

## 8. pgwire / Connection-String Surface

**One connection string, any gateway** — forwarding keeps clients dumb; unmodified psql and TypeORM work, and `packages/guardiandb-postgres-typeorm`'s existing `peers`/`consistency` options (index.ts:39-41) map through unchanged.

**CLI** (activating the flags parsed-but-ignored today, src/bin/guardian-pgwire.rs:29-31):
`guardian-pgwire --addr ... --path <dir> --database app --consistency local|strict --role static-primary|primary-candidate|replica --peer <ticket|endpoint>... --witnesses <endpoint>... [--commit-ack quorum|replicated=2|local]`

**Session GUCs** (handled where `SET lock_timeout` already is, engine.rs:145-147):
- `SET guardian.read_freshness = 'strict' | 'bounded_500ms' | 'prefix'`
- `SET guardian.staleness_action = 'warn' | 'error'`
- `SHOW guardian.sync_token` / `SET guardian.wait_for_token = '<token>'`
- `SHOW guardian.commit_ack` / `SHOW guardian.shard` / `SHOW guardian.epoch`

**DDL:** `CREATE TABLE ... WITH (shard_group='g')`; `ALTER TABLE t SET (shard_group='g2')`.

**Introspection views** (synthesized like pg_locks from `LockManager::snapshot`, select.rs:558): `guardian_shards(shard_id, group, namespace, primary_endpoint, epoch, applied_lsn, checkpoint_lsn)`; `guardian_replication_lag(shard_id, replica, lag_entries, lag_ms)`; `guardian_replication_status(namespace, mode, authors, dual_author_conflict, affected_keys)`; `guardian_conflicts(key, table, authors, entries)` (LocalFirst diagnostics). pg_locks is now truthful: all lockers of a shard live in one process.

**Typed errors** (all via `pg_error`, src/pgwire/mod.rs:201; every error that has a better place to go carries it as a HINT):

| SQLSTATE | Code | Meaning / hint |
|---|---|---|
| 25006 | `GDB_READ_ONLY_SHARD` | write on non-primary; **hint = current primary endpoint + epoch** |
| 57P03 | `GDB_PRIMARY_UNAVAILABLE` | no lease holder reachable; hint = last known primary, witness quorum state |
| 57P03 | `GDB_STALENESS_EXCEEDED` / `GDB_TOKEN_WAIT_TIMEOUT` / `GDB_REPLICA_SYNCING` | freshness contract unmeetable |
| 0A000 | `GDB_CROSS_SHARD_TXN` | write txn touched a second shard; txn aborted |
| 40001 | `GDB_TABLE_MOVING` / `GDB_EPOCH_CHANGED` | retryable |
| 08006 | `GDB_SHARD_UNAVAILABLE` | forwarding stream failed |
| 08007 | `GDB_COMMIT_OUTCOME_UNKNOWN` | drop raced the ack; commit either fully happened or did not |
| startup | `GuardianError::NamespaceUnresolved` | gateway refuses to start; no create-local fallback, ever |

---

## 9. Failure-Mode Table

| Failure | Behavior |
|---|---|
| **Primary crash** | Lease expires at witnesses; candidate wins e+1, syncs from the promise majority, writes `Adopt`, sweeps, replays, serves. At `quorum` ack, **no acked commit is ever lost**; at `replicated(k)`/`local`, the loss window is named and reported via `SHOW guardian.commit_ack`. |
| **Crash mid-commit (after log append, before row materialization)** | The log entry *is* the commit; recovery replays entries beyond checkpoint over row docs idempotently. The torn-COMMIT flaw of engine.rs:425-449 is retired. |
| **Partition, primary in minority** | Primary self-demotes at `expires_at − ε` (monotonic clock); writes there fail `25006` with new-primary hint once known; majority side elects e+1 and continues. Writes are CP: no witness majority ⇒ `57P03`, never dual-primary. Reads stay available everywhere at `prefix`/`bounded`. |
| **Split-brain attempt / deposed primary keeps writing** | Three independent fences: witnesses refuse acks below promised epoch (so `stable_upto` freezes at `quorum` tier); replicas discard log entries invalid under the adoption chain; row-doc arbitration is fencing-aware Lsn, so late-reconciled fenced row docs are discarded regardless of wall-clock timestamp, and the sweep re-materializes any affected keys. Dual-author audit flags the residue by name. |
| **Clock skew / pathological wall clocks** | Wall clocks are never load-bearing for data (ordering = Lsn; readers ignore iroh timestamps). The single timing assumption is bounded monotonic drift ρ for lease expiry; violating it degrades to the fenced-overlap case above — fenced, not divergent. |
| **Replica lag / gossip or EventBus loss** | Correctness never rides on gossip; the log arrives via Willow reconciliation with gap-stall. Freshness degrades ≤ 30s via the pull net; `bounded` reads warn or fail per `staleness_action`; `prefix` always serves a consistent prefix. |
| **Replica beyond compaction horizon** | Deterministically detected (applied Lsn < retained floor); checkpoint-based full resync with fencing-aware row-doc base. |
| **Concurrent duplicate PK / UNIQUE race** | Impossible within an epoch (single writer, sound check_unique); across failover the new primary serves no writes until replay completes. LocalFirst keeps its documented eventual-uniqueness LWW behavior, now visible in `guardian_conflicts`. |
| **Concurrent DDL** | Serialized through the system-shard log; the whole-doc LWW catalog write path is unreachable in strict mode; stale-epoch catalog entries are fenced like any entry. |
| **Forwarding stream drops mid-txn** | Primary aborts the pinned txn; `Session::Drop` (engine.rs:63) releases locks; client gets `08006`, or `08007` if the drop raced the commit ack. No partial state is possible. |
| **Gateway crash** | Same as connection loss; replicas unaffected; describe_cache and Session state die with the connection as today. |
| **Crash mid table-move** | Logged state machine resumes or rolls back before `MoveCommit`; table readable at src throughout; never dual-writable (`AccessExclusive` + move_epoch). |
| **Witness set below majority** | Shard writes unavailable by design (`57P03`); remediation = logged `witness reconfigure` per the shipped runbook; chaos-tested before Phase 3 ships. |
| **Namespace unresolvable at open** | `GuardianError::NamespaceUnresolved`; gateway refuses to start. The create-local split-brain fallback is deleted for SQL namespaces in **all** modes. |
| **Epoch flip races an open transaction** | Transaction pinned its shard-map version + epoch at BEGIN; commit fails retryable `40001 GDB_EPOCH_CHANGED` instead of mixing epochs. |

---

## 10. Phased Delivery (each phase ships alone; LocalFirst stays byte-identical throughout)

**Phase 0 — Wiring and honest LocalFirst.** Activate `--path/--consistency/--peer` in guardian-pgwire over `open_sql_with`; event-driven index refresh (`ShardApplied` on the EventBus) replacing the 2s poll, plus the 30s pull net; delete the `resolve_shared_ticket` create-local fallback for SQL namespaces (`NamespaceUnresolved`); replica serve-gating until first `SyncFinished`; `guardian_replication_status` with dual-author audit; `guardian_conflicts` view. Gate: un-ignore tests/sql_replication.rs with event-based waits. *Ships: a working replicated LocalFirst read gateway with real diagnostics and zero new distributed machinery.*

**Phase 1 — Logged commits + static single primary (strict v1).** `CommitBatch`/`apply_batch` (additive, default-impl'd); LogEntry commit point; Lsn-stamped row materialization + fencing-aware arbitration rule; checkpoint docs; catalog through the log; uuid `storage_collection`; read/write split with `25006` + primary hint; ACL tightened to primary-only write tickets. Roles are static (`--role static-primary|replica`), no election. *Ships: the documented "single-writer leader per database" promise for static topologies; torn commits and catalog clobber are dead.*

**Phase 2 — Replica read path + freshness.** `ReplicaView` ordered log-apply with `stable_upto` gating; `read_freshness` + `staleness_action` GUCs; sync tokens; `guardian_shards`/`guardian_replication_lag`. *Ships: transaction-consistent-prefix replicas, bounded staleness, opt-in cross-endpoint RYW.*

**Phase 3 — Leases, fencing, failover, forwarding.** `LeaseAuthority` (spec §6.4, model-checked before merge), `Adopt` entries + sweep, `quorum` ack default, self-demotion, pgwire forwarding over `gdb/sql-fwd/1`, `witness reconfigure` + runbook. Static-primary mode remains a supported deployment forever. *Ships: automatic failover with zero acked-write loss at quorum tier.*

**Phase 4 — Shard groups.** `shard_group` catalog field + `WITH` clause; per-shard namespaces/tickets; `ShardedRelationalStorage` router off `table_lock_plan`; cross-shard write rejection; scatter reads; txn epoch/map pinning. *Ships: horizontal write scaling across table groups behind one connection string.*

**Phase 5 — Online table moves, log compaction, sequence-range leases.** *Ships: online rebalancing.*

**Phase 6 (optional, explicitly re-scoped docs).** 2PC for cross-shard write transactions, coordinated by the system-shard primary with epoch-fenced participants and presumed-abort recovery; docs state "atomic, per-shard serializable, not globally strictly serializable."

---

## 11. Design Rationale: Failure Modes and Rejected Alternatives

Each item names a way the design could go wrong — a subtle failure mode of the
Fenced Shard Primaries (FSP) approach itself, or a flaw in an alternative design
considered (GDB-RWS, a read/write-split proposal; Braided, a soft-flagged-uniqueness
proposal) — and how the design addresses or avoids it. They are the load-bearing
correctness arguments; the failure-mode table (§9) and test strategy (§12) exercise them.

1. **Checkpoint bootstrap could absorb a fenced primary's row docs via wall-clock LWW.** *Addressed* by three mechanisms in §5: row docs carry `__lsn` and are arbitrated by fencing-aware Lsn over per-`(key, author)` latest entries — never by iroh timestamp; the new primary's mandatory sweep re-materializes every key touched by fenced entries (covering the same-author-shadowing case, since fenced *log* entries enumerate touched keys); and the invariant *checkpoint epoch ≥ e+1 ⇒ sweep complete* means bootstrap-≤-checkpoint can never see fenced state. Late-arriving fenced docs after heal are discarded by the arbitration rule and trigger sweep re-runs.
2. **"Ignore epochs below highest seen" is ambiguous for lagging replicas.** *Addressed* by epoch adoption (§5): entry `(e+1, 0)` is `Adopt{prev: (e, watermark)}`, making the valid log a total, deterministic predicate. Lagging replicas apply the legitimate epoch-e tail up to the watermark and discard beyond it. A replica that applied fenced entries before learning of adoption (possible only below `quorum` ack) performs a deterministic rebuild from its last valid checkpoint; `stable_upto` gating excludes even that window at `quorum` tier.
3. **A `commit_ack=local` default would risk silent acked-commit loss.** *Addressed:* default is `quorum` whenever witnesses are configured; `local` is legal only on explicitly witness-less single-node deployments; the effective tier is surfaced in pgwire `ParameterStatus` at connect and via `SHOW guardian.commit_ack`. The `replicated(k)` failover-loss condition (ack set misses election quorum) is stated, not hidden.
4. **Witness administration is a new operational dependency.** *Addressed:* witnesses default to the shard's replica set (no new process type); sub-majority loss halts writes with `57P03` by design; remediation is the logged `witness reconfigure` transaction, shipped with a tested runbook and a dedicated chaos test *as a Phase 3 exit criterion*.
5. **LeaseAuthority is a hand-rolled consensus-lease protocol.** *Addressed by scoping and rigor, not denial:* the protocol is acknowledged as a single-decree-per-epoch promise protocol; it is fully specified (§6.4), confined to Phase 3, model-checked (stateright/TLA+-style exploration of promise/grant/expiry/drift interleavings) as a merge gate, and the static-primary deployment remains supported indefinitely so all Phase 0-2 value ships without it. Only ALPN/handler plumbing is reused from ticket_exchange — no pretense that the ticket protocol is a voting substrate.
6. **SERIALIZABLE via table 2PL can surprise users.** *Addressed by explicit re-scope:* per-shard strict serializability with blocking and `40P01` (not `40001` first-committer-wins); the `#[ignore]`d write-skew test is re-scoped accordingly and documented in postgres-compat.md §7.
7. **Mechanisms adapted from other designs do not import their flaws:** the sync token is defined on totally ordered Lsns applied gaplessly (sound, unlike a Willow-set-reconciliation version — flaw stated in §3.1); no HLC injection anywhere — the design never sets iroh-docs timestamps, so an infeasible-API dependency cannot arise; no check-then-write "CAS" over LWW — all metadata (shard map, witness set, catalog, moves) flows through the single-writer system-shard log; `guardian_conflicts` uses only the per-author latest entries iroh-docs actually retains, with that limitation documented; soft-flagged UNIQUE semantics (the Braided alternative) are not adopted — strict mode enforces immediately, LocalFirst keeps its already-documented LWW behavior with diagnostics only.
8. **Read/write-split (GDB-RWS) failure modes avoided by construction:** advisory-only fencing → replaced by witness-quorum promises + adoption-chain fencing + ack-plane freezing; ORM-hostile scatter-write/cross-shard-read rejections → dissolved by table-group sharding (single-table statements are always single-shard; only multi-shard *write transactions* are rejected).

---

## 12. Test Strategy

**Conformance (per guarantee):**
- **Convergence:** un-ignore tests/sql_replication.rs; rewrite `wait_for_propagation` fixed sleeps (tests/common/mod.rs:173) as `ShardApplied` event waits. Extend to deletes, >2 peers, and DDL.
- **Crash-atomic commit:** kill -9 the primary between log append and row materialization; restart; assert full transaction visible (replay) and replicas never observed a partial state.
- **Fencing:** partition the primary; elect e+1; let the deposed primary keep writing (log + row docs); heal; assert every fenced entry is discarded on all replicas, sweep re-materialized affected keys, `guardian_replication_status` shows no dual-author conflict post-sweep, and bootstrap of a brand-new replica after heal contains zero fenced state (regression for flaw #1).
- **Adoption watermark:** hold a replica behind the failover point with a fenced tail applied (ack tier `local`); deliver `Adopt`; assert deterministic rebuild converges to the valid log (regression for flaw #2).
- **Durability:** at `quorum` tier, ack a commit, immediately kill the primary and its disk; assert the commit survives election. At `replicated(1)`, demonstrate and document the loss window (negative test).
- **Consistent-prefix replicas:** property test — under heavy concurrent multi-row transactions, continuously scan a replica; assert no scan ever observes a proper subset of any transaction's rows.
- **Sync token RYW:** write on gateway A, carry token to replica B, assert read-your-writes or `GDB_TOKEN_WAIT_TIMEOUT`; never a stale success.
- **Freshness:** induce lag > bound; assert `warn` yields 01000 + data, `error` yields 57P03; `strict` reads route or fail, never downgrade.
- **Uniqueness across failover:** concurrent duplicate-PK inserts racing an election; assert exactly one row and one `23505`.
- **SERIALIZABLE (re-scoped):** classic write-skew pair on one shard blocks (2PL) and cannot commit skew; deadlock yields `40P01`.
- **DDL/sequences:** concurrent CREATE TABLE and nextval from two gateways; assert serialized catalog, unique OIDs, duplicate-free (gappy) sequences.
- **LocalFirst byte-identity:** golden-semantics regression — LocalFirst behavior is bit-for-bit today's, diagnostics aside.

**Chaos (deterministic-injection harness + jepsen-style nemesis):** partitions on every edge class (primary↔witnesses, primary↔replicas, gateway↔primary); clock nemesis (frozen, jumped, drifting monotonic clocks — assert data plane indifference, lease behavior within ρ bounds); witness sub-majority + `witness reconfigure` runbook executed by test; crash injection at every table-move step; EventBus/gossip drop storms (assert ≤30s freshness degradation, zero correctness impact); compaction-horizon resync.

**Model checking (Phase 3 gate):** LeaseAuthority state machine explored for: no two holders in one epoch; promise persistence across witness crash; quorum-intersection durability at `quorum` tier; drift-bound violation degrades to fenced overlap, never divergence.

---

## 13. Non-Goals

- **Global snapshots / cross-shard strict serializability.** Cross-shard reads are fractured (named); Phase 6 2PC, if built, is atomicity — not global serializability.
- **MVCC or snapshot isolation.** The engine's per-statement re-read model is unchanged; SERIALIZABLE is 2PL.
- **A distributed LockManager.** Single-writer-per-shard makes local locks sufficient; src/sql/lock.rs is not trait-ified, not networked, not modified.
- **Row-level sharding within a table**, predicate pushdown, or partial-table scans through `RelationalStorage`. Statement cost remains O(referenced table bytes) in RAM on the executing node — stated, not promised away.
- **Multi-writer strict mode**, quorum reads, or consensus on the data path. Exactly one fenced writer per shard per epoch.
- **Forking iroh-docs** (no HLC/timestamp injection; no server-side entry rejection). All ordering and fencing live in the log layer above it.
- **Changing LocalFirst semantics.** It remains AP, LWW, offline-capable, and byte-identical; only diagnostics are added.
- **Automatic witness-set healing** or membership auto-shrink. Reconfiguration is always an explicit, logged, operator-initiated transaction.
- **Byzantine fault tolerance.** Peers holding write capabilities are trusted; the threat model is crashes, partitions, and clock pathology — not malice.