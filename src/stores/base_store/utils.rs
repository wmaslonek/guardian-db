use crate::data_store::Datastore;
use crate::guardian::error::{GuardianError, Result};
use crate::stores::base_store::StoreSnapshot;
use crate::traits::Store;
use byteorder::{BigEndian, WriteBytesExt};
use iroh_blobs::Hash;

pub type CacheGuard<'a> = std::sync::MutexGuard<'a, Box<dyn Datastore + Send + Sync>>;

/// The function is `async` and generic over any type `S` that implements the `Store` trait.
/// It builds the snapshot into a byte buffer and adds it to iroh.
pub async fn save_snapshot<S: Store + Send + Sync>(store: &S) -> Result<Hash> {
    let unfinished_queue: Vec<Hash> = Vec::new();
    let oplog = store.op_log();

    // Create and serialize the snapshot header.
    // Since oplog is Arc<RwLock<Log>>, we need to access its methods through the lock.
    let snapshot_header = {
        let oplog_guard = oplog.read();
        StoreSnapshot {
            id: oplog_guard.id().to_string(),
            heads: oplog_guard
                .heads()
                .iter()
                .map(|arc_entry| (**arc_entry).clone())
                .collect(),
            size: oplog_guard.len(),
            store_type: store.store_type().to_string(),
        }
    };
    let header_bytes = crate::guardian::serializer::serialize(&snapshot_header)
        .map_err(|e| GuardianError::Store(format!("Unable to serialize snapshot header: {}", e)))?;

    // Start building the final byte stream.
    let mut rs: Vec<u8> = Vec::new();

    // Write the header size (2 bytes, big-endian) and the header.
    rs.write_u16::<BigEndian>(header_bytes.len() as u16)?;
    rs.extend_from_slice(&header_bytes);

    // Iterate over the log entries, serializing each one with its size prefix.
    // Since oplog is Arc<RwLock<Log>>, we access it via the lock.
    let entries = {
        let oplog_guard = oplog.read();
        oplog_guard.values()
    };
    for entry in entries {
        let entry_bytes = crate::guardian::serializer::serialize(&*entry)
            .map_err(|e| GuardianError::Store(format!("Unable to serialize entry: {}", e)))?;
        rs.write_u16::<BigEndian>(entry_bytes.len() as u16)?;
        rs.extend_from_slice(&entry_bytes);
    }

    // Add the null byte at the end, for compatibility.
    rs.push(0);

    // Add the file to Iroh using the `add_bytes` method.
    let add_response =
        store.client().add_bytes(rs).await.map_err(|e| {
            GuardianError::Store(format!("Unable to save log data on store: {}", e))
        })?;

    // Convert the hash string into a Hash.
    let hash_bytes = hex::decode(&add_response.hash)
        .map_err(|e| GuardianError::Store(format!("Failed to decode hash hex: {}", e)))?;
    if hash_bytes.len() != 32 {
        return Err(GuardianError::Store("Invalid hash length".to_string()));
    }
    let mut hash_array = [0u8; 32];
    hash_array.copy_from_slice(&hash_bytes);
    let snapshot_hash = Hash::from(hash_array);

    // Save the snapshot Hash and the pending queue to the cache.
    let cache = store.cache();
    cache
        .put(
            "snapshot".as_bytes(),
            hex::encode(snapshot_hash.as_bytes()).as_bytes(),
        )
        .await
        .map_err(|e| {
            GuardianError::Store(format!("Unable to add snapshot data to cache: {}", e))
        })?;

    let unfinished_bytes = crate::guardian::serializer::serialize(&unfinished_queue)
        .map_err(|e| GuardianError::Store(format!("Unable to marshal unfinished hashes: {}", e)))?;
    cache
        .put("queue".as_bytes(), &unfinished_bytes)
        .await
        .map_err(|e| {
            GuardianError::Store(format!("Unable to add unfinished data to cache: {}", e))
        })?;

    Ok(snapshot_hash)
}
