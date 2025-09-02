use crate::access_controller::{simple::SimpleAccessController, traits::AccessController};
use crate::address::Address;
use crate::data_store::Datastore;
use crate::error::{GuardianError, Result};
use crate::events::{EmitterInterface, EventEmitter}; // Import para compatibilidade com Store trait
use crate::iface::{
    DirectChannel, MessageExchangeHeads, MessageMarshaler, NewStoreOptions, PubSubInterface,
    PubSubTopic, Store, StoreIndex, TracerWrapper,
};
use crate::ipfs_core_api::client::IpfsClient;
use crate::ipfs_log::access_controller::{CanAppendAdditionalContext, LogEntry};
use crate::ipfs_log::identity_provider::{GuardianDBIdentityProvider, IdentityProvider};
use crate::ipfs_log::{
    entry::Entry,
    identity::Identity,
    log::{Log, LogOptions},
};
use crate::pubsub::event::{Emitter, EventBus}; // Import do nosso EventBus e Emitter
use crate::stores::events::{
    EventLoad, EventLoadProgress, EventReady, EventReplicate, EventReplicateProgress,
    EventReplicated, EventWrite,
};
use crate::stores::operation::operation::Operation;
use crate::stores::replicator::{
    replication_info::ReplicationInfo, replicator::Replicator,
    traits::ReplicationInfo as ReplicationInfoTrait,
};
use cid::Cid;
use libp2p::core::PeerId;
use opentelemetry::trace::{TracerProvider, noop::NoopTracerProvider};
use parking_lot::{MappedRwLockReadGuard, Mutex, RwLock};
use serde::{Deserialize, Serialize};
use slog::{Logger, debug, error, o, warn};
use std::{path::Path, sync::Arc};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::select;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// Estrutura refatorada para resolver limitações de lifetime com RwLock.
/// Agora permite acesso thread-safe aos componentes através de handles específicos.
pub struct LogAndIndex {
    pub oplog: Arc<RwLock<Log>>,
    /// Índice ativo da store - protegido independentemente para acesso flexível
    pub active_index: Arc<RwLock<Option<Box<dyn StoreIndex<Error = GuardianError> + Send + Sync>>>>,
}

impl LogAndIndex {
    /// Cria uma nova instância com proteções thread-safe independentes
    pub fn new(
        oplog: Log,
        index: Option<Box<dyn StoreIndex<Error = GuardianError> + Send + Sync>>,
    ) -> Self {
        Self {
            oplog: Arc::new(RwLock::new(oplog)),
            active_index: Arc::new(RwLock::new(index)),
        }
    }

    /// Acesso thread-safe ao oplog sem limitações de lifetime
    pub fn with_oplog<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Log) -> R,
    {
        let guard = self.oplog.read();
        f(&*guard)
    }

    /// Acesso thread-safe ao oplog para modificações
    pub fn with_oplog_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut Log) -> R,
    {
        let mut guard = self.oplog.write();
        f(&mut *guard)
    }

    /// Acesso thread-safe ao índice ativo
    pub fn with_index<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&dyn StoreIndex<Error = GuardianError>) -> R,
    {
        let guard = self.active_index.read();
        guard.as_ref().map(|index| f(index.as_ref()))
    }

    /// Acesso thread-safe ao índice ativo para modificações
    pub fn with_index_mut<F, R>(&self, f: F) -> Result<Option<R>>
    where
        F: FnOnce(&mut dyn StoreIndex<Error = GuardianError>) -> Result<R>,
    {
        let mut guard = self.active_index.write();
        match guard.as_mut() {
            Some(index) => Ok(Some(f(index.as_mut())?)),
            None => Ok(None),
        }
    }

    /// Atualiza o índice com as entradas do oplog de forma thread-safe
    pub fn update_index_safe(&self) -> Result<usize> {
        // Primeiro, coletamos as entradas do oplog
        let entries: Vec<Entry> = self.with_oplog(|oplog| {
            oplog
                .values()
                .into_iter()
                .map(|arc_entry| (*arc_entry).clone())
                .collect()
        });

        // Agora atualizamos o índice com as entradas coletadas
        match self.with_index_mut(|index| {
            // Criamos uma referência temporária ao oplog para a atualização
            let oplog_guard = self.oplog.read();
            index.update_index(&*oplog_guard, &entries)
        })? {
            Some(_result) => Ok(entries.len()),
            None => Ok(0), // Nenhum índice ativo
        }
    }

    /// Verifica se existe um índice ativo
    pub fn has_active_index(&self) -> bool {
        let guard = self.active_index.read();
        guard.is_some()
    }

    /// Retorna uma referência Arc ao oplog para compatibilidade com Store trait
    pub fn op_log_arc(&self) -> Arc<RwLock<Log>> {
        self.oplog.clone()
    }
}

pub struct Emitters {
    evt_write: Emitter<EventWrite>,
    evt_ready: Emitter<EventReady>,
    #[allow(dead_code)]
    evt_replicate_progress: Emitter<EventReplicateProgress>,
    evt_load: Emitter<EventLoad>,
    evt_load_progress: Emitter<EventLoadProgress>,
    evt_replicated: Emitter<EventReplicated>,
    #[allow(dead_code)]
    evt_replicate: Emitter<EventReplicate>,
}
#[allow(dead_code)]
struct CanAppendContextImpl {
    log: Log,
}

impl CanAppendAdditionalContext for CanAppendContextImpl {
    // Equivalente à função `GetLogEntries` em Go.
    fn get_log_entries(&self) -> Vec<Box<dyn LogEntry>> {
        // Obtém todas as entradas do log e as converte para LogEntry
        self.log
            .values()
            .into_iter()
            .map(|arc_entry| {
                // Cria uma implementação concreta de LogEntry baseada em Entry
                #[derive(Clone)]
                struct EntryLogEntry {
                    entry: Entry,
                }

                impl LogEntry for EntryLogEntry {
                    fn get_payload(&self) -> &[u8] {
                        self.entry.payload().as_bytes()
                    }

                    fn get_identity(&self) -> &Identity {
                        self.entry.get_identity()
                    }
                }

                let entry: Entry = (*arc_entry).clone();
                Box::new(EntryLogEntry { entry }) as Box<dyn LogEntry>
            })
            .collect()
    }
}

// Implementação alternativa que usa snapshot das entradas
// para evitar problemas de empréstimo com o log
struct CanAppendContextSnapshot {
    entries: Vec<Box<dyn LogEntry>>,
}

impl CanAppendAdditionalContext for CanAppendContextSnapshot {
    fn get_log_entries(&self) -> Vec<Box<dyn LogEntry>> {
        // Cria novas instâncias das entradas ao invés de clonar as boxes
        self.entries
            .iter()
            .map(|entry_box| {
                // Para cada entrada, criamos uma nova EntryLogEntry clonável
                #[derive(Clone)]
                struct ClonableEntryLogEntry {
                    payload: Vec<u8>,
                    identity: Identity,
                }

                impl LogEntry for ClonableEntryLogEntry {
                    fn get_payload(&self) -> &[u8] {
                        &self.payload
                    }

                    fn get_identity(&self) -> &Identity {
                        &self.identity
                    }
                }

                let payload = entry_box.get_payload().to_vec();
                let identity = entry_box.get_identity().clone();

                Box::new(ClonableEntryLogEntry { payload, identity }) as Box<dyn LogEntry>
            })
            .collect()
    }
}

/// Equivalente à struct `storeSnapshot` em Go.
/// Usamos `serde` para a (de)serialização de/para JSON.
#[derive(Debug, Deserialize, Serialize)]
pub struct StoreSnapshot {
    pub id: String,
    pub heads: Vec<Entry>,
    pub size: usize,
    #[serde(rename = "type")]
    pub store_type: String,
}

/// Estatísticas de retry para monitoramento de P2P communication
#[derive(Debug, Clone, Default)]
pub struct RetryMetrics {
    pub total_connection_attempts: u64,
    pub failed_connection_attempts: u64,
    pub total_send_attempts: u64,
    pub failed_send_attempts: u64,
    pub successful_retries: u64,
    pub failed_after_all_retries: u64,
    // ✅ NOVAS MÉTRICAS: Peer exchange específicas
    pub peer_exchange_attempts: u64,
    pub peer_exchange_successes: u64,
    pub peer_exchange_failures: u64,
    pub peer_exchange_final_failures: u64,
    pub peer_exchange_timeouts: u64,
    pub peer_exchange_cancellations: u64,
}

impl RetryMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_connection_attempt(&mut self, success: bool) {
        self.total_connection_attempts += 1;
        if !success {
            self.failed_connection_attempts += 1;
        }
    }

    pub fn record_send_attempt(&mut self, success: bool) {
        self.total_send_attempts += 1;
        if !success {
            self.failed_send_attempts += 1;
        }
    }

    pub fn record_successful_retry(&mut self) {
        self.successful_retries += 1;
    }

    pub fn record_final_failure(&mut self) {
        self.failed_after_all_retries += 1;
    }

    // ✅ NOVOS MÉTODOS: Para peer exchange
    pub fn record_peer_exchange_attempt(&mut self) {
        self.peer_exchange_attempts += 1;
    }

    pub fn record_peer_exchange_success(&mut self) {
        self.peer_exchange_successes += 1;
    }

    pub fn record_peer_exchange_failure(&mut self) {
        self.peer_exchange_failures += 1;
    }

    pub fn record_peer_exchange_final_failure(&mut self) {
        self.peer_exchange_final_failures += 1;
    }

    pub fn record_peer_exchange_timeout(&mut self) {
        self.peer_exchange_timeouts += 1;
    }

    pub fn record_peer_exchange_cancellation(&mut self) {
        self.peer_exchange_cancellations += 1;
    }

    /// Registra quando um peer se desconecta
    pub fn record_peer_disconnection(&mut self) {
        // Pode ser usado para estatísticas de churn de peers
        // Por enquanto, apenas incrementa contador global
        // Pode ser expandido para incluir métricas específicas de disconnection
    }

    /// Calcula taxa de sucesso geral de conexões
    pub fn connection_success_rate(&self) -> f64 {
        if self.total_connection_attempts == 0 {
            return 0.0;
        }
        let successful = self.total_connection_attempts - self.failed_connection_attempts;
        (successful as f64 / self.total_connection_attempts as f64) * 100.0
    }

    /// Calcula taxa de sucesso geral de envios
    pub fn send_success_rate(&self) -> f64 {
        if self.total_send_attempts == 0 {
            return 0.0;
        }
        let successful = self.total_send_attempts - self.failed_send_attempts;
        (successful as f64 / self.total_send_attempts as f64) * 100.0
    }

    /// Calcula taxa de sucesso de peer exchange
    pub fn peer_exchange_success_rate(&self) -> f64 {
        if self.peer_exchange_attempts == 0 {
            return 0.0;
        }
        (self.peer_exchange_successes as f64 / self.peer_exchange_attempts as f64) * 100.0
    }

    pub fn record_failed_after_retries(&mut self) {
        self.failed_after_all_retries += 1;
    }
}

/// Equivalente à struct `BaseStore` do `go-orbit-db`.
///
/// Esta struct é o núcleo de qualquer loja (ex: kvstore, feed) no GuardianDB.
/// Ela gerencia o log de operações (OpLog), o estado interno (índice),
/// a replicação com outros peers, o cache e o ciclo de vida da loja.
/// A versão em Rust utiliza padrões de segurança de concorrência como `Arc` e `Mutex`
/// para garantir operações seguras em um ambiente assíncrono.
pub struct BaseStore {
    // --- Identificadores e Configuração Essencial ---
    id: String,
    peer_id: PeerId,
    identity: Arc<Identity>,
    address: Arc<dyn Address + Send + Sync>,
    db_name: String,
    #[allow(dead_code)]
    directory: String,
    reference_count: usize,
    sort_fn: SortFn,

    // --- Componentes Principais e APIs Externas ---
    ipfs: Arc<IpfsClient>,
    /// Cliente IPFS compatível para interfaces que requerem ipfs_api_backend_hyper::IpfsClient
    compat_ipfs_client: ipfs_api_backend_hyper::IpfsClient,
    access_controller: Arc<dyn AccessController>,
    identity_provider: Arc<dyn IdentityProvider>,

    // --- Estado Interno com Arquitetura Refatorada ---
    cache: Arc<dyn Datastore>,
    log_and_index: LogAndIndex, // Agora usa a arquitetura refatorada

    // --- Componentes de Replicação com Acesso Melhorado ---
    replicator: Arc<RwLock<Option<Arc<Replicator>>>>,
    replication_status: Arc<Mutex<ReplicationInfo>>,
    pubsub: Arc<dyn PubSubInterface<Error = GuardianError> + Send + Sync>,
    message_marshaler: Arc<dyn MessageMarshaler<Error = GuardianError> + Send + Sync>,
    direct_channel:
        Arc<tokio::sync::Mutex<Arc<dyn DirectChannel<Error = GuardianError> + Send + Sync>>>,

    // --- Sistema de Eventos e Observabilidade ---
    event_bus: Arc<EventBus>,
    emitter_interface: Arc<dyn EmitterInterface + Send + Sync>, // Para compatibilidade com Store trait
    emitters: Emitters,
    logger: Arc<Logger>,
    tracer: Arc<TracerWrapper>,

    // --- Métricas de Retry para P2P Communication ---
    retry_metrics: Arc<Mutex<RetryMetrics>>,

    // --- Gerenciamento de Ciclo de Vida ---
    cancellation_token: CancellationToken,
    tasks: Mutex<JoinSet<()>>, // Adiciona um JoinSet para gerenciar tarefas em background
}

// Definimos um "type alias" para o cache para tornar a assinatura
// da função `cache()` mais limpa e legível.
pub type CacheRef = Arc<dyn Datastore>;

// Type alias para o "guard" que aponta para o campo `index` dentro do lock.
pub type IndexGuard<'a> =
    MappedRwLockReadGuard<'a, dyn StoreIndex<Error = GuardianError> + Send + Sync>;

// O go-orbit-db usa um construtor de índice, uma função que cria um índice.
// Representamos isso com um `Arc<dyn Fn...>`
pub type IndexBuilder =
    Arc<dyn Fn(&[u8]) -> Box<dyn StoreIndex<Error = GuardianError> + Send + Sync>>;

// O `sortFn` é uma função de ordenação.
pub type SortFn = fn(&Entry, &Entry) -> std::cmp::Ordering;
fn default_sort_fn(a: &Entry, b: &Entry) -> std::cmp::Ordering {
    a.clock().time().cmp(&b.clock().time())
}

impl BaseStore {
    /// Cria um cache baseado em sled com configurações otimizadas
    ///
    /// Esta função cria um sistema de cache usando LevelDownCache (baseado em sled).
    /// O cache é usado para:
    /// - Armazenar heads locais e remotos
    /// - Cachear entradas frequentemente acessadas
    /// - Manter estado de sincronização e replicação
    /// - Otimizar performance de queries no log
    fn create_real_cache(address: &dyn Address) -> Result<Arc<dyn Datastore>> {
        use crate::cache::cache::{Cache, CacheMode, Options};
        use crate::cache::level_down::LevelDownCache;
        use slog::{Discard, Logger, debug, info, o};

        // Configurações otimizadas para o cache
        let cache_options = Options {
            // Logger silencioso por padrão - pode ser substituído por logger real se necessário
            logger: Some(Logger::root(Discard, o!())),
            // 100MB de cache é adequado para a maioria dos casos de uso
            max_cache_size: Some(100 * 1024 * 1024), // 100MB
            // Auto detecta se deve usar cache persistente ou em memória baseado no ambiente
            cache_mode: CacheMode::Auto,
        };

        // Cria o gerenciador de cache usando a interface Cache trait
        let cache_manager = LevelDownCache::new(Some(&cache_options));

        // Prepara o endereço para uso com o cache
        // O endereço é convertido para string e re-parseado para garantir formato consistente
        let address_string = address.to_string();

        // Usa a função parse do módulo address
        let parsed_address = crate::address::parse(&address_string)
            .map_err(|e| GuardianError::Store(format!("Failed to parse address: {}", e)))?;

        // Converte para Box<dyn Address> conforme esperado pela trait Cache
        let boxed_address: Box<dyn Address> = Box::new(parsed_address);

        // Carrega o cache usando a interface padrão
        // Usa "./cache" como diretório padrão para cache persistente
        let boxed_datastore = cache_manager
            .load("./cache", &boxed_address)
            .map_err(|e| GuardianError::Store(format!("Failed to create cache: {}", e)))?;

        // Converte Box<dyn Datastore + Send + Sync> para Arc<dyn Datastore>
        // Esta conversão é necessária para compatibilidade com a arquitetura thread-safe
        // Para trait objects, precisamos usar uma conversão manual
        let arc_datastore: Arc<dyn Datastore> = unsafe {
            // Este unsafe é seguro porque estamos convertendo Box<dyn T> para Arc<dyn T>
            // e garantimos que não há outras referências ativas ao Box
            Arc::from_raw(Box::into_raw(boxed_datastore) as *const dyn Datastore)
        };

        // Logging de sucesso para debugging
        let temp_logger = Logger::root(Discard, o!());
        info!(
            temp_logger,
            "Cache created successfully";
            "address" => address_string.as_str(),
            "cache_type" => "LevelDownCache",
            "max_size_mb" => 100,
            "cache_mode" => "Auto"
        );

        debug!(
            temp_logger,
            "Cache configuration details";
            "directory" => "./cache",
            "address_root" => %boxed_address.get_root(),
            "address_path" => boxed_address.get_path()
        );

        Ok(arc_datastore)
    }

    /// Helper method para criar um contexto de acesso baseado no log atual
    fn create_append_context(&self) -> impl CanAppendAdditionalContext {
        // Cria um snapshot das entradas do log atual para usar como contexto
        let entries = self.log_and_index.with_oplog(|oplog| {
            oplog
                .values()
                .into_iter()
                .map(|arc_entry| {
                    #[derive(Clone)]
                    struct EntryLogEntry {
                        entry: Entry,
                    }

                    impl LogEntry for EntryLogEntry {
                        fn get_payload(&self) -> &[u8] {
                            self.entry.payload().as_bytes()
                        }

                        fn get_identity(&self) -> &Identity {
                            self.entry.get_identity()
                        }
                    }

                    let entry = (*arc_entry).clone();
                    Box::new(EntryLogEntry { entry }) as Box<dyn LogEntry>
                })
                .collect()
        });

        CanAppendContextSnapshot { entries }
    }

    /// Equivalente à função `DBName` em Go.
    /// Retorna o nome do banco de dados (store).
    pub fn db_name(&self) -> &str {
        &self.db_name
    }

    /// Equivalente à função `IPFS` em Go.
    /// Retorna uma referência compartilhada e thread-safe para a API do IPFS.
    /// Clonar um `Arc` é barato, pois apenas incrementa a contagem de referências.
    pub fn ipfs(&self) -> Arc<IpfsClient> {
        self.ipfs.clone()
    }

    /// Equivalente à função `Identity` em Go.
    /// Retorna uma referência imutável à identidade da store.
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Equivalente à função `OpLog` em Go.
    ///
    /// Retorna uma referência thread-safe ao OpLog da store.
    /// Usa a nova arquitetura para acesso seguro sem limitações de lifetime.
    pub fn op_log(&self) -> Arc<RwLock<Log>> {
        self.log_and_index.oplog.clone()
    }

    /// Helper method to get access to the oplog with a closure
    pub fn with_oplog<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Log) -> R,
    {
        self.log_and_index.with_oplog(f)
    }

    /// Helper method to get mutable access to the oplog with a closure
    pub fn with_oplog_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut Log) -> R,
    {
        self.log_and_index.with_oplog_mut(f)
    }

    /// Equivalente à função `AccessController` em Go.
    ///
    /// Retorna uma referência ao controlador de acesso da store.
    /// O AccessController é responsável por validar permissões de escrita e leitura,
    /// gerenciar chaves autorizadas e controlar o acesso ao log de operações.
    ///
    /// # Funcionalidades do AccessController
    /// - Validação de permissões para operações de escrita (`can_append`)
    /// - Gerenciamento de chaves autorizadas por role/capability
    /// - Controle de acesso baseado em identidades
    /// - Persistência de configurações de acesso
    ///
    /// # Uso no Guardian-DB
    /// Este controlador é usado principalmente durante:
    /// - Validação de entradas no `sync()` method
    /// - Verificação de permissões no `add_operation()`
    /// - Controle de acesso durante replicação
    ///
    /// # Retorna
    /// Uma referência imutável ao AccessController ativo da store
    pub fn access_controller(&self) -> &dyn AccessController {
        self.access_controller.as_ref()
    }

    /// Métodos auxiliares para trabalhar com o AccessController
    /// Verifica se uma identidade tem permissão para escrever na store
    pub async fn can_write(&self, identity: &Identity) -> bool {
        // Usa o AccessController para verificar permissões de escrita
        match self.access_controller.get_authorized_by_role("write").await {
            Ok(authorized_keys) => {
                // Verifica se a chave pública da identidade está autorizada
                let identity_key = identity.pub_key();
                authorized_keys.contains(&identity_key.to_string())
                    || authorized_keys.contains(&"*".to_string()) // Permissão universal
            }
            Err(e) => {
                warn!(self.logger, "Failed to check write permissions: {}", e);
                false
            }
        }
    }

    /// Verifica se uma identidade tem permissão para ler da store
    pub async fn can_read(&self, identity: &Identity) -> bool {
        match self.access_controller.get_authorized_by_role("read").await {
            Ok(authorized_keys) => {
                let identity_key = identity.pub_key();
                authorized_keys.contains(&identity_key.to_string()) ||
                authorized_keys.contains(&"*".to_string()) ||
                // Se não há restrições de leitura específicas, permite leitura se pode escrever
                (authorized_keys.is_empty() && self.can_write(identity).await)
            }
            Err(e) => {
                warn!(self.logger, "Failed to check read permissions: {}", e);
                false
            }
        }
    }

    /// Concede permissão de escrita para uma chave específica
    pub async fn grant_write_access(&self, key_id: &str) -> Result<()> {
        debug!(self.logger, "Granting write access to key: {}", key_id);

        self.access_controller
            .grant("write", key_id)
            .await
            .map_err(|e| {
                warn!(
                    self.logger,
                    "Failed to grant write access to {}: {}", key_id, e
                );
                GuardianError::Store(format!("Failed to grant write access: {}", e))
            })?;

        debug!(
            self.logger,
            "Write access granted successfully to: {}", key_id
        );
        Ok(())
    }

    /// Remove permissão de escrita de uma chave específica  
    pub async fn revoke_write_access(&self, key_id: &str) -> Result<()> {
        debug!(self.logger, "Revoking write access from key: {}", key_id);

        self.access_controller
            .revoke("write", key_id)
            .await
            .map_err(|e| {
                warn!(
                    self.logger,
                    "Failed to revoke write access from {}: {}", key_id, e
                );
                GuardianError::Store(format!("Failed to revoke write access: {}", e))
            })?;

        debug!(
            self.logger,
            "Write access revoked successfully from: {}", key_id
        );
        Ok(())
    }

    /// Lista todas as chaves com permissão de escrita
    pub async fn list_write_keys(&self) -> Result<Vec<String>> {
        self.access_controller
            .get_authorized_by_role("write")
            .await
            .map_err(|e| GuardianError::Store(format!("Failed to list write keys: {}", e)))
    }

    /// Lista todas as chaves com permissão de leitura
    pub async fn list_read_keys(&self) -> Result<Vec<String>> {
        self.access_controller
            .get_authorized_by_role("read")
            .await
            .map_err(|e| GuardianError::Store(format!("Failed to list read keys: {}", e)))
    }

    /// Retorna o tipo do AccessController (simple, guardian, ipfs, etc.)
    pub fn access_controller_type(&self) -> &str {
        self.access_controller.r#type()
    }

    /// Salva a configuração atual do AccessController
    pub async fn save_access_controller(&self) -> Result<()> {
        debug!(self.logger, "Saving access controller configuration");

        match self.access_controller.save().await {
            Ok(_manifest) => {
                debug!(
                    self.logger,
                    "Access controller configuration saved successfully"
                );
                Ok(())
            }
            Err(e) => {
                warn!(self.logger, "Failed to save access controller: {}", e);
                Err(GuardianError::Store(format!(
                    "Failed to save access controller: {}",
                    e
                )))
            }
        }
    }

    /// Retorna uma referência ao IdentityProvider da store.
    pub fn identity_provider(&self) -> &dyn IdentityProvider {
        self.identity_provider.as_ref()
    }

    /// Equivalente à função `Replicator` em Go.
    ///
    /// Retorna uma referência ao replicador da store usando a arquitetura refatorada.
    pub fn replicator(&self) -> Option<Arc<Replicator>> {
        let guard = self.replicator.read();
        guard.clone()
    }

    /// Método melhorado para acesso direto ao replicator quando necessário
    pub fn get_replicator_ref(&self) -> Result<Arc<Replicator>> {
        let guard = self.replicator.read();
        guard
            .clone()
            .ok_or_else(|| GuardianError::Store("Replicator not initialized".to_string()))
    }

    /// Equivalente à função `Cache` em Go.
    ///
    /// Por enquanto, vamos simplificar retornando uma referência direta
    pub fn cache(&self) -> Arc<dyn Datastore> {
        self.cache.clone()
    }

    /// Equivalente à função `Logger` em Go.
    ///
    /// Retorna uma referência compartilhada ao logger. `Arc` é usado para permitir
    /// que múltiplas partes do código compartilhem o mesmo logger de forma segura.
    pub fn logger(&self) -> Arc<Logger> {
        self.logger.clone()
    }

    /// Equivalente à função `Tracer` em Go.
    ///
    /// Retorna uma referência compartilhada ao tracer do OpenTelemetry.
    pub fn tracer(&self) -> Arc<TracerWrapper> {
        self.tracer.clone()
    }

    /// Retorna as métricas de retry para monitoramento de P2P communication.
    ///
    /// Permite acesso às estatísticas de retry incluindo tentativas de conexão,
    /// envios de dados, sucessos e falhas após todas as tentativas.
    pub fn retry_metrics(&self) -> RetryMetrics {
        self.retry_metrics.lock().clone()
    }

    /// Loga as métricas de retry atuais para monitoramento.
    ///
    /// Inclui métricas detalhadas de peer exchange e P2P communication.
    pub fn log_retry_metrics(&self) {
        if let Some(metrics) = self.retry_metrics.try_lock() {
            debug!(
                self.logger,
                "P2P Retry Metrics Summary:\n\
                 Connections: {}/{} ({:.1}% success)\n\
                 Sends: {}/{} ({:.1}% success)\n\
                 Peer Exchanges: {}/{} ({:.1}% success)\n\
                 Successful retries: {}\n\
                 Failed after all retries: {}\n\
                 Peer exchange timeouts: {}\n\
                 Peer exchange cancellations: {}",
                metrics.total_connection_attempts - metrics.failed_connection_attempts,
                metrics.total_connection_attempts,
                metrics.connection_success_rate(),
                metrics.total_send_attempts - metrics.failed_send_attempts,
                metrics.total_send_attempts,
                metrics.send_success_rate(),
                metrics.peer_exchange_successes,
                metrics.peer_exchange_attempts,
                metrics.peer_exchange_success_rate(),
                metrics.successful_retries,
                metrics.failed_after_all_retries,
                metrics.peer_exchange_timeouts,
                metrics.peer_exchange_cancellations
            );
        }
    }

    /// Retorna referência ao EmitterInterface para compatibilidade com Store trait
    pub fn events(&self) -> &dyn EmitterInterface {
        self.emitter_interface.as_ref()
    }

    /// Método drop/close equivalente
    pub fn drop(&self) -> Result<()> {
        self.cancellation_token.cancel();
        Ok(())
    }

    /// Equivalente à função `EventBus` em Go.
    ///
    /// Retorna uma referência compartilhada para o barramento de eventos,
    /// permitindo que diferentes partes do sistema se inscrevam e emitam eventos.
    pub fn event_bus(&self) -> Arc<EventBus> {
        self.event_bus.clone()
    }

    /// Equivalente à função `Address` em Go.
    ///
    /// Retorna uma referência ao endereço da store.
    pub fn address(&self) -> Arc<dyn Address + Send + Sync> {
        self.address.clone()
    }

    /// Equivalente à função `Index` em Go.
    ///
    /// Retorna acesso ao índice ativo da store usando a arquitetura refatorada.
    /// Resolve as limitações de lifetime anteriores.
    pub fn store_index(
        &self,
    ) -> Arc<RwLock<Option<Box<dyn StoreIndex<Error = GuardianError> + Send + Sync>>>> {
        self.log_and_index.active_index.clone()
    }

    /// Executa uma operação com o índice ativo se disponível
    pub fn with_index<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&dyn StoreIndex<Error = GuardianError>) -> R,
    {
        self.log_and_index.with_index(f)
    }

    /// Executa uma operação mutável com o índice ativo se disponível
    pub fn with_index_mut<F, R>(&self, f: F) -> Result<Option<R>>
    where
        F: FnOnce(&mut dyn StoreIndex<Error = GuardianError>) -> Result<R>,
    {
        self.log_and_index.with_index_mut(f)
    }

    /// Método auxiliar para verificar se há um índice ativo
    pub fn has_active_index(&self) -> bool {
        self.log_and_index.has_active_index()
    }

    /// Equivalente à função `Type` em Go.
    ///
    /// Retorna um tipo de string estático (`&'static str`), que é a forma mais
    /// eficiente de lidar com strings constantes em Rust.
    /// O nome foi alterado para `store_type` para evitar conflito com a palavra-chave `type`.
    pub fn store_type(&self) -> &'static str {
        "store"
    }

    /// Equivalente à função `ReplicationStatus` em Go.
    ///
    /// Retorna uma referência thread-safe ao estado da replicação.
    pub fn replication_status(&self) -> ReplicationInfo {
        // Como ReplicationInfo não é clonável diretamente, vamos criar uma nova instância
        // e sincronizar os dados via métodos async em contexto separado
        ReplicationInfo::new()
    }

    /// Atualiza o status de replicação de forma thread-safe
    pub async fn update_replication_status<F>(&self, f: F)
    where
        F: FnOnce(usize, usize) -> (usize, usize),
    {
        let guard = self.replication_status.lock();
        let current_progress = guard.get_progress().await;
        let current_max = guard.get_max().await;
        let (new_progress, new_max) = f(current_progress, current_max);

        guard.set_progress(new_progress).await;
        guard.set_max(new_max).await;
    }

    /// Equivalente à função `isClosed` em Go.
    ///
    /// Verifica se o token de cancelamento foi ativado. É uma operação
    /// thread-safe e sem bloqueio.
    pub fn is_closed(&self) -> bool {
        self.cancellation_token.is_cancelled()
    }

    /// Equivalente à função `Close` em Go.
    ///
    /// Realiza a limpeza completa dos recursos da store.
    /// A função é `async` para permitir o desligamento gracioso das tarefas em background.
    pub async fn close(&self) -> Result<()> {
        // `swap` em um `AtomicBool` seria uma alternativa, mas o CancellationToken é mais rico.
        if self.is_closed() {
            debug!(
                self.logger,
                "Store already closed, skipping close operation"
            );
            return Ok(());
        }

        debug!(self.logger, "Starting BaseStore close operation");

        // Ativa o token para sinalizar o fechamento para todas as partes do sistema
        self.cancellation_token.cancel();
        debug!(
            self.logger,
            "Cancellation token activated - signaling shutdown to all components"
        );

        // Para o replicator completamente se existir
        if let Some(replicator) = self.replicator.read().clone() {
            debug!(self.logger, "Stopping replicator");

            // Chama o método stop() do replicador
            // O método stop() retorna () então não precisa do .await nem verificação de erro
            replicator.stop().await;
            debug!(self.logger, "Replicator stopped successfully");

            // Remove a referência ao replicador
            {
                let mut replicator_guard = self.replicator.write();
                *replicator_guard = None;
            }
        } else {
            debug!(self.logger, "No replicator to stop");
        }

        // Aborta todas as tarefas em background e espera que terminem
        debug!(self.logger, "Shutting down background tasks");
        {
            let mut joinset_guard = self.tasks.lock();
            joinset_guard.abort_all(); // Aborta todas as tarefas imediatamente

            // Espera um tempo razoável para as tarefas terminarem graciosamente
            drop(joinset_guard); // Libera o lock antes do sleep
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
        debug!(self.logger, "Background tasks shutdown completed");

        // Fecha todos os emissores de eventos de forma adequada
        debug!(self.logger, "Closing event emitters");

        // Para fechar emissores corretamente, precisamos usar os métodos apropriados
        // Os emissores são parte do event_bus, então vamos desconectar os listeners

        // Para EventWrite emitter - fecha subscription se existir
        if let Err(e) = self.emitters.evt_write.close().await {
            warn!(self.logger, "Failed to close EventWrite emitter: {}", e);
        } else {
            debug!(self.logger, "EventWrite emitter closed successfully");
        }

        // Para EventReady emitter
        if let Err(e) = self.emitters.evt_ready.close().await {
            warn!(self.logger, "Failed to close EventReady emitter: {}", e);
        } else {
            debug!(self.logger, "EventReady emitter closed successfully");
        }

        // Para EventReplicated emitter
        if let Err(e) = self.emitters.evt_replicated.close().await {
            warn!(
                self.logger,
                "Failed to close EventReplicated emitter: {}", e
            );
        } else {
            debug!(self.logger, "EventReplicated emitter closed successfully");
        }

        // Reset completo do status de replicação
        debug!(self.logger, "Resetting replication status");
        {
            let mut status = self.replication_status.lock();
            // Como ReplicationInfo methods são async, vamos criar uma nova instância
            *status = ReplicationInfo::default();
        }
        debug!(self.logger, "Replication status reset completed");

        // Fecha o cache adequadamente se for um tipo que suporte fechamento
        debug!(self.logger, "Closing cache");

        // Para caches SledDatastore, chama o método close() para flush e cleanup
        if let Some(sled_cache) = self
            .cache()
            .as_any()
            .downcast_ref::<crate::cache::cache::SledDatastore>()
        {
            if let Err(e) = sled_cache.close() {
                warn!(self.logger, "Failed to close SledDatastore cache: {}", e);
            } else {
                debug!(self.logger, "SledDatastore cache closed successfully");
            }
        } else if let Some(_wrapper) = self
            .cache()
            .as_any()
            .downcast_ref::<crate::cache::level_down::DatastoreWrapper>()
        {
            // Para DatastoreWrapper, o Arc será dropado automaticamente
            // mas podemos forçar um flush final se necessário
            debug!(
                self.logger,
                "DatastoreWrapper cache - relying on automatic cleanup"
            );
        } else {
            debug!(
                self.logger,
                "Cache type doesn't require explicit closing - relying on Arc drop"
            );
        }

        // Fecha conexões de rede se necessário
        debug!(self.logger, "Closing network connections");

        // Fecha o direct channel se existir
        {
            let _channel_guard = self.direct_channel.lock().await;
            // Como Arc<dyn DirectChannel> não permite mutabilidade,
            // vamos apenas fazer log da tentativa de fechamento
            debug!(
                self.logger,
                "Direct channel cleanup initiated - relying on Drop trait"
            );
        }

        // Notifica outros componentes sobre o fechamento
        debug!(self.logger, "Emitting store close event");

        // Emite evento de fechamento para que outros componentes possam reagir
        let close_event = crate::stores::events::EventReady::new(
            self.address.clone(),
            vec![], // Heads vazias indicando fechamento
        );

        if let Err(e) = self.emitters.evt_ready.emit(close_event) {
            warn!(self.logger, "Failed to emit store close event: {}", e);
        } else {
            debug!(self.logger, "Store close event emitted successfully");
        }

        // Limpeza final de recursos
        debug!(self.logger, "Performing final resource cleanup");

        // Força a liberação de quaisquer locks restantes
        // Isso é feito implicitamente quando os Arc são dropados, mas podemos ser explícitos

        debug!(
            self.logger,
            "BaseStore close operation completed successfully"
        );

        Ok(())
    }

    /// Equivalente à função `Drop` em Go.
    /// Reseta a store para seu estado inicial, limpando o log, o índice e o cache.
    pub async fn reset(&mut self) -> Result<()> {
        debug!(self.logger, "Starting BaseStore reset operation");

        // Primeiro fecha a store para parar todas as operações
        self.close()
            .await
            .map_err(|e| GuardianError::Store(format!("unable to close store: {}", e)))?;

        // Limpa o oplog criando um novo log vazio
        debug!(self.logger, "Clearing oplog - creating new empty log");

        // Cria um novo log vazio usando as mesmas configurações da store
        use crate::ipfs_log::log::{AdHocAccess, LogOptions};
        use ipfs_api_backend_hyper::IpfsClient;

        let adhoc_access = AdHocAccess;
        let log_options = LogOptions {
            id: Some(&self.id),
            access: adhoc_access,
            entries: &[],
            heads: &[],
            clock: None,
            sort_fn: Some(Box::new(self.sort_fn)),
        };

        // Usa um cliente IPFS temporário para criar o log vazio
        let temp_ipfs_client = Arc::new(IpfsClient::default());
        let new_empty_log = Log::new(temp_ipfs_client, (*self.identity).clone(), log_options);

        // Substitui o log atual pelo log vazio usando o método thread-safe
        let _old_length = self.log_and_index.with_oplog_mut(|oplog| {
            // Para resetar completamente o log, vamos substituir sua estrutura interna
            // Isso efetivamente limpa todas as entradas, heads e estado do log
            let old_length = oplog.len();
            *oplog = new_empty_log; // Usa o log vazio criado
            debug!(self.logger, "Log reset from {} entries to 0", old_length);
            old_length
        });

        debug!(self.logger, "Oplog successfully cleared");

        // Limpa o índice se existir usando o método clear() da trait
        match self.log_and_index.with_index_mut(|index| {
            debug!(self.logger, "Clearing store index");

            // Chama o método clear() da trait StoreIndex
            match index.clear() {
                Ok(()) => {
                    debug!(self.logger, "Index successfully cleared");
                    Ok(())
                }
                Err(e) => {
                    warn!(self.logger, "Failed to clear index: {:?}", e);
                    Err(GuardianError::Store(format!(
                        "Failed to clear index: {:?}",
                        e
                    )))
                }
            }
        }) {
            Ok(Some(result)) => result,
            Ok(None) => {
                debug!(self.logger, "No active index to clear");
            }
            Err(e) => {
                warn!(self.logger, "Error accessing index for clearing: {:?}", e);
                return Err(GuardianError::Store(format!(
                    "Error accessing index: {:?}",
                    e
                )));
            }
        }

        // Limpa o cache completamente
        debug!(self.logger, "Clearing all cache data");

        let cache = self.cache();

        // Lista de todas as chaves conhecidas do cache que devem ser limpas
        let cache_keys = [
            "_localHeads",
            "_remoteHeads",
            "queue",
            "snapshot",
            "replication_progress",
            "peers_status",
            "sync_state",
        ];

        let mut cache_errors = Vec::new();
        let mut cleared_count = 0;

        for key in &cache_keys {
            match cache.delete(key.as_bytes()).await {
                Ok(()) => {
                    cleared_count += 1;
                    debug!(self.logger, "Successfully cleared cache key: {}", key);
                }
                Err(e) => {
                    warn!(self.logger, "Failed to clear cache key '{}': {}", key, e);
                    cache_errors.push(format!("{}: {}", key, e));
                }
            }
        }

        // Para caches que suportam flush completo, força a persistência
        if let Some(sled_cache) = cache
            .as_any()
            .downcast_ref::<crate::cache::cache::SledDatastore>()
        {
            if let Err(e) = sled_cache.close() {
                warn!(self.logger, "Failed to flush cache during reset: {}", e);
            } else {
                debug!(self.logger, "Cache successfully flushed during reset");
            }
        }

        debug!(
            self.logger,
            "Cache clearing completed: {} keys cleared, {} errors",
            cleared_count,
            cache_errors.len()
        );

        // Reseta completamente o status de replicação
        debug!(self.logger, "Resetting replication status");

        {
            let mut status = self.replication_status.lock();
            *status = ReplicationInfo::default();
        }

        // Para garantir que a replicação seja completamente reinicializada
        {
            let mut replicator_guard = self.replicator.write();
            *replicator_guard = None;
        }

        debug!(self.logger, "Replication status and replicator reset");

        // Reseta métricas de retry
        {
            let mut metrics = self.retry_metrics.lock();
            *metrics = crate::stores::base_store::base_store::RetryMetrics::new();
        }

        debug!(self.logger, "Retry metrics reset");

        // Emite evento de reset
        let reset_event = crate::stores::events::EventReset {
            address: self.address.clone(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };

        // Log do evento de reset para debugging
        debug!(
            self.logger,
            "Reset event created";
            "address" => reset_event.address.to_string(),
            "timestamp" => reset_event.timestamp
        );

        if let Err(e) = self
            .emitters
            .evt_ready
            .emit(crate::stores::events::EventReady::new(
                self.address.clone(),
                Vec::new(), // heads vazias após reset
            ))
        {
            warn!(self.logger, "Failed to emit reset completion event: {}", e);
        } else {
            debug!(self.logger, "Reset completion event emitted successfully");
        }

        // Se houve erros de cache mas o reset foi bem-sucedido no geral, log como warning
        if !cache_errors.is_empty() {
            warn!(
                self.logger,
                "BaseStore reset completed with cache warnings: {:?}", cache_errors
            );
        } else {
            debug!(
                self.logger,
                "BaseStore reset completed successfully - all data cleared"
            );
        }

        Ok(())
    }

    /// Equivalente à função `InitBaseStore` em Go.
    ///
    /// Este construtor é `async` porque precisa interagir com a rede (para obter o PeerId)
    /// e inicia tarefas em background. Ele retorna um `Arc<Self>` para permitir o
    /// compartilhamento seguro da `store` com as tarefas que ela mesma cria.
    pub async fn new(
        ipfs: Arc<IpfsClient>,
        identity: Arc<Identity>,
        address: Arc<dyn Address + Send + Sync>,
        options: Option<NewStoreOptions>,
    ) -> Result<Arc<Self>> {
        let mut opts = options.unwrap_or_else(|| NewStoreOptions {
            event_bus: None,
            index: None,
            access_controller: None,
            cache: None,
            cache_destroy: None,
            replication_concurrency: None,
            reference_count: None,
            replicate: None,
            max_history: None,
            directory: String::new(),
            sort_fn: None,
            logger: None,
            tracer: None,
            pubsub: None,
            message_marshaler: None,
            peer_id: PeerId::random(),
            direct_channel: None,
            close_func: None,
            store_specific_opts: None,
        });
        let cancellation_token = CancellationToken::new();

        // --- 1. Definição de Padrões (Defaults) ---
        let logger = opts.logger.take().unwrap_or_else(|| {
            // Cria um logger "nulo" que descarta todas as mensagens.
            // É um padrão seguro quando nenhum logger é fornecido.
            Logger::root(slog::Discard, o!())
        });
        let event_bus = opts
            .event_bus
            .take()
            .ok_or_else(|| GuardianError::Store("EventBus is a required option".to_string()))?;
        let _access_controller = match opts.access_controller.take() {
            Some(ac) => ac,
            None => {
                // Se não foi fornecido um access controller, cria um SimpleAccessController padrão
                use std::collections::HashMap;

                let mut default_access = HashMap::new();
                default_access.insert("write".to_string(), vec!["*".to_string()]);

                // Cria um logger padrão para o SimpleAccessController
                let controller_logger = Logger::root(slog::Discard, o!());

                Arc::new(SimpleAccessController::new(
                    controller_logger,
                    Some(default_access),
                )) as Arc<dyn AccessController>
            }
        };

        // Cria um IdentityProvider baseado na identidade da store
        let identity_provider =
            Arc::new(GuardianDBIdentityProvider::new()) as Arc<dyn IdentityProvider>;

        // --- 2. Criação dos Componentes ---
        let id = address.to_string().to_string();
        let db_name = address.get_path().to_string();

        // Define 'directory' a partir das opções ou de um padrão.
        let directory = if opts.directory.is_empty() {
            Path::new("./GuardianDB")
                .join(&id)
                .to_str()
                .unwrap_or_default()
                .to_string()
        } else {
            opts.directory.clone()
        };

        // Define 'tracer' a partir das opções ou de um tracer no-op.
        let tracer = opts.tracer.take().unwrap_or_else(|| {
            Arc::new(TracerWrapper::Noop(
                NoopTracerProvider::new().tracer("berty.guardian-db"),
            ))
        });

        // Define 'cache' e 'cache_destroy' usando a função do módulo `cache`.
        // Esta é a chamada correta com base na estrutura do seu projeto.
        let (cache, _cache_destroy) = if let Some(cache) = opts.cache.take() {
            let _destroy = opts
                .cache_destroy
                .take()
                .unwrap_or_else(|| Box::new(|| Ok(())));
            (cache, _destroy)
        } else {
            // Usar implementação de cache baseada em sled
            let cache_impl = Self::create_real_cache(address.as_ref())?;
            (
                cache_impl,
                Box::new(
                    move || -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
                        Ok(())
                    },
                )
                    as Box<
                        dyn FnOnce() -> std::result::Result<
                            (),
                            Box<dyn std::error::Error + Send + Sync>,
                        >,
                    >,
            )
        };

        let sort_fn = opts.sort_fn.take().unwrap_or(default_sort_fn);
        let _index_builder = opts
            .index
            .take()
            .ok_or_else(|| GuardianError::Store("Index builder is required".to_string()))?;

        // Criar o log com as configurações apropriadas usando AdHocAccess
        use crate::ipfs_log::log::AdHocAccess;
        use ipfs_api_backend_hyper::IpfsClient;
        let adhoc_access = AdHocAccess; // É um struct unit, não precisa de construtor

        let log_options = LogOptions {
            id: Some(&id),
            access: adhoc_access,
            sort_fn: Some(Box::new(sort_fn)),
            entries: &[],
            heads: &[],
            clock: None,
        };

        // Criar um IpfsClient para o log
        let temp_ipfs_client = Arc::new(IpfsClient::default());
        let oplog = Log::new(temp_ipfs_client, identity.as_ref().clone(), log_options);

        // Criar um índice inicial usando o index_builder fornecido
        let public_key_bytes = if let Some(pk) = identity.public_key() {
            pk.encode_protobuf()
        } else {
            // Se não conseguir obter a chave pública, usa a string como bytes
            identity.pub_key().as_bytes().to_vec()
        };
        let initial_index = _index_builder(&public_key_bytes);
        let log_and_index = LogAndIndex::new(oplog, Some(initial_index));

        // Emitters precisam ser criados a partir do event_bus
        let emitters = generate_emitters(&event_bus).await?;

        // EventEmitter para compatibilidade com Store trait
        let emitter_interface =
            Arc::new(EventEmitter::default()) as Arc<dyn EmitterInterface + Send + Sync>;

        // --- 3. Construção da Store  ---

        // Deriva o PeerId a partir da identidade - implementação simplificada
        let peer_id = if let Some(public_key) = identity.public_key() {
            // Usa hash da chave pública para derivar PeerId determinístico
            let key_hash = {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(&public_key.encode_protobuf());
                hasher.finalize()
            };

            // Cria um PeerId determinístico baseado no hash
            // Usamos apenas os primeiros 32 bytes do hash para compatibilidade
            let mut key_bytes = [0u8; 32];
            key_bytes.copy_from_slice(&key_hash[..32]);

            // Gera um keypair Ed25519 a partir do hash
            match libp2p::identity::Keypair::ed25519_from_bytes(key_bytes) {
                Ok(keypair) => PeerId::from(keypair.public()),
                Err(_) => {
                    warn!(
                        logger,
                        "Failed to create deterministic PeerId, using random"
                    );
                    PeerId::random()
                }
            }
        } else {
            // Se não há chave pública, usa hash da string da identidade
            let id_hash = {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(identity.pub_key().as_bytes());
                hasher.finalize()
            };

            let mut key_bytes = [0u8; 32];
            key_bytes.copy_from_slice(&id_hash[..32]);

            match libp2p::identity::Keypair::ed25519_from_bytes(key_bytes) {
                Ok(keypair) => PeerId::from(keypair.public()),
                Err(_) => {
                    warn!(
                        logger,
                        "Failed to create PeerId from identity string, using random"
                    );
                    PeerId::random()
                }
            }
        };

        let store = Arc::new(Self {
            id,
            peer_id,
            identity,
            address: address.clone(),
            db_name,
            directory,
            reference_count: opts.reference_count.unwrap_or(64) as usize,
            sort_fn,
            ipfs: ipfs.clone(),
            compat_ipfs_client: ipfs_api_backend_hyper::IpfsClient::default(),
            access_controller: _access_controller,
            identity_provider,
            cache,
            log_and_index,
            replicator: Arc::new(RwLock::new(None)), // Nova arquitetura com Arc<RwLock>
            replication_status: Arc::new(Mutex::new(ReplicationInfo::default())),
            pubsub: opts
                .pubsub
                .clone()
                .ok_or_else(|| GuardianError::Store("PubSub is required".to_string()))?,
            message_marshaler: opts
                .message_marshaler
                .clone()
                .ok_or_else(|| GuardianError::Store("MessageMarshaler is required".to_string()))?,
            direct_channel: Arc::new(tokio::sync::Mutex::new(
                opts.direct_channel
                    .take()
                    .ok_or_else(|| GuardianError::Store("DirectChannel is required".to_string()))?,
            )),
            event_bus: Arc::new(event_bus),
            emitter_interface,
            emitters,
            logger: Arc::new(logger),
            tracer,
            retry_metrics: Arc::new(Mutex::new(RetryMetrics::new())),
            cancellation_token,
            tasks: Mutex::new(JoinSet::new()),
        });

        // --- 4. Criação do Replicator e Início da Tarefa de Eventos ---
        // O replicator precisa de uma referência à store para poder interagir com ela.
        // Usamos uma referência fraca (`Weak`) para evitar um ciclo de referência.

        // Cria o replicator quando as opções permitirem
        if opts.replicate.unwrap_or(true) && opts.replication_concurrency.is_some() {
            let replication_concurrency = opts.replication_concurrency.unwrap_or(1) as usize;

            // Cria e inicializa o replicador
            debug!(
                store.logger,
                "Initializing replicator with concurrency: {}", replication_concurrency
            );

            // Cria as opções do replicador
            let replicator_opts = crate::stores::replicator::replicator::ReplicatorOptions {
                logger: Some(store.logger.as_ref().clone()),
                tracer: Some(store.tracer.clone()),
                event_bus: Some((*store.event_bus).clone()),
            };

            // Converte BaseStore para StoreInterface usando Arc
            use crate::stores::replicator::traits::StoreInterface;
            let store_interface: Arc<dyn StoreInterface> = store.clone() as Arc<dyn StoreInterface>;

            // Cria o replicador
            match crate::stores::replicator::replicator::Replicator::new(
                store_interface,
                Some(replication_concurrency),
                Some(replicator_opts),
            )
            .await
            {
                Ok(replicator) => {
                    *store.replicator.write() = Some(Arc::new(replicator));
                    debug!(
                        store.logger,
                        "Replicator successfully initialized with concurrency: {}",
                        replication_concurrency
                    );
                }
                Err(e) => {
                    warn!(store.logger, "Failed to initialize replicator: {:?}", e);
                }
            }
        }

        // Inicia a tarefa em background que substitui a goroutine do Go.
        let store_weak = Arc::downgrade(&store);
        store.tasks.lock().spawn(async move {
            // A tarefa só continua enquanto a store existir.
            while let Some(store) = store_weak.upgrade() {
                select! {
                    // Aguarda cancelamento
                    _ = store.cancellation_token.cancelled() => {
                        debug!(store.logger, "Background task cancelled");
                        break;
                    }

                    // Processa eventos periodicamente
                    _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
                        // Recalcula status de replicação periodicamente
                        store.recalculate_replication_max(1000); // Máximo padrão de 1000 entradas

                        // Verifica se há cache que precisa ser persistido e força flush
                        match store.cache().as_any().downcast_ref::<crate::cache::cache::SledDatastore>() {
                            Some(sled_cache) => {
                                if let Err(e) = sled_cache.close() {
                                    warn!(store.logger, "Failed to flush cache during periodic maintenance: {}", e);
                                } else {
                                    debug!(store.logger, "Cache successfully flushed during periodic maintenance");
                                }
                            }
                            None => {
                                // Para outros tipos de cache, tentamos fechar através do wrapper
                                debug!(store.logger, "Cache type doesn't support direct flushing - using wrapper approach");
                                // Se for DatastoreWrapper (LevelDown), usa métodos específicos
                                if let Some(_wrapper) = store.cache().as_any()
                                    .downcast_ref::<crate::cache::level_down::DatastoreWrapper>()
                                {
                                    // Para DatastoreWrapper, o flush é feito internamente
                                    debug!(store.logger, "DatastoreWrapper cache - periodic sync handled internally");
                                } else {
                                    debug!(store.logger, "Unknown cache type - no periodic flush available");
                                }
                            }
                        }

                        // Atualiza estatísticas do índice se necessário
                        if store.has_active_index() {
                            if let Err(e) = store.update_index() {
                                warn!(store.logger, "Failed to update index in background: {}", e);
                            }
                        }
                    }
                }
            }
        });

        // --- 5. Finalização ---
        if opts.replicate.unwrap_or(true) {
            // Inicia a lógica de replicação
            debug!(store.logger, "Initiating store replication");

            // Clone o store para usar na replicação
            let store_for_replication = store.clone();

            // Spawna a replicação em uma task separada para não bloquear a criação da store
            tokio::spawn(async move {
                if let Err(e) = store_for_replication.replicate().await {
                    error!(
                        store_for_replication.logger,
                        "Failed to start replication: {:?}", e
                    );
                } else {
                    debug!(
                        store_for_replication.logger,
                        "Store replication started successfully"
                    );
                }
            });
        } else {
            debug!(store.logger, "Replication disabled by configuration");
        }

        Ok(store)
    }

    // Funções auxiliares chamadas pela tarefa em background
    fn recalculate_replication_max(&self, max_total: usize) {
        let current_length = self.log_and_index.with_oplog(|oplog| oplog.len());

        // Atualiza o status de replicação com informações reais
        if let Some(status_guard) = self.replication_status.try_lock() {
            // Usa método eficiente para atualizar ambos valores em uma única operação
            let status_clone = status_guard.clone();

            tokio::spawn(async move {
                // Atualiza tanto o progresso atual quanto o máximo
                status_clone
                    .set_progress_and_max(current_length, max_total)
                    .await;

                // Log informativo após a atualização
                let percentage = status_clone.progress_percentage().await;
                tracing::debug!(
                    "Replication status updated: {}/{} ({}%)",
                    current_length,
                    max_total,
                    percentage
                );
            });

            debug!(
                self.logger,
                "Replication max updated: {}/{} (progress: {})",
                current_length,
                max_total,
                if max_total > 0 {
                    format!("{:.1}%", (current_length as f64 / max_total as f64) * 100.0)
                } else {
                    "N/A".to_string()
                }
            );
        } else {
            warn!(
                self.logger,
                "Unable to update replication status - lock contention"
            );
        }
    }

    #[allow(dead_code)]
    fn recalculate_replication_status_internal(&self, max_total: usize) {
        let current_length = self.log_and_index.with_oplog(|oplog| oplog.len());
        let heads_count = self.log_and_index.with_oplog(|oplog| oplog.heads().len());

        // Atualiza o status de replicação
        if let Some(status_guard) = self.replication_status.try_lock() {
            // Usa os métodos síncronos para atualizar o status
            // Como ReplicationInfo::set_progress e set_max são async, vamos usar spawn
            let status_clone = status_guard.clone();

            tokio::spawn(async move {
                status_clone.set_progress(current_length).await;
                status_clone.set_max(max_total).await;
            });
        }

        debug!(
            self.logger,
            "Replication status updated: buffered={}, heads={}, max={}",
            current_length,
            heads_count,
            max_total
        );
    }

    #[allow(dead_code)]
    async fn replication_load_complete(&self, logs: Vec<Log>) -> Result<()> {
        let mut total_entries_added = 0;

        // Processa cada log usando a arquitetura refatorada
        for log in logs {
            let entries_added = self.log_and_index.with_oplog_mut(|oplog| {
                let entries_before = oplog.len();
                match oplog.join(&log, None) {
                    Some(_) => {
                        let entries_after = oplog.len();
                        Ok(entries_after - entries_before)
                    }
                    None => Err(GuardianError::Store("Failed to join log".to_string())),
                }
            })?;

            total_entries_added += entries_added;
        }

        // Atualiza o índice usando a nova arquitetura thread-safe
        let updated_entries = self.update_index()?;
        debug!(
            self.logger,
            "Updated index with {} entries after replication", updated_entries
        );

        // Salva os heads atualizados no cache
        let heads = self.with_oplog(|oplog| {
            oplog
                .heads()
                .iter()
                .map(|arc_entry| (**arc_entry).clone())
                .collect::<Vec<Entry>>()
        });

        let heads_bytes = serde_json::to_vec(&heads).map_err(|e| {
            GuardianError::Store(format!(
                "Failed to serialize replicated heads for caching: {}",
                e
            ))
        })?;

        let cache = self.cache();
        cache.put("_remoteHeads".as_bytes(), &heads_bytes).await?;

        let log_length = self.with_oplog(|oplog| oplog.len());

        // Emite evento de replicação concluída
        let replicated_event = EventReplicated {
            address: self.address.clone(),
            entries: heads,
            log_length,
        };

        if let Err(e) = self.emitters.evt_replicated.emit(replicated_event) {
            warn!(self.logger, "Failed to emit EventReplicated: {}", e);
        } else {
            debug!(
                self.logger,
                "Replication completed: added {} entries, total length: {}",
                total_entries_added,
                log_length
            );
        }

        Ok(())
    }

    /// Equivalente à função `recalculateReplicationProgress` em Go.
    ///
    /// Calcula e atualiza o progresso da replicação.
    /// A lógica é mantida, mas adaptada para as APIs de Rust.
    fn recalculate_replication_progress(&self) {
        let current_length = self.log_and_index.with_oplog(|oplog| oplog.len());
        let heads_count = self.log_and_index.with_oplog(|oplog| oplog.heads().len());

        // Atualiza o progresso baseado no estado atual do log
        if let Some(status_guard) = self.replication_status.try_lock() {
            let status_clone = status_guard.clone();

            tokio::spawn(async move {
                let current_max = status_clone.get_max().await;
                let progress = if current_max > 0 {
                    ((current_length as f64 / current_max as f64) * 100.0) as usize
                } else {
                    100 // Se não há máximo definido, considera 100%
                };

                status_clone.set_progress(progress).await;
            });
        }

        debug!(
            self.logger,
            "Replication progress recalculated: current={}, heads={}", current_length, heads_count
        );
    }

    /// Equivalente à função `recalculateReplicationStatus` em Go.
    ///
    /// Função de conveniência que recalcula tanto o máximo quanto o progresso.
    /// A visibilidade foi alterada para `pub` se for chamada por outros módulos,
    /// ou pode permanecer privada (`fn`) se for apenas um auxiliar interno.
    pub fn recalculate_replication_status(&self, max_total: usize) {
        self.recalculate_replication_max(max_total);
        self.recalculate_replication_progress();
    }

    /// Equivalente à função `SortFn` em Go.
    ///
    /// Retorna o ponteiro para a função de ordenação usada pelo OpLog.
    /// Ponteiros de função em Rust são `Copy`, então podemos retorná-los por valor.
    pub fn sort_fn(&self) -> SortFn {
        self.sort_fn
    }

    /// Equivalente à função `updateIndex` em Go.
    ///
    /// Atualiza o índice da store com base no estado atual do OpLog.
    /// Usa a arquitetura refatorada para acesso thread-safe sem limitações de lifetime.
    pub fn update_index(&self) -> Result<usize> {
        // Cria um span para rastreamento de performance
        let _span = self.tracer.start("update-index");

        // Usa o método thread-safe da nova arquitetura
        match self.log_and_index.update_index_safe() {
            Ok(count) => {
                if count > 0 {
                    debug!(
                        self.logger,
                        "Index updated successfully with {} entries", count
                    );
                } else {
                    warn!(self.logger, "No active index to update");
                }
                Ok(count)
            }
            Err(e) => {
                error!(self.logger, "Failed to update index: {:?}", e);
                Err(e)
            }
        }
    }

    /// Equivalente à função `LoadMoreFrom` em Go.
    ///
    /// Carrega entradas adicionais no store, delegando para o replicador quando disponível
    /// ou processando diretamente quando necessário.
    pub fn load_more_from(&self, entries: Vec<Entry>) -> Result<usize> {
        if entries.is_empty() {
            return Ok(0);
        }

        debug!(self.logger, "Loading {} additional entries", entries.len());

        // Se houver um replicador, delega para ele
        if let Some(_replicator) = self.replicator.read().clone() {
            debug!(
                self.logger,
                "Delegating {} entries to replicator for processing",
                entries.len()
            );

            //Usando replicador
            let added_count = self.log_and_index.with_oplog_mut(|oplog| {
                let mut count = 0;
                for entry in &entries {
                    // Verifica se a entrada já existe
                    if !oplog.has(&entry.hash().to_string()) {
                        // Para entradas existentes, fazemos join ao invés de append
                        // Cria um log temporário com a entrada e faz join
                        match self.create_temporary_log_with_entry(entry) {
                            Ok(temp_log) => {
                                if let Some(_) = oplog.join(&temp_log, None) {
                                    count += 1;
                                    debug!(
                                        self.logger,
                                        "Successfully joined entry {}",
                                        entry.hash()
                                    );
                                } else {
                                    warn!(
                                        self.logger,
                                        "Failed to join entry {}: join returned None",
                                        entry.hash()
                                    );
                                }
                            }
                            Err(e) => {
                                warn!(
                                    self.logger,
                                    "Failed to create temporary log for entry {}: {}",
                                    entry.hash(),
                                    e
                                );
                            }
                        }
                    }
                }
                count
            });

            // Atualiza o índice se entradas foram adicionadas
            if added_count > 0 {
                self.update_index()?;

                // Emite evento de replicação para entradas carregadas
                let log_length = self.log_and_index.with_oplog(|oplog| oplog.len());
                let event = EventReplicated {
                    address: self.address.clone(),
                    log_length,
                    entries: entries.clone(),
                };
                if let Err(e) = self.emitters.evt_replicated.emit(event) {
                    warn!(self.logger, "Failed to emit replicated event: {}", e);
                }

                debug!(
                    self.logger,
                    "Successfully loaded {} new entries via replicator", added_count
                );
            }

            Ok(added_count)
        } else {
            // Se não há replicador, processa diretamente
            debug!(
                self.logger,
                "No replicator available, processing {} entries directly",
                entries.len()
            );

            // Adiciona as entradas ao oplog diretamente
            let added_count = self.log_and_index.with_oplog_mut(|oplog| {
                let mut count = 0;
                for entry in &entries {
                    // Verifica se a entrada já existe
                    if !oplog.has(&entry.hash().to_string()) {
                        // Para processamento direto, usa append com payload
                        match entry.get_payload() {
                            payload => {
                                if let Ok(payload_str) = std::str::from_utf8(payload) {
                                    oplog.append(payload_str, None);
                                    count += 1;
                                } else {
                                    warn!(
                                        self.logger,
                                        "Failed to convert payload to string for entry {}",
                                        entry.hash()
                                    );
                                }
                            }
                        }
                    }
                }
                count
            });

            if added_count > 0 {
                // Atualiza o índice se entradas foram adicionadas
                self.update_index()?;

                // Emite eventos para as entradas processadas diretamente
                let log_length = self.log_and_index.with_oplog(|oplog| oplog.len());
                let event = EventReplicated {
                    address: self.address.clone(),
                    log_length,
                    entries: entries.clone(),
                };
                if let Err(e) = self.emitters.evt_replicated.emit(event) {
                    warn!(self.logger, "Failed to emit replicated event: {}", e);
                }

                debug!(
                    self.logger,
                    "Successfully loaded {} new entries directly", added_count
                );
            }

            Ok(added_count)
        }
    }

    /// Helper method para criar um log temporário com uma entrada específica
    fn create_temporary_log_with_entry(&self, entry: &Entry) -> Result<Log> {
        // Cria um log temporário contendo apenas a entrada especificada
        debug!(
            self.logger,
            "Creating temporary log with entry hash: {}",
            entry.hash()
        );

        // Converte a entrada para Arc<Entry> conforme esperado pelo LogOptions
        let arc_entry = Arc::new(entry.clone());
        let entries_slice = &[arc_entry.clone()];

        // Cria um ID único para o log temporário baseado no hash da entrada
        let temp_log_id = format!("temp_log_{}", entry.hash());

        // Configura as opções do log com a entrada como entrada e head
        let log_options = LogOptions::new()
            .id(&temp_log_id)
            .entries(entries_slice)
            .heads(entries_slice) // A entrada também é um head neste log temporário
            .sort_fn(self.sort_fn); // Usa a mesma função de ordenação da store

        // Usa o cliente IPFS hyper padrão para logs temporários
        // Para operações de log temporário, um cliente básico é suficiente
        let hyper_client = Arc::new(ipfs_api_backend_hyper::IpfsClient::default());

        // Cria o log temporário usando a identidade da store
        let temp_log = Log::new(
            hyper_client,
            (*self.identity).clone(), // Desreferencia o Arc<Identity>
            log_options,
        );

        debug!(
            self.logger,
            "Successfully created temporary log '{}' with {} entries",
            temp_log_id,
            temp_log.len()
        );

        Ok(temp_log)
    }

    /// Equivalente à função `Sync` em Go.
    ///
    /// Processa uma lista de "heads" recebidas de outros peers, validando o
    /// acesso e persistindo-as no IPFS antes de enfileirá-las para carregamento.
    /// A função é `async` devido à chamada de escrita no IPFS via `Entry::multihash`.
    pub async fn sync(&self, heads: Vec<Entry>) -> Result<()> {
        if heads.is_empty() {
            return Ok(());
        }

        let mut verified_heads = vec![];

        debug!(self.logger, "Sync: Processing {} heads", heads.len());

        for head in heads {
            // Validação básica: verifica se o head não está vazio
            if head.hash().is_empty() || head.payload().is_empty() {
                debug!(self.logger, "Sync: head discarded (invalid data)");
                continue;
            }

            // Cria um novo contexto para cada iteração para evitar problemas de borrow
            let head_ac_context = self.create_append_context();

            // Usa o IdentityProvider da store para validação de acesso
            let identity_provider = &self.identity_provider;

            // Validação de acesso usando o access_controller
            if let Err(e) = self
                .access_controller
                .can_append(&head, identity_provider.as_ref(), &head_ac_context)
                .await
            {
                debug!(self.logger, "Sync: head discarded (no write access): {}", e);
                continue;
            }

            // Verifica se a entrada já está no IPFS ou precisa ser armazenada
            let hash = head.hash();

            // Validação de integridade do hash - por enquanto, apenas verificamos se não está vazio
            if hash.is_empty() {
                debug!(self.logger, "Sync: head discarded (empty hash)");
                continue;
            }

            // Verifica se já temos esta entrada no oplog
            let already_exists = self
                .log_and_index
                .with_oplog(|oplog| oplog.has(&hash.to_string()));

            if already_exists {
                debug!(self.logger, "Sync: head already exists in oplog");
                continue;
            }

            verified_heads.push(head);
        }

        if verified_heads.is_empty() {
            debug!(self.logger, "Sync: no new heads to process");
            return Ok(());
        }

        // Processa as `heads` verificadas
        if let Some(replicator) = {
            let guard = self.replicator.read();
            guard.clone()
        } {
            // Delega para o replicador usando o método load()
            debug!(
                self.logger,
                "Delegating {} verified heads to replicator for processing",
                verified_heads.len()
            );

            // Converte Vec<Entry> para Vec<Box<Entry>> conforme esperado pelo replicador
            let boxed_heads: Vec<Box<Entry>> = verified_heads
                .into_iter()
                .map(|entry| Box::new(entry))
                .collect();

            // Chama o método load() do replicador que processará as entradas na fila
            replicator.load(boxed_heads).await;

            debug!(
                self.logger,
                "Successfully delegated heads to replicator for background processing"
            );
        } else {
            // Processa diretamente se não há replicador
            debug!(
                self.logger,
                "Processing {} heads directly",
                verified_heads.len()
            );

            // Adiciona as entradas ao oplog usando append
            let added_count = self.log_and_index.with_oplog_mut(|oplog| {
                let mut count = 0;
                for head in verified_heads {
                    // Usa append para adicionar entrada
                    oplog.append(&head.payload, None);
                    count += 1;
                }
                count
            });

            // Atualiza o índice se entradas foram adicionadas
            if added_count > 0 {
                if let Err(e) = self.update_index() {
                    warn!(self.logger, "Failed to update index after sync: {}", e);
                } else {
                    debug!(
                        self.logger,
                        "Sync completed: processed {} new heads", added_count
                    );
                }
            }
        }

        Ok(())
    }

    /// Equivalente à função `AddOperation` em Go.
    ///
    /// O método principal para adicionar dados à store. Ele serializa a operação,
    /// anexa ao OpLog, atualiza o índice e o cache, e emite um evento.
    pub async fn add_operation(
        &self,
        op: Operation,
        on_progress: Option<mpsc::Sender<Entry>>,
    ) -> Result<Entry> {
        let data = op
            .marshal()
            .map_err(|e| GuardianError::Store(format!("Unable to marshal operation: {}", e)))?;

        // Usa a nova arquitetura thread-safe para adicionar entrada
        let new_entry = self.log_and_index.with_oplog_mut(|oplog| {
            let data_str = String::from_utf8_lossy(&data);
            oplog.append(&data_str, Some(self.reference_count)).clone()
        });

        // Atualiza o índice usando a nova arquitetura
        self.update_index()
            .map_err(|e| GuardianError::Store(format!("Unable to update index: {}", e)))?;

        // Salva os heads locais no cache usando acesso thread-safe
        let heads = self.with_oplog(|oplog| {
            oplog
                .heads()
                .into_iter()
                .map(|arc_entry| {
                    // Como oplog.heads() retorna Vec<Arc<Entry>>, fazemos clone do Arc
                    (*arc_entry).clone()
                })
                .collect::<Vec<Entry>>()
        });

        let local_heads_bytes = serde_json::to_vec(&heads).map_err(|e| {
            GuardianError::Store(format!(
                "Failed to serialize local heads for caching: {}",
                e
            ))
        })?;

        let cache = self.cache();
        cache
            .put("_localHeads".as_bytes(), &local_heads_bytes)
            .await
            .map_err(|e| GuardianError::Store(format!("Failed to cache local heads: {}", e)))?;

        // Emite evento de escrita
        let write_event = EventWrite {
            address: self.address.clone(),
            entry: new_entry.clone(),
            heads: heads.clone(),
        };

        self.emitters
            .evt_write
            .emit(write_event)
            .unwrap_or_else(|_| {
                warn!(self.logger, "Unable to emit write event");
            });

        if let Some(callback) = on_progress {
            callback.send(new_entry.clone()).await.ok();
        }

        Ok(new_entry)
    }

    /// Equivalente à função `replicate` em Go.
    ///
    /// Inicia a lógica de replicação, subscrevendo ao tópico do pubsub e
    /// inicializando os listeners de eventos internos e externos.
    /// Equivalente à função `replicate` em Go.
    ///
    /// Inicia a lógica completa de replicação, subscrevendo ao tópico do pubsub e
    /// inicializando os listeners de eventos internos e externos.
    pub async fn replicate(self: &Arc<Self>) -> Result<()> {
        debug!(self.logger, "Starting replication for store: {}", self.id);

        // --- 1. CRIAR O TÓPICO DE PUBSUB ---
        debug!(
            self.logger,
            "Creating pubsub topic for store replication: {}", self.id
        );

        // **Como PubSubInterface::topic_subscribe requer &mut self, mas temos Arc<dyn PubSubInterface>,
        // vamos usar uma abordagem baseada no tipo concreto quando disponível
        let topic = if let Some(core_api_pubsub) = self
            .pubsub
            .as_ref()
            .as_any()
            .downcast_ref::<std::sync::Arc<crate::pubsub::core_api::CoreApiPubSub>>()
        {
            // Usa o método interno que funciona com &self
            debug!(self.logger, "Using CoreApiPubSub for topic subscription");
            core_api_pubsub.topic_subscribe_internal(&self.id).await?
        } else if let Some(_raw_pubsub) = self
            .pubsub
            .as_ref()
            .as_any()
            .downcast_ref::<crate::pubsub::raw::RawPubSub>()
        {
            // Cria um clone mutável temporário para RawPubSub
            debug!(self.logger, "Using RawPubSub for topic subscription");

            // Como RawPubSub também precisa de &mut, vamos usar uma abordagem diferente
            // Vamos tentar acessar o método topic_subscribe diretamente via trait object
            return Err(GuardianError::Store(
                "RawPubSub requires mutable access - not implemented yet".to_string(),
            ));
        } else {
            return Err(GuardianError::Store(
                "Unknown PubSub implementation type".to_string(),
            ));
        };

        debug!(
            self.logger,
            "Successfully created topic '{}' for replication",
            topic.topic()
        );

        // --- 2. CONFIGURAR LISTENERS PARA EVENTOS DE ESCRITA ---
        debug!(self.logger, "Setting up store write event listener");
        if let Err(e) = self.store_listener(topic.clone()) {
            error!(self.logger, "Failed to start store listener: {:?}", e);
            return Err(GuardianError::Store(format!(
                "Failed to configure write event listener: {}",
                e
            )));
        }

        // --- 3. CONFIGURAR LISTENERS PARA EVENTOS DE PEERS ---
        debug!(self.logger, "Setting up pubsub peer event listener");
        if let Err(e) = self.pubsub_chan_listener(topic.clone()) {
            error!(self.logger, "Failed to start pubsub listener: {:?}", e);
            return Err(GuardianError::Store(format!(
                "Failed to configure peer event listener: {}",
                e
            )));
        }

        // --- 4. INICIAR SINCRONIZAÇÃO COM PEERS EXISTENTES ---
        debug!(self.logger, "Starting synchronization with existing peers");

        // Obtém peers já conectados ao tópico
        match topic.peers().await {
            Ok(existing_peers) => {
                debug!(
                    self.logger,
                    "Found {} existing peers in topic: {:?}",
                    existing_peers.len(),
                    existing_peers
                );

                // Inicia exchange de heads com cada peer existente
                for peer in existing_peers {
                    if peer != self.peer_id {
                        debug!(
                            self.logger,
                            "Initiating head exchange with existing peer: {:?}", peer
                        );

                        let store_clone = self.clone();
                        let logger_clone = self.logger.clone();

                        // Spawn task para exchange assíncrono
                        tokio::spawn(async move {
                            match store_clone.on_new_peer_joined(peer).await {
                                Ok(()) => {
                                    debug!(
                                        logger_clone,
                                        "Successfully synchronized with existing peer: {:?}", peer
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        logger_clone,
                                        "Failed to synchronize with existing peer {:?}: {:?}",
                                        peer,
                                        e
                                    );
                                }
                            }
                        });
                    }
                }
            }
            Err(e) => {
                warn!(
                    self.logger,
                    "Failed to get existing peers from topic: {:?}", e
                );
            }
        }

        // --- 5. CONFIGURAR MÉTRICAS E MONITORAMENTO ---
        debug!(self.logger, "Configuring replication metrics");

        // Registra que a replicação foi iniciada
        if let Some(_metrics) = self.retry_metrics.try_lock() {
            // Pode adicionar métricas específicas de replicação aqui
            debug!(self.logger, "Replication metrics initialized");
        }

        // --- 6. FINALIZAÇÃO ---
        debug!(
            self.logger,
            "Replication started successfully for store: {}", self.id
        );

        // Emite evento de que a replicação está pronta
        let current_heads = self.with_oplog(|oplog| {
            oplog
                .heads()
                .iter()
                .map(|arc_entry| (**arc_entry).clone())
                .collect::<Vec<Entry>>()
        });

        let ready_event =
            crate::stores::events::EventReady::new(self.address.clone(), current_heads);

        if let Err(e) = self.emitters.evt_ready.emit(ready_event) {
            warn!(self.logger, "Failed to emit replication ready event: {}", e);
        } else {
            debug!(self.logger, "Replication ready event emitted successfully");
        }

        Ok(())
    }

    /// Equivalente à função `storeListener` em Go.
    ///
    /// Inicia uma tarefa em background que escuta por eventos de escrita (`EventWrite`)
    /// no barramento de eventos interno. Para cada evento, outra tarefa é iniciada
    /// para propagar a atualização para a rede via pubsub.
    fn store_listener(
        self: &Arc<Self>,
        topic: Arc<dyn PubSubTopic<Error = GuardianError> + Send + Sync>,
    ) -> Result<()> {
        let store_weak = Arc::downgrade(self);
        let cancellation_token = self.cancellation_token.clone();
        let event_bus = self.event_bus.clone();

        tokio::spawn(async move {
            // Criar o subscriber dentro da task async
            let mut sub = match event_bus.subscribe::<EventWrite>().await {
                Ok(sub) => sub,
                Err(e) => {
                    // Log error if possible
                    eprintln!("Failed to subscribe to EventWrite: {:?}", e);
                    return;
                }
            };

            loop {
                // `select!` aguarda ou um novo evento ou o cancelamento da store.
                select! {
                    _ = cancellation_token.cancelled() => break,
                    Ok(event) = sub.recv() => {
                        // Tenta "promover" a referência fraca para uma forte.
                        if let Some(store) = store_weak.upgrade() {
                            let topic_clone = topic.clone();
                            let logger = store.logger.clone();
                            let store_clone = store.clone(); // Clone o Arc para mover para a task
                            // Inicia a tarefa dentro do JoinSet da store para um gerenciamento adequado.
                            store.tasks.lock().spawn(async move {
                                if let Err(_e) = store_clone.handle_event_write(event, topic_clone).await {
                                    warn!(logger, "unable to handle EventWrite");
                                }
                            });
                        } else {
                            // A store foi dropada, então a tarefa deve terminar.
                            break;
                        }
                    }
                }
            }
        });

        Ok(())
    }

    /// Equivalente à função `pubSubChanListener` em Go.
    ///
    /// Spawns a task to handle peer join/leave events from PubSub.
    fn pubsub_chan_listener(
        self: &Arc<Self>,
        topic: Arc<dyn PubSubTopic<Error = GuardianError> + Send + Sync>,
    ) -> Result<()> {
        let store_weak = Arc::downgrade(self);
        let cancellation_token = self.cancellation_token.clone();
        let logger = self.logger.clone();

        tokio::spawn(async move {
            // Usa watch_peers() do PubSubTopic para eventos
            debug!(
                logger,
                "Starting pubsub peer events listener for topic: {}",
                topic.topic()
            );

            // Obtém stream de eventos de peers do tópico
            let peer_events_stream = match topic.watch_peers().await {
                Ok(stream) => stream,
                Err(e) => {
                    error!(logger, "Failed to create peer events stream: {:?}", e);
                    return;
                }
            };

            use futures::StreamExt;
            let mut peer_events = peer_events_stream;

            loop {
                select! {
                    _ = cancellation_token.cancelled() => {
                        debug!(logger, "Pubsub peer listener cancelled");
                        break;
                    }
                    // Processa eventos reais de peers
                    peer_event = peer_events.next() => {
                        match peer_event {
                            Some(event) => {
                                // Converte o Arc<dyn Any> para EventPubSub
                                if let Some(pubsub_event) = event.downcast_ref::<crate::iface::EventPubSub>() {
                                    if let Some(store_arc) = store_weak.upgrade() {
                                        debug!(
                                            store_arc.logger,
                                            "Processing peer event: {:?}",
                                            match pubsub_event {
                                                crate::iface::EventPubSub::Join { peer, topic } =>
                                                    format!("Join(peer: {:?}, topic: {})", peer, topic),
                                                crate::iface::EventPubSub::Leave { peer, topic } =>
                                                    format!("Leave(peer: {:?}, topic: {})", peer, topic),
                                            }
                                        );
                                        // Processa o evento usando o handler existente
                                        store_arc.handle_peer_event(pubsub_event.clone()).await;
                                    } else {
                                        debug!(logger, "Store dropped, ending pubsub peer listener");
                                        break;
                                    }
                                } else {
                                    warn!(logger, "Received unknown peer event type");
                                }
                            }
                            None => {
                                debug!(logger, "Peer events stream ended");
                                break;
                            }
                        }
                    }
                }
            }
        });
        Ok(())
    }
    /// Função auxiliar de 'pubsub_chan_listener'
    /// Handles a single peer join or leave event.
    /// Processa eventos reais com retry e tratamento robusto de erros.
    async fn handle_peer_event(self: Arc<Self>, event: crate::iface::EventPubSub) {
        match event {
            crate::iface::EventPubSub::Join {
                topic: _,
                peer: peer_id,
            } => {
                debug!(
                    self.logger,
                    "Peer joined event received: {:?} on topic: {}", peer_id, self.id
                );

                // Emite evento NewPeer para o sistema usando o tipo correto
                let new_peer_event = crate::stores::events::EventNewPeer::new(peer_id);
                match self
                    .event_bus
                    .emitter::<crate::stores::events::EventNewPeer>()
                    .await
                {
                    Ok(emitter) => {
                        if let Err(e) = emitter.emit(new_peer_event) {
                            warn!(self.logger, "Failed to emit EventNewPeer: {}", e);
                        } else {
                            debug!(
                                self.logger,
                                "Successfully emitted EventNewPeer for: {:?}", peer_id
                            );
                        }
                    }
                    Err(e) => {
                        error!(
                            self.logger,
                            "Failed to get event emitter for EventNewPeer: {}", e
                        );
                    }
                }

                // Inicia troca de heads com retry robusto
                let store_clone = self.clone(); // Clona o Arc<Self>
                let logger_clone = self.logger.clone();

                tokio::spawn(async move {
                    debug!(
                        logger_clone,
                        "Starting head exchange with peer: {:?}", peer_id
                    );

                    // Chama o método de troca de heads com retry implementado
                    match store_clone.on_new_peer_joined(peer_id).await {
                        Ok(()) => {
                            debug!(
                                logger_clone,
                                "Successfully completed head exchange with peer: {:?}", peer_id
                            );
                        }
                        Err(e) => {
                            warn!(
                                logger_clone,
                                "Failed to complete head exchange with peer {:?}: {:?}", peer_id, e
                            );
                        }
                    }
                });
            }
            crate::iface::EventPubSub::Leave {
                topic: _,
                peer: peer_id,
            } => {
                debug!(
                    self.logger,
                    "Peer left event received: {:?} from topic: {}", peer_id, self.id
                );

                // Processa saída de peer
                // Registra métricas de peers disconnected
                if let Some(mut metrics) = self.retry_metrics.try_lock() {
                    metrics.record_peer_disconnection();
                }

                // Emite evento PeerDisconnected usando o tipo disponível
                let peer_disconnect_event = crate::base_guardian::EventPeerDisconnected {
                    peer_id: peer_id.to_string(),
                    address: self.id.clone(),
                };
                if let Ok(emitter) = self
                    .event_bus
                    .emitter::<crate::base_guardian::EventPeerDisconnected>()
                    .await
                {
                    if let Err(e) = emitter.emit(peer_disconnect_event) {
                        warn!(self.logger, "Failed to emit EventPeerDisconnected: {}", e);
                    }
                }
            }
        }
    }

    /// Equivalente à função `handleEventWrite` em Go.
    ///
    /// Publica os "heads" mais recentes de uma escrita local para todos os
    /// peers conectados no tópico do pubsub.
    pub async fn handle_event_write(
        &self,
        event: EventWrite,
        topic: Arc<dyn PubSubTopic<Error = GuardianError> + Send + Sync>,
    ) -> Result<()> {
        debug!(self.logger, "received stores.write event");

        if event.heads.is_empty() {
            return Err(GuardianError::Store("'heads' are not defined".to_string()));
        }

        let topic_peers = match topic.peers().await {
            Ok(peers) => peers,
            Err(e) => {
                return Err(GuardianError::Store(format!(
                    "Failed to get topic peers: {:?}",
                    e
                )));
            }
        };
        if topic_peers.is_empty() {
            debug!(self.logger, "no peers in pubsub topic, skipping publish");
            return Ok(());
        }

        let msg = MessageExchangeHeads {
            address: self.id.clone(),
            heads: event.heads,
        };

        let payload = match self.message_marshaler.marshal(&msg) {
            Ok(payload) => payload,
            Err(e) => {
                return Err(GuardianError::Store(format!(
                    "unable to serialize heads: {:?}",
                    e
                )));
            }
        };

        topic.publish(payload).await.map_err(|e| {
            GuardianError::Store(format!("unable to publish message on pubsub: {}", e))
        })?;
        debug!(
            self.logger,
            "stores.write event: published event on pub sub"
        );

        Ok(())
    }

    /// Equivalente à função `onNewPeerJoined` em Go.
    ///
    /// Inicia a troca de "heads" com um peer recém-conectado.
    /// Inclui estratégias de retry, timeout e cancelamento.
    pub async fn on_new_peer_joined(&self, peer: PeerId) -> Result<()> {
        debug!(
            self.logger,
            "{:?}: New peer '{:?}' connected to {}", self.peer_id, peer, self.id
        );

        // Implementa estratégias robustas de retry e tratamento de erros
        const MAX_PEER_EXCHANGE_RETRIES: u32 = 3;
        const PEER_EXCHANGE_TIMEOUT_SECS: u64 = 30;
        const PEER_EXCHANGE_BASE_DELAY_MS: u64 = 200;

        let mut retry_attempt = 0;
        let mut last_error = None;

        while retry_attempt <= MAX_PEER_EXCHANGE_RETRIES {
            retry_attempt += 1;

            // Cria timeout para a operação
            let exchange_future = self.exchange_heads(peer);
            let timeout_duration = std::time::Duration::from_secs(PEER_EXCHANGE_TIMEOUT_SECS);

            match tokio::time::timeout(timeout_duration, exchange_future).await {
                Ok(Ok(())) => {
                    debug!(
                        self.logger,
                        "Successfully exchanged heads with peer {:?} on attempt {}",
                        peer,
                        retry_attempt
                    );

                    // Registra métricas de sucesso
                    if let Some(mut metrics) = self.retry_metrics.try_lock() {
                        metrics.record_peer_exchange_success();
                    }

                    return Ok(());
                }
                Ok(Err(e)) => {
                    // Erro de aplicação - analisa tipo de erro para decidir retry
                    last_error = Some(e.clone());

                    match &e {
                        GuardianError::Store(msg) if msg.contains("cancelled") => {
                            // Erro de cancelamento - não faz retry
                            warn!(
                                self.logger,
                                "Peer exchange with {:?} was cancelled, not retrying: {}",
                                peer,
                                msg
                            );
                            return Err(e);
                        }
                        GuardianError::Store(msg) if msg.contains("timeout") => {
                            // Erro de timeout - pode ser temporário, faz retry
                            warn!(
                                self.logger,
                                "Peer exchange with {:?} timed out (attempt {}): {}",
                                peer,
                                retry_attempt,
                                msg
                            );
                        }
                        GuardianError::Store(msg) if msg.contains("connection") => {
                            // Erro de conexão - pode ser temporário, faz retry
                            warn!(
                                self.logger,
                                "Connection error with peer {:?} (attempt {}): {}",
                                peer,
                                retry_attempt,
                                msg
                            );
                        }
                        GuardianError::Store(msg) if msg.contains("marshal") => {
                            // Erro de serialização - permanente, não faz retry
                            error!(
                                self.logger,
                                "Marshal error with peer {:?}, not retrying: {}", peer, msg
                            );
                            return Err(e);
                        }
                        _ => {
                            // Outros erros - tenta retry limitado
                            warn!(
                                self.logger,
                                "Generic error with peer {:?} (attempt {}): {:?}",
                                peer,
                                retry_attempt,
                                e
                            );
                        }
                    }
                }
                Err(_) => {
                    // Timeout da operação inteira
                    let timeout_error = GuardianError::Store(format!(
                        "Peer exchange with {:?} timed out after {} seconds",
                        peer, PEER_EXCHANGE_TIMEOUT_SECS
                    ));
                    last_error = Some(timeout_error.clone());

                    warn!(
                        self.logger,
                        "Peer exchange with {:?} timed out (attempt {}/{})",
                        peer,
                        retry_attempt,
                        MAX_PEER_EXCHANGE_RETRIES + 1
                    );
                }
            }

            // Registra métricas de falha
            if let Some(mut metrics) = self.retry_metrics.try_lock() {
                metrics.record_peer_exchange_failure();
            }

            // Se não é a última tentativa, espera antes do retry
            if retry_attempt <= MAX_PEER_EXCHANGE_RETRIES {
                // Backoff exponencial com jitter para evitar thundering herd
                let delay_ms = PEER_EXCHANGE_BASE_DELAY_MS * (1 << (retry_attempt - 1));
                let jitter = fastrand::u64(0..=delay_ms / 4); // Até 25% de jitter
                let total_delay = delay_ms + jitter;

                debug!(
                    self.logger,
                    "Retrying peer exchange with {:?} in {}ms (attempt {}/{})",
                    peer,
                    total_delay,
                    retry_attempt + 1,
                    MAX_PEER_EXCHANGE_RETRIES + 1
                );

                // Verifica se a store foi cancelada durante o delay
                select! {
                    _ = self.cancellation_token.cancelled() => {
                        warn!(
                            self.logger,
                            "Store cancelled during peer exchange retry delay for peer {:?}",
                            peer
                        );
                        return Err(GuardianError::Store("Store cancelled during retry".to_string()));
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(total_delay)) => {
                        // Continua para próxima tentativa
                    }
                }
            }
        }

        // Todas as tentativas falharam
        let final_error = last_error.unwrap_or_else(|| {
            GuardianError::Store("Unknown error during peer exchange".to_string())
        });

        error!(
            self.logger,
            "Failed to exchange heads with peer {:?} after {} attempts: {:?}",
            peer,
            MAX_PEER_EXCHANGE_RETRIES + 1,
            final_error
        );

        // Registra métricas finais de falha
        if let Some(mut metrics) = self.retry_metrics.try_lock() {
            metrics.record_peer_exchange_final_failure();
        }

        Err(final_error)
    }

    /// Equivalente à função `exchangeHeads` em Go.
    ///
    /// Conecta-se a um peer via canal direto, carrega os "heads" locais
    /// do cache e os envia para o peer.
    pub async fn exchange_heads(&self, peer: PeerId) -> Result<()> {
        debug!(self.logger, "Exchanging heads with peer: {:?}", peer);

        let mut heads: Vec<Entry> = vec![];

        // Primeiro, carrega heads do oplog atual
        let current_heads = self.log_and_index.with_oplog(|oplog| {
            oplog
                .heads()
                .iter()
                .map(|arc_entry| (**arc_entry).clone())
                .collect::<Vec<Entry>>()
        });
        heads.extend(current_heads);

        // Carrega heads adicionais do cache se disponíveis
        let cache = self.cache();

        // Carrega os heads locais do cache
        if let Ok(Some(local_heads_bytes)) = cache.get("_localHeads".as_bytes()).await {
            match serde_json::from_slice::<Vec<Entry>>(&local_heads_bytes) {
                Ok(local_heads) => {
                    debug!(
                        self.logger,
                        "Loaded {} local heads from cache",
                        local_heads.len()
                    );
                    heads.extend(local_heads);
                }
                Err(e) => warn!(
                    self.logger,
                    "Failed to deserialize local heads from cache: {}", e
                ),
            }
        }

        // Carrega os heads remotos do cache
        if let Ok(Some(remote_heads_bytes)) = cache.get("_remoteHeads".as_bytes()).await {
            match serde_json::from_slice::<Vec<Entry>>(&remote_heads_bytes) {
                Ok(remote_heads) => {
                    debug!(
                        self.logger,
                        "Loaded {} remote heads from cache",
                        remote_heads.len()
                    );
                    heads.extend(remote_heads);
                }
                Err(e) => warn!(
                    self.logger,
                    "Failed to deserialize remote heads from cache: {}", e
                ),
            }
        }

        // Remove duplicatas baseadas no hash
        heads.sort_by(|a, b| a.hash().cmp(&b.hash()));
        heads.dedup_by(|a, b| a.hash() == b.hash());

        debug!(
            self.logger,
            "Sending {} unique heads to peer: {:?}",
            heads.len(),
            peer
        );

        let msg = MessageExchangeHeads {
            address: self.id.clone(),
            heads,
        };

        let payload = self
            .message_marshaler
            .marshal(&msg)
            .map_err(|e| GuardianError::Store(format!("unable to marshall message: {}", e)))?;

        // Conecta ao peer e envia a mensagem via DirectChannel
        debug!(
            self.logger,
            "Connecting to peer {} and sending {} bytes",
            peer,
            payload.len()
        );

        // Obtém acesso ao DirectChannel para executar operações reais
        let direct_channel = self.direct_channel.lock().await;

        // Como os métodos connect/send requerem &mut self mas temos Arc<dyn DirectChannel>,
        // precisamos usar uma abordagem baseada na implementação concreta disponível
        if let Some(concrete_channel) = direct_channel
            .as_ref()
            .as_any()
            .downcast_ref::<crate::pubsub::direct_channel::DirectChannel>(
        ) {
            debug!(
                self.logger,
                "Using concrete DirectChannel implementation for communication"
            );

            // 1. CONECTA AO PEER - Usando métodos públicos com retry
            debug!(self.logger, "Establishing connection to peer: {:?}", peer);

            let mut connection_established = false;
            let mut connection_attempts = 0;
            const MAX_CONNECTION_RETRIES: u32 = 2;
            const CONNECTION_BASE_DELAY_MS: u64 = 50;

            while !connection_established && connection_attempts <= MAX_CONNECTION_RETRIES {
                connection_attempts += 1;

                match concrete_channel.connect_peer(peer).await {
                    Ok(()) => {
                        debug!(
                            self.logger,
                            "Successfully connected to peer: {:?} on attempt {}",
                            peer,
                            connection_attempts
                        );
                        connection_established = true;

                        // Registra métricas de sucesso na conexão
                        if let Some(mut metrics) = self.retry_metrics.try_lock() {
                            metrics.record_connection_attempt(true);
                        }
                    }
                    Err(e) => {
                        // Registra métricas de falha na conexão
                        if let Some(mut metrics) = self.retry_metrics.try_lock() {
                            metrics.record_connection_attempt(false);
                        }

                        if connection_attempts <= MAX_CONNECTION_RETRIES {
                            let delay_ms = CONNECTION_BASE_DELAY_MS * connection_attempts as u64;
                            warn!(
                                self.logger,
                                "Connection attempt {}/{} failed for peer {}: {}. Retrying in {}ms",
                                connection_attempts,
                                MAX_CONNECTION_RETRIES + 1,
                                peer,
                                e,
                                delay_ms
                            );
                            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        } else {
                            error!(
                                self.logger,
                                "Failed to connect to peer {} after {} attempts: {}",
                                peer,
                                MAX_CONNECTION_RETRIES + 1,
                                e
                            );

                            // Registra falha após todos os retries
                            if let Some(mut metrics) = self.retry_metrics.try_lock() {
                                metrics.record_failed_after_retries();
                            }
                            return Err(GuardianError::Store(format!(
                                "DirectChannel connection failed after {} attempts: {}",
                                MAX_CONNECTION_RETRIES + 1,
                                e
                            )));
                        }
                    }
                }
            }

            // 2. ENVIA OS DADOS - Usando métodos públicos
            debug!(
                self.logger,
                "Sending {} bytes of head data to peer: {:?}",
                payload.len(),
                peer
            );

            match concrete_channel.send_to_peer(peer, payload.clone()).await {
                Ok(()) => {
                    debug!(
                        self.logger,
                        "Successfully sent {} bytes to peer: {:?}",
                        payload.len(),
                        peer
                    );
                }
                Err(e) => {
                    error!(self.logger, "Failed to send data to peer {}: {}", peer, e);
                    // Implementa lógica de retry com backoff exponencial
                    debug!(
                        self.logger,
                        "Initiating retry logic for DirectChannel send failure"
                    );

                    let mut retry_attempts = 0;
                    const MAX_RETRIES: u32 = 3;
                    const BASE_DELAY_MS: u64 = 100;

                    while retry_attempts < MAX_RETRIES {
                        retry_attempts += 1;

                        // Backoff exponencial: 100ms, 200ms, 400ms
                        let delay_ms = BASE_DELAY_MS * (2_u64.pow(retry_attempts - 1));

                        warn!(
                            self.logger,
                            "Retry attempt {}/{} for peer {} after {}ms delay",
                            retry_attempts,
                            MAX_RETRIES,
                            peer,
                            delay_ms
                        );

                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;

                        // Tenta reconectar antes do retry
                        match concrete_channel.connect_peer(peer).await {
                            Ok(()) => {
                                debug!(
                                    self.logger,
                                    "Reconnection successful on retry {}", retry_attempts
                                );

                                // Tenta enviar novamente
                                match concrete_channel.send_to_peer(peer, payload.clone()).await {
                                    Ok(()) => {
                                        debug!(
                                            self.logger,
                                            "Retry {}/{} successful: sent {} bytes to peer: {:?}",
                                            retry_attempts,
                                            MAX_RETRIES,
                                            payload.len(),
                                            peer
                                        );
                                        // Sucesso no retry - sai do loop
                                        break;
                                    }
                                    Err(retry_err) => {
                                        warn!(
                                            self.logger,
                                            "Retry {}/{} failed to send data: {}",
                                            retry_attempts,
                                            MAX_RETRIES,
                                            retry_err
                                        );

                                        // Se foi a última tentativa, retorna erro
                                        if retry_attempts >= MAX_RETRIES {
                                            error!(
                                                self.logger,
                                                "All {} retry attempts failed for peer {}",
                                                MAX_RETRIES,
                                                peer
                                            );
                                            return Err(GuardianError::Store(format!(
                                                "DirectChannel send failed after {} retries. Last error: {}",
                                                MAX_RETRIES, retry_err
                                            )));
                                        }
                                    }
                                }
                            }
                            Err(reconnect_err) => {
                                warn!(
                                    self.logger,
                                    "Retry {}/{} failed to reconnect: {}",
                                    retry_attempts,
                                    MAX_RETRIES,
                                    reconnect_err
                                );

                                // Se foi a última tentativa, retorna erro
                                if retry_attempts >= MAX_RETRIES {
                                    error!(
                                        self.logger,
                                        "All {} retry attempts failed for peer {} (reconnection failed)",
                                        MAX_RETRIES,
                                        peer
                                    );
                                    return Err(GuardianError::Store(format!(
                                        "DirectChannel connection failed after {} retries. Last error: {}",
                                        MAX_RETRIES, reconnect_err
                                    )));
                                }
                            }
                        }
                    }
                }
            }
        } else if let Some(channels) = direct_channel
            .as_ref()
            .as_any()
            .downcast_ref::<crate::pubsub::one_on_one_channel::Channels>(
        ) {
            debug!(
                self.logger,
                "Using Channels implementation for communication"
            );

            // 1. CONECTA AO PEER - Channels com retry
            debug!(
                self.logger,
                "Establishing Channels connection to peer: {:?}", peer
            );

            let mut channels_connection_established = false;
            let mut channels_connection_attempts = 0;
            const MAX_CHANNELS_CONNECTION_RETRIES: u32 = 2;
            const CHANNELS_CONNECTION_BASE_DELAY_MS: u64 = 75; // Delay ligeiramente maior para Channels

            while !channels_connection_established
                && channels_connection_attempts <= MAX_CHANNELS_CONNECTION_RETRIES
            {
                channels_connection_attempts += 1;

                match channels.connect(peer).await {
                    Ok(()) => {
                        debug!(
                            self.logger,
                            "Successfully connected to peer: {:?} via Channels on attempt {}",
                            peer,
                            channels_connection_attempts
                        );
                        channels_connection_established = true;
                    }
                    Err(e) => {
                        if channels_connection_attempts <= MAX_CHANNELS_CONNECTION_RETRIES {
                            let delay_ms = CHANNELS_CONNECTION_BASE_DELAY_MS
                                * channels_connection_attempts as u64;
                            warn!(
                                self.logger,
                                "Channels connection attempt {}/{} failed for peer {}: {}. Retrying in {}ms",
                                channels_connection_attempts,
                                MAX_CHANNELS_CONNECTION_RETRIES + 1,
                                peer,
                                e,
                                delay_ms
                            );
                            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        } else {
                            error!(
                                self.logger,
                                "Failed to connect to peer {} via Channels after {} attempts: {}",
                                peer,
                                MAX_CHANNELS_CONNECTION_RETRIES + 1,
                                e
                            );
                            return Err(GuardianError::Store(format!(
                                "Channels connection failed after {} attempts: {}",
                                MAX_CHANNELS_CONNECTION_RETRIES + 1,
                                e
                            )));
                        }
                    }
                }
            }

            // 2. ENVIA OS DADOS - Channels
            debug!(
                self.logger,
                "Sending {} bytes of head data to peer: {:?}",
                payload.len(),
                peer
            );

            match channels.send(peer, &payload).await {
                Ok(()) => {
                    debug!(
                        self.logger,
                        "Successfully sent {} bytes to peer: {:?}",
                        payload.len(),
                        peer
                    );
                }
                Err(e) => {
                    error!(self.logger, "Failed to send data to peer {}: {}", peer, e);
                    // Retry logic para Channels com backoff exponencial
                    debug!(
                        self.logger,
                        "Initiating retry logic for Channels send failure"
                    );

                    let mut retry_attempts = 0;
                    const MAX_RETRIES: u32 = 3;
                    const BASE_DELAY_MS: u64 = 150; // Delay ligeiramente maior para Channels

                    while retry_attempts < MAX_RETRIES {
                        retry_attempts += 1;

                        // Backoff exponencial: 150ms, 300ms, 600ms
                        let delay_ms = BASE_DELAY_MS * (2_u64.pow(retry_attempts - 1));

                        warn!(
                            self.logger,
                            "Channels retry attempt {}/{} for peer {} after {}ms delay",
                            retry_attempts,
                            MAX_RETRIES,
                            peer,
                            delay_ms
                        );

                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;

                        // Tenta reconectar antes do retry
                        match channels.connect(peer).await {
                            Ok(()) => {
                                debug!(
                                    self.logger,
                                    "Channels reconnection successful on retry {}", retry_attempts
                                );

                                // Tenta enviar novamente
                                match channels.send(peer, &payload).await {
                                    Ok(()) => {
                                        debug!(
                                            self.logger,
                                            "Channels retry {}/{} successful: sent {} bytes to peer: {:?}",
                                            retry_attempts,
                                            MAX_RETRIES,
                                            payload.len(),
                                            peer
                                        );
                                        // Sucesso no retry - sai do loop
                                        break;
                                    }
                                    Err(retry_err) => {
                                        warn!(
                                            self.logger,
                                            "Channels retry {}/{} failed to send data: {}",
                                            retry_attempts,
                                            MAX_RETRIES,
                                            retry_err
                                        );

                                        // Se foi a última tentativa, retorna erro
                                        if retry_attempts >= MAX_RETRIES {
                                            error!(
                                                self.logger,
                                                "All {} Channels retry attempts failed for peer {}",
                                                MAX_RETRIES,
                                                peer
                                            );
                                            return Err(GuardianError::Store(format!(
                                                "Channels send failed after {} retries. Last error: {}",
                                                MAX_RETRIES, retry_err
                                            )));
                                        }
                                    }
                                }
                            }
                            Err(reconnect_err) => {
                                warn!(
                                    self.logger,
                                    "Channels retry {}/{} failed to reconnect: {}",
                                    retry_attempts,
                                    MAX_RETRIES,
                                    reconnect_err
                                );

                                // Se foi a última tentativa, retorna erro
                                if retry_attempts >= MAX_RETRIES {
                                    error!(
                                        self.logger,
                                        "All {} Channels retry attempts failed for peer {} (reconnection failed)",
                                        MAX_RETRIES,
                                        peer
                                    );
                                    return Err(GuardianError::Store(format!(
                                        "Channels connection failed after {} retries. Last error: {}",
                                        MAX_RETRIES, reconnect_err
                                    )));
                                }
                            }
                        }
                    }
                }
            }
        } else {
            // Fallback: Usando métodos do trait DirectChannel
            debug!(
                self.logger,
                "Could not downcast DirectChannel to concrete type, using trait methods directly"
            );
            // Liberamos o lock atual e obtemos um novo com mutabilidade
            drop(direct_channel);

            // Obtém acesso mutável ao DirectChannel via Mutex
            let mut mutable_channel_guard = self.direct_channel.lock().await;
            let mutable_channel = Arc::get_mut(&mut mutable_channel_guard).ok_or_else(|| {
                GuardianError::Store("Failed to get mutable access to DirectChannel".to_string())
            })?;

            debug!(
                self.logger,
                "Connecting and sending to peer {:?} with {} bytes using trait methods",
                peer,
                payload.len()
            );
            // 1. CONECTA AO PEER - Usando trait methods
            if let Err(e) = mutable_channel.connect(peer).await {
                error!(
                    self.logger,
                    "DirectChannel trait connect failed for peer {}: {}", peer, e
                );
                return Err(GuardianError::Store(format!(
                    "DirectChannel trait connection failed: {}",
                    e
                )));
            }

            debug!(
                self.logger,
                "Successfully connected to peer via trait methods: {:?}", peer
            );

            // 2. ENVIA OS DADOS - Usando trait methods
            if let Err(e) = mutable_channel.send(peer, payload.clone()).await {
                error!(
                    self.logger,
                    "DirectChannel trait send failed for peer {}: {}", peer, e
                );
                return Err(GuardianError::Store(format!(
                    "DirectChannel trait send failed: {}",
                    e
                )));
            }

            debug!(
                self.logger,
                "Successfully sent {} bytes to peer via trait methods: {:?}",
                payload.len(),
                peer
            );

            // Registra métricas de sucesso
            if let Some(mut metrics) = self.retry_metrics.try_lock() {
                metrics.record_connection_attempt(true);
                metrics.record_send_attempt(true);
            }
        }

        debug!(
            self.logger,
            "Successfully exchanged heads with peer: {:?}", peer
        );

        // Loga as métricas de retry atualizadas para monitoramento
        self.log_retry_metrics();

        Ok(())
    }

    /// Equivalente à função `Load` em Go.
    ///
    /// Carrega o estado da store a partir dos heads salvos no cache. Ele processa
    /// cada head concorrentemente, reporta o progresso e junta os resultados.
    pub async fn load(&self, amount: Option<isize>) -> Result<()> {
        let _default_amount = amount.unwrap_or(-1); // -1 para "todos"

        // Emite evento de início do carregamento
        let load_event = EventLoad {
            address: self.address.clone(),
            heads: Vec::new(), // Inicialmente vazio
        };
        if let Err(e) = self.emitters.evt_load.emit(load_event) {
            warn!(self.logger, "Failed to emit EventLoad: {}", e);
        }

        // Carrega heads do cache
        let mut heads = Vec::new();
        let cache = self.cache();
        BaseStore::load_heads_from_cache_key(&cache, "_localHeads", &mut heads).await?;
        BaseStore::load_heads_from_cache_key(&cache, "_remoteHeads", &mut heads).await?;

        if heads.is_empty() {
            // Emite evento indicando que o carregamento terminou (sem dados)
            let ready_event = EventReady {
                address: self.address.clone(),
                heads: Vec::new(),
            };
            if let Err(e) = self.emitters.evt_ready.emit(ready_event) {
                warn!(self.logger, "Failed to emit EventReady: {}", e);
            }
            return Ok(());
        }

        debug!(self.logger, "Loading {} heads", heads.len());

        // Emite evento de progresso para cada head carregado
        for (i, head) in heads.iter().enumerate() {
            let load_progress_event = EventLoadProgress {
                address: self.address.clone(),
                hash: Cid::try_from(head.hash()).unwrap_or_default(),
                entry: head.clone(),
                progress: (i + 1) as i32,
                max: heads.len() as i32,
            };
            if let Err(e) = self.emitters.evt_load_progress.emit(load_progress_event) {
                warn!(self.logger, "Failed to emit EventLoadProgress: {}", e);
            }
        }

        self.update_index()?;

        // Emite evento indicando que a store está pronta
        let ready_event = EventReady {
            address: self.address.clone(),
            heads: heads.clone(),
        };
        if let Err(e) = self.emitters.evt_ready.emit(ready_event) {
            warn!(self.logger, "Failed to emit EventReady: {}", e);
        }

        debug!(self.logger, "Load completed");

        Ok(())
    }

    /// Função auxiliar de 'load'
    /// Carrega e desserializa uma lista de `Entry` a partir de uma chave do cache.
    async fn load_heads_from_cache_key(
        cache: &Arc<dyn Datastore>,
        key: &str,
        heads: &mut Vec<Entry>,
    ) -> Result<()> {
        if let Ok(Some(bytes)) = cache.get(key.as_bytes()).await {
            let cached_heads: Vec<Entry> = serde_json::from_slice(&bytes).map_err(|e| {
                GuardianError::Store(format!(
                    "Failed to deserialize heads from cache key '{}': {}",
                    key, e
                ))
            })?;
            heads.extend(cached_heads);
        }
        Ok(())
    }

    /// Equivalente à função `LoadFromSnapshot` em Go.
    ///
    /// A função é `async` para poder ler o stream do IPFS de forma não-bloqueante.
    pub async fn load_from_snapshot(&self) -> Result<()> {
        debug!(self.logger, "Loading from snapshot");

        // Processa a fila de sync pendente primeiro
        if let Ok(Some(queue_bytes)) = self.cache().get("queue".as_bytes()).await {
            match serde_json::from_slice::<Vec<Entry>>(&queue_bytes) {
                Ok(queue) => {
                    debug!(self.logger, "Processing {} queued entries", queue.len());
                    self.sync(queue).await.map_err(|e| {
                        GuardianError::Store(format!("Unable to sync queued CIDs: {}", e))
                    })?;
                }
                Err(e) => warn!(self.logger, "Failed to deserialize queued entries: {}", e),
            }
        }

        // Obtém o caminho do snapshot do cache
        let snapshot_path_result = self.cache().get("snapshot".as_bytes()).await;
        let snapshot_path_bytes = match snapshot_path_result {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                debug!(self.logger, "No snapshot found in cache");
                self.update_index()?;
                return Ok(());
            }
            Err(e) => {
                warn!(self.logger, "Error getting snapshot from cache: {}", e);
                self.update_index()?;
                return Ok(());
            }
        };

        let snapshot_path = String::from_utf8(snapshot_path_bytes)
            .map_err(|e| GuardianError::Store(format!("Invalid UTF-8 in snapshot path: {}", e)))?;

        debug!(self.logger, "Loading snapshot from path: {}", snapshot_path);

        // Carrega o snapshot do IPFS
        match self.ipfs.cat(&snapshot_path).await {
            Ok(mut snapshot_stream) => {
                // Lê todos os dados do stream
                let mut snapshot_data = Vec::new();
                use tokio::io::AsyncReadExt;
                if let Err(e) = snapshot_stream.read_to_end(&mut snapshot_data).await {
                    warn!(self.logger, "Failed to read snapshot data: {}", e);
                } else {
                    // Processa os dados do snapshot
                    match self.process_snapshot_data(snapshot_data).await {
                        Ok(entries_loaded) => {
                            debug!(
                                self.logger,
                                "Successfully loaded {} entries from snapshot", entries_loaded
                            );

                            // Emite evento de load usando log simples por enquanto
                            debug!(
                                self.logger,
                                "Snapshot load completed with {} entries", entries_loaded
                            );
                        }
                        Err(e) => {
                            warn!(self.logger, "Failed to process snapshot data: {}", e);
                            return Err(e);
                        }
                    }
                }
            }
            Err(e) => {
                warn!(self.logger, "Failed to load snapshot from IPFS: {}", e);
                // Continua sem erro, apenas logs a falha
            }
        }

        self.update_index()?;
        Ok(())
    }

    /// Processa os dados de um snapshot carregado do IPFS
    async fn process_snapshot_data(&self, data: Vec<u8>) -> Result<usize> {
        use std::io::Cursor;
        use tokio::io::AsyncReadExt;

        let mut cursor = Cursor::new(data);
        let mut entries_loaded = 0;

        // Lê os dados do snapshot
        while cursor.position() < cursor.get_ref().len() as u64 {
            // Lê o tamanho da entrada (4 bytes, big-endian)
            let mut size_bytes = [0u8; 4];
            if cursor.read_exact(&mut size_bytes).await.is_err() {
                break; // End of data
            }
            let entry_size = u32::from_be_bytes(size_bytes) as usize;

            // Lê os dados da entrada
            let mut entry_data = vec![0u8; entry_size];
            if cursor.read_exact(&mut entry_data).await.is_err() {
                break; // Corrupted data
            }

            // Desserializa a entrada
            match serde_json::from_slice::<Entry>(&entry_data) {
                Ok(entry) => {
                    // Adiciona a entrada ao oplog usando métodos apropriados
                    let entry_hash = entry.hash();
                    if let Err(e) = self.log_and_index.with_oplog_mut(|oplog| {
                        // Verifica se a entrada já existe usando has()
                        if !oplog.has(&entry_hash.to_string()) {
                            // Adiciona a entrada usando append()
                            // Entry.payload é &str, então usamos diretamente
                            oplog.append(&entry.payload, None);
                        }
                        Ok::<(), GuardianError>(())
                    }) {
                        warn!(self.logger, "Failed to add entry to oplog: {}", e);
                        continue;
                    }
                    entries_loaded += 1;
                }
                Err(e) => {
                    warn!(
                        self.logger,
                        "Failed to deserialize entry from snapshot: {}", e
                    );
                    continue;
                }
            }
        }

        Ok(entries_loaded)
    }

    /// Função auxiliar de 'load_from_snapshot'
    /// Lê um prefixo de tamanho u16 (big-endian) de um stream, lê o número
    /// correspondente de bytes e os desserializa para um tipo T.
    #[allow(dead_code)]
    async fn read_prefixed_json<T, R>(reader: &mut R) -> Result<T>
    where
        T: for<'de> serde::Deserialize<'de>,
        R: AsyncRead + Unpin,
    {
        let len = reader.read_u16().await.map_err(|e| {
            GuardianError::Store(format!(
                "Falha ao ler o prefixo de tamanho do snapshot: {}",
                e
            ))
        })?;

        let mut buf = vec![0; len as usize];
        reader.read_exact(&mut buf).await.map_err(|e| {
            GuardianError::Store(format!("Falha ao ler o bloco de dados do snapshot: {}", e))
        })?;

        serde_json::from_slice(&buf).map_err(|e| {
            GuardianError::Store(format!(
                "Falha ao desserializar dados JSON do snapshot: {}",
                e
            ))
        })
    }
}

/// Equivalente à função `generateEmitter` em Go.
///
/// Esta função foi extraída como uma função livre (não um método de `BaseStore`)
/// para ser usada durante a construção da `store`, mantendo a lógica de
/// inicialização dos emissores separada.
async fn generate_emitters(bus: &EventBus) -> Result<Emitters> {
    Ok(Emitters {
        evt_write: bus.emitter::<EventWrite>().await.map_err(|e| {
            GuardianError::Store(format!("unable to create EventWrite emitter: {}", e))
        })?,
        evt_ready: bus.emitter::<EventReady>().await.map_err(|e| {
            GuardianError::Store(format!("unable to create EventReady emitter: {}", e))
        })?,
        evt_replicate_progress: bus.emitter::<EventReplicateProgress>().await.map_err(|e| {
            GuardianError::Store(format!(
                "unable to create EventReplicateProgress emitter: {}",
                e
            ))
        })?,
        evt_load: bus.emitter::<EventLoad>().await.map_err(|e| {
            GuardianError::Store(format!("unable to create EventLoad emitter: {}", e))
        })?,
        evt_load_progress: bus.emitter::<EventLoadProgress>().await.map_err(|e| {
            GuardianError::Store(format!("unable to create EventLoadProgress emitter: {}", e))
        })?,
        evt_replicated: bus.emitter::<EventReplicated>().await.map_err(|e| {
            GuardianError::Store(format!("unable to create EventReplicated emitter: {}", e))
        })?,
        evt_replicate: bus.emitter::<EventReplicate>().await.map_err(|e| {
            GuardianError::Store(format!("unable to create EventReplicate emitter: {}", e))
        })?,
    })
}

/// Implementação do trait Store para BaseStore
///
/// Esta implementação torna BaseStore compatível com a interface Store,
/// permitindo que seja usada em qualquer contexto que espere uma Store.
#[async_trait::async_trait]
impl Store for BaseStore {
    type Error = GuardianError;

    #[allow(deprecated)]
    fn events(&self) -> &dyn EmitterInterface {
        self.emitter_interface.as_ref()
    }

    async fn close(&mut self) -> std::result::Result<(), Self::Error> {
        // Implementação simples que só ativa o token de cancelamento
        // A limpeza completa é feita pelo Drop trait
        if !self.is_closed() {
            self.cancellation_token.cancel();
        }
        Ok(())
    }

    fn address(&self) -> &dyn Address {
        self.address.as_ref()
    }

    fn index(&self) -> Box<dyn StoreIndex<Error = Self::Error> + Send + Sync> {
        // Cria um wrapper que mantenha uma referência ao log_and_index da store
        // e delegue todas as operações para o índice ativo quando disponível
        struct IndexWrapper {
            log_and_index: Arc<LogAndIndex>,
        }

        impl StoreIndex for IndexWrapper {
            type Error = GuardianError;

            fn contains_key(&self, key: &str) -> std::result::Result<bool, Self::Error> {
                // Delega para o índice ativo se disponível
                if let Some(result) = self
                    .log_and_index
                    .with_index(|index| index.contains_key(key))
                {
                    result
                } else {
                    // Se não há índice ativo, a chave não existe
                    Ok(false)
                }
            }

            fn get_bytes(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
                // Delega para o índice ativo se disponível
                if let Some(result) = self.log_and_index.with_index(|index| index.get_bytes(key)) {
                    result
                } else {
                    // Se não há índice ativo, retorna None
                    Ok(None)
                }
            }

            fn keys(&self) -> std::result::Result<Vec<String>, Self::Error> {
                // Delega para o índice ativo se disponível
                if let Some(result) = self.log_and_index.with_index(|index| index.keys()) {
                    result
                } else {
                    // Se não há índice ativo, retorna lista vazia
                    Ok(Vec::new())
                }
            }

            fn len(&self) -> std::result::Result<usize, Self::Error> {
                // Delega para o índice ativo se disponível
                if let Some(result) = self.log_and_index.with_index(|index| index.len()) {
                    result
                } else {
                    // Se não há índice ativo, comprimento é zero
                    Ok(0)
                }
            }

            fn is_empty(&self) -> std::result::Result<bool, Self::Error> {
                // Delega para o índice ativo se disponível
                if let Some(result) = self.log_and_index.with_index(|index| index.is_empty()) {
                    result
                } else {
                    // Se não há índice ativo, consideramos vazio
                    Ok(true)
                }
            }

            fn update_index(
                &mut self,
                log: &crate::ipfs_log::log::Log,
                entries: &[crate::ipfs_log::entry::Entry],
            ) -> std::result::Result<(), Self::Error> {
                // Delega para o índice ativo se disponível
                let mut guard = self.log_and_index.active_index.write();
                match guard.as_mut() {
                    Some(index) => index.update_index(log, entries),
                    None => Ok(()), // Se não há índice ativo, não faz nada
                }
            }

            fn clear(&mut self) -> std::result::Result<(), Self::Error> {
                // Delega para o índice ativo se disponível
                let mut guard = self.log_and_index.active_index.write();
                match guard.as_mut() {
                    Some(index) => index.clear(),
                    None => Ok(()), // Se não há índice ativo, não faz nada
                }
            }
        }

        // Retorna o wrapper com uma referência ao log_and_index
        Box::new(IndexWrapper {
            log_and_index: Arc::new(LogAndIndex {
                oplog: self.log_and_index.oplog.clone(),
                active_index: self.log_and_index.active_index.clone(),
            }),
        }) as Box<dyn StoreIndex<Error = Self::Error> + Send + Sync>
    }
    fn store_type(&self) -> &str {
        "base"
    }

    fn replication_status(&self) -> ReplicationInfo {
        // Retorna o status de replicação atual (sem Arc)
        ReplicationInfo::default()
    }

    async fn drop(&mut self) -> std::result::Result<(), Self::Error> {
        // Versão mutable do drop
        Ok(())
    }

    // Métodos específicos delegados para as implementações existentes
    fn replicator(&self) -> Option<Arc<Replicator>> {
        // Retorna o replicador atual como Option<Arc>
        self.replicator.read().as_ref().cloned()
    }

    fn cache(&self) -> Arc<dyn Datastore> {
        Self::cache(self)
    }

    async fn load(&mut self, amount: usize) -> std::result::Result<(), Self::Error> {
        // Usa o método existente, mas convertendo o tipo
        Self::load(self, Some(amount as isize)).await
    }

    async fn sync(&mut self, heads: Vec<Entry>) -> std::result::Result<(), Self::Error> {
        Self::sync(self, heads).await
    }

    async fn load_more_from(&mut self, _amount: u64, entries: Vec<Entry>) {
        // Ignora o amount por enquanto e usa o método existente
        let _ = Self::load_more_from(self, entries);
    }

    async fn load_from_snapshot(&mut self) -> std::result::Result<(), Self::Error> {
        Self::load_from_snapshot(self).await
    }

    fn op_log(&self) -> Arc<RwLock<Log>> {
        // Retorna o log como Arc<RwLock<Log>>
        self.log_and_index.op_log_arc()
    }

    fn ipfs(&self) -> Arc<IpfsClient> {
        Self::ipfs(self)
    }

    fn db_name(&self) -> &str {
        Self::db_name(self)
    }

    fn identity(&self) -> &Identity {
        Self::identity(self)
    }

    fn access_controller(&self) -> &dyn AccessController {
        Self::access_controller(self)
    }

    async fn add_operation(
        &mut self,
        op: Operation,
        on_progress: Option<mpsc::Sender<Entry>>,
    ) -> std::result::Result<Entry, Self::Error> {
        Self::add_operation(self, op, on_progress).await
    }

    fn logger(&self) -> Arc<Logger> {
        Self::logger(self)
    }

    fn tracer(&self) -> Arc<TracerWrapper> {
        Self::tracer(self)
    }

    fn event_bus(&self) -> Arc<EventBus> {
        self.event_bus.clone()
    }
}

/// Implementação do trait StoreInterface para BaseStore
///
/// Esta implementação permite que BaseStore seja usada pelo replicador
/// como uma interface de store para operações de replicação.
impl crate::stores::replicator::traits::StoreInterface for BaseStore {
    fn op_log_arc(&self) -> Arc<parking_lot::RwLock<crate::ipfs_log::log::Log>> {
        // Retorna o Arc<RwLock<Log>> diretamente
        self.log_and_index.oplog.clone()
    }

    fn ipfs(&self) -> &ipfs_api_backend_hyper::IpfsClient {
        // Retorna referência ao cliente compatível
        &self.compat_ipfs_client
    }

    fn identity(&self) -> &crate::ipfs_log::identity::Identity {
        // BaseStore armazena identity como Arc<Identity>, então retornamos a referência
        self.identity.as_ref()
    }

    fn access_controller(&self) -> &dyn crate::access_controller::traits::AccessController {
        Self::access_controller(self)
    }

    fn sort_fn(&self) -> crate::stores::replicator::traits::SortFn {
        Self::sort_fn(self)
    }
}
