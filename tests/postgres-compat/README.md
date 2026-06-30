# PostgreSQL compatibility conformance tests

These tests exercise GuardianDB's PostgreSQL gateway with **real** clients —
the standard `pg` (node-postgres) driver and **TypeORM** with
`type: "postgres"` — over an actual TCP socket. There is no custom client code:
if these pass, ordinary PostgreSQL tooling works against GuardianDB.

## What is covered

`node-postgres.test.ts`
- startup / authentication, simple query protocol, command tags
- extended (parameterized) queries and named prepared-statement reuse
- type round-trips: `int`, `text`, `boolean`, `numeric`, `jsonb`, `timestamptz`, `uuid`
- `RETURNING`, `INSERT ... ON CONFLICT DO UPDATE`
- SQLSTATE error codes (`42P01`, `23505`, `23502`)
- transactions (BEGIN/COMMIT/ROLLBACK), concurrent connections
- joins, aggregates, GROUP BY / HAVING

`typeorm.test.ts`
- `DataSource.initialize()` + `synchronize` (schema creation)
- schema **re-introspection** (synchronize against an existing schema)
- Repository CRUD, generated ids, JSONB and timestamp columns
- unique-constraint enforcement
- relations (`OneToMany` / `ManyToOne`), eager `relations` loading
- QueryBuilder joins / where / order
- `EntityManager` transactions
- **migrations** (`runMigrations`, idempotent re-runs) via `QueryRunner`

## Running

```bash
# from the repo root, build the gateway binary the tests spawn:
cargo build --features pgwire --bin guardian-pgwire

cd tests/postgres-compat
npm install
npm test
```

Each test file spawns its own fresh in-memory gateway on a free port (override
the binary with `GUARDIAN_PGWIRE_BIN=/path/to/guardian-pgwire`).

> Documented gaps are tracked in `docs/postgres-compat.md` and as ignored
> conformance tests in `tests/sql_conformance.rs`.
