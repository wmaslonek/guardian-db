use crate::error::{GuardianError, Result};
use async_trait::async_trait;
use libp2p_identity::Keypair;
use sled::Db;
use std::sync::Arc;

use crate::ipfs_log::identity_provider::Keystore as KeystoreInterface;

/// Implementação de Keystore que usa sled como backend de persistência
/// e é compatível com a interface do ipfs_log
#[derive(Clone, Debug)]
pub struct SledKeystore {
    db: Db,
}

impl SledKeystore {
    /// Cria um novo SledKeystore
    /// Se path for None, cria um banco em memória temporário
    pub fn new(path: Option<std::path::PathBuf>) -> Result<Self> {
        let db = match path {
            Some(p) => sled::open(p)
                .map_err(|e| GuardianError::Other(format!("Erro ao abrir sled: {}", e).into()))?,
            None => sled::Config::new().temporary(true).open().map_err(|e| {
                GuardianError::Other(format!("Erro ao criar sled temporário: {}", e).into())
            })?,
        };

        Ok(Self { db })
    }

    /// Cria um keystore temporário em memória para testes
    pub fn temporary() -> Result<Self> {
        Self::new(None)
    }

    /// Armazena um Keypair libp2p com codificação protobuf
    pub async fn put_keypair(&self, key: &str, keypair: &Keypair) -> Result<()> {
        let encoded = keypair.to_protobuf_encoding().map_err(|e| {
            GuardianError::Other(format!("Erro ao codificar keypair: {}", e).into())
        })?;
        self.put(key, &encoded).await
    }

    /// Recupera um Keypair libp2p decodificando de protobuf
    pub async fn get_keypair(&self, key: &str) -> Result<Option<Keypair>> {
        match self.get(key).await? {
            Some(bytes) => {
                let keypair = Keypair::from_protobuf_encoding(&bytes).map_err(|e| {
                    GuardianError::Other(format!("Erro ao decodificar keypair: {}", e).into())
                })?;
                Ok(Some(keypair))
            }
            None => Ok(None),
        }
    }

    /// Lista todas as chaves armazenadas
    pub async fn list_keys(&self) -> Result<Vec<String>> {
        let keys: std::result::Result<Vec<String>, sled::Error> = self
            .db
            .iter()
            .keys()
            .map(|k| k.map(|key| String::from_utf8_lossy(&key).to_string()))
            .collect();

        keys.map_err(|e| GuardianError::Other(format!("Erro ao listar chaves: {}", e).into()))
    }

    /// Fecha o banco de dados, fazendo flush dos dados pendentes
    pub async fn close(&self) -> Result<()> {
        self.db
            .flush_async()
            .await
            .map_err(|e| GuardianError::Other(format!("Erro ao fechar keystore: {}", e).into()))?;
        Ok(())
    }
}

#[async_trait]
impl KeystoreInterface for SledKeystore {
    async fn put(&self, key: &str, value: &[u8]) -> Result<()> {
        self.db.insert(key, value).map_err(|e| {
            GuardianError::Other(format!("Erro ao inserir no keystore: {}", e).into())
        })?;
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        match self.db.get(key).map_err(|e| {
            GuardianError::Other(format!("Erro ao recuperar do keystore: {}", e).into())
        })? {
            Some(bytes) => Ok(Some(bytes.to_vec())),
            None => Ok(None),
        }
    }

    async fn has(&self, key: &str) -> Result<bool> {
        Ok(self.db.contains_key(key).map_err(|e| {
            GuardianError::Other(format!("Erro ao verificar chave no keystore: {}", e).into())
        })?)
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.db.remove(key).map_err(|e| {
            GuardianError::Other(format!("Erro ao remover do keystore: {}", e).into())
        })?;
        Ok(())
    }
}

/// Factory function para criar keystores baseados em configuração
pub fn create_keystore(
    directory: Option<std::path::PathBuf>,
) -> Result<Arc<dyn KeystoreInterface + Send + Sync>> {
    let keystore = SledKeystore::new(directory)?;
    Ok(Arc::new(keystore))
}

/// Cria um keystore temporário em memória
pub fn create_temp_keystore() -> Result<Arc<dyn KeystoreInterface + Send + Sync>> {
    let keystore = SledKeystore::temporary()?;
    Ok(Arc::new(keystore))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_sled_keystore_basic_operations() {
        let keystore = SledKeystore::temporary().unwrap();

        // Test put/get/has
        let key = "test_key";
        let value = b"test_value";

        assert!(!keystore.has(key).await.unwrap());

        keystore.put(key, value).await.unwrap();
        assert!(keystore.has(key).await.unwrap());

        let retrieved = keystore.get(key).await.unwrap().unwrap();
        assert_eq!(retrieved, value);

        // Test delete
        keystore.delete(key).await.unwrap();
        assert!(!keystore.has(key).await.unwrap());
    }

    #[tokio::test]
    async fn test_keypair_storage() {
        let keystore = SledKeystore::temporary().unwrap();
        let key_name = "test_keypair";

        // Generate a keypair
        let original_keypair = Keypair::generate_ed25519();

        // Store it
        keystore
            .put_keypair(key_name, &original_keypair)
            .await
            .unwrap();

        // Retrieve it
        let retrieved_keypair = keystore.get_keypair(key_name).await.unwrap().unwrap();

        // Compare public keys (private keys can't be compared directly)
        assert_eq!(
            original_keypair.public().encode_protobuf(),
            retrieved_keypair.public().encode_protobuf()
        );
    }

    #[tokio::test]
    async fn test_list_keys() {
        let keystore = SledKeystore::temporary().unwrap();

        // Add some keys
        keystore.put("key1", b"value1").await.unwrap();
        keystore.put("key2", b"value2").await.unwrap();
        keystore.put("key3", b"value3").await.unwrap();

        // List keys
        let mut keys = keystore.list_keys().await.unwrap();
        keys.sort();

        assert_eq!(keys, vec!["key1", "key2", "key3"]);
    }
}
