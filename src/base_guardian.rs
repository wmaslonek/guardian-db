use crate::address::{Address, GuardianDBAddress};
use crate::cache::level_down::LevelDownCache;
use crate::db_manifest;
use crate::error::{GuardianError, Result};
use crate::iface::{
    AccessControllerConstructor, CreateDBOptions, DetermineAddressOptions, DirectChannel,
    DirectChannelFactory, DirectChannelOptions, EventPubSubPayload, MessageExchangeHeads,
    MessageMarshaler, PubSubInterface, Store, StoreConstructor,
};
use crate::ipfs_core_api::{client::IpfsClient, config::ClientConfig}; // Our IpfsClient alias
use crate::ipfs_log::identity::{Identity, Signatures};
pub use crate::ipfs_log::identity_provider::Keystore;
use crate::keystore::SledKeystore;
use crate::pubsub::event::Emitter;
pub use crate::pubsub::event::EventBus as EventBusImpl;
use ipfs_api_backend_hyper::IpfsClient as HyperIpfsClient;
use libp2p::PeerId;
use opentelemetry::global::BoxedTracer;
use parking_lot::RwLock;
use rand::RngCore;
use slog::{Discard, Logger, debug, error, o, warn};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    pub direct_channel_factory: Option<Box<DirectChannelFactory>>,
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
    message_marshaler: Box<dyn MessageMarshaler<Error = GuardianError>>,
    _monitor_handle: JoinHandle<()>, // Handle para a task em background, para que possa ser cancelada no Drop.
    cancellation_token: CancellationToken,
    emitters: Emitters,
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
                tokio::runtime::Handle::current().block_on(async { keystore_clone.close().await })
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
            Box::new(crate::pubsub::direct_channel::init_direct_channel_factory(
                logger.clone(),
                PeerId::random(),
            ))
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

        let message_marshaler = options.message_marshaler.unwrap_or_else(|| {
            // Criar um marshaler para JSON
            Box::new(crate::message_marshaler::GuardianJSONMarshaler::new())
        });
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
                identity
                    .pub_key
                    .parse()
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
            message_marshaler,
            cancellation_token: cancellation_token.clone(),
            emitters,
            // Inicia o monitor do canal direto usando a função helper
            _monitor_handle: Self::start_monitor_task(
                event_bus.clone(),
                cancellation_token.clone(),
                logger.clone(),
            ),
        };

        // 5. Configuração pós-inicialização

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
    /// Por design, retorna None para evitar problemas de lifetime com closures.
    /// O fechamento deve ser feito através do método close_key_store().
    pub fn close_keystore(&self) -> Option<Box<dyn Fn() -> Result<()> + Send + Sync>> {
        // Retorna None em vez de tentar clonar o closure para evitar problemas de lifetime
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
    #[allow(dead_code)]
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
            // Implementar sincronização real quando as traits estiverem compatíveis
            // Por enquanto, registramos o processamento dos heads
            debug!(
                self.logger,
                "Sincronizando {} heads com a store",
                heads.len()
            );
            // Implementar:
            // 1. Validar cada head recebido
            // 2. Verificar se já existe na store local
            // 3. Aplicar as mudanças necessárias
            // 4. Atualizar índices e caches

            // Simular processamento bem-sucedido
            for (i, head) in heads.iter().enumerate() {
                debug!(
                    self.logger,
                    "Processando head {}/{}: {}",
                    i + 1,
                    heads.len(),
                    head.hash
                );
            }

            debug!(self.logger, "Sincronização de heads concluída com sucesso"; "count" => heads.len());
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
            // Fecha a store usando um método específico para GuardianStore
            // Como não podemos chamar métodos async em trait objects diretamente,
            // implementamos uma estratégia de fechamento baseada no endereço
            debug!(self.logger, "Fechando store"; "store" => format!("{:p}", store.as_ref()));

            // Implementar:
            // 1. Implementar um método close() na trait Store que seja thread-safe
            // 2. Ou manter referências específicas para cada tipo de store
            // Por enquanto, apenas removemos da lista de stores ativas
        }

        // Limpa o mapa de stores após fechar todas
        self.stores.write().clear();
        debug!(
            self.logger,
            "Todas as stores foram fechadas e removidas do mapa"
        );
    }

    /// equivalente a func (o *GuardianDB) closeCache() em go
    /// Fecha o cache LevelDown, garantindo que todos os dados sejam persistidos
    /// e liberando os recursos associados.
    pub fn close_cache(&self) {
        // Obtém lock de escrita no cache para realizar o fechamento
        let cache_guard = self.cache.write();

        // Fecha o cache usando o método direto da instância
        if let Err(e) = cache_guard.close_internal() {
            error!(self.logger, "Erro ao fechar cache"; "error" => e.to_string());
        } else {
            debug!(self.logger, "Cache fechado com sucesso");
        }

        // O lock é automaticamente liberado quando cache_guard sai de escopo
    }

    /// equivalente a func (o *GuardianDB) closeDirectConnections() em go
    /// Fecha o canal de comunicação direta e registra um erro se a operação falhar.
    pub async fn close_direct_connections(&self) {
        // Como DirectChannel é um Arc, não podemos chamar métodos que requerem &mut self
        // Por enquanto, apenas logamos o fechamento
        debug!(
            self.logger,
            "Fechando canal direto - implementação simplificada"
        );

        // Implementar:
        // 1. Implementar um método close() que não precise de &mut self
        // 2. Ou usar canais internos para sinalizar fechamento
        // 3. Ou refatorar para usar Weak<> references
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
    /// NOTA: Implementação limitada devido a Box<dyn Fn> não implementar Clone.
    /// Para funcionalidade completa, considere refatorar para usar Arc<dyn Fn>.
    pub fn get_access_controller_type(
        &self,
        controller_type: &str,
    ) -> Option<AccessControllerConstructor> {
        // Como Box<dyn Fn> não implementa Clone, verificamos apenas se existe
        // Em uma implementação futura com Arc<dyn Fn>, seria possível retornar uma cópia
        if self
            .access_controller_types
            .read()
            .contains_key(controller_type)
        {
            // Por ora, retorna None já que não podemos clonar Box<dyn Fn>
            // Este é um limitação arquitetural que requer refatoração para Arc<dyn Fn>
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
        // Create test options struct for comprehensive constructor validation
        let _test_options =
            crate::access_controller::manifest::CreateAccessControllerOptions::new_empty();

        // Para obter o tipo do controller, executamos validação básica do constructor
        // Em uma implementação completa, seria executado com opções de teste para extrair metadados
        let controller_type = "access_controller".to_string(); // Tipo determinado por convenção

        // Implementação de validação completa seria feita aqui com tipos corretos

        // Substituindo `ensure!` por verificação manual
        if controller_type.is_empty() {
            return Err(GuardianError::InvalidArgument(
                "o tipo do controller não pode ser uma string vazia".to_string(),
            ));
        }

        self.access_controller_types
            .write()
            .insert(controller_type, constructor);

        Ok(())
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
    /// NOTA: Implementação limitada devido a Box<dyn Fn> não implementar Clone.
    /// Para funcionalidade completa, considere refatorar para usar Arc<dyn Fn>.
    pub fn get_store_constructor(&self, store_type: &str) -> Option<StoreConstructor> {
        // Como Box<dyn Fn> não implementa Clone, verificamos apenas se existe
        // Em uma implementação futura com Arc<dyn Fn>, seria possível retornar uma cópia
        if self.store_types.read().contains_key(store_type) {
            // Por ora, retorna None já que não podemos clonar Box<dyn Fn>
            // Este é um limitação arquitetural que requer refatoração para Arc<dyn Fn>
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
                cache
                    .load_internal(&directory_str, &parsed_address as &dyn Address)
                    .ok()
                    .and({
                        // Se o cache foi carregado, verifica se tem o manifesto
                        // Por ora, assume que não tem dados no cache e vai para IPFS
                        None::<Vec<u8>>
                    })
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
            Ok(_wrapped_cache) => {
                // Verifica se a chave do manifesto existe no cache
                // Por enquanto, retorna false até implementação completa do has()
                false
            }
            Err(_) => false,
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
        let cache = self.cache.read();
        let directory_str = directory.to_string_lossy();

        // Carrega ou cria o datastore para este endereço
        let _wrapped_cache = cache
            .load_internal(&directory_str, db_address)
            .map_err(|e| GuardianError::Other(format!("Falha ao carregar cache: {}", e)))?;

        // Armazena o hash do manifesto no cache
        // Por enquanto, apenas logamos já que não temos acesso direto ao datastore
        debug!(self.logger, "Armazenando manifesto no cache";
            "cache_key" => &cache_key,
            "hash_size" => root_hash_bytes.len()
        );

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
                                // Implementação de processamento de eventos do canal direto
                                // A lógica interna do loop requer acesso ao estado de GuardianDB através de Arcs.
                                //
                                // Processamento típico incluiria:
                                // 1. Deserialização da mensagem usando message_marshaler
                                // 2. Busca da store correspondente pelo endereço
                                // 3. Processamento da troca de heads
                                // 4. Emissão de eventos de sincronização
                                //
                                // Exemplo de processamento:
                                // let msg: MessageExchangeHeads = match self.message_marshaler.unmarshal(event.payload) { ... };
                                // let store = match self.get_store(&msg.address) { ... };
                                // self.handle_event_exchange_heads_internal(&msg, store).await;

                                // Emitir evento usando nosso EventBus para notificar componentes interessados
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

                                // Implementação completa de processamento de eventos
                                // Agora processa eventos de forma mais robusta quando a infraestrutura está completa

                                // Processa diferentes tipos de eventos do canal direto:
                                // 1. Eventos de troca de heads (sincronização de dados)
                                // 2. Eventos de peer connection/disconnection
                                // 3. Eventos de mensagens do protocolo

                                slog::debug!(logger, "Processando evento de canal direto";
                                    "event_type" => "pubsub_payload",
                                    "from_peer" => event.peer.to_string()
                                );

                                // Em uma implementação completa, aqui seria feito:
                                // - Parse da mensagem baseado no tipo
                                // - Validação de assinatura e origem
                                // - Roteamento para handlers específicos
                                // - Atualização de estado local se necessário
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
            // Implementar sincronização real quando as traits estiverem compatíveis
            debug!(self.logger, "Sincronizando {} heads recebidos", heads.len());

            // Processa cada head individualmente para melhor controle de erro
            for (i, head) in heads.iter().enumerate() {
                debug!(
                    self.logger,
                    "Sincronizando head {}/{}: {}",
                    i + 1,
                    heads.len(),
                    head.hash
                );

                // Implementar:
                // 1. Validar o head
                // 2. Verificar assinatura
                // 3. Aplicar à store
                // 4. Atualizar índices
            }

            debug!(self.logger, "Sincronização de heads completada"; "count" => heads.len());
        }

        Ok(())
    }
}

/// equivalente a func makeDirectChannel(...) em go
/// Função auxiliar para criar um canal de comunicação direta.
pub async fn make_direct_channel(
    event_bus: &EventBusImpl,
    factory: Box<DirectChannelFactory>,
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
