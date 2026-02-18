use crate::guardian::core::{GuardianDB as BaseGuardianDB, NewGuardianDBOptions};
use crate::guardian::error::{GuardianError, Result};
use crate::p2p::network::client::IrohClient;
use crate::traits::{
    AsyncDocumentFilter, BaseGuardianDB as BaseGuardianDBTrait, CreateDBOptions, Document,
    DocumentStore, EventLogStore, GuardianDBKVStoreProvider, KeyValueStore, ProgressCallback,
    Store,
};
use parking_lot::RwLock;
use std::sync::Arc;

pub mod core;
pub mod error;
pub mod serializer;

pub struct GuardianDB {
    base: BaseGuardianDB,
}

impl GuardianDB {
    /// Cria uma nova instância do GuardianDB
    pub async fn new(client: IrohClient, options: Option<NewGuardianDBOptions>) -> Result<Self> {
        // Usar DefaultIdentificator para criar uma identidade válida com assinaturas criptográficas
        use crate::log::identity::{DefaultIdentificator, Identificator};

        let mut identificator = DefaultIdentificator::new();
        let identity = identificator.create(&client.node_id().to_string());

        let base = BaseGuardianDB::new_guardian_db(client, identity, options).await?;
        Ok(GuardianDB { base })
    }

    /// Cria um EventLogStore
    pub async fn log(
        &self,
        address: &str,
        options: Option<CreateDBOptions>,
    ) -> Result<Arc<dyn EventLogStore<Error = GuardianError>>> {
        let mut opts = options.unwrap_or_default();
        opts.create = Some(true);
        opts.store_type = Some("eventlog".to_string());

        // Passa o event_bus do GuardianDB para as options
        if opts.event_bus.is_none() {
            opts.event_bus = Some((*self.base.event_bus()).clone());
        }

        tracing::debug!(address = address, "GuardianDB::log - Criando EventLogStore");

        // Usa create() diretamente para nomes simples (sem hash válido)
        let store = self.base.create(address, "eventlog", Some(opts)).await?;

        tracing::debug!(
            address = address,
            store_type = store.store_type(),
            has_index = store.index().supports_entry_queries(),
            "EventLogStore criado"
        );

        // Verifica se o store retornado é do tipo correto
        if store.store_type() == "eventlog" {
            // Cria um wrapper simples que implementa EventLogStore
            Ok(Arc::new(EventLogStoreWrapper::new(store)))
        } else {
            Err(GuardianError::Store(format!(
                "Tipo de store incorreto. Esperado: eventlog, encontrado: {}",
                store.store_type()
            )))
        }
    }

    /// Cria um KeyValueStore
    pub async fn key_value(
        &self,
        address: &str,
        options: Option<CreateDBOptions>,
    ) -> Result<Arc<dyn KeyValueStore<Error = GuardianError>>> {
        let mut opts = options.unwrap_or_default();
        opts.create = Some(true);
        opts.store_type = Some("keyvalue".to_string());

        // Passa o event_bus do GuardianDB para as options
        if opts.event_bus.is_none() {
            opts.event_bus = Some((*self.base.event_bus()).clone());
        }

        // Usa create() diretamente para nomes simples
        let store = self.base.create(address, "keyvalue", Some(opts)).await?;

        // Para KeyValueStore, criamos um wrapper genérico
        if store.store_type() == "keyvalue" {
            Ok(Arc::new(KeyValueStoreWrapper::new(store)))
        } else {
            Err(GuardianError::Store(format!(
                "Tipo de store incorreto. Esperado: keyvalue, encontrado: {}",
                store.store_type()
            )))
        }
    }

    /// Cria um DocumentStore
    pub async fn docs(
        &self,
        address: &str,
        options: Option<CreateDBOptions>,
    ) -> Result<Arc<dyn DocumentStore<Error = GuardianError>>> {
        let mut opts = options.unwrap_or_default();
        opts.create = Some(true);
        opts.store_type = Some("document".to_string());

        // Passa o event_bus do GuardianDB para as options
        if opts.event_bus.is_none() {
            opts.event_bus = Some((*self.base.event_bus()).clone());
        }

        // Usa create() diretamente para nomes simples
        let store = self.base.create(address, "document", Some(opts)).await?;

        // Verifica se o store retornado é do tipo correto
        if store.store_type() == "document" {
            // Cria um wrapper que implementa DocumentStore
            Ok(Arc::new(DocumentStoreWrapper::new(store)))
        } else {
            Err(GuardianError::Store(format!(
                "Tipo de store incorreto. Esperado: document, encontrado: {}",
                store.store_type()
            )))
        }
    }

    /// Acesso direto ao BaseGuardianDB para funcionalidades avançadas
    pub fn base(&self) -> &BaseGuardianDB {
        &self.base
    }

    /// Método de conveniência para registrar um tipo de access controller com nome explícito
    pub fn register_access_control_type_with_name(
        &self,
        controller_type: &str,
        constructor: crate::traits::AccessControllerConstructor,
    ) -> Result<()> {
        self.base
            .register_access_control_type_with_name(controller_type, constructor)
    }

    /// Método de conveniência para registrar um access controller com tipo padrão
    pub async fn register_access_control_type(
        &self,
        constructor: crate::traits::AccessControllerConstructor,
    ) -> Result<()> {
        self.base.register_access_control_type(constructor).await
    }

    /// Método de conveniência para obter um construtor de access controller
    pub fn get_access_control_type(
        &self,
        controller_type: &str,
    ) -> Option<crate::traits::AccessControllerConstructor> {
        self.base.get_access_control_type(controller_type)
    }

    /// Método de conveniência para listar nomes de access controller types
    pub fn access_control_types_names(&self) -> Vec<String> {
        self.base.access_control_types_names()
    }

    /// Método de conveniência para registrar access controllers padrão
    pub async fn register_default_access_control_types(&self) -> Result<()> {
        self.base.register_default_access_control_types().await
    }

    /// Conecta e sincroniza com um peer específico
    ///
    /// Este método facilita a conexão manual com peers quando a descoberta
    /// automática não é suficiente ou você quer forçar uma sincronização.
    ///
    /// # Argumentos
    /// * `peer_id` - NodeId do peer com quem sincronizar
    ///
    /// # Retorna
    /// `Ok(())` se a sincronização foi iniciada com sucesso
    pub async fn connect_to_peer(&self, peer_id: iroh::NodeId) -> Result<()> {
        self.base.connect_to_peer(peer_id).await
    }
}

/// Wrapper que adapta um Store genérico para EventLogStore
///
/// SOLUÇÃO IMPLEMENTADA: O problema das limitações de &mut self foi resolvido
/// usando downcasting para BaseStore, que implementa add_operation(&self) de
/// forma thread-safe usando Arc<RwLock<T>> internamente.
pub struct EventLogStoreWrapper {
    store: Arc<dyn Store<Error = GuardianError> + Send + Sync>,
}

impl EventLogStoreWrapper {
    fn new(store: Arc<dyn Store<Error = GuardianError> + Send + Sync>) -> Self {
        Self { store }
    }

    /// Retorna uma referência ao store interno
    pub fn inner_store(&self) -> &Arc<dyn Store<Error = GuardianError> + Send + Sync> {
        &self.store
    }

    /// Tenta acessar o BaseStore subjacente através de downcast
    ///
    /// Este método é útil para operações avançadas como sincronização manual com peers.
    /// Retorna `None` se o store interno não for do tipo esperado.
    pub fn try_get_basestore(&self) -> Option<&crate::stores::base_store::BaseStore> {
        // Tenta fazer downcast para GuardianDBEventLogStore
        if let Some(event_log_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::event_log_store::GuardianDBEventLogStore>()
        {
            return Some(event_log_store.basestore());
        }
        None
    }

    /// Conecta e sincroniza com um peer específico
    ///
    /// Este método facilita a conexão manual com peers quando a descoberta
    /// automática não é suficiente ou você quer forçar uma sincronização.
    ///
    /// # Argumentos
    /// * `peer_id` - NodeId do peer com quem sincronizar
    ///
    /// # Retorna
    /// `Ok(())` se a sincronização foi iniciada com sucesso
    pub async fn connect_to_peer(&self, peer_id: iroh::NodeId) -> Result<()> {
        if let Some(base_store) = self.try_get_basestore() {
            base_store.exchange_heads(peer_id).await
        } else {
            Err(GuardianError::Store(
                "Não foi possível acessar BaseStore para sincronização".to_string(),
            ))
        }
    }

    /// Query otimizada usando o índice da store
    fn query_from_index(
        &self,
        options: &crate::traits::StreamOptions,
    ) -> Result<Vec<crate::log::entry::Entry>> {
        let index = self.store.index();

        // Query simples por quantidade (caso mais comum)
        let is_simple_amount_query = options.gt.is_none()
            && options.gte.is_none()
            && options.lt.is_none()
            && options.lte.is_none();

        if is_simple_amount_query {
            let amount = match options.amount {
                Some(a) if a > 0 => a as usize,
                Some(-1) | None => {
                    // -1 ou None significa "todas as entradas"
                    match index.len() {
                        Ok(len) => len,
                        Err(_) => return self.query_from_oplog(options), // Fallback
                    }
                }
                _ => 0,
            };

            // Usa método otimizado do índice se disponível
            if let Some(entries) = index.get_last_entries(amount) {
                return Ok(entries);
            }
        }

        // Query por Hash específico
        if let Some(hash) = options.gte.as_ref()
            && options.amount == Some(1)
            && options.gt.is_none()
            && options.lt.is_none()
            && options.lte.is_none()
        {
            if let Some(entry) = index.get_entry_by_hash(hash) {
                return Ok(vec![entry]);
            } else {
                return Ok(Vec::new()); // Hash não encontrado
            }
        }

        // Para queries mais complexas, usa fallback
        self.query_from_oplog(options)
    }

    /// Fallback: busca direta no oplog quando índice não suporta a query
    fn query_from_oplog(
        &self,
        options: &crate::traits::StreamOptions,
    ) -> Result<Vec<crate::log::entry::Entry>> {
        let oplog = self.store.op_log();
        let oplog_guard = oplog.read();

        // Coleta todas as entradas do oplog
        let mut all_entries: Vec<_> = oplog_guard
            .values()
            .iter()
            .map(|arc_entry| arc_entry.as_ref().clone())
            .collect();

        // Ordena por ordem cronológica (mais antigas primeiro - ordem de inserção)
        all_entries.sort_by_key(|b| b.clock().time());

        // Aplica filtros de Hash se especificados
        let mut filtered_entries = all_entries;

        // Filtro gte (maior ou igual)
        if let Some(hash) = &options.gte {
            if let Some(start_idx) = filtered_entries.iter().position(|e| e.hash() == hash) {
                filtered_entries = filtered_entries.into_iter().skip(start_idx).collect();
            } else {
                return Ok(Vec::new()); // Hash não encontrado
            }
        }

        // Filtro gt (maior que)
        if let Some(hash) = &options.gt {
            if let Some(start_idx) = filtered_entries.iter().position(|e| e.hash() == hash) {
                filtered_entries = filtered_entries.into_iter().skip(start_idx + 1).collect();
            } else {
                return Ok(Vec::new()); // Hash não encontrado
            }
        }

        // Filtro lte (menor ou igual)
        if let Some(hash) = &options.lte {
            if let Some(end_idx) = filtered_entries.iter().position(|e| e.hash() == hash) {
                filtered_entries = filtered_entries.into_iter().take(end_idx + 1).collect();
            } else {
                return Ok(Vec::new()); // Hash não encontrado
            }
        }

        // Filtro lt (menor que)
        if let Some(hash) = &options.lt {
            if let Some(end_idx) = filtered_entries.iter().position(|e| e.hash() == hash) {
                filtered_entries = filtered_entries.into_iter().take(end_idx).collect();
            } else {
                return Ok(Vec::new()); // Hash não encontrado
            }
        }

        // Aplica limitação de quantidade
        let amount = match options.amount {
            Some(a) if a > 0 => a as usize,
            Some(-1) | None => filtered_entries.len(), // -1 ou None = todas
            _ => 0,
        };

        filtered_entries.truncate(amount);
        Ok(filtered_entries)
    }
}

#[async_trait::async_trait]
impl Store for EventLogStoreWrapper {
    type Error = GuardianError;

    #[allow(deprecated)]
    fn events(&self) -> &dyn crate::events::EmitterInterface {
        self.store.events()
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        self.store.close().await
    }

    fn address(&self) -> &dyn crate::address::Address {
        self.store.address()
    }

    fn index(&self) -> Box<dyn crate::traits::StoreIndex<Error = GuardianError> + Send + Sync> {
        self.store.index()
    }

    fn store_type(&self) -> &str {
        self.store.store_type()
    }

    fn replication_status(&self) -> crate::stores::replicator::replication_info::ReplicationInfo {
        self.store.replication_status()
    }

    fn cache(&self) -> Arc<dyn crate::data_store::Datastore> {
        self.store.cache()
    }

    async fn drop(&self) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    async fn load(&self, amount: usize) -> std::result::Result<(), Self::Error> {
        self.store.load(amount).await
    }

    async fn sync(
        &self,
        heads: Vec<crate::log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        self.store.sync(heads).await
    }

    async fn load_more_from(&self, _amount: u64, entries: Vec<crate::log::entry::Entry>) {
        self.store.load_more_from(_amount, entries).await
    }

    async fn load_from_snapshot(&self) -> std::result::Result<(), Self::Error> {
        self.store.load_from_snapshot().await
    }

    fn op_log(&self) -> Arc<RwLock<crate::log::Log>> {
        self.store.op_log()
    }

    fn client(&self) -> Arc<IrohClient> {
        unimplemented!("Adaptação entre tipos de cliente iroh pendente")
    }

    fn db_name(&self) -> &str {
        self.store.db_name()
    }

    fn identity(&self) -> &crate::log::identity::Identity {
        self.store.identity()
    }

    fn access_controller(&self) -> &dyn crate::access_control::traits::AccessController {
        self.store.access_controller()
    }

    async fn add_operation(
        &self,
        op: crate::stores::operation::Operation,
        on_progress_callback: Option<ProgressCallback>,
    ) -> std::result::Result<crate::log::entry::Entry, Self::Error> {
        // Delega diretamente para o store interno através da trait Store
        self.store.add_operation(op, on_progress_callback).await
    }

    fn span(&self) -> Arc<tracing::Span> {
        self.store.span()
    }

    fn tracer(&self) -> Arc<crate::traits::TracerWrapper> {
        self.store.tracer()
    }

    fn event_bus(&self) -> Arc<crate::p2p::EventBus> {
        self.store.event_bus()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait::async_trait]
impl EventLogStore for EventLogStoreWrapper {
    async fn add(
        &self,
        data: Vec<u8>,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Cria uma operação ADD e a adiciona ao store
        let operation =
            crate::stores::operation::Operation::new(None, "ADD".to_string(), Some(data));

        let _entry = self.add_operation(operation.clone(), None).await?;

        // Retorna a operação que foi adicionada com sucesso
        // ***Em uma implementação mais sofisticada, poderíamos re-parsear a entrada
        // para garantir consistência, mas para este caso a operação original serve
        Ok(operation)
    }

    async fn get(
        &self,
        hash: &iroh_blobs::Hash,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Busca uma operação específica por Hash

        // Primeiro, tenta usar o índice para busca otimizada
        if let Some(entry) = self.store.index().get_entry_by_hash(hash) {
            // Converte a Entry para Operation usando parse_operation
            let operation = crate::stores::operation::parse_operation(entry)
                .map_err(|e| GuardianError::Store(format!("Falha ao parsear entrada: {}", e)))?;
            return Ok(operation);
        }

        // Fallback: busca no oplog diretamente
        let oplog = self.store.op_log();
        let oplog_guard = oplog.read();

        // Busca linear no oplog por Hash
        for arc_entry in oplog_guard.values() {
            if arc_entry.hash() == hash {
                // Converte Entry para Operation
                let entry = arc_entry.as_ref().clone();
                let operation = crate::stores::operation::parse_operation(entry).map_err(|e| {
                    GuardianError::Store(format!("Falha ao parsear entrada: {}", e))
                })?;
                return Ok(operation);
            }
        }

        // Hash não encontrado
        Err(GuardianError::Store(format!(
            "Operação não encontrada para Hash: {}",
            hex::encode(hash.as_bytes())
        )))
    }

    async fn list(
        &self,
        options: Option<crate::traits::StreamOptions>,
    ) -> std::result::Result<Vec<crate::stores::operation::Operation>, Self::Error> {
        // Lista operações com filtros opcionais
        let options = options.unwrap_or_default();

        // Tenta usar índice otimizado primeiro
        let entries = if self.store.index().supports_entry_queries() {
            // Query otimizada usando o índice
            self.query_from_index(&options)?
        } else {
            // Fallback: busca no oplog
            self.query_from_oplog(&options)?
        };

        // Converte todas as entradas para operações
        let mut operations = Vec::with_capacity(entries.len());
        for entry in entries {
            match crate::stores::operation::parse_operation(entry) {
                Ok(operation) => operations.push(operation),
                Err(e) => {
                    // Log do erro mas continua processando outras entradas
                    eprintln!("Aviso: Falha ao parsear entrada: {}", e);
                }
            }
        }

        Ok(operations)
    }
}

/// Wrapper que adapta um Store genérico para KeyValueStore
struct KeyValueStoreWrapper {
    store: Arc<dyn Store<Error = GuardianError> + Send + Sync>,
}

impl KeyValueStoreWrapper {
    fn new(store: Arc<dyn Store<Error = GuardianError> + Send + Sync>) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl Store for KeyValueStoreWrapper {
    type Error = GuardianError;

    #[allow(deprecated)]
    fn events(&self) -> &dyn crate::events::EmitterInterface {
        self.store.events()
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        self.store.close().await
    }

    fn address(&self) -> &dyn crate::address::Address {
        self.store.address()
    }

    fn index(&self) -> Box<dyn crate::traits::StoreIndex<Error = GuardianError> + Send + Sync> {
        self.store.index()
    }

    fn store_type(&self) -> &str {
        self.store.store_type()
    }

    fn replication_status(&self) -> crate::stores::replicator::replication_info::ReplicationInfo {
        self.store.replication_status()
    }

    fn cache(&self) -> Arc<dyn crate::data_store::Datastore> {
        self.store.cache()
    }

    async fn drop(&self) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    async fn load(&self, amount: usize) -> std::result::Result<(), Self::Error> {
        self.store.load(amount).await
    }

    async fn sync(
        &self,
        heads: Vec<crate::log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        self.store.sync(heads).await
    }

    async fn load_more_from(&self, _amount: u64, entries: Vec<crate::log::entry::Entry>) {
        self.store.load_more_from(_amount, entries).await
    }

    async fn load_from_snapshot(&self) -> std::result::Result<(), Self::Error> {
        self.store.load_from_snapshot().await
    }

    fn op_log(&self) -> Arc<RwLock<crate::log::Log>> {
        self.store.op_log()
    }

    fn client(&self) -> Arc<IrohClient> {
        unimplemented!("Adaptação entre tipos de cliente iroh pendente")
    }

    fn db_name(&self) -> &str {
        self.store.db_name()
    }

    fn identity(&self) -> &crate::log::identity::Identity {
        self.store.identity()
    }

    fn access_controller(&self) -> &dyn crate::access_control::traits::AccessController {
        self.store.access_controller()
    }

    async fn add_operation(
        &self,
        op: crate::stores::operation::Operation,
        on_progress_callback: Option<ProgressCallback>,
    ) -> std::result::Result<crate::log::entry::Entry, Self::Error> {
        self.store.add_operation(op, on_progress_callback).await
    }

    fn span(&self) -> Arc<tracing::Span> {
        self.store.span()
    }

    fn tracer(&self) -> Arc<crate::traits::TracerWrapper> {
        self.store.tracer()
    }

    fn event_bus(&self) -> Arc<crate::p2p::EventBus> {
        self.store.event_bus()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait::async_trait]
impl KeyValueStore for KeyValueStoreWrapper {
    async fn get(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
        // Busca um valor por chave no KeyValue store

        // Primeiro, tenta buscar no índice (mais eficiente)
        let index = self.store.index();
        if let Ok(Some(bytes)) = index.get_bytes(key) {
            return Ok(Some(bytes));
        }

        // Fallback: busca no oplog por operações PUT com a chave especificada
        let oplog = self.store.op_log();
        let oplog_guard = oplog.read();

        // Busca pela operação PUT mais recente com a chave especificada
        let mut latest_value: Option<Vec<u8>> = None;
        let mut latest_time = 0;

        for arc_entry in oplog_guard.values() {
            let entry = arc_entry.as_ref().clone();

            // Converte Entry para Operation
            if let Ok(operation) = crate::stores::operation::parse_operation(entry.clone()) {
                // Verifica se é uma operação relevante para a chave
                if let Some(op_key) = operation.key()
                    && op_key == key
                {
                    let entry_time = entry.clock().time();

                    let op_str = operation.op();
                    if op_str == "PUT" {
                        // Se é mais recente que a operação anterior
                        if entry_time > latest_time {
                            latest_time = entry_time;
                            latest_value = Some(operation.value().to_vec());
                        }
                    } else if op_str == "DEL" {
                        // Se é mais recente que a operação anterior e é uma deleção
                        if entry_time > latest_time {
                            latest_time = entry_time;
                            latest_value = None; // Valor foi deletado
                        }
                    }
                }
            }
        }

        Ok(latest_value)
    }

    async fn put(
        &self,
        key: &str,
        value: Vec<u8>,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Cria uma operação PUT com chave e valor
        let operation = crate::stores::operation::Operation::new(
            Some(key.to_string()),
            "PUT".to_string(),
            Some(value),
        );

        self.add_operation(operation.clone(), None).await?;
        Ok(operation)
    }

    async fn delete(
        &self,
        key: &str,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Cria uma operação DEL com chave
        let operation = crate::stores::operation::Operation::new(
            Some(key.to_string()),
            "DEL".to_string(),
            None,
        );

        self.add_operation(operation.clone(), None).await?;
        Ok(operation)
    }

    fn all(&self) -> std::collections::HashMap<String, Vec<u8>> {
        let mut result = std::collections::HashMap::new();

        // Primeiro, tenta coletar do índice (mais eficiente se estiver atualizado)
        let index = self.store.index();
        if let Ok(keys) = index.keys() {
            for key in keys {
                if let Ok(Some(bytes)) = index.get_bytes(&key) {
                    result.insert(key, bytes);
                }
            }
        }

        // Se o índice não retornou dados, ou como complemento, processa o oplog
        // Isso garante que temos os dados mais atualizados, incluindo operações não indexadas
        if result.is_empty() {
            let oplog = self.store.op_log();
            let oplog_guard = oplog.read();

            // Mapeia chave -> (timestamp, operação, valor)
            let mut key_operations: std::collections::HashMap<
                String,
                (u64, String, Option<Vec<u8>>),
            > = std::collections::HashMap::new();

            // Coleta todas as operações relevantes
            for arc_entry in oplog_guard.values() {
                let entry = arc_entry.as_ref().clone();

                // Converte Entry para Operation
                if let Ok(operation) = crate::stores::operation::parse_operation(entry.clone())
                    && let Some(op_key) = operation.key()
                {
                    let timestamp = entry.clock().time();
                    let op_type = operation.op().to_string();
                    let value = if !operation.value().is_empty() {
                        Some(operation.value().to_vec())
                    } else {
                        None
                    };

                    // Atualiza se é mais recente ou se não existia antes
                    let key_clone = op_key.clone();
                    if let Some((existing_time, _, _)) = key_operations.get(&key_clone) {
                        if timestamp > *existing_time {
                            key_operations.insert(key_clone, (timestamp, op_type, value));
                        }
                    } else {
                        key_operations.insert(key_clone, (timestamp, op_type, value));
                    }
                }
            }

            // Processa as operações finais para cada chave
            for (key, (_timestamp, op_type, value)) in key_operations {
                let op_str = op_type.as_str();
                if op_str == "PUT" {
                    if let Some(val) = value {
                        result.insert(key, val);
                    }
                } else if op_str == "DEL" {
                    // Remove da lista se foi deletado
                    result.remove(&key);
                } else {
                    // Para outras operações, adiciona se tiver valor
                    if let Some(val) = value {
                        result.insert(key, val);
                    }
                }
            }
        }

        result
    }
}

/// Wrapper que adapta um Store genérico para DocumentStore
struct DocumentStoreWrapper {
    store: Arc<dyn Store<Error = GuardianError> + Send + Sync>,
}

impl DocumentStoreWrapper {
    fn new(store: Arc<dyn Store<Error = GuardianError> + Send + Sync>) -> Self {
        Self { store }
    }

    /// Busca documentos no índice usando uma chave
    fn search_documents_by_key(
        &self,
        key: &str,
        opts: &crate::traits::DocumentStoreGetOptions,
    ) -> Result<Vec<Document>> {
        let index = self.store.index();

        // Prepara a chave de busca de acordo com as opções
        let mut key_for_search = key.to_string();
        let has_multiple_terms = key.contains(' ');

        if has_multiple_terms {
            key_for_search = key_for_search.replace('.', " ");
        }
        if opts.case_insensitive {
            key_for_search = key_for_search.to_lowercase();
        }

        let mut documents = Vec::new();

        // Obtém todas as chaves do índice
        let all_keys = index.keys().unwrap_or_default();

        for index_key in all_keys {
            let mut index_key_for_search = index_key.clone();

            // Normaliza a chave do índice para a busca
            if opts.case_insensitive {
                index_key_for_search = index_key_for_search.to_lowercase();
            }

            // Verifica se a chave corresponde aos critérios de busca
            let matches = if opts.partial_matches {
                index_key_for_search.contains(&key_for_search)
            } else {
                index_key_for_search == key_for_search
            };

            if matches {
                // Busca o valor no índice
                if let Ok(Some(doc_bytes)) = index.get_bytes(&index_key) {
                    // Desserializa o documento
                    match serde_json::from_slice::<serde_json::Value>(&doc_bytes) {
                        Ok(json_value) => {
                            let doc: Document = Box::new(json_value);
                            documents.push(doc);
                        }
                        Err(e) => {
                            eprintln!(
                                "Aviso: Falha ao desserializar documento para chave '{}': {}",
                                index_key, e
                            );
                        }
                    }
                } else {
                    eprintln!(
                        "Aviso: chave '{}' encontrada mas sem valor correspondente",
                        index_key
                    );
                }
            }
        }

        Ok(documents)
    }

    /// Busca documentos usando operações do oplog
    fn search_documents_from_oplog(
        &self,
        key: &str,
        opts: &crate::traits::DocumentStoreGetOptions,
    ) -> Result<Vec<Document>> {
        let oplog = self.store.op_log();
        let oplog_guard = oplog.read();

        let mut documents = Vec::new();
        let mut processed_keys = std::collections::HashSet::new();

        // Coleta todas as entradas do oplog em um vetor para iterar em ordem reversa
        let entries: Vec<Arc<crate::log::entry::Entry>> =
            oplog_guard.values().into_iter().collect();

        // Itera em ordem reversa (do mais novo para o mais antigo)
        // para garantir que apenas a operação mais recente para cada chave seja considerada
        for arc_entry in entries.iter().rev() {
            let entry: crate::log::entry::Entry = (**arc_entry).clone();

            // Converte Entry para Operation
            if let Ok(operation) = crate::stores::operation::parse_operation(entry) {
                // Verifica se a operação é relevante para documentos
                if let Some(op_key) = operation.key() {
                    // Evita processar a mesma chave múltiplas vezes
                    if processed_keys.contains(op_key) {
                        continue;
                    }

                    let mut op_key_search = op_key.clone();
                    let mut key_search = key.to_string();

                    if opts.case_insensitive {
                        op_key_search = op_key_search.to_lowercase();
                        key_search = key_search.to_lowercase();
                    }

                    let matches = if opts.partial_matches {
                        op_key_search.contains(&key_search)
                    } else {
                        op_key_search == key_search
                    };

                    if matches {
                        processed_keys.insert(op_key.clone());

                        // Se a operação mais recente for DEL, ignora este documento
                        if operation.op() == "DEL" {
                            continue;
                        }

                        // Se for PUT e tiver valor, adiciona o documento
                        if operation.op() == "PUT" && !operation.value().is_empty() {
                            // Tenta desserializar o valor como documento
                            match serde_json::from_slice::<serde_json::Value>(operation.value()) {
                                Ok(json_value) => {
                                    let doc: Document = Box::new(json_value);
                                    documents.push(doc);
                                }
                                Err(_) => {
                                    // Se não conseguir desserializar como JSON, cria um documento simples
                                    let simple_doc = serde_json::json!({
                                        "key": op_key,
                                        "value": String::from_utf8_lossy(operation.value()),
                                        "op_type": operation.op()
                                    });
                                    let doc: Document = Box::new(simple_doc);
                                    documents.push(doc);
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(documents)
    }

    /// Coleta todos os documentos do índice para queries
    fn get_all_documents_from_index(&self) -> Result<Vec<Document>> {
        let index = self.store.index();
        let mut documents = Vec::new();

        let all_keys = index.keys().unwrap_or_default();

        eprintln!(
            "DEBUG: get_all_documents_from_index - Total de chaves no índice: {}",
            all_keys.len()
        );

        for key in all_keys {
            eprintln!(
                "DEBUG: get_all_documents_from_index - Processando chave: {}",
                key
            );
            if let Ok(Some(doc_bytes)) = index.get_bytes(&key) {
                eprintln!(
                    "DEBUG: get_all_documents_from_index - Bytes recuperados para chave '{}': {} bytes",
                    key,
                    doc_bytes.len()
                );
                match serde_json::from_slice::<serde_json::Value>(&doc_bytes) {
                    Ok(json_value) => {
                        eprintln!(
                            "DEBUG: get_all_documents_from_index - Documento deserializado com sucesso: {:?}",
                            json_value
                        );
                        let doc: Document = Box::new(json_value);
                        documents.push(doc);
                    }
                    Err(e) => {
                        eprintln!(
                            "Aviso: Falha ao desserializar documento para chave '{}': {}",
                            key, e
                        );
                    }
                }
            } else {
                eprintln!(
                    "DEBUG: get_all_documents_from_index - Nenhum byte encontrado para chave: {}",
                    key
                );
            }
        }

        eprintln!(
            "DEBUG: get_all_documents_from_index - Total de documentos coletados: {}",
            documents.len()
        );
        Ok(documents)
    }
}

#[async_trait::async_trait]
impl Store for DocumentStoreWrapper {
    type Error = GuardianError;

    #[allow(deprecated)]
    fn events(&self) -> &dyn crate::events::EmitterInterface {
        self.store.events()
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        self.store.close().await
    }

    fn address(&self) -> &dyn crate::address::Address {
        self.store.address()
    }

    fn index(&self) -> Box<dyn crate::traits::StoreIndex<Error = GuardianError> + Send + Sync> {
        self.store.index()
    }

    fn store_type(&self) -> &str {
        self.store.store_type()
    }

    fn replication_status(&self) -> crate::stores::replicator::replication_info::ReplicationInfo {
        self.store.replication_status()
    }

    fn cache(&self) -> Arc<dyn crate::data_store::Datastore> {
        self.store.cache()
    }

    async fn drop(&self) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    async fn load(&self, amount: usize) -> std::result::Result<(), Self::Error> {
        self.store.load(amount).await
    }

    async fn sync(
        &self,
        heads: Vec<crate::log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        self.store.sync(heads).await
    }

    async fn load_more_from(&self, _amount: u64, entries: Vec<crate::log::entry::Entry>) {
        self.store.load_more_from(_amount, entries).await
    }

    async fn load_from_snapshot(&self) -> std::result::Result<(), Self::Error> {
        self.store.load_from_snapshot().await
    }

    fn op_log(&self) -> Arc<RwLock<crate::log::Log>> {
        self.store.op_log()
    }

    fn client(&self) -> Arc<IrohClient> {
        unimplemented!("Adaptação entre tipos de cliente iroh pendente")
    }

    fn db_name(&self) -> &str {
        self.store.db_name()
    }

    fn identity(&self) -> &crate::log::identity::Identity {
        self.store.identity()
    }

    fn access_controller(&self) -> &dyn crate::access_control::traits::AccessController {
        self.store.access_controller()
    }

    async fn add_operation(
        &self,
        op: crate::stores::operation::Operation,
        on_progress_callback: Option<ProgressCallback>,
    ) -> std::result::Result<crate::log::entry::Entry, Self::Error> {
        // Delega diretamente para o store interno através da trait Store
        self.store.add_operation(op, on_progress_callback).await
    }

    fn span(&self) -> Arc<tracing::Span> {
        self.store.span()
    }

    fn tracer(&self) -> Arc<crate::traits::TracerWrapper> {
        self.store.tracer()
    }

    fn event_bus(&self) -> Arc<crate::p2p::EventBus> {
        self.store.event_bus()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait::async_trait]
impl DocumentStore for DocumentStoreWrapper {
    async fn put(
        &self,
        document: Document,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Extrai a chave do documento (campo _id se for JSON)
        let key = if let Some(json_val) = document.downcast_ref::<serde_json::Value>() {
            json_val
                .get("_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        } else {
            None
        };

        // Serializa o documento para bytes
        let data = if let Some(json_val) = document.downcast_ref::<serde_json::Value>() {
            serde_json::to_vec(json_val).map_err(|e| {
                GuardianError::Store(format!("Failed to serialize JSON document: {}", e))
            })?
        } else if let Some(bytes) = document.downcast_ref::<Vec<u8>>() {
            bytes.clone()
        } else {
            // Para outros tipos, usa uma serialização genérica
            format!("{:?}", document).into_bytes()
        };

        // Cria uma operação PUT para documento com chave extraída do _id
        let operation =
            crate::stores::operation::Operation::new(key, "PUT".to_string(), Some(data));

        self.add_operation(operation.clone(), None).await?;
        Ok(operation)
    }

    async fn delete(
        &self,
        key: &str,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Cria uma operação DEL para documento
        let operation = crate::stores::operation::Operation::new(
            Some(key.to_string()),
            "DEL".to_string(),
            None,
        );

        self.add_operation(operation.clone(), None).await?;
        Ok(operation)
    }

    async fn put_batch(
        &self,
        values: Vec<Document>,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Serializa múltiplos documentos
        let mut batch_data = Vec::new();
        for document in values {
            let data = if let Some(bytes) = document.downcast_ref::<Vec<u8>>() {
                bytes.clone()
            } else {
                format!("{:?}", document).into_bytes()
            };
            batch_data.extend(data);
            batch_data.push(b'\n'); // Separador
        }

        // Cria uma operação PUT_BATCH
        let operation = crate::stores::operation::Operation::new(
            None,
            "PUT_BATCH".to_string(),
            Some(batch_data),
        );

        self.add_operation(operation.clone(), None).await?;
        Ok(operation)
    }

    async fn put_all(
        &self,
        values: Vec<Document>,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Similar ao put_batch, mas com operação PUT_ALL
        let mut batch_data = Vec::new();
        for document in values {
            let data = if let Some(bytes) = document.downcast_ref::<Vec<u8>>() {
                bytes.clone()
            } else {
                format!("{:?}", document).into_bytes()
            };
            batch_data.extend(data);
            batch_data.push(b'\n'); // Separador
        }

        // Cria uma operação PUT_ALL
        let operation =
            crate::stores::operation::Operation::new(None, "PUT_ALL".to_string(), Some(batch_data));

        self.add_operation(operation.clone(), None).await?;
        Ok(operation)
    }

    async fn get(
        &self,
        key: &str,
        opts: Option<crate::traits::DocumentStoreGetOptions>,
    ) -> std::result::Result<Vec<Document>, Self::Error> {
        // Busca documentos por chave com opções avançadas

        let opts = opts.unwrap_or_default();

        // Tenta usar o índice primeiro (mais eficiente)
        let documents_from_index = self.search_documents_by_key(key, &opts)?;

        if !documents_from_index.is_empty() {
            return Ok(documents_from_index);
        }

        // Fallback: busca no oplog se o índice não retornar resultados
        // Isso pode acontecer se o índice ainda não foi populado ou se estiver desatualizado
        let documents_from_oplog = self.search_documents_from_oplog(key, &opts)?;

        Ok(documents_from_oplog)
    }

    async fn query(
        &self,
        filter: AsyncDocumentFilter,
    ) -> std::result::Result<Vec<Document>, Self::Error> {
        // Query com filtro assíncrono customizável

        // Obtém todos os documentos disponíveis
        let all_documents = self.get_all_documents_from_index()?;

        let mut filtered_documents = Vec::new();

        // Aplica o filtro assíncrono a cada documento
        for document in all_documents {
            // Chama o filtro assíncrono
            let filter_future = filter(&document);

            match filter_future.await {
                Ok(true) => {
                    // Documento passou no filtro
                    filtered_documents.push(document);
                }
                Ok(false) => {
                    // Documento não passou no filtro, continua
                    continue;
                }
                Err(e) => {
                    // Erro no filtro - logamos mas continuamos processando
                    eprintln!("Aviso: Erro ao aplicar filtro no documento: {}", e);
                    continue;
                }
            }
        }

        Ok(filtered_documents)
    }
}

#[async_trait::async_trait]
impl BaseGuardianDBTrait for GuardianDB {
    type Error = GuardianError;

    async fn open(
        &self,
        address: &str,
        options: &mut CreateDBOptions,
    ) -> std::result::Result<Arc<dyn Store<Error = GuardianError>>, Self::Error> {
        let opts = options.clone();
        let result = self.base.open(address, opts).await?;
        // Convert Send+Sync to non-Send+Sync
        Ok(result as Arc<dyn Store<Error = GuardianError>>)
    }

    async fn determine_address(
        &self,
        name: &str,
        store_type: &str,
        options: &crate::traits::DetermineAddressOptions,
    ) -> std::result::Result<Box<dyn crate::address::Address>, Self::Error> {
        let opts = Some(options.clone());
        let result = self.base.determine_address(name, store_type, opts).await?;
        Ok(Box::new(result))
    }

    fn client(&self) -> Arc<crate::p2p::network::client::IrohClient> {
        Arc::new(self.base.client().clone())
    }

    fn identity(&self) -> Arc<crate::log::identity::Identity> {
        Arc::new(self.base.identity().clone())
    }

    fn get_store(&self, address: &str) -> Option<Arc<dyn Store<Error = GuardianError>>> {
        self.base
            .get_store(address)
            .map(|store| store as Arc<dyn Store<Error = GuardianError>>)
    }

    async fn create(
        &self,
        name: &str,
        store_type: &str,
        options: &mut CreateDBOptions,
    ) -> std::result::Result<Arc<dyn Store<Error = GuardianError>>, Self::Error> {
        let opts = Some(options.clone());
        let result = self.base.create(name, store_type, opts).await?;
        Ok(result as Arc<dyn Store<Error = GuardianError>>)
    }

    fn register_store_type(
        &mut self,
        store_type: &str,
        constructor: crate::traits::StoreConstructor,
    ) {
        // BaseGuardianDB já usa Arc<RwLock<>> internamente para store_types,
        // então é thread-safe e não precisa de &mut self
        self.base
            .register_store_type(store_type.to_string(), constructor);
    }

    fn unregister_store_type(&mut self, store_type: &str) {
        // BaseGuardianDB já usa Arc<RwLock<>> internamente para store_types
        self.base.unregister_store_type(store_type);
    }

    fn register_access_controller_type(
        &mut self,
        constructor: crate::traits::AccessControllerConstructor,
    ) -> std::result::Result<(), Self::Error> {
        // BaseGuardianDB já usa Arc<RwLock<>> internamente para access_control_types,
        // então é thread-safe. Usamos o método legado que registra com tipo "simple"
        self.base
            .register_access_control_type_with_name("simple", constructor)
    }

    fn unregister_access_controller_type(&mut self, controller_type: &str) {
        // BaseGuardianDB já usa Arc<RwLock<>> internamente para access_control_types
        self.base.unregister_access_control_type(controller_type);
    }

    fn get_access_controller_type(
        &self,
        controller_type: &str,
    ) -> Option<crate::traits::AccessControllerConstructor> {
        self.base.get_access_controller_type(controller_type)
    }

    fn event_bus(&self) -> crate::p2p::EventBus {
        (*self.base.event_bus()).clone()
    }

    fn span(&self) -> &tracing::Span {
        self.base.span()
    }

    fn tracer(&self) -> Arc<crate::traits::TracerWrapper> {
        // Converter BoxedTracer para TracerWrapper
        let boxed_tracer = self.base.tracer();
        Arc::new(crate::traits::TracerWrapper::new_opentelemetry(
            boxed_tracer,
        ))
    }
}

#[async_trait::async_trait]
impl GuardianDBKVStoreProvider for GuardianDB {
    type Error = GuardianError;

    async fn key_value(
        &self,
        address: &str,
        options: &mut CreateDBOptions,
    ) -> std::result::Result<Box<dyn KeyValueStore<Error = GuardianError>>, Self::Error> {
        // Usa o método já implementado do wrapper que retorna Arc
        let opts_clone = options.clone();
        let arc_store = self.key_value(address, Some(opts_clone)).await?;

        // Converte Arc para Box usando um wrapper
        Ok(Box::new(KeyValueStoreBoxWrapper::new(arc_store)))
    }
}

/// Wrapper para converter Arc<dyn KeyValueStore> para Box<dyn KeyValueStore>
pub struct KeyValueStoreBoxWrapper {
    inner: Arc<dyn KeyValueStore<Error = GuardianError>>,
}

impl KeyValueStoreBoxWrapper {
    pub fn new(inner: Arc<dyn KeyValueStore<Error = GuardianError>>) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl Store for KeyValueStoreBoxWrapper {
    type Error = GuardianError;

    fn address(&self) -> &dyn crate::address::Address {
        self.inner.address()
    }

    fn store_type(&self) -> &str {
        self.inner.store_type()
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        // Delega para o store interno usando close()
        self.inner.close().await
    }

    async fn drop(&self) -> std::result::Result<(), Self::Error> {
        self.inner.close().await
    }

    fn events(&self) -> &dyn crate::events::EmitterInterface {
        // events() está deprecated
        unimplemented!("events() is deprecated, use event_bus() instead")
    }

    fn index(&self) -> Box<dyn crate::traits::StoreIndex<Error = Self::Error> + Send + Sync> {
        self.inner.index()
    }

    fn replication_status(&self) -> crate::stores::replicator::replication_info::ReplicationInfo {
        self.inner.replication_status()
    }

    fn cache(&self) -> Arc<dyn crate::data_store::Datastore> {
        self.inner.cache()
    }

    async fn load(&self, amount: usize) -> std::result::Result<(), Self::Error> {
        // Delega diretamente para o inner store
        self.inner.load(amount).await
    }

    async fn sync(
        &self,
        heads: Vec<crate::log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        // Delega diretamente para o inner store
        self.inner.sync(heads).await
    }

    async fn load_more_from(&self, _amount: u64, entries: Vec<crate::log::entry::Entry>) {
        // Delega diretamente para o inner store
        self.inner.load_more_from(_amount, entries).await
    }

    async fn load_from_snapshot(&self) -> std::result::Result<(), Self::Error> {
        // Delega diretamente para o inner store
        self.inner.load_from_snapshot().await
    }

    fn op_log(&self) -> Arc<parking_lot::RwLock<crate::log::Log>> {
        self.inner.op_log()
    }

    fn client(&self) -> Arc<crate::p2p::network::client::IrohClient> {
        self.inner.client()
    }

    fn db_name(&self) -> &str {
        self.inner.db_name()
    }

    fn identity(&self) -> &crate::log::identity::Identity {
        self.inner.identity()
    }

    fn access_controller(&self) -> &dyn crate::access_control::traits::AccessController {
        self.inner.access_controller()
    }

    async fn add_operation(
        &self,
        op: crate::stores::operation::Operation,
        on_progress_callback: Option<crate::traits::ProgressCallback>,
    ) -> std::result::Result<crate::log::entry::Entry, Self::Error> {
        // Delega diretamente para o inner store
        self.inner.add_operation(op, on_progress_callback).await
    }

    fn span(&self) -> Arc<tracing::Span> {
        self.inner.span()
    }

    fn tracer(&self) -> Arc<crate::traits::TracerWrapper> {
        self.inner.tracer()
    }

    fn event_bus(&self) -> Arc<crate::p2p::EventBus> {
        self.inner.event_bus()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait::async_trait]
impl KeyValueStore for KeyValueStoreBoxWrapper {
    async fn put(
        &self,
        key: &str,
        value: Vec<u8>,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Delega para o inner KeyValueStore que já tem put implementado
        self.inner.put(key, value).await
    }

    async fn get(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
        self.inner.get(key).await
    }

    async fn delete(
        &self,
        key: &str,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Delega para o inner KeyValueStore que já tem delete implementado
        self.inner.delete(key).await
    }

    fn all(&self) -> std::collections::HashMap<String, Vec<u8>> {
        self.inner.all()
    }
}
