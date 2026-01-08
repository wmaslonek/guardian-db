use crate::guardian::error::GuardianError;
use crate::log::{Log, entry::Entry};
use crate::traits::StoreIndex;
use parking_lot::RwLock;
use std::sync::Arc;

/// `EventIndex` armazena uma cópia do log completo para queries e stream de eventos.
///
/// Um EventLogStore é um log de eventos "append-only" onde todas as operações
/// são do tipo "ADD" e o índice mantém acesso ao log completo para permitir
/// queries temporais e streaming de eventos.
pub struct EventIndex {
    /// Cache de entradas para acesso rápido por posição
    entries_cache: Arc<RwLock<Vec<Entry>>>,
}

impl Default for EventIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl EventIndex {
    /// Construtor padrão para um EventIndex.
    pub fn new() -> Self {
        EventIndex {
            entries_cache: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Retorna o número de entradas no log
    pub fn len(&self) -> usize {
        let cache = self.entries_cache.read();
        cache.len()
    }

    /// Verifica se o log está vazio
    pub fn is_empty(&self) -> bool {
        let cache = self.entries_cache.read();
        cache.is_empty()
    }

    /// Obtém todas as entradas do log
    pub fn get_all_entries(&self) -> Vec<Entry> {
        let cache = self.entries_cache.read();
        cache.clone()
    }

    /// Obtém uma entrada específica por índice
    pub fn get_entry_at(&self, index: usize) -> Option<Entry> {
        let cache = self.entries_cache.read();
        cache.get(index).cloned()
    }

    /// Obtém as últimas N entradas
    pub fn get_last_entries(&self, count: usize) -> Vec<Entry> {
        let cache = self.entries_cache.read();
        let start = cache.len().saturating_sub(count);
        cache[start..].to_vec()
    }
}

/// Implementação do trait StoreIndex para EventIndex.
impl StoreIndex for EventIndex {
    type Error = GuardianError;

    /// Verifica se uma chave existe no índice.
    /// Para EventLogStore, a chave é interpretada como um índice numérico.
    fn contains_key(&self, key: &str) -> std::result::Result<bool, Self::Error> {
        if let Ok(index) = key.parse::<usize>() {
            let cache = self.entries_cache.read();
            Ok(index < cache.len())
        } else {
            Ok(false)
        }
    }

    /// Retorna uma entrada específica como bytes.
    /// Para EventLogStore, retorna o payload da entrada no índice especificado.
    fn get_bytes(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
        if let Ok(index) = key.parse::<usize>() {
            let cache = self.entries_cache.read();
            if let Some(entry) = cache.get(index) {
                // Payload já é Vec<u8>, pode retornar diretamente
                Ok(Some(entry.payload().to_vec()))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    /// Retorna todas as "chaves" disponíveis (índices) como strings.
    fn keys(&self) -> std::result::Result<Vec<String>, GuardianError> {
        let cache = self.entries_cache.read();

        // Return indices as string keys
        let keys: Vec<String> = (0..cache.len()).map(|i| i.to_string()).collect();

        Ok(keys)
    }

    /// Retorna o número de entradas no log.
    fn len(&self) -> std::result::Result<usize, Self::Error> {
        let cache = self.entries_cache.read();
        Ok(cache.len())
    }

    /// Verifica se o log está vazio.
    fn is_empty(&self) -> std::result::Result<bool, Self::Error> {
        let cache = self.entries_cache.read();
        Ok(cache.is_empty())
    }

    /// Substitui o índice interno pelo novo log fornecido e atualiza o cache.
    /// Como Log não implementa Clone, vamos reconstruir o cache
    /// diretamente das entradas fornecidas, que é mais eficiente.
    fn update_index(
        &mut self,
        _log: &Log,
        entries: &[Entry],
    ) -> std::result::Result<(), Self::Error> {
        // Atualiza o cache diretamente com as entradas fornecidas
        {
            let mut cache = self.entries_cache.write();
            cache.clear();
            cache.extend_from_slice(entries);
        }

        Ok(())
    }

    /// Limpa todas as entradas do log.
    fn clear(&mut self) -> std::result::Result<(), Self::Error> {
        let mut cache = self.entries_cache.write();
        cache.clear();
        Ok(())
    }

    // === IMPLEMENTAÇÃO DOS MÉTODOS OPCIONAIS DE OTIMIZAÇÃO ===

    /// Implementa acesso otimizado a range de entradas para EventLogStore.
    ///
    /// EventIndex mantém Entry completas em cache, permitindo acesso
    /// direto sem necessidade de deserialização.
    fn get_entries_range(&self, start: usize, end: usize) -> Option<Vec<Entry>> {
        let cache = self.entries_cache.read();

        // Validação de bounds
        if start > end || start >= cache.len() {
            return None;
        }

        let actual_end = end.min(cache.len());
        Some(cache[start..actual_end].to_vec())
    }

    /// Acesso otimizado às últimas N entradas.
    ///
    /// Caso de uso muito comum para EventLogStore - buscar eventos recentes.
    fn get_last_entries(&self, count: usize) -> Option<Vec<Entry>> {
        let cache = self.entries_cache.read();

        if cache.is_empty() || count == 0 {
            return Some(Vec::new());
        }

        let start = cache.len().saturating_sub(count);
        Some(cache[start..].to_vec())
    }

    /// Busca otimizada por Hash.
    ///
    /// Atualmente usa busca linear O(n), mas estrutura preparada
    /// para futuro índice secundário O(1) por Hash.
    fn get_entry_by_hash(&self, hash: &iroh_blobs::Hash) -> Option<Entry> {
        let cache = self.entries_cache.read();

        // Busca linear por enquanto - futuro: HashMap<Hash, Entry>
        cache.iter().find(|entry| entry.hash() == hash).cloned()
    }

    /// EventIndex suporta queries otimizadas com Entry completas.
    fn supports_entry_queries(&self) -> bool {
        true
    }
}

/// Esta é a função fábrica que cria uma nova instância do índice.
pub fn new_event_index(_params: &[u8]) -> Box<dyn StoreIndex<Error = GuardianError>> {
    Box::new(EventIndex::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{
        entry::Entry,
        identity::{Identity, Signatures},
    };
    use std::sync::Arc;

    fn create_test_identity() -> Arc<Identity> {
        // Create a simple test identity
        Arc::new(Identity::new(
            "test_id",
            "test_public_key",
            Signatures::new("id_signature", "public_signature"),
        ))
    }

    fn create_test_entry(payload: &str) -> Entry {
        let identity = (*create_test_identity()).clone();

        // Create a simple test entry using the correct signature
        Entry::new(
            identity,
            "test_log",         // log_id
            payload.as_bytes(), // data
            &[],                // next (EntryOrHash slice)
            None,               // clock
        )
    }

    #[test]
    fn test_event_index_creation() {
        let index = EventIndex::new();
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_event_index_basic_operations() {
        let index = EventIndex::new();

        // Test initial state
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
        assert!(index.get_all_entries().is_empty());
        assert!(index.get_entry_at(0).is_none());
        assert!(index.get_last_entries(5).is_empty());
    }

    #[test]
    fn test_entries_cache_functionality() {
        let index = EventIndex::new();

        // Simula dados no cache diretamente (para teste de cache)
        {
            let mut cache = index.entries_cache.write();
            cache.push(create_test_entry("test1"));
            cache.push(create_test_entry("test2"));
            cache.push(create_test_entry("test3"));
        }

        assert_eq!(index.len(), 3);
        assert!(!index.is_empty());

        let all_entries = index.get_all_entries();
        assert_eq!(all_entries.len(), 3);

        let entry_at_1 = index.get_entry_at(1);
        assert!(entry_at_1.is_some());
        assert_eq!(entry_at_1.unwrap().payload(), b"test2");

        let last_2 = index.get_last_entries(2);
        assert_eq!(last_2.len(), 2);
        assert_eq!(last_2[0].payload(), b"test2");
        assert_eq!(last_2[1].payload(), b"test3");
    }

    #[test]
    fn test_new_event_index_factory() {
        let params = b"test_params";
        let index_box = new_event_index(params);

        // Verify it returns a valid StoreIndex with the new interface
        assert!(index_box.is_empty().unwrap()); // Should be empty initially
        assert_eq!(index_box.len().unwrap(), 0); // Should have length 0
    }

    #[test]
    fn test_store_index_trait_implementation() {
        let mut index = EventIndex::new();

        // Test new trait methods using the internal EventIndex methods
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
        assert!(index.get_all_entries().is_empty());

        // Test trait methods
        assert!(
            (&index as &dyn StoreIndex<Error = GuardianError>)
                .is_empty()
                .unwrap()
        );
        assert_eq!(
            (&index as &dyn StoreIndex<Error = GuardianError>)
                .len()
                .unwrap(),
            0
        );
        assert!(
            (&index as &dyn StoreIndex<Error = GuardianError>)
                .keys()
                .unwrap()
                .is_empty()
        );
        assert!(
            !(&index as &dyn StoreIndex<Error = GuardianError>)
                .contains_key("0")
                .unwrap()
        );
        assert!(
            (&index as &dyn StoreIndex<Error = GuardianError>)
                .get_bytes("0")
                .unwrap()
                .is_none()
        );

        // Test after adding some entries to cache
        {
            let mut cache = index.entries_cache.write();
            cache.push(create_test_entry("test1"));
            cache.push(create_test_entry("test2"));
        }

        // Test with data using trait methods
        let store_index = &index as &dyn StoreIndex<Error = GuardianError>;
        assert!(!store_index.is_empty().unwrap());
        assert_eq!(store_index.len().unwrap(), 2);
        assert_eq!(store_index.keys().unwrap(), vec!["0", "1"]);
        assert!(store_index.contains_key("0").unwrap());
        assert!(store_index.contains_key("1").unwrap());
        assert!(!store_index.contains_key("2").unwrap());

        // Test get_bytes
        let bytes_0 = store_index.get_bytes("0").unwrap();
        assert!(bytes_0.is_some());
        assert_eq!(bytes_0.unwrap(), b"test1".to_vec());

        let bytes_1 = store_index.get_bytes("1").unwrap();
        assert!(bytes_1.is_some());
        assert_eq!(bytes_1.unwrap(), b"test2".to_vec());

        // Test clear
        index.clear().unwrap();
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
    }
}
