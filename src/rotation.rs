//! Namespace rotation helpers.
//!
//! For iroh-docs-based stores (`KeyValueStore`, `DocumentStore`), the write capability is the
//! namespace secret. Once a node receives a write ticket it holds that secret forever, so
//! removing a key from the `write` role in the access controller only stops *future* grants —
//! it cannot retract a secret a compromised writer already has.
//!
//! The supported way to truly revoke a writer is to **rotate the namespace**: create a fresh
//! namespace (new `NamespaceId`/secret), migrate the current state into it, and redistribute
//! new tickets (write to the writers you still trust, read to the readers). The old namespace
//! is then abandoned.
//!
//! This module provides the mechanical core of that procedure. The full operational runbook
//! lives in `docs/NAMESPACE_ROTATION.md`.

use crate::guardian::error::{GuardianError, Result};
use crate::traits::KeyValueStore;
use tracing::{info, instrument};

/// Copies every key/value pair from `src` into `dst`, returning the number of keys copied.
///
/// This is the state-migration step of a key-value namespace rotation: after creating a fresh
/// namespace (a new writer store, with a new namespace secret), the current state is copied
/// into it. `dst` must be writable; if it is a read-only replica, `put` will fail.
///
/// Note that this reads `src`'s current *local* view via [`KeyValueStore::all`]. For a faithful
/// rotation, run it on a writer node whose replica is fully synced.
#[instrument(skip(src, dst))]
pub async fn copy_key_value_state(
    src: &dyn KeyValueStore<Error = GuardianError>,
    dst: &dyn KeyValueStore<Error = GuardianError>,
) -> Result<usize> {
    let entries = src.all();
    let total = entries.len();
    for (key, value) in entries {
        dst.put(&key, value).await.map_err(|e| {
            GuardianError::Store(format!("Rotation failed copying key '{}': {}", key, e))
        })?;
    }
    info!(
        total,
        "Copied key-value state into the new namespace during rotation"
    );
    Ok(total)
}
