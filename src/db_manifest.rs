use crate::guardian::error::{GuardianError, Result};
use iroh_blobs::Hash;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    #[serde(rename = "name")]
    pub name: String,

    #[serde(rename = "type")]
    pub get_type: String,

    #[serde(rename = "access_controller")]
    pub access_controller: String,
}

/// Cria um novo manifesto de banco de dados e o salva no Iroh.
pub async fn create_db_manifest(
    client: &crate::p2p::network::client::IrohClient,
    name: &str,
    db_type: &str,
    access_controller_address: &str,
) -> Result<Hash> {
    let access_controller_path = {
        let mut p = PathBuf::from("/iroh");
        // evita que um endereço com "/" inicial apague o prefixo
        p.push(access_controller_address.trim_start_matches('/'));
        p
    };

    let manifest = Manifest {
        name: name.to_string(),
        get_type: db_type.to_string(),
        access_controller: access_controller_path
            .as_path()
            .to_string_lossy()
            .into_owned(),
    };

    // Serializa o manifesto para CBOR
    let cbor_data = serde_cbor::to_vec(&manifest).map_err(|e| {
        GuardianError::Other(format!(
            "Não foi possível escrever os dados do manifesto em CBOR: {}",
            e
        ))
    })?;

    // Adiciona os dados ao Iroh
    let response = client
        .add_bytes(cbor_data)
        .await
        .map_err(|e| GuardianError::Other(format!("Erro ao adicionar manifesto: {}", e)))?;

    // Converte o hash string (hex) para Hash
    let hash_bytes = hex::decode(&response.hash)
        .map_err(|e| GuardianError::Other(format!("Erro ao decodificar hash: {}", e)))?;

    if hash_bytes.len() != 32 {
        return Err(GuardianError::Other(format!(
            "Hash inválido: esperado 32 bytes, encontrado {}",
            hash_bytes.len()
        )));
    }

    let mut hash_array = [0u8; 32];
    hash_array.copy_from_slice(&hash_bytes);

    Ok(Hash::from_bytes(hash_array))
}

/// Lê um manifesto de banco de dados do Iroh a partir de um Hash
pub async fn read_db_manifest(
    client: &crate::p2p::network::client::IrohClient,
    manifest_hash: &Hash,
) -> Result<Manifest> {
    // Converte Hash para string hex
    let hash_str = hex::encode(manifest_hash.as_bytes());

    // Busca os dados do manifesto no Iroh usando cat_bytes
    let data = client
        .cat_bytes(&hash_str)
        .await
        .map_err(|e| GuardianError::Other(format!("Não foi possível buscar o manifesto: {}", e)))?;

    // Desserializa os dados CBOR para a struct Manifest
    let manifest: Manifest = serde_cbor::from_slice(&data).map_err(|e| {
        GuardianError::Other(format!(
            "Não foi possível decodificar o manifesto CBOR: {}",
            e
        ))
    })?;

    Ok(manifest)
}
