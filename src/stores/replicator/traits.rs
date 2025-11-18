//! # Traits do Módulo Replicator
//!
//! Este módulo define as interfaces fundamentais para o sistema de replicação
//! do GuardianDB. Ele fornece abstrações que permitem a sincronização de dados
//! entre diferentes instâncias de stores na rede distribuída.
//!
//! ## Arquitetura
//!
//! O sistema de replicação é baseado em três traits principais:
//!
//! 1. **`StoreInterface`**: Define como o replicador acessa dados da store
//! 2. **`Replicator`**: Interface pública para operações de replicação
//! 3. **`ReplicationInfo`**: Rastreamento do progresso de replicação

use crate::access_controller::traits::AccessController;
use crate::ipfs_core_api::client::IpfsClient;
use crate::ipfs_log::{entry::Entry, identity::Identity, log::Log};
use crate::p2p::events::EventBus;
use cid::Cid;
use std::future::Future;
use std::sync::Arc;

/// Tipo de função para ordenar entradas no log.
///
/// Define como duas entradas devem ser comparadas durante operações
/// de ordenação no processo de replicação.
///
/// # Parâmetros
/// - `a`: Primeira entrada para comparação
/// - `b`: Segunda entrada para comparação
///
/// # Retorna
/// `std::cmp::Ordering` indicando a relação de ordem entre as entradas
pub type SortFn = fn(a: &Entry, b: &Entry) -> std::cmp::Ordering;

/// Resultado padrão para operações de replicação
pub type ReplicationResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Hash de entrada usado para identificação única
pub type EntryHash = String;

/// Um trait usado para evitar ciclos de importação, definindo a interface
/// que o Replicator espera que uma `Store` implemente.
///
/// Esta interface abstrai as operações essenciais que o replicador
/// precisa para acessar e manipular dados de uma store.
pub trait StoreInterface: Send + Sync {
    /// Retorna uma referência compartilhada ao log de operações (OpLog) da store.
    /// Este log contém todas as entradas ordenadas cronologicamente.
    /// Usando Arc<RwLock<Log>> para compatibilidade com arquitetura thread-safe.
    fn op_log_arc(&self) -> Arc<parking_lot::RwLock<Log>>;

    /// Retorna a instância da API do IPFS.
    /// Usado para buscar dados distribuídos da rede IPFS.
    fn ipfs(&self) -> &IpfsClient;

    /// Retorna a identidade da store.
    /// Identifica unicamente esta instância na rede.
    fn identity(&self) -> &Identity;

    /// Retorna o controlador de acesso da store.
    /// Gerencia permissões e autorização para operações.
    fn access_controller(&self) -> &dyn AccessController;

    /// Retorna a função de ordenação para as entradas do log.
    /// Define como as entradas devem ser ordenadas durante a replicação.
    fn sort_fn(&self) -> SortFn;
}

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

    /// Determina se um hash específico deve ser excluído da replicação.
    /// Retorna `true` se o hash já existe no log ou foi processado com sucesso.
    fn should_exclude(&self, hash: &Cid) -> impl Future<Output = bool> + Send;
}

/// Mantém informações sobre o estado atual da replicação.
///
/// Este trait fornece uma interface thread-safe para rastrear
/// o progresso da replicação entre diferentes instâncias de store.
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
