use crate::eqlabs_ipfs_log::entry::Entry;
use cid::Cid;

// equivalente a EventLoadAdded em go
/// Um evento acionado quando novas entradas são adicionadas à fila de replicação.
#[derive(Debug, Clone)]
pub struct EventLoadAdded {
    pub entry: Box<Entry>,
    pub hash: Cid,
}

// equivalente a NewEventLoadAdded em go
impl EventLoadAdded {
    /// Cria um novo evento EventLoadAdded.
    pub fn new(hash: Cid, entry: Box<Entry>) -> Self {
        Self { entry, hash }
    }
}

// equivalente a EventLoadProgress em go
/// Um evento acionado à medida que as entradas são carregadas (buscadas) da rede.
#[derive(Debug, Clone)]
pub struct EventLoadProgress {
    pub entry: Box<Entry>,
}

// equivalente a NewEventLoadProgress em go
impl EventLoadProgress {
    /// Cria um novo evento EventLoadProgress.
    pub fn new(entry: Box<Entry>) -> Self {
        Self { entry }
    }
}

// equivalente a EventLoadEnd em go
/// Um evento acionado quando um lote de carregamento (load) é concluído.
#[derive(Debug, Clone)]
pub struct EventLoadEnd {
    // Using String IDs instead of Log struct to avoid Debug/Clone trait issues
    pub log_ids: Vec<String>,
}

// equivalente a NewEventLoadEnd em go
impl EventLoadEnd {
    /// Cria um novo evento EventLoadEnd.
    pub fn new(log_ids: Vec<String>) -> Self {
        Self { log_ids }
    }
}
