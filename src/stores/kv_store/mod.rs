use crate::access_control::acl_simple::SimpleAccessController;
use crate::access_control::traits::AccessController;
use crate::address::Address;
use crate::data_store::Datastore;
use crate::events::EventEmitter;
use crate::guardian::error::{GuardianError, Result};
use crate::log::identity::Identity;
use crate::log::lamport_clock::LamportClock;
use crate::p2p::EventBus;
use crate::p2p::network::core::docs::WillowDocs;
use crate::stores::operation::Operation;
use crate::traits::{KeyValueStore, NewStoreOptions, Store, StoreIndex, TracerWrapper};
use bytes::Bytes;
use iroh_docs::{AuthorId, api::Doc, store::Query};
use opentelemetry::trace::{TracerProvider, noop::NoopTracerProvider};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{Span, debug, info, instrument, warn};

pub mod index;

// Chave usada no cache para persistir o NamespaceId do documento iroh-docs
const NAMESPACE_CACHE_KEY: &[u8] = b"_iroh_docs_namespace_id";

/// Implementação de StoreIndex para KeyValue Store
///
/// Mantém um índice thread-safe em memória que espelha o estado do
/// documento iroh-docs. É atualizado atomicamente após cada operação
/// de put/delete, servindo como cache síncrono para queries do StoreIndex.
pub struct KeyValueIndex {
    /// Índice interno que mapeia chaves para valores
    index: Arc<RwLock<HashMap<String, Vec<u8>>>>,
}

impl Default for KeyValueIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyValueIndex {
    pub fn new() -> Self {
        Self {
            index: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Obtém um valor do índice
    pub fn get_value(&self, key: &str) -> Option<Vec<u8>> {
        let guard = self.index.read();
        guard.get(key).cloned()
    }

    /// Obtém todos os pares chave-valor
    pub fn get_all(&self) -> HashMap<String, Vec<u8>> {
        let guard = self.index.read();
        guard.clone()
    }

    /// Conta o número de entradas
    pub fn len(&self) -> usize {
        let guard = self.index.read();
        guard.len()
    }

    /// Verifica se o índice está vazio
    pub fn is_empty(&self) -> bool {
        let guard = self.index.read();
        guard.is_empty()
    }

    /// Insere um par chave-valor no índice
    pub fn insert(&self, key: String, value: Vec<u8>) {
        let mut guard = self.index.write();
        guard.insert(key, value);
    }

    /// Remove uma chave do índice
    pub fn remove(&self, key: &str) {
        let mut guard = self.index.write();
        guard.remove(key);
    }

    /// Limpa todo o índice
    pub fn clear_all(&self) {
        let mut guard = self.index.write();
        guard.clear();
    }
}

impl StoreIndex for KeyValueIndex {
    type Error = GuardianError;

    fn contains_key(&self, key: &str) -> std::result::Result<bool, Self::Error> {
        let guard = self.index.read();
        Ok(guard.contains_key(key))
    }

    fn get_bytes(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
        let guard = self.index.read();
        Ok(guard.get(key).cloned())
    }

    fn keys(&self) -> std::result::Result<Vec<String>, Self::Error> {
        let guard = self.index.read();
        Ok(guard.keys().cloned().collect())
    }

    fn len(&self) -> std::result::Result<usize, Self::Error> {
        let guard = self.index.read();
        Ok(guard.len())
    }

    fn is_empty(&self) -> std::result::Result<bool, Self::Error> {
        let guard = self.index.read();
        Ok(guard.is_empty())
    }

    /// No-op para a implementação baseada em iroh-docs.
    /// O índice local é atualizado diretamente após cada operação put/delete,
    /// sem necessidade de replay do OpLog.
    fn update_index(
        &mut self,
        _log: &crate::log::Log,
        _entries: &[crate::log::entry::Entry],
    ) -> std::result::Result<(), Self::Error> {
        // iroh-docs gerencia seu próprio estado — o índice local é atualizado
        // diretamente nas operações put/delete.
        Ok(())
    }

    fn clear(&mut self) -> std::result::Result<(), Self::Error> {
        let mut guard = self.index.write();
        guard.clear();
        Ok(())
    }
}

/// Implementação da KeyValue Store para GuardianDB usando iroh-docs (WillowDocs).
///
/// Esta implementação utiliza o protocolo iroh-docs para armazenamento KV distribuído
/// com resolução de conflitos Last-Write-Wins (LWW), substituindo a arquitetura
/// anterior baseada em BaseStore + OpLog.
///
/// # Arquitetura
///
/// - **Backend**: iroh-docs (Willow range-based reconciliation)
/// - **Escrita**: `doc.set_bytes()` / `doc.del()` via WillowDocs
/// - **Leitura**: Query iroh-docs + fetch de bytes via blob store
/// - **Sync**: Automático via Willow (sem gossip heads manuais)
/// - **Índice local**: HashMap em memória espelhando o estado do iroh-docs
///
/// Componentes mantidos da arquitetura anterior:
/// - `AccessController` — validação de permissões em cada escrita
/// - `EventBus` — eventos reativos para UI/observers
/// - `Identity` → `AuthorId` mapping consistente
pub struct GuardianDBKeyValue {
    /// WillowDocs backend (iroh-docs)
    docs: WillowDocs,
    /// Handle do documento iroh-docs para este namespace KV
    doc_handle: Doc,
    /// AuthorId para operações de escrita (mapeado da Identity)
    author_id: AuthorId,
    /// Controlador de acesso para validação de permissões
    access_controller: Arc<dyn AccessController>,
    /// Barramento de eventos para notificações reativas
    event_bus: Arc<EventBus>,
    /// Referência ao IrohClient (para leitura de blobs e compatibilidade Store trait)
    client: Arc<crate::p2p::network::client::IrohClient>,
    /// Identidade criptográfica da store
    identity: Arc<Identity>,
    /// Endereço da store (cached para resolver problemas de lifetime)
    cached_address: Arc<dyn Address + Send + Sync>,
    /// Nome do banco de dados
    db_name: String,
    /// Cache local (sled) — usado para persistir o NamespaceId entre recarregamentos
    cache: Arc<dyn Datastore>,
    /// Índice local em memória espelhando o estado do iroh-docs
    index: Arc<KeyValueIndex>,
    /// Span para tracing estruturado
    span: Span,
    /// Tracer para telemetria
    tracer: Arc<TracerWrapper>,
    /// Interface de emissão de eventos (para compatibilidade com Store trait)
    emitter_interface: Arc<dyn crate::events::EmitterInterface + Send + Sync>,
    /// Log vazio para compatibilidade com a trait Store (op_log())
    empty_log: Arc<RwLock<crate::log::Log>>,
}

#[async_trait::async_trait]
impl Store for GuardianDBKeyValue {
    type Error = GuardianError;

    #[allow(deprecated)]
    fn events(&self) -> &dyn crate::events::EmitterInterface {
        self.emitter_interface.as_ref()
    }

    async fn close(&self) -> Result<()> {
        debug!("Starting KeyValue store close operation (iroh-docs backend)");

        // Fecha o documento iroh-docs
        if let Err(e) = self.docs.close_doc(&self.doc_handle).await {
            warn!("Failed to close iroh-docs document: {:?}", e);
        }

        debug!("KeyValue store close completed");
        Ok(())
    }

    fn address(&self) -> &dyn Address {
        self.cached_address.as_ref()
    }

    fn index(&self) -> Box<dyn StoreIndex<Error = GuardianError> + Send + Sync> {
        Box::new(KeyValueIndex {
            index: self.index.index.clone(),
        })
    }

    fn store_type(&self) -> &str {
        "keyvalue"
    }

    fn replication_status(&self) -> crate::stores::replicator::replication_info::ReplicationInfo {
        crate::stores::replicator::replication_info::ReplicationInfo::new()
    }

    fn cache(&self) -> Arc<dyn Datastore> {
        self.cache.clone()
    }

    async fn drop(&self) -> Result<()> {
        debug!("Starting KeyValue store drop operation (iroh-docs backend)");

        // Limpa o índice local
        self.index.clear_all();

        // Remove o documento iroh-docs permanentemente
        let namespace_id = self.doc_handle.id();
        if let Err(e) = self.docs.drop_doc(namespace_id).await {
            warn!("Failed to drop iroh-docs document: {:?}", e);
        }

        // Remove o NamespaceId do cache
        if let Err(e) = self.cache.delete(NAMESPACE_CACHE_KEY).await {
            warn!("Failed to remove namespace from cache: {:?}", e);
        }

        debug!("KeyValue store drop completed");
        Ok(())
    }

    /// No-op para iroh-docs — Willow sync gerencia o carregamento automaticamente.
    async fn load(&self, _amount: usize) -> Result<()> {
        // iroh-docs usa Willow range sync, não precisa de load manual.
        // Sincroniza o índice local com o estado atual do documento.
        self.sync_index_from_docs().await?;
        Ok(())
    }

    /// No-op para iroh-docs — Willow sync substitui o exchange de heads via gossip.
    async fn sync(&self, _heads: Vec<crate::log::entry::Entry>) -> Result<()> {
        // iroh-docs usa Willow range reconciliation internamente.
        // Após um sync externo, atualizamos o índice local.
        self.sync_index_from_docs().await?;
        Ok(())
    }

    /// No-op para iroh-docs.
    async fn load_more_from(&self, _amount: u64, _entries: Vec<crate::log::entry::Entry>) {
        // iroh-docs gerencia seu próprio carregamento incremental.
    }

    /// No-op para iroh-docs.
    async fn load_from_snapshot(&self) -> Result<()> {
        // iroh-docs não usa snapshots — o estado é autoridade.
        self.sync_index_from_docs().await?;
        Ok(())
    }

    /// Retorna um Log vazio para compatibilidade com a trait Store.
    /// Na arquitetura iroh-docs, o OpLog não é mais utilizado —
    /// iroh-docs gerencia seu próprio estado com LWW.
    fn op_log(&self) -> Arc<RwLock<crate::log::Log>> {
        self.empty_log.clone()
    }

    fn client(&self) -> Arc<crate::p2p::network::client::IrohClient> {
        self.client.clone()
    }

    fn db_name(&self) -> &str {
        &self.db_name
    }

    fn identity(&self) -> &Identity {
        &self.identity
    }

    fn access_controller(&self) -> &dyn crate::access_control::traits::AccessController {
        self.access_controller.as_ref()
    }

    /// Traduz uma Operation para operações iroh-docs (set_bytes/del).
    /// Retorna uma Entry sintética para compatibilidade com a trait Store.
    async fn add_operation(
        &self,
        op: Operation,
        _on_progress_callback: Option<tokio::sync::mpsc::Sender<crate::log::entry::Entry>>,
    ) -> Result<crate::log::entry::Entry> {
        let key = op.key().cloned().unwrap_or_default();

        match op.op() {
            "PUT" => {
                let value = op.value().to_vec();
                self.docs
                    .set_bytes(
                        &self.doc_handle,
                        self.author_id,
                        Bytes::from(key.clone().into_bytes()),
                        Bytes::from(value.clone()),
                    )
                    .await?;

                // Atualiza índice local
                self.index.insert(key, value);
            }
            "DEL" => {
                self.docs
                    .del(
                        &self.doc_handle,
                        self.author_id,
                        Bytes::from(key.clone().into_bytes()),
                    )
                    .await?;

                // Atualiza índice local
                self.index.remove(&key);
            }
            other => {
                return Err(GuardianError::Store(format!(
                    "Operação desconhecida: {}",
                    other
                )));
            }
        }

        // Cria Entry sintética para compatibilidade
        let payload = crate::guardian::serializer::serialize(&op).unwrap_or_default();
        let clock = LamportClock::new(self.identity.pub_key());
        let entry_arc = crate::log::entry::Entry::create(
            &self.client,
            (*self.identity).clone(),
            "",
            &payload,
            &[],
            Some(clock),
        );
        let entry = (*entry_arc).clone();

        Ok(entry)
    }

    fn span(&self) -> Arc<tracing::Span> {
        Arc::new(self.span.clone())
    }

    fn tracer(&self) -> Arc<TracerWrapper> {
        self.tracer.clone()
    }

    fn event_bus(&self) -> Arc<EventBus> {
        self.event_bus.clone()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl GuardianDBKeyValue {
    /// Retorna uma referência ao span de tracing para instrumentação
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Retorna o NamespaceId do documento iroh-docs subjacente
    pub fn namespace_id(&self) -> iroh_docs::NamespaceId {
        self.doc_handle.id()
    }

    /// Retorna o AuthorId usado para operações de escrita
    pub fn author_id(&self) -> AuthorId {
        self.author_id
    }
}

// Implementação da trait `KeyValueStore` para `GuardianDBKeyValue`.
#[async_trait::async_trait]
impl KeyValueStore for GuardianDBKeyValue {
    fn all(&self) -> HashMap<String, Vec<u8>> {
        self.index.get_all()
    }

    async fn put(&self, key: &str, value: Vec<u8>) -> Result<Operation> {
        self.put_impl(key, value).await
    }

    async fn delete(&self, key: &str) -> Result<Operation> {
        self.delete_impl(key).await
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.get_impl(key).await
    }
}

impl GuardianDBKeyValue {
    /// Retorna o número de pares chave-valor na store
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Verifica se a store está vazia
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Verifica se uma chave existe na store
    pub fn contains_key(&self, key: &str) -> bool {
        self.index.get_value(key).is_some()
    }

    /// Retorna todas as chaves da store
    pub fn keys(&self) -> Vec<String> {
        self.index.get_all().keys().cloned().collect()
    }

    /// Retorna todos os pares chave-valor da store
    pub fn all(&self) -> HashMap<String, Vec<u8>> {
        self.index.get_all()
    }

    /// Sincroniza o índice local com o estado atual do documento iroh-docs.
    ///
    /// Consulta todas as entradas do documento e reconstrói o índice em memória.
    /// Usado após operações de load/sync ou para recuperação de estado.
    pub async fn sync_index_from_docs(&self) -> Result<usize> {
        let entries = self
            .docs
            .get_many(&self.doc_handle, Query::single_latest_per_key().build())
            .await?;

        let mut count = 0;

        // Limpa e reconstrói o índice
        self.index.clear_all();

        for entry in &entries {
            let key = String::from_utf8_lossy(entry.key()).to_string();

            // Entradas com content_len == 0 são marcadores de deleção
            if entry.content_len() == 0 {
                continue;
            }

            // Lê os bytes do conteúdo via blob store usando o content_hash
            let content_hash = entry.content_hash();
            let hash_str = content_hash.to_hex();
            match self.client.cat_bytes(&hash_str).await {
                Ok(value) => {
                    self.index.insert(key, value);
                    count += 1;
                }
                Err(e) => {
                    warn!("Failed to read content for key from iroh-docs: {:?}", e);
                }
            }
        }

        info!(
            "KeyValue index synchronized from iroh-docs: {} entries loaded",
            count
        );

        Ok(count)
    }

    /// Adiciona ou atualiza um valor para uma chave específica.
    ///
    /// Escreve diretamente no documento iroh-docs via `set_bytes()`.
    /// O valor é armazenado no blob store e referenciado pelo documento.
    /// A sincronização com outros peers é automática via Willow.
    #[instrument(level = "debug", skip(self, value))]
    pub async fn put_impl(&self, key: &str, value: Vec<u8>) -> Result<Operation> {
        if key.is_empty() {
            return Err(GuardianError::Store(
                "A chave não pode estar vazia".to_string(),
            ));
        }

        if value.is_empty() {
            return Err(GuardianError::Store(
                "O valor não pode estar vazio".to_string(),
            ));
        }

        // Escreve no documento iroh-docs
        self.docs
            .set_bytes(
                &self.doc_handle,
                self.author_id,
                Bytes::from(key.as_bytes().to_vec()),
                Bytes::from(value.clone()),
            )
            .await
            .map_err(|e| {
                GuardianError::Store(format!(
                    "Erro ao escrever chave '{}' no iroh-docs: {}",
                    key, e
                ))
            })?;

        // Atualiza o índice local imediatamente
        self.index.insert(key.to_string(), value.clone());

        debug!("PUT key='{}' ({} bytes) via iroh-docs", key, value.len());

        Ok(Operation::new(
            Some(key.to_string()),
            "PUT".to_string(),
            Some(value),
        ))
    }

    /// Remove um valor associado a uma chave específica.
    ///
    /// Remove a chave do documento iroh-docs via `del()`.
    /// A deleção é propagada para outros peers automaticamente via Willow.
    #[instrument(level = "debug", skip(self))]
    pub async fn delete_impl(&self, key: &str) -> Result<Operation> {
        if key.is_empty() {
            return Err(GuardianError::Store(
                "A chave não pode estar vazia".to_string(),
            ));
        }

        // Verifica se a chave existe no índice local
        if !self.contains_key(key) {
            return Err(GuardianError::Store(format!(
                "Chave '{}' não encontrada",
                key
            )));
        }

        // Remove do documento iroh-docs
        let deleted = self
            .docs
            .del(
                &self.doc_handle,
                self.author_id,
                Bytes::from(key.as_bytes().to_vec()),
            )
            .await
            .map_err(|e| {
                GuardianError::Store(format!(
                    "Erro ao deletar chave '{}' no iroh-docs: {}",
                    key, e
                ))
            })?;

        // Atualiza o índice local imediatamente
        self.index.remove(key);

        debug!(
            "DEL key='{}' ({} entries removed) via iroh-docs",
            key, deleted
        );

        Ok(Operation::new(
            Some(key.to_string()),
            "DEL".to_string(),
            None,
        ))
    }

    /// Obtém o valor associado a uma chave específica.
    ///
    /// Consulta o índice local em memória primeiro (cache síncrono).
    /// A leitura é O(1) via HashMap, sem necessidade de replay de log.
    #[instrument(level = "debug", skip(self))]
    pub async fn get_impl(&self, key: &str) -> Result<Option<Vec<u8>>> {
        if key.is_empty() {
            return Err(GuardianError::Store(
                "A chave não pode estar vazia".to_string(),
            ));
        }

        // Consulta o índice local (espelha o estado do iroh-docs)
        Ok(self.index.get_value(key))
    }

    pub fn get_type(&self) -> &'static str {
        "keyvalue"
    }

    /// Cria uma nova instância de GuardianDBKeyValue usando iroh-docs como backend.
    ///
    /// # Fluxo de inicialização
    ///
    /// 1. Inicializa o WillowDocs a partir do IrohClient backend
    /// 2. Obtém/cria o AuthorId padrão
    /// 3. Cria ou abre um documento iroh-docs (namespace persistido via cache)
    /// 4. Configura AccessController e EventBus
    /// 5. Sincroniza o índice local com o estado do documento
    #[instrument(level = "debug", skip(client, identity, addr, options))]
    pub async fn new(
        client: Arc<crate::p2p::network::client::IrohClient>,
        identity: Arc<Identity>,
        addr: Arc<dyn Address + Send + Sync>,
        options: Option<NewStoreOptions>,
    ) -> Result<Self> {
        let opts = options.unwrap_or_default();

        // --- 1. Inicializa iroh-docs ---

        // Garante que o subsistema iroh-docs está inicializado no cliente
        if !client.has_docs_client().await {
            client.init_docs().await.map_err(|e| {
                GuardianError::Store(format!("Falha ao inicializar iroh-docs: {}", e))
            })?;
        }

        let mut docs = client.docs_client().await.ok_or_else(|| {
            GuardianError::Store("iroh-docs não disponível após inicialização".to_string())
        })?;

        // --- 2. Obtém AuthorId ---

        let author_id = docs.get_or_init_author().await.map_err(|e| {
            GuardianError::Store(format!("Falha ao inicializar author iroh-docs: {}", e))
        })?;

        // --- 3. Configura componentes ---

        let db_name = addr.get_path().to_string();
        let span = tracing::info_span!("keyvalue_store", address = %addr.to_string());

        // EventBus
        let event_bus = opts.event_bus.unwrap_or_default();
        let event_bus = Arc::new(event_bus);

        // AccessController
        let access_controller = opts.access_controller.unwrap_or_else(|| {
            let mut default_access = HashMap::new();
            default_access.insert("write".to_string(), vec!["*".to_string()]);
            Arc::new(SimpleAccessController::new(Some(default_access))) as Arc<dyn AccessController>
        });

        // Tracer
        let tracer = opts.tracer.unwrap_or_else(|| {
            Arc::new(TracerWrapper::Noop(
                NoopTracerProvider::new().tracer("berty.guardian-db"),
            ))
        });

        // Cache (usa sled se diretório fornecido, senão cria um cache em memória)
        let cache: Arc<dyn Datastore> = if let Some(cache) = opts.cache {
            cache
        } else {
            let cache_dir = if opts.directory.is_empty() {
                format!("./GuardianDB/{}/cache", addr)
            } else {
                format!("{}/cache", opts.directory)
            };
            Self::create_cache(addr.as_ref(), &cache_dir)?
        };

        // EventEmitter para compatibilidade com Store trait
        let emitter_interface: Arc<dyn crate::events::EmitterInterface + Send + Sync> =
            Arc::new(EventEmitter::new());

        // --- 4. Cria ou abre documento iroh-docs ---

        // Tenta recuperar o NamespaceId do cache para reabrir documento existente
        let doc_handle = match cache.get(NAMESPACE_CACHE_KEY).await {
            Ok(Some(namespace_bytes)) if namespace_bytes.len() == 32 => {
                // NamespaceId existente — tenta reabrir o documento
                let mut ns_bytes = [0u8; 32];
                ns_bytes.copy_from_slice(&namespace_bytes);
                let namespace_id = iroh_docs::NamespaceId::from(ns_bytes);

                match docs.open_doc(namespace_id).await? {
                    Some(doc) => {
                        info!("Reopened existing iroh-docs document: {:?}", namespace_id);
                        doc
                    }
                    None => {
                        // Documento não encontrado — cria um novo
                        warn!(
                            "Cached namespace {:?} not found, creating new document",
                            namespace_id
                        );
                        let doc = docs.create_doc().await?;
                        let ns_id = doc.id();
                        cache
                            .put(NAMESPACE_CACHE_KEY, ns_id.as_bytes())
                            .await
                            .map_err(|e| {
                                GuardianError::Store(format!(
                                    "Falha ao persistir NamespaceId: {}",
                                    e
                                ))
                            })?;
                        info!("Created new iroh-docs document: {:?}", ns_id);
                        doc
                    }
                }
            }
            _ => {
                // Sem NamespaceId no cache — cria novo documento
                let doc = docs.create_doc().await?;
                let ns_id = doc.id();
                cache
                    .put(NAMESPACE_CACHE_KEY, ns_id.as_bytes())
                    .await
                    .map_err(|e| {
                        GuardianError::Store(format!("Falha ao persistir NamespaceId: {}", e))
                    })?;
                info!("Created new iroh-docs document: {:?}", ns_id);
                doc
            }
        };

        // --- 5. Cria Log vazio para compatibilidade com Store trait ---

        let empty_log = {
            use crate::log::{AdHocAccess, Log, LogOptions};
            let log_opts = LogOptions {
                id: Some(&db_name),
                access: AdHocAccess,
                entries: &[],
                heads: &[],
                clock: None,
                sort_fn: None,
            };
            Arc::new(RwLock::new(Log::new(
                client.clone(),
                (*identity).clone(),
                log_opts,
            )))
        };

        // --- 6. Cria a instância e sincroniza o índice ---

        let index = Arc::new(KeyValueIndex::new());
        let cached_address = addr.clone();

        let store = GuardianDBKeyValue {
            docs,
            doc_handle,
            author_id,
            access_controller,
            event_bus,
            client,
            identity,
            cached_address,
            db_name,
            cache,
            index,
            span,
            tracer,
            emitter_interface,
            empty_log,
        };

        // Sincroniza o índice local com o estado do documento iroh-docs
        match store.sync_index_from_docs().await {
            Ok(count) => {
                if count > 0 {
                    info!(
                        "KeyValue store initialized with {} entries from iroh-docs",
                        count
                    );
                }
            }
            Err(e) => {
                warn!(
                    "Failed to sync index on initialization: {:?}. Store will start empty.",
                    e
                );
            }
        }

        info!(
            "GuardianDBKeyValue initialized with iroh-docs backend (namespace={:?}, author={:?})",
            store.doc_handle.id(),
            store.author_id
        );

        Ok(store)
    }

    /// Cria um cache baseado em sled para persistir o NamespaceId
    fn create_cache(address: &dyn Address, cache_dir: &str) -> Result<Arc<dyn Datastore>> {
        use crate::cache::level_down::LevelDownCache;
        use crate::cache::{Cache, CacheMode, Options};

        let cache_options = Options {
            span: None,
            max_cache_size: Some(100 * 1024 * 1024),
            cache_mode: CacheMode::Auto,
        };

        let cache_manager = LevelDownCache::new(Some(&cache_options));
        let address_string = address.to_string();
        let parsed_address = crate::address::parse(&address_string)
            .map_err(|e| GuardianError::Store(format!("Failed to parse address: {}", e)))?;

        let boxed_datastore = cache_manager
            .load(cache_dir, &parsed_address)
            .map_err(|e| GuardianError::Store(format!("Failed to create cache: {}", e)))?;

        // Wrapper para converter Box<dyn Datastore + Send + Sync> para Arc<dyn Datastore>
        struct DatastoreWrapper {
            inner: Box<dyn Datastore + Send + Sync>,
        }

        #[async_trait::async_trait]
        impl Datastore for DatastoreWrapper {
            async fn get(&self, key: &[u8]) -> crate::guardian::error::Result<Option<Vec<u8>>> {
                self.inner.get(key).await
            }
            async fn put(&self, key: &[u8], value: &[u8]) -> crate::guardian::error::Result<()> {
                self.inner.put(key, value).await
            }
            async fn has(&self, key: &[u8]) -> crate::guardian::error::Result<bool> {
                self.inner.has(key).await
            }
            async fn delete(&self, key: &[u8]) -> crate::guardian::error::Result<()> {
                self.inner.delete(key).await
            }
            async fn query(
                &self,
                query: &crate::data_store::Query,
            ) -> crate::guardian::error::Result<crate::data_store::Results> {
                self.inner.query(query).await
            }
            async fn list_keys(
                &self,
                prefix: &[u8],
            ) -> crate::guardian::error::Result<Vec<crate::data_store::Key>> {
                self.inner.list_keys(prefix).await
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }

        Ok(Arc::new(DatastoreWrapper {
            inner: boxed_datastore,
        }))
    }
}
