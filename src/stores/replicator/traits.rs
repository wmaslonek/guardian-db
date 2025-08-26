use cid::Cid;
use crate::pubsub::event::EventBus; // Usando nosso EventBus
use std::future::Future;
use ipfs_api_backend_hyper::IpfsClient;
use crate::eqlabs_ipfs_log::{entry::Entry, log::Log, identity::Identity};
use crate::access_controller::traits::AccessController;
// Removido: use crate::stores::base_store::base_store::IpfsLogIo;
// A funcionalidade de I/O está implementada diretamente em Entry::multihash

pub type SortFn = fn(a: &Entry, b: &Entry) -> std::cmp::Ordering;

// equivalente a storeInterface em go
/// Um trait usado para evitar ciclos de importação, definindo a interface
/// que o Replicator espera que uma `Store` implemente.
pub trait StoreInterface: Send + Sync {
    /// Retorna o log de operações (OpLog) da store.
    fn op_log(&self) -> &Log;

    /// Retorna a instância da API do IPFS.
    fn ipfs(&self) -> &IpfsClient;

    /// Retorna a identidade da store.
    fn identity(&self) -> &Identity;

    /// Retorna o controlador de acesso da store.
    fn access_controller(&self) -> &dyn AccessController;

    /// Retorna a função de ordenação para as entradas do log.
    fn sort_fn(&self) -> SortFn;
    
    // Removido: fn io() - A funcionalidade de I/O está implementada
    // diretamente nos métodos Entry::multihash() e Entry::from_multihash()
}

// equivalente a Replicator em go
/// O trait `Replicator` define a API pública para replicar informações
/// de uma store entre pares.
pub trait Replicator {
    /// Para a replicação.
    fn stop(&self) -> impl Future<Output = ()> + Send;

    /// Carrega novas entradas (heads) para replicar.
    fn load(&self, heads: Vec<Box<Entry>>) -> impl Future<Output = ()> + Send;

    /// Retorna a lista de CIDs atualmente na fila de processamento.
    fn get_queue(&self) -> impl Future<Output = Vec<Cid>> + Send;

    /// Retorna o barramento de eventos para escutar os eventos de replicação.
    fn event_bus(&self) -> EventBus;
}

// equivalente a ReplicationInfo em go
/// Mantém informações sobre o estado atual da replicação.
pub trait ReplicationInfo: Send + Sync {
    /// Retorna o progresso atual (número de entradas processadas).
    fn get_progress(&self) -> impl Future<Output = usize> + Send;

    /// Define o progresso atual.
    fn set_progress(&self, i: usize) -> impl Future<Output = ()> + Send;

    /// Retorna o número máximo de entradas a serem processadas.
    fn get_max(&self) -> impl Future<Output = usize> + Send;

    /// Define o número máximo de entradas.
    fn set_max(&self, i: usize) -> impl Future<Output = ()> + Send;

    /// Reinicia todos os valores para 0.
    fn reset(&self) -> impl Future<Output = ()> + Send;
}