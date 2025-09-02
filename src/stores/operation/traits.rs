use crate::ipfs_log::entry::Entry;

/// equivalente à interface OpDoc em go
pub trait OpDoc {
    fn get_key(&self) -> &str;
    fn get_value(&self) -> &[u8];
}

/// equivalente à interface Operation em go
/// Descreve uma operação CRDT.
pub trait Operation {
    /// Obtém uma chave, se aplicável (ex: para stores de chave-valor).
    fn get_key(&self) -> Option<&String>;

    /// Retorna o nome da operação (ex: "append", "put", "remove").
    fn get_operation(&self) -> &str;

    /// Retorna o payload (dados) da operação.
    fn get_value(&self) -> &[u8];

    /// Obtém a Entry do log IPFS subjacente.
    fn get_entry(&self) -> &Entry;

    /// Obtém a lista de documentos.
    /// A versão em Go retorna `[]OpDoc` (um slice de interfaces). O equivalente em Rust
    /// é um Vec de "trait objects" (`Box<dyn OpDoc>`), que permite polimorfismo.
    fn get_docs(&self) -> Vec<Box<dyn OpDoc>>;

    /// Serializa a operação.
    /// Retorna um Result, onde o erro é um trait object de erro genérico.
    fn marshal(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>>;
}
