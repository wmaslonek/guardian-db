use crate::data_store::Datastore;
use crate::guardian::error::{GuardianError, Result};
use crate::stores::base_store::StoreSnapshot;
use crate::traits::Store;
use byteorder::{BigEndian, WriteBytesExt};
use iroh_blobs::Hash;

pub type CacheGuard<'a> = std::sync::MutexGuard<'a, Box<dyn Datastore + Send + Sync>>;

/// A função é `async` e genérica sobre qualquer tipo `S` que implemente o trait `Store`.
/// Ela constrói o snapshot em um buffer de bytes e o adiciona ao iroh.
pub async fn save_snapshot<S: Store + Send + Sync>(store: &S) -> Result<Hash> {
    let unfinished_queue: Vec<Hash> = Vec::new();
    let oplog = store.op_log();

    // Cria e serializa o cabeçalho do snapshot.
    // Como oplog é Arc<RwLock<Log>>, precisamos acessar seus métodos através do lock
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

    // Inicia a construção do stream de bytes final.
    let mut rs: Vec<u8> = Vec::new();

    // Escreve o tamanho do cabeçalho (2 bytes, big-endian) e o cabeçalho.
    rs.write_u16::<BigEndian>(header_bytes.len() as u16)?;
    rs.extend_from_slice(&header_bytes);

    // Itera sobre as entradas do log, serializando cada uma com seu prefixo de tamanho.
    // Como oplog é Arc<RwLock<Log>>, acessamos via lock
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

    // Adiciona o byte nulo no final, para compatibilidade.
    rs.push(0);

    // Adiciona o arquivo ao Iroh usando o método `add_bytes`.
    let add_response =
        store.client().add_bytes(rs).await.map_err(|e| {
            GuardianError::Store(format!("Unable to save log data on store: {}", e))
        })?;

    // Converte o hash string para Hash
    let hash_bytes = hex::decode(&add_response.hash)
        .map_err(|e| GuardianError::Store(format!("Failed to decode hash hex: {}", e)))?;
    if hash_bytes.len() != 32 {
        return Err(GuardianError::Store("Invalid hash length".to_string()));
    }
    let mut hash_array = [0u8; 32];
    hash_array.copy_from_slice(&hash_bytes);
    let snapshot_hash = Hash::from(hash_array);

    // Salva o Hash do snapshot e a fila pendente no cache.
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
