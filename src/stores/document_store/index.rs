use crate::traits::{CreateDocumentDBOptions, StoreIndex};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[allow(dead_code)]
type Result<T> = std::result::Result<T, crate::guardian::error::GuardianError>;

/// DocumentIndex mantém um índice de chave-valor em memória para a DocumentStore.
///
/// Trabalha diretamente com iroh-docs Entry.
#[derive(Clone)]
pub struct DocumentIndex {
    // O índice principal, protegido por um RwLock para acesso concorrente seguro.
    // Mapeia: key (String) -> hash_do_blob (Vec<u8>)
    // Arc permite compartilhar o mesmo índice entre múltiplas instâncias
    index: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    // Opções de configuração da store, compartilhadas via Arc.
    #[allow(dead_code)]
    opts: Arc<CreateDocumentDBOptions>,
}

impl DocumentIndex {
    /// Cria uma nova instância de DocumentIndex.
    pub fn new(opts: Arc<CreateDocumentDBOptions>) -> Self {
        Self {
            index: Arc::new(RwLock::new(HashMap::new())),
            opts,
        }
    }

    /// Retorna uma cópia de todas as chaves presentes no índice.
    pub fn keys(&self) -> Vec<String> {
        // Adquire um bloqueio de leitura. O unwrap trata casos de "poisoning" do mutex.
        let index_lock = self
            .index
            .read()
            .expect("Failed to acquire read lock on document index");
        // Coleta as chaves do mapa. `.keys()` retorna um iterador de &String,
        // então `.cloned()` cria novas Strings a partir das referências.
        index_lock.keys().cloned().collect()
    }

    /// Método específico para obter Vec<u8> do índice
    /// Usado internamente pela DocumentStore
    pub fn get_bytes(&self, key: &str) -> Option<Vec<u8>> {
        let index_lock = self
            .index
            .read()
            .expect("Failed to acquire read lock on document index");
        index_lock.get(key).cloned()
    }
}

// Implementa o trait StoreIndex para DocumentIndex
impl StoreIndex for DocumentIndex {
    type Error = crate::guardian::error::GuardianError;

    /// Verifica se uma chave existe no índice.
    fn contains_key(&self, key: &str) -> std::result::Result<bool, Self::Error> {
        let index_lock = self
            .index
            .read()
            .expect("Failed to acquire read lock on document index");
        Ok(index_lock.contains_key(key))
    }

    /// Retorna uma cópia dos dados para uma chave específica como bytes.
    fn get_bytes(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
        let index_lock = self
            .index
            .read()
            .expect("Failed to acquire read lock on document index");
        Ok(index_lock.get(key).cloned())
    }

    /// Retorna todas as chaves disponíveis no índice.
    fn keys(&self) -> std::result::Result<Vec<String>, Self::Error> {
        let index_lock = self
            .index
            .read()
            .expect("Failed to acquire read lock on document index");
        Ok(index_lock.keys().cloned().collect())
    }

    /// Retorna o número de entradas no índice.
    fn len(&self) -> std::result::Result<usize, Self::Error> {
        let index_lock = self
            .index
            .read()
            .expect("Failed to acquire read lock on document index");
        Ok(index_lock.len())
    }

    /// Verifica se o índice está vazio.
    fn is_empty(&self) -> std::result::Result<bool, Self::Error> {
        let index_lock = self
            .index
            .read()
            .expect("Failed to acquire read lock on document index");
        Ok(index_lock.is_empty())
    }

    /// Atualiza o índice processando as entradas do log de operações.
    ///
    /// Processa operações PUT, DEL e PUTALL para manter o índice sincronizado.
    fn update_index(
        &mut self,
        _log: &crate::log::Log,
        entries: &[crate::log::entry::Entry],
    ) -> std::result::Result<(), Self::Error> {
        let mut index = self
            .index
            .write()
            .expect("Failed to acquire write lock on document index");

        // Processa cada entrada do log
        for entry in entries {
            // Tenta desserializar o payload da entrada para obter a operação
            match crate::stores::operation::parse_operation(entry.clone()) {
                Ok(operation) => {
                    match operation.op() {
                        "PUT" => {
                            // Para PUT, adiciona/atualiza a chave com o valor
                            if let Some(key) = operation.key()
                                && !operation.value().is_empty() {
                                    index.insert(key.clone(), operation.value().to_vec());
                                }
                        }
                        "DEL" => {
                            // Para DEL, remove a chave do índice
                            if let Some(key) = operation.key() {
                                index.remove(key);
                            }
                        }
                        "PUTALL" => {
                            // Para PUTALL, adiciona todos os documentos da operação
                            for doc in operation.docs() {
                                index.insert(doc.key().to_string(), doc.value().to_vec());
                            }
                        }
                        _ => {
                            // Ignora operações desconhecidas
                        }
                    }
                }
                Err(_) => {
                    // Se falhar a desserialização, ignora esta entrada
                    // Isso pode acontecer se a entrada não for uma operação válida
                    continue;
                }
            }
        }

        Ok(())
    }

    /// Limpa todos os dados do índice.
    fn clear(&mut self) -> std::result::Result<(), Self::Error> {
        let mut index = self
            .index
            .write()
            .expect("Failed to acquire write lock on document index");
        index.clear();
        Ok(())
    }
}

// === MÉTODOS ESPECÍFICOS PARA IROH-DOCS ===

impl DocumentIndex {
    /// Atualiza o índice a partir de entradas do iroh-docs
    ///
    /// Este método é usado pelo IrohDocsDocumentStore para sincronizar
    /// o índice local com o estado do documento iroh-docs.
    ///
    /// # Argumentos
    /// * `entries` - Vetor de entradas do iroh-docs (key, hash_bytes)
    pub fn update_from_iroh_entries(
        &mut self,
        entries: Vec<(String, Vec<u8>)>,
    ) -> std::result::Result<(), crate::guardian::error::GuardianError> {
        let mut index = self
            .index
            .write()
            .expect("Failed to acquire write lock on document index");

        // Limpa índice atual
        index.clear();

        // Atualiza com novas entradas
        for (key, hash_bytes) in entries {
            if !hash_bytes.is_empty() {
                index.insert(key, hash_bytes);
            }
        }

        Ok(())
    }

    /// Adiciona ou atualiza uma única entrada no índice
    ///
    /// # Argumentos
    /// * `key` - Chave do documento
    /// * `hash_bytes` - Hash do blob (32 bytes)
    pub fn put(
        &self,
        key: String,
        hash_bytes: Vec<u8>,
    ) -> std::result::Result<(), crate::guardian::error::GuardianError> {
        let mut index = self
            .index
            .write()
            .expect("Failed to acquire write lock on document index");

        index.insert(key, hash_bytes);
        Ok(())
    }

    /// Remove uma entrada do índice
    ///
    /// # Argumentos
    /// * `key` - Chave do documento a remover
    pub fn remove(
        &self,
        key: &str,
    ) -> std::result::Result<Option<Vec<u8>>, crate::guardian::error::GuardianError> {
        let mut index = self
            .index
            .write()
            .expect("Failed to acquire write lock on document index");

        Ok(index.remove(key))
    }

    /// Retorna estatísticas do índice
    pub fn stats(&self) -> IndexStats {
        let index = self
            .index
            .read()
            .expect("Failed to acquire read lock on document index");

        let total_keys = index.len();
        let total_bytes: usize = index.values().map(|v| v.len()).sum();

        IndexStats {
            total_keys,
            total_bytes,
        }
    }
}

/// Estatísticas do índice
#[derive(Debug, Clone)]
pub struct IndexStats {
    /// Número total de chaves
    pub total_keys: usize,
    /// Bytes totais armazenados (hashes)
    pub total_bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_opts() -> Arc<CreateDocumentDBOptions> {
        Arc::new(CreateDocumentDBOptions {
            marshal: Arc::new(|doc| {
                serde_json::to_vec(doc)
                    .map_err(|e| crate::guardian::error::GuardianError::Other(e.to_string()))
            }),
            unmarshal: Arc::new(|bytes| {
                serde_json::from_slice(bytes)
                    .map_err(|e| crate::guardian::error::GuardianError::Other(e.to_string()))
            }),
            key_extractor: Arc::new(|doc| {
                Ok(doc
                    .get("_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string())
            }),
            item_factory: Arc::new(|| {
                serde_json::json!({
                    "_id": "",
                    "data": {}
                })
            }),
        })
    }

    #[test]
    fn test_new_index() {
        let opts = create_test_opts();
        let index = DocumentIndex::new(opts);
        assert!(index.is_empty().unwrap());
    }

    #[test]
    fn test_put_and_get() {
        let opts = create_test_opts();
        let index = DocumentIndex::new(opts);

        let hash = vec![1u8; 32];
        index.put("key1".to_string(), hash.clone()).unwrap();

        let retrieved = index.get_bytes("key1");
        assert_eq!(retrieved, Some(hash));
    }

    #[test]
    fn test_remove() {
        let opts = create_test_opts();
        let index = DocumentIndex::new(opts);

        let hash = vec![2u8; 32];
        index.put("key2".to_string(), hash.clone()).unwrap();

        let removed = index.remove("key2").unwrap();
        assert_eq!(removed, Some(hash.clone()));

        let retrieved = index.get_bytes("key2");
        assert_eq!(retrieved, None);
    }

    #[test]
    fn test_update_from_iroh_entries() {
        let opts = create_test_opts();
        let mut index = DocumentIndex::new(opts);

        let entries = vec![
            ("key1".to_string(), vec![1u8; 32]),
            ("key2".to_string(), vec![2u8; 32]),
            ("key3".to_string(), vec![3u8; 32]),
        ];

        index.update_from_iroh_entries(entries).unwrap();

        assert_eq!(index.len().unwrap(), 3);
        assert!(index.contains_key("key1").unwrap());
        assert!(index.contains_key("key2").unwrap());
        assert!(index.contains_key("key3").unwrap());
    }

    #[test]
    fn test_stats() {
        let opts = create_test_opts();
        let index = DocumentIndex::new(opts);

        index.put("key1".to_string(), vec![1u8; 32]).unwrap();
        index.put("key2".to_string(), vec![2u8; 32]).unwrap();

        let stats = index.stats();
        assert_eq!(stats.total_keys, 2);
        assert_eq!(stats.total_bytes, 64); // 2 * 32 bytes
    }

    #[test]
    fn test_clear() {
        let opts = create_test_opts();
        let mut index = DocumentIndex::new(opts);

        index.put("key1".to_string(), vec![1u8; 32]).unwrap();
        index.put("key2".to_string(), vec![2u8; 32]).unwrap();

        assert_eq!(index.len().unwrap(), 2);

        index.clear().unwrap();
        assert!(index.is_empty().unwrap());
    }
}
