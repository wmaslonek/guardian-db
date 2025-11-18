use crate::address::Address;
use crate::data_store::Datastore;
use crate::error::{GuardianError, Result};
use crate::ipfs_core_api::client::IpfsClient;
use crate::ipfs_log::identity::Identity;
use crate::p2p::events::EventBus;
use crate::stores::base_store::base_store::BaseStore;
use crate::stores::document_store::index::DocumentIndex;
use crate::stores::operation::{operation, operation::Operation};
use crate::traits::{
    CreateDocumentDBOptions, DocumentStoreGetOptions, NewStoreOptions, Store, TracerWrapper,
};
use serde_json::{Map, Value};
use std::sync::Arc;
use tracing::{Span, instrument, warn};

/// Representa um documento genérico.
pub type Document = Value;

/// Estrutura principal da DocumentStore.
pub struct GuardianDBDocumentStore {
    // Incorpora a lógica da BaseStore. Em Rust, a composição é preferível à herança.
    base_store: Arc<BaseStore>,
    // Opções específicas para a manipulação de documentos.
    doc_opts: CreateDocumentDBOptions,
    // Índice específico para documentos - usado pelos métodos específicos da DocumentStore
    doc_index: Arc<DocumentIndex>,
    // Cache dos valores para resolver problemas de lifetime
    cached_address: Arc<dyn Address + Send + Sync>,
    span: Span,
    cached_replicator: Option<Arc<crate::stores::replicator::replicator::Replicator>>,
}

// Implementação da trait Store, delegando para base_store
#[async_trait::async_trait]
impl Store for GuardianDBDocumentStore {
    type Error = GuardianError;

    #[allow(deprecated)]
    fn events(&self) -> &dyn crate::events::EmitterInterface {
        self.base_store.events()
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        self.base_store.close().await
    }

    fn address(&self) -> &dyn Address {
        // Usa o valor em cache para evitar problemas de lifetime
        self.cached_address.as_ref()
    }

    fn index(&self) -> Box<dyn crate::traits::StoreIndex<Error = GuardianError> + Send + Sync> {
        // Usa o DocumentIndex local que foi criado especificamente para esta DocumentStore
        // Este índice mantém compatibilidade com a trait Store
        let default_opts = Arc::new(default_store_opts_for_map("_id"));
        Box::new(DocumentIndex::new(default_opts))
    }

    fn store_type(&self) -> &str {
        "docstore"
    }

    fn replication_status(&self) -> crate::stores::replicator::replication_info::ReplicationInfo {
        self.base_store.replication_status()
    }

    fn replicator(&self) -> Option<Arc<crate::stores::replicator::replicator::Replicator>> {
        // Usa o valor em cache para evitar problemas de lifetime
        self.cached_replicator.clone()
    }

    fn cache(&self) -> Arc<dyn Datastore> {
        self.base_store.cache()
    }

    async fn drop(&mut self) -> std::result::Result<(), Self::Error> {
        // O método drop não é assíncrono, então não podemos usar await
        // ***TODO: Implementar funcionalidade apropriada quando BaseStore for corrigido
        Ok(())
    }

    async fn load(&mut self, amount: usize) -> std::result::Result<(), Self::Error> {
        // BaseStore espera Option<isize>, então convertemos
        self.base_store.load(Some(amount as isize)).await
    }

    async fn sync(
        &mut self,
        heads: Vec<crate::ipfs_log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        self.base_store.sync(heads).await
    }

    async fn load_more_from(&mut self, _amount: u64, entries: Vec<crate::ipfs_log::entry::Entry>) {
        // ***BaseStore.load_more_from não é async e tem assinatura diferente
        let _ = self.base_store.load_more_from(entries);
    }

    async fn load_from_snapshot(&mut self) -> std::result::Result<(), Self::Error> {
        // ***BaseStore refatorada agora permite implementação adequada
        // Por enquanto, delegamos para BaseStore ou implementamos lógica básica
        Ok(())
    }

    fn op_log(&self) -> Arc<parking_lot::RwLock<crate::ipfs_log::log::Log>> {
        self.base_store.op_log()
    }

    fn ipfs(&self) -> Arc<IpfsClient> {
        self.base_store.ipfs()
    }

    fn db_name(&self) -> &str {
        self.base_store.db_name()
    }

    fn identity(&self) -> &Identity {
        self.base_store.identity()
    }

    fn access_controller(&self) -> &dyn crate::access_controller::traits::AccessController {
        self.base_store.access_controller()
    }

    async fn add_operation(
        &mut self,
        op: Operation,
        on_progress_callback: Option<tokio::sync::mpsc::Sender<crate::ipfs_log::entry::Entry>>,
    ) -> std::result::Result<crate::ipfs_log::entry::Entry, Self::Error> {
        self.base_store
            .add_operation(op, on_progress_callback)
            .await
    }

    fn span(&self) -> Arc<tracing::Span> {
        Arc::new(self.span.clone())
    }

    fn tracer(&self) -> Arc<TracerWrapper> {
        self.base_store.tracer()
    }

    fn event_bus(&self) -> Arc<EventBus> {
        Arc::new(EventBus::new())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl GuardianDBDocumentStore {
    /// Acessa o BaseStore interno para operações de sync
    pub fn basestore(&self) -> &BaseStore {
        &self.base_store
    }

    /// Retorna uma referência ao span de tracing para instrumentação
    pub fn span(&self) -> &Span {
        &self.span
    }

    #[instrument(level = "debug", skip(ipfs, identity, options))]
    pub async fn new(
        ipfs: Arc<IpfsClient>,
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
        //    O `take()` remove o valor da Option, permitindo-nos tomar posse do Box.
        let specific_opts_box = options.store_specific_opts.take().ok_or_else(|| {
            GuardianError::InvalidArgument("StoreSpecificOpts is required".to_string())
        })?;
        let doc_opts_box = specific_opts_box
            .downcast::<CreateDocumentDBOptions>()
            .map_err(|_| {
                GuardianError::InvalidArgument(
                    "Tipo inválido fornecido para opts.StoreSpecificOpts".to_string(),
                )
            })?;

        // Converte Box para Arc para compatibilidade com DocumentIndex
        let doc_opts = Arc::new(*doc_opts_box);

        // 3. Clona as opções (que estão dentro de um Arc) para a closure da fábrica de índice.
        let doc_opts_for_index = doc_opts.clone();

        // 4. Define a fábrica que a BaseStore usará para criar o índice.
        options.index = Some(Box::new(move |_data: &[u8]| {
            // A closure retorna o índice concreto, encapsulado em um Box<dyn ...> para ser um trait object.
            Box::new(DocumentIndex::new(doc_opts_for_index.clone()))
        }));

        // 5. Inicializa a BaseStore
        //    Esta chamada assíncrona lida com toda a configuração do oplog, etc.
        let base_store = BaseStore::new(ipfs, identity, addr, Some(options))
            .await
            .map_err(|e| {
                GuardianError::Store(format!(
                    "Não foi possível inicializar a document store: {}",
                    e
                ))
            })?;

        // 6. Constrói e retorna a instância final da GuardianDBDocumentStore.
        let doc_index = Arc::new(DocumentIndex::new(doc_opts.clone()));

        // Cache dos valores para resolver problemas de lifetime
        let cached_address = base_store.address();
        let cached_replicator = base_store.replicator();

        // Create a tracing span for the document store
        let span = tracing::info_span!("document_store", address = %cached_address.to_string());

        let store = GuardianDBDocumentStore {
            base_store,
            doc_opts: (*doc_opts).clone(),
            doc_index,
            cached_address,
            span,
            cached_replicator,
        };

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

        // Prepara a chave de busca de acordo com as opções.
        let has_multiple_terms = key.contains(' ');
        let mut key_for_search = key.to_string();

        if has_multiple_terms {
            key_for_search = key_for_search.replace('.', " ");
        }
        if opts.case_insensitive {
            key_for_search = key_for_search.to_lowercase();
        }

        // Usa diretamente o DocumentIndex armazenado na struct
        let doc_index = &self.doc_index;

        let mut documents: Vec<Document> = Vec::new();

        for index_key in doc_index.keys() {
            let mut index_key_for_search = index_key.clone();

            // Normaliza a chave do índice para a busca, se necessário.
            if opts.case_insensitive {
                index_key_for_search = index_key_for_search.to_lowercase();
                if has_multiple_terms {
                    index_key_for_search = index_key_for_search.replace('.', " ");
                }
            }

            // Verifica a correspondência da chave.
            let matches = if opts.partial_matches {
                index_key_for_search.contains(&key_for_search)
            } else {
                index_key_for_search == key_for_search
            };

            if !matches {
                continue;
            }

            // Obtém o valor usando o DocumentIndex diretamente
            if let Some(value_bytes) = doc_index.get_bytes(&index_key) {
                let doc: Document = serde_json::from_slice(&value_bytes).map_err(|e| {
                    GuardianError::Serialization(format!(
                        "Impossível desserializar o valor para a chave {}: {}",
                        index_key, e
                    ))
                })?;
                documents.push(doc);
            } else {
                // Pode ser um erro ou apenas um log, dependendo da consistência esperada do índice.
                eprintln!(
                    "Aviso: chave '{}' encontrada no conjunto de chaves do índice, mas sem valor correspondente.",
                    index_key
                );
            }
        }

        Ok(documents)
    }

    #[instrument(level = "debug", skip(self, document))]
    pub async fn put(&mut self, document: Document) -> Result<Operation> {
        let _entered = self.span.enter();
        // Extrai a chave e serializa o documento usando as funções fornecidas nas opções.
        let key = (self.doc_opts.key_extractor)(&document)?;
        let data = (self.doc_opts.marshal)(&document)?;

        // Cria a operação PUT.
        let op = Operation::new(Some(key), "PUT".to_string(), Some(data));

        // Adiciona a operação ao log da store (oplog).
        let entry = self.base_store.add_operation(op, None).await?;

        // Analisa o 'entry' retornado para criar um objeto Operation.
        let parsed_op = operation::parse_operation(entry)?;

        Ok(parsed_op)
    }

    #[instrument(level = "debug", skip(self))]
    pub async fn delete(&mut self, document_id: &str) -> Result<Operation> {
        let _entered = self.span.enter();
        // Usa diretamente o DocumentIndex armazenado na struct
        let doc_index = &self.doc_index;

        // Verifica se a entrada existe antes de deletar.
        if doc_index.get_bytes(document_id).is_none() {
            return Err(GuardianError::NotFound(format!(
                "Nenhuma entrada com a chave '{}' na base de dados",
                document_id
            )));
        }

        // Cria a operação DEL. O payload é None.
        let op = Operation::new(Some(document_id.to_string()), "DEL".to_string(), None);

        // Adiciona a operação DEL ao log.
        let entry = self.base_store.add_operation(op, None).await?;

        // Analisa o 'entry' retornado.
        let parsed_op = operation::parse_operation(entry)?;

        Ok(parsed_op)
    }

    #[instrument(level = "debug", skip(self, documents))]
    pub async fn put_batch(&mut self, documents: Vec<Document>) -> Result<Vec<Operation>> {
        if documents.is_empty() {
            return Err(GuardianError::InvalidArgument(
                "Nada para adicionar à store".to_string(),
            ));
        }

        let mut operations = Vec::new();

        // Itera sobre cada documento, chamando a função `put` individualmente.
        // Isso cria uma operação para cada documento no log.
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

        // Agrega todos os documentos em um único vetor para uma operação em lote.
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

        // Cria uma única operação "PUTALL" com todos os documentos.
        let op = Operation::new_with_documents(None, "PUTALL".to_string(), to_add);

        let entry = self.base_store.add_operation(op, None).await?;

        let parsed_op = operation::parse_operation(entry)?;
        Ok(parsed_op)
    }

    #[instrument(level = "debug", skip(self, filter))]
    pub fn query<F>(&self, mut filter: F) -> Result<Vec<Document>>
    where
        // Aceita qualquer closure que possa ser chamado múltiplas vezes,
        // recebe uma referência a um Documento e retorna um Result<bool>.
        F: FnMut(&Document) -> Result<bool>,
    {
        // Usa diretamente o DocumentIndex armazenado na struct
        let doc_index = &self.doc_index;

        let mut results: Vec<Document> = Vec::new();

        for index_key in doc_index.keys() {
            if let Some(doc_bytes) = doc_index.get_bytes(&index_key) {
                let doc: Document = serde_json::from_slice(&doc_bytes).map_err(|e| {
                    GuardianError::Serialization(format!(
                        "Não foi possível desserializar o documento: {}",
                        e
                    ))
                })?;

                // Chama a closure do filtro. O `?` propaga o erro se o filtro falhar.
                if filter(&doc)? {
                    results.push(doc);
                }
            }
        }

        Ok(results)
    }

    pub fn store_type(&self) -> &'static str {
        "docstore"
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
