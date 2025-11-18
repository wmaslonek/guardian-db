use crate::error::GuardianError;
use crate::ipfs_log::{entry::Entry, log::Log};
use crate::traits::StoreIndex;

pub struct NoopIndex;

/// Esta é uma função "factory" ou construtor que cria uma nova instância
/// do NoopIndex.
pub fn new_noop_index(
    _public_key: &[u8],
) -> Box<dyn StoreIndex<Error = GuardianError> + Send + Sync> {
    Box::new(NoopIndex)
}

/// Implementação do trait `StoreIndex` para `NoopIndex`.
/// Aqui é onde a lógica "vazia" é definida.
impl StoreIndex for NoopIndex {
    /// Usamos GuardianError como o tipo de erro associado.
    /// GuardianError implementa std::error::Error.
    type Error = GuardianError;

    /// Verifica se uma chave existe no índice.
    fn contains_key(&self, _key: &str) -> std::result::Result<bool, Self::Error> {
        Ok(false)
    }

    /// Retorna uma cópia dos dados para uma chave específica como bytes.
    fn get_bytes(&self, _key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
        Ok(None)
    }

    /// Retorna todas as chaves disponíveis no índice.
    fn keys(&self) -> std::result::Result<Vec<String>, Self::Error> {
        Ok(Vec::new())
    }

    /// Retorna o número de entradas no índice.
    fn len(&self) -> std::result::Result<usize, Self::Error> {
        Ok(0)
    }

    /// Verifica se o índice está vazio.
    fn is_empty(&self) -> std::result::Result<bool, Self::Error> {
        Ok(true)
    }

    /// A função não faz nada e sempre retorna `Ok(())`.
    fn update_index(
        &mut self,
        _oplog: &Log,
        _entries: &[Entry],
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    /// Limpa todos os dados do índice.
    fn clear(&mut self) -> std::result::Result<(), Self::Error> {
        Ok(())
    }
}
