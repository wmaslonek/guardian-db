#[cfg(feature = "odm")]
extern crate self as guardian_db;

pub mod access_control;
pub mod address;
pub mod cache;
pub mod data_store;
pub mod db_manifest;
pub mod events;
pub mod guardian;
pub mod keystore;
pub mod log;
pub mod message_marshaler;
#[cfg(feature = "odm")]
pub mod odm;
pub mod p2p;
pub mod reactive_synchronizer;
/// Storage-agnostic relational core (catalog, types, values, indexes) underlying
/// the PostgreSQL compatibility layer. Enabled by the `sql` feature.
#[cfg(feature = "sql")]
pub mod relational;
#[cfg(feature = "sql")]
pub mod sql;
/// PostgreSQL wire-protocol server fronting the [`sql`] engine. Enabled by the
/// `pgwire` feature (which implies `sql`).
#[cfg(feature = "pgwire")]
pub mod pgwire;
pub mod stores;
pub mod traits;

#[cfg(test)]
pub mod tests;
