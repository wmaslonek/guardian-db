use crate::data_store::Datastore;
use crate::error::{GuardianError, Result};
use crate::iface::Store;
use crate::stores::base_store::base_store::StoreSnapshot;
use byteorder::{BigEndian, WriteBytesExt};
use cid::Cid;
use std::io::Cursor;

pub type CacheGuard<'a> = std::sync::MutexGuard<'a, Box<dyn Datastore + Send + Sync>>;

/// Equivalente à função `SaveSnapshot` em Go.
///
/// A função é `async` e genérica sobre qualquer tipo `S` que implemente o trait `Store`.
/// Ela constrói o snapshot em um buffer de bytes e o adiciona ao IPFS.
pub async fn save_snapshot<S: Store + Send + Sync>(store: &S) -> Result<Cid> {
    let unfinished_queue = store.replicator().get_queue().await;
    let oplog = store.op_log();

    // Cria e serializa o cabeçalho do snapshot.
    let snapshot_header = StoreSnapshot {
        id: oplog.id().to_string(),
        heads: oplog
            .heads()
            .iter()
            .map(|arc_entry| (**arc_entry).clone())
            .collect(),
        size: oplog.len(),
        store_type: store.store_type().to_string(),
    };
    let header_json = serde_json::to_vec(&snapshot_header)
        .map_err(|e| GuardianError::Store(format!("Unable to serialize snapshot header: {}", e)))?;

    // Inicia a construção do stream de bytes final.
    let mut rs: Vec<u8> = Vec::new();

    // Escreve o tamanho do cabeçalho (2 bytes, big-endian) e o cabeçalho.
    rs.write_u16::<BigEndian>(header_json.len() as u16)?;
    rs.extend_from_slice(&header_json);

    // Itera sobre as entradas do log, serializando cada uma com seu prefixo de tamanho.
    for entry in oplog.values() {
        let entry_json = serde_json::to_vec(&*entry).map_err(|e| {
            GuardianError::Store(format!("Unable to serialize entry as JSON: {}", e))
        })?;
        rs.write_u16::<BigEndian>(entry_json.len() as u16)?;
        rs.extend_from_slice(&entry_json);
    }

    // Adiciona o byte nulo no final, para compatibilidade.
    rs.push(0);

    // Adiciona o arquivo ao IPFS usando o método `add` e convertendo o hash para CID.
    let add_response =
        store.ipfs().add(Cursor::new(rs)).await.map_err(|e| {
            GuardianError::Store(format!("Unable to save log data on store: {}", e))
        })?;

    // Converte o hash para CID
    let snapshot_cid = Cid::try_from(add_response.hash.as_str())
        .map_err(|e| GuardianError::Store(format!("Unable to parse CID from hash: {}", e)))?;

    // Salva o CID do snapshot e a fila pendente no cache.
    let cache = store.cache();
    cache
        .put("snapshot".as_bytes(), snapshot_cid.to_string().as_bytes())
        .await
        .map_err(|e| {
            GuardianError::Store(format!("Unable to add snapshot data to cache: {}", e))
        })?;

    let unfinished_json = serde_json::to_vec(&unfinished_queue)
        .map_err(|e| GuardianError::Store(format!("Unable to marshal unfinished cids: {}", e)))?;
    cache
        .put("queue".as_bytes(), &unfinished_json)
        .await
        .map_err(|e| {
            GuardianError::Store(format!("Unable to add unfinished data to cache: {}", e))
        })?;

    Ok(snapshot_cid)
}
