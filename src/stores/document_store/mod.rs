use crate::access_control::acl_simple::SimpleAccessController;
use crate::access_control::traits::AccessController;
use crate::address::Address;
use crate::data_store::Datastore;
use crate::events::EventEmitter;
use crate::guardian::error::{GuardianError, Result};
use crate::log::identity::Identity;
use crate::log::lamport_clock::LamportClock;
use crate::p2p::EventBus;
use crate::p2p::network::client::IrohClient;
use crate::p2p::network::core::docs::WillowDocs;
use crate::stores::operation::Operation;
use crate::traits::{
    CreateDocumentDBOptions, DocumentStoreGetOptions, NewStoreOptions, Store, StoreIndex,
    TracerWrapper,
};
use bytes::Bytes;
use iroh_docs::{AuthorId, api::Doc, store::Query};
use opentelemetry::trace::{TracerProvider, noop::NoopTracerProvider};
use parking_lot::RwLock;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{Span, debug, info, instrument, warn};

/// Representa um documento genérico.
pub type Document = Value;

/// Chave usada no cache para persistir o NamespaceId do documento iroh-docs
const NAMESPACE_CACHE_KEY: &[u8] = b"_iroh_docs_doc_namespace_id";

/// Índice local em memória que espelha o estado do documento iroh-docs.
///
/// Atualizado atomicamente após cada operação de put/delete,
/// servindo como cache síncrono para queries do StoreIndex.
pub struct DocumentStoreIndex {
    index: Arc<RwLock<HashMap<String, Vec<u8>>>>,
}

impl Default for DocumentStoreIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl DocumentStoreIndex {
    pub fn new() -> Self {
        Self {
            index: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn get_value(&self, key: &str) -> Option<Vec<u8>> {
        let guard = self.index.read();
        guard.get(key).cloned()
    }

    pub fn keys(&self) -> Vec<String> {
        let guard = self.index.read();
        guard.keys().cloned().collect()
    }

    pub fn insert(&self, key: String, value: Vec<u8>) {
        let mut guard = self.index.write();
        guard.insert(key, value);
    }

    pub fn remove(&self, key: &str) {
        let mut guard = self.index.write();
        guard.remove(key);
    }

    pub fn clear_all(&self) {
        let mut guard = self.index.write();
        guard.clear();
    }

    pub fn len(&self) -> usize {
        let guard = self.index.read();
        guard.len()
    }

    pub fn is_empty(&self) -> bool {
        let guard = self.index.read();
        guard.is_empty()
    }
}

impl StoreIndex for DocumentStoreIndex {
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
    /// O índice local é atualizado diretamente após cada operação put/delete.
    fn update_index(
        &mut self,
        _log: &crate::log::Log,
        _entries: &[crate::log::entry::Entry],
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn clear(&mut self) -> std::result::Result<(), Self::Error> {
        let mut guard = self.index.write();
        guard.clear();
        Ok(())
    }
}

/// Implementação da DocumentStore para GuardianDB usando iroh-docs (WillowDocs).
///
/// Esta implementação utiliza o protocolo iroh-docs para armazenamento de documentos
/// distribuído com resolução de conflitos Last-Write-Wins (LWW), substituindo a
/// arquitetura anterior baseada em BaseStore + OpLog.
///
/// # Arquitetura
///
/// - **Backend**: iroh-docs (Willow range-based reconciliation)
/// - **Escrita**: `doc.set_bytes()` / `doc.del()` via WillowDocs
/// - **Leitura**: Índice local em memória espelhando o estado do iroh-docs
/// - **Sync**: Automático via Willow (sem gossip heads manuais)
/// - **Índice local**: HashMap em memória espelhando o estado do iroh-docs
pub struct GuardianDBDocumentStore {
    /// WillowDocs backend (iroh-docs) — substitui BaseStore
    docs: WillowDocs,
    /// Handle do documento iroh-docs para este namespace
    doc_handle: Doc,
    /// AuthorId para operações de escrita (mapeado da Identity)
    author_id: AuthorId,
    /// Controlador de acesso para validação de permissões
    access_controller: Arc<dyn AccessController>,
    /// Barramento de eventos para notificações reativas
    event_bus: Arc<EventBus>,
    /// Referência ao IrohClient (compatibilidade Store trait)
    client: Arc<IrohClient>,
    /// Identidade criptográfica da store
    identity: Arc<Identity>,
    /// Endereço da store (cached para resolver problemas de lifetime)
    cached_address: Arc<dyn Address + Send + Sync>,
    /// Nome do banco de dados
    db_name: String,
    /// Cache local (sled) — usado para persistir o NamespaceId entre recarregamentos
    cache: Arc<dyn Datastore>,
    /// Índice local em memória espelhando o estado do iroh-docs
    index: Arc<DocumentStoreIndex>,
    /// Opções de documento (marshal, unmarshal, key_extractor)
    doc_opts: CreateDocumentDBOptions,
    /// Span para tracing estruturado
    span: Span,
    /// Tracer para telemetria
    tracer: Arc<TracerWrapper>,
    /// Interface de emissão de eventos (compatibilidade Store trait)
    emitter_interface: Arc<dyn crate::events::EmitterInterface + Send + Sync>,
    /// Log vazio para compatibilidade com a trait Store (op_log())
    empty_log: Arc<RwLock<crate::log::Log>>,
    /// Optional: BlobStore para grandes attachments binários
    #[allow(dead_code)]
    blob_store: Option<Arc<crate::p2p::network::core::blobs::BlobStore>>,
}

#[async_trait::async_trait]
impl Store for GuardianDBDocumentStore {
    type Error = GuardianError;

    #[allow(deprecated)]
    fn events(&self) -> &dyn crate::events::EmitterInterface {
        self.emitter_interface.as_ref()
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        debug!("Starting DocumentStore close operation (iroh-docs backend)");
        if let Err(e) = self.docs.close_doc(&self.doc_handle).await {
            warn!("Failed to close iroh-docs document: {:?}", e);
        }
        debug!("DocumentStore close completed");
        Ok(())
    }

    fn address(&self) -> &dyn Address {
        self.cached_address.as_ref()
    }

    fn index(&self) -> Box<dyn crate::traits::StoreIndex<Error = GuardianError> + Send + Sync> {
        Box::new(DocumentStoreIndex {
            index: self.index.index.clone(),
        })
    }

    fn store_type(&self) -> &str {
        "document"
    }

    fn replication_status(&self) -> crate::stores::replicator::replication_info::ReplicationInfo {
        crate::stores::replicator::replication_info::ReplicationInfo::new()
    }

    fn cache(&self) -> Arc<dyn Datastore> {
        self.cache.clone()
    }

    async fn drop(&self) -> std::result::Result<(), Self::Error> {
        debug!("Starting DocumentStore drop operation (iroh-docs backend)");
        self.index.clear_all();
        let namespace_id = self.doc_handle.id();
        if let Err(e) = self.docs.drop_doc(namespace_id).await {
            warn!("Failed to drop iroh-docs document: {:?}", e);
        }
        if let Err(e) = self.cache.delete(NAMESPACE_CACHE_KEY).await {
            warn!("Failed to remove namespace from cache: {:?}", e);
        }
        debug!("DocumentStore drop completed");
        Ok(())
    }

    /// No-op para iroh-docs — Willow sync gerencia o carregamento automaticamente.
    async fn load(&self, _amount: usize) -> std::result::Result<(), Self::Error> {
        self.sync_index_from_docs().await?;
        Ok(())
    }

    /// No-op para iroh-docs — Willow sync substitui o exchange de heads via gossip.
    async fn sync(
        &self,
        _heads: Vec<crate::log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        self.sync_index_from_docs().await?;
        Ok(())
    }

    /// No-op para iroh-docs.
    async fn load_more_from(&self, _amount: u64, _entries: Vec<crate::log::entry::Entry>) {
        // iroh-docs gerencia seu próprio carregamento incremental.
    }

    /// No-op para iroh-docs.
    async fn load_from_snapshot(&self) -> std::result::Result<(), Self::Error> {
        self.sync_index_from_docs().await?;
        Ok(())
    }

    /// Retorna um Log vazio para compatibilidade com a trait Store.
    /// Na arquitetura iroh-docs, o OpLog não é mais utilizado.
    fn op_log(&self) -> Arc<parking_lot::RwLock<crate::log::Log>> {
        self.empty_log.clone()
    }

    fn client(&self) -> Arc<IrohClient> {
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
    ) -> std::result::Result<crate::log::entry::Entry, Self::Error> {
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

impl GuardianDBDocumentStore {
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

    /// Sincroniza o índice local com o estado atual do documento iroh-docs.
    ///
    /// Consulta todas as entradas do documento e reconstrói o índice em memória.
    pub async fn sync_index_from_docs(&self) -> Result<usize> {
        let entries = self
            .docs
            .get_many(&self.doc_handle, Query::single_latest_per_key().build())
            .await?;

        let mut count = 0;
        self.index.clear_all();

        for entry in &entries {
            let key = String::from_utf8_lossy(entry.key()).to_string();

            if entry.content_len() == 0 {
                continue;
            }

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
            "DocumentStore index synchronized from iroh-docs: {} entries loaded",
            count
        );

        Ok(count)
    }

    #[instrument(level = "debug", skip(client, identity, options))]
    pub async fn new(
        client: Arc<IrohClient>,
        identity: Arc<Identity>,
        addr: Arc<dyn Address>,
        mut options: NewStoreOptions,
    ) -> Result<Self> {
        // 1. Se opções específicas da store não forem fornecidas, usa o padrão para
        //    documentos com uma chave "_id".
        if options.store_specific_opts.is_none() {
            let default_opts = default_store_opts_for_map("_id");
            options.store_specific_opts = Some(Box::new(default_opts));
        }

        // 2. Faz o "downcast" das opções específicas para o tipo esperado.
        let specific_opts_box = options.store_specific_opts.take().ok_or_else(|| {
            GuardianError::InvalidArgument("StoreSpecificOpts is required".to_string())
        })?;
        let doc_opts = *specific_opts_box
            .downcast::<CreateDocumentDBOptions>()
            .map_err(|_| {
                GuardianError::InvalidArgument(
                    "Tipo inválido fornecido para opts.StoreSpecificOpts".to_string(),
                )
            })?;

        // --- 3. Inicializa iroh-docs ---

        if !client.has_docs_client().await {
            client.init_docs().await.map_err(|e| {
                GuardianError::Store(format!("Falha ao inicializar iroh-docs: {}", e))
            })?;
        }

        let mut docs = client.docs_client().await.ok_or_else(|| {
            GuardianError::Store("iroh-docs não disponível após inicialização".to_string())
        })?;

        // --- 4. Obtém AuthorId ---

        let author_id = docs.get_or_init_author().await.map_err(|e| {
            GuardianError::Store(format!("Falha ao inicializar author iroh-docs: {}", e))
        })?;

        // --- 5. Configura componentes ---

        let db_name = addr.get_path().to_string();
        let span = tracing::info_span!("document_store", address = %addr.to_string());

        let event_bus = Arc::new(options.event_bus.unwrap_or_default());

        let access_controller = options.access_controller.unwrap_or_else(|| {
            let mut default_access = HashMap::new();
            default_access.insert("write".to_string(), vec!["*".to_string()]);
            Arc::new(SimpleAccessController::new(Some(default_access))) as Arc<dyn AccessController>
        });

        let tracer = options.tracer.unwrap_or_else(|| {
            Arc::new(TracerWrapper::Noop(
                NoopTracerProvider::new().tracer("berty.guardian-db"),
            ))
        });

        let cache: Arc<dyn Datastore> = if let Some(cache) = options.cache {
            cache
        } else {
            let cache_dir = if options.directory.is_empty() {
                format!("./GuardianDB/{}/cache", addr)
            } else {
                format!("{}/cache", options.directory)
            };
            Self::create_cache(addr.as_ref(), &cache_dir)?
        };

        let emitter_interface: Arc<dyn crate::events::EmitterInterface + Send + Sync> =
            Arc::new(EventEmitter::new());

        // --- 6. Cria ou abre documento iroh-docs ---

        let doc_handle = match cache.get(NAMESPACE_CACHE_KEY).await {
            Ok(Some(namespace_bytes)) if namespace_bytes.len() == 32 => {
                let mut ns_bytes = [0u8; 32];
                ns_bytes.copy_from_slice(&namespace_bytes);
                let namespace_id = iroh_docs::NamespaceId::from(ns_bytes);

                match docs.open_doc(namespace_id).await? {
                    Some(doc) => {
                        info!("Reopened existing iroh-docs document: {:?}", namespace_id);
                        doc
                    }
                    None => {
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

        // --- 7. Cria Log vazio para compatibilidade com Store trait ---

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

        // --- 8. Inicializa BlobStore (opcional, para attachments grandes) ---

        let blob_store = client.blobs_client().await.map(Arc::new);

        // --- 9. Cria a instância e sincroniza o índice ---

        let index = Arc::new(DocumentStoreIndex::new());
        // Address trait already requires Send + Sync, so this coercion is safe
        let cached_address: Arc<dyn Address + Send + Sync> = addr as Arc<dyn Address + Send + Sync>;

        let store = GuardianDBDocumentStore {
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
            doc_opts,
            span,
            tracer,
            emitter_interface,
            empty_log,
            blob_store,
        };

        // Sincroniza o índice local com o estado do documento iroh-docs
        match store.sync_index_from_docs().await {
            Ok(count) => {
                if count > 0 {
                    info!(
                        "DocumentStore initialized with {} entries from iroh-docs",
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
            "GuardianDBDocumentStore initialized with iroh-docs backend (namespace={:?}, author={:?})",
            store.doc_handle.id(),
            store.author_id
        );

        Ok(store)
    }

    #[instrument(level = "debug", skip(self, opts))]
    pub async fn get(
        &self,
        key: &str,
        opts: Option<DocumentStoreGetOptions>,
    ) -> Result<Vec<Document>> {
        let _entered = self.span.enter();
        let opts = opts.unwrap_or_default();

        let has_multiple_terms = key.contains(' ');
        let mut key_for_search = key.to_string();

        if has_multiple_terms {
            key_for_search = key_for_search.replace('.', " ");
        }
        if opts.case_insensitive {
            key_for_search = key_for_search.to_lowercase();
        }

        let mut documents: Vec<Document> = Vec::new();

        for index_key in self.index.keys() {
            let mut index_key_for_search = index_key.clone();

            if opts.case_insensitive {
                index_key_for_search = index_key_for_search.to_lowercase();
                if has_multiple_terms {
                    index_key_for_search = index_key_for_search.replace('.', " ");
                }
            }

            let matches = if opts.partial_matches {
                index_key_for_search.contains(&key_for_search)
            } else {
                index_key_for_search == key_for_search
            };

            if !matches {
                continue;
            }

            if let Some(value_bytes) = self.index.get_value(&index_key) {
                let doc: Document = serde_json::from_slice(&value_bytes).map_err(|e| {
                    GuardianError::Serialization(format!(
                        "Impossível desserializar o valor para a chave {}: {}",
                        index_key, e
                    ))
                })?;
                documents.push(doc);
            }
        }

        Ok(documents)
    }

    #[instrument(level = "debug", skip(self, document))]
    pub async fn put(&mut self, document: Document) -> Result<Operation> {
        let _entered = self.span.enter();

        let key = (self.doc_opts.key_extractor)(&document)?;
        let data = (self.doc_opts.marshal)(&document)?;

        // Escreve diretamente no iroh-docs (Willow handles sync)
        self.docs
            .set_bytes(
                &self.doc_handle,
                self.author_id,
                Bytes::from(key.clone().into_bytes()),
                Bytes::from(data.clone()),
            )
            .await
            .map_err(|e| {
                GuardianError::Store(format!(
                    "Erro ao escrever chave '{}' no iroh-docs: {}",
                    key, e
                ))
            })?;

        // Atualiza o índice local imediatamente
        self.index.insert(key.clone(), data.clone());

        debug!("PUT key='{}' ({} bytes) via iroh-docs", key, data.len());

        Ok(Operation::new(Some(key), "PUT".to_string(), Some(data)))
    }

    #[instrument(level = "debug", skip(self))]
    pub async fn delete(&mut self, document_id: &str) -> Result<Operation> {
        let _entered = self.span.enter();

        // Verifica se a entrada existe no índice local
        if self.index.get_value(document_id).is_none() {
            return Err(GuardianError::NotFound(format!(
                "Nenhuma entrada com a chave '{}' na base de dados",
                document_id
            )));
        }

        // Remove do iroh-docs (Willow handles sync)
        let deleted = self
            .docs
            .del(
                &self.doc_handle,
                self.author_id,
                Bytes::from(document_id.as_bytes().to_vec()),
            )
            .await
            .map_err(|e| {
                GuardianError::Store(format!(
                    "Erro ao deletar chave '{}' no iroh-docs: {}",
                    document_id, e
                ))
            })?;

        // Atualiza o índice local imediatamente
        self.index.remove(document_id);

        debug!(
            "DEL key='{}' ({} entries removed) via iroh-docs",
            document_id, deleted
        );

        Ok(Operation::new(
            Some(document_id.to_string()),
            "DEL".to_string(),
            None,
        ))
    }

    #[instrument(level = "debug", skip(self, documents))]
    pub async fn put_batch(&mut self, documents: Vec<Document>) -> Result<Vec<Operation>> {
        if documents.is_empty() {
            return Err(GuardianError::InvalidArgument(
                "Nada para adicionar à store".to_string(),
            ));
        }

        let mut operations = Vec::new();

        for doc in documents {
            let op = self.put(doc).await?;
            operations.push(op);
        }

        Ok(operations)
    }

    #[instrument(level = "debug", skip(self, documents))]
    pub async fn put_all(&mut self, documents: Vec<Document>) -> Result<Operation> {
        if documents.is_empty() {
            return Err(GuardianError::InvalidArgument(
                "Nada para adicionar à store".to_string(),
            ));
        }

        let mut to_add: Vec<(String, Vec<u8>)> = Vec::new();

        for doc in documents {
            let key = (self.doc_opts.key_extractor)(&doc).map_err(|_| {
                GuardianError::InvalidArgument(
                    "Um dos documentos fornecidos não possui chave de índice".to_string(),
                )
            })?;

            let data = (self.doc_opts.marshal)(&doc).map_err(|_| {
                GuardianError::Serialization(
                    "Não foi possível serializar um dos documentos fornecidos".to_string(),
                )
            })?;

            to_add.push((key, data));
        }

        // Cada documento é um set_bytes individual (iroh-docs não tem batch API)
        for (key, data) in &to_add {
            self.docs
                .set_bytes(
                    &self.doc_handle,
                    self.author_id,
                    Bytes::from(key.clone().into_bytes()),
                    Bytes::from(data.clone()),
                )
                .await
                .map_err(|e| {
                    GuardianError::Store(format!(
                        "Erro ao escrever chave '{}' no iroh-docs: {}",
                        key, e
                    ))
                })?;

            // Atualiza o índice local imediatamente
            self.index.insert(key.clone(), data.clone());
        }

        debug!("PUTALL {} documents via iroh-docs", to_add.len());

        // Retorna uma Operation representando o batch
        let first_key = to_add.first().map(|(k, _)| k.clone());
        Ok(Operation::new(first_key, "PUTALL".to_string(), None))
    }

    #[instrument(level = "debug", skip(self, filter))]
    pub fn query<F>(&self, mut filter: F) -> Result<Vec<Document>>
    where
        F: FnMut(&Document) -> Result<bool>,
    {
        let mut results: Vec<Document> = Vec::new();

        for index_key in self.index.keys() {
            if let Some(doc_bytes) = self.index.get_value(&index_key) {
                let doc: Document = serde_json::from_slice(&doc_bytes).map_err(|e| {
                    GuardianError::Serialization(format!(
                        "Não foi possível desserializar o documento: {}",
                        e
                    ))
                })?;

                if filter(&doc)? {
                    results.push(doc);
                }
            }
        }

        Ok(results)
    }

    pub fn store_type(&self) -> &'static str {
        "document"
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

/// Retorna uma closure que extrai um campo de um `serde_json::Value::Object`.
///
/// A closure retornada captura o `key_field` para uso posterior.
pub fn map_key_extractor(key_field: String) -> impl Fn(&Document) -> Result<String> {
    move |doc: &Document| {
        // Assegura que o documento é um objeto JSON (mapa)
        let obj = doc.as_object().ok_or_else(|| {
            GuardianError::InvalidArgument(
                "A entrada precisa ser um objeto JSON (map[string]interface{{}})".to_string(),
            )
        })?;

        // Procura pelo campo chave no objeto
        let value = obj.get(&key_field).ok_or_else(|| {
            GuardianError::NotFound(format!(
                "Faltando valor para o campo `{}` na entrada",
                key_field
            ))
        })?;

        // Assegura que o valor encontrado é uma string
        let key = value.as_str().ok_or_else(|| {
            GuardianError::InvalidArgument(format!(
                "O valor para o campo `{}` não é uma string",
                key_field
            ))
        })?;

        // Valida que a chave não está vazia
        if key.is_empty() {
            return Err(GuardianError::InvalidArgument(format!(
                "O campo `{}` não pode ser uma string vazia",
                key_field
            )));
        }

        Ok(key.to_string())
    }
}

/// Cria um conjunto de opções padrão para uma store que lida com documentos
/// baseados em mapas (JSON Objects), usando um campo específico como chave.
pub fn default_store_opts_for_map(key_field: &str) -> CreateDocumentDBOptions {
    CreateDocumentDBOptions {
        marshal: Arc::new(|doc: &Document| serde_json::to_vec(doc).map_err(GuardianError::from)),
        unmarshal: Arc::new(|bytes: &[u8]| {
            serde_json::from_slice(bytes).map_err(GuardianError::from)
        }),
        // Usa a função de ordem superior para criar a closure extratora de chave
        key_extractor: Arc::new(map_key_extractor(key_field.to_string())),

        item_factory: Arc::new(|| Value::Object(Map::new())),
    }
}
