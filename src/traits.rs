use crate::access_controller::{
    manifest::ManifestParams, traits::AccessController, traits::Option as AccessControllerOption,
};
use crate::address::Address;
use crate::data_store::Datastore;
use crate::error::GuardianError;
use crate::events::{self, EmitterInterface};
use crate::ipfs_core_api::client::IpfsClient;
use crate::ipfs_log::{entry::Entry, identity::Identity, log::Log};
use crate::p2p::events::EventBus;
use crate::stores::{
    operation::operation::Operation,
    replicator::{replication_info::ReplicationInfo, replicator::Replicator},
};
use cid::Cid;
use futures::stream::Stream;
use libp2p::core::PeerId;
use opentelemetry::global::{BoxedSpan, BoxedTracer};
use opentelemetry::trace::{Tracer, noop::NoopTracer};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::Span;

// Type aliases para reduzir complexidade de tipos
type KeyExtractorFn =
    Arc<dyn Fn(&serde_json::Value) -> Result<String, GuardianError> + Send + Sync>;
type MarshalFn = Arc<dyn Fn(&serde_json::Value) -> Result<Vec<u8>, GuardianError> + Send + Sync>;
type UnmarshalFn = Arc<dyn Fn(&[u8]) -> Result<serde_json::Value, GuardianError> + Send + Sync>;
type ItemFactoryFn = Arc<dyn Fn() -> serde_json::Value + Send + Sync>;
type CleanupCallback = Box<
    dyn FnOnce() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> + Send + Sync,
>;

// Re-export from the canonical location to avoid duplication
pub use crate::stores::replicator::traits::SortFn;

// Type aliases para melhorar legibilidade de assinaturas complexas
/// Alias para documentos dinâmicos thread-safe
pub type Document = Box<dyn Any + Send + Sync>;

/// Alias para resultado padrão com GuardianError
pub type GuardianResult<T> = std::result::Result<T, GuardianError>;

/// Alias para filtros de query assíncronos
pub type AsyncDocumentFilter = Pin<
    Box<
        dyn Fn(
                &Document,
            )
                -> Pin<Box<dyn Future<Output = Result<bool, Box<dyn Error + Send + Sync>>> + Send>>
            + Send
            + Sync,
    >,
>;

/// Alias para callback de progresso
pub type ProgressCallback = mpsc::Sender<Entry>;

/// Wrapper para diferentes tipos de tracer, integrado com o sistema tracing
///
/// Este enum permite usar tanto tracers OpenTelemetry quanto o sistema
/// tracing nativo do Rust de forma transparente.
#[derive(Default)]
pub enum TracerWrapper {
    /// Tracer OpenTelemetry para observabilidade distribuída
    OpenTelemetry(Arc<BoxedTracer>),
    /// Tracer baseado no sistema tracing nativo do Rust
    #[default]
    Tracing,
    /// Tracer noop para quando telemetria está desabilitada
    Noop(NoopTracer),
}

impl Clone for TracerWrapper {
    fn clone(&self) -> Self {
        match self {
            TracerWrapper::OpenTelemetry(tracer) => TracerWrapper::OpenTelemetry(tracer.clone()),
            TracerWrapper::Tracing => TracerWrapper::Tracing,
            TracerWrapper::Noop(_) => TracerWrapper::Noop(NoopTracer::new()),
        }
    }
}

impl TracerWrapper {
    /// Cria um novo TracerWrapper usando o sistema tracing nativo
    pub fn new_tracing() -> Self {
        TracerWrapper::Tracing
    }

    /// Cria um novo TracerWrapper usando OpenTelemetry
    pub fn new_opentelemetry(tracer: Arc<BoxedTracer>) -> Self {
        TracerWrapper::OpenTelemetry(tracer)
    }

    /// Cria um TracerWrapper noop (sem operação)
    pub fn new_noop() -> Self {
        TracerWrapper::Noop(NoopTracer::new())
    }

    /// Inicia um novo span instrumentado
    ///
    /// Este método cria spans de forma consistente independentemente
    /// do tipo de tracer sendo usado.
    pub fn start_span(&self, name: &str) -> TracerSpan {
        match self {
            TracerWrapper::OpenTelemetry(tracer) => {
                // Para OpenTelemetry, cria um span usando a trait Tracer
                let span = tracer.start(name.to_string());
                TracerSpan::OpenTelemetry(span)
            }
            TracerWrapper::Tracing => {
                // Para tracing nativo, usa a macro tracing::span!
                let span = tracing::info_span!("guardian_db", operation = name);
                TracerSpan::Tracing(span)
            }
            TracerWrapper::Noop(_) => {
                // Para noop, retorna um span vazio
                TracerSpan::Noop
            }
        }
    }

    /// Verifica se o tracer está ativo (não é noop)
    pub fn is_active(&self) -> bool {
        !matches!(self, TracerWrapper::Noop(_))
    }

    /// Retorna o tipo do tracer como string para logs/debug
    pub fn tracer_type(&self) -> &'static str {
        match self {
            TracerWrapper::OpenTelemetry(_) => "opentelemetry",
            TracerWrapper::Tracing => "tracing",
            TracerWrapper::Noop(_) => "noop",
        }
    }
}

/// Enum para representar diferentes tipos de spans instrumentados
///
/// Permite trabalhar com spans de diferentes sistemas de tracing
/// de forma unificada.
pub enum TracerSpan {
    /// Span OpenTelemetry para observabilidade distribuída
    OpenTelemetry(BoxedSpan),
    /// Span do sistema tracing nativo do Rust
    Tracing(tracing::Span),
    /// Span noop para quando telemetria está desabilitada
    Noop,
}

impl TracerSpan {
    /// Adiciona um atributo/campo ao span
    pub fn set_attribute<T: Into<opentelemetry::Value>>(&mut self, key: &str, value: T) {
        match self {
            TracerSpan::OpenTelemetry(span) => {
                use opentelemetry::trace::Span as OtelSpan;
                span.set_attribute(opentelemetry::KeyValue::new(key.to_string(), value));
            }
            TracerSpan::Tracing(span) => {
                // Para tracing, registra como evento dentro do span
                span.in_scope(|| {
                    tracing::info!(key = %format!("{:?}", value.into()), "span_attribute");
                });
            }
            TracerSpan::Noop => {
                // Noop - não faz nada
            }
        }
    }

    /// Registra um evento no span
    pub fn add_event(&mut self, name: &str, attributes: Vec<(&str, &str)>) {
        match self {
            TracerSpan::OpenTelemetry(span) => {
                use opentelemetry::trace::Span as OtelSpan;
                let attrs: Vec<opentelemetry::KeyValue> = attributes
                    .into_iter()
                    .map(|(k, v)| opentelemetry::KeyValue::new(k.to_string(), v.to_string()))
                    .collect();
                span.add_event(name.to_string(), attrs);
            }
            TracerSpan::Tracing(span) => {
                // Para tracing, registra como evento estruturado
                span.in_scope(|| {
                    let fields: std::collections::HashMap<&str, &str> =
                        attributes.into_iter().collect();
                    tracing::info!(event = name, ?fields, "span_event");
                });
            }
            TracerSpan::Noop => {
                // Noop - não faz nada
            }
        }
    }

    /// Marca o span como erro
    pub fn set_error<E: std::fmt::Display>(&mut self, error: E) {
        match self {
            TracerSpan::OpenTelemetry(span) => {
                use opentelemetry::trace::Span as OtelSpan;
                span.set_status(opentelemetry::trace::Status::Error {
                    description: std::borrow::Cow::Owned(error.to_string()),
                });
                span.set_attribute(opentelemetry::KeyValue::new("error".to_string(), true));
                span.set_attribute(opentelemetry::KeyValue::new(
                    "error.message".to_string(),
                    error.to_string(),
                ));
            }
            TracerSpan::Tracing(span) => {
                span.in_scope(|| {
                    tracing::error!(error = %error, "span_error");
                });
            }
            TracerSpan::Noop => {
                // Noop - não faz nada
            }
        }
    }

    /// Finaliza o span explicitamente
    pub fn finish(mut self) {
        match &mut self {
            TracerSpan::OpenTelemetry(_span) => {
                // OpenTelemetry spans são finalizados automaticamente no Drop
                // Mas podemos marcar como concluído aqui se necessário
            }
            TracerSpan::Tracing(_span) => {
                // Tracing spans são finalizados automaticamente quando saem de escopo
                // Não é necessário fazer nada aqui
            }
            TracerSpan::Noop => {
                // Noop - não faz nada
            }
        }
        // O Drop será chamado automaticamente quando self sair de escopo
    }
}

impl Drop for TracerSpan {
    fn drop(&mut self) {
        // Para OpenTelemetry, garantimos que o span seja finalizado
        match self {
            TracerSpan::OpenTelemetry(_span) => {
                // OpenTelemetry spans são finalizados automaticamente quando Drop
                // Não precisamos chamar end() explicitamente aqui
            }
            TracerSpan::Tracing(_) => {
                // Tracing spans são finalizados automaticamente quando saem de escopo
            }
            TracerSpan::Noop => {
                // Noop - não faz nada
            }
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MessageExchangeHeads {
    #[serde(rename = "address")]
    pub address: String,

    #[serde(rename = "heads")]
    pub heads: Vec<Entry>,
}

pub trait MessageMarshaler: Send + Sync {
    /// Define um tipo de erro associado para flexibilidade na implementação.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Serializa uma mensagem para um vetor de bytes.
    fn marshal(&self, msg: &MessageExchangeHeads) -> Result<Vec<u8>, Self::Error>;

    /// Desserializa um vetor de bytes para uma mensagem.
    fn unmarshal(&self, data: &[u8]) -> Result<MessageExchangeHeads, Self::Error>;
}

#[derive(Default)]
pub struct CreateDBOptions {
    pub event_bus: Option<EventBus>,
    pub directory: Option<String>,
    pub overwrite: Option<bool>,
    pub local_only: Option<bool>,
    pub create: Option<bool>,
    pub store_type: Option<String>,
    pub access_controller_address: Option<String>,
    pub access_controller: Option<Box<dyn ManifestParams>>,
    pub replicate: Option<bool>,
    pub keystore: Option<Arc<dyn crate::ipfs_log::identity_provider::Keystore>>,
    pub cache: Option<Arc<dyn Datastore>>,
    pub identity: Option<Identity>,
    pub sort_fn: Option<SortFn>,
    pub timeout: Option<Duration>,
    pub message_marshaler: Option<Arc<dyn MessageMarshaler<Error = GuardianError>>>,
    pub span: Option<Span>,
    pub close_func: Option<Box<dyn FnOnce() + Send>>,
    pub store_specific_opts: Option<Box<dyn Any + Send + Sync>>,
}

impl Clone for CreateDBOptions {
    fn clone(&self) -> Self {
        Self {
            event_bus: self.event_bus.clone(),
            directory: self.directory.clone(),
            overwrite: self.overwrite,
            local_only: self.local_only,
            create: self.create,
            store_type: self.store_type.clone(),
            access_controller_address: self.access_controller_address.clone(),
            access_controller: None, // Cannot clone Box<dyn ManifestParams>
            replicate: self.replicate,
            keystore: self.keystore.clone(),
            cache: self.cache.clone(),
            identity: self.identity.clone(),
            sort_fn: self.sort_fn,
            timeout: self.timeout,
            message_marshaler: self.message_marshaler.clone(),
            span: self.span.clone(),
            close_func: None,          // Cannot clone Box<dyn FnOnce()>
            store_specific_opts: None, // Cannot clone Box<dyn Any>
        }
    }
}

// Usando Arc<dyn Fn> em vez de Box<dyn Fn> para permitir clonagem
pub type StoreConstructor = Arc<
    dyn Fn(
            Arc<IpfsClient>,
            Arc<Identity>,
            Box<dyn Address>,
            NewStoreOptions,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Box<dyn Store<Error = GuardianError>>, GuardianError>>
                    + Send,
            >,
        > + Send
        + Sync,
>;

#[derive(Clone)]
pub struct CreateDocumentDBOptions {
    /// Extrai a chave de um documento genérico.
    pub key_extractor: KeyExtractorFn,

    /// Serializa um documento genérico para bytes.
    pub marshal: MarshalFn,

    /// Desserializa bytes para um documento genérico.
    pub unmarshal: UnmarshalFn,

    /// Cria uma nova instância vazia do tipo de item do documento.
    pub item_factory: ItemFactoryFn,
}

#[derive(Default, Clone)]
pub struct DetermineAddressOptions {
    pub only_hash: Option<bool>,
    pub replicate: Option<bool>,
    pub access_controller: crate::access_controller::manifest::CreateAccessControllerOptions,
}

#[async_trait::async_trait]
pub trait BaseGuardianDB: Send + Sync {
    /// Define um tipo de erro associado para flexibilidade na implementação.
    type Error: Error + Send + Sync + 'static;

    /// Retorna a instância da API do IPFS.
    fn ipfs(&self) -> Arc<IpfsClient>;

    /// Retorna a identidade utilizada pelo GuardianDB.
    fn identity(&self) -> Arc<Identity>;

    /// Cria ou abre uma store com o endereço e opções fornecidos.
    async fn open(
        &self,
        address: &str,
        options: &mut CreateDBOptions,
    ) -> Result<Arc<dyn Store<Error = GuardianError>>, Self::Error>;

    /// Retorna uma instância da store se ela já estiver aberta.
    fn get_store(&self, address: &str) -> Option<Arc<dyn Store<Error = GuardianError>>>;

    /// Cria uma nova store com o nome, tipo e opções fornecidos.
    async fn create(
        &self,
        name: &str,
        store_type: &str,
        options: &mut CreateDBOptions,
    ) -> Result<Arc<dyn Store<Error = GuardianError>>, Self::Error>;

    /// Determina o endereço de um banco de dados com base nos seus parâmetros.
    async fn determine_address(
        &self,
        name: &str,
        store_type: &str,
        options: &DetermineAddressOptions,
    ) -> Result<Box<dyn Address>, Self::Error>;

    /// Registra um novo tipo de Store.
    fn register_store_type(&mut self, store_type: &str, constructor: StoreConstructor);

    /// Desregistra um tipo de Store.
    fn unregister_store_type(&mut self, store_type: &str);

    /// Registra um novo tipo de Access Controller.
    fn register_access_controller_type(
        &mut self,
        constructor: AccessControllerConstructor,
    ) -> Result<(), Self::Error>;

    /// Desregistra um tipo de Access Controller.
    fn unregister_access_controller_type(&mut self, controller_type: &str);

    /// Obtém um construtor de Access Controller pelo seu tipo.
    fn get_access_controller_type(
        &self,
        controller_type: &str,
    ) -> Option<AccessControllerConstructor>;

    /// Retorna o barramento de eventos.
    fn event_bus(&self) -> EventBus;

    /// Retorna o span para tracing.
    fn span(&self) -> &tracing::Span;

    /// Retorna o tracer para telemetria.
    fn tracer(&self) -> Arc<TracerWrapper>;
}

/// Expõe um método para criar ou abrir uma `DocumentStore`.
#[async_trait::async_trait]
pub trait GuardianDBDocumentStoreProvider {
    /// Define um tipo de erro associado para este trait.
    type Error: Error + Send + Sync + 'static;

    /// Cria ou abre uma DocumentStore.
    async fn docs(
        &self,
        address: &str,
        options: &mut CreateDBOptions,
    ) -> Result<Box<dyn DocumentStore<Error = GuardianError>>, Self::Error>;
}
/// Combina as traits `BaseGuardianDB` e `GuardianDBDocumentStoreProvider`.
pub trait GuardianDBDocumentStore: BaseGuardianDB + GuardianDBDocumentStoreProvider {}

// Implementação "blanket" que aplica automaticamente a trait `GuardianDBDocumentStore`
impl<T: BaseGuardianDB + GuardianDBDocumentStoreProvider> GuardianDBDocumentStore for T {}

/// Expõe um método para criar ou abrir uma `KeyValueStore`.
#[async_trait::async_trait]
pub trait GuardianDBKVStoreProvider: Send + Sync {
    /// Define um tipo de erro associado para este trait.
    type Error: Error + Send + Sync + 'static;

    /// Cria ou abre uma KeyValueStore.
    async fn key_value(
        &self,
        address: &str,
        options: &mut CreateDBOptions,
    ) -> Result<Box<dyn KeyValueStore<Error = GuardianError>>, Self::Error>;
}

/// Combina as traits `BaseGuardianDB` e `GuardianDBKVStoreProvider`.
pub trait GuardianDBKVStore: BaseGuardianDB + GuardianDBKVStoreProvider {}

// Implementação "blanket" que aplica automaticamente a trait `GuardianDBKVStore`
// a qualquer tipo que já satisfaça as condições.
impl<T: BaseGuardianDB + GuardianDBKVStoreProvider> GuardianDBKVStore for T {}

/// Expõe um método para criar ou abrir uma `EventLogStore`.
#[async_trait::async_trait]
pub trait GuardianDBLogStoreProvider {
    /// Define um tipo de erro associado para este trait.
    type Error: Error + Send + Sync + 'static;

    /// Cria ou abre uma EventLogStore (um log de eventos append-only).
    async fn log(
        &self,
        address: &str,
        options: &mut CreateDBOptions,
    ) -> Result<Box<dyn EventLogStore<Error = GuardianError>>, Self::Error>;
}

/// Combina as traits `BaseGuardianDB` e `GuardianDBLogStoreProvider`.
pub trait GuardianDBLogStore: BaseGuardianDB + GuardianDBLogStoreProvider {}

// Implementação "blanket" para `GuardianDBLogStore`.
impl<T: BaseGuardianDB + GuardianDBLogStoreProvider> GuardianDBLogStore for T {}

/// Combina todas as traits principais do GuardianDB.
pub trait GuardianDB:
    BaseGuardianDB
    + GuardianDBKVStoreProvider
    + GuardianDBLogStoreProvider
    + GuardianDBDocumentStoreProvider
{
}

// A implementação "blanket" permite que qualquer tipo que já satisfaça todas
// as constraints seja automaticamente considerado `GuardianDB`.
impl<
    T: BaseGuardianDB
        + GuardianDBKVStoreProvider
        + GuardianDBLogStoreProvider
        + GuardianDBDocumentStoreProvider,
> GuardianDB for T
{
}

#[derive(Default, Debug, Clone)]
pub struct StreamOptions {
    /// "Greater Than": Retorna entradas que são posteriores à CID fornecida.
    pub gt: Option<Cid>,

    /// "Greater Than or Equal": Retorna entradas que são a CID fornecida ou posteriores.
    pub gte: Option<Cid>,

    /// "Less Than": Retorna entradas que são anteriores à CID fornecida.
    pub lt: Option<Cid>,

    /// "Less Than or Equal": Retorna entradas que são a CID fornecida ou anteriores.
    pub lte: Option<Cid>,

    /// Limita o número de entradas a serem retornadas.
    pub amount: Option<i32>,
}

pub trait StoreEvents {
    fn subscribe(&mut self);
}

/// Define as operações comuns a todos os tipos de stores.
#[async_trait::async_trait]
pub trait Store: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    #[deprecated(note = "use event_bus() instead")]
    fn events(&self) -> &dyn EmitterInterface;

    /// Fecha a store e libera seus recursos.
    /// Modificado para aceitar &self em vez de &mut self para compatibilidade com Arc<T>
    async fn close(&self) -> Result<(), Self::Error>;

    /// Retorna o endereço da store.
    fn address(&self) -> &dyn Address;

    /// Retorna o índice da store, que mantém o estado atual dos dados.
    /// Retorna Box para evitar problemas de lifetime com RwLock
    fn index(&self) -> Box<dyn StoreIndex<Error = Self::Error> + Send + Sync>;

    /// Retorna o tipo da store como uma string (ex: "eventlog", "kvstore").
    fn store_type(&self) -> &str;

    /// Retorna o status atual da replicação.
    fn replication_status(&self) -> ReplicationInfo;

    /// Retorna o replicador responsável pela sincronização de dados.
    fn replicator(&self) -> Option<Arc<Replicator>>;

    /// Retorna o cache da store.
    fn cache(&self) -> Arc<dyn Datastore>;

    /// Remove todo o conteúdo local da store.
    async fn drop(&mut self) -> Result<(), Self::Error>;

    /// Carrega as `amount` entradas mais recentes da rede.
    async fn load(&mut self, amount: usize) -> Result<(), Self::Error>;

    /// Sincroniza a store com uma lista de `heads` (entradas mais recentes) de outro par.
    async fn sync(&mut self, heads: Vec<Entry>) -> Result<(), Self::Error>;

    /// Carrega mais entradas a partir de um conjunto de CIDs conhecidos.
    async fn load_more_from(&mut self, amount: u64, entries: Vec<Entry>);

    /// Carrega o conteúdo da store a partir de um snapshot.
    async fn load_from_snapshot(&mut self) -> Result<(), Self::Error>;

    /// Retorna o log de operações (OpLog) subjacente.
    /// Modificado para retornar Arc para evitar problemas de lifetime
    fn op_log(&self) -> Arc<RwLock<Log>>;

    /// Retorna a instância da API do IPFS.
    fn ipfs(&self) -> Arc<IpfsClient>;

    /// Retorna o nome do banco de dados.
    fn db_name(&self) -> &str;

    /// Retorna a identidade usada pela store.
    fn identity(&self) -> &Identity;

    /// Retorna o controlador de acesso da store.
    fn access_controller(&self) -> &dyn AccessController;

    /// Adiciona uma nova operação à store.
    async fn add_operation(
        &mut self,
        op: Operation,
        on_progress_callback: Option<ProgressCallback>,
    ) -> Result<Entry, Self::Error>;

    /// Retorna o span.
    /// Modificado para retornar Arc para evitar problemas de lifetime
    fn span(&self) -> Arc<Span>;

    /// Retorna o tracer para telemetria.
    fn tracer(&self) -> Arc<TracerWrapper>;

    /// Retorna o barramento de eventos.
    fn event_bus(&self) -> Arc<EventBus>;

    /// Método auxiliar para downcast
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Uma store que se comporta como um log de eventos "append-only" distribuído.
/// Herda todas as funcionalidades da trait `Store` e adiciona operações
/// específicas para logs sequenciais imutáveis.
///
/// Ideal para casos de uso como auditoria, event sourcing, e sistemas
/// que requerem histórico completo e ordenado de eventos.
#[async_trait::async_trait]
pub trait EventLogStore: Store {
    /// Adiciona um novo dado ao log.
    /// Os dados são anexados de forma sequencial e imutável.
    ///
    /// # Argumentos
    /// * `data` - Os dados binários a serem adicionados ao log
    ///
    /// # Retorna
    /// A operação ADD criada, contendo metadados do evento adicionado
    async fn add(&mut self, data: Vec<u8>) -> Result<Operation, Self::Error>;

    /// Obtém uma entrada específica do log pelo seu CID.
    /// Permite acesso direto a qualquer entrada histórica.
    ///
    /// # Argumentos
    /// * `cid` - O Content Identifier da entrada desejada
    ///
    /// # Retorna
    /// A operação correspondente ao CID, ou erro se não encontrada
    async fn get(&self, cid: Cid) -> Result<Operation, Self::Error>;

    /// Retorna um stream de operações, com opções de filtro.
    /// Em Rust, em vez de passar um canal, é idiomático retornar um `Stream`.
    ///
    /// # TODO
    /// Esta funcionalidade requer implementação cuidadosa de Stream para evitar
    /// problemas de lifetime. Por enquanto, use `list()` para casos síncronos.
    ///
    /// # Implementação futura
    /// ```ignore
    /// async fn stream(&self, options: Option<StreamOptions>)
    ///     -> Result<Pin<Box<dyn Stream<Item = Operation> + Send>>, Self::Error>;
    /// ```
    /// Retorna uma lista de operações que ocorreram na store, com opções de filtro.
    /// Permite consultas históricas com critérios específicos de tempo/posição.
    ///
    /// # Argumentos
    /// * `options` - Filtros opcionais para limitar/ordenar os resultados
    ///
    /// # Retorna
    /// Lista ordenada de operações que atendem aos critérios
    async fn list(&self, options: Option<StreamOptions>) -> Result<Vec<Operation>, Self::Error>;
}

/// Uma store que se comporta como um banco de dados chave-valor distribuído.
/// Herda todas as funcionalidades da trait `Store` e adiciona operações
/// específicas para pares chave-valor com semântica CRDT.
///
/// Todas as operações são replicadas automaticamente através da rede
/// e mantêm consistência eventual entre os peers.
#[async_trait::async_trait]
pub trait KeyValueStore: Store {
    /// Retorna todos os pares chave-valor da store em um mapa.
    /// Esta operação lê o estado atual do índice local.
    fn all(&self) -> std::collections::HashMap<String, Vec<u8>>;

    /// Define um valor para uma chave específica.
    /// Cria uma nova operação PUT no log distribuído que será replicada.
    ///
    /// # Argumentos
    /// * `key` - A chave para associar ao valor (não pode estar vazia)
    /// * `value` - Os dados binários a serem armazenados
    ///
    /// # Retorna
    /// A operação PUT criada, ou erro se a operação falhar
    async fn put(&mut self, key: &str, value: Vec<u8>) -> Result<Operation, Self::Error>;

    /// Remove uma chave e seu valor associado.
    /// Cria uma nova operação DEL no log distribuído que será replicada.
    ///
    /// # Argumentos
    /// * `key` - A chave a ser removida
    ///
    /// # Retorna
    /// A operação DEL criada, ou erro se a chave não existir ou operação falhar
    async fn delete(&mut self, key: &str) -> Result<Operation, Self::Error>;

    /// Obtém o valor associado a uma chave.
    /// Consulta o índice local para o estado mais recente.
    ///
    /// # Argumentos
    /// * `key` - A chave a ser procurada
    ///
    /// # Retorna
    /// `Some(Vec<u8>)` se a chave existir, `None` se não existir, ou erro se houver falha no acesso
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error>;
}

/// Uma struct simples para passar opções ao método `get` de uma DocumentStore.
#[derive(Default, Debug, Clone, Copy)]
pub struct DocumentStoreGetOptions {
    pub case_insensitive: bool,
    pub partial_matches: bool,
}

#[derive(Default, Debug, Clone)]
pub struct DocumentStoreQueryOptions {
    pub limit: Option<usize>,
    pub skip: Option<usize>,
    pub sort: Option<String>,
}

/// Uma store que lida com documentos (objetos semi-estruturados).
///
/// Esta trait combina funcionalidades de store básica com operações específicas
/// para documentos, incluindo consultas avançadas e operações em lote.
#[async_trait::async_trait]
pub trait DocumentStore: Store {
    /// Armazena um único documento.
    /// O documento deve implementar as traits Send + Sync para thread safety.
    async fn put(&mut self, document: Document) -> Result<Operation, Self::Error>;

    /// Deleta um documento pela sua chave.
    /// Retorna a operação de deleção que foi aplicada ao log.
    async fn delete(&mut self, key: &str) -> Result<Operation, Self::Error>;

    /// Adiciona múltiplos documentos em operações separadas e retorna a última.
    /// Cada documento é processado individualmente, criando uma entrada separada no log.
    async fn put_batch(&mut self, values: Vec<Document>) -> Result<Operation, Self::Error>;

    /// Adiciona múltiplos documentos em uma única operação e a retorna.
    /// Todos os documentos são incluídos em uma única entrada do log.
    async fn put_all(&mut self, values: Vec<Document>) -> Result<Operation, Self::Error>;

    /// Recupera documentos por uma chave, com opções de busca.
    /// Suporta busca case-insensitive e correspondências parciais baseadas nas opções.
    async fn get(
        &self,
        key: &str,
        opts: Option<DocumentStoreGetOptions>,
    ) -> Result<Vec<Document>, Self::Error>;

    /// Encontra documentos usando uma função de filtro (predicado).
    async fn query(&self, filter: AsyncDocumentFilter) -> Result<Vec<Document>, Self::Error>;
}

/// Index contém o estado atual de uma store. Ele processa o log de
/// operações (`OpLog`) para construir a visão mais recente dos dados,
/// implementando a lógica do CRDT.
pub trait StoreIndex: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    /// Verifica se uma chave existe no índice.
    /// Método seguro que não requer acesso aos dados em si.
    fn contains_key(&self, key: &str) -> std::result::Result<bool, Self::Error>;

    /// Retorna uma cópia dos dados para uma chave específica como bytes.
    /// Método seguro que funciona com qualquer implementação de sincronização.
    fn get_bytes(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error>;

    /// Retorna todas as chaves disponíveis no índice.
    /// Útil para iteração e operações de listagem.
    fn keys(&self) -> std::result::Result<Vec<String>, Self::Error>;

    /// Retorna o número de entradas no índice.
    fn len(&self) -> std::result::Result<usize, Self::Error>;

    /// Verifica se o índice está vazio.
    fn is_empty(&self) -> std::result::Result<bool, Self::Error>;

    /// Atualiza o índice aplicando novas entradas do log de operações.
    /// Recebe `&mut self` pois este método modifica o estado do índice.
    fn update_index(
        &mut self,
        log: &Log,
        entries: &[Entry],
    ) -> std::result::Result<(), Self::Error>;

    /// Limpa todos os dados do índice.
    /// Útil para reset ou reconstrução completa.
    fn clear(&mut self) -> std::result::Result<(), Self::Error>;

    // === MÉTODOS OPCIONAIS PARA OTIMIZAÇÃO ===

    /// Retorna um range de entradas completas (se suportado pelo índice).
    ///
    /// Este método opcional permite que índices que mantêm Entry completas
    /// exponham acesso direto otimizado para queries de range.
    ///
    /// # Argumentos
    ///
    /// * `start` - Índice inicial (inclusivo)
    /// * `end` - Índice final (exclusivo)
    ///
    /// # Retorna
    ///
    /// `Some(Vec<Entry>)` se o índice suporta acesso direto a Entry
    /// `None` se o índice não suporta ou range inválido
    ///
    /// # Performance
    ///
    /// - O(1) para validação de range
    /// - O(end - start) para coleção dos resultados
    /// - Evita deserialização de bytes para Entry
    fn get_entries_range(&self, _start: usize, _end: usize) -> Option<Vec<Entry>> {
        // ***Implementação padrão retorna None - índices que suportam podem override
        None
    }

    /// Retorna as últimas N entradas (se suportado pelo índice).
    ///
    /// Otimização comum para EventLogStore onde frequentemente
    /// queremos as entradas mais recentes.
    ///
    /// # Argumentos
    ///
    /// * `count` - Número de entradas a retornar
    ///
    /// # Retorna
    ///
    /// `Some(Vec<Entry>)` se o índice suporta acesso direto
    /// `None` se não suportado
    fn get_last_entries(&self, _count: usize) -> Option<Vec<Entry>> {
        // ***Implementação padrão retorna None
        None
    }

    /// Retorna uma Entry específica por CID (se suportado pelo índice).
    ///
    /// Permite busca O(1) ou O(log n) por CID ao invés de busca linear.
    ///
    /// # Argumentos
    ///
    /// * `cid` - Content Identifier da entrada desejada
    ///
    /// # Retorna
    ///
    /// `Some(Entry)` se encontrada e suportada
    /// `None` se não encontrada ou não suportada
    fn get_entry_by_cid(&self, _cid: &Cid) -> Option<Entry> {
        // ***Implementação padrão retorna None
        None
    }

    /// Verifica se o índice suporta queries otimizadas com Entry completas.
    ///
    /// Permite que o código cliente determine se pode usar os métodos
    /// opcionais de otimização.
    fn supports_entry_queries(&self) -> bool {
        // ***Implementação padrão retorna false
        false
    }
}

/// Opções detalhadas para a criação de uma nova instância de Store.
/// Esta struct é o ponto central de configuração para todas as funcionalidades
/// avançadas de uma store, incluindo índices, cache, replicação e telemetria.
pub struct NewStoreOptions {
    // === CORE CONFIGURATION ===
    /// Barramento de eventos para comunicação interna
    pub event_bus: Option<EventBus>,

    /// Construtor do índice personalizado para a store
    pub index: Option<IndexConstructor>,

    /// Controlador de acesso para permissões e autenticação
    pub access_controller: Option<Arc<dyn AccessController>>,

    /// Diretório base para armazenamento de dados
    pub directory: String,

    /// Função de ordenação personalizada para entradas do log
    pub sort_fn: Option<SortFn>,

    // === NETWORKING & P2P ===
    /// Identificador único do peer na rede P2P
    pub peer_id: PeerId,

    /// Interface PubSub para comunicação distribuída
    pub pubsub: Option<Arc<dyn PubSubInterface<Error = GuardianError>>>,

    /// Canal direto para comunicação peer-to-peer
    pub direct_channel: Option<Arc<dyn DirectChannel<Error = GuardianError>>>,

    /// Marshaler para serialização de mensagens de rede
    pub message_marshaler: Option<Arc<dyn MessageMarshaler<Error = GuardianError>>>,

    // === PERFORMANCE & STORAGE ===
    /// Sistema de cache para otimização de acesso a dados
    pub cache: Option<Arc<dyn Datastore>>,

    /// Callback para destruição do cache (pode falhar)
    pub cache_destroy: Option<CleanupCallback>,

    /// Número de workers para replicação concorrente
    pub replication_concurrency: Option<u32>,

    /// Contador de referências para garbage collection
    pub reference_count: Option<i32>,

    /// Limite máximo de entradas no histórico
    pub max_history: Option<i32>,

    // === BEHAVIOR FLAGS ===
    /// Habilita/desabilita replicação automática
    pub replicate: Option<bool>,

    // === OBSERVABILITY ===
    /// Sistema de logging estruturado
    pub span: Option<Span>,

    /// Tracer para telemetria distribuída (OpenTelemetry)
    pub tracer: Option<Arc<TracerWrapper>>,

    // === LIFECYCLE MANAGEMENT ===
    /// Callback executado no fechamento da store
    pub close_func: Option<Box<dyn FnOnce() + Send>>,

    // === EXTENSIBILITY ===
    /// Opções específicas do tipo de store (extensibilidade)
    /// Permite que diferentes tipos de store tenham configurações customizadas
    pub store_specific_opts: Option<Box<dyn Any + Send + Sync>>,
}

impl Default for NewStoreOptions {
    fn default() -> Self {
        let peer_id = PeerId::random();

        Self {
            event_bus: None,
            index: None,
            access_controller: None,
            directory: String::new(),
            sort_fn: None,
            peer_id,
            pubsub: None,
            direct_channel: None,
            message_marshaler: None,
            cache: None,
            cache_destroy: None,
            replication_concurrency: None,
            reference_count: None,
            max_history: None,
            replicate: None,
            span: None,
            tracer: None,
            close_func: None,
            store_specific_opts: None,
        }
    }
}

/// Opções para configurar um `DirectChannel`.
#[derive(Default, Clone)]
pub struct DirectChannelOptions {
    pub span: Option<Span>,
}

/// Trait para a comunicação direta com outro par na rede.
#[async_trait::async_trait]
pub trait DirectChannel: Send + Sync + std::any::Any {
    type Error: Error + Send + Sync + 'static;

    /// Espera até que a conexão com o outro par seja estabelecida.
    async fn connect(&mut self, peer: PeerId) -> Result<(), Self::Error>;

    /// Envia dados para o outro par.
    async fn send(&mut self, peer: PeerId, data: Vec<u8>) -> Result<(), Self::Error>;

    /// Fecha a conexão.
    async fn close(&mut self) -> Result<(), Self::Error>;

    /// Fecha a conexão usando referência compartilhada (&self).
    /// Este método permite fechar o canal quando usado dentro de Arc<>.
    async fn close_shared(&self) -> Result<(), Self::Error>;

    /// Método auxiliar para downcast
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Define o conteúdo de uma mensagem recebida via pubsub ou canal direto.
/// Esta struct é necessária para a definição de `DirectChannelEmitter`.
#[derive(Debug, Clone)]
pub struct EventPubSubPayload {
    pub payload: Vec<u8>,
    pub peer: PeerId,
}

/// Uma trait usada para emitir eventos recebidos de um `DirectChannel`.
#[async_trait::async_trait]
pub trait DirectChannelEmitter: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    /// Emite um payload recebido.
    async fn emit(&self, payload: EventPubSubPayload) -> Result<(), Self::Error>;

    /// Fecha o emissor.
    async fn close(&self) -> Result<(), Self::Error>;
}

/// Uma fábrica para criar instâncias de `DirectChannel`.
pub type DirectChannelFactory = Arc<
    dyn Fn(
            Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
            Option<DirectChannelOptions>,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = Result<
                            Arc<dyn DirectChannel<Error = GuardianError>>,
                            Box<dyn Error + Send + Sync>,
                        >,
                    > + Send,
            >,
        > + Send
        + Sync,
>;

/// Define o protótipo de uma função (ou closure) que constrói e retorna
/// uma nova instância de um `StoreIndex`.
pub type IndexConstructor =
    Box<dyn Fn(&[u8]) -> Box<dyn StoreIndex<Error = GuardianError>> + Send + Sync>;

/// Um protótipo para a função de callback que é acionada quando novas entradas
/// (`Entry`) são escritas na store. É um tipo de função assíncrona.
pub type OnWritePrototype = Box<
    dyn Fn(
            Cid,
            Entry,
            Vec<Cid>,
        )
            -> Pin<Box<dyn Future<Output = Result<(), Box<dyn Error + Send + Sync>>> + Send>>
        + Send
        + Sync,
>;

/// Representa uma nova mensagem recebida em um tópico pub/sub.
#[derive(Debug, Clone)]
pub struct EventPubSubMessage {
    pub content: Vec<u8>,
}

/// Define o protótipo para um construtor de `AccessController`.
pub type AccessControllerConstructor = Arc<
    dyn Fn(
            Arc<dyn BaseGuardianDB<Error = GuardianError>>,
            &crate::access_controller::manifest::CreateAccessControllerOptions,
            Option<Vec<AccessControllerOption>>,
        )
            -> Pin<Box<dyn Future<Output = Result<Arc<dyn AccessController>, GuardianError>> + Send>>
        + Send
        + Sync,
>;

/// Representa a inscrição em um tópico pub/sub específico.
#[async_trait::async_trait]
pub trait PubSubTopic: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    /// Publica uma nova mensagem no tópico.
    async fn publish(&self, message: Vec<u8>) -> Result<(), Self::Error>;

    /// Lista os pares (peers) conectados a este tópico.
    async fn peers(&self) -> Result<Vec<PeerId>, Self::Error>;

    /// Observa os pares que entram e saem do tópico.
    async fn watch_peers(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = events::Event> + Send>>, Self::Error>;

    /// Observa as novas mensagens publicadas no tópico.
    async fn watch_messages(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = EventPubSubMessage> + Send>>, Self::Error>;

    /// Retorna o nome do tópico.
    fn topic(&self) -> &str;
}

/// Trait principal do sistema pub/sub.
#[async_trait::async_trait]
pub trait PubSubInterface: Send + Sync + std::any::Any {
    type Error: Error + Send + Sync + 'static;

    /// Inscreve-se em um tópico.
    async fn topic_subscribe(
        &mut self,
        topic: &str,
    ) -> Result<Arc<dyn PubSubTopic<Error = GuardianError>>, Self::Error>;

    /// Método auxiliar para downcast
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Opções para a criação de uma inscrição em um tópico Pub/Sub.
#[derive(Default, Clone)]
pub struct PubSubSubscriptionOptions {
    pub span: Option<Span>,
    pub tracer: Option<Arc<TracerWrapper>>,
}

/// EventPubSub::Leave
/// Representa um evento disparado quando um par (peer) sai
/// de um tópico do canal Pub/Sub.
///
/// EventPubSub::Join
/// Representa um evento disparado quando um par (peer) entra
/// em um tópico do canal Pub/Sub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventPubSub {
    Join { topic: String, peer: PeerId },
    Leave { topic: String, peer: PeerId },
}
