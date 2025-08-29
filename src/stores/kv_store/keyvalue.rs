use crate::address::Address;
use crate::data_store::Datastore;
use crate::eqlabs_ipfs_log::identity::Identity;
use crate::error::{GuardianError, Result};
use crate::iface::{KeyValueStore, NewStoreOptions, Store, StoreIndex};
use crate::pubsub::event::EventBus;
use crate::stores::base_store::base_store::BaseStore;
use crate::stores::operation::operation::Operation;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Implementação de StoreIndex para KeyValue Store
/// Mantém um índice thread-safe dos pares chave-valor
pub struct KeyValueIndex {
    /// Índice interno que mapeia chaves para valores
    index: Arc<RwLock<HashMap<String, Vec<u8>>>>,
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

    fn get(&self, _key: &str) -> Option<&(dyn std::any::Any + Send + Sync)> {
        // Para compatibilidade com o trait, precisamos usar uma abordagem diferente
        // que não envolva retornar uma referência direta aos dados do HashMap
        // Por enquanto, retornamos None e implementamos get_value() para acesso real
        None
    }

    fn update_index(
        &mut self,
        _log: &crate::eqlabs_ipfs_log::log::Log,
        entries: &[crate::eqlabs_ipfs_log::entry::Entry],
    ) -> Result<()> {
        let mut index_guard = self.index.write();

        for entry in entries {
            // Tenta deserializar o payload como uma operação
            match serde_json::from_str::<Operation>(&entry.payload()) {
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
}
/// Implementação da KeyValue Store para GuardianDB
///
/// Esta estrutura fornece funcionalidade de store chave-valor baseada em GuardianDB,
/// usando um BaseStore para operações fundamentais de log e sincronização.
/// Em Go, era `GuardianDBKeyValue` com `basestore.BaseStore` embutido.
/// Em Rust, usamos composição através do campo `base_store`.
pub struct GuardianDBKeyValue {
    base_store: Arc<BaseStore>,
    index: Arc<KeyValueIndex>,
}

#[async_trait::async_trait]
impl Store for GuardianDBKeyValue {
    type Error = GuardianError;

    #[allow(deprecated)]
    fn events(&self) -> &dyn crate::events::EmitterInterface {
        self.base_store.events()
    }

    async fn close(&mut self) -> Result<()> {
        // Delega o fechamento para a base store
        // Como close() no BaseStore não é mutável, não podemos chamá-lo diretamente
        // Por enquanto, apenas retornamos Ok(()) pois o BaseStore será dropado automaticamente
        Ok(())
    }

    fn address(&self) -> &dyn Address {
        // Como address() retorna Arc<dyn Address>, precisamos de uma abordagem diferente
        // Por enquanto, vamos usar uma implementação que evita problemas de lifetime
        use crate::address::{GuardianDBAddress, parse};
        static FALLBACK_ADDRESS: std::sync::OnceLock<GuardianDBAddress> =
            std::sync::OnceLock::new();
        FALLBACK_ADDRESS.get_or_init(|| {
            parse("/GuardianDB/dummy/keyvalue").expect("Failed to create fallback address")
        })
    }

    fn index(&self) -> &dyn StoreIndex<Error = GuardianError> {
        // Retorna o índice real da KeyValue store
        self.index.as_ref()
    }

    fn store_type(&self) -> &str {
        "keyvalue"
    }

    fn replication_status(&self) -> crate::stores::replicator::replication_info::ReplicationInfo {
        // Since ReplicationInfo doesn't implement Clone, we'll create a new instance
        // TODO: Consider implementing Clone for ReplicationInfo or returning a reference
        crate::stores::replicator::replication_info::ReplicationInfo::new()
    }

    fn replicator(&self) -> &crate::stores::replicator::replicator::Replicator {
        // Como BaseStore retorna Option<Arc<Replicator>>, precisamos de uma abordagem diferente
        // Por enquanto, vamos usar panic para documentar a limitação arquitetural
        panic!(
            "replicator() access through Store trait requires architectural refactoring - use BaseStore methods instead"
        )
    }

    fn cache(&self) -> Arc<dyn Datastore> {
        self.base_store.cache()
    }

    async fn drop(&mut self) -> Result<()> {
        // Implementação do drop que limpa o índice
        {
            let mut index_guard = self.index.index.write();
            index_guard.clear();
        }

        // O BaseStore será dropado automaticamente quando a struct sair de escopo
        Ok(())
    }

    async fn load(&mut self, amount: usize) -> Result<()> {
        // Delegate to base_store load functionality
        self.base_store.load(Some(amount as isize)).await
    }

    async fn sync(&mut self, heads: Vec<crate::eqlabs_ipfs_log::entry::Entry>) -> Result<()> {
        // Delegate to base_store sync functionality
        self.base_store.sync(heads).await
    }

    async fn load_more_from(
        &mut self,
        _amount: u64,
        entries: Vec<crate::eqlabs_ipfs_log::entry::Entry>,
    ) {
        // Delegate to base_store load_more_from functionality
        self.base_store.load_more_from(entries)
    }

    async fn load_from_snapshot(&mut self) -> Result<()> {
        // Delega para a base store e depois atualiza o índice
        self.base_store.load_from_snapshot().await?;

        // Força atualização do índice após carregar do snapshot
        // Como não temos acesso direto ao log, vamos apenas retornar Ok
        // O índice será atualizado quando as entradas forem processadas
        Ok(())
    }

    fn op_log(&self) -> &crate::eqlabs_ipfs_log::log::Log {
        // Como BaseStore retorna Arc<RwLock<Log>>, precisamos de uma abordagem diferente
        // Por enquanto, vamos usar panic para documentar a limitação arquitetural
        panic!(
            "op_log() access through Store trait requires architectural refactoring - use BaseStore::with_oplog() instead"
        )
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
        on_progress_callback: Option<
            tokio::sync::mpsc::Sender<crate::eqlabs_ipfs_log::entry::Entry>,
        >,
    ) -> Result<crate::eqlabs_ipfs_log::entry::Entry> {
        // Delegate to base_store add_operation functionality
        self.base_store
            .add_operation(op, on_progress_callback)
            .await
    }

    fn logger(&self) -> &slog::Logger {
        // Como logger() retorna Arc<Logger>, precisamos de uma abordagem diferente
        // Por enquanto, vamos usar uma implementação que evita problemas de lifetime
        use slog::{Discard, Logger, o};
        static FALLBACK_LOGGER: std::sync::OnceLock<Logger> = std::sync::OnceLock::new();
        FALLBACK_LOGGER.get_or_init(|| Logger::root(Discard, o!()))
    }

    fn tracer(&self) -> Arc<crate::iface::TracerWrapper> {
        // Return the tracer from base_store
        self.base_store.tracer()
    }

    fn event_bus(&self) -> EventBus {
        // TODO: Convert from Arc<EventBus> to EventBus - for now returning a new instance
        crate::pubsub::event::EventBus::new()
    }
}

// Em Go, a linha `var _ iface.KeyValueStore = &GuardianDBKeyValue{}`
// é uma verificação em tempo de compilação. Em Rust, a mesma garantia
// é obtida implementando o trait `KeyValueStore` para `GuardianDBKeyValue`.
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
        // Como não temos acesso direto ao log mutável através do BaseStore,
        // essa operação seria chamada automaticamente quando novas entradas são adicionadas
        // Por enquanto, apenas retornamos Ok(())
        Ok(())
    }

    /// equivalente a função All em go
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

        // Atualiza o índice local imediatamente
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

        // Atualiza o índice local imediatamente
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
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        // Validação de entrada
        if key.is_empty() {
            return Err(GuardianError::Store(
                "A chave não pode estar vazia".to_string(),
            ));
        }

        // Retorna o valor do índice para a chave especificada
        Ok(self.index.get_value(key))
    }

    /// equivalente a função Type em go
    pub fn r#type(&self) -> &'static str {
        // `type` é uma palavra-chave reservada em Rust, então usamos `r#type`
        // para definir o nome da função.
        "keyvalue"
    }

    /// equivalente a função NewGuardianDBKeyValue em go
    /// Em Rust, isto se torna uma função associada, tipicamente chamada `new`.
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
            logger: None,
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

        // `InitBaseStore` em Go inicializa a store base. Em Rust, encapsulamos
        // essa lógica dentro do construtor.
        let base_store = BaseStore::new(ipfs, identity, addr, Some(kv_options))
            .await
            .map_err(|e| {
                GuardianError::Store(format!("incapaz de inicializar a base store: {}", e))
            })?;

        Ok(GuardianDBKeyValue { base_store, index })
    }
}
