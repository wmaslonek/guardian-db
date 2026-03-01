use crate::guardian::error::GuardianError;
use crate::log::{Log, entry::Entry};
use crate::traits::StoreIndex;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// KvIndex mantém um índice de chave-valor em memória para a KvStore.
///
/// Na arquitetura iroh-docs, este índice é um espelho local do estado
/// do documento iroh-docs. O `update_index` é um no-op pois o índice
/// é atualizado diretamente pelas operações put/delete.
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

    fn contains_key(&self, key: &str) -> std::result::Result<bool, Self::Error> {
        let index = self.index.read();
        Ok(index.contains_key(key))
    }

    fn get_bytes(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
        let index = self.index.read();
        Ok(index.get(key).cloned())
    }

    fn keys(&self) -> std::result::Result<Vec<String>, Self::Error> {
        let index = self.index.read();
        Ok(index.keys().cloned().collect())
    }

    fn len(&self) -> std::result::Result<usize, Self::Error> {
        let index = self.index.read();
        Ok(index.len())
    }

    fn is_empty(&self) -> std::result::Result<bool, Self::Error> {
        let index = self.index.read();
        Ok(index.is_empty())
    }

    /// No-op para iroh-docs — o índice local é atualizado diretamente
    /// pelas operações put/delete no GuardianDBKeyValue.
    fn update_index(
        &mut self,
        _oplog: &Log,
        _entries: &[Entry],
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn clear(&mut self) -> std::result::Result<(), Self::Error> {
        let mut index = self.index.write();
        index.clear();
        Ok(())
    }
}
