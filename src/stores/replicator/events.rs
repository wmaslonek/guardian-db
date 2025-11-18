use crate::ipfs_log::entry::Entry;
use cid::Cid;

/// Um evento acionado quando novas entradas são adicionadas à fila de replicação.
#[derive(Debug, Clone, PartialEq)]
pub struct EventLoadAdded {
    pub entry: Entry,
    pub hash: Cid,
}

impl EventLoadAdded {
    /// Cria um novo evento EventLoadAdded.
    pub fn new(hash: Cid, entry: Entry) -> Self {
        Self { entry, hash }
    }
}

/// Um evento acionado à medida que as entradas são carregadas (buscadas) da rede no contexto do replicator.
#[derive(Debug, Clone, PartialEq)]
pub struct EventReplicatorProgress {
    pub entry: Entry,
}

impl EventReplicatorProgress {
    /// Cria um novo evento EventReplicatorProgress.
    pub fn new(entry: Entry) -> Self {
        Self { entry }
    }
}

/// Um evento acionado quando um lote de carregamento (load) é concluído.
#[derive(Debug, Clone, PartialEq)]
pub struct EventLoadEnd {
    // Using String IDs instead of Log struct to avoid Debug/Clone trait issues
    pub log_ids: Vec<String>,
}

impl EventLoadEnd {
    /// Cria um novo evento EventLoadEnd.
    pub fn new(log_ids: Vec<String>) -> Self {
        Self { log_ids }
    }
}
