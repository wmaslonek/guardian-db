use crate::error::GuardianError;
use crate::ipfs_log::{entry::Entry, log::Log};
use crate::stores::operation::operation::Operation;
use crate::traits::StoreIndex;
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
/// KvIndex mantém um índice de chave-valor em memória para a KvStore.
pub struct KvIndex {
    index: Arc<RwLock<HashMap<String, Vec<u8>>>>,
}

impl Default for KvIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl KvIndex {
    /// Cria uma nova instância de KvIndex.
    pub fn new() -> Self {
        KvIndex {
            index: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl StoreIndex for KvIndex {
    type Error = GuardianError;

    /// Verifica se uma chave existe no índice.
    fn contains_key(&self, key: &str) -> std::result::Result<bool, Self::Error> {
        let index = self.index.read();
        Ok(index.contains_key(key))
    }

    /// Retorna uma cópia dos dados para uma chave específica como bytes.
    fn get_bytes(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
        let index = self.index.read();
        Ok(index.get(key).cloned())
    }

    /// Retorna todas as chaves disponíveis no índice.
    fn keys(&self) -> std::result::Result<Vec<String>, Self::Error> {
        let index = self.index.read();
        Ok(index.keys().cloned().collect())
    }

    /// Retorna o número de entradas no índice.
    fn len(&self) -> std::result::Result<usize, Self::Error> {
        let index = self.index.read();
        Ok(index.len())
    }

    /// Verifica se o índice está vazio.
    fn is_empty(&self) -> std::result::Result<bool, Self::Error> {
        let index = self.index.read();
        Ok(index.is_empty())
    }
    /// Atualiza o índice processando as entradas do log de operações (oplog).
    fn update_index(
        &mut self,
        oplog: &Log,
        _entries: &[Entry],
    ) -> std::result::Result<(), Self::Error> {
        let mut handled = HashSet::new();

        // Usa um "write lock" para garantir acesso exclusivo durante a atualização.
        let mut index = self.index.write();

        // Itera sobre as entradas do log em ordem reversa para processar
        // as operações mais recentes primeiro.
        for entry in oplog.values().iter().rev() {
            // Since payload is a String, not Option<String>, we check if it's not empty
            if !entry.payload.is_empty() {
                match serde_json::from_str::<Operation>(&entry.payload) {
                    Ok(op_result) => {
                        // Pula entradas sem chave.
                        let key = match op_result.key() {
                            Some(k) => k,
                            None => continue,
                        };

                        // Processa cada chave apenas uma vez.
                        if !handled.contains(key) {
                            handled.insert(key.clone());

                            match op_result.op() {
                                "PUT" => {
                                    let value = op_result.value();
                                    if !value.is_empty() {
                                        index.insert(key.clone(), value.to_vec());
                                    }
                                }
                                "DEL" => {
                                    index.remove(key);
                                }
                                _ => { /* ignora outras operações */ }
                            }
                        }
                    }
                    Err(e) => {
                        return Err(GuardianError::Store(format!(
                            "Erro ao parsear operação: {}",
                            e
                        )));
                    }
                }
            }
        }

        Ok(())
    }

    /// Limpa todos os dados do índice.
    fn clear(&mut self) -> std::result::Result<(), Self::Error> {
        let mut index = self.index.write();
        index.clear();
        Ok(())
    }
}
