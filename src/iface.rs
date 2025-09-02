use crate::access_controller::{
    manifest::ManifestParams, traits::AccessController, traits::Option as AccessControllerOption,
};
use crate::address::Address;
use crate::data_store::Datastore; // Import da trait Datastore do módulo data_store
use crate::error::GuardianError;
use crate::events::{self, EmitterInterface};
use crate::ipfs_core_api::client::IpfsClient; // Use o cliente local
use crate::ipfs_log::{entry::Entry, identity::Identity, log::Log};
use crate::pubsub::event::EventBus; // Import do nosso EventBus
use crate::stores::{
    operation::operation::Operation,
    replicator::{replication_info::ReplicationInfo, replicator::Replicator},
};
use cid::Cid;
use futures::stream::Stream;
use libp2p::core::PeerId;
use opentelemetry::global::BoxedTracer;
use opentelemetry::trace::noop::NoopTracer;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use slog::Logger;
use std::any::Any;
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

// Type aliases para reduzir complexidade de tipos
type KeyExtractorFn =
    Arc<dyn Fn(&serde_json::Value) -> Result<String, GuardianError> + Send + Sync>;
type MarshalFn = Arc<dyn Fn(&serde_json::Value) -> Result<Vec<u8>, GuardianError> + Send + Sync>;
type UnmarshalFn = Arc<dyn Fn(&[u8]) -> Result<serde_json::Value, GuardianError> + Send + Sync>;
type ItemFactoryFn = Arc<dyn Fn() -> serde_json::Value + Send + Sync>;
type CleanupCallback = Box<
    dyn FnOnce() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> + Send + Sync,
>;
// Temporary type definitions until proper modules are available
pub type SortFn = fn(&Entry, &Entry) -> std::cmp::Ordering;

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

/// Enum para resolver problema de dyn compatibility do Tracer
pub enum TracerWrapper {
    Boxed(Arc<BoxedTracer>),
    Noop(NoopTracer),
}

impl Clone for TracerWrapper {
    fn clone(&self) -> Self {
        match self {
            TracerWrapper::Boxed(tracer) => TracerWrapper::Boxed(tracer.clone()),
            TracerWrapper::Noop(_) => TracerWrapper::Noop(NoopTracer::new()),
        }
    }
}

impl TracerWrapper {
    /// Start method para criar spans
    pub fn start(&self, name: &str) -> TracerSpan {
        let _name = name; // Suppress unused warning
        match self {
            TracerWrapper::Boxed(_) => TracerSpan::Boxed,
            TracerWrapper::Noop(_) => TracerSpan::Noop,
        }
    }
}

/// Struct simples para representar spans
pub enum TracerSpan {
    Boxed,
    Noop,
}

impl Drop for TracerSpan {
    fn drop(&mut self) {
        // Automatic span ending
    }
}

// Removemos implementação de Tracer para TracerWrapper por enquanto - será implementada depois
// quando todos os types estiverem definidos corretamente

/// equivalente a struct `MessageExchangeHeads` em go
///
/// Adicionamos os derives de `serde` para permitir a serialização e desserialização
/// de/para JSON, que é o propósito das tags `json:"..."` no código Go.
/// A struct agora possui os dados (owned data) em vez de slices de ponteiros
/// para se alinhar com o modelo de propriedade do Rust.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MessageExchangeHeads {
    #[serde(rename = "address")]
    pub address: String,

    #[serde(rename = "heads")]
    pub heads: Vec<Entry>,
}

/// equivalente a interface `MessageMarshaler` em go
///
/// Interfaces em Go são traduzidas para `traits` em Rust.
/// O trait define um comportamento (serializar e desserializar) que pode ser
/// implementado por diferentes tipos. O método `unmarshal` foi adaptado
/// para retornar um `Result<MessageExchangeHeads, ...>` o que é mais idiomático
/// em Rust do que modificar um parâmetro de entrada.
pub trait MessageMarshaler: Send + Sync {
    /// Define um tipo de erro associado para flexibilidade na implementação.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Serializa uma mensagem para um vetor de bytes.
    fn marshal(&self, msg: &MessageExchangeHeads) -> Result<Vec<u8>, Self::Error>;

    /// Desserializa um vetor de bytes para uma mensagem.
    fn unmarshal(&self, data: &[u8]) -> Result<MessageExchangeHeads, Self::Error>;
}

/// equivalente a struct `CreateDBOptions` em go
///
/// Campos que em Go eram ponteiros (ex: `*string`, `*bool`) são traduzidos
/// para `Option<T>` em Rust. Isso representa de forma segura a possibilidade
/// de um valor estar ausente.
/// Interfaces Go (`keystore.Interface`, `MessageMarshaler`) são traduzidas para
/// `Arc<dyn Trait>`, que é um ponteiro inteligente thread-safe para um objeto trait.
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
    pub keystore: Option<Arc<dyn std::fmt::Display>>, // Placeholder until keystore trait is available
    pub cache: Option<Arc<dyn Datastore>>,
    pub identity: Option<Identity>,
    pub sort_fn: Option<SortFn>,
    pub timeout: Option<Duration>,
    pub message_marshaler: Option<Arc<dyn MessageMarshaler<Error = GuardianError>>>,
    pub logger: Option<Logger>,

    /// Um `Box<dyn FnOnce()>` é um bom equivalente para uma função de fechamento que só deve ser chamada uma vez.
    pub close_func: Option<Box<dyn FnOnce() + Send>>,

    /// `interface{}` em Go é traduzido para `Box<dyn Any + Send + Sync>` em Rust
    /// para permitir qualquer tipo de dado de forma segura entre threads.
    pub store_specific_opts: Option<Box<dyn Any + Send + Sync>>,
}

// O tipo `StoreConstructor` precisa ser definido para ser usado na trait `BaseGuardianDB`.
// Em Go: func(coreiface.CoreAPI, *identityprovider.Identity, address.Address, *NewStoreOptions) (Store, error)
// Em Rust, isso se torna um tipo que pode ser um `Fn` ou `FnMut`.
// Usamos `Pin<Box<dyn Future>>` para um retorno assíncrono.
pub type StoreConstructor = Box<
    dyn Fn(
            Arc<IpfsClient>,
            Arc<Identity>,
            Box<dyn Address>,
            NewStoreOptions, // Assumindo que NewStoreOptions será definida
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Box<dyn Store<Error = GuardianError>>, GuardianError>>
                    + Send,
            >,
        > + Send
        + Sync,
>;

/// equivalente a struct `CreateDocumentDBOptions` em go
///
/// Funções em Go (func) são traduzidas para tipos `Box<dyn Fn(...)>` em Rust.
/// O tipo `interface{}` de Go é representado por `Box<dyn Any>`.
///
/// Nota: Em um design idiomático de Rust, seria mais comum usar genéricos (`<T>`)
/// em vez de `Box<dyn Any>`, mas esta é a tradução mais direta do conceito de Go.
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

/// equivalente a struct `DetermineAddressOptions` em go
///
/// Os campos que eram ponteiros em Go (`*bool`) tornam-se `Option<bool>` em Rust,
/// representando valores que podem ou não ser fornecidos.
#[derive(Default)]
pub struct DetermineAddressOptions {
    pub only_hash: Option<bool>,
    pub replicate: Option<bool>,
    pub access_controller: crate::access_controller::manifest::CreateAccessControllerOptions,
}

/// equivalente a interface `BaseGuardianDB` em go
///
/// A interface é convertida para uma trait em Rust. Funções que recebiam `context.Context`
/// em Go foram convertidas para métodos `async` em Rust.
#[async_trait::async_trait]
pub trait BaseGuardianDB: Send + Sync {
    /// Define um tipo de erro associado para flexibilidade na implementação.
    type Error: Error + Send + Sync + 'static;

    /// Retorna a instância da API do IPFS.
    fn ipfs(&self) -> Arc<IpfsClient>;

    /// Retorna a identidade utilizada pela GuardianDB.
    fn identity(&self) -> &Identity;

    /// Cria ou abre uma store com o endereço e opções fornecidos.
    async fn open(
        &self,
        address: &str,
        options: &mut CreateDBOptions,
    ) -> Result<Box<dyn Store<Error = GuardianError>>, Self::Error>;

    /// Retorna uma instância da store se ela já estiver aberta.
    fn get_store(&self, address: &str) -> Option<Box<dyn Store<Error = GuardianError>>>;

    /// Cria uma nova store com o nome, tipo e opções fornecidos.
    async fn create(
        &self,
        name: &str,
        store_type: &str,
        options: &mut CreateDBOptions,
    ) -> Result<Box<dyn Store<Error = GuardianError>>, Self::Error>;

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

    /// Retorna o logger.
    fn logger(&self) -> &slog::Logger;

    /// Retorna o tracer para telemetria.
    fn tracer(&self) -> Arc<TracerWrapper>;
}

/// equivalente a interface `GuardianDBDocumentStoreProvider` em go
///
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

/// equivalente a interface `GuardianDBDocumentStore` em go
///
/// Em Rust, a composição de interfaces é feita através da herança de traits.
/// Esta trait combina as capacidades de `BaseGuardianDB` e `GuardianDBDocumentStoreProvider`.
/// Uma struct que implemente `GuardianDBDocumentStore` deverá também implementar as outras duas traits.
pub trait GuardianDBDocumentStore: BaseGuardianDB + GuardianDBDocumentStoreProvider {}

// Para permitir que qualquer tipo que implemente as traits base possa ser
// usado como um `GuardianDBDocumentStore`, podemos fornecer uma implementação "blanket".
impl<T: BaseGuardianDB + GuardianDBDocumentStoreProvider> GuardianDBDocumentStore for T {}

/// equivalente a interface `GuardianDBKVStoreProvider` em go
///
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

/// equivalente a interface `GuardianDBKVStore` em go
///
/// Combina as traits `BaseGuardianDB` e `GuardianDBKVStoreProvider` (definida no passo anterior).
/// Qualquer tipo que implemente `GuardianDBKVStore` deve implementar ambas as traits base.
pub trait GuardianDBKVStore: BaseGuardianDB + GuardianDBKVStoreProvider {}

// Implementação "blanket" que aplica automaticamente a trait `GuardianDBKVStore`
// a qualquer tipo que já satisfaça as condições.
impl<T: BaseGuardianDB + GuardianDBKVStoreProvider> GuardianDBKVStore for T {}

/// equivalente a interface `GuardianDBLogStoreProvider` em go
///
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

/// equivalente a interface `GuardianDBLogStore` em go
///
/// Combina as traits `BaseGuardianDB` e `GuardianDBLogStoreProvider`.
pub trait GuardianDBLogStore: BaseGuardianDB + GuardianDBLogStoreProvider {}

// Implementação "blanket" para `GuardianDBLogStore`.
impl<T: BaseGuardianDB + GuardianDBLogStoreProvider> GuardianDBLogStore for T {}

/// equivalente a interface `GuardianDB` em go
///
/// Esta é a interface principal que agrega todas as funcionalidades.
/// Em Rust, criamos uma trait `GuardianDB` que herda de `BaseGuardianDB` e de todos
/// os `...Provider` traits. Isso garante que qualquer tipo que implemente `GuardianDB`
/// terá todos os métodos necessários (`ipfs`, `identity`, `log`, `key_value`, `docs`, etc.).
pub trait GuardianDB:
    BaseGuardianDB
    + GuardianDBKVStoreProvider
    + GuardianDBLogStoreProvider
    + GuardianDBDocumentStoreProvider
{
}

// A implementação "blanket" permite que qualquer tipo que já satisfaça todas
// as constraints seja automaticamente considerado um `GuardianDB`.
impl<
    T: BaseGuardianDB
        + GuardianDBKVStoreProvider
        + GuardianDBLogStoreProvider
        + GuardianDBDocumentStoreProvider,
> GuardianDB for T
{
}

/// equivalente a struct `StreamOptions` em go
///
/// Esta struct define os parâmetros para filtrar um stream de dados de um log.
/// Os campos que em Go eram ponteiros (`*cid.Cid`, `*int`) para indicar valores
/// opcionais, são convertidos para `Option<T>` em Rust, que é a forma
/// idiomática e segura de representar opcionalidade.
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

/// equivalente a interface `StoreEvents` em go
///
/// Uma interface simples que se torna uma trait em Rust.
/// O método `subscribe` recebe `&mut self`, pois é provável que a inscrição
/// modifique o estado do objeto (ex: adicionando um listener a uma lista interna).
pub trait StoreEvents {
    fn subscribe(&mut self);
}

/// equivalente a interface `Store` em go
///
/// Esta é a trait fundamental que define as operações comuns a todos os tipos de stores.
/// Muitas funções são `async` porque envolvem operações de I/O (rede ou disco).
#[async_trait::async_trait]
pub trait Store: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    // A interface `EmitterInterface` foi marcada como obsoleta no código Go.
    #[deprecated(note = "use event_bus() instead")]
    fn events(&self) -> &dyn EmitterInterface;

    /// Fecha a store e libera seus recursos.
    async fn close(&mut self) -> Result<(), Self::Error>;

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
    /// Retorna Option<Arc> para refletir a realidade arquitetural
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
    /// O canal `on_progress_callback` é usado para notificar o progresso,
    /// similar ao `chan<-` em Go.
    async fn add_operation(
        &mut self,
        op: Operation,
        on_progress_callback: Option<ProgressCallback>,
    ) -> Result<Entry, Self::Error>;

    /// Retorna o logger.
    /// Modificado para retornar Arc para evitar problemas de lifetime
    fn logger(&self) -> Arc<Logger>;

    /// Retorna o tracer para telemetria.
    fn tracer(&self) -> Arc<TracerWrapper>;

    /// Retorna o barramento de eventos.
    /// Modificado para retornar Arc para refletir arquitetura real
    fn event_bus(&self) -> Arc<EventBus>;
}

/// equivalente a interface `EventLogStore` em go
///
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
    /// # Nota
    /// Esta funcionalidade está comentada até que a implementação de Stream seja finalizada.
    // async fn stream(&self, options: Option<StreamOptions>) -> Result<impl Stream<Item = Operation>, Self::Error>;
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

/// equivalente a interface `KeyValueStore` em go
///
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

/// equivalente a struct `DocumentStoreGetOptions` em go
///
/// Uma struct simples para passar opções ao método `get` de uma DocumentStore.
/// Derivar `Default` e `Copy` a torna mais fácil de usar.
#[derive(Default, Debug, Clone, Copy)]
pub struct DocumentStoreGetOptions {
    pub case_insensitive: bool,
    pub partial_matches: bool,
}

/// equivalente a struct `DocumentStoreQueryOptions` em go
#[derive(Default, Debug, Clone)]
pub struct DocumentStoreQueryOptions {
    pub limit: Option<usize>,
    pub skip: Option<usize>,
    pub sort: Option<String>,
}

/// equivalente a interface `DocumentStore` em go
///
/// Uma store que lida com documentos (objetos semi-estruturados).
/// O tipo `interface{}` de Go é traduzido para `Box<dyn Any + Send + Sync>` em Rust
/// para permitir o armazenamento de qualquer tipo de dado de forma dinâmica e segura.
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
    /// A função de filtro em Go é `func(doc interface{}) (bool, error)`, que é traduzida
    /// para uma closure que pode falhar (`Result<bool, ...>`).
    /// A closure deve ser thread-safe (Send + Sync) para permitir processamento paralelo.
    async fn query(&self, filter: AsyncDocumentFilter) -> Result<Vec<Document>, Self::Error>;
}

/// equivalente a interface `StoreIndex` em go
///
/// Index contém o estado atual de uma store. Ele processa o log de
/// operações (`OpLog`) para construir a visão mais recente dos dados,
/// implementando a lógica do CRDT.
///
/// REFATORAÇÃO: Removido o método get() problemático que retornava referências
/// incompatíveis com dados protegidos por lock. Adicionados métodos mais específicos
/// e seguros para diferentes tipos de acesso aos dados.
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
}

/// equivalente a struct `NewStoreOptions` em go
///
/// Opções detalhadas para a criação de uma nova instância de Store.
/// Esta struct é o ponto central de configuração para todas as funcionalidades
/// avançadas de uma store, incluindo índices, cache, replicação e telemetria.
///
/// # Organização dos campos:
/// - **Core**: Campos essenciais para funcionamento básico
/// - **Networking**: Configurações de rede e comunicação P2P  
/// - **Performance**: Cache, índices e otimizações
/// - **Observability**: Logging, telemetria e monitoramento
/// - **Lifecycle**: Callbacks e gerenciamento de recursos
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
    pub logger: Option<Logger>,

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
        // Create a dummy PeerId for default - in real usage this should be properly generated
        let peer_id = PeerId::from_bytes(&[0u8; 32]).unwrap_or_else(|_| {
            // Fallback: generate a random PeerId
            libp2p::identity::Keypair::generate_ed25519()
                .public()
                .to_peer_id()
        });

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
            logger: None,
            tracer: None,
            close_func: None,
            store_specific_opts: None,
        }
    }
}

/// equivalente a struct `DirectChannelOptions` em go
///
/// Opções para configurar um `DirectChannel`.
#[derive(Default, Clone)]
pub struct DirectChannelOptions {
    pub logger: Option<Logger>,
}

/// equivalente a interface `DirectChannel` em go
///
/// Uma trait para a comunicação direta com outro par na rede.
/// Os métodos são `async` pois envolvem operações de rede.
#[async_trait::async_trait]
pub trait DirectChannel: Send + Sync + std::any::Any {
    type Error: Error + Send + Sync + 'static;

    /// Espera até que a conexão com o outro par seja estabelecida.
    async fn connect(&mut self, peer: PeerId) -> Result<(), Self::Error>;

    /// Envia dados para o outro par.
    async fn send(&mut self, peer: PeerId, data: Vec<u8>) -> Result<(), Self::Error>;

    /// Fecha a conexão.
    async fn close(&mut self) -> Result<(), Self::Error>;

    /// Método auxiliar para downcast
    fn as_any(&self) -> &dyn std::any::Any;
}

/// equivalente a struct `EventPubSubPayload` em go
///
/// Define o conteúdo de uma mensagem recebida via pubsub ou canal direto.
/// Esta struct é necessária para a definição de `DirectChannelEmitter`.
#[derive(Debug, Clone)]
pub struct EventPubSubPayload {
    pub payload: Vec<u8>,
    pub peer: PeerId,
}

/// equivalente a interface `DirectChannelEmitter` em go
///
/// Uma trait usada para emitir eventos recebidos de um `DirectChannel`.
#[async_trait::async_trait]
pub trait DirectChannelEmitter: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    /// Emite um payload recebido.
    async fn emit(&self, payload: EventPubSubPayload) -> Result<(), Self::Error>;

    /// Fecha o emissor.
    async fn close(&self) -> Result<(), Self::Error>;
}

/// equivalente ao tipo `DirectChannelFactory` em go
///
/// Em Rust, um tipo `func` de Go é traduzido para um alias de tipo para uma `Closure`.
/// Esta é uma fábrica para criar instâncias de `DirectChannel`.
pub type DirectChannelFactory = Box<
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

/// equivalente ao tipo `IndexConstructor` em go
///
/// Define o protótipo de uma função (ou closure) que constrói e retorna
/// uma nova instância de um `StoreIndex`.
pub type IndexConstructor =
    Box<dyn Fn(&[u8]) -> Box<dyn StoreIndex<Error = GuardianError>> + Send + Sync>;

/// equivalente ao tipo `OnWritePrototype` em go
///
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

/// equivalente a struct `EventPubSubMessage` em go
///
/// Representa uma nova mensagem recebida em um tópico pub/sub.
#[derive(Debug, Clone)]
pub struct EventPubSubMessage {
    pub content: Vec<u8>,
}

/// equivalente ao tipo `AccessControllerConstructor` em go
///
/// Define o protótipo para um construtor de `AccessController`.
/// Funções variádicas em Go (`...accesscontroller.Option`) são
/// geralmente traduzidas como um `Vec<T>` ou slice `&[T]` em Rust.
/// Usando o tipo concreto CreateAccessControllerOptions em vez da trait para dyn-compatibility.
pub type AccessControllerConstructor = Box<
    dyn Fn(
            Arc<dyn BaseGuardianDB<Error = GuardianError>>,
            &crate::access_controller::manifest::CreateAccessControllerOptions,
            Option<Vec<AccessControllerOption>>,
        )
            -> Pin<Box<dyn Future<Output = Result<Arc<dyn AccessController>, GuardianError>> + Send>>
        + Send
        + Sync,
>;

/// equivalente a interface `PubSubTopic` em go
///
/// Representa a inscrição em um tópico pub/sub específico.
#[async_trait::async_trait]
pub trait PubSubTopic: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    /// Publica uma nova mensagem no tópico.
    async fn publish(&self, message: Vec<u8>) -> Result<(), Self::Error>;

    /// Lista os pares (peers) conectados a este tópico.
    async fn peers(&self) -> Result<Vec<PeerId>, Self::Error>;

    /// Observa os pares que entram e saem do tópico.
    /// Em Rust, em vez de retornar um canal (chan), é idiomático retornar um `Stream`.
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

/// equivalente a interface `PubSubInterface` em go
///
/// Interface principal do sistema pub/sub.
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

/// equivalente a struct `PubSubSubscriptionOptions` em go
///
/// Opções para a criação de uma inscrição em um tópico Pub/Sub.
/// Usamos `Option` para indicar que o logger e o tracer podem não ser fornecidos.
#[derive(Default, Clone)]
pub struct PubSubSubscriptionOptions {
    pub logger: Option<Logger>,
    pub tracer: Option<Arc<TracerWrapper>>,
}

/// EventPubSub::Leave equivalente a struct `EventPubSubLeave` em go
/// Representa um evento disparado quando um par (peer) sai
/// de um tópico do canal Pub/Sub.
///
/// EventPubSub::Join equivalente a struct `EventPubSubJoin` em go
/// Representa um evento disparado quando um par (peer) entra
/// em um tópico do canal Pub/Sub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventPubSub {
    Join { topic: String, peer: PeerId },
    Leave { topic: String, peer: PeerId },
}
