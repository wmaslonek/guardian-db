use crate::guardian::error::{GuardianError, Result};
use crate::log::identity_provider::Keystore as KeystoreInterface;
use async_trait::async_trait;
use iroh::SecretKey;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use std::sync::Arc;

const KEYSTORE_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("keystore");

/// Implementação de Keystore que usa redb como backend de persistência
/// e é compatível com a interface do 'log' interno.
#[derive(Debug)]
pub struct RedbKeystore {
    db: Database,
}

// Send + Sync is safe because redb::Database is thread-safe
unsafe impl Send for RedbKeystore {}
unsafe impl Sync for RedbKeystore {}

impl RedbKeystore {
    /// Cria um novo RedbKeystore
    /// Se path for None, cria um banco em memória temporário
    pub fn new(path: Option<std::path::PathBuf>) -> Result<Self> {
        let db = match path {
            Some(p) => {
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        GuardianError::Other(format!("Erro ao criar diretório: {}", e))
                    })?;
                }
                Database::create(&p)
                    .map_err(|e| GuardianError::Other(format!("Erro ao abrir redb: {}", e)))?
            }
            None => Database::builder()
                .create_with_backend(redb::backends::InMemoryBackend::new())
                .map_err(|e| {
                    GuardianError::Other(format!("Erro ao criar redb temporário: {}", e))
                })?,
        };

        // Ensure table exists
        {
            let write_txn = db
                .begin_write()
                .map_err(|e| GuardianError::Other(format!("Erro ao iniciar transação: {}", e)))?;
            {
                let _ = write_txn
                    .open_table(KEYSTORE_TABLE)
                    .map_err(|e| GuardianError::Other(format!("Erro ao criar tabela: {}", e)))?;
            }
            write_txn
                .commit()
                .map_err(|e| GuardianError::Other(format!("Erro ao commitar tabela: {}", e)))?;
        }

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
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| GuardianError::Other(format!("Erro ao iniciar leitura: {}", e)))?;
        let table = read_txn
            .open_table(KEYSTORE_TABLE)
            .map_err(|e| GuardianError::Other(format!("Erro ao abrir tabela: {}", e)))?;

        let mut keys = Vec::new();
        let iter = table
            .iter()
            .map_err(|e| GuardianError::Other(format!("Erro ao iterar: {}", e)))?;

        for entry_result in iter {
            let entry = entry_result
                .map_err(|e| GuardianError::Other(format!("Erro ao listar chaves: {}", e)))?;
            keys.push(entry.0.value().to_string());
        }

        Ok(keys)
    }

    /// Fecha o banco de dados
    pub async fn close(&self) -> Result<()> {
        // Data is already persisted via write transactions in redb.
        Ok(())
    }
}

#[async_trait]
impl KeystoreInterface for Arc<RedbKeystore> {
    async fn put(&self, key: &str, value: &[u8]) -> Result<()> {
        (**self).put(key, value).await
    }
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        (**self).get(key).await
    }
    async fn has(&self, key: &str) -> Result<bool> {
        (**self).has(key).await
    }
    async fn delete(&self, key: &str) -> Result<()> {
        (**self).delete(key).await
    }
}

#[async_trait]
impl KeystoreInterface for RedbKeystore {
    async fn put(&self, key: &str, value: &[u8]) -> Result<()> {
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| GuardianError::Other(format!("Erro ao inserir no keystore: {}", e)))?;
        {
            let mut table = write_txn
                .open_table(KEYSTORE_TABLE)
                .map_err(|e| GuardianError::Other(format!("Erro ao abrir tabela: {}", e)))?;
            table
                .insert(key, value)
                .map_err(|e| GuardianError::Other(format!("Erro ao inserir no keystore: {}", e)))?;
        }
        write_txn
            .commit()
            .map_err(|e| GuardianError::Other(format!("Erro ao commitar inserção: {}", e)))?;
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| GuardianError::Other(format!("Erro ao recuperar do keystore: {}", e)))?;
        let table = read_txn
            .open_table(KEYSTORE_TABLE)
            .map_err(|e| GuardianError::Other(format!("Erro ao abrir tabela: {}", e)))?;
        match table.get(key) {
            Ok(Some(value)) => Ok(Some(value.value().to_vec())),
            Ok(None) => Ok(None),
            Err(e) => Err(GuardianError::Other(format!(
                "Erro ao recuperar do keystore: {}",
                e
            ))),
        }
    }

    async fn has(&self, key: &str) -> Result<bool> {
        let read_txn = self.db.begin_read().map_err(|e| {
            GuardianError::Other(format!("Erro ao verificar chave no keystore: {}", e))
        })?;
        let table = read_txn
            .open_table(KEYSTORE_TABLE)
            .map_err(|e| GuardianError::Other(format!("Erro ao abrir tabela: {}", e)))?;
        match table.get(key) {
            Ok(Some(_)) => Ok(true),
            Ok(None) => Ok(false),
            Err(e) => Err(GuardianError::Other(format!(
                "Erro ao verificar chave no keystore: {}",
                e
            ))),
        }
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| GuardianError::Other(format!("Erro ao remover do keystore: {}", e)))?;
        {
            let mut table = write_txn
                .open_table(KEYSTORE_TABLE)
                .map_err(|e| GuardianError::Other(format!("Erro ao abrir tabela: {}", e)))?;
            table
                .remove(key)
                .map_err(|e| GuardianError::Other(format!("Erro ao remover do keystore: {}", e)))?;
        }
        write_txn
            .commit()
            .map_err(|e| GuardianError::Other(format!("Erro ao commitar remoção: {}", e)))?;
        Ok(())
    }
}

/// Factory function para criar keystores baseados em configuração
pub fn create_keystore(
    directory: Option<std::path::PathBuf>,
) -> Result<Arc<dyn KeystoreInterface + Send + Sync>> {
    let keystore = RedbKeystore::new(directory)?;
    Ok(Arc::new(keystore))
}

/// Cria um keystore temporário em memória
pub fn create_temp_keystore() -> Result<Arc<dyn KeystoreInterface + Send + Sync>> {
    let keystore = RedbKeystore::temporary()?;
    Ok(Arc::new(keystore))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_redb_keystore_basic_operations() {
        let keystore = RedbKeystore::temporary().unwrap();

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

        let keystore = RedbKeystore::temporary().unwrap();
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
        let keystore = RedbKeystore::temporary().unwrap();

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
