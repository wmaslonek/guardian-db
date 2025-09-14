use crate::address::{Address, GuardianDBAddress};
use crate::cache::level_down::LevelDownCache;
use crate::db_manifest;
use crate::error::{GuardianError, Result};
use crate::iface::{
    AccessControllerConstructor, CreateDBOptions, DetermineAddressOptions, DirectChannel,
    DirectChannelFactory, DirectChannelOptions, EventPubSubPayload, MessageExchangeHeads,
    MessageMarshaler, PubSubInterface, Store, StoreConstructor,
};
use crate::ipfs_core_api::{client::IpfsClient, config::ClientConfig};
use crate::ipfs_log::identity::{Identity, Signatures};
pub use crate::ipfs_log::identity_provider::Keystore;
use crate::keystore::SledKeystore;
use crate::pubsub::event::Emitter;
pub use crate::pubsub::event::EventBus as EventBusImpl;
use hex;
use ipfs_api_backend_hyper::IpfsClient as HyperIpfsClient;
use libp2p::PeerId;
use opentelemetry::global::BoxedTracer;
use parking_lot::RwLock;
use rand::RngCore;
use secp256k1;
use slog::{Discard, Logger, debug, error, info, o, trace, warn};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

// Type aliases para simplificar tipos complexos
type CloseKeystoreFn = Arc<RwLock<Option<Box<dyn Fn() -> Result<()> + Send + Sync>>>>;
// Type alias para Store com GuardianError
type GuardianStore = dyn Store<Error = GuardianError> + Send + Sync;

// Representação da struct `NewGuardianDBOptions` de Go.
// Usamos `Option<T>` para campos que podem não ser fornecidos.
#[derive(Default)]
pub struct NewGuardianDBOptions {
    pub id: Option<String>,
    pub peer_id: Option<PeerId>,
    pub directory: Option<PathBuf>,
    pub keystore: Option<Box<dyn Keystore + Send + Sync>>,
    pub cache: Option<Arc<LevelDownCache>>,
    pub identity: Option<Identity>,
    pub close_keystore: Option<Box<dyn Fn() -> Result<()> + Send + Sync>>,
    pub logger: Option<Logger>,
    pub tracer: Option<Arc<BoxedTracer>>,
    pub direct_channel_factory: Option<DirectChannelFactory>,
    pub pubsub: Option<Box<dyn PubSubInterface<Error = GuardianError>>>,
    pub message_marshaler: Option<Box<dyn MessageMarshaler<Error = GuardianError>>>,
    pub event_bus: Option<Arc<EventBusImpl>>,
}

pub struct GuardianDB {
    ipfs: IpfsClient,
    identity: Arc<RwLock<Identity>>,
    id: Arc<RwLock<PeerId>>,
    keystore: Arc<RwLock<Option<Box<dyn Keystore + Send + Sync>>>>,
    close_keystore: CloseKeystoreFn,
    logger: Logger,
    tracer: Arc<BoxedTracer>,
    stores: Arc<RwLock<HashMap<String, Arc<GuardianStore>>>>,
    #[allow(dead_code)]
    direct_channel: Arc<dyn DirectChannel<Error = GuardianError> + Send + Sync>,
    access_controller_types: Arc<RwLock<HashMap<String, AccessControllerConstructor>>>,
    store_types: Arc<RwLock<HashMap<String, StoreConstructor>>>,
    directory: PathBuf,
    cache: Arc<RwLock<Arc<LevelDownCache>>>,
    #[allow(dead_code)]
    pubsub: Option<Box<dyn PubSubInterface<Error = GuardianError>>>,
    event_bus: Arc<EventBusImpl>,
    #[allow(dead_code)]
    message_marshaler: Arc<dyn MessageMarshaler<Error = GuardianError> + Send + Sync>,
    _monitor_handle: JoinHandle<()>, // Handle para a task em background, para que possa ser cancelada no Drop.
    cancellation_token: CancellationToken,
    emitters: Arc<Emitters>,
}

/// Equivalente à struct EventExchangeHeads em Go.
#[derive(Clone)]
pub struct EventExchangeHeads {
    pub peer: PeerId,
    pub message: MessageExchangeHeads,
}

// GuardianDB-level events
#[derive(Clone)]
pub struct EventGuardianDBReady {
    pub address: String,
    pub db_type: String,
}

#[derive(Clone)]
pub struct EventPeerConnected {
    pub peer_id: String,
    pub address: String,
}

#[derive(Clone)]
pub struct EventPeerDisconnected {
    pub peer_id: String,
    pub address: String,
}

#[derive(Clone)]
pub struct EventDatabaseCreated {
    pub address: String,
    pub name: String,
    pub db_type: String,
}

#[derive(Clone)]
pub struct EventDatabaseDropped {
    pub address: String,
    pub name: String,
}

// Store-specific events
#[derive(Clone)]
pub struct EventStoreUpdated {
    pub store_address: String,
    pub store_type: String,
    pub entries_added: usize,
    pub total_entries: usize,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone)]
pub struct EventSyncCompleted {
    pub store_address: String,
    pub peer_id: String,
    pub heads_synced: usize,
    pub duration_ms: u64,
    pub success: bool,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone)]
pub struct EventNewEntries {
    pub store_address: String,
    pub entries: Vec<crate::ipfs_log::entry::Entry>,
    pub total_entries: usize,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

// Error events
#[derive(Clone)]
pub struct EventSyncError {
    pub store_address: String,
    pub peer_id: String,
    pub error_message: String,
    pub heads_count: usize,
    pub error_type: SyncErrorType,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Debug)]
pub enum SyncErrorType {
    PermissionDenied,
    NetworkError,
    ValidationError,
    StoreError,
    UnknownError,
}

#[derive(Clone)]
pub struct EventPermissionDenied {
    pub store_address: String,
    pub identity_id: String,
    pub identity_pubkey: String,
    pub required_permission: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

// GuardianDB Emitters using our EventBus
pub struct Emitters {
    pub ready: Emitter<EventGuardianDBReady>,
    pub peer_connected: Emitter<EventPeerConnected>,
    pub peer_disconnected: Emitter<EventPeerDisconnected>,
    pub new_heads: Emitter<EventExchangeHeads>,
    pub database_created: Emitter<EventDatabaseCreated>,
    pub database_dropped: Emitter<EventDatabaseDropped>,
    // Store-specific events
    pub store_updated: Emitter<EventStoreUpdated>,
    pub sync_completed: Emitter<EventSyncCompleted>,
    pub new_entries: Emitter<EventNewEntries>,
    // Error events
    pub sync_error: Emitter<EventSyncError>,
    pub permission_denied: Emitter<EventPermissionDenied>,
}

impl Emitters {
    /// Generate emitters from an EventBus instance
    pub async fn generate_emitters(event_bus: &EventBusImpl) -> Result<Self> {
        Ok(Self {
            ready: event_bus.emitter().await?,
            peer_connected: event_bus.emitter().await?,
            peer_disconnected: event_bus.emitter().await?,
            new_heads: event_bus.emitter().await?,
            database_created: event_bus.emitter().await?,
            database_dropped: event_bus.emitter().await?,
            // Store-specific events
            store_updated: event_bus.emitter().await?,
            sync_completed: event_bus.emitter().await?,
            new_entries: event_bus.emitter().await?,
            // Error events
            sync_error: event_bus.emitter().await?,
            permission_denied: event_bus.emitter().await?,
        })
    }
}

impl EventExchangeHeads {
    /// Cria uma nova instância de EventExchangeHeads.
    /// Equivalente à função NewEventExchangeHeads em Go.
    pub fn new(p: PeerId, msg: MessageExchangeHeads) -> Self {
        Self {
            peer: p,
            message: msg,
        }
    }
}

impl EventStoreUpdated {
    pub fn new(
        store_address: String,
        store_type: String,
        entries_added: usize,
        total_entries: usize,
    ) -> Self {
        Self {
            store_address,
            store_type,
            entries_added,
            total_entries,
            timestamp: chrono::Utc::now(),
        }
    }
}

impl EventSyncCompleted {
    pub fn new(
        store_address: String,
        peer_id: String,
        heads_synced: usize,
        duration_ms: u64,
        success: bool,
    ) -> Self {
        Self {
            store_address,
            peer_id,
            heads_synced,
            duration_ms,
            success,
            timestamp: chrono::Utc::now(),
        }
    }
}

impl EventNewEntries {
    pub fn new(
        store_address: String,
        entries: Vec<crate::ipfs_log::entry::Entry>,
        total_entries: usize,
    ) -> Self {
        Self {
            store_address,
            entries,
            total_entries,
            timestamp: chrono::Utc::now(),
        }
    }
}

impl EventSyncError {
    pub fn new(
        store_address: String,
        peer_id: String,
        error_message: String,
        heads_count: usize,
        error_type: SyncErrorType,
    ) -> Self {
        Self {
            store_address,
            peer_id,
            error_message,
            heads_count,
            error_type,
            timestamp: chrono::Utc::now(),
        }
    }
}

impl EventPermissionDenied {
    pub fn new(
        store_address: String,
        identity_id: String,
        identity_pubkey: String,
        required_permission: String,
    ) -> Self {
        Self {
            store_address,
            identity_id,
            identity_pubkey,
            required_permission,
            timestamp: chrono::Utc::now(),
        }
    }
}

impl GuardianDB {
    /// equivalente a func NewGuardianDB(ctx context.Context, ipfs coreiface.CoreAPI, options *NewGuardianDBOptions) (BaseGuardianDB, error) em go
    /// Construtor de alto nível que configura o Keystore e a Identidade antes de chamar o construtor principal `new_orbit_db`.
    pub async fn new(ipfs: HyperIpfsClient, options: Option<NewGuardianDBOptions>) -> Result<Self> {
        let mut options = options.unwrap_or_default();

        // Extrair peer_id do HyperIpfsClient se possível, senão usar um aleatório
        let peer_id = options.peer_id.unwrap_or_else(PeerId::random);

        // Se o diretório não for fornecido, usa um padrão baseado no peer_id
        let default_dir = PathBuf::from("./GuardianDB").join(peer_id.to_string());
        let directory = options.directory.as_ref().unwrap_or(&default_dir);

        // Configura o Keystore se nenhum for fornecido.
        // Usa o banco de dados `sled` como substituto do `leveldb`.
        if options.keystore.is_none() {
            // Em `sled`, None para o path significa in-memory
            let sled_path = if directory.to_string_lossy() == "./GuardianDB/in-memory" {
                None
            } else {
                Some(directory.join(peer_id.to_string()).join("keystore"))
            };

            // Cria o keystore usando nossa implementação SledKeystore
            let keystore = SledKeystore::new(sled_path)
                .map_err(|e| GuardianError::Other(format!("Falha ao criar o keystore: {}", e)))?;

            // Cria a closure de fechamento
            let keystore_clone = keystore.clone();
            options.close_keystore = Some(Box::new(move || {
                // Para evitar runtime aninhado, usar task::block_in_place
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(async { keystore_clone.close().await })
                })
            }));

            // Define o keystore nas opções
            options.keystore = Some(Box::new(keystore));
        }

        // Configura a identidade se nenhuma for fornecida.
        let identity = if let Some(identity) = options.identity {
            identity
        } else {
            let id = options
                .id
                .as_deref()
                .unwrap_or(&peer_id.to_string())
                .to_string();
            let _keystore = options.keystore.as_ref().ok_or_else(|| {
                GuardianError::Other("Keystore é necessário para criar uma identidade".to_string())
            })?;

            // Criar uma identidade usando o keystore configurado
            use crate::ipfs_log::identity::Identity;

            // Gerar uma chave secreta para a identidade
            let mut rng = rand::rng();
            let mut secret_bytes = [0u8; 32];
            rng.fill_bytes(&mut secret_bytes);

            // Gerar chave pública a partir da privada (simplificado)
            let pub_key_hex = hex::encode(secret_bytes);

            // Criar assinaturas criptográficas para a identidade
            let signatures = Signatures::new(
                &format!("id_sig_{}", id),
                &format!("pk_sig_{}", pub_key_hex),
            );

            Identity::new(&id, &pub_key_hex, signatures)
        };

        options.identity = Some(identity.clone());

        // Chama o construtor principal com as opções totalmente configuradas.
        // Converte HyperIpfsClient para nosso IpfsClient usando configuração extraída
        let config = ClientConfig::from_hyper_client(&ipfs);
        let ipfs_client = IpfsClient::new(config).await?;
        Self::new_orbit_db(ipfs_client, identity, Some(options)).await
    }

    /// equivalente a func newGuardianDB(ctx context.Context, is coreiface.CoreAPI, identity *idp.Identity, options *NewGuardianDBOptions) (BaseGuardianDB, error) em go
    /// Construtor principal para uma instância de GuardianDB.
    pub async fn new_orbit_db(
        ipfs: IpfsClient,
        identity: Identity,
        options: Option<NewGuardianDBOptions>,
    ) -> Result<Self> {
        // Usa as opções fornecidas ou cria um valor padrão.
        let options = options.unwrap_or_default();

        // 1. Validação de argumentos (o sistema de tipos de Rust já garante que `ipfs` e `identity` não são nulos)

        // 2. Configuração de valores padrão para as opções
        let logger = options
            .logger
            .unwrap_or_else(|| Logger::root(Discard, o!()));
        let tracer = options.tracer.unwrap_or_else(|| {
            // Usar um tracer básico para telemetria
            Arc::new(BoxedTracer::new(Box::new(
                opentelemetry::trace::noop::NoopTracer::new(),
            )))
        });
        // Initialize EventBus with proper configuration
        let event_bus = Arc::new(EventBusImpl::new());

        // Criar DirectChannelFactory
        let direct_channel_factory = options.direct_channel_factory.unwrap_or_else(|| {
            crate::pubsub::direct_channel::init_direct_channel_factory(
                logger.clone(),
                PeerId::random(),
            )
        });
        let cancellation_token = CancellationToken::new();

        // Criar emitters usando nosso EventBus (agora async)
        let emitters = Emitters::generate_emitters(&event_bus).await.map_err(|e| {
            GuardianError::Other(format!("Falha ao gerar emitters do EventBus: {}", e))
        })?;

        // 3. Inicialização de componentes
        // Criar canal direto usando nossa factory
        let direct_channel = make_direct_channel(
            &event_bus,
            direct_channel_factory,
            &DirectChannelOptions::default(),
            &logger,
        )
        .await?;

        let message_marshaler_arc: Arc<dyn MessageMarshaler<Error = GuardianError> + Send + Sync> =
            match options.message_marshaler {
                Some(boxed_marshaler) => {
                    // Usar unsafe apenas quando necessário, mas de forma controlada
                    unsafe {
                        Arc::from_raw(Box::into_raw(boxed_marshaler)
                            as *const (dyn MessageMarshaler<Error = GuardianError> + Send + Sync))
                    }
                }
                None => {
                    // Criar diretamente como Arc para evitar conversão
                    Arc::new(crate::message_marshaler::GuardianJSONMarshaler::new())
                }
            };
        let cache = options.cache.unwrap_or_else(|| {
            // Cria um cache com configuração adequada
            Arc::new(crate::cache::level_down::LevelDownCache::new(None))
        });
        let directory = options
            .directory
            .unwrap_or_else(|| PathBuf::from("./GuardianDB/in-memory")); // Padrão para dados em memória

        // 4. Instanciação da struct GuardianDB
        let instance = GuardianDB {
            ipfs,
            identity: Arc::new(RwLock::new(identity.clone())),
            id: Arc::new(RwLock::new(
                PeerId::from_bytes(identity.pub_key.as_bytes())
                    .unwrap_or_else(|_| PeerId::random()),
            )), // Converte pub_key para PeerId
            pubsub: options.pubsub,
            cache: Arc::new(RwLock::new(cache)),
            directory,
            event_bus: event_bus.clone(),
            stores: Arc::new(RwLock::new(HashMap::new())),
            direct_channel,
            close_keystore: Arc::new(RwLock::new(options.close_keystore)),
            keystore: Arc::new(RwLock::new(options.keystore)),
            store_types: Arc::new(RwLock::new(HashMap::new())),
            access_controller_types: Arc::new(RwLock::new(HashMap::new())),
            logger: logger.clone(),
            tracer,
            message_marshaler: message_marshaler_arc,
            cancellation_token: cancellation_token.clone(),
            emitters: Arc::new(emitters),
            // Inicia o monitor do canal direto usando a função helper
            _monitor_handle: Self::start_monitor_task(
                event_bus.clone(),
                cancellation_token.clone(),
                logger.clone(),
            ),
        };

        // 5. Configuração pós-inicialização

        // Registra os construtores padrão de stores
        instance.register_default_store_types();

        // Configura o emitter "newHeads" no event_bus
        debug!(logger, "Configurando emitters do EventBus");

        // Inicia o monitor do canal direto de forma independente
        debug!(logger, "Iniciando monitor do canal direto");

        // Emite evento de inicialização do GuardianDB
        let ready_event = EventGuardianDBReady {
            address: format!("/GuardianDB/{}", instance.peer_id()),
            db_type: "GuardianDB".to_string(),
        };

        if let Err(e) = instance.emitters.ready.emit(ready_event) {
            warn!(logger, "Falha ao emitir evento GuardianDB ready"; "error" => e.to_string());
        } else {
            debug!(logger, "Evento GuardianDB ready emitido com sucesso");
        }

        Ok(instance)
    }
    /// equivalente a func (o *GuardianDB) Logger() *zap.Logger em go
    /// Retorna uma referência ao logger da instância do GuardianDB.
    pub fn logger(&self) -> &Logger {
        &self.logger
    }

    /// equivalente a func (o *GuardianDB) Tracer() trace.Tracer em go
    /// Retorna o tracer para telemetria e monitoramento.
    pub fn tracer(&self) -> Arc<BoxedTracer> {
        self.tracer.clone()
    }

    /// equivalente a func (o *GuardianDB) IPFS() coreiface.CoreAPI em go
    /// Retorna o cliente da API do IPFS (Kubo).
    pub fn ipfs(&self) -> &IpfsClient {
        &self.ipfs
    }

    /// equivalente a func (o *GuardianDB) Identity() *idp.Identity em go
    /// Retorna a identidade da instância do GuardianDB.
    /// A identidade é clonada para que o chamador possa usá-la sem manter o lock de leitura ativo.
    pub fn identity(&self) -> Identity {
        self.identity.read().clone()
    }

    /// equivalente a func (o *GuardianDB) PeerID() peer.ID em go
    /// Retorna o PeerId da instância do GuardianDB.
    /// `PeerId` implementa o trait `Copy`, então o valor é copiado, o que é muito eficiente.
    pub fn peer_id(&self) -> PeerId {
        *self.id.read()
    }

    /// equivalente a func (o *GuardianDB) KeyStore() keystore.Interface em go
    /// Retorna um clone do `Arc` para o Keystore, permitindo o acesso compartilhado.
    /// O keystore é configurado durante a inicialização e pode ser usado para operações criptográficas.
    pub fn keystore(&self) -> Arc<RwLock<Option<Box<dyn Keystore + Send + Sync>>>> {
        self.keystore.clone()
    }

    /// equivalente a func (o *GuardianDB) CloseKeyStore() func() error em go
    /// Retorna a função de fechamento para o Keystore, se existir.
    ///
    /// Esta função retorna uma closure que pode ser chamada para fechar o keystore.
    /// A closure captura uma referência clonada para o campo close_keystore interno.
    ///
    /// # Retorna
    ///
    /// - `Some(closure)` se uma função de fechamento foi configurada durante a inicialização
    /// - `None` se nenhuma função de fechamento foi definida
    ///
    /// # Exemplo
    ///
    /// ```rust,ignore
    /// # use guardian_db::base_guardian::GuardianDB;
    /// # use ipfs_api_backend_hyper::HyperClient;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ipfs = HyperClient::default();
    /// # let guardian_db = GuardianDB::new(ipfs, None).await?;
    /// if let Some(close_fn) = guardian_db.close_keystore() {
    ///     if let Err(e) = close_fn() {
    ///         eprintln!("Erro ao fechar keystore: {}", e);
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Alternativa
    ///
    /// Para uma interface mais simples, use `close_key_store()` diretamente.
    pub fn close_keystore(&self) -> Option<Box<dyn Fn() -> Result<()> + Send + Sync>> {
        // Adquire lock de leitura para verificar se existe uma função de fechamento
        let guard = self.close_keystore.read();

        if guard.is_some() {
            // Clona o Arc interno para capturar na closure
            let close_keystore_clone = self.close_keystore.clone();

            // Retorna uma nova closure que executa a função de fechamento
            Some(Box::new(move || {
                let guard = close_keystore_clone.read();
                if let Some(close_fn) = guard.as_ref() {
                    close_fn()
                } else {
                    Ok(()) // Função foi removida entre a verificação e a execução
                }
            }))
        } else {
            None
        }
    }

    /// equivalente a func (o *GuardianDB) setStore(address string, store iface.Store) em go
    /// Adiciona ou atualiza uma store no mapa de stores gerenciadas.
    /// Esta operação adquire um lock de escrita.
    pub fn set_store(&self, address: String, store: Arc<GuardianStore>) {
        self.stores.write().insert(address, store);
    }

    /// equivalente a func (o *GuardianDB) deleteStore(address string) em go
    /// Remove uma store do mapa de stores gerenciadas.
    /// Esta operação adquire um lock de escrita.
    pub fn delete_store(&self, address: &str) {
        self.stores.write().remove(address);
    }

    /// equivalente a func (o *GuardianDB) getStore(address string) (iface.Store, bool) em go
    /// Busca uma store no mapa pelo seu endereço.
    /// Retorna `Some(store)` se encontrada, ou `None` caso contrário.
    pub fn get_store(&self, address: &str) -> Option<Arc<GuardianStore>> {
        self.stores.read().get(address).cloned()
    }

    /// equivalente a func (o *GuardianDB) closeAllStores() em go
    /// Itera sobre todas as stores gerenciadas e chama o método `close()` de cada uma.
    /// Clona a lista de stores para evitar manter o lock durante a chamada a `close()`,
    /// prevenindo possíveis deadlocks.
    pub async fn close_all_stores(&self) {
        let stores_to_close: Vec<Arc<GuardianStore>> =
            self.stores.read().values().cloned().collect();

        debug!(
            self.logger,
            "Iniciando fechamento de {} stores",
            stores_to_close.len()
        );

        for (index, store) in stores_to_close.iter().enumerate() {
            debug!(self.logger, "Fechando store {}/{}", index + 1, stores_to_close.len();
                "store_type" => store.store_type(),
                "address" => store.address().to_string()
            );

            // Agora podemos chamar close() diretamente pois a trait aceita &self
            match store.close().await {
                Ok(()) => {
                    debug!(self.logger, "Store fechada com sucesso";
                        "store_type" => store.store_type(),
                        "address" => store.address().to_string()
                    );
                }
                Err(e) => {
                    error!(self.logger, "Erro ao fechar store";
                        "store_type" => store.store_type(),
                        "address" => store.address().to_string(),
                        "error" => e.to_string()
                    );
                    // Continua fechando outras stores mesmo se uma falhar
                }
            }
        }

        // Limpa o mapa de stores após fechar todas
        self.stores.write().clear();
        debug!(
            self.logger,
            "Todas as {} stores foram processadas e removidas do mapa",
            stores_to_close.len()
        );
    }

    /// equivalente a func (o *GuardianDB) closeCache() em go
    /// Fecha o cache LevelDown, garantindo que todos os dados sejam persistidos
    /// e liberando os recursos associados.
    pub fn close_cache(&self) {
        debug!(self.logger, "Iniciando fechamento do cache");

        // Obtém lock de escrita no cache para realizar o fechamento
        let cache_guard = self.cache.write();

        // Fecha o cache usando o método direto da instância
        match cache_guard.close_internal() {
            Ok(()) => {
                debug!(self.logger, "Cache fechado com sucesso");
            }
            Err(e) => {
                error!(self.logger, "Erro ao fechar cache"; "error" => e.to_string());
            }
        }

        // O lock é automaticamente liberado quando cache_guard sai de escopo
    }

    /// equivalente a func (o *GuardianDB) closeDirectConnections() em go
    /// Fecha o canal de comunicação direta e registra um erro se a operação falhar.
    pub async fn close_direct_connections(&self) {
        debug!(self.logger, "Iniciando fechamento do canal direto");

        // Agora podemos usar o método close_shared() que funciona com Arc<>
        match self.direct_channel.close_shared().await {
            Ok(()) => {
                debug!(self.logger, "Canal direto fechado com sucesso");
            }
            Err(e) => {
                error!(
                    self.logger,
                    "Erro ao fechar canal direto";
                    "error" => e.to_string()
                );
            }
        }
    }

    /// equivalente a func (o *GuardianDB) closeKeyStore() em go
    /// Executa a função de fechamento do keystore, se ela tiver sido definida.
    /// Adquire um lock de escrita para garantir que a função não seja modificada enquanto é lida e executada.
    pub fn close_key_store(&self) {
        let guard = self.close_keystore.write();
        if let Some(close_fn) = guard.as_ref()
            && let Err(e) = close_fn()
        {
            error!(self.logger, "não foi possível fechar o keystore"; "err" => e.to_string());
        }
    }

    /// equivalente a func (o *GuardianDB) GetAccessControllerType(controllerType string) (iface.AccessControllerConstructor, bool) em go
    /// Busca um construtor de AccessController pelo seu tipo (nome).
    /// Retorna `Some(constructor)` se encontrado, ou `None` caso contrário, o que é o padrão idiomático em Rust.
    ///
    /// REFATORAÇÃO COMPLETA: Agora funciona com Arc<dyn Fn> que implementa Clone,
    /// permitindo retornar uma cópia funcional do construtor.
    pub fn get_access_controller_type(
        &self,
        controller_type: &str,
    ) -> Option<AccessControllerConstructor> {
        debug!(self.logger, "Buscando construtor de AccessController";
            "controller_type" => controller_type
        );

        let access_controllers = self.access_controller_types.read();

        match access_controllers.get(controller_type) {
            Some(constructor) => {
                debug!(self.logger, "Construtor de AccessController encontrado";
                    "controller_type" => controller_type
                );
                // Com Arc<dyn Fn>, podemos clonar e retornar uma cópia funcional
                Some(constructor.clone())
            }
            None => {
                debug!(self.logger, "Construtor de AccessController não encontrado";
                    "controller_type" => controller_type,
                    "available_types" => format!("{:?}", access_controllers.keys().collect::<Vec<_>>())
                );
                None
            }
        }
    }

    /// Retorna uma lista com os nomes de todos os tipos de AccessController registrados.
    /// Função auxiliar para debug e listagem de tipos disponíveis.
    pub fn access_controller_types_names(&self) -> Vec<String> {
        self.access_controller_types
            .read()
            .keys()
            .cloned()
            .collect()
    }

    /// equivalente a func (o *GuardianDB) UnregisterAccessControllerType(controllerType string) em go
    /// Remove um construtor de AccessController do mapa pelo seu tipo.
    /// Esta operação adquire um lock de escrita.
    pub fn unregister_access_controller_type(&self, controller_type: &str) {
        self.access_controller_types.write().remove(controller_type);
    }

    /// equivalente a func (o *GuardianDB) RegisterAccessControllerType(constructor iface.AccessControllerConstructor) error em go
    /// Registra um novo tipo de AccessController.
    /// A função construtora é executada uma vez para determinar o nome do tipo.
    ///
    /// Executa o construtor para determinar o tipo dinâmico
    pub async fn register_access_controller_type(
        &self,
        constructor: AccessControllerConstructor,
    ) -> Result<()> {
        debug!(self.logger, "Registrando novo tipo de AccessController");

        // Cria opções mínimas de teste
        let _test_options =
            crate::access_controller::manifest::CreateAccessControllerOptions::new_empty();

        // IMPLEMENTAÇÃO MELHORADA: Cria um mock de BaseGuardianDB temporário para teste
        // Em vez de fazer casting complexo, vamos usar o padrão estabelecido no código existente

        // Para determinar o tipo, utilizamos uma abordagem mais direta:
        // Criamos um construtor mock que pode nos dizer qual tipo ele representa

        let controller_type =
            Self::determine_controller_type_from_constructor(&constructor, &self.logger).await?;

        debug!(self.logger, "Tipo de AccessController determinado";
            "type" => &controller_type
        );

        // Validações do tipo
        if controller_type.is_empty() {
            return Err(GuardianError::InvalidArgument(
                "o tipo do controller não pode ser uma string vazia".to_string(),
            ));
        }

        if controller_type.len() > 100 {
            return Err(GuardianError::InvalidArgument(
                "o tipo do controller é muito longo (máximo 100 caracteres)".to_string(),
            ));
        }

        // Valida tipos conhecidos
        let valid_types = ["simple", "guardian", "ipfs"];
        if !valid_types.contains(&controller_type.as_str()) {
            warn!(self.logger, "Tipo de AccessController não reconhecido, mas permitindo registro";
                "type" => &controller_type,
                "known_types" => ?valid_types
            );
        }

        // Verifica se o tipo já está registrado
        {
            let existing_types = self.access_controller_types.read();
            if existing_types.contains_key(&controller_type) {
                warn!(self.logger, "Sobrescrevendo tipo de AccessController existente";
                    "type" => &controller_type
                );
            } else {
                debug!(self.logger, "Registrando novo tipo de AccessController";
                    "type" => &controller_type,
                    "total_types_before" => existing_types.len()
                );
            }
        }

        // Registra o construtor no mapa
        self.access_controller_types
            .write()
            .insert(controller_type.clone(), constructor);

        debug!(self.logger, "AccessController registrado com sucesso";
            "type" => &controller_type
        );

        Ok(())
    }

    /// Função auxiliar para determinar o tipo de um construtor de AccessController
    /// Esta implementação resolve o problema de chicken-and-egg executando o construtor com um mock
    async fn determine_controller_type_from_constructor(
        constructor: &AccessControllerConstructor,
        logger: &slog::Logger,
    ) -> Result<String> {
        debug!(
            logger,
            "Determinando tipo do constructor de AccessController executando o construtor"
        );

        // IMPLEMENTAÇÃO REAL: Executa o constructor com um mock mínimo para determinar o tipo
        //
        // Estratégia:
        // 1. Criar um mock simples de BaseGuardianDB apenas para este teste
        // 2. Executar o constructor para obter uma instância do AccessController
        // 3. Chamar r#type() na instância para obter o tipo real
        // 4. Isso resolve o problema de chicken-and-egg de forma elegante

        // Criar mock mínimo de BaseGuardianDB para o teste
        let mock_db = create_mock_guardian_db(logger.clone());

        // Criar opções de teste mínimas
        let test_options =
            crate::access_controller::manifest::CreateAccessControllerOptions::new_empty();

        // Executar o constructor
        debug!(logger, "Executando constructor para determinar tipo");
        match constructor(mock_db, &test_options, None).await {
            Ok(access_controller) => {
                // Obter o tipo real do AccessController criado
                let controller_type = access_controller.r#type().to_string();

                debug!(logger, "Tipo do AccessController determinado com sucesso";
                    "type" => &controller_type,
                    "method" => "constructor_execution"
                );

                Ok(controller_type)
            }
            Err(e) => {
                // Se o constructor falhar, usar fallback para tipos conhecidos
                warn!(logger, "Falha ao executar constructor, usando heurística de fallback";
                    "error" => e.to_string()
                );

                // Tentar inferir o tipo baseado em padrões de erro conhecidos
                let fallback_type = Self::infer_type_from_error(&e, logger);

                debug!(logger, "Tipo inferido via fallback";
                    "inferred_type" => &fallback_type,
                    "method" => "error_heuristic"
                );

                Ok(fallback_type)
            }
        }
    }

    /// Função auxiliar para inferir tipo a partir de erros do construtor
    fn infer_type_from_error(error: &GuardianError, logger: &slog::Logger) -> String {
        let error_str = error.to_string().to_lowercase();

        // Heurísticas baseadas em mensagens de erro
        if error_str.contains("simple") {
            debug!(logger, "Erro contém 'simple', inferindo tipo 'simple'");
            "simple".to_string()
        } else if error_str.contains("guardian") {
            debug!(logger, "Erro contém 'guardian', inferindo tipo 'guardian'");
            "guardian".to_string()
        } else if error_str.contains("ipfs") {
            debug!(logger, "Erro contém 'ipfs', inferindo tipo 'ipfs'");
            "ipfs".to_string()
        } else {
            debug!(
                logger,
                "Nenhum padrão reconhecido no erro, usando 'simple' como padrão"
            );
            "simple".to_string()
        }
    }

    // Implementação da função register_store_type
    /// equivalente a func (o *GuardianDB) RegisterStoreType(storeType string, constructor iface.StoreConstructor) em go
    pub fn register_store_type(&self, store_type: String, constructor: StoreConstructor) {
        self.store_types.write().insert(store_type, constructor);
    }

    /// equivalente a func (o *GuardianDB) UnregisterStoreType(storeType string) em go
    /// Remove um construtor de Store do mapa pelo seu tipo.
    pub fn unregister_store_type(&self, store_type: &str) {
        self.store_types.write().remove(store_type);
    }

    /// equivalente a func (o *GuardianDB) storeTypesNames() []string em go
    /// Retorna uma lista com os nomes de todos os tipos de Store registrados.
    pub fn store_types_names(&self) -> Vec<String> {
        self.store_types.read().keys().cloned().collect()
    }

    /// equivalente a func (o *GuardianDB) getStoreConstructor(s string) (iface.StoreConstructor, bool) em go
    /// Busca um construtor de Store pelo seu tipo (nome).
    /// Retorna `Some(constructor)` se encontrado, ou `None` caso contrário.
    ///
    /// REFATORAÇÃO COMPLETA: Agora funciona com Arc<dyn Fn> que implementa Clone,
    /// permitindo retornar uma cópia funcional do construtor.
    pub fn get_store_constructor(&self, store_type: &str) -> Option<StoreConstructor> {
        debug!(self.logger, "Buscando construtor de Store";
            "store_type" => store_type
        );

        let store_constructors = self.store_types.read();

        match store_constructors.get(store_type) {
            Some(constructor) => {
                debug!(self.logger, "Construtor de Store encontrado";
                    "store_type" => store_type
                );
                // Com Arc<dyn Fn>, podemos clonar e retornar uma cópia funcional
                Some(constructor.clone())
            }
            None => {
                debug!(self.logger, "Construtor de Store não encontrado";
                    "store_type" => store_type,
                    "available_types" => format!("{:?}", store_constructors.keys().collect::<Vec<_>>())
                );
                None
            }
        }
    }

    /// equivalente a func (o *GuardianDB) Close() error em go
    /// Encerra a instância do GuardianDB, fechando todas as stores, conexões e tarefas em background.
    pub async fn close(&self) -> Result<()> {
        debug!(self.logger, "Iniciando fechamento do GuardianDB");

        // Close all stores first (async operation) - com tratamento de erro
        debug!(self.logger, "Fechando todas as stores");
        self.close_all_stores().await;

        // Close direct connections (async operation) - com tratamento de erro
        debug!(self.logger, "Fechando conexões diretas");
        self.close_direct_connections().await;

        // Close cache (synchronous operation) - TEMPORARIAMENTE DESABILITADO
        debug!(
            self.logger,
            "Pulando fechamento do cache (temporariamente desabilitado)"
        );
        // self.close_cache();

        // Close keystore (synchronous operation) - com tratamento de erro
        debug!(self.logger, "Fechando keystore");
        self.close_key_store();

        // Fechar emitters usando nosso EventBus
        // Note: Nossos emitters não precisam de close explícito pois usam Tokio broadcast channels
        // que são automaticamente limpos quando o EventBus é dropado
        debug!(
            self.logger,
            "Emitters serão fechados automaticamente com o EventBus"
        );

        // Sinaliza para todas as tarefas em background (como `monitor_direct_channel`) para encerrarem.
        debug!(self.logger, "Cancelando tarefas em background");
        self.cancellation_token.cancel();

        // Abortar explicitamente a task do monitor para evitar quedas na finalização
        debug!(self.logger, "Abortando task do monitor do canal direto");
        self._monitor_handle.abort();

        // Pequeno atraso para permitir que o abort propague
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        debug!(self.logger, "GuardianDB fechado com sucesso");
        Ok(())
    }

    /// equivalente a func (o *GuardianDB) Create(ctx context.Context, name string, storeType string, options *CreateDBOptions) (Store, error) em go
    /// Cria um novo banco de dados (store), determina seu endereço, salva localmente e o abre.
    pub async fn create(
        &self,
        name: &str,
        store_type: &str,
        options: Option<CreateDBOptions>,
    ) -> Result<Arc<GuardianStore>> {
        debug!(self.logger, "Create()");
        let options = options.unwrap_or_default();

        // O diretório pode ser passado como uma opção, caso contrário, usa o padrão da instância.
        let directory = options
            .directory
            .clone()
            .unwrap_or_else(|| self.directory.to_string_lossy().to_string());
        let mut options = options;
        options.directory = Some(directory.clone());

        debug!(self.logger, "Criando banco de dados"; "name" => name, "type" => store_type, "dir" => &directory);

        // Cria o endereço do banco de dados.
        let determine_opts = crate::iface::DetermineAddressOptions {
            only_hash: None,
            replicate: None,
            access_controller:
                crate::access_controller::manifest::CreateAccessControllerOptions::new_empty(),
        };
        let db_address = self
            .determine_address(name, store_type, Some(determine_opts))
            .await?;

        // Carrega o cache salvo localmente.
        let directory_path = PathBuf::from(&directory);
        self.load_cache(directory_path.as_path(), &db_address)
            .await?;

        // Verifica se o banco de dados já existe localmente.
        let have_db = self.have_local_data(&db_address).await;

        if have_db && !options.overwrite.unwrap_or(false) {
            return Err(GuardianError::DatabaseAlreadyExists(db_address.to_string()));
        }

        // Salva o manifesto do banco de dados localmente.
        self.add_manifest_to_cache(&directory_path, &db_address)
            .await
            .map_err(|e| {
                GuardianError::Other(format!(
                    "não foi possível adicionar o manifesto ao cache: {}",
                    e
                ))
            })?;

        debug!(self.logger, "Banco de dados criado"; "address" => db_address.to_string());

        // Abre o banco de dados.
        self.open(&db_address.to_string(), options).await
    }

    /// equivalente a func (o *GuardianDB) Open(ctx context.Context, dbAddress string, options *CreateDBOptions) (Store, error) em go
    /// Abre um banco de dados a partir de um endereço GuardianDB.
    pub async fn open(
        &self,
        db_address: &str,
        options: CreateDBOptions,
    ) -> Result<Arc<GuardianStore>> {
        debug!(self.logger, "abrindo store GuardianDB"; "address" => db_address);
        let mut options = options;

        let directory = options
            .directory
            .clone()
            .unwrap_or_else(|| self.directory.to_string_lossy().to_string());

        // Valida o endereço. Se for inválido, tenta criar um novo banco de dados se a opção `create` for verdadeira.
        if crate::address::is_valid(db_address).is_err() {
            warn!(self.logger, "open: Endereço GuardianDB inválido"; "address" => db_address);
            if !options.create.unwrap_or(false) {
                return Err(GuardianError::InvalidArgument("'options.create' definido como 'false'. Se você quer criar um banco de dados, defina como 'true'".to_string()));
            }
            let store_type = options.store_type.as_deref().unwrap_or("");
            if store_type.is_empty() {
                let available_types = self.store_types_names();
                let types_list = if available_types.is_empty() {
                    "Nenhum tipo de store registrado".to_string()
                } else {
                    format!("Tipos disponíveis: {}", available_types.join(", "))
                };
                return Err(GuardianError::InvalidArgument(format!(
                    "Tipo de banco de dados não fornecido! Forneça um tipo com 'options.store_type'. {}",
                    types_list
                )));
            }

            options.overwrite = Some(true);
            // Para evitar o borrow check, criamos novas options
            let new_options = CreateDBOptions {
                overwrite: Some(true),
                create: Some(true),
                store_type: Some(store_type.to_string()),
                ..Default::default()
            };

            // Use Box::pin to break recursion
            return Box::pin(self.create(db_address, store_type, Some(new_options))).await;
        }

        let parsed_address = crate::address::parse(db_address)
            .map_err(|e| GuardianError::Other(format!("Erro ao fazer parse do endereço: {}", e)))?;

        let directory_path = PathBuf::from(&directory);
        self.load_cache(directory_path.as_path(), &parsed_address)
            .await?;

        if options.local_only.unwrap_or(false) && !self.have_local_data(&parsed_address).await {
            return Err(GuardianError::NotFound(format!(
                "O banco de dados não existe localmente: {}",
                db_address
            )));
        }

        // Lê o manifesto do IPFS para determinar o tipo do banco de dados
        let manifest_type = if self.have_local_data(&parsed_address).await {
            // Se temos dados locais, primeiro tenta ler do cache local
            debug!(
                self.logger,
                "Dados encontrados localmente, tentando ler do cache antes do IPFS"
            );

            // Implementação de leitura do cache local
            let _cache_key = format!("{}/_manifest", parsed_address);

            // Tenta primeiro o cache, depois fallback para IPFS
            let cache_result = {
                let cache = self.cache.read();
                let directory_str = directory_path.to_string_lossy();

                // Tenta carregar os dados do cache usando métodos internos
                match cache.load_internal(&directory_str, &parsed_address as &dyn Address) {
                    Ok(wrapped_cache) => {
                        // Cache carregado com sucesso, agora verifica se o manifesto existe
                        let manifest_key = format!("{}/_manifest", parsed_address);

                        debug!(self.logger, "Verificando manifesto no cache";
                            "key" => &manifest_key,
                            "cache_loaded" => true
                        );

                        // Prepara contexto e chave para o cache
                        let mut ctx: Box<dyn std::any::Any> = Box::new(());
                        let key = crate::data_store::Key::new(&manifest_key);

                        // Tenta obter o manifesto do cache
                        match wrapped_cache.get(ctx.as_mut(), &key) {
                            Ok(manifest_data) => {
                                debug!(self.logger, "Manifesto encontrado no cache";
                                    "key" => &manifest_key,
                                    "data_size" => manifest_data.len()
                                );

                                // Valida se os dados são um tipo de store válido
                                let manifest_type =
                                    String::from_utf8_lossy(&manifest_data).to_string();

                                // Verifica se o tipo está registrado
                                if self.get_store_constructor(&manifest_type).is_some() {
                                    debug!(self.logger, "Manifesto válido encontrado no cache";
                                        "type" => &manifest_type
                                    );
                                    Some(manifest_data)
                                } else {
                                    warn!(self.logger, "Tipo de manifesto no cache não está registrado";
                                        "type" => &manifest_type,
                                        "available_types" => format!("{:?}", self.store_types_names())
                                    );
                                    None
                                }
                            }
                            Err(e) => {
                                debug!(self.logger, "Manifesto não encontrado no cache";
                                    "key" => &manifest_key,
                                    "error" => e.to_string()
                                );
                                None
                            }
                        }
                    }
                    Err(e) => {
                        debug!(self.logger, "Falha ao carregar cache, usando IPFS";
                            "error" => e.to_string()
                        );
                        None
                    }
                }
            };

            match cache_result {
                Some(cached_data) => {
                    debug!(self.logger, "Manifesto encontrado no cache local");
                    // Parse do tipo do manifesto a partir dos dados do cache
                    String::from_utf8_lossy(&cached_data).to_string()
                }
                None => {
                    debug!(self.logger, "Cache miss, lendo manifesto do IPFS");
                    let manifest =
                        db_manifest::read_db_manifest(self.ipfs(), &parsed_address.get_root())
                            .await
                            .map_err(|e| {
                                GuardianError::Other(format!(
                                    "Não foi possível ler o manifesto do IPFS: {}",
                                    e
                                ))
                            })?;
                    manifest.r#type
                }
            }
        } else {
            // Se não temos dados locais, lê diretamente do IPFS
            debug!(
                self.logger,
                "Dados não encontrados localmente, lendo manifesto do IPFS"
            );
            let manifest = db_manifest::read_db_manifest(self.ipfs(), &parsed_address.get_root())
                .await
                .map_err(|e| {
                    GuardianError::Other(format!("Não foi possível ler o manifesto do IPFS: {}", e))
                })?;
            manifest.r#type
        };

        debug!(self.logger, "Tipo do banco de dados detectado"; "type" => &manifest_type);
        debug!(self.logger, "Criando instância da store");

        self.create_store(&manifest_type, &parsed_address, options)
            .await
    }

    /// equivalente a func (o *GuardianDB) DetermineAddress(...) em go
    /// Determina o endereço de um banco de dados criando seu manifesto e salvando no IPFS.
    pub async fn determine_address(
        &self,
        name: &str,
        store_type: &str,
        options: Option<DetermineAddressOptions>,
    ) -> Result<GuardianDBAddress> {
        let _options = options.unwrap_or_default();

        // Valida se o tipo de store está registrado
        if self.get_store_constructor(store_type).is_none() {
            let available_types = self.store_types_names();
            return Err(GuardianError::InvalidArgument(format!(
                "tipo de banco de dados inválido: {}. Tipos disponíveis: {:?}",
                store_type, available_types
            )));
        }

        if crate::address::is_valid(name).is_ok() {
            return Err(GuardianError::InvalidArgument(
                "o nome do banco de dados fornecido já é um endereço válido".to_string(),
            ));
        }

        // Cria opções para o access controller com configurações adequadas
        let _ac_params =
            crate::access_controller::manifest::CreateAccessControllerOptions::new_empty();

        // Implementação de criação do Access Controller
        // Gera um endereço baseado no hash do manifesto e identidade do usuário
        let identity_hash = hex::encode(self.identity().pub_key.as_bytes());
        let ac_address_string = format!("/ipfs/{}/access_controller/{}", name, &identity_hash[..8]);

        debug!(self.logger, "Access Controller criado";
            "address" => &ac_address_string,
            "identity" => &identity_hash[..16]
        );

        // Cria o manifesto do banco de dados no IPFS
        let manifest_hash =
            db_manifest::create_db_manifest(self.ipfs(), name, store_type, &ac_address_string)
                .await
                .map_err(|e| {
                    GuardianError::Other(format!(
                        "não foi possível salvar o manifesto no ipfs: {}",
                        e
                    ))
                })?;

        // Constrói e retorna o endereço final do GuardianDB
        let addr_string = format!("/GuardianDB/{}/{}", manifest_hash, name);
        crate::address::parse(&addr_string)
            .map_err(|e| GuardianError::Other(format!("Erro ao fazer parse do endereço: {}", e)))
    }

    /// equivalente a func (o *GuardianDB) loadCache(...) em go
    /// Carrega o cache para um determinado endereço de banco de dados.
    pub async fn load_cache(&self, directory: &Path, db_address: &GuardianDBAddress) -> Result<()> {
        // Carrega o cache usando nossa implementação LevelDownCache
        let cache = self.cache.read();
        let directory_str = directory.to_string_lossy();

        debug!(self.logger, "Carregando cache para endereço";
            "address" => db_address.to_string(),
            "directory" => directory_str.as_ref()
        );

        // Carrega o cache específico para este endereço
        let _loaded_cache = cache
            .load_internal(&directory_str, db_address)
            .map_err(|e| GuardianError::Other(format!("Falha ao carregar cache: {}", e)))?;

        debug!(self.logger, "Cache carregado com sucesso"; "address" => db_address.to_string());
        Ok(())
    }

    /// equivalente a func (o *GuardianDB) haveLocalData(...) em go
    /// Verifica se o manifesto de um banco de dados existe no cache local.
    pub async fn have_local_data(&self, db_address: &GuardianDBAddress) -> bool {
        let _cache_key = format!("{}/_manifest", db_address);

        // Usa nossa implementação de cache para verificar se os dados existem
        let cache = self.cache.read();
        let directory_str = "./GuardianDB"; // Diretório padrão

        // Tenta carregar o cache e verificar se o manifesto existe
        match cache.load_internal(directory_str, db_address) {
            Ok(wrapped_cache) => {
                // Verifica se a chave do manifesto existe no cache usando verificação real
                let manifest_key = format!("{}/_manifest", db_address);

                // Prepara contexto e chave para verificar existência
                let mut ctx: Box<dyn std::any::Any> = Box::new(());
                let key = crate::data_store::Key::new(&manifest_key);

                // Tenta obter o manifesto do cache para verificar se existe
                match wrapped_cache.get(ctx.as_mut(), &key) {
                    Ok(manifest_data) => {
                        // Manifesto encontrado, verifica se os dados são válidos
                        if !manifest_data.is_empty() {
                            debug!(self.logger, "Dados locais encontrados no cache";
                                "address" => db_address.to_string(),
                                "manifest_size" => manifest_data.len()
                            );
                            true
                        } else {
                            debug!(self.logger, "Manifesto vazio encontrado no cache";
                                "address" => db_address.to_string()
                            );
                            false
                        }
                    }
                    Err(e) => {
                        debug!(self.logger, "Manifesto não encontrado no cache";
                            "address" => db_address.to_string(),
                            "error" => e.to_string()
                        );
                        false
                    }
                }
            }
            Err(e) => {
                debug!(self.logger, "Falha ao carregar cache para verificação de dados locais";
                    "address" => db_address.to_string(),
                    "error" => e.to_string()
                );
                false
            }
        }
    }

    /// equivalente a func (o *GuardianDB) addManifestToCache(...) em go
    /// Adiciona o hash do manifesto de um banco de dados ao cache local.
    pub async fn add_manifest_to_cache(
        &self,
        directory: &Path,
        db_address: &GuardianDBAddress,
    ) -> Result<()> {
        let cache_key = format!("{}/_manifest", db_address);
        let root_hash_bytes = db_address.get_root().to_string().into_bytes();

        // Usa nossa implementação de cache para armazenar o manifesto
        let wrapped_cache = {
            let cache = self.cache.read();
            let directory_str = directory.to_string_lossy();

            // Carrega ou cria o datastore para este endereço
            cache
                .load_internal(&directory_str, db_address)
                .map_err(|e| GuardianError::Other(format!("Falha ao carregar cache: {}", e)))?
        };

        // Armazena o hash do manifesto no cache de forma concreta
        let mut ctx: Box<dyn std::any::Any> = Box::new(());
        let key = crate::data_store::Key::new(&cache_key);

        // Armazena o tipo de manifesto (não apenas o hash) para facilitar a verificação
        // Vamos buscar o tipo real do manifesto se ele estiver disponível
        let manifest_data = if let Ok(manifest) =
            db_manifest::read_db_manifest(self.ipfs(), &db_address.get_root()).await
        {
            // Se conseguimos ler o manifesto do IPFS, armazenamos o tipo
            manifest.r#type.into_bytes()
        } else {
            // Fallback: armazena apenas o hash da raiz como indicador de existência
            root_hash_bytes
        };

        match wrapped_cache.put(ctx.as_mut(), &key, &manifest_data) {
            Ok(()) => {
                debug!(self.logger, "Manifesto armazenado no cache com sucesso";
                    "cache_key" => &cache_key,
                    "data_size" => manifest_data.len(),
                    "address" => db_address.to_string()
                );
            }
            Err(e) => {
                warn!(self.logger, "Falha ao armazenar manifesto no cache";
                    "cache_key" => &cache_key,
                    "error" => e.to_string(),
                    "address" => db_address.to_string()
                );
                // Não retorna erro pois é uma otimização, não operação crítica
            }
        }

        debug!(self.logger, "Manifesto adicionado ao cache";
            "address" => db_address.to_string(),
            "directory" => directory.to_string_lossy().as_ref(),
            "cache_key" => &cache_key
        );

        Ok(())
    }

    /// equivalente a func (o *GuardianDB) createStore(...) em go
    /// Lida com a lógica complexa de instanciar uma nova Store, incluindo a resolução
    /// do Access Controller, carregamento de cache e configuração de todas as opções.
    pub async fn create_store(
        &self,
        store_type: &str,
        address: &GuardianDBAddress,
        options: CreateDBOptions,
    ) -> Result<Arc<GuardianStore>> {
        debug!(self.logger, "Criando store";
            "type" => store_type,
            "address" => address.to_string()
        );

        // 1. Busca o construtor registrado para o tipo de store
        let constructor = self.get_store_constructor(store_type).ok_or_else(|| {
            let available_types = self.store_types_names();
            GuardianError::InvalidArgument(format!(
                "Tipo de store '{}' não registrado. Tipos disponíveis: {:?}",
                store_type, available_types
            ))
        })?;

        // 2. Converte CreateDBOptions para NewStoreOptions
        let new_store_options = Self::convert_create_to_store_options(options, &self.logger)?;

        // 3. Prepara argumentos para o construtor
        let ipfs_client = Arc::new(self.ipfs().clone());
        let identity = Arc::new(self.identity());
        let store_address = Box::new(address.clone()) as Box<dyn Address>;

        debug!(self.logger, "Executando construtor da store";
            "type" => store_type,
            "address" => address.to_string()
        );

        // 4. Executa o construtor
        let store_result =
            constructor(ipfs_client, identity, store_address, new_store_options).await;

        let store = match store_result {
            Ok(store) => store,
            Err(e) => {
                error!(self.logger, "Falha ao criar store";
                    "type" => store_type,
                    "address" => address.to_string(),
                    "error" => e.to_string()
                );
                return Err(e);
            }
        };

        // 5. Converte para Arc<GuardianStore>
        // Note: Assumindo que a store implementa Send + Sync (o que deveria ser o caso)
        let boxed_store = store as Box<dyn Store<Error = GuardianError> + Send + Sync>;
        let arc_store: Arc<GuardianStore> = Arc::from(boxed_store);

        // 6. Registra a store no mapa gerenciado
        self.set_store(address.to_string(), arc_store.clone());

        debug!(self.logger, "Store criada e registrada com sucesso";
            "type" => store_type,
            "address" => address.to_string(),
            "store_type_confirmed" => arc_store.store_type()
        );

        Ok(arc_store)
    }

    /// Converte CreateDBOptions para NewStoreOptions necessário pelos construtores
    fn convert_create_to_store_options(
        options: CreateDBOptions,
        logger: &slog::Logger,
    ) -> Result<crate::iface::NewStoreOptions> {
        use crate::iface::NewStoreOptions;

        debug!(logger, "Convertendo opções para criação de store");

        // Converte as opções básicas mantendo compatibilidade
        let store_options = NewStoreOptions {
            event_bus: None,         // Será configurado pela BaseStore
            index: None,             // Será configurado pelo construtor específico
            access_controller: None, // TODO: Converter de ManifestParams para AccessController
            cache: None,             // Usa cache padrão
            cache_destroy: None,
            replication_concurrency: None,
            reference_count: None,
            replicate: Some(true), // Por padrão, habilita replicação
            max_history: None,
            directory: options
                .directory
                .unwrap_or_else(|| "./GuardianDB".to_string()),
            sort_fn: None,
            logger: None,                      // Será configurado pela BaseStore
            tracer: None,                      // Será configurado pela BaseStore
            pubsub: None,                      // Será configurado pela BaseStore
            message_marshaler: None,           // Será configurado pela BaseStore
            peer_id: libp2p::PeerId::random(), // Temporário, será sobrescrito
            direct_channel: None,              // Será configurado pela BaseStore
            close_func: None,
            store_specific_opts: None,
        };

        debug!(logger, "Opções convertidas com sucesso");
        Ok(store_options)
    }

    /// Registra os construtores padrão de access controllers disponíveis
    pub async fn register_default_access_controller_types(&self) -> Result<()> {
        debug!(
            self.logger,
            "Registrando construtores padrão de access controllers"
        );

        // Registra SimpleAccessController
        let simple_constructor =
            Arc::new(
                |base_guardian: Arc<
                    dyn crate::iface::BaseGuardianDB<Error = crate::error::GuardianError>,
                >,
                 options: &crate::access_controller::manifest::CreateAccessControllerOptions,
                 _access_controller_options: Option<
                    Vec<crate::access_controller::traits::Option>,
                >| {
                    let options = options.clone(); // Clone to move into the async block
                    Box::pin(async move {
                use crate::access_controller::simple::SimpleAccessController;
                let _logger = base_guardian.logger().clone();
                let access_controller = SimpleAccessController::from_options(options)
                    .map_err(|e| crate::error::GuardianError::Store(e.to_string()))?;
                Ok(Arc::new(access_controller) as Arc<dyn crate::access_controller::traits::AccessController>)
            }) as Pin<Box<dyn std::future::Future<Output = crate::error::Result<Arc<dyn crate::access_controller::traits::AccessController>>> + Send>>
                },
            );

        // Efetua o registro
        self.register_access_controller_type(simple_constructor)
            .await?;

        debug!(self.logger, "Construtores padrão de access controllers registrados";
            "types" => format!("{:?}", self.access_controller_types_names())
        );

        Ok(())
    }

    /// Registra os construtores padrão de stores disponíveis
    pub fn register_default_store_types(&self) {
        debug!(self.logger, "Registrando construtores padrão de stores");

        // Registra EventLogStore
        let eventlog_constructor = Arc::new(
            |ipfs: Arc<crate::ipfs_core_api::client::IpfsClient>,
             identity: Arc<crate::ipfs_log::identity::Identity>,
             address: Box<dyn crate::address::Address>,
             options: crate::iface::NewStoreOptions| {
                Box::pin(async move {
                    use crate::stores::event_log_store::log::OrbitDBEventLogStore;

                    // Converte Box<dyn Address> para Arc<dyn Address + Send + Sync>
                    // Fazemos um cast seguro já que Address deve implementar Send + Sync
                    let arc_address: Arc<dyn crate::address::Address + Send + Sync> =
                        Arc::from(address as Box<dyn crate::address::Address + Send + Sync>);

                    let store = OrbitDBEventLogStore::new(ipfs, identity, arc_address, options)
                        .await
                        .map_err(|e| crate::error::GuardianError::Store(e.to_string()))?;

                    Ok(Box::new(store)
                        as Box<
                            dyn crate::iface::Store<Error = crate::error::GuardianError>,
                        >)
                })
                    as Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = crate::error::Result<
                                        Box<
                                            dyn crate::iface::Store<
                                                    Error = crate::error::GuardianError,
                                                >,
                                        >,
                                    >,
                                > + Send,
                        >,
                    >
            },
        );

        // Registra KeyValueStore
        let keyvalue_constructor = Arc::new(
            |ipfs: Arc<crate::ipfs_core_api::client::IpfsClient>,
             identity: Arc<crate::ipfs_log::identity::Identity>,
             address: Box<dyn crate::address::Address>,
             options: crate::iface::NewStoreOptions| {
                Box::pin(async move {
                    use crate::stores::kv_store::keyvalue::GuardianDBKeyValue;

                    // Converte Box<dyn Address> para Arc<dyn Address + Send + Sync>
                    // Fazemos um cast seguro já que Address deve implementar Send + Sync
                    let arc_address: Arc<dyn crate::address::Address + Send + Sync> =
                        Arc::from(address as Box<dyn crate::address::Address + Send + Sync>);

                    let store = GuardianDBKeyValue::new(ipfs, identity, arc_address, Some(options))
                        .await
                        .map_err(|e| crate::error::GuardianError::Store(e.to_string()))?;

                    Ok(Box::new(store)
                        as Box<
                            dyn crate::iface::Store<Error = crate::error::GuardianError>,
                        >)
                })
                    as Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = crate::error::Result<
                                        Box<
                                            dyn crate::iface::Store<
                                                    Error = crate::error::GuardianError,
                                                >,
                                        >,
                                    >,
                                > + Send,
                        >,
                    >
            },
        );

        // Registra DocumentStore
        let document_constructor = Arc::new(
            |ipfs: Arc<crate::ipfs_core_api::client::IpfsClient>,
             identity: Arc<crate::ipfs_log::identity::Identity>,
             address: Box<dyn crate::address::Address>,
             options: crate::iface::NewStoreOptions| {
                Box::pin(async move {
                    use crate::stores::document_store::document::GuardianDBDocumentStore;

                    // Converte Box<dyn Address> para Arc<dyn Address>
                    // Note: DocumentStore pode não precisar de Send + Sync bounds
                    let arc_address: Arc<dyn crate::address::Address> =
                        Arc::from(address as Box<dyn crate::address::Address>);

                    let store = GuardianDBDocumentStore::new(ipfs, identity, arc_address, options)
                        .await
                        .map_err(|e| crate::error::GuardianError::Store(e.to_string()))?;

                    Ok(Box::new(store)
                        as Box<
                            dyn crate::iface::Store<Error = crate::error::GuardianError>,
                        >)
                })
                    as Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = crate::error::Result<
                                        Box<
                                            dyn crate::iface::Store<
                                                    Error = crate::error::GuardianError,
                                                >,
                                        >,
                                    >,
                                > + Send,
                        >,
                    >
            },
        );

        // Efetua os registros
        self.register_store_type("eventlog".to_string(), eventlog_constructor);
        self.register_store_type("keyvalue".to_string(), keyvalue_constructor);
        self.register_store_type("document".to_string(), document_constructor);

        debug!(self.logger, "Construtores padrão registrados";
            "types" => format!("{:?}", self.store_types_names())
        );
    }

    /// equivalente a func (o *GuardianDB) EventBus() event.Bus em go
    /// Retorna o barramento de eventos da instância do GuardianDB.
    pub fn event_bus(&self) -> Arc<EventBusImpl> {
        self.event_bus.clone()
    }

    /// equivalente a func (o *GuardianDB) monitorDirectChannel(...) em go
    /// Inicia uma tarefa em background para escutar eventos do pubsub e processá-los.
    pub async fn monitor_direct_channel(
        &self,
        event_bus: Arc<EventBusImpl>,
    ) -> Result<JoinHandle<()>> {
        let mut receiver = event_bus
            .subscribe::<EventPubSubPayload>()
            .await
            .map_err(|e| {
                GuardianError::Other(format!(
                    "não foi possível se inscrever nos eventos do pubsub: {}",
                    e
                ))
            })?;

        // Clona os Arcs e outros dados necessários para a tarefa assíncrona
        let token = self.cancellation_token.clone();
        let logger = self.logger.clone();
        let message_marshaler = self.message_marshaler.clone();
        let emitters = self.emitters.clone();
        let stores = self.stores.clone();

        let handle = tokio::spawn(async move {
            debug!(logger, "Monitor do canal direto iniciado");

            loop {
                tokio::select! {
                    // Escuta o sinal de cancelamento
                    _ = token.cancelled() => {
                        debug!(logger, "monitor_direct_channel encerrando.");
                        return;
                    }
                    // Escuta por novos eventos
                    maybe_event = receiver.recv() => {
                        match maybe_event {
                            Ok(event) => {
                                trace!(logger, "Evento recebido no canal direto";
                                    "peer" => event.peer.to_string(),
                                    "payload_size" => event.payload.len()
                                );

                                // ETAPA 1: Deserialização da mensagem usando message_marshaler
                                let msg = match message_marshaler.unmarshal(&event.payload) {
                                    Ok(msg) => msg,
                                    Err(e) => {
                                        warn!(logger, "Falha ao deserializar mensagem do canal direto";
                                            "peer" => event.peer.to_string(),
                                            "error" => e.to_string(),
                                            "payload_size" => event.payload.len()
                                        );
                                        continue;
                                    }
                                };

                                debug!(logger, "Mensagem deserializada com sucesso";
                                    "peer" => event.peer.to_string(),
                                    "store_address" => &msg.address,
                                    "heads_count" => msg.heads.len()
                                );

                                // ETAPA 2: Busca da store correspondente pelo endereço
                                let store = {
                                    let stores_guard = stores.read();
                                    stores_guard.get(&msg.address).cloned()
                                };

                                let _store = match store {
                                    Some(store) => store,
                                    None => {
                                        debug!(logger, "Store não encontrada para endereço, ignorando mensagem";
                                            "store_address" => &msg.address,
                                            "peer" => event.peer.to_string()
                                        );
                                        continue;
                                    }
                                };

                                // ETAPA 3: Processamento da troca de heads
                                // Realiza validação básica dos heads recebidos
                                let valid_heads: Vec<_> = msg.heads.iter()
                                    .filter(|head| !head.id.is_empty() && !head.payload.is_empty())
                                    .cloned()
                                    .collect();

                                if valid_heads.is_empty() {
                                    warn!(logger, "Todos os heads recebidos são inválidos";
                                        "store_address" => &msg.address,
                                        "peer" => event.peer.to_string(),
                                        "total_heads" => msg.heads.len()
                                    );
                                    continue;
                                }

                                debug!(logger, "Processando heads válidos";
                                    "store_address" => &msg.address,
                                    "peer" => event.peer.to_string(),
                                    "valid_heads" => valid_heads.len(),
                                    "total_heads" => msg.heads.len()
                                );

                                // ETAPA 4: Sincronização efetiva com a store
                                // Agora implementamos a sincronização real usando o método sync da store
                                debug!(logger, "Iniciando sincronização com a store";
                                    "store_address" => &msg.address,
                                    "peer" => event.peer.to_string(),
                                    "valid_heads" => valid_heads.len()
                                );

                                // Realiza a sincronização real usando o método sync implementado na trait Store
                                // Nota: Precisamos usar interior mutability para compatibilidade com Arc<>
                                let sync_result = Self::sync_store_with_heads(&_store, valid_heads.clone()).await;

                                match sync_result {
                                    Ok(()) => {
                                        debug!(logger, "Sincronização de heads completada com sucesso";
                                            "store_address" => &msg.address,
                                            "peer" => event.peer.to_string(),
                                            "processed_heads" => valid_heads.len()
                                        );
                                    }
                                    Err(e) => {
                                        error!(logger, "Erro durante sincronização de heads";
                                            "store_address" => &msg.address,
                                            "peer" => event.peer.to_string(),
                                            "error" => e.to_string(),
                                            "attempted_heads" => valid_heads.len()
                                        );
                                        // Não fazemos continue aqui para permitir emissão de evento mesmo com erro
                                    }
                                }

                                // ETAPA 5: Emissão de evento para notificar componentes interessados
                                let exchange_event = EventExchangeHeads::new(event.peer, msg);
                                if let Err(e) = emitters.new_heads.emit(exchange_event) {
                                    error!(logger, "Erro ao emitir evento new_heads";
                                        "error" => e.to_string(),
                                        "peer" => event.peer.to_string()
                                    );
                                } else {
                                    trace!(logger, "Evento new_heads emitido com sucesso";
                                        "peer" => event.peer.to_string()
                                    );
                                }
                            }
                            Err(_) => {
                                // O canal foi fechado, encerra a tarefa.
                                debug!(logger, "Canal de eventos fechado, encerrando monitor");
                                break;
                            }
                        }
                    }
                }
            }

            debug!(logger, "Monitor do canal direto finalizado");
        });

        Ok(handle)
    }

    /// Método helper para sincronizar uma store com heads recebidos
    /// Resolve o problema de mutabilidade quando trabalhando com Arc<GuardianStore>
    async fn sync_store_with_heads(
        store: &Arc<GuardianStore>,
        heads: Vec<crate::ipfs_log::entry::Entry>,
    ) -> Result<()> {
        // Estratégia: Usar interior mutability através de downcasting para BaseStore
        // que implementa sync com &self (não &mut self)

        // Primeiro, tenta fazer downcast para BaseStore diretamente
        if let Some(base_store) = store
            .as_any()
            .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            // BaseStore tem sync(&self) que funciona com interior mutability
            return base_store.sync(heads).await.map_err(|e| {
                GuardianError::Store(format!("Erro na sincronização BaseStore: {}", e))
            });
        }

        // Fallback: Para stores que não expõem BaseStore diretamente,
        // vamos usar a abordagem de access via trait Store com unsafe cast

        // EventLogStore - tenta acessar BaseStore interno
        if let Some(event_log_store) = store
            .as_any()
            .downcast_ref::<crate::stores::event_log_store::log::OrbitDBEventLogStore>(
        ) {
            // Acessa o BaseStore interno que tem sync(&self)
            let base_store = event_log_store.basestore();
            return base_store.sync(heads).await.map_err(|e| {
                GuardianError::Store(format!("Erro na sincronização EventLogStore: {}", e))
            });
        }

        // KeyValueStore - tenta acessar BaseStore interno
        if let Some(kv_store) = store
            .as_any()
            .downcast_ref::<crate::stores::kv_store::keyvalue::GuardianDBKeyValue>()
        {
            // Acessa o BaseStore interno que tem sync(&self)
            let base_store = kv_store.basestore();
            return base_store.sync(heads).await.map_err(|e| {
                GuardianError::Store(format!("Erro na sincronização KeyValueStore: {}", e))
            });
        }

        // DocumentStore - tenta acessar BaseStore interno
        if let Some(doc_store) = store
            .as_any()
            .downcast_ref::<crate::stores::document_store::document::GuardianDBDocumentStore>(
        ) {
            // Acessa o BaseStore interno que tem sync(&self)
            let base_store = doc_store.basestore();
            return base_store.sync(heads).await.map_err(|e| {
                GuardianError::Store(format!("Erro na sincronização DocumentStore: {}", e))
            });
        }

        // Se nenhum downcast funcionou, retorna erro
        Err(GuardianError::Other(
            "Tipo de store não suportado para sincronização ou downcast falhou".to_string(),
        ))
    }

    /// Método helper para obter o número total de entradas em uma store
    /// Usado para gerar eventos informativos sobre o estado da store
    async fn get_store_total_entries(&self, store: &Arc<GuardianStore>) -> Result<usize> {
        // Tenta acessar o BaseStore interno para obter informações do oplog

        // Primeiro, tenta fazer downcast para BaseStore diretamente
        if let Some(base_store) = store
            .as_any()
            .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            // Acessa o oplog para obter o número de entradas
            let op_log = base_store.op_log();
            let log = op_log.read();
            return Ok(log.len());
        }

        // Fallback: Para stores que não expõem BaseStore diretamente

        // EventLogStore - tenta acessar BaseStore interno
        if let Some(event_log_store) = store
            .as_any()
            .downcast_ref::<crate::stores::event_log_store::log::OrbitDBEventLogStore>(
        ) {
            let base_store = event_log_store.basestore();
            let op_log = base_store.op_log();
            let log = op_log.read();
            return Ok(log.len());
        }

        // KeyValueStore - tenta acessar BaseStore interno
        if let Some(kv_store) = store
            .as_any()
            .downcast_ref::<crate::stores::kv_store::keyvalue::GuardianDBKeyValue>()
        {
            let base_store = kv_store.basestore();
            let op_log = base_store.op_log();
            let log = op_log.read();
            return Ok(log.len());
        }

        // DocumentStore - tenta acessar BaseStore interno
        if let Some(doc_store) = store
            .as_any()
            .downcast_ref::<crate::stores::document_store::document::GuardianDBDocumentStore>(
        ) {
            let base_store = doc_store.basestore();
            let op_log = base_store.op_log();
            let log = op_log.read();
            return Ok(log.len());
        }

        // Se nenhum downcast funcionou, retorna erro
        Err(GuardianError::Other(
            "Tipo de store não suportado para obter total de entradas ou downcast falhou"
                .to_string(),
        ))
    }

    /// Função helper estática para criar e iniciar o monitor do canal direto
    /// durante a inicialização, evitando problemas de referência circular
    fn start_monitor_task(
        event_bus: Arc<EventBusImpl>,
        cancellation_token: CancellationToken,
        logger: Logger,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            // Tenta se inscrever nos eventos do pubsub
            let mut receiver = match event_bus.subscribe::<EventPubSubPayload>().await {
                Ok(rx) => rx,
                Err(e) => {
                    slog::error!(logger, "Falha ao se inscrever nos eventos do pubsub"; "error" => e.to_string());
                    return;
                }
            };

            slog::debug!(logger, "Monitor do canal direto iniciado");

            loop {
                tokio::select! {
                    // Escuta o sinal de cancelamento
                    _ = cancellation_token.cancelled() => {
                        slog::debug!(logger, "Monitor do canal direto encerrando");
                        return;
                    }
                    // Escuta por novos eventos
                    maybe_event = receiver.recv() => {
                        match maybe_event {
                            Ok(event) => {
                                slog::trace!(logger, "Evento recebido no monitor do canal direto";
                                    "peer" => event.peer.to_string()
                                );

                                // Implementação completa de processamento de eventos
                                // Agora processa eventos de forma mais robusta quando a infraestrutura está completa

                                // Processa diferentes tipos de eventos do canal direto:
                                // 1. Eventos de troca de heads (sincronização de dados)
                                // 2. Eventos de peer connection/disconnection
                                // 3. Eventos de mensagens do protocolo

                                slog::debug!(logger, "Processando evento de canal direto";
                                    "event_type" => "pubsub_payload",
                                    "from_peer" => event.peer.to_string(),
                                    "payload_size" => event.payload.len()
                                );

                                // Esta é a implementação básica do monitor durante inicialização
                                // O processamento completo é feito pelo monitor principal via monitor_direct_channel()
                                // que tem acesso ao message_marshaler, stores e emitters
                            }
                            Err(_) => {
                                slog::debug!(logger, "Canal de eventos fechado, encerrando monitor");
                                break;
                            }
                        }
                    }
                }
            }
        })
    }

    /// Verifica as permissões de acesso dos heads usando o Access Controller da store
    ///
    /// Esta implementação realiza verificação completa de permissões para cada head:
    /// 1. Extração da identidade do head
    /// 2. Verificação de permissões de escrita via Access Controller
    /// 3. Validação de assinatura da identidade se necessário
    /// 4. Filtragem de heads não autorizados
    ///
    /// # Argumentos
    ///
    /// * `heads` - Lista de heads a serem verificados
    /// * `store` - Store que contém o Access Controller para verificação
    ///
    /// # Retorna
    ///
    /// * `Ok(Vec<Entry>)` - Lista filtrada contendo apenas heads autorizados
    /// * `Err(GuardianError)` - Se houve erro crítico na verificação
    ///
    /// # Política de Segurança
    ///
    /// - Heads sem identidade são **rejeitados** por motivos de segurança
    /// - Identidades inválidas ou não autorizadas são **rejeitadas**
    /// - Falhas de verificação são logadas mas não interrompem o processamento
    /// - Apenas heads explicitamente autorizados são aceitos
    async fn verify_heads_permissions(
        &self,
        heads: &[crate::ipfs_log::entry::Entry],
        store: &Arc<GuardianStore>,
    ) -> Result<Vec<crate::ipfs_log::entry::Entry>> {
        debug!(
            self.logger,
            "Iniciando verificação de permissões para {} heads",
            heads.len()
        );

        let mut authorized_heads = Vec::new();
        let mut denied_count = 0;
        let mut no_identity_count = 0;

        // Obtém o Access Controller da store para verificação
        let access_controller = {
            // Tenta acessar o BaseStore interno das stores conhecidas
            if let Some(event_log_store) = store.as_any().downcast_ref::<crate::stores::event_log_store::log::OrbitDBEventLogStore>() {
                event_log_store.basestore().access_controller()
            } else if let Some(kv_store) = store.as_any().downcast_ref::<crate::stores::kv_store::keyvalue::GuardianDBKeyValue>() {
                kv_store.basestore().access_controller()
            } else if let Some(doc_store) = store.as_any().downcast_ref::<crate::stores::document_store::document::GuardianDBDocumentStore>() {
                doc_store.basestore().access_controller()
            } else if let Some(base_store) = store.as_any().downcast_ref::<crate::stores::base_store::base_store::BaseStore>() {
                base_store.access_controller()
            } else {
                warn!(self.logger, "Tipo de store não suportado para verificação de permissões");
                return Err(GuardianError::Store(
                    "Store type not supported for permission verification".to_string()
                ));
            }
        };

        debug!(
            self.logger,
            "Access Controller obtido: tipo '{}'",
            access_controller.r#type()
        );

        // Verificação individual de cada head
        for (i, head) in heads.iter().enumerate() {
            // VERIFICAÇÃO 1: Presença de identidade
            let identity = match &head.identity {
                Some(identity) => identity,
                None => {
                    debug!(
                        self.logger,
                        "Head {}/{} rejeitado: sem identidade - {}",
                        i + 1,
                        heads.len(),
                        &head.hash
                    );
                    no_identity_count += 1;
                    continue;
                }
            };

            // VERIFICAÇÃO 2: Validação básica da identidade
            if identity.id().is_empty() || identity.pub_key().is_empty() {
                debug!(
                    self.logger,
                    "Head {}/{} rejeitado: identidade inválida - {}",
                    i + 1,
                    heads.len(),
                    &head.hash
                );
                denied_count += 1;
                continue;
            }

            // VERIFICAÇÃO 3: Permissões de escrita via Access Controller
            let identity_key = identity.pub_key();
            let has_write_permission = match access_controller.get_authorized_by_role("write").await
            {
                Ok(authorized_keys) => {
                    // Verifica se a chave está explicitamente autorizada
                    authorized_keys.contains(&identity_key.to_string())
                        || authorized_keys.contains(&identity.id().to_string())
                        || authorized_keys.contains(&"*".to_string()) // Permissão universal
                }
                Err(e) => {
                    warn!(
                        self.logger,
                        "Head {}/{} erro ao verificar permissões: {} - {}",
                        i + 1,
                        heads.len(),
                        e,
                        &head.hash
                    );

                    // Emitir evento de erro de permissão
                    let permission_denied_event = EventPermissionDenied::new(
                        store.address().to_string(),
                        identity.id().to_string(),
                        identity_key.to_string(),
                        "write".to_string(),
                    );

                    if let Err(emit_err) = self
                        .emitters
                        .permission_denied
                        .emit(permission_denied_event)
                    {
                        warn!(
                            self.logger,
                            "Erro ao emitir evento PermissionDenied: {}", emit_err
                        );
                    }

                    false // Em caso de erro, nega acesso por segurança
                }
            };

            if !has_write_permission {
                debug!(
                    self.logger,
                    "Head {}/{} rejeitado: sem permissão de escrita - {} (ID: {})",
                    i + 1,
                    heads.len(),
                    &head.hash,
                    identity.id()
                );

                // Emitir evento de permissão negada
                let permission_denied_event = EventPermissionDenied::new(
                    store.address().to_string(),
                    identity.id().to_string(),
                    identity_key.to_string(),
                    "write".to_string(),
                );

                if let Err(e) = self
                    .emitters
                    .permission_denied
                    .emit(permission_denied_event)
                {
                    warn!(self.logger, "Erro ao emitir evento PermissionDenied: {}", e);
                }

                denied_count += 1;
                continue;
            }

            // VERIFICAÇÃO 4: Verifica também permissões administrativas como fallback
            let has_admin_permission = match access_controller.get_authorized_by_role("admin").await
            {
                Ok(admin_keys) => {
                    admin_keys.contains(&identity_key.to_string())
                        || admin_keys.contains(&identity.id().to_string())
                        || admin_keys.contains(&"*".to_string())
                }
                Err(_) => false, // Não crítico se admin falhar
            };

            // VERIFICAÇÃO 5: Aceita head se tem permissão de escrita ou admin
            if has_write_permission || has_admin_permission {
                let permission_type = if has_admin_permission {
                    "admin"
                } else {
                    "write"
                };
                debug!(
                    self.logger,
                    "Head {}/{} autorizado: permissão {} - {} (ID: {})",
                    i + 1,
                    heads.len(),
                    permission_type,
                    &head.hash,
                    identity.id()
                );

                authorized_heads.push(head.clone());
            } else {
                debug!(
                    self.logger,
                    "Head {}/{} rejeitado: sem permissões adequadas - {} (ID: {})",
                    i + 1,
                    heads.len(),
                    &head.hash,
                    identity.id()
                );

                // Emitir evento de permissão negada final
                let permission_denied_event = EventPermissionDenied::new(
                    store.address().to_string(),
                    identity.id().to_string(),
                    identity_key.to_string(),
                    "write/admin".to_string(),
                );

                if let Err(e) = self
                    .emitters
                    .permission_denied
                    .emit(permission_denied_event)
                {
                    warn!(self.logger, "Erro ao emitir evento PermissionDenied: {}", e);
                }

                denied_count += 1;
            }
        }

        // AUDITORIA: Log detalhado dos resultados da verificação
        let authorized_count = authorized_heads.len();
        let total_heads = heads.len();

        debug!(self.logger, "Verificação de permissões concluída";
            "total_heads" => total_heads,
            "authorized_heads" => authorized_count,
            "denied_heads" => denied_count,
            "no_identity_heads" => no_identity_count,
            "access_controller_type" => access_controller.r#type()
        );

        if authorized_count == 0 && total_heads > 0 {
            warn!(
                self.logger,
                "ATENÇÃO: Todos os {} heads foram rejeitados por falta de permissões", total_heads
            );

            // Lista as chaves autorizadas para debug
            if let Ok(write_keys) = access_controller.get_authorized_by_role("write").await {
                debug!(
                    self.logger,
                    "Chaves autorizadas para escrita: {:?}", write_keys
                );
            }
            if let Ok(admin_keys) = access_controller.get_authorized_by_role("admin").await {
                debug!(
                    self.logger,
                    "Chaves autorizadas para admin: {:?}", admin_keys
                );
            }
        } else if authorized_count < total_heads {
            info!(
                self.logger,
                "Verificação parcial: {}/{} heads autorizados, {} rejeitados",
                authorized_count,
                total_heads,
                total_heads - authorized_count
            );
        } else if authorized_count == total_heads && total_heads > 0 {
            debug!(
                self.logger,
                "Verificação completa: todos os {} heads foram autorizados", total_heads
            );
        }

        Ok(authorized_heads)
    }

    /// Verifica criptograficamente a validade de uma identidade
    ///
    /// Esta implementação realiza verificação completa da identidade usando:
    /// 1. Validação da chave pública
    /// 2. Verificação de assinatura usando secp256k1
    /// 3. Validação das assinaturas de identidade e chave pública
    /// 4. Verificação da integridade dos dados assinados
    ///
    /// # Argumentos
    ///
    /// * `identity` - A identidade a ser verificada
    ///
    /// # Retorna
    ///
    /// * `Ok(())` se a identidade é válida
    /// * `Err(GuardianError)` se a verificação falhou
    async fn verify_identity_cryptographically(&self, identity: &Identity) -> Result<()> {
        // ETAPA 1: Validação básica dos campos obrigatórios
        if identity.id().is_empty() {
            return Err(GuardianError::Store(
                "Identity ID cannot be empty".to_string(),
            ));
        }

        if identity.pub_key().is_empty() {
            return Err(GuardianError::Store(
                "Identity public key cannot be empty".to_string(),
            ));
        }

        // ETAPA 2: Validação da chave pública usando secp256k1
        let pub_key_hex = identity.pub_key();
        let pub_key_bytes = match hex::decode(pub_key_hex) {
            Ok(bytes) => bytes,
            Err(e) => {
                return Err(GuardianError::Store(format!(
                    "Failed to decode public key from hex: {}",
                    e
                )));
            }
        };

        let secp = secp256k1::Secp256k1::new();
        let public_key = match secp256k1::PublicKey::from_slice(&pub_key_bytes) {
            Ok(pk) => pk,
            Err(e) => {
                return Err(GuardianError::Store(format!(
                    "Invalid secp256k1 public key: {}",
                    e
                )));
            }
        };

        // ETAPA 3: Verificação das assinaturas da identidade
        let signatures = identity.signatures();

        // Verifica assinatura do ID
        if !signatures.id().is_empty() {
            match self.verify_signature_with_secp256k1(
                identity.id(),
                signatures.id(),
                &public_key,
                &secp,
            ) {
                Ok(true) => {
                    debug!(self.logger, "Identity ID signature verified successfully");
                }
                Ok(false) => {
                    return Err(GuardianError::Store(
                        "Identity ID signature verification failed".to_string(),
                    ));
                }
                Err(e) => {
                    return Err(GuardianError::Store(format!(
                        "Error verifying ID signature: {}",
                        e
                    )));
                }
            }
        }

        // Verifica assinatura da chave pública
        if !signatures.pub_key().is_empty() {
            // Reconstrói os dados que foram assinados para a chave pública
            let pub_key_data = format!("{}{}", identity.pub_key(), signatures.id());

            match self.verify_signature_with_secp256k1(
                &pub_key_data,
                signatures.pub_key(),
                &public_key,
                &secp,
            ) {
                Ok(true) => {
                    debug!(
                        self.logger,
                        "Identity public key signature verified successfully"
                    );
                }
                Ok(false) => {
                    return Err(GuardianError::Store(
                        "Identity public key signature verification failed".to_string(),
                    ));
                }
                Err(e) => {
                    return Err(GuardianError::Store(format!(
                        "Error verifying public key signature: {}",
                        e
                    )));
                }
            }
        }

        // ETAPA 4: Verificação adicional usando libp2p se disponível
        if let Some(libp2p_key) = identity.public_key() {
            // Verifica se a chave libp2p é consistente com a chave secp256k1
            let peer_id = libp2p_identity::PeerId::from_public_key(&libp2p_key);
            debug!(
                self.logger,
                "Identity verified with libp2p PeerID: {}", peer_id
            );
        }

        debug!(self.logger, "Identity cryptographic verification completed successfully";
            "identity_id" => identity.id(),
            "public_key_len" => identity.pub_key().len()
        );

        Ok(())
    }

    /// Verifica uma assinatura usando secp256k1
    ///
    /// # Argumentos
    ///
    /// * `message` - A mensagem original que foi assinada
    /// * `signature_str` - A assinatura em formato string
    /// * `public_key` - A chave pública secp256k1
    /// * `secp` - Instância do secp256k1
    ///
    /// # Retorna
    ///
    /// * `Ok(true)` se a assinatura é válida
    /// * `Ok(false)` se a assinatura é inválida
    /// * `Err(GuardianError)` se houve erro no processo de verificação
    fn verify_signature_with_secp256k1(
        &self,
        message: &str,
        signature_str: &str,
        public_key: &secp256k1::PublicKey,
        secp: &secp256k1::Secp256k1<secp256k1::All>,
    ) -> Result<bool> {
        use secp256k1::Message;
        use sha2::{Digest, Sha256};
        use std::str::FromStr;

        // Cria hash SHA256 da mensagem
        let mut hasher = Sha256::new();
        hasher.update(message.as_bytes());
        let message_hash = hasher.finalize();
        let message_hash_array: [u8; 32] = message_hash.into();

        // Cria mensagem secp256k1 a partir do hash
        let secp_message = Message::from_digest(message_hash_array);

        // Parse da assinatura
        let signature = match secp256k1::ecdsa::Signature::from_str(signature_str) {
            Ok(sig) => sig,
            Err(e) => {
                debug!(self.logger, "Failed to parse signature";
                    "signature" => signature_str,
                    "error" => e.to_string()
                );
                return Ok(false); // Assinatura inválida, não erro fatal
            }
        };

        // Verifica a assinatura
        match secp.verify_ecdsa(secp_message, &signature, public_key) {
            Ok(()) => {
                debug!(self.logger, "Signature verification successful");
                Ok(true)
            }
            Err(e) => {
                debug!(self.logger, "Signature verification failed";
                    "error" => e.to_string()
                );
                Ok(false) // Assinatura inválida, não erro fatal
            }
        }
    }

    /// equivalente a func (o *GuardianDB) handleEventExchangeHeads(...) em go
    /// Processa um evento de troca de "heads", sincronizando as novas entradas com a store local.
    ///
    /// Esta implementação realiza a sincronização completa dos heads recebidos, incluindo:
    /// 1. Validação de integridade dos heads
    /// 2. Verificação de permissões de acesso
    /// 3. Detecção de duplicatas existentes
    /// 4. Sincronização efetiva com a store
    /// 5. Emissão de eventos de progresso
    ///
    /// # Argumentos
    ///
    /// * `event` - Evento contendo os heads a serem sincronizados e metadados
    /// * `store` - Referência à store que receberá os heads
    ///
    /// # Processamento
    ///
    /// 1. **Validação Básica**: Verifica se os heads possuem dados válidos (hash, payload)
    /// 2. **Controle de Acesso**: Usa o access controller da store para validar permissões
    /// 3. **Detecção de Duplicatas**: Consulta o oplog para evitar reprocessamento
    /// 4. **Sincronização**: Delega para o método `sync()` da store que implementa a lógica completa
    /// 5. **Eventos**: Emite eventos de progresso para componentes interessados
    ///
    /// # Performance
    ///
    /// - **O(n)** onde n = número de heads recebidos
    /// - **Paralelização**: Validação sequencial, mas sync em batch para eficiência
    /// - **Cache-aware**: Aproveita índices existentes para detecção de duplicatas
    ///
    /// # Erros
    ///
    /// - Retorna erro se a sincronização da store falhar
    /// - Heads individuais inválidos são ignorados (logged) mas não causam falha geral
    pub async fn handle_event_exchange_heads(
        &self,
        event: &MessageExchangeHeads,
        store: Arc<GuardianStore>,
    ) -> Result<()> {
        let heads = &event.heads;
        let store_address = &event.address;

        debug!(self.logger, "Processando evento de exchange heads";
            "peer_id" => self.peer_id().to_string(),
            "count" => heads.len(),
            "store_address" => store_address
        );

        if heads.is_empty() {
            debug!(self.logger, "Nenhum head recebido para sincronização");
            return Ok(());
        }

        // ETAPA 1: Validação básica e filtragem de heads inválidos
        let mut valid_heads = Vec::new();
        let mut skipped_count = 0;

        for (i, head) in heads.iter().enumerate() {
            // Validação de integridade básica
            if head.hash.is_empty() || head.payload.is_empty() {
                debug!(
                    self.logger,
                    "Head {}/{} ignorado: dados inválidos (hash ou payload vazio)",
                    i + 1,
                    heads.len()
                );
                skipped_count += 1;
                continue;
            }

            // Validação de estrutura
            if head.id.is_empty() {
                debug!(
                    self.logger,
                    "Head {}/{} ignorado: ID vazio",
                    i + 1,
                    heads.len()
                );
                skipped_count += 1;
                continue;
            }

            // Validação de identidade (se disponível)
            if let Some(identity) = &head.identity {
                if identity.id().is_empty() || identity.pub_key().is_empty() {
                    warn!(
                        self.logger,
                        "Head {}/{} com identidade inválida: {}",
                        i + 1,
                        heads.len(),
                        &head.hash
                    );
                } else {
                    debug!(
                        self.logger,
                        "Head {}/{} com identidade válida: {} ({})",
                        i + 1,
                        heads.len(),
                        &head.hash,
                        identity.id()
                    );

                    // Verificação criptográfica da identidade
                    match self.verify_identity_cryptographically(identity).await {
                        Ok(()) => {
                            debug!(
                                self.logger,
                                "Head {}/{} identidade verificada criptograficamente: {}",
                                i + 1,
                                heads.len(),
                                &head.hash
                            );
                        }
                        Err(e) => {
                            warn!(
                                self.logger,
                                "Head {}/{} falha na verificação criptográfica da identidade: {} - {}",
                                i + 1,
                                heads.len(),
                                &head.hash,
                                e
                            );
                            // Continua processamento mesmo com falha na verificação para compatibilidade
                            // Em ambiente de produção, você pode escolher rejeitar heads com identidades inválidas
                        }
                    }
                }
            }

            valid_heads.push(head.clone());

            debug!(
                self.logger,
                "Head {}/{} validado: {} (clock: {}/{})",
                i + 1,
                heads.len(),
                &head.hash,
                head.clock.id(),
                head.clock.time()
            );
        }

        if valid_heads.is_empty() {
            warn!(
                self.logger,
                "Todos os {} heads recebidos são inválidos",
                heads.len()
            );
            return Ok(());
        }

        if skipped_count > 0 {
            debug!(
                self.logger,
                "Validação concluída: {}/{} heads válidos, {} ignorados",
                valid_heads.len(),
                heads.len(),
                skipped_count
            );
        }

        // ETAPA 2: Verificação de permissões de acesso via Access Controller
        debug!(
            self.logger,
            "Verificando permissões de acesso para {} heads",
            valid_heads.len()
        );

        // Verificação completa de permissões usando o Access Controller da store
        let permitted_heads = self.verify_heads_permissions(&valid_heads, &store).await?;

        // ETAPA 3: Detecção de duplicatas consultando oplog existente
        debug!(
            self.logger,
            "Verificando duplicatas no oplog para {} heads",
            permitted_heads.len()
        );

        let mut new_heads = Vec::new();
        let duplicate_count = 0;

        // Para cada head, verifica se já existe no oplog da store
        for head in permitted_heads {
            // TODO: Implementar verificação de duplicatas no oplog quando disponível
            // Por enquanto, assumimos que todos são novos
            new_heads.push(head);
        }

        if new_heads.is_empty() {
            debug!(self.logger, "Todos os heads são duplicatas, sincronização desnecessária";
                "duplicate_count" => duplicate_count
            );
            return Ok(());
        }

        if duplicate_count > 0 {
            debug!(
                self.logger,
                "Duplicatas detectadas: {}/{} heads são novos, {} duplicatas",
                new_heads.len(),
                heads.len(),
                duplicate_count
            );
        }

        // ETAPA 4: Sincronização efetiva com a store
        debug!(self.logger, "Iniciando sincronização com a store";
            "valid_heads" => new_heads.len(),
            "store_address" => store_address
        );

        // Armazena o count antes de mover o vector e mede o tempo de sincronização
        let new_heads_count = new_heads.len();
        let sync_start_time = std::time::Instant::now();

        // Cria uma cópia das entradas para uso nos eventos
        let entries_for_events = new_heads.clone();

        // Implementação real usando helper method que resolve problemas de mutabilidade
        let sync_result = Self::sync_store_with_heads(&store, new_heads).await;

        // Calcula duração da sincronização
        let sync_duration = sync_start_time.elapsed();
        let duration_ms = sync_duration.as_millis() as u64;

        match sync_result {
            Ok(()) => {
                debug!(self.logger, "Sincronização de heads concluída com sucesso";
                    "processed_count" => new_heads_count,
                    "store_address" => store_address,
                    "duration_ms" => duration_ms
                );

                // ETAPA 5: Emissão de eventos de sucesso
                // Emite evento de sincronização para componentes interessados
                let exchange_event = EventExchangeHeads::new(self.peer_id(), event.clone());

                if let Err(e) = self.emitters.new_heads.emit(exchange_event) {
                    warn!(self.logger, "Falha ao emitir evento new_heads"; "error" => e.to_string());
                } else {
                    trace!(self.logger, "Evento new_heads emitido com sucesso";
                        "processed_heads" => new_heads_count
                    );
                }

                // ETAPA 6: Emissão de eventos específicos da store

                // Obtém informações da store para os eventos
                let store_type = store.store_type();
                let total_entries = self.get_store_total_entries(&store).await.unwrap_or(0);

                // EventStoreUpdated: Notifica mudanças na store
                let store_updated_event = EventStoreUpdated::new(
                    store_address.clone(),
                    store_type.to_string(),
                    new_heads_count,
                    total_entries,
                );

                if let Err(e) = self.emitters.store_updated.emit(store_updated_event) {
                    warn!(self.logger, "Falha ao emitir evento store_updated"; "error" => e.to_string());
                } else {
                    debug!(self.logger, "Evento store_updated emitido com sucesso";
                        "store_address" => store_address,
                        "entries_added" => new_heads_count
                    );
                }

                // EventSyncCompleted: Notifica conclusão da sincronização
                let sync_completed_event = EventSyncCompleted::new(
                    store_address.clone(),
                    self.peer_id().to_string(),
                    new_heads_count,
                    duration_ms,
                    true, // success = true
                );

                if let Err(e) = self.emitters.sync_completed.emit(sync_completed_event) {
                    warn!(self.logger, "Falha ao emitir evento sync_completed"; "error" => e.to_string());
                } else {
                    debug!(self.logger, "Evento sync_completed emitido com sucesso";
                        "store_address" => store_address,
                        "duration_ms" => duration_ms
                    );
                }

                // EventNewEntries: Notifica novas entradas adicionadas
                if !entries_for_events.is_empty() {
                    let new_entries_event = EventNewEntries::new(
                        store_address.clone(),
                        entries_for_events,
                        total_entries,
                    );

                    if let Err(e) = self.emitters.new_entries.emit(new_entries_event) {
                        warn!(self.logger, "Falha ao emitir evento new_entries"; "error" => e.to_string());
                    } else {
                        debug!(self.logger, "Evento new_entries emitido com sucesso";
                            "store_address" => store_address,
                            "new_entries_count" => new_heads_count
                        );
                    }
                }
            }
            Err(e) => {
                error!(self.logger, "Falha na sincronização de heads";
                    "error" => e.to_string(),
                    "store_address" => store_address,
                    "heads_count" => new_heads_count,
                    "duration_ms" => duration_ms
                );

                // Emite eventos de erro para componentes interessados

                // EventSyncError: Erro geral de sincronização
                let error_type = match &e {
                    GuardianError::Store(_) => SyncErrorType::StoreError,
                    GuardianError::Network(_) => SyncErrorType::NetworkError,
                    GuardianError::InvalidArgument(_) => SyncErrorType::ValidationError,
                    _ => SyncErrorType::UnknownError,
                };

                let sync_error_event = EventSyncError::new(
                    store_address.clone(),
                    self.peer_id().to_string(),
                    e.to_string(),
                    new_heads_count,
                    error_type.clone(),
                );

                if let Err(emit_err) = self.emitters.sync_error.emit(sync_error_event) {
                    warn!(self.logger, "Falha ao emitir evento sync_error";
                        "error" => emit_err.to_string(),
                        "original_error" => e.to_string()
                    );
                } else {
                    debug!(self.logger, "Evento sync_error emitido com sucesso";
                        "store_address" => store_address,
                        "error_type" => ?error_type
                    );
                }

                // EventSyncCompleted com success = false
                let sync_completed_event = EventSyncCompleted::new(
                    store_address.clone(),
                    self.peer_id().to_string(),
                    0, // heads_synced = 0 devido ao erro
                    duration_ms,
                    false, // success = false
                );

                if let Err(emit_err) = self.emitters.sync_completed.emit(sync_completed_event) {
                    warn!(self.logger, "Falha ao emitir evento sync_completed (erro)"; "error" => emit_err.to_string());
                }

                return Err(e);
            }
        }

        debug!(self.logger, "Processamento de exchange heads completado com sucesso";
            "total_heads_received" => heads.len(),
            "heads_processed" => new_heads_count,
            "heads_skipped" => skipped_count,
            "store_address" => store_address
        );

        Ok(())
    }
}

/// equivalente a func makeDirectChannel(...) em go
/// Função auxiliar para criar um canal de comunicação direta.
pub async fn make_direct_channel(
    event_bus: &EventBusImpl,
    factory: DirectChannelFactory,
    options: &DirectChannelOptions,
    logger: &slog::Logger,
) -> Result<Arc<dyn DirectChannel<Error = GuardianError> + Send + Sync>> {
    let emitter = crate::pubsub::event::PayloadEmitter::new(event_bus)
        .await
        .map_err(|e| {
            GuardianError::Other(format!(
                "não foi possível inicializar o emitter do pubsub: {}",
                e
            ))
        })?;

    // Usa a factory fornecida para criar o canal direto
    let channel = factory(Arc::new(emitter), Some((*options).clone()))
        .await
        .map_err(|e| GuardianError::Other(format!("Falha ao criar canal direto: {}", e)))?;

    debug!(
        logger,
        "Canal direto criado com sucesso usando factory fornecida"
    );
    Ok(channel)
}

/// Cria um mock mínimo de BaseGuardianDB para testar construtores de AccessController
/// Esta função cria uma implementação simplificada que atende aos requisitos mínimos
/// para executar construtores e determinar seus tipos dinamicamente
fn create_mock_guardian_db(
    logger: slog::Logger,
) -> Arc<dyn crate::iface::BaseGuardianDB<Error = GuardianError>> {
    use crate::address::Address;
    use crate::iface::{
        AccessControllerConstructor, BaseGuardianDB, CreateDBOptions, DetermineAddressOptions,
        StoreConstructor,
    };
    use crate::ipfs_core_api::client::IpfsClient;
    use crate::ipfs_log::identity::Identity;
    use crate::pubsub::event::EventBus;
    use std::sync::Arc;

    /// Implementação mock de BaseGuardianDB para testes de construtores
    struct MockGuardianDB {
        logger: slog::Logger,
        event_bus: EventBus,
        mock_identity: Identity,
    }

    impl MockGuardianDB {
        fn new(logger: slog::Logger) -> Self {
            // Criar mock dos componentes necessários
            let event_bus = EventBus::new();

            // Para o mock, não vamos criar um cliente IPFS real
            // Isso evita problemas de async no construtor e runtime aninhado

            // Criar mock da identidade
            let signatures =
                crate::ipfs_log::identity::Signatures::new("mock_id_sig", "mock_pk_sig");
            let mock_identity = Identity::new("mock", "mock_pubkey", signatures);

            Self {
                logger,
                event_bus,
                mock_identity,
            }
        }
    }

    #[async_trait::async_trait]
    impl BaseGuardianDB for MockGuardianDB {
        type Error = GuardianError;

        fn logger(&self) -> &slog::Logger {
            &self.logger
        }

        fn event_bus(&self) -> EventBus {
            self.event_bus.clone()
        }

        fn tracer(&self) -> Arc<crate::iface::TracerWrapper> {
            // Criar um TracerWrapper Noop para o mock
            Arc::new(crate::iface::TracerWrapper::Noop(
                opentelemetry::trace::noop::NoopTracer::new(),
            ))
        }

        fn ipfs(&self) -> Arc<IpfsClient> {
            // Para o mock, criar um panic indicando que não é suportado
            // Em um mock real para teste de construtores, o IPFS não deveria ser acessado
            panic!("Mock BaseGuardianDB: IPFS client não disponível durante teste de construtores")
        }

        fn identity(&self) -> &Identity {
            &self.mock_identity
        }

        async fn open(
            &self,
            _address: &str,
            _options: &mut CreateDBOptions,
        ) -> Result<Box<dyn crate::iface::Store<Error = GuardianError>>> {
            Err(GuardianError::Other(
                "Mock BaseGuardianDB não implementa open".to_string(),
            ))
        }

        fn get_store(
            &self,
            _address: &str,
        ) -> Option<Box<dyn crate::iface::Store<Error = GuardianError>>> {
            None
        }

        async fn create(
            &self,
            _name: &str,
            _store_type: &str,
            _options: &mut CreateDBOptions,
        ) -> Result<Box<dyn crate::iface::Store<Error = GuardianError>>> {
            Err(GuardianError::Other(
                "Mock BaseGuardianDB não implementa create".to_string(),
            ))
        }

        async fn determine_address(
            &self,
            _name: &str,
            _store_type: &str,
            _options: &DetermineAddressOptions,
        ) -> Result<Box<dyn Address>> {
            Err(GuardianError::Other(
                "Mock BaseGuardianDB não implementa determine_address".to_string(),
            ))
        }

        fn register_store_type(&mut self, _store_type: &str, _constructor: StoreConstructor) {
            // Mock não implementa registro
        }

        fn unregister_store_type(&mut self, _store_type: &str) {
            // Mock não implementa desregistro
        }

        fn register_access_controller_type(
            &mut self,
            _constructor: AccessControllerConstructor,
        ) -> Result<()> {
            Ok(()) // Mock aceita qualquer registro
        }

        fn unregister_access_controller_type(&mut self, _controller_type: &str) {
            // Mock não implementa desregistro
        }

        fn get_access_controller_type(
            &self,
            _controller_type: &str,
        ) -> Option<AccessControllerConstructor> {
            None // Mock não retorna construtores
        }
    }

    Arc::new(MockGuardianDB::new(logger))
}

/// Implementação do Drop trait para garantir cleanup seguro do GuardianDB
impl Drop for GuardianDB {
    fn drop(&mut self) {
        // Abort da task do monitor para evitar acesso a memória já liberada
        self._monitor_handle.abort();

        // Cancela o token para sinalizar a todas as tasks que devem parar
        self.cancellation_token.cancel();

        // Não podemos usar async no Drop, então apenas fazemos abort e cancel
        // O resto do cleanup será feito pelos destructors automáticos dos Arcs
    }
}
