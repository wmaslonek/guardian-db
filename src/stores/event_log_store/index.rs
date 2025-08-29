use crate::eqlabs_ipfs_log::{entry::Entry, log::Log};
use crate::error::{GuardianError, Result};
use crate::iface::StoreIndex;
use parking_lot::RwLock;
use std::{any::Any, sync::Arc};

/// Equivalente a struct "eventIndex" em go.
/// `EventIndex` armazena uma cópia do log completo para queries e stream de eventos.
///
/// Um EventLogStore é um log de eventos "append-only" onde todas as operações
/// são do tipo "ADD" e o índice mantém acesso ao log completo para permitir
/// queries temporais e streaming de eventos.
pub struct EventIndex {
    /// Cache de entradas para acesso rápido por posição
    entries_cache: Arc<RwLock<Vec<Entry>>>,
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
/// Isto garante a compatibilidade com a interface, substituindo a verificação
/// `var _ iface.StoreIndex = &eventIndex{}` do Go.
impl StoreIndex for EventIndex {
    type Error = GuardianError;

    /// Equivalente a função "Get" em go.
    /// Para EventLogStore, retorna todas as entradas do log.
    /// A chave é ignorada pois EventLog é um log sequencial.
    fn get(&self, _key: &str) -> Option<&(dyn Any + Send + Sync)> {
        // Devido às limitações do trait StoreIndex que retorna uma referência,
        // e nossa implementação usar Arc<RwLock<>>, não podemos retornar uma
        // referência direta. Use os métodos específicos como get_all_entries()
        // para acesso aos dados.
        //
        // Esta limitação é arquitetural - o trait foi projetado para estruturas
        // que podem retornar referencias diretas, não para dados protegidos por locks.
        None
    }

    /// Equivalente a função "UpdateIndex" em go.
    /// Substitui o índice interno pelo novo log fornecido e atualiza o cache.
    ///
    /// Nota: Como Log não implementa Clone, vamos reconstruir o cache
    /// diretamente das entradas fornecidas, que é mais eficiente.
    fn update_index(&mut self, _log: &Log, entries: &[Entry]) -> Result<()> {
        // Atualiza o cache diretamente com as entradas fornecidas
        {
            let mut cache = self.entries_cache.write();
            cache.clear();
            cache.extend_from_slice(entries);
        }

        Ok(())
    }
}

/// Equivalente a função "NewEventIndex" em go.
/// Esta é a função fábrica que cria uma nova instância do nosso índice.
/// A verificação `var _ iface.IndexConstructor = NewEventIndex` do Go é garantida
/// pela assinatura desta função, que deve corresponder ao que a store espera.
pub fn new_event_index(_params: &[u8]) -> Box<dyn StoreIndex<Error = GuardianError>> {
    Box::new(EventIndex::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eqlabs_ipfs_log::{
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
            "test_log", // log_id
            payload,    // data
            &[],        // next (EntryOrHash slice)
            None,       // clock
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
        assert_eq!(entry_at_1.unwrap().payload(), "test2");

        let last_2 = index.get_last_entries(2);
        assert_eq!(last_2.len(), 2);
        assert_eq!(last_2[0].payload(), "test2");
        assert_eq!(last_2[1].payload(), "test3");
    }

    #[test]
    fn test_new_event_index_factory() {
        let params = b"test_params";
        let index_box = new_event_index(params);

        // Verify it returns a valid StoreIndex
        assert!(index_box.get("test").is_none()); // Should return None as per implementation
    }

    #[test]
    fn test_store_index_trait_implementation() {
        let index = EventIndex::new();

        // Test get method (should return None due to architectural limitations)
        assert!(index.get("any_key").is_none());

        // Test update_index with a simplified approach
        // Since creating a real Log is complex, we'll test the core functionality
        // by directly manipulating the internal state and then calling update_index

        // Create a minimal log for testing - this tests the basic functionality
        // without the complexity of full IPFS setup
        let _identity = (*create_test_identity()).clone();

        // For this test, we'll create a simple log structure
        // In real usage, the Log would be properly initialized by the BaseStore

        // Skip the complex IpfsClient setup and just test that update_index accepts parameters
        // and executes without panicking
        let _entries: Vec<Entry> = vec![];

        // Note: We can't easily create a real Log here without async setup,
        // so we'll test the other functionality instead
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
    }
}
