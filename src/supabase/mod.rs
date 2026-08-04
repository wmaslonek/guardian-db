//! # Supabase-compatible HTTP gateway
//!
//! A Kong-shaped HTTP surface (`/rest/v1`, `/auth/v1`, ...) in front of the
//! GuardianDB [`sql`](crate::sql) engine, so that Supabase client libraries
//! (`supabase-js`, PostgREST clients, GoTrue clients) can talk to a GuardianDB
//! node with no GuardianDB-specific code. Enabled by the `supabase` feature
//! (which implies `sql`). Default builds are entirely unaffected.
//!
//! Implemented end-to-end: **REST** (PostgREST-compatible), **Auth**
//! (GoTrue-compatible), **Storage** (storage-api-compatible, bytes in a
//! replicated `bytea` table), **postgres-meta** (what Supabase Studio talks
//! to), **Realtime** (Phoenix-protocol websocket), **GraphQL**
//! (pg_graphql-compatible schema reflection over the `public` schema), and
//! **Functions** (Supabase Edge Functions-compatible, backed by the Guardian
//! Compute WASM sandbox when the `compute` feature is on — otherwise the
//! service returns a typed `SUPA_COMPAT_FUNCTION_BOOT_ERROR` on invocation
//! and admin CRUD still works so functions can be deployed for a later
//! runtime).
//!
//! ## Scouted seams (Stage 0)
//!
//! Everything here is built strictly on the engine's public surface; no file
//! under `src/sql/**` or `src/relational/**` is modified.
//!
//! * [`Database<S>`](crate::sql::engine::Database) — the shared, storage-backed
//!   database. Built with `Database::new(Arc<S>, name)` where `S:
//!   RelationalStorage`. Backends: [`MemoryStorage`](crate::relational::MemoryStorage)
//!   (tests / in-memory binary) and
//!   [`GuardianRelationalStorage`](crate::sql::GuardianRelationalStorage) via
//!   [`open_sql`](crate::sql::open_sql) (persistent, Iroh-replicated).
//! * [`Session<S>`](crate::sql::engine::Session) — a connection-scoped session.
//!   `Session::new(Arc<Database<S>>, username)`; the `username` is the role the
//!   statement runs as. We open **one session per HTTP request**, bound to the
//!   request's resolved Postgres role (`anon` / `authenticated` /
//!   `service_role`) — the seam an RLS-enforcement slice will hook into.
//! * SQL execution: `Session::prepare(sql) -> Prepared` then
//!   `Session::execute_one(&Prepared.statement, &[SqlValue])` runs a **single**
//!   parameterised statement (`$1`, `$2`, ...). This is the injection-safe path
//!   we use for REST/Auth data operations. `Session::execute(sql)` runs a
//!   multi-statement string (no params) — used only for the auth-schema
//!   bootstrap DDL.
//! * [`ExecResult`](crate::sql::ExecResult): `Rows { fields: Vec<OutField>,
//!   rows: Vec<Vec<SqlValue>> }` or `Command { tag }`. `OutField` carries the
//!   column `name` and [`SqlType`](crate::relational::SqlType); rows are
//!   rendered to JSON in [`rest`] via [`rest::value_to_json`].
//! * [`SqlValue`](crate::relational::SqlValue) / [`SqlType`]: value model with
//!   `to_text()` / `from_text(text, ty)`. REST coerces filter/body string values
//!   to the *declared column type* (read from the [`Catalog`](crate::relational::Catalog))
//!   via `SqlValue::from_text`, so numeric/temporal comparisons are typed rather
//!   than lexical.
//! * [`RelError`](crate::relational::RelError)`::sqlstate()` — every engine error
//!   carries a PostgreSQL SQLSTATE, mapped to PostgREST/GoTrue error shapes in
//!   [`error`].
//! * Crypto: `bcrypt` (from the pgcrypto work) hashes/verifies passwords;
//!   `hmac` + `sha2` + `base64` (already in-tree for `sql`) implement HS256 JWTs
//!   from scratch in [`jwt`] — no `jsonwebtoken` dependency added.
//! * When the `compute` feature is also on, [`crate::compute::WasmRuntime`]
//!   backs the Supabase Functions runtime: deployed WebAssembly modules run
//!   under the same sandbox as delegated Guardian Compute tasks.
//!
//! ## Architecture
//!
//! ```text
//!   HTTP request
//!     │
//!     ├─ request_id middleware        (x-request-id: read or generate)
//!     │
//!     ├─ apikey middleware            (verify `apikey` JWT against project keys,
//!     │     (rest + auth only)         verify optional `Authorization: Bearer`,
//!     │                                resolve effective Postgres role,
//!     │                                attach AuthContext extension)
//!     │
//!     ├─ /rest/v1/*    → rest.rs     → Session(role) → SQL → PostgREST JSON
//!     ├─ /graphql/v1   → graphql.rs  → Session(role) → SQL → GraphQL JSON
//!     ├─ /auth/v1/*    → auth.rs     → Session(service_role) → auth.* tables
//!     ├─ /storage/v1/* → storage.rs  → Session(role) → storage.* tables (RLS)
//!     ├─ /pg-meta/*    → pg_meta.rs  → catalog + pg_catalog views (service_role)
//!     ├─ /realtime/v1/websocket → realtime.rs → Phoenix ws + change hook
//!     └─ /functions/v1/* → functions.rs → WASM sandbox (invocation)
//!                                          + service_role admin CRUD
//! ```
//!
//! Each request opens a fresh [`Session`] bound to the resolved role. A single
//! [`SupabaseCompatProject`] is served per gateway instance (the "single-project
//! shell").

pub mod auth;
pub mod error;
pub mod functions;
pub mod gateway;
pub mod graphql;
pub mod jwt;
pub mod pg_meta;
pub mod project;
pub mod realtime;
pub mod rest;
pub mod storage;

pub use error::SupaError;
pub use gateway::{AppState, build_router};
pub use jwt::{Claims, JwtError};
pub use project::{ProjectKeys, Secret, ServiceConfig, SupabaseCompatProject};
