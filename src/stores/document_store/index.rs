use crate::iface::{CreateDocumentDBOptions, StoreIndex};
use crate::ipfs_log::{entry::Entry, log::Log};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

type Result<T> = std::result::Result<T, crate::error::GuardianError>;

/// DocumentIndex mantém um índice de chave-valor em memória para a DocumentStore.
pub struct DocumentIndex {
    // O índice principal, protegido por um RwLock para acesso concorrente seguro.
    index: RwLock<HashMap<String, Vec<u8>>>,
    // Opções de configuração da store, compartilhadas via Arc.
    opts: Arc<CreateDocumentDBOptions>,
}

impl DocumentIndex {
    /// equivalente a newDocumentIndex em go
    /// Cria uma nova instância de DocumentIndex.
    pub fn new(opts: Arc<CreateDocumentDBOptions>) -> Self {
        Self {
            index: RwLock::new(HashMap::new()),
            opts,
        }
    }

    /// equivalente a Keys em go
    /// Retorna uma cópia de todas as chaves presentes no índice.
    pub fn keys(&self) -> Vec<String> {
        // Adquire um bloqueio de leitura. O unwrap trata casos de "poisoning" do mutex.
        let index_lock = self.index.read().unwrap();

        // Coleta as chaves do mapa. `.keys()` retorna um iterador de &String,
        // então `.cloned()` cria novas Strings a partir das referências.
        index_lock.keys().cloned().collect()
    }

    /// Método específico para obter Vec<u8> do índice
    /// Usado internamente pela DocumentStore
    pub fn get_bytes(&self, key: &str) -> Option<Vec<u8>> {
        let index_lock = self.index.read().unwrap();
        index_lock.get(key).cloned()
    }
}

// Implementa o trait StoreIndex, análogo à implementação da interface em Go.
impl StoreIndex for DocumentIndex {
    type Error = crate::error::GuardianError;

    /// Verifica se uma chave existe no índice.
    fn contains_key(&self, key: &str) -> std::result::Result<bool, Self::Error> {
        let index_lock = self.index.read().unwrap();
        Ok(index_lock.contains_key(key))
    }

    /// Retorna uma cópia dos dados para uma chave específica como bytes.
    fn get_bytes(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
        let index_lock = self.index.read().unwrap();
        Ok(index_lock.get(key).cloned())
    }

    /// Retorna todas as chaves disponíveis no índice.
    fn keys(&self) -> std::result::Result<Vec<String>, Self::Error> {
        let index_lock = self.index.read().unwrap();
        Ok(index_lock.keys().cloned().collect())
    }

    /// Retorna o número de entradas no índice.
    fn len(&self) -> std::result::Result<usize, Self::Error> {
        let index_lock = self.index.read().unwrap();
        Ok(index_lock.len())
    }

    /// Verifica se o índice está vazio.
    fn is_empty(&self) -> std::result::Result<bool, Self::Error> {
        let index_lock = self.index.read().unwrap();
        Ok(index_lock.is_empty())
    }

    /// equivalente a UpdateIndex em go
    /// Atualiza o índice processando as entradas do log de operações (oplog).
    fn update_index(
        &mut self,
        _log: &Log,
        entries: &[Entry],
    ) -> std::result::Result<(), Self::Error> {
        // Um conjunto para rastrear chaves já processadas, garantindo que
        // apenas a operação mais recente para cada chave seja aplicada.
        let mut handled = HashSet::new();

        // Adquire um bloqueio de escrita, pois vamos modificar o índice.
        let mut index = self.index.write().unwrap();

        // Itera sobre as entradas fornecidas em ordem reversa (do mais novo para o mais antigo).
        for entry in entries.iter().rev() {
            let operation = crate::stores::operation::operation::parse_operation(entry.clone())
                .map_err(|e| {
                    crate::error::GuardianError::Store(format!("Erro ao parsear operação: {}", e))
                })?;

            // Para operações normais, obtém a chave principal.
            let key = match operation.key() {
                Some(k) if !k.is_empty() => k,
                _ => continue, // Ignora entradas com chave nula ou vazia.
            };

            if handled.contains(key) {
                continue;
            }
            handled.insert(key.clone());

            // Aplica a operação (PUT ou DEL).
            match operation.op() {
                "PUT" => {
                    let value = operation.value();
                    if !value.is_empty() {
                        index.insert(key.clone(), value.to_vec());
                    }
                }
                "DEL" => {
                    index.remove(key);
                }
                _ => {} // Ignora outras operações.
            }
        }

        Ok(())
    }

    /// Limpa todos os dados do índice.
    fn clear(&mut self) -> std::result::Result<(), Self::Error> {
        let mut index = self.index.write().unwrap();
        index.clear();
        Ok(())
    }
}
