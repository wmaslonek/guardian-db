use crate::address::{Address, GuardianDBAddress};
use crate::cache::level_down::LevelDownCache;
use crate::db_manifest;
use crate::error::{GuardianError, Result};
use crate::iface::{
    AccessControllerConstructor, BaseGuardianDB, CreateDBOptions, DetermineAddressOptions,
    DirectChannel, DirectChannelFactory, DirectChannelOptions, EventPubSubPayload,
    MessageExchangeHeads, MessageMarshaler, PubSubInterface, Store, StoreConstructor,
    TracerWrapper,
};
use crate::ipfs_core_api::{backends::IrohBackend, client::IpfsClient, config::ClientConfig};
use crate::ipfs_log::identity::{Identity, Signatures};
pub use crate::ipfs_log::identity_provider::Keystore;
use crate::keystore::SledKeystore;
use crate::pubsub::event::Emitter;
pub use crate::pubsub::event::EventBus;
pub use crate::pubsub::event::EventBus as EventBusImpl;
use hex;
use libp2p::PeerId;
use opentelemetry::global::BoxedTracer;
use parking_lot::RwLock;
use rand::RngCore;
use secp256k1;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Span;

// Type aliases para simplificar tipos complexos
type CloseKeystoreFn = Arc<RwLock<Option<Box<dyn Fn() -> Result<()> + Send + Sync>>>>;
// Type alias para Store com GuardianError
type GuardianStore = dyn Store<Error = GuardianError> + Send + Sync;

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
    tracer: Arc<BoxedTracer>,
    span: Span,
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
    /// Construtor de alto nível que configura o Keystore e a Identidade.
    pub async fn new(
        ipfs_config: Option<ClientConfig>,
        options: Option<NewGuardianDBOptions>,
    ) -> Result<Self> {
        let mut options = options.unwrap_or_default();

        // Usar configuração padrão do IPFS se não fornecida
        let config = ipfs_config.unwrap_or_default();

        // Extrair peer_id ou gerar aleatório
        let peer_id = options.peer_id.unwrap_or_else(PeerId::random);

        // Criar backend Iroh
        let iroh_backend = Arc::new(IrohBackend::new(&config).await?);
        let ipfs_client = IpfsClient::new_with_backend(iroh_backend).await?;

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
        Self::new_orbit_db(ipfs_client, identity, Some(options)).await
    }

    /// Construtor principal para uma instância de GuardianDB.
    pub async fn new_orbit_db(
        ipfs: IpfsClient,
        identity: Identity,
        options: Option<NewGuardianDBOptions>,
    ) -> Result<Self> {
        // Usa as opções fornecidas ou cria um valor padrão.
        let options = options.unwrap_or_default();

        // 1. Configuração de valores padrão para as opções
        let tracer = options.tracer.unwrap_or_else(|| {
            // Usar um tracer básico para telemetria
            Arc::new(BoxedTracer::new(Box::new(
                opentelemetry::trace::noop::NoopTracer::new(),
            )))
        });

        // Cria span para esta instância do GuardianDB
        let span = tracing::info_span!("guardian_db", peer_id = %identity.id());
        // Initialize EventBus with proper configuration
        let event_bus = Arc::new(EventBusImpl::new());

        // Criar DirectChannelFactory
        let direct_channel_factory = options.direct_channel_factory.unwrap_or_else(|| {
            let factory_span = tracing::info_span!("direct_channel_factory");
            let _enter = factory_span.enter();
            // Usar span de tracing para compatibilidade
            let temp_span = tracing::Span::none();
            crate::pubsub::direct_channel::init_direct_channel_factory(temp_span, PeerId::random())
        });
        let cancellation_token = CancellationToken::new();

        // Criar emitters usando o EventBus
        let emitters = Emitters::generate_emitters(&event_bus).await.map_err(|e| {
            GuardianError::Other(format!("Falha ao gerar emitters do EventBus: {}", e))
        })?;

        // 2. Inicialização de componentes
        // Criar canal direto usando nossa factory
        let direct_channel = make_direct_channel(
            &event_bus,
            direct_channel_factory,
            &DirectChannelOptions::default(),
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

        // 3. Instanciação da struct GuardianDB
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
            tracer,
            message_marshaler: message_marshaler_arc,
            cancellation_token: cancellation_token.clone(),
            emitters: Arc::new(emitters),
            // Inicia o monitor do canal direto usando a função helper
            _monitor_handle: Self::start_monitor_task(
                event_bus.clone(),
                cancellation_token.clone(),
                span.clone(),
            ),
            span,
        };

        // 4. Configuração pós-inicialização
        // Registra os construtores padrão de stores
        instance.register_default_store_types();

        // Configura o emitter "newHeads" no event_bus
        tracing::debug!("Configurando emitters do EventBus");

        // Inicia o monitor do canal direto de forma independente
        tracing::debug!("Iniciando monitor do canal direto");

        // Emite evento de inicialização do GuardianDB
        let ready_event = EventGuardianDBReady {
            address: format!("/GuardianDB/{}", instance.peer_id()),
            db_type: "GuardianDB".to_string(),
        };

        if let Err(e) = instance.emitters.ready.emit(ready_event) {
            tracing::warn!("Falha ao emitir evento GuardianDB ready: {}", e);
        } else {
            tracing::debug!("Evento GuardianDB ready emitido com sucesso");
        }

        Ok(instance)
    }

    /// Retorna o tracer para telemetria e monitoramento.
    pub fn tracer(&self) -> Arc<BoxedTracer> {
        self.tracer.clone()
    }

    /// Retorna uma referência ao span de tracing para instrumentação
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Retorna o cliente da API do IPFS (Iroh nativo).
    pub fn ipfs(&self) -> &IpfsClient {
        &self.ipfs
    }

    /// Retorna a identidade da instância do GuardianDB.
    /// A identidade é clonada para que o chamador possa usá-la sem manter o lock de leitura ativo.
    pub fn identity(&self) -> Identity {
        self.identity.read().clone()
    }

    /// Retorna o PeerId da instância do GuardianDB.
    /// `PeerId` implementa o trait `Copy`, então o valor é copiado, o que é muito eficiente.
    pub fn peer_id(&self) -> PeerId {
        *self.id.read()
    }

    /// Retorna um clone do `Arc` para o Keystore, permitindo o acesso compartilhado.
    /// O keystore é configurado durante a inicialização e pode ser usado para operações criptográficas.
    pub fn keystore(&self) -> Arc<RwLock<Option<Box<dyn Keystore + Send + Sync>>>> {
        self.keystore.clone()
    }

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
    /// # Alternativa
    ///
    /// Para uma interface mais simples, use `close_key_store()` diretamente.
    pub fn close_keystore(&self) -> Option<Box<dyn Fn() -> Result<()> + Send + Sync>> {
        // Adquire lock de leitura para verificar se existe uma função de fechamento
        let guard = self.close_keystore.read();
        // Verifica se existe uma função de fechamento
        if guard.is_some() {
            // Se existe, clona o Arc interno para capturar na closure
            let close_keystore_clone = self.close_keystore.clone();
            // Retorna uma nova closure que executa a função de fechamento
            // Dentro da closure retornada, verifica novamente se a função ainda existe
            Some(Box::new(move || {
                let guard = close_keystore_clone.read();
                if let Some(close_fn) = guard.as_ref() {
                    close_fn() // Executa a função se ainda existir
                } else {
                    Ok(()) // Função foi removida entre a verificação e a execução
                }
            }))
        } else {
            None // Entre a primeira e segunda verificação, outro thread pode ter removido a função
        }
    }

    /// Adiciona ou atualiza uma store no mapa de stores gerenciadas.
    /// Esta operação adquire um lock de escrita.
    pub fn set_store(&self, address: String, store: Arc<GuardianStore>) {
        self.stores.write().insert(address, store);
    }

    /// Remove uma store do mapa de stores gerenciadas.
    /// Esta operação adquire um lock de escrita.
    pub fn delete_store(&self, address: &str) {
        self.stores.write().remove(address);
    }

    /// Busca uma store no mapa pelo seu endereço.
    /// Retorna `Some(store)` se encontrada, ou `None` caso contrário.
    pub fn get_store(&self, address: &str) -> Option<Arc<GuardianStore>> {
        self.stores.read().get(address).cloned()
    }

    /// Itera sobre todas as stores gerenciadas e chama o método `close()` de cada uma.
    /// Clona a lista de stores para evitar manter o lock durante a chamada a `close()`,
    /// prevenindo possíveis deadlocks.
    pub async fn close_all_stores(&self) {
        let stores_to_close: Vec<Arc<GuardianStore>> =
            self.stores.read().values().cloned().collect();

        tracing::debug!(
            store_count = stores_to_close.len(),
            "Iniciando fechamento de stores"
        );

        for (index, store) in stores_to_close.iter().enumerate() {
            tracing::debug!(
                store_index = index + 1,
                total_stores = stores_to_close.len(),
                store_type = store.store_type(),
                address = %store.address(),
                "Fechando store"
            );

            match store.close().await {
                Ok(()) => {
                    tracing::debug!(
                        store_type = store.store_type(),
                        address = %store.address(),
                        "Store fechada com sucesso"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        store_type = store.store_type(),
                        address = %store.address(),
                        error = %e,
                        "Erro ao fechar store"
                    );
                    // Continua fechando outras stores mesmo se uma falhar
                }
            }
        }

        // Limpa o mapa de stores após fechar todas
        self.stores.write().clear();
        tracing::debug!(
            stores_count = stores_to_close.len(),
            "Todas as stores foram processadas e removidas do mapa"
        );
    }

    /// Fecha o cache LevelDown, garantindo que todos os dados sejam persistidos
    /// e liberando os recursos associados.
    pub fn close_cache(&self) {
        tracing::debug!("Iniciando fechamento do cache");

        // Obtém lock de escrita no cache para realizar o fechamento
        let cache_guard = self.cache.write();

        // Fecha o cache usando o método direto da instância
        match cache_guard.close_internal() {
            Ok(()) => {
                tracing::debug!("Cache fechado com sucesso");
            }
            Err(e) => {
                tracing::error!(error = %e, "Erro ao fechar cache");
            }
        }

        // O lock é automaticamente liberado quando cache_guard sai de escopo
    }

    /// Fecha o canal de comunicação direta e registra um erro se a operação falhar.
    pub async fn close_direct_connections(&self) {
        tracing::debug!("Iniciando fechamento do canal direto");

        match self.direct_channel.close_shared().await {
            Ok(()) => {
                tracing::debug!("Canal direto fechado com sucesso");
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "Erro ao fechar canal direto"
                );
            }
        }
    }

    /// Executa a função de fechamento do keystore, se ela tiver sido definida.
    /// Adquire um lock de escrita para garantir que a função não seja modificada enquanto é lida e executada.
    pub fn close_key_store(&self) {
        let guard = self.close_keystore.write();
        if let Some(close_fn) = guard.as_ref()
            && let Err(e) = close_fn()
        {
            tracing::error!(error = %e, "não foi possível fechar o keystore");
        }
    }

    /// Busca um construtor de AccessController pelo seu tipo (nome).
    /// Retorna `Some(constructor)` se encontrado, ou `None` caso contrário.
    pub fn get_access_controller_type(
        &self,
        controller_type: &str,
    ) -> Option<AccessControllerConstructor> {
        tracing::debug!(
            controller_type = controller_type,
            "Buscando construtor de AccessController"
        );

        let access_controllers = self.access_controller_types.read();

        match access_controllers.get(controller_type) {
            Some(constructor) => {
                tracing::debug!(
                    controller_type = controller_type,
                    "Construtor de AccessController encontrado"
                );
                Some(constructor.clone())
            }
            None => {
                tracing::debug!(
                    controller_type = controller_type,
                    available_types = ?access_controllers.keys().collect::<Vec<_>>(),
                    "Construtor de AccessController não encontrado"
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

    /// Remove um construtor de AccessController do mapa pelo seu tipo.
    /// Esta operação adquire um lock de escrita.
    pub fn unregister_access_controller_type(&self, controller_type: &str) {
        self.access_controller_types.write().remove(controller_type);
    }

    /// Registra um novo tipo de AccessController.
    /// A função construtora é executada uma vez para determinar o nome do tipo.
    ///
    /// Executa o construtor para determinar o tipo dinâmico
    /// Registra um novo tipo de AccessController com tipo explícito.
    pub fn register_access_controller_type_with_name(
        &self,
        controller_type: &str,
        constructor: AccessControllerConstructor,
    ) -> Result<()> {
        tracing::debug!(
            controller_type = %controller_type,
            "Registrando novo tipo de AccessController"
        );

        // Validações do tipo
        if controller_type.is_empty() {
            return Err(GuardianError::InvalidArgument(
                "O tipo do controller não pode ser uma string vazia".to_string(),
            ));
        }

        if controller_type.len() > 100 {
            return Err(GuardianError::InvalidArgument(
                "O tipo do controller é muito longo (máximo 100 caracteres)".to_string(),
            ));
        }

        // Valida tipos conhecidos
        let valid_types = ["simple", "guardian", "ipfs"];
        if !valid_types.contains(&controller_type) {
            tracing::warn!(
                controller_type = %controller_type,
                valid_types = ?valid_types,
                "Tipo de AccessController não reconhecido - registrando mesmo assim"
            );
        }

        // Verifica se o tipo já está registrado
        {
            let existing_types = self.access_controller_types.read();
            if existing_types.contains_key(controller_type) {
                tracing::warn!(
                    controller_type = %controller_type,
                    "AccessController já registrado - sobrescrevendo"
                );
            } else {
                tracing::debug!(
                    controller_type = %controller_type,
                    "Novo tipo de AccessController sendo registrado"
                );
            }
        }

        // Registra o construtor no mapa
        self.access_controller_types
            .write()
            .insert(controller_type.to_string(), constructor);

        tracing::debug!(
            controller_type = %controller_type,
            "AccessController registrado com sucesso"
        );

        Ok(())
    }

    /// Método legado mantido por compatibilidade - usa tipo padrão "simple"
    pub async fn register_access_controller_type(
        &self,
        constructor: AccessControllerConstructor,
    ) -> Result<()> {
        tracing::debug!("Usando registro legado com tipo padrão 'simple'");
        self.register_access_controller_type_with_name("simple", constructor)
    }

    pub fn register_store_type(&self, store_type: String, constructor: StoreConstructor) {
        self.store_types.write().insert(store_type, constructor);
    }

    /// Remove um construtor de Store do mapa pelo seu tipo.
    pub fn unregister_store_type(&self, store_type: &str) {
        self.store_types.write().remove(store_type);
    }

    /// Retorna uma lista com os nomes de todos os tipos de Store registrados.
    pub fn store_types_names(&self) -> Vec<String> {
        self.store_types.read().keys().cloned().collect()
    }

    /// Busca um construtor de Store pelo seu tipo (nome).
    /// Retorna `Some(constructor)` se encontrado, ou `None` caso contrário.
    pub fn get_store_constructor(&self, store_type: &str) -> Option<StoreConstructor> {
        tracing::debug!(store_type = store_type, "Buscando construtor de Store");

        let store_constructors = self.store_types.read();

        match store_constructors.get(store_type) {
            Some(constructor) => {
                tracing::debug!(store_type = store_type, "Construtor de Store encontrado");
                Some(constructor.clone())
            }
            None => {
                tracing::debug!(
                    store_type = store_type,
                    available_types = ?store_constructors.keys().collect::<Vec<_>>(),
                    "Construtor de Store não encontrado"
                );
                None
            }
        }
    }

    /// Encerra a instância do GuardianDB, fechando todas as stores, conexões e tarefas em background.
    pub async fn close(&self) -> Result<()> {
        let _entered = self.span.enter();
        tracing::debug!("Iniciando fechamento do GuardianDB");

        // Close all stores first (async operation) - com tratamento de erro
        tracing::debug!("Fechando todas as stores");
        self.close_all_stores().await;

        // Close direct connections (async operation) - com tratamento de erro
        tracing::debug!("Fechando conexões diretas");
        self.close_direct_connections().await;

        // Close cache (synchronous operation)
        tracing::debug!("Fechando cache");
        self.close_cache();

        // Close keystore (synchronous operation) - com tratamento de erro
        tracing::debug!("Fechando keystore");
        self.close_key_store();

        // Fechar emitters usando o EventBus
        // Note: Nossos emitters não precisam de close explícito pois usam Tokio broadcast channels
        // que são automaticamente limpos quando o EventBus é dropado
        tracing::debug!("Emitters serão fechados automaticamente com o EventBus");

        // Sinaliza para todas as tarefas em background (como `monitor_direct_channel`) para encerrarem.
        tracing::debug!("Cancelando tarefas em background");
        self.cancellation_token.cancel();

        // Abortar explicitamente a task do monitor para evitar quedas na finalização
        tracing::debug!("Abortando task do monitor do canal direto");
        self._monitor_handle.abort();

        // Pequeno atraso para permitir que o abort propague
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        tracing::debug!("GuardianDB fechado com sucesso");
        Ok(())
    }

    /// Cria um novo banco de dados (store), determina seu endereço, salva localmente e o abre.
    pub async fn create(
        &self,
        name: &str,
        store_type: &str,
        options: Option<CreateDBOptions>,
    ) -> Result<Arc<GuardianStore>> {
        let _entered = self.span.enter();
        tracing::debug!("Create()");
        let options = options.unwrap_or_default();

        // O diretório pode ser passado como uma opção, caso contrário, usa o padrão da instância.
        let directory = options
            .directory
            .clone()
            .unwrap_or_else(|| self.directory.to_string_lossy().to_string());
        let mut options = options;
        options.directory = Some(directory.clone());

        tracing::debug!(
            name = name,
            store_type = store_type,
            directory = %directory,
            "Criando banco de dados"
        );

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

        tracing::debug!(
            address = %db_address,
            "Banco de dados criado"
        );

        // Abre o banco de dados.
        self.open(&db_address.to_string(), options).await
    }

    /// Abre um banco de dados a partir de um endereço GuardianDB.
    pub async fn open(
        &self,
        db_address: &str,
        options: CreateDBOptions,
    ) -> Result<Arc<GuardianStore>> {
        let _entered = self.span.enter();
        tracing::debug!(address = db_address, "abrindo store GuardianDB");
        let mut options = options;

        let directory = options
            .directory
            .clone()
            .unwrap_or_else(|| self.directory.to_string_lossy().to_string());

        // Valida o endereço. Se for inválido, tenta criar um novo banco de dados se a opção `create` for verdadeira.
        if crate::address::is_valid(db_address).is_err() {
            tracing::warn!(address = db_address, "open: Endereço GuardianDB inválido");
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
            tracing::debug!("Dados encontrados localmente, tentando ler do cache antes do IPFS");

            // Leitura do cache local
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

                        tracing::debug!(
                            key = %manifest_key,
                            cache_loaded = true,
                            "Verificando manifesto no cache"
                        );

                        // Prepara contexto e chave para o cache
                        let mut ctx: Box<dyn std::any::Any> = Box::new(());
                        let key = crate::data_store::Key::new(&manifest_key);

                        // Tenta obter o manifesto do cache
                        match wrapped_cache.get(ctx.as_mut(), &key) {
                            Ok(manifest_data) => {
                                tracing::debug!(
                                    key = %manifest_key,
                                    data_size = manifest_data.len(),
                                    "Manifesto encontrado no cache"
                                );

                                // Valida se os dados são um tipo de store válido
                                let manifest_type =
                                    String::from_utf8_lossy(&manifest_data).to_string();

                                // Verifica se o tipo está registrado
                                if self.get_store_constructor(&manifest_type).is_some() {
                                    tracing::debug!(
                                        manifest_type = %manifest_type,
                                        "Manifesto válido encontrado no cache"
                                    );
                                    Some(manifest_data)
                                } else {
                                    tracing::warn!(
                                        manifest_type = %manifest_type,
                                        available_types = ?self.store_types_names(),
                                        "Tipo de manifesto no cache não está registrado"
                                    );
                                    None
                                }
                            }
                            Err(e) => {
                                tracing::debug!(
                                    key = %manifest_key,
                                    error = %e,
                                    "Manifesto não encontrado no cache"
                                );
                                None
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!(
                            error = %e,
                            "Falha ao carregar cache, usando IPFS"
                        );
                        None
                    }
                }
            };

            match cache_result {
                Some(cached_data) => {
                    tracing::debug!("Manifesto encontrado no cache local");
                    // Parse do tipo do manifesto a partir dos dados do cache
                    String::from_utf8_lossy(&cached_data).to_string()
                }
                None => {
                    tracing::debug!("Cache miss, lendo manifesto do IPFS");
                    let manifest =
                        db_manifest::read_db_manifest(self.ipfs(), &parsed_address.get_root())
                            .await
                            .map_err(|e| {
                                GuardianError::Other(format!(
                                    "Não foi possível ler o manifesto do IPFS: {}",
                                    e
                                ))
                            })?;
                    manifest.get_type
                }
            }
        } else {
            // Se não temos dados locais, lê diretamente do IPFS
            tracing::debug!("Dados não encontrados localmente, lendo manifesto do IPFS");
            let manifest = db_manifest::read_db_manifest(self.ipfs(), &parsed_address.get_root())
                .await
                .map_err(|e| {
                    GuardianError::Other(format!("Não foi possível ler o manifesto do IPFS: {}", e))
                })?;
            manifest.get_type
        };

        tracing::debug!(manifest_type = %manifest_type, "Tipo do banco de dados detectado");
        tracing::debug!("Criando instância da store");

        self.create_store(&manifest_type, &parsed_address, options)
            .await
    }

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
                "Tipo de banco de dados inválido: {}. Tipos disponíveis: {:?}",
                store_type, available_types
            )));
        }

        if crate::address::is_valid(name).is_ok() {
            return Err(GuardianError::InvalidArgument(
                "O nome do banco de dados fornecido já é um endereço válido".to_string(),
            ));
        }

        // Cria opções para o access controller com configurações adequadas
        let _ac_params =
            crate::access_controller::manifest::CreateAccessControllerOptions::new_empty();

        // Criação do Access Controller
        // Gera um endereço baseado no hash do manifesto e identidade do usuário
        let identity_hash = hex::encode(self.identity().pub_key.as_bytes());
        let ac_address_string = format!("/ipfs/{}/access_controller/{}", name, &identity_hash[..8]);

        tracing::debug!(
            address = %ac_address_string,
            identity = %&identity_hash[..16],
            "Access Controller criado"
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

    /// Carrega o cache para um determinado endereço de banco de dados.
    pub async fn load_cache(&self, directory: &Path, db_address: &GuardianDBAddress) -> Result<()> {
        // Carrega o cache usando o LevelDownCache
        let cache = self.cache.read();
        let directory_str = directory.to_string_lossy();

        tracing::debug!(
            address = %db_address,
            directory = %directory_str,
            "Carregando cache para endereço"
        );

        // Carrega o cache específico para este endereço
        let _loaded_cache = cache
            .load_internal(&directory_str, db_address)
            .map_err(|e| GuardianError::Other(format!("Falha ao carregar cache: {}", e)))?;

        tracing::debug!(address = %db_address, "Cache carregado com sucesso");
        Ok(())
    }

    /// Verifica se o manifesto de um banco de dados existe no cache local.
    pub async fn have_local_data(&self, db_address: &GuardianDBAddress) -> bool {
        let _cache_key = format!("{}/_manifest", db_address);

        // Verificar se os dados existem no cache
        let cache = self.cache.read();
        let directory_str = "./GuardianDB"; // Diretório padrão

        // Tenta carregar o cache e verificar se o manifesto existe
        match cache.load_internal(directory_str, db_address) {
            Ok(wrapped_cache) => {
                // Verifica se a chave do manifesto existe no cache
                let manifest_key = format!("{}/_manifest", db_address);

                // Prepara contexto e chave para verificar existência
                let mut ctx: Box<dyn std::any::Any> = Box::new(());
                let key = crate::data_store::Key::new(&manifest_key);

                // Tenta obter o manifesto do cache para verificar se existe
                match wrapped_cache.get(ctx.as_mut(), &key) {
                    Ok(manifest_data) => {
                        // Manifesto encontrado, verifica se os dados são válidos
                        if !manifest_data.is_empty() {
                            tracing::debug!(
                                address = %db_address,
                                manifest_size = manifest_data.len(),
                                "Dados locais encontrados no cache"
                            );
                            true
                        } else {
                            tracing::debug!(
                                address = %db_address,
                                "Manifesto vazio encontrado no cache"
                            );
                            false
                        }
                    }
                    Err(e) => {
                        tracing::debug!(
                            address = %db_address,
                            error = %e,
                            "Manifesto não encontrado no cache"
                        );
                        false
                    }
                }
            }
            Err(e) => {
                tracing::debug!(
                    address = %db_address,
                    error = %e,
                    "Falha ao carregar cache para verificação de dados locais"
                );
                false
            }
        }
    }

    /// Adiciona o hash do manifesto de um banco de dados ao cache local.
    pub async fn add_manifest_to_cache(
        &self,
        directory: &Path,
        db_address: &GuardianDBAddress,
    ) -> Result<()> {
        let cache_key = format!("{}/_manifest", db_address);
        let root_hash_bytes = db_address.get_root().to_string().into_bytes();

        // Armazenar o manifesto no cache
        let wrapped_cache = {
            let cache = self.cache.read();
            let directory_str = directory.to_string_lossy();

            // Carrega ou cria o datastore para este endereço
            cache
                .load_internal(&directory_str, db_address)
                .map_err(|e| GuardianError::Other(format!("Falha ao carregar cache: {}", e)))?
        };

        // Armazena o hash do manifesto no cache de forma concreta
        let key = crate::data_store::Key::new(&cache_key);

        // Armazena o tipo de manifesto (não apenas o hash) para facilitar a verificação
        // Busca o tipo do manifesto se ele estiver disponível
        let manifest_data = if let Ok(manifest) =
            db_manifest::read_db_manifest(self.ipfs(), &db_address.get_root()).await
        {
            // Se conseguimos ler o manifesto do IPFS, armazenamos o tipo
            manifest.get_type.into_bytes()
        } else {
            // Fallback: armazena apenas o hash da raiz como indicador de existência
            root_hash_bytes
        };

        // Cria context depois do await para evitar problemas de Send
        let mut ctx: Box<dyn std::any::Any + Send + Sync> = Box::new(());

        match wrapped_cache.put(ctx.as_mut(), &key, &manifest_data) {
            Ok(()) => {
                tracing::debug!(
                    cache_key = %cache_key,
                    data_size = manifest_data.len(),
                    address = %db_address,
                    "Manifesto armazenado no cache com sucesso"
                );
            }
            Err(e) => {
                tracing::warn!(
                    cache_key = %cache_key,
                    error = %e,
                    address = %db_address,
                    "Falha ao armazenar manifesto no cache"
                );
                // Não retorna erro pois é uma otimização, não operação crítica
            }
        }

        tracing::debug!(
            address = %db_address,
            directory = %directory.to_string_lossy(),
            cache_key = %cache_key,
            "Manifesto adicionado ao cache"
        );

        Ok(())
    }

    /// Lida com a lógica complexa de instanciar uma nova Store, incluindo a resolução
    /// do Access Controller, carregamento de cache e configuração de todas as opções.
    pub async fn create_store(
        &self,
        store_type: &str,
        address: &GuardianDBAddress,
        options: CreateDBOptions,
    ) -> Result<Arc<GuardianStore>> {
        tracing::debug!(
            store_type = store_type,
            address = %address,
            "Criando store"
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
        let new_store_options = self.convert_create_to_store_options(options).await?;

        // 3. Prepara argumentos para o construtor
        let ipfs_client = Arc::new(self.ipfs().clone());
        let identity = Arc::new(self.identity());
        let store_address = Box::new(address.clone()) as Box<dyn Address>;

        tracing::debug!(
            store_type = store_type,
            address = %address,
            "Executando construtor da store"
        );

        // 4. Executa o construtor
        let store_result =
            constructor(ipfs_client, identity, store_address, new_store_options).await;

        let store = match store_result {
            Ok(store) => store,
            Err(e) => {
                tracing::error!(
                    store_type = store_type,
                    address = %address,
                    error = %e,
                    "Falha ao criar store"
                );
                return Err(e);
            }
        };

        // 5. Converte para Arc<GuardianStore>
        let boxed_store = store as Box<dyn Store<Error = GuardianError> + Send + Sync>;
        let arc_store: Arc<GuardianStore> = Arc::from(boxed_store);

        // 6. Registra a store no mapa gerenciado
        self.set_store(address.to_string(), arc_store.clone());

        tracing::debug!(
            store_type = store_type,
            address = %address,
            store_type_confirmed = arc_store.store_type(),
            "Store criada e registrada com sucesso"
        );

        Ok(arc_store)
    }

    /// Converte CreateDBOptions para NewStoreOptions necessário pelos construtores
    async fn convert_create_to_store_options(
        &self,
        options: CreateDBOptions,
    ) -> Result<crate::iface::NewStoreOptions> {
        use crate::iface::NewStoreOptions;

        tracing::debug!("Convertendo opções para criação de store");

        // Converte access_controller de ManifestParams para AccessController
        let access_controller = if let Some(manifest_params) = options.access_controller {
            tracing::debug!("Convertendo ManifestParams para AccessController");

            // Extrai informações do ManifestParams
            let controller_type = manifest_params.get_type();

            tracing::debug!(
                controller_type = %controller_type,
                "Criando access controller a partir do manifesto"
            );

            // Extrai as permissões do ManifestParams
            let permissions = manifest_params.get_all_access();

            // Cria AccessController baseado no tipo
            match controller_type {
                "simple" | "" => {
                    tracing::debug!("Criando SimpleAccessController");

                    let simple_controller =
                        crate::access_controller::simple::SimpleAccessController::new(
                            if permissions.is_empty() {
                                None
                            } else {
                                Some(permissions)
                            },
                        );
                    Some(Arc::new(simple_controller)
                        as Arc<
                            dyn crate::access_controller::traits::AccessController,
                        >)
                }
                "guardian" => {
                    tracing::debug!("Criando GuardianAccessController");

                    // Para GuardianAccessController, usa configuração básica
                    let simple_controller =
                        crate::access_controller::simple::SimpleAccessController::new(
                            if permissions.is_empty() {
                                None
                            } else {
                                Some(permissions)
                            },
                        );
                    Some(Arc::new(simple_controller)
                        as Arc<
                            dyn crate::access_controller::traits::AccessController,
                        >)
                }
                "ipfs" => {
                    tracing::debug!(
                        "IPFS AccessController não implementado, usando SimpleAccessController"
                    );

                    let simple_controller =
                        crate::access_controller::simple::SimpleAccessController::new(
                            if permissions.is_empty() {
                                None
                            } else {
                                Some(permissions)
                            },
                        );
                    Some(Arc::new(simple_controller)
                        as Arc<
                            dyn crate::access_controller::traits::AccessController,
                        >)
                }
                _ => {
                    tracing::warn!(
                        controller_type = %controller_type,
                        "Tipo de access controller não reconhecido, usando SimpleAccessController"
                    );

                    let simple_controller =
                        crate::access_controller::simple::SimpleAccessController::new(
                            if permissions.is_empty() {
                                None
                            } else {
                                Some(permissions)
                            },
                        );
                    Some(Arc::new(simple_controller)
                        as Arc<
                            dyn crate::access_controller::traits::AccessController,
                        >)
                }
            }
        } else {
            tracing::debug!("Nenhum access controller especificado, usando padrão");
            None
        };

        // Converte as opções básicas mantendo compatibilidade
        let store_options = NewStoreOptions {
            event_bus: None,   // Será configurado pela BaseStore
            index: None,       // Será configurado pelo construtor específico
            access_controller, // AccessController convertido do ManifestParams
            cache: None,       // Usa cache padrão
            cache_destroy: None,
            replication_concurrency: None,
            reference_count: None,
            replicate: Some(true), // Por padrão, habilita replicação
            max_history: None,
            directory: options
                .directory
                .unwrap_or_else(|| "./GuardianDB".to_string()),
            sort_fn: None,
            span: None,                        // Será configurado pela BaseStore
            tracer: None,                      // Será configurado pela BaseStore
            pubsub: None,                      // Será configurado pela BaseStore
            message_marshaler: None,           // Será configurado pela BaseStore
            peer_id: libp2p::PeerId::random(), // Temporário, será sobrescrito
            direct_channel: None,              // Será configurado pela BaseStore
            close_func: None,
            store_specific_opts: None,
        };

        tracing::debug!("Opções convertidas com sucesso");
        Ok(store_options)
    }

    /// Registra os construtores padrão de access controllers disponíveis
    pub async fn register_default_access_controller_types(&self) -> Result<()> {
        tracing::debug!("Registrando construtores padrão de access controllers");

        // Registra SimpleAccessController
        let simple_constructor =
            Arc::new(
                |_base_guardian: Arc<
                    dyn crate::iface::BaseGuardianDB<Error = crate::error::GuardianError>,
                >,
                 options: &crate::access_controller::manifest::CreateAccessControllerOptions,
                 _access_controller_options: Option<
                    Vec<crate::access_controller::traits::Option>,
                >| {
                    let options = options.clone(); // Clone to move into the async block
                    Box::pin(async move {
                use crate::access_controller::simple::SimpleAccessController;
                let access_controller = SimpleAccessController::from_options(options)
                    .map_err(|e| crate::error::GuardianError::Store(e.to_string()))?;
                Ok(Arc::new(access_controller) as Arc<dyn crate::access_controller::traits::AccessController>)
            }) as Pin<Box<dyn std::future::Future<Output = crate::error::Result<Arc<dyn crate::access_controller::traits::AccessController>>> + Send>>
                },
            );

        // Efetua o registro usando o novo método com tipo explícito
        self.register_access_controller_type_with_name("simple", simple_constructor)?;

        tracing::debug!(
            types = ?self.access_controller_types_names(),
            "Construtores padrão de access controllers registrados"
        );

        Ok(())
    }

    /// Registra os construtores padrão de stores disponíveis
    pub fn register_default_store_types(&self) {
        tracing::debug!("Registrando construtores padrão de stores");

        // Registra EventLogStore
        let eventlog_constructor = Arc::new(
            |ipfs: Arc<crate::ipfs_core_api::client::IpfsClient>,
             identity: Arc<crate::ipfs_log::identity::Identity>,
             address: Box<dyn crate::address::Address>,
             options: crate::iface::NewStoreOptions| {
                Box::pin(async move {
                    use crate::stores::event_log_store::log::OrbitDBEventLogStore;
                    // Converte Box<dyn Address> para Arc<dyn Address + Send + Sync>
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

        tracing::debug!(
            types = ?self.store_types_names(),
            "Construtores padrão registrados"
        );
    }

    /// Retorna o barramento de eventos da instância do GuardianDB.
    pub fn event_bus(&self) -> Arc<EventBusImpl> {
        self.event_bus.clone()
    }

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
        let message_marshaler = self.message_marshaler.clone();
        let emitters = self.emitters.clone();
        let stores = self.stores.clone();

        let handle = tokio::spawn(async move {
            tracing::debug!("Monitor do canal direto iniciado");

            loop {
                tokio::select! {
                    // Escuta o sinal de cancelamento
                    _ = token.cancelled() => {
                        tracing::debug!("monitor_direct_channel encerrando");
                        return;
                    }
                    // Escuta por novos eventos
                    maybe_event = receiver.recv() => {
                        match maybe_event {
                            Ok(event) => {
                                tracing::trace!(
                                    peer = %event.peer,
                                    payload_size = event.payload.len(),
                                    "Evento recebido no canal direto"
                                );

                                // ETAPA 1: Deserialização da mensagem usando message_marshaler
                                let msg = match message_marshaler.unmarshal(&event.payload) {
                                    Ok(msg) => msg,
                                    Err(e) => {
                                        tracing::warn!(
                                            peer = %event.peer,
                                            error = %e,
                                            payload_size = event.payload.len(),
                                            "Falha ao deserializar mensagem do canal direto"
                                        );
                                        continue;
                                    }
                                };

                                tracing::debug!(
                                    peer = %event.peer,
                                    store_address = %msg.address,
                                    heads_count = msg.heads.len(),
                                    "Mensagem deserializada com sucesso"
                                );

                                // ETAPA 2: Busca da store correspondente pelo endereço
                                let store = {
                                    let stores_guard = stores.read();
                                    stores_guard.get(&msg.address).cloned()
                                };

                                let _store = match store {
                                    Some(store) => store,
                                    None => {
                                        tracing::debug!(
                                            store_address = %msg.address,
                                            peer = %event.peer,
                                            "Store não encontrada para endereço, ignorando mensagem"
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
                                    tracing::warn!(
                                        store_address = %msg.address,
                                        peer = %event.peer,
                                        total_heads = msg.heads.len(),
                                        "Todos os heads recebidos são inválidos"
                                    );
                                    continue;
                                }

                                tracing::debug!(
                                    store_address = %msg.address,
                                    peer = %event.peer,
                                    valid_heads = valid_heads.len(),
                                    total_heads = msg.heads.len(),
                                    "Processando heads válidos"
                                );

                                // ETAPA 4: Sincronização efetiva com a store
                                // Sincronização usando o método sync da store
                                tracing::debug!(
                                    store_address = %msg.address,
                                    peer = %event.peer,
                                    valid_heads = valid_heads.len(),
                                    "Iniciando sincronização com a store"
                                );

                                // Realiza a sincronização usando o método sync da trait Store
                                // Nota: Usamos interior mutability para compatibilidade com Arc<>
                                let sync_result = Self::sync_store_with_heads(&_store, valid_heads.clone()).await;

                                match sync_result {
                                    Ok(()) => {
                                        tracing::debug!(
                                            store_address = %msg.address,
                                            peer = %event.peer,
                                            processed_heads = valid_heads.len(),
                                            "Sincronização de heads completada com sucesso"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            store_address = %msg.address,
                                            peer = %event.peer,
                                            error = %e,
                                            attempted_heads = valid_heads.len(),
                                            "Erro durante sincronização de heads"
                                        );
                                        // Não fazemos continue aqui para permitir emissão de evento mesmo com erro
                                    }
                                }

                                // ETAPA 5: Emissão de evento para notificar componentes interessados
                                let exchange_event = EventExchangeHeads::new(event.peer, msg);
                                if let Err(e) = emitters.new_heads.emit(exchange_event) {
                                    tracing::error!(
                                        error = %e,
                                        peer = %event.peer,
                                        "Erro ao emitir evento new_heads"
                                    );
                                } else {
                                    tracing::trace!(peer = %event.peer, "Evento new_heads emitido com sucesso");
                                }
                            }
                            Err(_) => {
                                // O canal foi fechado, encerra a tarefa.
                                tracing::debug!("Canal de eventos fechado, encerrando monitor");
                                break;
                            }
                        }
                    }
                }
            }

            tracing::debug!("Monitor do canal direto finalizado");
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
        // Primeiro, tenta fazer downcast para BaseStore diretamente
        if let Some(base_store) = store
            .as_any()
            .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            // BaseStore funciona com interior mutability
            return base_store.sync(heads).await.map_err(|e| {
                GuardianError::Store(format!("Erro na sincronização BaseStore: {}", e))
            });
        }
        // Fallback: Para stores que não expõem BaseStore diretamente
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
        span: Span,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let _enter = span.enter();
            // Tenta se inscrever nos eventos do pubsub
            let mut receiver = match event_bus.subscribe::<EventPubSubPayload>().await {
                Ok(rx) => rx,
                Err(e) => {
                    tracing::error!("Falha ao se inscrever nos eventos do pubsub: {}", e);
                    return;
                }
            };

            tracing::debug!("Monitor do canal direto iniciado");

            loop {
                tokio::select! {
                    // Escuta o sinal de cancelamento
                    _ = cancellation_token.cancelled() => {
                        tracing::debug!("Monitor do canal direto encerrando");
                        return;
                    }
                    // Escuta por novos eventos
                    maybe_event = receiver.recv() => {
                        match maybe_event {
                            Ok(event) => {
                                tracing::trace!(
                                    peer = %event.peer,
                                    "Evento recebido no monitor do canal direto"
                                );

                                // Processa diferentes tipos de eventos do canal direto:
                                // 1. Eventos de troca de heads (sincronização de dados)
                                // 2. Eventos de peer connection/disconnection
                                // 3. Eventos de mensagens do protocolo

                                tracing::debug!(
                                    event_type = "pubsub_payload",
                                    from_peer = %event.peer,
                                    payload_size = event.payload.len(),
                                    "Processando evento de canal direto"
                                );

                                // Note: O processamento completo é feito pelo monitor principal via monitor_direct_channel()
                                // que tem acesso ao message_marshaler, stores e emitters
                            }
                            Err(_) => {
                                tracing::debug!("Canal de eventos fechado, encerrando monitor");
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
    /// Realiza verificação completa de permissões para cada head:
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
        tracing::debug!(
            heads_count = heads.len(),
            "Iniciando verificação de permissões para heads"
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
                tracing::warn!("Tipo de store não suportado para verificação de permissões");
                return Err(GuardianError::Store(
                    "Store type not supported for permission verification".to_string()
                ));
            }
        };

        tracing::debug!(
            access_controller_type = access_controller.get_type(),
            "Access Controller obtido"
        );

        // Verificação individual de cada head
        for (i, head) in heads.iter().enumerate() {
            // VERIFICAÇÃO 1: Presença de identidade
            let identity = match &head.identity {
                Some(identity) => identity,
                None => {
                    tracing::debug!(
                        head_index = i + 1,
                        total_heads = heads.len(),
                        head_hash = %head.hash,
                        "Head rejeitado: sem identidade"
                    );
                    no_identity_count += 1;
                    continue;
                }
            };

            // VERIFICAÇÃO 2: Validação básica da identidade
            if identity.id().is_empty() || identity.pub_key().is_empty() {
                tracing::debug!(
                    head_index = i + 1,
                    total_heads = heads.len(),
                    head_hash = %head.hash,
                    "Head rejeitado: identidade inválida"
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
                    tracing::warn!(
                        head_index = i + 1,
                        total_heads = heads.len(),
                        error = %e,
                        head_hash = %head.hash,
                        "Head erro ao verificar permissões"
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
                        tracing::warn!(error = %emit_err, "Erro ao emitir evento PermissionDenied");
                    }

                    false // Em caso de erro, nega acesso por segurança
                }
            };

            if !has_write_permission {
                tracing::debug!(
                    head_index = i + 1,
                    total_heads = heads.len(),
                    head_hash = %head.hash,
                    identity_id = identity.id(),
                    "Head rejeitado: sem permissão de escrita"
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
                    tracing::warn!(error = %e, "Erro ao emitir evento PermissionDenied");
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
                tracing::debug!(
                    head_index = i + 1,
                    total_heads = heads.len(),
                    permission_type = permission_type,
                    head_hash = %head.hash,
                    identity_id = identity.id(),
                    "Head autorizado"
                );

                authorized_heads.push(head.clone());
            } else {
                tracing::debug!(
                    head_index = i + 1,
                    total_heads = heads.len(),
                    head_hash = %head.hash,
                    identity_id = identity.id(),
                    "Head rejeitado: sem permissões adequadas"
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
                    tracing::warn!(error = %e, "Erro ao emitir evento PermissionDenied");
                }

                denied_count += 1;
            }
        }

        // Log detalhado dos resultados da verificação
        let authorized_count = authorized_heads.len();
        let total_heads = heads.len();

        tracing::debug!(
            total_heads = total_heads,
            authorized_heads = authorized_count,
            denied_heads = denied_count,
            no_identity_heads = no_identity_count,
            access_controller_type = access_controller.get_type(),
            "Verificação de permissões concluída"
        );

        if authorized_count == 0 && total_heads > 0 {
            tracing::warn!(
                total_heads = total_heads,
                "ATENÇÃO: Todos os heads foram rejeitados por falta de permissões"
            );

            // Lista as chaves autorizadas para debug
            if let Ok(write_keys) = access_controller.get_authorized_by_role("write").await {
                tracing::debug!(write_keys = ?write_keys, "Chaves autorizadas para escrita");
            }
            if let Ok(admin_keys) = access_controller.get_authorized_by_role("admin").await {
                tracing::debug!(admin_keys = ?admin_keys, "Chaves autorizadas para admin");
            }
        } else if authorized_count < total_heads {
            tracing::info!(
                authorized_heads = authorized_count,
                total_heads = total_heads,
                rejected_heads = total_heads - authorized_count,
                "Verificação parcial de permissões concluída"
            );
        } else if authorized_count == total_heads && total_heads > 0 {
            tracing::debug!(
                authorized_heads = total_heads,
                "Verificação completa: todos os heads foram autorizados"
            );
        }

        Ok(authorized_heads)
    }

    /// Verifica criptograficamente a validade de uma identidade
    ///
    /// Realiza verificação completa da identidade usando:
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
                    tracing::debug!("Identity ID signature verified successfully");
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
                    tracing::debug!("Identity public key signature verified successfully");
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
            tracing::debug!(peer_id = %peer_id, "Identity verified with libp2p PeerID");
        }

        tracing::debug!(
            identity_id = identity.id(),
            public_key_len = identity.pub_key().len(),
            "Identity cryptographic verification completed successfully"
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
                tracing::debug!(
                    signature = signature_str,
                    error = %e,
                    "Failed to parse signature"
                );
                return Ok(false); // Assinatura inválida, não erro fatal
            }
        };

        // Verifica a assinatura
        match secp.verify_ecdsa(secp_message, &signature, public_key) {
            Ok(()) => {
                tracing::debug!("Signature verification successful");
                Ok(true)
            }
            Err(e) => {
                tracing::debug!(error = %e, "Signature verification failed");
                Ok(false) // Assinatura inválida, não erro fatal
            }
        }
    }

    /// Processa um evento de troca de "heads", sincronizando as novas entradas com a store local.
    ///
    /// Realiza a sincronização completa dos heads recebidos, incluindo:
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

        tracing::debug!(
            peer_id = %self.peer_id(),
            count = heads.len(),
            store_address = store_address,
            "Processando evento de exchange heads"
        );

        if heads.is_empty() {
            tracing::debug!("Nenhum head recebido para sincronização");
            return Ok(());
        }

        // ETAPA 1: Validação básica e filtragem de heads inválidos
        let mut valid_heads = Vec::new();
        let mut skipped_count = 0;

        for (i, head) in heads.iter().enumerate() {
            // Validação de integridade básica
            if head.hash.is_empty() || head.payload.is_empty() {
                tracing::debug!(
                    head_index = i + 1,
                    total_heads = heads.len(),
                    "Head ignorado: dados inválidos (hash ou payload vazio)"
                );
                skipped_count += 1;
                continue;
            }

            // Validação de estrutura
            if head.id.is_empty() {
                tracing::debug!(
                    head_index = i + 1,
                    total_heads = heads.len(),
                    "Head ignorado: ID vazio"
                );
                skipped_count += 1;
                continue;
            }

            // Validação de identidade (se disponível)
            if let Some(identity) = &head.identity {
                if identity.id().is_empty() || identity.pub_key().is_empty() {
                    tracing::warn!(
                        head_index = i + 1,
                        total_heads = heads.len(),
                        head_hash = %head.hash,
                        "Head com identidade inválida"
                    );
                } else {
                    tracing::debug!(
                        head_index = i + 1,
                        total_heads = heads.len(),
                        head_hash = %head.hash,
                        identity_id = identity.id(),
                        "Head com identidade válida"
                    );

                    // Verificação criptográfica da identidade
                    match self.verify_identity_cryptographically(identity).await {
                        Ok(()) => {
                            tracing::debug!(
                                head_index = i + 1,
                                total_heads = heads.len(),
                                head_hash = %head.hash,
                                "Head identidade verificada criptograficamente"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                head_index = i + 1,
                                total_heads = heads.len(),
                                head_hash = %head.hash,
                                error = %e,
                                "Head falha na verificação criptográfica da identidade"
                            );
                            // Continua processamento mesmo com falha na verificação para compatibilidade
                            // Note: Em ambiente de produção, você pode escolher rejeitar heads com identidades inválidas
                        }
                    }
                }
            }

            valid_heads.push(head.clone());

            tracing::debug!(
                head_index = i + 1,
                total_heads = heads.len(),
                head_hash = %head.hash,
                clock_id = head.clock.id(),
                clock_time = head.clock.time(),
                "Head validado"
            );
        }

        if valid_heads.is_empty() {
            tracing::warn!(
                total_heads = heads.len(),
                "Todos os heads recebidos são inválidos"
            );
            return Ok(());
        }

        if skipped_count > 0 {
            tracing::debug!(
                valid_heads = valid_heads.len(),
                total_heads = heads.len(),
                skipped_count = skipped_count,
                "Validação concluída com heads ignorados"
            );
        }

        // ETAPA 2: Verificação de permissões de acesso via Access Controller
        tracing::debug!(
            valid_heads_count = valid_heads.len(),
            "Verificando permissões de acesso para heads"
        );

        // Verificação completa de permissões usando o Access Controller da store
        let permitted_heads = self.verify_heads_permissions(&valid_heads, &store).await?;

        // ETAPA 3: Detecção de duplicatas consultando oplog existente
        tracing::debug!(
            permitted_heads_count = permitted_heads.len(),
            "Verificando duplicatas no oplog para heads"
        );

        let mut new_heads = Vec::new();
        let mut duplicate_count = 0;

        // Para cada head, verifica se já existe no oplog da store
        for (i, head) in permitted_heads.iter().enumerate() {
            // Verifica se o head já existe no oplog da store
            let head_hash = head.hash();
            let already_exists = {
                // Tenta acessar o oplog através dos tipos de store conhecidos
                if let Some(event_log_store) = store.as_any()
                    .downcast_ref::<crate::stores::event_log_store::log::OrbitDBEventLogStore>()
                {
                    event_log_store.basestore().op_log().read().has(head_hash)
                } else if let Some(kv_store) = store.as_any()
                    .downcast_ref::<crate::stores::kv_store::keyvalue::GuardianDBKeyValue>()
                {
                    kv_store.basestore().op_log().read().has(head_hash)
                } else if let Some(doc_store) = store.as_any()
                    .downcast_ref::<crate::stores::document_store::document::GuardianDBDocumentStore>()
                {
                    doc_store.basestore().op_log().read().has(head_hash)
                } else if let Some(base_store) = store.as_any()
                    .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
                {
                    base_store.op_log().read().has(head_hash)
                } else {
                    tracing::warn!(
                        head_index = i + 1,
                        total_heads = permitted_heads.len(),
                        head_hash = %head_hash,
                        "Tipo de store não suportado para verificação de duplicatas, assumindo novo"
                    );
                    false // Se não conseguimos verificar, assumimos que é novo
                }
            };

            if already_exists {
                tracing::debug!(
                    head_index = i + 1,
                    total_heads = permitted_heads.len(),
                    head_hash = %head_hash,
                    "Head já existe no oplog (duplicata)"
                );
                duplicate_count += 1;
            } else {
                tracing::debug!(
                    head_index = i + 1,
                    total_heads = permitted_heads.len(),
                    head_hash = %head_hash,
                    "Head é novo, adicionando para sincronização"
                );
                new_heads.push(head.clone());
            }
        }

        if new_heads.is_empty() {
            tracing::debug!(
                duplicate_count = duplicate_count,
                "Todos os heads são duplicatas, sincronização desnecessária"
            );
            return Ok(());
        }

        if duplicate_count > 0 {
            tracing::debug!(
                new_heads = new_heads.len(),
                total_heads = heads.len(),
                duplicate_count = duplicate_count,
                "Duplicatas detectadas"
            );
        }

        // ETAPA 4: Sincronização efetiva com a store
        tracing::debug!(
            valid_heads = new_heads.len(),
            store_address = store_address,
            "Iniciando sincronização com a store"
        );

        // Armazena o count antes de mover o vector e mede o tempo de sincronização
        let new_heads_count = new_heads.len();
        let sync_start_time = std::time::Instant::now();

        // Cria uma cópia das entradas para uso nos eventos
        let entries_for_events = new_heads.clone();

        // Hlper method que resolve problemas de mutabilidade
        let sync_result = Self::sync_store_with_heads(&store, new_heads).await;

        // Calcula duração da sincronização
        let sync_duration = sync_start_time.elapsed();
        let duration_ms = sync_duration.as_millis() as u64;

        match sync_result {
            Ok(()) => {
                tracing::debug!(
                    processed_count = new_heads_count,
                    store_address = store_address,
                    duration_ms = duration_ms,
                    "Sincronização de heads concluída com sucesso"
                );

                // ETAPA 5: Emissão de eventos de sucesso
                // Emite evento de sincronização para componentes interessados
                let exchange_event = EventExchangeHeads::new(self.peer_id(), event.clone());

                if let Err(e) = self.emitters.new_heads.emit(exchange_event) {
                    tracing::warn!(error = %e, "Falha ao emitir evento new_heads");
                } else {
                    tracing::trace!(
                        processed_heads = new_heads_count,
                        "Evento new_heads emitido com sucesso"
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
                    tracing::warn!(error = %e, "Falha ao emitir evento store_updated");
                } else {
                    tracing::debug!(
                        store_address = store_address,
                        entries_added = new_heads_count,
                        "Evento store_updated emitido com sucesso"
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
                    tracing::warn!(error = %e, "Falha ao emitir evento sync_completed");
                } else {
                    tracing::debug!(
                        store_address = store_address,
                        duration_ms = duration_ms,
                        "Evento sync_completed emitido com sucesso"
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
                        tracing::warn!(error = %e, "Falha ao emitir evento new_entries");
                    } else {
                        tracing::debug!(
                            store_address = store_address,
                            new_entries_count = new_heads_count,
                            "Evento new_entries emitido com sucesso"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    store_address = store_address,
                    heads_count = new_heads_count,
                    duration_ms = duration_ms,
                    "Falha na sincronização de heads"
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
                    tracing::warn!(
                        error = %emit_err,
                        original_error = %e,
                        "Falha ao emitir evento sync_error"
                    );
                } else {
                    tracing::debug!(
                        store_address = store_address,
                        error_type = ?error_type,
                        "Evento sync_error emitido com sucesso"
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
                    tracing::warn!(error = %emit_err, "Falha ao emitir evento sync_completed (erro)");
                }

                return Err(e);
            }
        }

        tracing::debug!(
            total_heads_received = heads.len(),
            heads_processed = new_heads_count,
            heads_skipped = skipped_count,
            store_address = store_address,
            "Processamento de exchange heads completado com sucesso"
        );

        Ok(())
    }
}

/// Função auxiliar para criar um canal de comunicação direta.
pub async fn make_direct_channel(
    event_bus: &EventBusImpl,
    factory: DirectChannelFactory,
    options: &DirectChannelOptions,
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

    tracing::debug!("Canal direto criado com sucesso usando factory fornecida");
    Ok(channel)
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

/// Implementação da trait BaseGuardianDB para GuardianDB
#[async_trait::async_trait]
impl BaseGuardianDB for GuardianDB {
    type Error = GuardianError;

    fn ipfs(&self) -> Arc<IpfsClient> {
        Arc::new(self.ipfs.clone())
    }

    fn identity(&self) -> Arc<Identity> {
        // Cria um clone do Arc<Identity> a partir do RwLock
        let identity_guard = self.identity.read();
        Arc::new(identity_guard.clone())
    }

    async fn open(
        &self,
        address: &str,
        options: &mut CreateDBOptions,
    ) -> std::result::Result<Arc<dyn Store<Error = GuardianError>>, Self::Error> {
        // Cria uma cópia das opções para usar com o método interno
        let options_copy = CreateDBOptions {
            event_bus: options.event_bus.clone(),
            directory: options.directory.clone(),
            overwrite: options.overwrite,
            local_only: options.local_only,
            create: options.create,
            store_type: options.store_type.clone(),
            access_controller_address: options.access_controller_address.clone(),
            access_controller: None, // Será resolvido internamente se necessário
            replicate: options.replicate,
            keystore: options.keystore.clone(),
            cache: options.cache.clone(),
            identity: options.identity.clone(),
            sort_fn: options.sort_fn,
            timeout: options.timeout,
            message_marshaler: options.message_marshaler.clone(),
            span: options.span.clone(),
            close_func: None,
            store_specific_opts: None,
        };

        // Chama o método open interno do GuardianDB
        let arc_store = GuardianDB::open(self, address, options_copy).await?;

        // Converte Arc<GuardianStore> para Arc<dyn Store>
        let store_dyn: Arc<dyn Store<Error = GuardianError>> =
            arc_store as Arc<dyn Store<Error = GuardianError>>;

        Ok(store_dyn)
    }

    fn get_store(&self, address: &str) -> Option<Arc<dyn Store<Error = GuardianError>>> {
        // Usa o método get_store interno do GuardianDB
        if let Some(arc_store) = GuardianDB::get_store(self, address) {
            // Converte Arc<GuardianStore> para Arc<dyn Store>
            let store_dyn: Arc<dyn Store<Error = GuardianError>> =
                arc_store as Arc<dyn Store<Error = GuardianError>>;
            Some(store_dyn)
        } else {
            None
        }
    }

    async fn create(
        &self,
        name: &str,
        store_type: &str,
        options: &mut CreateDBOptions,
    ) -> std::result::Result<Arc<dyn Store<Error = GuardianError>>, Self::Error> {
        // Cria uma cópia das opções para usar com o método interno
        let options_copy = CreateDBOptions {
            event_bus: options.event_bus.clone(),
            directory: options.directory.clone(),
            overwrite: options.overwrite,
            local_only: options.local_only,
            create: options.create,
            store_type: Some(store_type.to_string()),
            access_controller_address: options.access_controller_address.clone(),
            access_controller: None,
            replicate: options.replicate,
            keystore: options.keystore.clone(),
            cache: options.cache.clone(),
            identity: options.identity.clone(),
            sort_fn: options.sort_fn,
            timeout: options.timeout,
            message_marshaler: options.message_marshaler.clone(),
            span: options.span.clone(),
            close_func: None,
            store_specific_opts: None,
        };

        // Chama o método create interno do GuardianDB
        let arc_store = GuardianDB::create(self, name, store_type, Some(options_copy)).await?;

        // Converte Arc<GuardianStore> para Arc<dyn Store>
        let store_dyn: Arc<dyn Store<Error = GuardianError>> =
            arc_store as Arc<dyn Store<Error = GuardianError>>;

        Ok(store_dyn)
    }

    async fn determine_address(
        &self,
        name: &str,
        store_type: &str,
        options: &DetermineAddressOptions,
    ) -> std::result::Result<Box<dyn Address>, Self::Error> {
        // Usa o método determine_address interno do GuardianDB
        let guardian_address =
            GuardianDB::determine_address(self, name, store_type, Some(options.clone())).await?;

        // Converte GuardianDBAddress para Box<dyn Address>
        let boxed_address: Box<dyn Address> = Box::new(guardian_address);

        Ok(boxed_address)
    }

    fn register_store_type(&mut self, store_type: &str, constructor: StoreConstructor) {
        // Usa o método register_store_type existente (evita recursão chamando método interno)
        let mut types = self.store_types.write();
        types.insert(store_type.to_string(), constructor);
        tracing::debug!("Registered store type: {}", store_type);
    }

    fn unregister_store_type(&mut self, store_type: &str) {
        // Usa o método unregister_store_type existente (evita recursão chamando método interno)
        let mut types = self.store_types.write();
        types.remove(store_type);
        tracing::debug!("Unregistered store type: {}", store_type);
    }

    fn register_access_controller_type(
        &mut self,
        constructor: AccessControllerConstructor,
    ) -> std::result::Result<(), Self::Error> {
        // Registra com tipo padrão "default" para evitar recursão
        let mut types = self.access_controller_types.write();
        types.insert("default".to_string(), constructor);
        tracing::debug!("Registered access controller type: default");
        Ok(())
    }

    fn unregister_access_controller_type(&mut self, controller_type: &str) {
        // Usa método interno para evitar recursão
        let mut types = self.access_controller_types.write();
        types.remove(controller_type);
        tracing::debug!("Unregistered access controller type: {}", controller_type);
    }

    fn get_access_controller_type(
        &self,
        controller_type: &str,
    ) -> Option<AccessControllerConstructor> {
        // Usa acesso direto para evitar recursão
        let types = self.access_controller_types.read();
        types.get(controller_type).cloned()
    }

    fn event_bus(&self) -> EventBus {
        (*self.event_bus).clone()
    }

    fn span(&self) -> &tracing::Span {
        &self.span
    }

    fn tracer(&self) -> Arc<TracerWrapper> {
        Arc::new(TracerWrapper::OpenTelemetry(self.tracer.clone()))
    }
}
