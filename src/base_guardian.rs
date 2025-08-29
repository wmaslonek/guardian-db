use crate::address::{Address, GuardianDBAddress};
use crate::eqlabs_ipfs_log::identity::Identity;
use crate::error::{GuardianError, Result};
use crate::iface::{
    AccessControllerConstructor, CreateDBOptions, DetermineAddressOptions, DirectChannel,
    DirectChannelFactory, DirectChannelOptions, EventPubSubPayload, MessageExchangeHeads,
    MessageMarshaler, PubSubInterface, Store, StoreConstructor,
};
use crate::ipfs_core_api::client::IpfsClient; // Our IpfsClient alias
use ipfs_api_backend_hyper::IpfsClient as HyperIpfsClient;
use libp2p::PeerId;
use opentelemetry::global::BoxedTracer;
use parking_lot::RwLock;
use slog::{Discard, Logger, debug, error, o, warn};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::cache::level_down::LevelDownCache;
use crate::db_manifest;
use crate::pubsub::event::Emitter;

pub use crate::eqlabs_ipfs_log::identity_provider::Keystore;
use crate::keystore::SledKeystore;
pub use crate::pubsub::event::EventBus as EventBusImpl;

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
    pub direct_channel_factory: Option<Box<DirectChannelFactory>>,
    pub pubsub: Option<Box<dyn PubSubInterface<Error = GuardianError>>>,
    pub message_marshaler: Option<Box<dyn MessageMarshaler<Error = GuardianError>>>,
    pub event_bus: Option<Arc<EventBusImpl>>,
}

pub struct GuardianDB {
    ipfs: IpfsClient, // Our KuboCoreApiClient
    identity: Arc<RwLock<Identity>>,
    id: Arc<RwLock<PeerId>>,
    keystore: Arc<RwLock<Option<Box<dyn Keystore + Send + Sync>>>>,
    close_keystore: Arc<RwLock<Option<Box<dyn Fn() -> Result<()> + Send + Sync>>>>,
    logger: Logger,
    tracer: Arc<BoxedTracer>,
    stores: Arc<RwLock<HashMap<String, Arc<GuardianStore>>>>,
    direct_channel: Arc<dyn DirectChannel<Error = GuardianError> + Send + Sync>,
    access_controller_types: Arc<RwLock<HashMap<String, AccessControllerConstructor>>>,
    store_types: Arc<RwLock<HashMap<String, StoreConstructor>>>,
    directory: PathBuf,
    cache: Arc<RwLock<Arc<LevelDownCache>>>,
    pubsub: Option<Box<dyn PubSubInterface<Error = GuardianError>>>,
    event_bus: Arc<EventBusImpl>,
    message_marshaler: Box<dyn MessageMarshaler<Error = GuardianError>>,
    _monitor_handle: JoinHandle<()>, // Handle para a task em background, para que possa ser cancelada no Drop.
    cancellation_token: CancellationToken,
    this: Arc<Self>, // Arc<Self> para uso em closures
    emitters: Emitters,
}

/// Equivalente à struct EventExchangeHeads em Go.
#[derive(Clone)]
pub struct EventExchangeHeads {
    pub peer: PeerId,
    pub message: MessageExchangeHeads, // Em Rust, é mais idiomático conter o valor diretamente.
                                       // Se a semântica de ponteiro for necessária, `Box<MessageExchangeHeads>` seria uma opção.
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

// GuardianDB Emitters using our EventBus
pub struct Emitters {
    pub ready: Emitter<EventGuardianDBReady>,
    pub peer_connected: Emitter<EventPeerConnected>,
    pub peer_disconnected: Emitter<EventPeerDisconnected>,
    pub new_heads: Emitter<EventExchangeHeads>,
    pub database_created: Emitter<EventDatabaseCreated>,
    pub database_dropped: Emitter<EventDatabaseDropped>,
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

impl GuardianDB {
    /// equivalente a func NewGuardianDB(ctx context.Context, ipfs coreiface.CoreAPI, options *NewGuardianDBOptions) (BaseGuardianDB, error) em go
    /// Construtor de alto nível que configura o Keystore e a Identidade antes de chamar o construtor principal `new_orbit_db`.
    pub async fn new(
        _ipfs: HyperIpfsClient,
        options: Option<NewGuardianDBOptions>,
    ) -> Result<Self> {
        let mut options = options.unwrap_or_default();

        // Usar uma implementação temporária para obter peer_id até que a API do IPFS seja clarificada
        let peer_id = format!("temp_peer_id_{}", std::process::id());

        // Se o diretório não for fornecido, usa um padrão.
        let default_dir = PathBuf::from("./GuardianDB/default");
        let directory = options.directory.as_ref().unwrap_or(&default_dir);

        // Configura o Keystore se nenhum for fornecido.
        // Usa o banco de dados `sled` como substituto do `leveldb`.
        if options.keystore.is_none() {
            // No Go, um diretório vazio significava "in-memory". Em `sled`, `None` para o path faz isso.
            let sled_path = if directory.to_str() == Some("./GuardianDB/in-memory") {
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
                tokio::runtime::Handle::current().block_on(async { keystore_clone.close().await })
            }));

            // Define o keystore nas opções
            options.keystore = Some(Box::new(keystore));
        }

        // Configura a identidade se nenhuma for fornecida.
        let identity = if let Some(identity) = options.identity {
            identity
        } else {
            let _id = options
                .id
                .as_deref()
                .unwrap_or(&peer_id.to_string())
                .to_string();
            let _keystore = options.keystore.as_ref().ok_or_else(|| {
                GuardianError::Other("Keystore é necessário para criar uma identidade".to_string())
            })?;

            // Criar uma identidade temporária - TODO: implementar corretamente
            // Por enquanto, usar uma identidade placeholder em vez de panic
            return Err(GuardianError::Other(
                "Implementação de criação de identidade ainda não disponível. Forneça uma identidade nas opções.".to_string()
            ));
        };

        options.identity = Some(identity);

        // Chama o construtor principal com as opções totalmente configuradas.
        // Convert HyperIpfsClient to our IpfsClient (KuboCoreApiClient)
        let kubo_client = IpfsClient::default().await?; // TODO: Extract configuration from HyperIpfsClient
        Self::new_orbit_db(kubo_client, options.identity.take().unwrap(), Some(options)).await
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
            // Usar um placeholder simples para o tracer
            Arc::new(BoxedTracer::new(Box::new(
                opentelemetry::trace::noop::NoopTracer::new(),
            )))
        });
        // TODO: Implementar a lógica real para EventBus e DirectChannelFactory
        let event_bus = Arc::new(EventBusImpl::new());

        // Skip DirectChannelFactory for now since it has complex type requirements
        // let _direct_channel_factory = options.direct_channel_factory;
        let cancellation_token = CancellationToken::new();

        // Criar emitters usando nosso EventBus (agora async)
        let emitters = Emitters::generate_emitters(&event_bus).await.map_err(|e| {
            GuardianError::Other(format!("Falha ao gerar emitters do EventBus: {}", e))
        })?;

        // 3. Inicialização de componentes
        // TODO: Implementar a chamada real para make_direct_channel
        // let direct_channel = make_direct_channel(/* &event_bus, &direct_channel_factory, ... */)?;
        // Usando a implementação melhorada do DirectChannel
        let emitter = crate::pubsub::event::PayloadEmitter::new(&event_bus)
            .await
            .map_err(|e| GuardianError::Other(format!("Erro ao criar emitter: {}", e)))?;

        // Cria um peer ID para o DirectChannel
        let own_peer_id = crate::pubsub::direct_channel::create_test_peer_id();
        let libp2p_interface = Arc::new(crate::pubsub::direct_channel::GossipsubInterface::new(
            logger.clone(),
        ));

        let direct_channel: Arc<dyn DirectChannel<Error = GuardianError> + Send + Sync> = Arc::new(
            crate::pubsub::direct_channel::create_direct_channel_with_libp2p(
                libp2p_interface,
                Arc::new(emitter),
                logger.clone(),
                own_peer_id,
            )
            .await?,
        );

        let message_marshaler = options.message_marshaler.unwrap_or_else(|| {
            // Criar um placeholder para o MessageMarshaler
            Box::new(crate::message_marshaler::GuardianJSONMarshaler::new())
        });
        let cache = options.cache.unwrap_or_else(|| {
            // TODO: Implementar LevelDownCache corretamente
            Arc::new(crate::cache::level_down::LevelDownCache::new(None))
        });
        let directory = options
            .directory
            .unwrap_or_else(|| PathBuf::from("./GuardianDB/in-memory")); // Placeholder para InMemoryDirectory

        // 4. Instanciação da struct usando Arc::new_cyclic para resolver a referência circular
        let odb = Arc::new_cyclic(|weak_self| {
            GuardianDB {
                ipfs,
                identity: Arc::new(RwLock::new(identity)),
                id: Arc::new(RwLock::new(PeerId::random())), // Temporary placeholder
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
                message_marshaler,
                cancellation_token: cancellation_token.clone(),
                this: weak_self
                    .upgrade()
                    .unwrap_or_else(|| panic!("Failed to create Arc")),
                emitters,
                // Inicia o monitor do canal direto usando a função helper
                _monitor_handle: Self::start_monitor_task(
                    event_bus.clone(),
                    cancellation_token.clone(),
                    logger.clone(),
                ),
            }
        });

        // 5. Configuração pós-inicialização
        let instance = Arc::try_unwrap(odb).unwrap_or_else(|_| panic!("Failed to unwrap Arc"));

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
    pub fn keystore(&self) -> Arc<RwLock<Option<Box<dyn Keystore + Send + Sync>>>> {
        // TODO: verificar por que o.keystore nunca é definido no código Go original
        self.keystore.clone()
    }

    /// equivalente a func (o *GuardianDB) CloseKeyStore() func() error em go
    /// Retorna a função de fechamento para o Keystore, se existir.
    pub fn close_keystore(&self) -> Option<Box<dyn Fn() -> Result<()> + Send + Sync>> {
        // TODO: verificar por que o.closeKeystore nunca é definido no código Go original
        // Retorna None em vez de tentar clonar o closure
        None
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

    /// Processa um evento de troca de "heads", sincronizando as novas entradas com a store local.
    async fn handle_event_exchange_heads_internal(
        &self,
        msg: &MessageExchangeHeads,
        _store: Arc<GuardianStore>,
    ) -> Result<()> {
        // Clona os heads para processamento
        let heads = msg.heads.clone();

        debug!(self.logger, "Processando exchange heads";
            "peer_id" => self.peer_id().to_string(),
            "count" => heads.len(),
            "store_address" => &msg.address
        );

        if !heads.is_empty() {
            // TODO: Implementar sincronização quando trait bounds estiverem corretos
            // match store.sync(heads).await {
            //     Ok(_) => {},
            //     Err(e) => return Err(anyhow::anyhow!("Erro ao sincronizar heads: {}", e.to_string())),
            // }
            debug!(self.logger, "Sincronização de heads seria chamada aqui"; "count" => heads.len());
        }

        Ok(())
    }

    /// equivalente a func (o *GuardianDB) closeAllStores() em go
    /// Itera sobre todas as stores gerenciadas e chama o método `close()` de cada uma.
    /// Clona a lista de stores para evitar manter o lock durante a chamada a `close()`,
    /// prevenindo possíveis deadlocks.
    pub async fn close_all_stores(&self) {
        let stores_to_close: Vec<Arc<GuardianStore>> =
            self.stores.read().values().cloned().collect();

        for store in stores_to_close {
            // TODO: Implementar fechamento quando trait bounds estiverem corretos
            // match store.close().await {
            //     Ok(_) => {},
            //     Err(e) => error!(self.logger, "não foi possível fechar a store"; "err" => e.to_string()),
            // }
            debug!(self.logger, "Store seria fechada aqui"; "store" => format!("{:p}", store.as_ref()));
        }
    }

    /// equivalente a func (o *GuardianDB) closeCache() em go
    /// Fecha o cache LevelDown, garantindo que todos os dados sejam persistidos
    /// e liberando os recursos associados.
    pub fn close_cache(&self) {
        // Obtém lock de escrita no cache para realizar o fechamento
        let _cache_guard = self.cache.write();

        // Por enquanto, apenas logamos que o cache seria fechado
        // TODO: Implementar método close quando LevelDownCache estiver completo
        debug!(self.logger, "Cache fechado com sucesso (placeholder)");

        // O lock é automaticamente liberado quando _cache_guard sai de escopo
    }

    /// equivalente a func (o *GuardianDB) closeDirectConnections() em go
    /// Fecha o canal de comunicação direta e registra um erro se a operação falhar.
    pub async fn close_direct_connections(&self) {
        // TODO: Implementar fechamento quando trait bounds estiverem corretos
        // match self.direct_channel.close().await {
        //     Ok(_) => {},
        //     Err(e) => error!(self.logger, "não foi possível fechar a conexão direta"; "err" => e.to_string()),
        // }
        debug!(self.logger, "Canal direto seria fechado aqui");
    }

    /// equivalente a func (o *GuardianDB) closeKeyStore() em go
    /// Executa a função de fechamento do keystore, se ela tiver sido definida.
    /// Adquire um lock de escrita para garantir que a função não seja modificada enquanto é lida e executada.
    pub fn close_key_store(&self) {
        let guard = self.close_keystore.write();
        if let Some(close_fn) = guard.as_ref() {
            if let Err(e) = close_fn() {
                error!(self.logger, "não foi possível fechar o keystore"; "err" => e.to_string());
            }
        }
    }

    /// equivalente a func (o *GuardianDB) GetAccessControllerType(controllerType string) (iface.AccessControllerConstructor, bool) em go
    /// Busca um construtor de AccessController pelo seu tipo (nome).
    /// Retorna `Some(constructor)` se encontrado, ou `None` caso contrário, o que é o padrão idiomático em Rust.
    pub fn get_access_controller_type(
        &self,
        controller_type: &str,
    ) -> Option<AccessControllerConstructor> {
        // Como não podemos clonar Box<dyn Fn>, retornamos None por enquanto
        // TODO: Implementar uma estrutura que permita compartilhamento
        if self
            .access_controller_types
            .read()
            .contains_key(controller_type)
        {
            // Por enquanto, retorna um constructor dummy
            None
        } else {
            None
        }
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
    pub async fn register_access_controller_type(
        &self,
        constructor: AccessControllerConstructor,
    ) -> Result<()> {
        // Create a dummy options struct for testing the constructor
        let _dummy_options =
            crate::access_controller::manifest::CreateAccessControllerOptions::new_empty();

        // Para obter o tipo do controller, vamos usar um approach diferente
        // Por enquanto, vamos apenas registrar usando um tipo padrão
        let controller_type = "default_controller";

        // Substituindo `ensure!` por verificação manual
        if controller_type.is_empty() {
            return Err(GuardianError::InvalidArgument(
                "o tipo do controller não pode ser uma string vazia".to_string(),
            ));
        }

        self.access_controller_types
            .write()
            .insert(controller_type.to_string(), constructor);

        Ok(())
    }

    // Implementação completa da função que antes era um placeholder.
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
    pub fn get_store_constructor(&self, store_type: &str) -> Option<StoreConstructor> {
        // Como não podemos clonar Box<dyn Fn>, retornamos None por enquanto
        // TODO: Implementar uma estrutura que permita compartilhamento
        if self.store_types.read().contains_key(store_type) {
            // Por enquanto, retorna None
            None
        } else {
            None
        }
    }

    /// equivalente a func (o *GuardianDB) Close() error em go
    /// Encerra a instância do GuardianDB, fechando todas as stores, conexões e tarefas em background.
    pub async fn close(&self) -> Result<()> {
        // Close all stores first (async operation)
        self.close_all_stores().await;

        // Close direct connections (async operation)
        self.close_direct_connections().await;

        // Close cache (synchronous operation)
        self.close_cache();

        // Close keystore (synchronous operation)
        self.close_key_store();

        // Fechar emitters usando nosso EventBus
        // Note: Nossos emitters não precisam de close explícito pois usam Tokio broadcast channels
        // que são automaticamente limpos quando o EventBus é dropado
        slog::debug!(
            self.logger,
            "Emitters serão fechados automaticamente com o EventBus"
        );

        // Sinaliza para todas as tarefas em background (como `monitor_direct_channel`) para encerrarem.
        self.cancellation_token.cancel();

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
        let _db_cache = self.load_cache(&directory_path, &db_address).await?;

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
            let mut new_options = CreateDBOptions::default();
            new_options.overwrite = Some(true);
            new_options.create = Some(true);
            new_options.store_type = Some(store_type.to_string());

            // Use Box::pin to break recursion
            return Box::pin(self.create(db_address, store_type, Some(new_options))).await;
        }

        let parsed_address = crate::address::parse(db_address)
            .map_err(|e| GuardianError::Other(format!("Erro ao fazer parse do endereço: {}", e)))?;

        let directory_path = PathBuf::from(&directory);
        let _db_cache = self.load_cache(&directory_path, &parsed_address).await?;

        if options.local_only.unwrap_or(false) && !self.have_local_data(&parsed_address).await {
            return Err(GuardianError::NotFound(format!(
                "O banco de dados não existe localmente: {}",
                db_address
            )));
        }

        // Lê o manifesto do IPFS para determinar o tipo do banco de dados
        let manifest_type = if self.have_local_data(&parsed_address).await {
            // Se temos dados locais, primeiro tenta ler do cache
            // TODO: Implementar leitura do cache local
            debug!(
                self.logger,
                "Dados encontrados localmente, lendo manifesto do IPFS"
            );
            let manifest = db_manifest::read_db_manifest(self.ipfs(), &parsed_address.get_root())
                .await
                .map_err(|e| {
                    GuardianError::Other(format!("Não foi possível ler o manifesto do IPFS: {}", e))
                })?;
            manifest.r#type
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

        // Cria opções padrão para o access controller
        let _ac_params =
            crate::access_controller::manifest::CreateAccessControllerOptions::new_empty();

        // TODO: Implementar criação do Access Controller
        // Por enquanto, usar um endereço temporário
        let ac_address_string = format!("temp_ac_{}", name);

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
        let addr_string = format!("/GuardianDB/{}/{}", manifest_hash.to_string(), name);
        crate::address::parse(&addr_string)
            .map_err(|e| GuardianError::Other(format!("Erro ao fazer parse do endereço: {}", e)))
    }

    /// equivalente a func (o *GuardianDB) loadCache(...) em go
    /// Carrega o cache para um determinado endereço de banco de dados.
    pub async fn load_cache(
        &self,
        _directory: &PathBuf,
        db_address: &GuardianDBAddress,
    ) -> Result<()> {
        // Carrega o cache usando nossa implementação LevelDownCache
        let _cache = self.cache.read();
        debug!(self.logger, "Carregando cache para endereço"; "address" => db_address.to_string());

        // A implementação do cache já foi inicializada, então apenas registramos o carregamento
        Ok(())
    }

    /// equivalente a func (o *GuardianDB) haveLocalData(...) em go
    /// Verifica se o manifesto de um banco de dados existe no cache local.
    pub async fn have_local_data(&self, db_address: &GuardianDBAddress) -> bool {
        let _cache_key = format!("{}/_manifest", db_address.to_string());

        // Usa nossa implementação de cache para verificar se os dados existem
        let _cache = self.cache.read();
        // Por enquanto, sempre retorna false até que a implementação do cache esteja completa
        false
    }

    /// equivalente a func (o *GuardianDB) addManifestToCache(...) em go
    /// Adiciona o hash do manifesto de um banco de dados ao cache local.
    pub async fn add_manifest_to_cache(
        &self,
        directory: &PathBuf,
        db_address: &GuardianDBAddress,
    ) -> Result<()> {
        let _cache_key = format!("{}/_manifest", db_address.to_string());
        let _root_hash_bytes = db_address.get_root().to_string().into_bytes();

        // Usa nossa implementação de cache para armazenar o manifesto
        let _cache = self.cache.read();
        // TODO: Implementar quando o cache estiver completo

        debug!(self.logger, "Manifesto adicionado ao cache";
            "address" => db_address.to_string(),
            "directory" => directory.to_string_lossy().as_ref()
        );

        Ok(())
    }

    /// equivalente a func (o *GuardianDB) createStore(...) em go
    /// Lida com a lógica complexa de instanciar uma nova Store, incluindo a resolução
    /// do Access Controller, carregamento de cache e configuração de todas as opções.
    ///
    /// NOTA: Esta implementação está incompleta aguardando correção dos trait bounds
    /// do StoreConstructor. Por enquanto, retorna um erro informativo.
    pub async fn create_store(
        &self,
        _store_type: &str,
        _address: &GuardianDBAddress,
        _options: CreateDBOptions,
    ) -> Result<Arc<GuardianStore>> {
        Err(GuardianError::Other(
            "Implementação da store ainda não disponível. Aguardando correção dos trait bounds do StoreConstructor.".to_string()
        ))
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
        // ... clonar self.message_marshaler, self.emitters, etc. (requer que sejam Arc<T>)

        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    // Escuta o sinal de cancelamento
                    _ = token.cancelled() => {
                        slog::debug!(logger, "monitor_direct_channel encerrando.");
                        return;
                    }
                    // Escuta por novos eventos
                    maybe_event = receiver.recv() => {
                        match maybe_event {
                            Ok(_event) => {
                                // TODO: A lógica interna do loop precisa de Arcs para acessar o estado de GuardianDB.
                                // Por exemplo:
                                // let msg: MessageExchangeHeads = match self.message_marshaler.unmarshal(event.payload) { ... };
                                // let store = match self.get_store(&msg.address) { ... };
                                // self.handle_event_exchange_heads_internal(&msg, store).await;

                                // Emitir evento usando nosso EventBus
                                // if let Ok(msg) = self.message_marshaler.unmarshal(event.payload) {
                                //     let exchange_event = EventExchangeHeads::new(event.peer, msg);
                                //     if let Err(e) = self.emitters.new_heads.emit(&exchange_event).await {
                                //         slog::error!(logger, "Erro ao emitir evento new_heads: {}", e);
                                //     }
                                // }
                            }
                            Err(_) => {
                                // O canal foi fechado, encerra a tarefa.
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(handle)
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

                                // TODO: Processar eventos quando a infraestrutura estiver completa
                                // Por ora, apenas logamos que recebemos um evento
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

    /// equivalente a func (o *GuardianDB) handleEventExchangeHeads(...) em go
    /// Processa um evento de troca de "heads", sincronizando as novas entradas com a store local.
    pub async fn handle_event_exchange_heads(
        &self,
        event: &MessageExchangeHeads,
        _store: Arc<GuardianStore>,
    ) -> Result<()> {
        // Em Rust, a cópia do slice/vetor não é necessária como no Go.
        // Podemos clonar os dados se a função `sync` precisar tomar posse deles,
        // ou passar uma referência se ela aceitar. Assumindo que `sync` toma posse:
        let heads = event.heads.clone();

        debug!(self.logger, "Heads recebidos";
            "peer_id" => self.peer_id().to_string(),
            "count" => heads.len(),
            "store_address" => &event.address
        );

        if !heads.is_empty() {
            // TODO: Implementar sincronização quando trait bounds estiverem corretos
            // match store.sync(heads).await {
            //     Ok(_) => {},
            //     Err(e) => return Err(anyhow::anyhow!("Erro ao sincronizar heads: {}", e.to_string())),
            // }
            debug!(self.logger, "Sincronização seria executada aqui"; "count" => heads.len());
        }

        Ok(())
    }
}

/// equivalente a func makeDirectChannel(...) em go
/// Função auxiliar para criar um canal de comunicação direta.
pub async fn make_direct_channel(
    event_bus: &EventBusImpl,
    _factory: Box<DirectChannelFactory>,
    _options: &DirectChannelOptions,
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

    // Cria um peer ID para o DirectChannel
    let own_peer_id = crate::pubsub::direct_channel::create_test_peer_id();
    let libp2p_interface = Arc::new(crate::pubsub::direct_channel::GossipsubInterface::new(
        logger.clone(),
    ));

    // TODO: Implementar factory.create quando a interface estiver correta
    Ok(Arc::new(
        crate::pubsub::direct_channel::create_direct_channel_with_libp2p(
            libp2p_interface,
            Arc::new(emitter),
            logger.clone(),
            own_peer_id,
        )
        .await?,
    ))
}
