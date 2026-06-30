# PostgreSQL compatibility

GuardianDB ships a **PostgreSQL-compatible relational layer** on top of its
local-first, P2P document model. Existing PostgreSQL clients — `psql`,
node-postgres, TypeORM, DBeaver — connect to GuardianDB over the standard
PostgreSQL wire protocol and run ordinary SQL, with no GuardianDB-specific
client code.

```
TypeORM / node-postgres / psql / DBeaver
        │  PostgreSQL wire protocol (v3)
        ▼
guardian-pgwire   ── startup/auth, simple + extended query, prepared statements
        │
        ▼
guardian-sql      ── parser (sqlparser) → planner → executor; DDL/DML;
        │             transactions; information_schema / pg_catalog
        ▼
guardian-relational ── types, values, catalog, indexes, RelationalStorage
        │
        ▼
GuardianDB document / key-value storage  ──►  Iroh-backed replication
```

The relational core (`guardian-relational`, `guardian-sql`, `guardian-pgwire`)
is independent of the iroh stack and is driven entirely through the
`RelationalStorage` trait. The default gateway uses an in-memory store; the
GuardianDB-backed store maps rows onto replicated `iroh-docs` documents while
preserving the local-first model.

---

## 1. Starting the gateway

```bash
cargo run -p guardian-pgwire        # listens on 127.0.0.1:15432, database "app"

# options:
cargo run -p guardian-pgwire -- --addr 127.0.0.1:15432 --database app --username guardian
```

| Flag             | Default            | Notes                                            |
| ---------------- | ------------------ | ------------------------------------------------ |
| `--addr`         | `127.0.0.1:15432`  | Bind address                                     |
| `--database`     | `app`              | Logical database name                            |
| `--username`     | `guardian`         | Reported role                                    |
| `--path`         | —                  | Data directory (GuardianDB-backed gateway)       |
| `--consistency`  | `local`            | `local` or `strict` (GuardianDB-backed gateway)  |
| `--peer`         | —                  | Iroh peer to replicate with (repeatable)         |

Authentication is **trust** by default (any username/password connects); the
configured username is reported to clients. The server answers SSL negotiation
requests with "SSL not supported" and continues in cleartext.

## 2. Connecting with `psql`

```bash
psql 'postgres://guardian:guardian@127.0.0.1:15432/app'
```

```sql
CREATE TABLE users (id SERIAL PRIMARY KEY, email TEXT UNIQUE NOT NULL, data JSONB);
INSERT INTO users (email, data) VALUES ('a@x.com', '{"plan":"pro"}') RETURNING id;
SELECT count(*) FROM users;
```

## 3. Connecting with TypeORM (`type: "postgres"`)

This is the required, primary path. No custom code:

```ts
import { DataSource } from "typeorm";

const ds = new DataSource({
  type: "postgres",
  host: "127.0.0.1",
  port: 15432,
  username: "guardian",
  password: "guardian",
  database: "app",
  synchronize: true,         // or run migrations
  entities: [User, Post, Org],
});
await ds.initialize();
```

`synchronize`, schema **re-introspection**, migrations (`QueryRunner`),
repositories, `EntityManager`, QueryBuilder, transactions and relation metadata
all work. See `examples/postgres-typeorm` for a complete app and
`tests/postgres-compat` for the conformance suite.

## 4. Native GuardianDB TypeORM driver (optional)

`@guardiandb/typeorm` provides `GuardianDataSource`, which manages an embedded
gateway and otherwise behaves like a normal `DataSource`:

```ts
import { GuardianDataSource } from "@guardiandb/typeorm";

const ds = new GuardianDataSource({
  path: "./data",
  database: "app",
  peers: [],
  consistency: "strict",
  entities: [User, Post, Org],
});
await ds.initialize();
```

---

## 5. Supported SQL

### DDL
- `CREATE SCHEMA`, `DROP SCHEMA [CASCADE]`
- `CREATE TABLE` / `CREATE TABLE IF NOT EXISTS`
- `ALTER TABLE ADD COLUMN`, `DROP COLUMN`, `RENAME COLUMN`
- `ALTER TABLE ALTER COLUMN SET/DROP DEFAULT`, `SET/DROP NOT NULL`, `SET DATA TYPE`
- `ALTER TABLE ADD/DROP CONSTRAINT`, `RENAME TO`
- `DROP TABLE` / `DROP TABLE IF EXISTS`, `TRUNCATE`
- `CREATE INDEX`, `CREATE UNIQUE INDEX`, `DROP INDEX`
- `CREATE VIEW`, `DROP VIEW`

### DML
- `INSERT`, multi-row `INSERT`, `INSERT ... RETURNING`, `DEFAULT VALUES`
- `UPDATE`, `UPDATE ... RETURNING`
- `DELETE`, `DELETE ... RETURNING`
- `INSERT ... ON CONFLICT DO NOTHING` / `DO UPDATE` (with `excluded`)

### SELECT
- projection, aliases, `WHERE`, `AND`/`OR`/`NOT`
- `IS [NOT] NULL`, `IS [NOT] DISTINCT FROM`, `IN (list)`, `IN (subquery)`,
  `BETWEEN`, `LIKE`/`ILIKE` (with `ESCAPE`)
- `ORDER BY` (ASC/DESC, NULLS FIRST/LAST, by output alias or position),
  `LIMIT`/`OFFSET`, `DISTINCT`
- `GROUP BY`, `HAVING`, aggregates `COUNT`/`SUM`/`AVG`/`MIN`/`MAX`
  (incl. `COUNT(DISTINCT ...)`), `bool_and`/`bool_or`, `string_agg`, `array_agg`
- `INNER`/`LEFT`/`RIGHT`/`FULL`/`CROSS JOIN`, `USING`, `NATURAL`
- scalar / `IN` / `EXISTS` subqueries (correlated supported)
- `UNION`/`UNION ALL`/`INTERSECT`/`EXCEPT`
- non-recursive `WITH` (CTEs)
- parameterized queries (`$1`, ...)
- `x = ANY(array)` / `ALL(array)`, `UNNEST(array)` in projections

### Expressions & functions
- arithmetic (`+ - * / %`), integer vs numeric vs float semantics,
  division-by-zero errors
- boolean three-valued logic, string comparison, NULL semantics
- `CAST` / `::type`, including `::regclass` → OID resolution
- `CASE`, `COALESCE`, `NULLIF`, `GREATEST`, `LEAST`
- string: `upper`, `lower`, `length`, `trim`/`btrim`/`ltrim`/`rtrim`,
  `substring`, `position`, `replace`, `concat`, `concat_ws`, `left`, `right`
- numeric: `abs`, `ceil`, `floor`, `round`, `trunc`, `sign`, `sqrt`, `power`,
  `mod`
- temporal: `now()`, `current_timestamp`, `current_date`, `current_time`,
  `EXTRACT`
- identity: `gen_random_uuid()`, `uuid_generate_v4()`
- session: `current_schema()`, `current_database()`, `current_user`, `version()`

### Types
`boolean`, `smallint`/`int2`, `integer`/`int4`, `bigint`/`int8`,
`serial`/`bigserial`/`smallserial`, `real`/`float4`, `double precision`/`float8`,
`numeric`/`decimal`, `text`, `varchar(n)`, `char(n)`, `bytea`, `uuid`, `date`,
`time`, `timestamp`, `timestamptz`, `json`, `jsonb`, and one-dimensional arrays.

`numeric` preserves exact precision (decimal-backed). `bigint` is stored exactly;
values beyond JS `Number.MAX_SAFE_INTEGER` are transmitted as text (node-postgres
exposes them as strings — use `pg.types` parsers if you need `BigInt`).

### Constraints & indexes
- `PRIMARY KEY`, `UNIQUE`, `NOT NULL`, `DEFAULT`, `CHECK`
- `FOREIGN KEY` with `ON DELETE`/`ON UPDATE` actions (metadata + parsing;
  **local enforcement of cascade/restrict is a documented gap**, see below)
- real BTree indexes: primary-key, unique, secondary, composite; maintained on
  insert/update/delete; used for equality index scans (the planner chooses an
  index scan when a single base table is filtered by `col = const` on an indexed
  column, and falls back to a full scan otherwise — results are identical).

### Catalog / introspection
Queryable `information_schema` (`tables`, `columns`, `schemata`,
`table_constraints`, `key_column_usage`, `constraint_column_usage`,
`referential_constraints`, `views`) and `pg_catalog` (`pg_class`,
`pg_attribute`, `pg_type`, `pg_namespace`, `pg_index`, `pg_constraint`,
`pg_database`, `pg_indexes`, `pg_attrdef`, `pg_am`, `pg_roles`, and empty
`pg_description`/`pg_enum`/`pg_collation`/`pg_settings`). This is enough for
TypeORM schema sync, migrations and QueryRunner inspection, and for
node-postgres metadata.

---

## 6. Unsupported SQL (documented gaps)

Each gap has a conformance test in `crates/guardian-sql/tests/conformance.rs`
(clean-failure tests pass; intended-future features are `#[ignore]`d).

| Feature                              | Status | Behaviour                              |
| ------------------------------------ | ------ | -------------------------------------- |
| Window functions (`OVER`)            | ✗      | error `0A000`                          |
| `WITH RECURSIVE`                     | ✗      | ignored test (non-recursive CTEs only) |
| Set-returning funcs in `FROM`        | ✗      | error `0A000` (scalar table funcs ok)  |
| `WITH` inside a subquery             | ✗      | error `0A000` (top-level `WITH` ok)    |
| `COPY` (bulk load)                   | ✗      | error `0A000` (no CopyIn/Out framing)  |
| Materialized views                   | ✗      | error `0A000`                          |
| `CREATE FUNCTION` / procedures / triggers | ✗ | error `0A000`                          |
| Full-text search (`tsvector`/`@@`)   | ✗      | error                                  |
| Generated/computed columns           | ✗      | ignored test                           |
| `SAVEPOINT` partial rollback         | partial| `SAVEPOINT`/`RELEASE` no-op; `ROLLBACK TO` collapses to full rollback |
| FK cascade/restrict enforcement      | partial| constraints parsed + introspectable; referential actions not enforced |
| `SERIALIZABLE` isolation             | ✗      | ignored test (read-committed only)     |
| SSL/TLS transport                    | ✗      | negotiated-away, cleartext             |
| Binary result encoding               | ✗      | results sent as text (node-postgres/psql use text) |
| `LISTEN`/`NOTIFY` pub/sub            | no-op  | accepted, no delivery                  |

---

## 7. Consistency modes

GuardianDB is local-first; SQL does not change that. Two modes are defined.

### Local-first mode (default)
- A statement (and an explicit `BEGIN ... COMMIT`) is **atomic on the local
  replica**: it loads the touched tables, validates constraints and uniqueness,
  applies all writes in one batch, and only then flushes to storage. A failure
  flushes nothing.
- Replication remains **asynchronous**; peers converge under GuardianDB's
  CRDT/`iroh-docs` rules (last-writer-wins per key).
- This is **PostgreSQL-compatible API behaviour**, not globally serializable
  PostgreSQL storage behaviour. Two disconnected replicas can both insert the
  same primary key; on sync the documents converge by LWW and the relational
  layer surfaces the survivor. Cross-replica uniqueness is therefore *eventual*,
  not immediate.

### Strict SQL mode (`consistency: "strict"`)
- Intended to add stronger coordination where PostgreSQL semantics require it,
  via a **single-writer leader per database** (a transaction coordinator over
  GuardianDB/Iroh primitives). Writes route to the leader, giving a global
  serial order and immediate cross-replica uniqueness.
- Status: the API surface and routing flag exist; the leader/coordinator is a
  documented in-progress component. `SERIALIZABLE` isolation has an `#[ignore]`
  conformance test describing the target (one transaction aborts with `40001`
  on write-skew).

## 8. Transaction semantics

- `BEGIN` / `COMMIT` / `ROLLBACK` are supported. Within a transaction, writes
  buffer in an overlay; reads merge the overlay over storage; `COMMIT` flushes
  atomically; `ROLLBACK` discards.
- Isolation: **read committed** within a connection (a transaction sees its own
  uncommitted writes; other connections see committed state on their next
  statement). Autocommit wraps each statement in its own transaction.
- Constraint checks (NOT NULL, unique, CHECK) run before the write is staged, so
  a violating statement aborts without partial effects.

## 9. Replication semantics

- Each table maps to a GuardianDB document collection; each row is a JSON
  document with a stable id (`__gdb_sql_rows_<oid>`), carrying internal fields
  `_id`, `__schema`, `__table`, `__version`, `__deleted`.
- The catalog is a single replicated document (`__gdb_sql_catalog`); schema
  changes (DDL) replicate like data.
- Convergence follows GuardianDB/`iroh-docs` semantics (range-based
  reconciliation, LWW per key). The relational layer reads a synchronous,
  locally-mirrored view (exactly like the existing DocumentStore index) and
  re-derives indexes from the live rows on each statement.
- The local view updates on local writes and on `load`/`sync`, not automatically
  when documents arrive from peers. `GuardianRelationalStorage::refresh()`
  re-syncs the index from replicated state; a gateway serving a replicating node
  should call it periodically or before reads to observe remote writes.
- Single-node SQL over the GuardianDB document store (including persistence
  across reopening the backend) is verified by `guardian_db::sql` tests. Making
  the *relational* view converge **across peers** additionally needs the two
  `open_sql` stores to share an iroh-docs namespace plus a background refresh;
  this is the in-progress distributed-coordination work, captured by the
  `#[ignore]`d `tests/sql_replication.rs` conformance target (raw document
  replication between peers already works — see `tests/integration_replication.rs`).

## 10. Index behaviour

- Indexes are real ordered (BTree) structures built from live rows and
  maintained incrementally within a statement/transaction.
- Unique indexes enforce uniqueness on the local replica (NULLs are distinct,
  matching PostgreSQL).
- The planner performs an **index scan** for `col = const` on a single
  single-column-indexed base table, otherwise a sequential scan. A conformance
  test asserts indexed lookups return the same rows as a full scan.
- `REINDEX` is implicit: indexes are rebuilt from storage on load.

## 11. Error codes

Errors carry standard PostgreSQL SQLSTATE codes, surfaced to clients in the
`code` field:

| SQLSTATE | Meaning                         |
| -------- | ------------------------------- |
| `42P01`  | undefined table                 |
| `42703`  | undefined column                |
| `42P07`  | duplicate table/index           |
| `42601`  | syntax error                    |
| `23505`  | unique violation                |
| `23502`  | not-null violation              |
| `23503`  | foreign-key violation           |
| `23514`  | check violation                 |
| `22P02`  | invalid text representation     |
| `22003`  | numeric value out of range      |
| `22012`  | division by zero                |
| `42804`  | datatype mismatch               |
| `3F000`  | undefined schema                |
| `0A000`  | feature not supported           |

## 12. Examples

- `examples/postgres-typeorm` — a complete TypeORM app (entities, migration,
  seed, queries, transactions). Run `npm run demo`.
- `tests/postgres-compat` — node-postgres and TypeORM conformance tests.
- `crates/guardian-pgwire/tests/wire.rs` — a `tokio-postgres` client driving the
  gateway over TCP.

## 13. Testing summary

| Layer                | Tests                                                  |
| -------------------- | ------------------------------------------------------ |
| `guardian-relational`| types, values, encoding, catalog, indexes, storage     |
| `guardian-sql`       | engine (DDL/DML/SELECT/joins/aggregates/txn/index) + conformance gaps |
| `guardian-pgwire`    | `tokio-postgres` over TCP (startup, query, errors, txn)|
| `tests/postgres-compat` | node-postgres + TypeORM (synchronize, migrations, relations, QueryBuilder, transactions) |
