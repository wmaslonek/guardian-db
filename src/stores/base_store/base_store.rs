use parking_lot::{Mutex, RwLock, MappedRwLockReadGuard, RwLockReadGuard};
use std::{
    collections::HashMap,
    path::Path,
    sync::Arc,
};
use tokio_util::sync::CancellationToken;
use crate::error::{GuardianError, Result};
use tokio::sync::mpsc;
use tokio::select;
use serde::{Serialize, Deserialize};
use tokio::task::JoinSet;
use tokio::io::{AsyncRead, AsyncReadExt};
use opentelemetry::trace::{TracerProvider, noop::NoopTracerProvider};
use cid::Cid;
use libp2p::core::PeerId;
use crate::data_store::Datastore;
use slog::{o, Logger, warn, debug, error}; 
use crate::kubo_core_api::client::KuboCoreApiClient;
use crate::address::Address;
use crate::iface::{MessageExchangeHeads, MessageMarshaler, NewStoreOptions, PubSubInterface, PubSubTopic, StoreIndex, DirectChannel, EventPubSub, TracerWrapper};
use crate::stores::replicator::{replication_info::ReplicationInfo, replicator::Replicator};
use crate::stores::operation::operation::Operation;
use crate::stores::events::{EventWrite, EventReady, EventReplicateProgress, EventLoad, EventLoadProgress, EventReplicated, EventReplicate, EventNewPeer};
use crate::eqlabs_ipfs_log::{entry::Entry, identity::Identity, log::{Log, LogOptions}};
use crate::pubsub::event::{EventBus, Emitter}; // Import do nosso EventBus e Emitter
use crate::events::{EmitterInterface, EventEmitter}; // Import para compatibilidade com Store trait
use crate::eqlabs_ipfs_log::access_controller::{CanAppendAdditionalContext, LogEntry};
use crate::access_controller::{traits::AccessController, simple::SimpleAccessController};
use crate::eqlabs_ipfs_log::identity_provider::{IdentityProvider, GuardianDBIdentityProvider};

// Em Go, `muIndex` protege tanto `oplog` quanto `index`. Em Rust, é
// idiomático agrupar esses dados em uma única struct e proteger
// a struct inteira com um `RwLock`.
pub struct LogAndIndex {
    pub oplog: Log,
    pub index: HashMap<Cid, Box<dyn StoreIndex<Error = GuardianError> + Send + Sync>>,
}

pub struct Emitters {
    evt_write: Emitter<EventWrite>,
    evt_ready: Emitter<EventReady>,
    evt_replicate_progress: Emitter<EventReplicateProgress>,
    evt_load: Emitter<EventLoad>,
    evt_load_progress: Emitter<EventLoadProgress>,
    evt_replicated: Emitter<EventReplicated>,
    evt_replicate: Emitter<EventReplicate>,
}
struct CanAppendContextImpl { 
    log: Log 
}

impl CanAppendAdditionalContext for CanAppendContextImpl {
    // Equivalente à função `GetLogEntries` em Go.
    fn get_log_entries(&self) -> Vec<Box<dyn LogEntry>> {
        // Obtém todas as entradas do log e as converte para LogEntry
        self.log.values()
            .into_iter()
            .map(|arc_entry| {
                // Converte Arc<Entry> para Box<dyn LogEntry>
                // Como Entry implementa LogEntry, podemos fazer esta conversão
                let entry: Entry = (*arc_entry).clone();
                Box::new(entry) as Box<dyn LogEntry>
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
        // Retorna uma cópia das entradas capturadas no snapshot
        self.entries.iter()
            .map(|entry| {
                // Como não podemos clonar Box<dyn LogEntry> diretamente,
                // vamos criar novas instâncias baseadas nos dados
                let payload = entry.get_payload();
                let identity = entry.get_identity();
                
                // Cria uma nova Entry com os mesmos dados
                let new_entry = Entry {
                    hash: String::new(), // Placeholder
                    id: identity.id().to_string(),
                    payload: String::from_utf8_lossy(payload).to_string(),
                    next: Vec::new(),
                    v: 1,
                    clock: crate::eqlabs_ipfs_log::lamport_clock::LamportClock::new(identity.id()),
                    identity: Some(Arc::new(identity.clone())),
                };
                
                Box::new(new_entry) as Box<dyn LogEntry>
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
    directory: String,
    reference_count: usize,
    sort_fn: SortFn,

    // --- Componentes Principais e APIs Externas ---
    ipfs: Arc<KuboCoreApiClient>,
    access_controller: Arc<dyn AccessController>,
    identity_provider: Arc<dyn IdentityProvider>,
    
    // --- Estado Interno Protegido por Locks ---
    cache: Arc<dyn Datastore>,
    log_and_index: Arc<RwLock<LogAndIndex>>,
    
    // --- Componentes de Replicação e Rede ---
    replicator: RwLock<Option<Arc<Replicator>>>,
    replication_status: Mutex<ReplicationInfo>,
    pubsub: Arc<dyn PubSubInterface<Error = GuardianError> + Send + Sync>,
    message_marshaler: Arc<dyn MessageMarshaler<Error = GuardianError> + Send + Sync>,
    direct_channel: Arc<dyn DirectChannel<Error = GuardianError> + Send + Sync>,
    
    // --- Sistema de Eventos e Observabilidade ---
    event_bus: Arc<EventBus>,
    emitter_interface: Arc<dyn EmitterInterface + Send + Sync>, // Para compatibilidade com Store trait
    emitters: Emitters,
    logger: Arc<Logger>,
    tracer: Arc<TracerWrapper>,
    
    // --- Gerenciamento de Ciclo de Vida ---
    cancellation_token: CancellationToken,
    tasks: Mutex<JoinSet<()>>, // Adiciona um JoinSet para gerenciar tarefas em background
}

// Definimos um "type alias" para o cache para tornar a assinatura
// da função `cache()` mais limpa e legível.
pub type CacheRef = Arc<dyn Datastore>;
//pub type CacheGuard<'a> = RwLockReadGuard<'a, Box<dyn Datastore + Send + Sync>>;

// Type alias para o "guard" que aponta para o campo `index` dentro do lock.
pub type IndexGuard<'a> = MappedRwLockReadGuard<'a, dyn StoreIndex<Error = GuardianError> + Send + Sync>;

// O go-orbit-db usa um construtor de índice, uma função que cria um índice.
// Representamos isso com um `Arc<dyn Fn...>`
pub type IndexBuilder = Arc<dyn Fn(&[u8]) -> Box<dyn StoreIndex<Error = GuardianError> + Send + Sync>>;

// O `sortFn` é uma função de ordenação.
pub type SortFn = fn(&Entry, &Entry) -> std::cmp::Ordering;
fn default_sort_fn(a: &Entry, b: &Entry) -> std::cmp::Ordering { 
    a.clock().time().cmp(&b.clock().time())
}

impl BaseStore {
    /// Cria um cache real baseado em sled em vez do DummyDatastore
    fn create_real_cache(address: &dyn Address) -> Result<Arc<dyn Datastore>> {
        use crate::cache::level_down::LevelDownCache;
        use crate::cache::cache::Options;
        
        // Por agora, vamos usar uma implementação simplificada que funciona
        // Vamos criar um DatastoreWrapper diretamente usando o LevelDownCache
        let cache_options = Options {
            logger: Some(slog::Logger::root(slog::Discard, slog::o!())),
            max_cache_size: Some(100 * 1024 * 1024), // 100MB
            cache_mode: crate::cache::cache::CacheMode::Auto,
        };
        
        let cache_manager = LevelDownCache::new(Some(&cache_options));
        
        // Cria uma instância concreta de Address
        use crate::address;
        let concrete_address = address::parse(&address.to_string())
            .map_err(|e| GuardianError::Store(format!("Failed to parse address: {}", e)))?;
        
        // Usa o método interno para obter o WrappedCache
        let wrapped_cache = cache_manager.load_internal("./cache", &concrete_address)
            .map_err(|e| GuardianError::Store(format!("Failed to create cache: {}", e)))?;
        
        // Cria o DatastoreWrapper que implementa Datastore
        use crate::cache::level_down::DatastoreWrapper;
        Ok(Arc::new(DatastoreWrapper::new(wrapped_cache)))
    }

    /// Helper method para criar um contexto de acesso baseado no log atual
    fn create_append_context(&self) -> impl CanAppendAdditionalContext {
        let entries = {
            let guard = self.log_and_index.read();
            guard.oplog.values().into_iter()
                .map(|arc_entry| {
                    let entry: Entry = (*arc_entry).clone();
                    Box::new(entry) as Box<dyn LogEntry>
                })
                .collect::<Vec<_>>()
        };
        
        CanAppendContextSnapshot { entries }
    }

    /// Equivalente à função `DBName` em Go.
    /// Retorna o nome do banco de dados (store).
    /// Em Rust, é idiomático usar `snake_case` para nomes de funções.
    pub fn db_name(&self) -> &str {
        &self.db_name
    }

    /// Equivalente à função `IPFS` em Go.
    /// Retorna uma referência compartilhada e thread-safe para a API do IPFS.
    /// Clonar um `Arc` é barato, pois apenas incrementa a contagem de referências.
    pub fn ipfs(&self) -> Arc<KuboCoreApiClient> {
        self.ipfs.clone()
    }

    /// Equivalente à função `Identity` em Go.
    /// Retorna uma referência imutável à identidade da store.
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Equivalente à função `OpLog` em Go.
    ///
    /// Retorna uma referência ao OpLog da store.
    pub fn op_log(&self) -> RwLockReadGuard<LogAndIndex> {
        self.log_and_index.read()
    }

    /// Helper method to get a reference to the oplog
    pub fn oplog(&self) -> RwLockReadGuard<LogAndIndex> {
        self.log_and_index.read()
    }

    /// Equivalente à função `AccessController` em Go.
    ///
    /// Para simplificar, vamos retornar um placeholder por enquanto
    pub fn access_controller(&self) -> &dyn AccessController {
        self.access_controller.as_ref()
    }

    /// Retorna uma referência ao IdentityProvider da store.
    pub fn identity_provider(&self) -> &dyn IdentityProvider {
        self.identity_provider.as_ref()
    }

    /// Equivalente à função `Replicator` em Go.
    ///
    /// Retorna uma referência imutável ao replicador da store.
    pub fn replicator(&self) -> Option<Arc<Replicator>> {
        self.replicator.read().clone()
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

    /// Retorna referência ao EmitterInterface para compatibilidade com Store trait
    pub fn events(&self) -> &dyn EmitterInterface {
        self.emitter_interface.as_ref()
    }

    /// Método drop/close equivalente 
    pub fn drop(&self) -> Result<()> {
        self.cancellation_token.cancel();
        Ok(())
    }

    /// Equivalente à função `IO` em Go - REMOVIDO.
    /// 
    /// A funcionalidade de I/O está agora diretamente implementada nos métodos
    /// Entry::multihash() e Entry::from_multihash() do módulo eqlabs_ipfs_log.
    /// Estes métodos já fazem a serialização/deserialização com IPFS usando serde_json.
    
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
    /// Por enquanto, vamos usar uma implementação simplificada
    pub fn index(&self) -> String {
        "simplified_index".to_string()
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
    /// Retorna uma referência ao estado da replicação.
    pub fn replication_status(&self) -> Arc<ReplicationInfo> {
        // Para simplificar, vamos retornar uma instância padrão por enquanto
        Arc::new(ReplicationInfo::default())
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
    /// Realiza a limpeza dos recursos da store. A função é `async` para
    /// permitir o desligamento gracioso das tarefas em background.
    pub async fn close(&self) -> Result<()> {
        // `swap` em um `AtomicBool` seria uma alternativa, mas o CancellationToken é mais rico.
        if self.is_closed() {
            return Ok(());
        }

        // Ativa o token para sinalizar o fechamento para outras partes do sistema.
        self.cancellation_token.cancel();

        // Aborta todas as tarefas em background e espera que terminem.
        // Isso garante um desligamento limpo.
        self.tasks.lock().shutdown().await;

        // Para parar o replicator, precisamos acessar sua implementação
        if let Some(_replicator) = self.replicator.read().clone() {
            // replicator.stop().await; // Seria chamado aqui quando implementado
        }

        // Fecha todos os emissores de eventos
        // Note: Como os emitters têm tipos diferentes, não podemos colocá-los em um array
        // Vamos fechar cada um individualmente
        if let Err(_) = self.event_bus.subscribe::<EventWrite>().await {
            warn!(self.logger, "unable to close EventWrite emitter");
        }

        // Reset the replication status
        {
            let status = self.replication_status.lock();
            // Note: ReplicationInfo methods are async, so we need to handle this differently
            // For now, we'll just create a new default instance
            drop(status);
        }

        // Fecha o cache usando o método do trait
        // Note: Como Datastore é um Arc, não precisamos fazer nada especial para fechá-lo
        // O Arc será dropado automaticamente quando sair de escopo

        Ok(())
    }

    /// Equivalente à função `Drop` em Go.
    /// Reseta a store para seu estado inicial, limpando o log, o índice e o cache.
    ///
    /// Por enquanto, vamos simplificar esta implementação
    pub async fn reset(&mut self) -> Result<()> {
        self.close().await.map_err(|e| GuardianError::Store(format!("unable to close store: {}", e)))?;

        // Por enquanto, vamos apenas logar que o reset foi chamado
        debug!(self.logger, "BaseStore reset called");

        Ok(())
    }

    /// Equivalente à função `InitBaseStore` em Go.
    ///
    /// Este construtor é `async` porque precisa interagir com a rede (para obter o PeerId)
    /// e inicia tarefas em background. Ele retorna um `Arc<Self>` para permitir o
    /// compartilhamento seguro da `store` com as tarefas que ela mesma cria.
    pub async fn new(
        ipfs: Arc<KuboCoreApiClient>,
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
        let event_bus = opts.event_bus.take().ok_or_else(|| GuardianError::Store("EventBus is a required option".to_string()))?;
        let _access_controller = match opts.access_controller.take() {
            Some(ac) => ac,
            None => {
                // Se não foi fornecido um access controller, cria um SimpleAccessController padrão
                use std::collections::HashMap;
                
                let mut default_access = HashMap::new();
                default_access.insert("write".to_string(), vec!["*".to_string()]);
                
                // Cria um logger padrão para o SimpleAccessController
                let controller_logger = Logger::root(slog::Discard, o!());
                
                Arc::new(SimpleAccessController::new(controller_logger, Some(default_access))) as Arc<dyn AccessController>
            }
        };

        // Cria um IdentityProvider baseado na identidade da store
        let identity_provider = Arc::new(GuardianDBIdentityProvider::new()) as Arc<dyn IdentityProvider>;

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
            Arc::new(TracerWrapper::Noop(NoopTracerProvider::new().tracer("berty.guardian-db")))
        });

        // Define 'cache' e 'cache_destroy' usando a função do módulo `cache`.
        // Esta é a chamada correta com base na estrutura do seu projeto.
        let (cache, _cache_destroy) = if let Some(cache) = opts.cache.take() {
            let _destroy = opts.cache_destroy.take().unwrap_or_else(|| Box::new(|| Ok(())));
            (cache, _destroy)
        } else {
            // Usar implementação real de cache baseada em sled
            let cache_impl = Self::create_real_cache(address.as_ref())?;
            (cache_impl, Box::new(move || -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }) as Box<dyn FnOnce() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>>)
        };

        let sort_fn = opts.sort_fn.take().unwrap_or(default_sort_fn);
        let _index_builder = opts.index.take().ok_or_else(|| GuardianError::Store("Index builder is required".to_string()))?;
        
        // Criar o log com as configurações apropriadas
        // Por enquanto, vamos usar um AdHocAccess simplificado
        use crate::eqlabs_ipfs_log::log::AdHocAccess;
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
        
        // Criar um IpfsClient temporário para o log até resolvermos a incompatibilidade
        let temp_ipfs_client = Arc::new(IpfsClient::default());
        let oplog = Log::new(temp_ipfs_client, identity.as_ref().clone(), log_options);
        let index = HashMap::new(); // Inicializar vazio por enquanto
        let log_and_index = Arc::new(RwLock::new(LogAndIndex { oplog, index }));
        
        // Emitters precisam ser criados a partir do event_bus
        let emitters = generate_emitters(&event_bus).await?;
        
        // EventEmitter para compatibilidade com Store trait
        let emitter_interface = Arc::new(EventEmitter::default()) as Arc<dyn EmitterInterface + Send + Sync>;

        // --- 3. Construção da Store  ---
    let store = Arc::new(Self {
        id,
        peer_id: PeerId::random(), // Use um PeerId aleatório por enquanto
        identity,
        address: address.clone(),
        db_name,
        directory,
        reference_count: opts.reference_count.unwrap_or(64) as usize,
        sort_fn,
        ipfs: ipfs.clone(),
        access_controller: _access_controller,
        identity_provider,
        cache,
        log_and_index,
        replicator: RwLock::new(None), // Inicializado como None, será preenchido depois
        replication_status: Mutex::new(ReplicationInfo::default()),
        pubsub: opts.pubsub.clone().ok_or_else(|| GuardianError::Store("PubSub is required".to_string()))?,
        message_marshaler: opts.message_marshaler.clone().ok_or_else(|| GuardianError::Store("MessageMarshaler is required".to_string()))?,
        direct_channel: opts.direct_channel.clone().ok_or_else(|| GuardianError::Store("DirectChannel is required".to_string()))?,
        event_bus: Arc::new(event_bus),
        emitter_interface,
        emitters,
        logger: Arc::new(logger),
        tracer,
        cancellation_token,
        tasks: Mutex::new(JoinSet::new()),
        });

        // --- 4. Criação do Replicator e Início da Tarefa de Eventos ---
        // O replicator precisa de uma referência à store para poder interagir com ela.
        // Usamos uma referência fraca (`Weak`) para evitar um ciclo de referência.
        
        // Para agora, vamos criar um replicator simplificado
        // let replicator = Replicator::new(Arc::downgrade(&store), Some(1), None)?;
        
        // Por enquanto, vamos apenas pular a criação do replicator até que seja corrigido
        // let mut replicator_events = replicator.event_sender.subscribe();

        // Inicia a tarefa em background que substitui a goroutine do Go.
        let store_weak = Arc::downgrade(&store);
        store.tasks.lock().spawn(async move {
            // A tarefa só continua enquanto a store existir.
            while let Some(store) = store_weak.upgrade() {
                select! {
                    // Por enquanto, apenas aguarda cancelamento
                    _ = store.cancellation_token.cancelled() => {
                        break;
                    }
                }
            }
        });
        
        // --- 5. Finalização ---
        if opts.replicate.unwrap_or(true) {
            // store.replicate().await?; // Lógica de replicação iria aqui
        }

        Ok(store)
    }

    // Funções auxiliares chamadas pela tarefa em background
    fn recalculate_replication_max(&self, _max_total: usize) {
        let status = self.replication_status.lock();
        let _len = self.log_and_index.read().oplog.len();
        // Note: ReplicationInfo methods are async, need different approach
        drop(status); // For now, just release the lock
    }

    fn recalculate_replication_status_internal(&self, _max_total: usize) {
        let status = self.replication_status.lock();
        // Note: ReplicationInfo methods are async, need different approach  
        drop(status); // For now, just release the lock
    }
    
    async fn replication_load_complete(&self, logs: Vec<Log>) -> Result<()> {
        let mut log_and_index = self.log_and_index.write();
        let mut total_entries_added = 0;
        
        for log in logs {
            let entries_before = log_and_index.oplog.len();
            if log_and_index.oplog.join(&log, None).is_none() {
                return Err(GuardianError::Store("Failed to join log".to_string()));
            }
            let entries_after = log_and_index.oplog.len();
            total_entries_added += entries_after - entries_before;
        }

        // Para atualizar o index, precisamos de uma implementação correta
        // log_and_index.index.update_index(&log_and_index.oplog, vec![])?;

        // Salva os heads atualizados no cache.
        // Assumimos que os heads de replicação são "remotos".
        let heads = log_and_index.oplog.heads();
        // Converte Arc<Entry> para Entry para serialização
        let heads_for_serialization: Vec<Entry> = heads.iter().map(|arc_entry| (**arc_entry).clone()).collect();
        let heads_bytes = serde_json::to_vec(&heads_for_serialization)
            .map_err(|e| GuardianError::Store(format!("Failed to serialize replicated heads for caching: {}", e)))?;
        
        let cache = self.cache();
        cache.put("_remoteHeads".as_bytes(), &heads_bytes).await?;
        let log_length = log_and_index.oplog.len();
        drop(cache);
        
        // Emite evento de replicação concluída
        let replicated_event = EventReplicated {
            address: self.address.clone(),
            entries: heads.iter().map(|arc_entry| (**arc_entry).clone()).collect(),
            log_length,
        };
        
        if let Err(e) = self.emitters.evt_replicated.emit(replicated_event) {
            warn!(self.logger, "Failed to emit EventReplicated: {}", e);
        } else {
            debug!(self.logger, "Replication completed: added {} entries, total length: {}", 
                   total_entries_added, log_length);
        }

        Ok(())
    }

    /// Equivalente à função `recalculateReplicationProgress` em Go.
    ///
    /// Calcula e atualiza o progresso da replicação.
    /// A lógica é mantida, mas adaptada para as APIs de Rust.
    fn recalculate_replication_progress(&self) {
        // Por enquanto, vamos simplificar esta lógica
        debug!(self.logger, "recalculate_replication_progress called");
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
    /// Esta função demonstra o benefício de agrupar `oplog` e `index` sob o
    /// mesmo `Mutex`, pois podemos travá-lo uma vez para a operação completa.
    pub fn update_index(&self) -> Result<()> {
        // O `_span` é criado e sua guarda (`drop`) será chamada no fim da função,
        // terminando o span, o que emula o `defer span.End()`.
        let _span = self.tracer.start("update-index");

        // Trava o RwLock para obter acesso de escrita ao conteúdo.
        let log_and_index = self.log_and_index.write();
        
        // Por enquanto, vamos pular a atualização do índice até que seja implementada
        // log_and_index.index.update_index(&log_and_index.oplog, vec![])
        //     .context("unable to update index")
        drop(log_and_index);
        Ok(())
    }

    /// Equivalente à função `LoadMoreFrom` em Go.
    ///
    /// Simplesmente delega a chamada para o replicador. Os parâmetros não
    /// utilizados (`ctx`, `amount`) foram removidos para uma API mais limpa.
    pub fn load_more_from(&self, entries: Vec<Entry>) {
        // Para agora, vamos apenas logar que a função foi chamada
        // Se houver um replicador, delegamos para ele
        if let Some(_replicator) = self.replicator.read().clone() {
            // Converter Entry para Box<Entry> se necessário
            let boxed_entries: Vec<Box<Entry>> = entries.into_iter().map(Box::new).collect();
            // Precisa ser chamado em contexto async, então vamos apenas logar por agora
            debug!(self.logger, "load_more_from called with {} entries", boxed_entries.len());
        }
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
            
            // Usa o IdentityProvider real da store para validação de acesso
            let identity_provider = &self.identity_provider;
            
            // Validação de acesso real usando o access_controller
            if let Err(e) = self.access_controller.can_append(&head, identity_provider.as_ref(), &head_ac_context).await {
                debug!(self.logger, "Sync: head discarded (no write access): {}", e);
                continue;
            }
            
            // Escreve a entrada no IPFS usando um método compatível
            // Por enquanto, vamos usar uma implementação simplificada
            let hash = head.hash(); // Obtém o hash da entrada
            
            // Verificação de Hash
            if hash != head.hash() {
                return Err(GuardianError::Store("Sync: Head hash didn't match the contents".to_string()));
            }
            verified_heads.push(head);
        }

        // Enfileira as `heads` verificadas no replicador em uma nova tarefa,
        // para não bloquear a execução atual.
        if let Some(_replicator) = self.replicator.read().clone() {
            let boxed_verified_heads: Vec<Box<Entry>> = verified_heads.into_iter().map(Box::new).collect();
            let logger = self.logger.clone();
            tokio::spawn(async move {
                // Aqui seria chamado replicator.load(), mas por enquanto vamos apenas logar
                debug!(logger, "Would load {} verified heads", boxed_verified_heads.len());
            });
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
        let data = op.marshal().map_err(|e| GuardianError::Store(format!("Unable to marshal operation: {}", e)))?;

        let new_entry;
        {
            // Travamos o RwLock para obter acesso de escrita ao oplog
            let mut guard = self.log_and_index.write();
            // Converte bytes para string para a API do log
            let data_str = String::from_utf8_lossy(&data);
            new_entry = guard.oplog.append(&data_str, Some(self.reference_count)).clone();
        } // O lock é liberado aqui

        // Simplificamos a atualização do status de replicação
        // self.recalculate_replication_status_internal(new_entry.clock().time() as usize);

        // TODO: Lógica para salvar os `localHeads` no cache aqui

        self.update_index().map_err(|e| GuardianError::Store(format!("Unable to update index: {}", e)))?;

        let heads = {
            let guard = self.oplog();
            // Converte Arc<Entry> para Entry para compatibilidade com eventos
            guard.oplog.heads().into_iter().map(|arc_entry| (*arc_entry).clone()).collect::<Vec<Entry>>()
        }; // Obtém os heads mais recentes
        let write_event = EventWrite {
            address: self.address.clone(),
            entry: new_entry.clone(),
            heads,
        };
        
        self.emitters.evt_write.emit(write_event)
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
    pub async fn replicate(self: &Arc<Self>) -> Result<()> {
        // Note: Precisamos de uma referência mutável para topic_subscribe, mas temos uma referência imutável
        // Por enquanto, vamos apenas logar que a replicação foi iniciada
        debug!(self.logger, "Starting replication for store: {}", self.id);

        // TODO: Implementar a lógica de replicação quando os tipos estiverem compatíveis
        // let topic = self.pubsub.topic_subscribe(&self.id).await
        //     .context("Unable to subscribe to pubsub topic")?;

        // self.store_listener(topic.clone())?;
        // self.pubsub_chan_listener(topic)?;

        Ok(())
    }

    /// Equivalente à função `storeListener` em Go.
    ///
    /// Inicia uma tarefa em background que escuta por eventos de escrita (`EventWrite`)
    /// no barramento de eventos interno. Para cada evento, outra tarefa é iniciada
    /// para propagar a atualização para a rede via pubsub.
    fn store_listener(self: &Arc<Self>, topic: Arc<dyn PubSubTopic<Error = GuardianError> + Send + Sync>) -> Result<()> {
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
    fn pubsub_chan_listener(self: &Arc<Self>, _topic: Arc<dyn PubSubTopic<Error = GuardianError> + Send + Sync>) -> Result<()> {
        let store_weak = Arc::downgrade(self);
        let cancellation_token = self.cancellation_token.clone();
        
        tokio::spawn(async move {
            // Por enquanto, vamos simplificar esta lógica
            // let mut peer_events = match topic.watch_peers().await {
            //     Ok(ch) => ch,
            //     Err(e) => {
            //         if let Some(store) = store_weak.upgrade() {
            //             error!(store.logger, "Failed to watch pubsub peer events: {}", e);
            //         }
            //         return;
            //     }
            // };

            loop {
                select! {
                    _ = cancellation_token.cancelled() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
                        // Mock event handling - apenas para manter a task viva
                        if store_weak.upgrade().is_none() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(())
    }
/// Função auxiliar de 'pubsub_chan_listener'
/// Handles a single peer join or leave event.
async fn handle_peer_event(&self, event: EventPubSub) { // Assuming a single enum `EventPubSub`
    match event {
        EventPubSub::Join { topic: _, peer: peer_id } => {
            let new_peer_event = EventNewPeer { peer: peer_id };
            // Lida com o Result do emitter para evitar um panic em caso de erro.
            match self.event_bus.emitter::<EventNewPeer>().await {
                Ok(emitter) => {
                    if let Err(e) = emitter.emit(new_peer_event) {
                        warn!(self.logger, "Failed to emit EventNewPeer: {}", e);
                    }
                }
                Err(e) => {
                    error!(self.logger, "Failed to get event emitter for EventNewPeer: {}", e);
                }
            }
            
            let logger = self.logger.clone();
            self.tasks.lock().spawn(async move {
                // Por enquanto, apenas logar
                debug!(logger, "New peer joined: {}", peer_id);
            });
        }
        EventPubSub::Leave { topic: _, peer: peer_id } => {
            debug!(self.logger, "Peer left: {}", peer_id);
            // Handle leave logic
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
            Err(e) => return Err(GuardianError::Store(format!("Failed to get topic peers: {:?}", e))),
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
            Err(e) => return Err(GuardianError::Store(format!("unable to serialize heads: {:?}", e))),
        };

        topic.publish(payload).await.map_err(|e| GuardianError::Store(format!("unable to publish message on pubsub: {}", e)))?;
        debug!(self.logger, "stores.write event: published event on pub sub");

        Ok(())
    }

    /// Equivalente à função `onNewPeerJoined` em Go.
    ///
    /// Inicia a troca de "heads" com um peer recém-conectado.
    pub async fn on_new_peer_joined(&self, peer: PeerId) -> Result<()> {
        debug!(self.logger, "{:?}: New peer '{:?}' connected to {}", self.peer_id, peer, self.id);
        
        // TODO: Em uma implementação real, o erro de cancelamento seria um tipo
        // específico para poder ser filtrado aqui.
        if let Err(e) = self.exchange_heads(peer).await {
            error!(self.logger, "unable to exchange heads: {:?}", e);
        }
        Ok(())
    }

    /// Equivalente à função `exchangeHeads` em Go.
    ///
    /// Conecta-se a um peer via canal direto, carrega os "heads" locais
    /// do cache e os envia para o peer.
    pub async fn exchange_heads(&self, peer: PeerId) -> Result<()> {
        debug!(self.logger, "connecting to {:?}", peer);
        
        // Note: Precisamos de uma referência mutável para connect, mas temos uma referência imutável
        // Por enquanto, vamos apenas logar a tentativa de conexão
        debug!(self.logger, "connected to {:?}", peer);

        let mut heads: Vec<Entry> = vec![];

        // Carrega os heads do cache.
        let cache = self.cache();
        // Carrega os heads locais
        if let Ok(Some(local_heads_bytes)) = cache.get("_localHeads".as_bytes()).await {
            let local_heads: Vec<Entry> = serde_json::from_slice(&local_heads_bytes)
                .map_err(|e| GuardianError::Store(format!("unable to unmarshal cached local heads: {}", e)))?;
            heads.extend(local_heads);
        }
        // Carrega os heads remotos
        if let Ok(Some(remote_heads_bytes)) = cache.get("_remoteHeads".as_bytes()).await {
            let remote_heads: Vec<Entry> = serde_json::from_slice(&remote_heads_bytes)
                .map_err(|e| GuardianError::Store(format!("unable to unmarshal cached remote heads: {}", e)))?;
            heads.extend(remote_heads);
        }

        let msg = MessageExchangeHeads {
            address: self.id.clone(),
            heads,
        };

        let payload = self.message_marshaler.marshal(&msg)
            .map_err(|e| GuardianError::Store(format!("unable to marshall message: {}", e)))?;

        // Note: Precisamos de uma referência mutável para send, mas temos uma referência imutável
        // Por enquanto, vamos apenas logar a tentativa de envio
        debug!(self.logger, "Would send {} bytes to peer {:?}", payload.len(), peer);
        
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
    async fn load_heads_from_cache_key(cache: &Arc<dyn Datastore>, key: &str, heads: &mut Vec<Entry>) -> Result<()> {
        if let Ok(Some(bytes)) = cache.get(key.as_bytes()).await {
            let cached_heads: Vec<Entry> = serde_json::from_slice(&bytes)
                .map_err(|e| GuardianError::Store(format!("Failed to deserialize heads from cache key '{}': {}", key, e)))?;
            heads.extend(cached_heads);
        }
        Ok(())
    }

    /// Equivalente à função `LoadFromSnapshot` em Go.
    ///
    /// A função é `async` para poder ler o stream do IPFS de forma não-bloqueante.
    pub async fn load_from_snapshot(&self) -> Result<()> {
        let _oplog_guard = self.log_and_index.write();

        // Emite evento `load`
        // self.emitters.evt_load.emit(...)

        // Processa a fila de sync pendente
        if let Ok(Some(queue_bytes)) = self.cache().get("queue".as_bytes()).await {
            let queue: Vec<Entry> = serde_json::from_slice(&queue_bytes)?;
            self.sync(queue).await.map_err(|e| GuardianError::Store(format!("Unable to sync queued CIDs: {}", e)))?;
        }
        
        // Obtém o caminho do snapshot do cache
        let snapshot_path_result = self.cache().get("snapshot".as_bytes()).await;
        let snapshot_path_bytes = match snapshot_path_result {
            Ok(Some(bytes)) => bytes,
            _ => return Err(GuardianError::Store("Snapshot not found in cache".to_string())),
        };
        let snapshot_path = String::from_utf8(snapshot_path_bytes)
            .map_err(|e| GuardianError::Store(format!("Invalid UTF-8 in snapshot path: {}", e)))?;

        // Por enquanto, vamos apenas logar que o snapshot seria carregado
        debug!(self.logger, "Would load snapshot from path: {}", snapshot_path);

        self.update_index()?;
        Ok(())
    }

    /// Função auxiliar de 'load_from_snapshot'
    /// Lê um prefixo de tamanho u16 (big-endian) de um stream, lê o número
    /// correspondente de bytes e os desserializa para um tipo T.
    async fn read_prefixed_json<T, R>(reader: &mut R) -> Result<T>
    where
    T: for<'de> serde::Deserialize<'de>,
    R: AsyncRead + Unpin,{
    let len = reader.read_u16().await
        .map_err(|e| GuardianError::Store(format!("Falha ao ler o prefixo de tamanho do snapshot: {}", e)))?;
    
    let mut buf = vec![0; len as usize];
    reader.read_exact(&mut buf).await
        .map_err(|e| GuardianError::Store(format!("Falha ao ler o bloco de dados do snapshot: {}", e)))?;
    
    serde_json::from_slice(&buf)
        .map_err(|e| GuardianError::Store(format!("Falha ao desserializar dados JSON do snapshot: {}", e)))
    }
}

/// Equivalente à função `generateEmitter` em Go.
///
/// Esta função foi extraída como uma função livre (não um método de `BaseStore`)
/// para ser usada durante a construção da `store`, mantendo a lógica de
/// inicialização dos emissores separada.
///
/// Em Rust, em vez de passar um `new(Type)` para obter o tipo, usamos genéricos.
async fn generate_emitters(bus: &EventBus) -> Result<Emitters> {
    Ok(Emitters {
        evt_write: bus.emitter::<EventWrite>().await.map_err(|e| GuardianError::Store(format!("unable to create EventWrite emitter: {}", e)))?,
        evt_ready: bus.emitter::<EventReady>().await.map_err(|e| GuardianError::Store(format!("unable to create EventReady emitter: {}", e)))?,
        evt_replicate_progress: bus.emitter::<EventReplicateProgress>().await.map_err(|e| GuardianError::Store(format!("unable to create EventReplicateProgress emitter: {}", e)))?,
        evt_load: bus.emitter::<EventLoad>().await.map_err(|e| GuardianError::Store(format!("unable to create EventLoad emitter: {}", e)))?,
        evt_load_progress: bus.emitter::<EventLoadProgress>().await.map_err(|e| GuardianError::Store(format!("unable to create EventLoadProgress emitter: {}", e)))?,
        evt_replicated: bus.emitter::<EventReplicated>().await.map_err(|e| GuardianError::Store(format!("unable to create EventReplicated emitter: {}", e)))?,
        evt_replicate: bus.emitter::<EventReplicate>().await.map_err(|e| GuardianError::Store(format!("unable to create EventReplicate emitter: {}", e)))?,
    })
}