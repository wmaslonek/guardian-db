use crate::guardian::error::{GuardianError, Result};
use crate::log::identity_provider::Keystore as KeystoreInterface;
use async_trait::async_trait;
use iroh::SecretKey;
use sled::Db;
use std::sync::Arc;

/// Implementação de Keystore que usa sled como backend de persistência
/// e é compatível com a interface do 'log' interno.
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
                .map_err(|e| GuardianError::Other(format!("Erro ao abrir sled: {}", e)))?,
            None => sled::Config::new().temporary(true).open().map_err(|e| {
                GuardianError::Other(format!("Erro ao criar sled temporário: {}", e))
            })?,
        };

        Ok(Self { db })
    }

    /// Cria um keystore temporário em memória para testes
    pub fn temporary() -> Result<Self> {
        Self::new(None)
    }

    /// Armazena uma SecretKey do Iroh como bytes
    pub async fn put_keypair(&self, key: &str, secret_key: &SecretKey) -> Result<()> {
        let encoded = secret_key.to_bytes();
        self.put(key, &encoded).await
    }

    /// Recupera uma SecretKey do Iroh de bytes
    pub async fn get_keypair(&self, key: &str) -> Result<Option<SecretKey>> {
        match self.get(key).await? {
            Some(bytes) => {
                if bytes.len() != 32 {
                    return Err(GuardianError::Other(
                        "Tamanho inválido de chave secreta".to_string(),
                    ));
                }
                let secret_key = SecretKey::try_from(&bytes[..32]).map_err(|e| {
                    GuardianError::Other(format!("Erro ao decodificar secret key: {}", e))
                })?;
                Ok(Some(secret_key))
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

        keys.map_err(|e| GuardianError::Other(format!("Erro ao listar chaves: {}", e)))
    }

    /// Fecha o banco de dados, fazendo flush dos dados pendentes
    pub async fn close(&self) -> Result<()> {
        self.db
            .flush_async()
            .await
            .map_err(|e| GuardianError::Other(format!("Erro ao fechar keystore: {}", e)))?;
        Ok(())
    }
}

#[async_trait]
impl KeystoreInterface for SledKeystore {
    async fn put(&self, key: &str, value: &[u8]) -> Result<()> {
        self.db
            .insert(key, value)
            .map_err(|e| GuardianError::Other(format!("Erro ao inserir no keystore: {}", e)))?;
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        match self
            .db
            .get(key)
            .map_err(|e| GuardianError::Other(format!("Erro ao recuperar do keystore: {}", e)))?
        {
            Some(bytes) => Ok(Some(bytes.to_vec())),
            None => Ok(None),
        }
    }

    async fn has(&self, key: &str) -> Result<bool> {
        Ok(self.db.contains_key(key).map_err(|e| {
            GuardianError::Other(format!("Erro ao verificar chave no keystore: {}", e))
        })?)
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.db
            .remove(key)
            .map_err(|e| GuardianError::Other(format!("Erro ao remover do keystore: {}", e)))?;
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
        use rand_core::OsRng;

        let keystore = SledKeystore::temporary().unwrap();
        let key_name = "test_keypair";

        // Generate a secret key
        let original_secret = SecretKey::generate(OsRng);

        // Store it
        keystore
            .put_keypair(key_name, &original_secret)
            .await
            .unwrap();

        // Retrieve it
        let retrieved_secret = keystore.get_keypair(key_name).await.unwrap().unwrap();

        // Compare public keys
        assert_eq!(original_secret.public(), retrieved_secret.public());
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
