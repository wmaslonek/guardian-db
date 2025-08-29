use crate::stores::replicator::traits::ReplicationInfo as ReplicationInfoTrait;
use std::future::Future;
use tokio::sync::RwLock;

// Struct interna para conter os dados protegidos pelo Lock.
#[derive(Default, Debug, Clone, Copy)]
struct InfoState {
    progress: usize,
    max: usize,
}

/// Refinamento da struct `ReplicationInfo`. Em vez de ser protegida por um
/// Mutex, seus campos internos são atômicos, permitindo modificações
/// concorrentes seguras, assim como na implementação original em Go.
// equivalente a replicationInfo struct em go
/// Implementação thread-safe da interface `ReplicationInfo`.
/// Utiliza um `RwLock` para garantir acesso seguro de múltiplas threads.
#[derive(Default, Debug)]
pub struct ReplicationInfo {
    state: RwLock<InfoState>,
}

// equivalente a NewReplicationInfo em go
impl ReplicationInfo {
    /// Cria uma nova instância de ReplicationInfo.
    pub fn new() -> Self {
        Self::default()
    }
}

// Implementação do trait `ReplicationInfo` para a nossa struct.
// Isso garante que nossa struct está em conformidade com a interface que definimos.
impl ReplicationInfoTrait for ReplicationInfo {
    // equivalente a GetProgress em go
    fn get_progress(&self) -> impl Future<Output = usize> + Send {
        async {
            let state = self.state.read().await;
            state.progress
        }
    }

    // equivalente a GetMax em go
    fn get_max(&self) -> impl Future<Output = usize> + Send {
        async {
            let state = self.state.read().await;
            state.max
        }
    }

    // equivalente a SetProgress em go
    fn set_progress(&self, i: usize) -> impl Future<Output = ()> + Send {
        async move {
            let mut state = self.state.write().await;
            state.progress = i;
        }
    }

    // equivalente a SetMax em go
    fn set_max(&self, i: usize) -> impl Future<Output = ()> + Send {
        async move {
            let mut state = self.state.write().await;
            state.max = i;
        }
    }

    // equivalente a Reset em go
    fn reset(&self) -> impl Future<Output = ()> + Send {
        async move {
            let mut state = self.state.write().await;
            state.progress = 0;
            state.max = 0;
        }
    }
}
