use crate::error::GuardianError;
use crate::iface::StoreIndex;
use crate::ipfs_log::{entry::Entry, log::Log};
use crate::stores::operation::operation::Operation;
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Em Go: kvIndex.
/// Em Rust, `muIndex` e `index` são combinados em um único campo
/// `RwLock<HashMap<...>>` para garantir acesso seguro entre threads.
pub struct KvIndex {
    index: Arc<RwLock<HashMap<String, Vec<u8>>>>,
}

impl KvIndex {
    /// equivalente a função NewKVIndex em go.
    /// Em Rust, é um construtor associado à struct.
    pub fn new() -> Self {
        KvIndex {
            index: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

// Em Go, `kvIndex` implementa a interface `StoreIndex`.
// Em Rust, fazemos isso explicitamente com um bloco `impl`.
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

    /// equivalente a função UpdateIndex em go
    fn update_index(
        &mut self,
        oplog: &Log,
        _entries: &[Entry],
    ) -> std::result::Result<(), Self::Error> {
        // Um `HashSet` é mais idiomático em Rust para rastrear chaves já processadas.
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
