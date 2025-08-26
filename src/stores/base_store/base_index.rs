use std::sync::{Arc, RwLock};
use std::any::Any;
use crate::error::GuardianError;
use crate::eqlabs_ipfs_log::{entry::Entry, log::Log};
use crate::iface::StoreIndex;

/// Equivalente à struct `baseIndex` do Go.
pub struct BaseIndex {
    /// ID do índice, geralmente a chave pública da loja.
    id: Vec<u8>,

    /// O índice em si, que é uma lista de entradas do log.
    /// É envolvido por um `RwLock` para permitir acesso concorrente seguro
    /// (múltiplos leitores ou um escritor).
    index: RwLock<Vec<Entry>>,
}

/// Equivalente à função `NewBaseIndex` do Go.
///
/// Construtor para o `BaseIndex`.
pub fn new_base_index(public_key: Vec<u8>) -> Box<dyn StoreIndex<Error = GuardianError> + Send + Sync> {
    Box::new(BaseIndex {
        id: public_key,
        // O índice começa como uma lista vazia.
        index: RwLock::new(Vec::new()),
    })
}

/// Implementação do trait `StoreIndex` para `BaseIndex`.
impl StoreIndex for BaseIndex {
    /// Especifica que usaremos GuardianError como o tipo de erro associado.
    type Error = GuardianError;

    /// Equivalente à função `Get` em Go.
    ///
    /// Retorna uma cópia da lista de entradas do índice. O parâmetro `key` é ignorado.
    fn get(&self, _key: &str) -> Option<&(dyn Any + Send + Sync)> {
        // Para este caso, retornamos None pois não podemos retornar uma referência
        // a dados protegidos por um lock que será liberado no fim desta função.
        // Uma implementação real precisaria de uma abordagem diferente.
        None
    }

    /// Equivalente à função `UpdateIndex` em Go.
    ///
    /// Substitui completamente o índice atual pela lista de todas as entradas do log fornecido.
    fn update_index(&mut self, log: &Log, _entries: &[Entry]) -> std::result::Result<(), Self::Error> {
        // Adquire um "write lock" para ter acesso exclusivo e de escrita.
        let mut guard = self.index.write().unwrap();

        // Convert Arc<Entry> to Entry for compatibility
        let log_values = log.values();
        let converted_values: Vec<Entry> = log_values.into_iter()
            .map(|arc_entry| {
                // Tenta fazer clone do Arc content, se não conseguir usa default
                match Arc::try_unwrap(arc_entry) {
                    Ok(entry) => entry,
                    Err(arc) => (*arc).clone(),
                }
            })
            .collect();
        
        // Substitui o conteúdo do vetor protegido pelo lock.
        *guard = converted_values;
        
        Ok(())
    }
}