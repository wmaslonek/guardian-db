use crate::guardian::error::{GuardianError, Result};
use iroh_blobs::Hash;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Database manifest: describes a store's name, type, and access controller address.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    #[serde(rename = "name")]
    pub name: String,

    #[serde(rename = "type")]
    pub get_type: String,

    #[serde(rename = "access_controller")]
    pub access_controller: String,
}

/// Creates a new database manifest and saves it to Iroh.
pub async fn create_db_manifest(
    client: &crate::p2p::network::client::IrohClient,
    name: &str,
    db_type: &str,
    access_controller_address: &str,
) -> Result<Hash> {
    let access_controller_path = {
        let mut p = PathBuf::from("/iroh");
        // Avoid an address with a leading "/" wiping out the prefix.
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

    // Serialize the manifest to CBOR.
    let cbor_data = serde_cbor::to_vec(&manifest).map_err(|e| {
        GuardianError::Other(format!("Could not write the manifest data to CBOR: {}", e))
    })?;

    // Add the data to Iroh.
    let response = client
        .add_bytes(cbor_data)
        .await
        .map_err(|e| GuardianError::Other(format!("Error adding manifest: {}", e)))?;

    // Convert the hash string (hex) into a Hash.
    let hash_bytes = hex::decode(&response.hash)
        .map_err(|e| GuardianError::Other(format!("Error decoding hash: {}", e)))?;

    if hash_bytes.len() != 32 {
        return Err(GuardianError::Other(format!(
            "Invalid hash: expected 32 bytes, found {}",
            hash_bytes.len()
        )));
    }

    let mut hash_array = [0u8; 32];
    hash_array.copy_from_slice(&hash_bytes);

    Ok(Hash::from_bytes(hash_array))
}

/// Reads a database manifest from Iroh given a Hash.
pub async fn read_db_manifest(
    client: &crate::p2p::network::client::IrohClient,
    manifest_hash: &Hash,
) -> Result<Manifest> {
    // Convert the Hash into a hex string.
    let hash_str = hex::encode(manifest_hash.as_bytes());

    // Fetch the manifest data from Iroh using cat_bytes.
    let data = client
        .cat_bytes(&hash_str)
        .await
        .map_err(|e| GuardianError::Other(format!("Could not fetch the manifest: {}", e)))?;

    // Deserialize the CBOR data into the Manifest struct.
    let manifest: Manifest = serde_cbor::from_slice(&data)
        .map_err(|e| GuardianError::Other(format!("Could not decode the CBOR manifest: {}", e)))?;

    Ok(manifest)
}
