#[cfg(feature = "odm")]
extern crate self as guardian_db;

pub mod access_control;
pub mod address;
pub mod cache;
/// Guardian Compute: delegation of business-logic execution (WASM) between
/// peers, with capability-aware routing. Enabled by the `compute` feature.
/// Currently Phase 0 (public types only) of `docs/rfcs/0002-guardian-compute.md`.
#[cfg(feature = "compute")]
pub mod compute;
pub mod data_store;
pub mod db_manifest;
/// Auto-embedding pipeline for the SQL engine (RFC 0005 phase 2): the
/// pluggable [`embedding::Embedder`] trait, the owner-curated
/// [`embedding::EmbeddingRegistry`], pluggable backends, and the
/// [`embedding::EmbeddingService`] that writes embeddings into `vector`
/// columns off the engine's change-feed. Enabled by the `embedding` feature.
#[cfg(feature = "embedding")]
pub mod embedding;
pub mod events;
pub mod guardian;
pub mod keystore;
pub mod log;
pub mod message_marshaler;
/// High-level P2P messaging: the default [`traits::DirectChannel`] transport
/// between peers (one-on-one channels over the shared gossip layer).
/// Enabled by the `messaging` feature (on by default); without it, callers
/// must supply their own `direct_channel_factory` in the GuardianDB options.
#[cfg(feature = "messaging")]
pub mod messaging;
#[cfg(feature = "odm")]
pub mod odm;
pub mod p2p;
/// PostgreSQL wire-protocol server fronting the [`sql`] engine. Enabled by the
/// `pgwire` feature (which implies `sql`).
#[cfg(feature = "pgwire")]
pub mod pgwire;
/// Storage-agnostic relational core (catalog, types, values, indexes) underlying
/// the PostgreSQL compatibility layer. Enabled by the `sql` feature.
#[cfg(feature = "sql")]
pub mod relational;
pub mod rotation;
/// Administration RPC: a loopback socket server fronting a live [`guardian::GuardianDB`]
/// so tools (e.g. the TUI panel) attach over a socket instead of opening the storage
/// directly, which the redb file lock forbids for a second process. Enabled by the
/// `sentinel` feature. See `docs/ADMIN_RPC_PLAN.md`.
#[cfg(feature = "sentinel")]
pub mod sentinel;
#[cfg(feature = "sql")]
pub mod sql;
pub mod stores;
/// Supabase-compatible HTTP gateway (Kong-shaped REST + Auth) fronting the
/// [`sql`] engine. Enabled by the `supabase` feature (which implies `sql`).
#[cfg(feature = "supabase")]
pub mod supabase;
pub mod traits;

#[cfg(test)]
pub mod tests;
