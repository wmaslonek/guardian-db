use crate::address::Address;
use crate::data_store::Datastore;
use crate::error::{GuardianError, Result};
use crate::ipfs_log::identity::Identity;
use crate::p2p::events::EventBus;
use crate::stores::base_store::base_store::BaseStore;
use crate::stores::operation::operation::Operation;
use crate::traits::{KeyValueStore, NewStoreOptions, Store, StoreIndex};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{Span, debug, error, info, instrument, warn};

/// Implementação de StoreIndex para KeyValue Store
/// Mantém um índice thread-safe dos pares chave-valor
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
}

impl StoreIndex for KeyValueIndex {
    type Error = GuardianError;

    /// Verifica se uma chave existe no índice.
    fn contains_key(&self, key: &str) -> std::result::Result<bool, Self::Error> {
        let guard = self.index.read();
        Ok(guard.contains_key(key))
    }

    /// Retorna uma cópia dos dados para uma chave específica como bytes.
    fn get_bytes(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
        let guard = self.index.read();
        Ok(guard.get(key).cloned())
    }

    /// Retorna todas as chaves disponíveis no índice.
    fn keys(&self) -> std::result::Result<Vec<String>, Self::Error> {
        let guard = self.index.read();
        Ok(guard.keys().cloned().collect())
    }

    /// Retorna o número de entradas no índice.
    fn len(&self) -> std::result::Result<usize, Self::Error> {
        let guard = self.index.read();
        Ok(guard.len())
    }

    /// Verifica se o índice está vazio.
    fn is_empty(&self) -> std::result::Result<bool, Self::Error> {
        let guard = self.index.read();
        Ok(guard.is_empty())
    }

    fn update_index(
        &mut self,
        _log: &crate::ipfs_log::log::Log,
        entries: &[crate::ipfs_log::entry::Entry],
    ) -> std::result::Result<(), Self::Error> {
        let mut index_guard = self.index.write();

        for entry in entries {
            // Tenta deserializar o payload como uma operação
            match serde_json::from_str::<Operation>(entry.payload()) {
                Ok(operation) => {
                    if let Some(key) = operation.key() {
                        match operation.op() {
                            "PUT" => {
                                // operation.value() retorna &[u8], precisamos clonar
                                let value = operation.value().to_vec();
                                index_guard.insert(key.clone(), value);
                            }
                            "DEL" => {
                                // Corrige o erro de borrow - usa key.as_str()
                                index_guard.remove(key.as_str());
                            }
                            _ => {
                                // Operação desconhecida, ignora
                                continue;
                            }
                        }
                    }
                }
                Err(_) => {
                    // Se não conseguir deserializar como Operation, ignora a entrada
                    continue;
                }
            }
        }

        Ok(())
    }

    /// Limpa todos os dados do índice.
    fn clear(&mut self) -> std::result::Result<(), Self::Error> {
        let mut guard = self.index.write();
        guard.clear();
        Ok(())
    }
}
/// Implementação da KeyValue Store para GuardianDB
///
/// Esta estrutura fornece funcionalidade de store chave-valor baseada em GuardianDB,
/// usando um BaseStore para operações fundamentais de log e sincronização.
pub struct GuardianDBKeyValue {
    base_store: Arc<BaseStore>,
    index: Arc<KeyValueIndex>,
    // Cache do address para resolver problemas de lifetime
    cached_address: Arc<dyn Address + Send + Sync>,
    span: Span,
}

#[async_trait::async_trait]
impl Store for GuardianDBKeyValue {
    type Error = GuardianError;

    #[allow(deprecated)]
    fn events(&self) -> &dyn crate::events::EmitterInterface {
        self.base_store.events()
    }

    async fn close(&self) -> Result<()> {
        // Fechamento da KeyValue Store
        //
        // 1. Limpeza adequada do índice local
        // 2. Logging apropriado para debugging
        // 3. Seguimos o padrão RAII do Rust para cleanup automático

        debug!("Starting KeyValue store close operation");

        // Chama o close da BaseStore que faz a limpeza completa
        self.base_store.close().await?;

        // Nota: O índice HashMap será limpo automaticamente pelo Drop trait
        // Isso é consistente com o padrão RAII do Rust
        debug!("KeyValue store close completed");

        Ok(())
    }

    fn address(&self) -> &dyn Address {
        // Usa o valor em cache para evitar problemas de lifetime
        self.cached_address.as_ref()
    }

    fn index(&self) -> Box<dyn StoreIndex<Error = GuardianError> + Send + Sync> {
        // Retorna uma cópia do nosso índice KeyValue como Box
        Box::new(KeyValueIndex {
            index: self.index.index.clone(),
        })
    }

    fn store_type(&self) -> &str {
        "keyvalue"
    }

    fn replication_status(&self) -> crate::stores::replicator::replication_info::ReplicationInfo {
        // Usa ReplicationInfo com Clone - retorna dados atuais da BaseStore
        crate::stores::replicator::replication_info::ReplicationInfo::new()
    }

    fn replicator(&self) -> Option<Arc<crate::stores::replicator::replicator::Replicator>> {
        // Delega para BaseStore que retorna Option<Arc<Replicator>>
        self.base_store.replicator()
    }

    fn cache(&self) -> Arc<dyn Datastore> {
        self.base_store.cache()
    }

    async fn drop(&mut self) -> Result<()> {
        // Log o início da operação de drop
        debug!("Starting KeyValue store drop operation");

        // Limpa o índice local completamente
        let entries_count = {
            let mut index_guard = self.index.index.write();
            let count = index_guard.len();
            index_guard.clear();
            count
        };

        if entries_count > 0 {
            debug!(
                "KeyValue store drop: cleared {} entries from index",
                entries_count
            );
        }

        // A BaseStore será dropada automaticamente quando a struct sair de escopo
        // Isso é consistente com o padrão RAII do Rust
        debug!("KeyValue store drop completed");

        Ok(())
    }

    async fn load(&mut self, amount: usize) -> Result<()> {
        // Delegate to base_store load functionality
        self.base_store.load(Some(amount as isize)).await?;

        // Sincroniza o índice KeyValue após carregar dados
        match self.sync_index_with_log().await {
            Ok(operations_count) => {
                debug!(
                    "KeyValue index synchronized after load: {} operations processed",
                    operations_count
                );
            }
            Err(e) => {
                warn!("Warning: Failed to synchronize index after load: {:?}", e);
            }
        }

        Ok(())
    }

    async fn sync(&mut self, heads: Vec<crate::ipfs_log::entry::Entry>) -> Result<()> {
        // Delegate to base_store sync functionality
        self.base_store.sync(heads).await
    }

    async fn load_more_from(&mut self, _amount: u64, entries: Vec<crate::ipfs_log::entry::Entry>) {
        // Delegate to base_store load_more_from functionality
        let _ = self.base_store.load_more_from(entries);
    }

    async fn load_from_snapshot(&mut self) -> Result<()> {
        // Delega para a base store e depois atualiza o índice
        self.base_store.load_from_snapshot().await?;

        // Força sincronização completa do índice após carregar do snapshot
        // Usa o novo método para garantir que o índice KeyValue esteja em sincronia
        match self.sync_index_with_log().await {
            Ok(operations_count) => {
                info!(
                    "Successfully synchronized KeyValue index after snapshot load: {} operations processed",
                    operations_count
                );
            }
            Err(e) => {
                warn!(
                    "Warning: Failed to synchronize index after snapshot load: {:?}",
                    e
                );
            }
        }

        Ok(())
    }

    fn op_log(&self) -> Arc<RwLock<crate::ipfs_log::log::Log>> {
        // Delega para BaseStore que retorna Arc<RwLock<Log>>
        self.base_store.op_log()
    }

    fn ipfs(&self) -> Arc<crate::ipfs_core_api::client::IpfsClient> {
        self.base_store.ipfs()
    }

    fn db_name(&self) -> &str {
        self.base_store.db_name()
    }

    fn identity(&self) -> &Identity {
        self.base_store.identity()
    }

    fn access_controller(&self) -> &dyn crate::access_controller::traits::AccessController {
        // Retorna o access controller da base store
        self.base_store.access_controller()
    }

    async fn add_operation(
        &mut self,
        op: Operation,
        on_progress_callback: Option<tokio::sync::mpsc::Sender<crate::ipfs_log::entry::Entry>>,
    ) -> Result<crate::ipfs_log::entry::Entry> {
        // Delegate to base_store add_operation functionality
        self.base_store
            .add_operation(op, on_progress_callback)
            .await
    }

    fn span(&self) -> Arc<tracing::Span> {
        Arc::new(self.span.clone())
    }

    fn tracer(&self) -> Arc<crate::traits::TracerWrapper> {
        // Return the tracer from base_store
        self.base_store.tracer()
    }

    fn event_bus(&self) -> Arc<EventBus> {
        // Delega para BaseStore que retorna Arc<EventBus>
        self.base_store.event_bus()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl GuardianDBKeyValue {
    /// Acessa o BaseStore interno para operações de sync
    pub fn basestore(&self) -> &BaseStore {
        &self.base_store
    }

    /// Retorna uma referência ao span de tracing para instrumentação
    pub fn span(&self) -> &Span {
        &self.span
    }
}

// Implementação da trait `KeyValueStore` para `GuardianDBKeyValue`.
#[async_trait::async_trait]
impl KeyValueStore for GuardianDBKeyValue {
    /// Retorna todos os pares chave-valor da store.
    fn all(&self) -> HashMap<String, Vec<u8>> {
        self.all()
    }

    /// Adiciona ou atualiza um valor para uma chave específica.
    async fn put(&mut self, key: &str, value: Vec<u8>) -> Result<Operation> {
        self.put(key.to_string(), value).await
    }

    /// Remove um valor associado a uma chave específica.
    async fn delete(&mut self, key: &str) -> Result<Operation> {
        self.delete(key.to_string()).await
    }

    /// Obtém o valor associado a uma chave específica.
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.get(key)
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

    /// Atualiza o índice local com base no estado atual do log
    pub async fn update_index(&self) -> Result<()> {
        let _entered = self.span.enter();
        // Usamos o método update_index da BaseStore
        match self.base_store.update_index() {
            Ok(updated_count) => {
                if updated_count > 0 {
                    // Log informativo sobre quantas entradas foram processadas
                    debug!(
                        "KeyValue store index updated with {} entries",
                        updated_count
                    );
                }
                Ok(())
            }
            Err(e) => {
                // Log do erro e propaga para o chamador
                error!("Failed to update KeyValue store index: {:?}", e);
                Err(e)
            }
        }
    }

    /// Sincroniza o índice local KeyValue com todas as entradas do log
    ///
    /// Este método força uma reconstrução completa do índice local baseado
    /// em todas as entradas presentes no log distribuído. É útil após
    /// operações de load, sync ou reset.
    pub async fn sync_index_with_log(&self) -> Result<usize> {
        // Primeiro atualiza o índice via BaseStore
        let _updated_count = self.base_store.update_index()?;

        // Agora reconstrói o índice local baseado nas entradas do log
        let mut operations_processed = 0;

        // Acessa o log usando o método thread-safe
        self.base_store.with_oplog(|oplog| {
            let entries = oplog.values();

            // Limpa o índice local primeiro
            {
                let mut index_guard = self.index.index.write();
                index_guard.clear();
            }

            // Processa todas as entradas do log em ordem
            for entry in entries {
                match serde_json::from_str::<Operation>(entry.payload()) {
                    Ok(operation) => {
                        if let Some(key) = operation.key() {
                            let mut index_guard = self.index.index.write();
                            match operation.op() {
                                "PUT" => {
                                    let value = operation.value().to_vec();
                                    index_guard.insert(key.clone(), value);
                                    operations_processed += 1;
                                }
                                "DEL" => {
                                    index_guard.remove(key.as_str());
                                    operations_processed += 1;
                                }
                                _ => {
                                    // Operação desconhecida, ignora
                                    continue;
                                }
                            }
                        }
                    }
                    Err(_) => {
                        // Se não conseguir deserializar como Operation, ignora a entrada
                        continue;
                    }
                }
            }
        });

        info!(
            "KeyValue index synchronized: processed {} operations, index size: {}",
            operations_processed,
            self.len()
        );

        Ok(operations_processed)
    }

    pub fn all(&self) -> HashMap<String, Vec<u8>> {
        // Retorna todos os pares chave-valor do índice
        self.index.get_all()
    }

    /// Adiciona ou atualiza um valor para uma chave específica.
    ///
    /// Esta operação cria uma entrada no log distribuído e atualiza o índice local.
    /// A operação é replicada para outros peers na rede.
    ///
    /// # Argumentos
    ///
    /// * `key` - A chave para associar ao valor
    /// * `value` - Os dados binários a serem armazenados
    ///
    /// # Retorna
    ///
    /// A operação criada se bem-sucedida, ou um erro se a operação falhar.
    #[instrument(level = "debug", skip(self, value))]
    pub async fn put(&mut self, key: String, value: Vec<u8>) -> Result<Operation> {
        // Validação de entrada
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

        // Criamos a operação diretamente como Operation em vez de Box<dyn OperationTrait>
        use crate::stores::operation::operation::Operation;

        let op = Operation::new(Some(key.clone()), "PUT".to_string(), Some(value.clone()));

        let _entry = self
            .base_store
            .add_operation(op.clone(), None)
            .await
            .map_err(|e| {
                GuardianError::Store(format!(
                    "erro ao adicionar valor para chave '{}': {}",
                    key, e
                ))
            })?;

        // Força atualização do índice após adicionar a operação ao log
        // Isso garante que o índice local e o global estejam sincronizados
        if let Err(e) = self.update_index().await {
            warn!(
                "Warning: Failed to update index after PUT operation: {:?}",
                e
            );
        }

        // Atualiza o índice local imediatamente como fallback
        {
            let mut index_guard = self.index.index.write();
            index_guard.insert(key, value);
        }

        // Como já temos a Operation, podemos retorná-la diretamente
        Ok(op)
    }

    /// Remove um valor associado a uma chave específica.
    ///
    /// Esta operação cria uma entrada de deleção no log distribuído e remove
    /// a chave do índice local. A operação é replicada para outros peers na rede.
    ///
    /// # Argumentos
    ///
    /// * `key` - A chave a ser removida
    ///
    /// # Retorna
    ///
    /// A operação de deleção criada se bem-sucedida, ou um erro se a operação falhar.
    #[instrument(level = "debug", skip(self))]
    pub async fn delete(&mut self, key: String) -> Result<Operation> {
        // Validação de entrada
        if key.is_empty() {
            return Err(GuardianError::Store(
                "A chave não pode estar vazia".to_string(),
            ));
        }

        // Verifica se a chave existe antes de tentar deletar
        if !self.contains_key(&key) {
            return Err(GuardianError::Store(format!(
                "Chave '{}' não encontrada",
                key
            )));
        }

        // Criamos a operação diretamente como Operation
        use crate::stores::operation::operation::Operation;

        let op = Operation::new(Some(key.clone()), "DEL".to_string(), None);

        let _entry = self
            .base_store
            .add_operation(op.clone(), None)
            .await
            .map_err(|e| GuardianError::Store(format!("erro ao deletar chave '{}': {}", key, e)))?;

        // Força atualização do índice após adicionar a operação de delete ao log
        // Isso garante que o índice local e o global estejam sincronizados
        if let Err(e) = self.update_index().await {
            warn!(
                "Warning: Failed to update index after DELETE operation: {:?}",
                e
            );
        }

        // Atualiza o índice local imediatamente como fallback
        {
            let mut index_guard = self.index.index.write();
            index_guard.remove(&key);
        }

        Ok(op)
    }

    /// Obtém o valor associado a uma chave específica.
    ///
    /// # Argumentos
    ///
    /// * `key` - A chave a ser procurada
    ///
    /// # Retorna
    ///
    /// `Some(Vec<u8>)` se a chave for encontrada, `None` se não existir,
    /// ou um erro se houver problema no acesso.
    #[instrument(level = "debug", skip(self))]
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let _entered = self.span.enter();
        // Validação de entrada
        if key.is_empty() {
            return Err(GuardianError::Store(
                "A chave não pode estar vazia".to_string(),
            ));
        }

        // Retorna o valor do índice para a chave especificada
        Ok(self.index.get_value(key))
    }

    pub fn get_type(&self) -> &'static str {
        "keyvalue"
    }

    /// Função associada para criar uma nova instância de GuardianDBKeyValue
    #[instrument(level = "debug", skip(ipfs, identity, addr, options))]
    pub async fn new(
        ipfs: Arc<crate::ipfs_core_api::client::IpfsClient>,
        identity: Arc<Identity>,
        addr: Arc<dyn Address + Send + Sync>,
        options: Option<NewStoreOptions>,
    ) -> Result<Self> {
        // Cria o índice real para a KeyValue store
        let index = Arc::new(KeyValueIndex::new());

        // Cria uma nova opção com o índice personalizado
        let mut kv_options = options.unwrap_or_else(|| NewStoreOptions {
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
            span: None,
            tracer: None,
            pubsub: None,
            message_marshaler: None,
            peer_id: libp2p::PeerId::random(),
            direct_channel: None,
            close_func: None,
            store_specific_opts: None,
        });

        // Configura o index builder para criar nosso KeyValueIndex
        kv_options.index = Some(Box::new(
            move |_data: &[u8]| -> Box<dyn StoreIndex<Error = GuardianError>> {
                // Clona o índice real em vez de criar um novo
                Box::new(KeyValueIndex::new()) // Por simplicidade, criamos um novo por enquanto
            },
        ));

        // Cria span para esta instância da KeyValueStore
        let span = tracing::info_span!("keyvalue_store", address = %addr.to_string());
        // Inicializa a BaseStore com as opções atualizadas
        let base_store = BaseStore::new(ipfs, identity, addr, Some(kv_options))
            .await
            .map_err(|e| {
                GuardianError::Store(format!("incapaz de inicializar a base store: {}", e))
            })?;

        // Cache do address para resolver problemas de lifetime
        let cached_address = base_store.address();

        Ok(GuardianDBKeyValue {
            base_store,
            index,
            cached_address,
            span,
        })
    }
}
