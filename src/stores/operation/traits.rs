use crate::log::entry::Entry;

pub trait OpDoc {
    fn get_key(&self) -> &str;
    fn get_value(&self) -> &[u8];
}

/// Descreve uma operação CRDT.
pub trait Operation {
    /// Obtém uma chave, se aplicável (ex: para stores de chave-valor).
    fn get_key(&self) -> Option<&String>;

    /// Retorna o nome da operação (ex: "append", "put", "remove").
    fn get_operation(&self) -> &str;

    /// Retorna o payload (dados) da operação.
    fn get_value(&self) -> &[u8];

    /// Obtém a Entry do Log subjacente.
    fn get_entry(&self) -> &Entry;

    /// Obtém a lista de documentos.
    fn get_docs(&self) -> Vec<Box<dyn OpDoc>>;

    /// Serializa a operação.
    fn marshal(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>>;
}
