use crate::error::{GuardianError, Result};
use cid::Cid;
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

/// Cria um novo manifesto de banco de dados e o salva no IPFS.
pub async fn create_db_manifest(
    ipfs: &crate::ipfs_core_api::client::IpfsClient,
    name: &str,
    db_type: &str,
    access_controller_address: &str,
) -> Result<Cid> {
    let access_controller_path = {
        let mut p = PathBuf::from("/ipfs");
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

    // Adiciona os dados ao IPFS
    let response = ipfs
        .add(std::io::Cursor::new(cbor_data))
        .await
        .map_err(|e| GuardianError::Other(format!("Erro ao adicionar manifesto no IPFS: {}", e)))?;

    // Converte o hash retornado para CID
    let cid: Cid = response
        .hash
        .parse()
        .map_err(|e| GuardianError::Other(format!("Erro ao converter hash para CID: {}", e)))?;

    Ok(cid)
}

/// Lê um manifesto de banco de dados do IPFS a partir de um CID
pub async fn read_db_manifest(
    ipfs: &crate::ipfs_core_api::client::IpfsClient,
    manifest_cid: &Cid,
) -> Result<Manifest> {
    // Busca os dados do manifesto no IPFS usando cat
    let mut stream = ipfs.cat(&manifest_cid.to_string()).await.map_err(|e| {
        GuardianError::Other(format!(
            "Não foi possível buscar o manifesto no IPFS: {}",
            e
        ))
    })?;

    // Lê todos os dados do stream
    let mut data = Vec::new();
    use tokio::io::AsyncReadExt;
    stream
        .read_to_end(&mut data)
        .await
        .map_err(|e| GuardianError::Other(format!("Erro ao ler dados do manifesto: {}", e)))?;

    // Desserializa os dados CBOR para a struct Manifest
    let manifest: Manifest = serde_cbor::from_slice(&data).map_err(|e| {
        GuardianError::Other(format!(
            "Não foi possível decodificar o manifesto CBOR: {}",
            e
        ))
    })?;

    Ok(manifest)
}
