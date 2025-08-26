use crate::error::GuardianError;
use std::any::Any;
use crate::iface::StoreIndex;
use crate::eqlabs_ipfs_log::{entry::Entry, log::Log};

/// Equivalente à `noopIndex struct{}` do Go.
///
/// Em Rust, uma struct sem campos é chamada de "unit struct" e é
/// definida com um ponto e vírgula. É a forma idiomática para tipos
/// que existem apenas para implementar traits.
pub struct NoopIndex;

/// Equivalente à função `NewNoopIndex` do Go.
///
/// Esta é uma função "factory" ou construtor que cria uma nova instância
/// do NoopIndex. Ela retorna um "trait object" (`Box<dyn StoreIndex>`),
/// que é como Rust se refere a um tipo que implementa uma interface dinamicamente.
/// O parâmetro `_public_key` é ignorado, como no original.
pub fn new_noop_index(_public_key: &[u8]) -> Box<dyn StoreIndex<Error = GuardianError> + Send + Sync> {
    Box::new(NoopIndex)
}

/// Implementação do trait `StoreIndex` para a nossa `NoopIndex`.
/// Aqui é onde a lógica "vazia" é definida.
impl StoreIndex for NoopIndex {
    /// Especifica que usaremos GuardianError como o tipo de erro associado.
    /// GuardianError implementa std::error::Error e é adequado para bibliotecas.
    type Error = GuardianError;

    /// Equivalente à função `Get` em Go.
    ///
    /// A função não faz nada e sempre retorna `None`, que é o
    /// equivalente idiomático em Rust para o `nil` do Go neste contexto.
    fn get(&self, _key: &str) -> Option<&(dyn Any + Send + Sync)> {
        None
    }

    /// Equivalente à função `UpdateIndex` em Go.
    ///
    /// A função não faz nada e sempre retorna `Ok(())`. `Ok(())` é
    /// o equivalente idiomático em Rust para um retorno de sucesso
    /// sem valor (como um `nil` no campo de erro do Go).
    fn update_index(&mut self, _oplog: &Log, _entries: &[Entry]) -> std::result::Result<(), Self::Error> {
        Ok(())
    }
}