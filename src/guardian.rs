use crate::base_guardian::{GuardianDB as BaseGuardianDB, NewGuardianDBOptions};
use crate::error::{GuardianError, Result};
use crate::iface::{
    AsyncDocumentFilter, CreateDBOptions, Document, DocumentStore, EventLogStore, KeyValueStore,
    ProgressCallback, Store,
};
use crate::ipfs_core_api::client::IpfsClient;
use parking_lot::RwLock;
use std::sync::Arc;
pub struct GuardianDB {
    base: BaseGuardianDB,
}

impl GuardianDB {
    /// Cria uma nova instância do GuardianDB
    pub async fn new(ipfs: IpfsClient, options: Option<NewGuardianDBOptions>) -> Result<Self> {
        // Usar imports necessários
        use crate::ipfs_log::identity::{Identity, Signatures};

        // Criar uma identidade temporária para usar com new_orbit_db
        let signatures = Signatures::new("temp_sig", "temp_pub_sig");
        let identity = Identity::new("temp_id", "temp_pubkey", signatures);

        let base = BaseGuardianDB::new_orbit_db(ipfs, identity, options).await?;
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

        let store = self.base.open(address, opts).await?;

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

        let store = self.base.open(address, opts).await?;

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
        opts.store_type = Some("docstore".to_string());

        let store = self.base.open(address, opts).await?;

        // Verifica se o store retornado é do tipo correto
        if store.store_type() == "docstore" {
            // Cria um wrapper que implementa DocumentStore
            Ok(Arc::new(DocumentStoreWrapper::new(store)))
        } else {
            Err(GuardianError::Store(format!(
                "Tipo de store incorreto. Esperado: docstore, encontrado: {}",
                store.store_type()
            )))
        }
    }

    /// Acesso direto ao BaseGuardianDB para funcionalidades avançadas
    pub fn base(&self) -> &BaseGuardianDB {
        &self.base
    }
}

/// Wrapper que adapta um Store genérico para EventLogStore
///
/// SOLUÇÃO IMPLEMENTADA: O problema das limitações de &mut self foi resolvido
/// usando downcasting para BaseStore, que implementa add_operation(&self) de
/// forma thread-safe usando Arc<RwLock<T>> internamente.
struct EventLogStoreWrapper {
    store: Arc<dyn Store<Error = GuardianError> + Send + Sync>,
}

impl EventLogStoreWrapper {
    fn new(store: Arc<dyn Store<Error = GuardianError> + Send + Sync>) -> Self {
        Self { store }
    }

    /// Query otimizada usando o índice da store
    fn query_from_index(
        &self,
        options: &crate::iface::StreamOptions,
    ) -> Result<Vec<crate::ipfs_log::entry::Entry>> {
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

        // Query por CID específico
        if let Some(cid) = options.gte.as_ref()
            && options.amount == Some(1)
            && options.gt.is_none()
            && options.lt.is_none()
            && options.lte.is_none()
        {
            if let Some(entry) = index.get_entry_by_cid(cid) {
                return Ok(vec![entry]);
            } else {
                return Ok(Vec::new()); // CID não encontrado
            }
        }

        // Para queries mais complexas, usa fallback
        self.query_from_oplog(options)
    }

    /// Fallback: busca direta no oplog quando índice não suporta a query
    fn query_from_oplog(
        &self,
        options: &crate::iface::StreamOptions,
    ) -> Result<Vec<crate::ipfs_log::entry::Entry>> {
        let oplog = self.store.op_log();
        let oplog_guard = oplog.read();

        // Coleta todas as entradas do oplog
        let mut all_entries: Vec<_> = oplog_guard
            .values()
            .iter()
            .map(|arc_entry| arc_entry.as_ref().clone())
            .collect();

        // Ordena por ordem cronológica (mais recentes primeiro para corresponder ao comportamento esperado)
        all_entries.sort_by_key(|b| std::cmp::Reverse(b.clock().time()));

        // Aplica filtros de CID se especificados
        let mut filtered_entries = all_entries;

        // Filtro gte (maior ou igual)
        if let Some(cid) = &options.gte {
            let cid_str = cid.to_string();
            if let Some(start_idx) = filtered_entries.iter().position(|e| e.hash() == cid_str) {
                filtered_entries = filtered_entries.into_iter().skip(start_idx).collect();
            } else {
                return Ok(Vec::new()); // CID não encontrado
            }
        }

        // Filtro gt (maior que)
        if let Some(cid) = &options.gt {
            let cid_str = cid.to_string();
            if let Some(start_idx) = filtered_entries.iter().position(|e| e.hash() == cid_str) {
                filtered_entries = filtered_entries.into_iter().skip(start_idx + 1).collect();
            } else {
                return Ok(Vec::new()); // CID não encontrado
            }
        }

        // Filtro lte (menor ou igual)
        if let Some(cid) = &options.lte {
            let cid_str = cid.to_string();
            if let Some(end_idx) = filtered_entries.iter().position(|e| e.hash() == cid_str) {
                filtered_entries = filtered_entries.into_iter().take(end_idx + 1).collect();
            } else {
                return Ok(Vec::new()); // CID não encontrado
            }
        }

        // Filtro lt (menor que)
        if let Some(cid) = &options.lt {
            let cid_str = cid.to_string();
            if let Some(end_idx) = filtered_entries.iter().position(|e| e.hash() == cid_str) {
                filtered_entries = filtered_entries.into_iter().take(end_idx).collect();
            } else {
                return Ok(Vec::new()); // CID não encontrado
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

    fn index(&self) -> Box<dyn crate::iface::StoreIndex<Error = GuardianError> + Send + Sync> {
        self.store.index()
    }

    fn store_type(&self) -> &str {
        self.store.store_type()
    }

    fn replication_status(&self) -> crate::stores::replicator::replication_info::ReplicationInfo {
        self.store.replication_status()
    }

    fn replicator(&self) -> Option<Arc<crate::stores::replicator::replicator::Replicator>> {
        self.store.replicator()
    }

    fn cache(&self) -> Arc<dyn crate::data_store::Datastore> {
        self.store.cache()
    }

    async fn drop(&mut self) -> std::result::Result<(), Self::Error> {
        // Delega para BaseStore usando downcasting
        if let Some(_base_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            // BaseStore não tem um método drop específico, mas podemos fazer limpeza básica
            // ***Por enquanto, retorna sucesso pois a limpeza será feita automaticamente no Drop trait
            Ok(())
        } else {
            Err(GuardianError::Store(
                "Não foi possível fazer downcast para BaseStore".to_string(),
            ))
        }
    }

    async fn load(&mut self, amount: usize) -> std::result::Result<(), Self::Error> {
        // Delega para BaseStore usando downcasting
        if let Some(base_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            base_store.load(Some(amount as isize)).await
        } else {
            Err(GuardianError::Store(
                "Não foi possível fazer downcast para BaseStore".to_string(),
            ))
        }
    }

    async fn sync(
        &mut self,
        heads: Vec<crate::ipfs_log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        // Delega para BaseStore usando downcasting
        if let Some(base_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            base_store.sync(heads).await
        } else {
            Err(GuardianError::Store(
                "Não foi possível fazer downcast para BaseStore".to_string(),
            ))
        }
    }

    async fn load_more_from(&mut self, _amount: u64, entries: Vec<crate::ipfs_log::entry::Entry>) {
        // Delega para BaseStore usando downcasting
        if let Some(base_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            // BaseStore::load_more_from retorna Result<usize>, mas esta trait não espera retorno
            let _ = base_store.load_more_from(entries);
        } else {
            // Log do erro, mas não pode retornar erro porque a assinatura não permite
            eprintln!("Aviso: Não foi possível fazer downcast para BaseStore em load_more_from");
        }
    }

    async fn load_from_snapshot(&mut self) -> std::result::Result<(), Self::Error> {
        // Delega para BaseStore usando downcasting
        if let Some(base_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            base_store.load_from_snapshot().await
        } else {
            Err(GuardianError::Store(
                "Não foi possível fazer downcast para BaseStore".to_string(),
            ))
        }
    }

    fn op_log(&self) -> Arc<RwLock<crate::ipfs_log::log::Log>> {
        self.store.op_log()
    }

    fn ipfs(&self) -> Arc<IpfsClient> {
        unimplemented!("Adaptação entre tipos de cliente IPFS pendente")
    }

    fn db_name(&self) -> &str {
        self.store.db_name()
    }

    fn identity(&self) -> &crate::ipfs_log::identity::Identity {
        self.store.identity()
    }

    fn access_controller(&self) -> &dyn crate::access_controller::traits::AccessController {
        self.store.access_controller()
    }

    async fn add_operation(
        &mut self,
        op: crate::stores::operation::operation::Operation,
        on_progress_callback: Option<ProgressCallback>,
    ) -> std::result::Result<crate::ipfs_log::entry::Entry, Self::Error> {
        // Faz downcast para BaseStore e usa o método add_operation(&self)
        // que é thread-safe e não requer &mut self
        if let Some(base_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            base_store.add_operation(op, on_progress_callback).await
        } else {
            Err(GuardianError::Store(
                "Não foi possível fazer downcast para BaseStore".to_string(),
            ))
        }
    }

    fn span(&self) -> Arc<tracing::Span> {
        self.store.span()
    }

    fn tracer(&self) -> Arc<crate::iface::TracerWrapper> {
        self.store.tracer()
    }

    fn event_bus(&self) -> Arc<crate::pubsub::event::EventBus> {
        self.store.event_bus()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait::async_trait]
impl EventLogStore for EventLogStoreWrapper {
    async fn add(
        &mut self,
        data: Vec<u8>,
    ) -> std::result::Result<crate::stores::operation::operation::Operation, Self::Error> {
        // Cria uma operação ADD e a adiciona ao store
        let operation = crate::stores::operation::operation::Operation::new(
            None,
            "ADD".to_string(),
            Some(data),
        );

        let _entry = self.add_operation(operation.clone(), None).await?;

        // Retorna a operação que foi adicionada com sucesso
        // ***Em uma implementação mais sofisticada, poderíamos re-parsear a entrada
        // para garantir consistência, mas para este caso a operação original serve
        Ok(operation)
    }

    async fn get(
        &self,
        cid: cid::Cid,
    ) -> std::result::Result<crate::stores::operation::operation::Operation, Self::Error> {
        // Busca uma operação específica por CID

        // Primeiro, tenta usar o índice para busca otimizada
        if let Some(entry) = self.store.index().get_entry_by_cid(&cid) {
            // Converte a Entry para Operation usando parse_operation
            let operation = crate::stores::operation::operation::parse_operation(entry)
                .map_err(|e| GuardianError::Store(format!("Falha ao parsear entrada: {}", e)))?;
            return Ok(operation);
        }

        // Fallback: busca no oplog diretamente
        let oplog = self.store.op_log();
        let oplog_guard = oplog.read();

        // Busca linear no oplog por CID
        let cid_str = cid.to_string();
        for arc_entry in oplog_guard.values() {
            if arc_entry.hash() == cid_str {
                // Converte Entry para Operation
                let entry = arc_entry.as_ref().clone();
                let operation = crate::stores::operation::operation::parse_operation(entry)
                    .map_err(|e| {
                        GuardianError::Store(format!("Falha ao parsear entrada: {}", e))
                    })?;
                return Ok(operation);
            }
        }

        // CID não encontrado
        Err(GuardianError::Store(format!(
            "Operação não encontrada para CID: {}",
            cid
        )))
    }

    async fn list(
        &self,
        options: Option<crate::iface::StreamOptions>,
    ) -> std::result::Result<Vec<crate::stores::operation::operation::Operation>, Self::Error> {
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
            match crate::stores::operation::operation::parse_operation(entry) {
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

    fn index(&self) -> Box<dyn crate::iface::StoreIndex<Error = GuardianError> + Send + Sync> {
        self.store.index()
    }

    fn store_type(&self) -> &str {
        self.store.store_type()
    }

    fn replication_status(&self) -> crate::stores::replicator::replication_info::ReplicationInfo {
        self.store.replication_status()
    }

    fn replicator(&self) -> Option<Arc<crate::stores::replicator::replicator::Replicator>> {
        self.store.replicator()
    }

    fn cache(&self) -> Arc<dyn crate::data_store::Datastore> {
        self.store.cache()
    }

    async fn drop(&mut self) -> std::result::Result<(), Self::Error> {
        // Delega para BaseStore usando downcasting
        if let Some(_base_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            // BaseStore não tem um método drop específico, mas podemos fazer limpeza básica
            // ***Por enquanto, retorna sucesso pois a limpeza será feita automaticamente no Drop trait
            Ok(())
        } else {
            Err(GuardianError::Store(
                "Não foi possível fazer downcast para BaseStore".to_string(),
            ))
        }
    }

    async fn load(&mut self, amount: usize) -> std::result::Result<(), Self::Error> {
        // Delega para BaseStore usando downcasting
        if let Some(base_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            base_store.load(Some(amount as isize)).await
        } else {
            Err(GuardianError::Store(
                "Não foi possível fazer downcast para BaseStore".to_string(),
            ))
        }
    }

    async fn sync(
        &mut self,
        heads: Vec<crate::ipfs_log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        // Delega para BaseStore usando downcasting
        if let Some(base_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            base_store.sync(heads).await
        } else {
            Err(GuardianError::Store(
                "Não foi possível fazer downcast para BaseStore".to_string(),
            ))
        }
    }

    async fn load_more_from(&mut self, _amount: u64, entries: Vec<crate::ipfs_log::entry::Entry>) {
        // Delega para BaseStore usando downcasting
        if let Some(base_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            // BaseStore::load_more_from retorna Result<usize>, mas esta trait não espera retorno
            let _ = base_store.load_more_from(entries);
        } else {
            // Log do erro, mas não pode retornar erro porque a assinatura não permite
            eprintln!("Aviso: Não foi possível fazer downcast para BaseStore em load_more_from");
        }
    }

    async fn load_from_snapshot(&mut self) -> std::result::Result<(), Self::Error> {
        // Delega para BaseStore usando downcasting
        if let Some(base_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            base_store.load_from_snapshot().await
        } else {
            Err(GuardianError::Store(
                "Não foi possível fazer downcast para BaseStore".to_string(),
            ))
        }
    }

    fn op_log(&self) -> Arc<RwLock<crate::ipfs_log::log::Log>> {
        self.store.op_log()
    }

    fn ipfs(&self) -> Arc<IpfsClient> {
        unimplemented!("Adaptação entre tipos de cliente IPFS pendente")
    }

    fn db_name(&self) -> &str {
        self.store.db_name()
    }

    fn identity(&self) -> &crate::ipfs_log::identity::Identity {
        self.store.identity()
    }

    fn access_controller(&self) -> &dyn crate::access_controller::traits::AccessController {
        self.store.access_controller()
    }

    async fn add_operation(
        &mut self,
        op: crate::stores::operation::operation::Operation,
        on_progress_callback: Option<ProgressCallback>,
    ) -> std::result::Result<crate::ipfs_log::entry::Entry, Self::Error> {
        // Faz downcast para BaseStore e usa o método add_operation(&self)
        if let Some(base_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            base_store.add_operation(op, on_progress_callback).await
        } else {
            Err(GuardianError::Store(
                "Não foi possível fazer downcast para BaseStore".to_string(),
            ))
        }
    }

    fn span(&self) -> Arc<tracing::Span> {
        self.store.span()
    }

    fn tracer(&self) -> Arc<crate::iface::TracerWrapper> {
        self.store.tracer()
    }

    fn event_bus(&self) -> Arc<crate::pubsub::event::EventBus> {
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
            if let Ok(operation) =
                crate::stores::operation::operation::parse_operation(entry.clone())
            {
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
        &mut self,
        key: &str,
        value: Vec<u8>,
    ) -> std::result::Result<crate::stores::operation::operation::Operation, Self::Error> {
        // Cria uma operação PUT com chave e valor
        let operation = crate::stores::operation::operation::Operation::new(
            Some(key.to_string()),
            "PUT".to_string(),
            Some(value),
        );

        self.add_operation(operation.clone(), None).await?;
        Ok(operation)
    }

    async fn delete(
        &mut self,
        key: &str,
    ) -> std::result::Result<crate::stores::operation::operation::Operation, Self::Error> {
        // Cria uma operação DEL com chave
        let operation = crate::stores::operation::operation::Operation::new(
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
                if let Ok(operation) =
                    crate::stores::operation::operation::parse_operation(entry.clone())
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
        opts: &crate::iface::DocumentStoreGetOptions,
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
        opts: &crate::iface::DocumentStoreGetOptions,
    ) -> Result<Vec<Document>> {
        let oplog = self.store.op_log();
        let oplog_guard = oplog.read();

        let mut documents = Vec::new();

        // Itera através de todas as operações no oplog
        for arc_entry in oplog_guard.values() {
            let entry = arc_entry.as_ref().clone();

            // Converte Entry para Operation
            if let Ok(operation) = crate::stores::operation::operation::parse_operation(entry) {
                // Verifica se a operação é relevante para documentos
                if let Some(op_key) = operation.key() {
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

                    if matches && !operation.value().is_empty() {
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

        Ok(documents)
    }

    /// Coleta todos os documentos do índice para queries
    fn get_all_documents_from_index(&self) -> Result<Vec<Document>> {
        let index = self.store.index();
        let mut documents = Vec::new();

        let all_keys = index.keys().unwrap_or_default();

        for key in all_keys {
            if let Ok(Some(doc_bytes)) = index.get_bytes(&key) {
                match serde_json::from_slice::<serde_json::Value>(&doc_bytes) {
                    Ok(json_value) => {
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
            }
        }

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

    fn index(&self) -> Box<dyn crate::iface::StoreIndex<Error = GuardianError> + Send + Sync> {
        self.store.index()
    }

    fn store_type(&self) -> &str {
        self.store.store_type()
    }

    fn replication_status(&self) -> crate::stores::replicator::replication_info::ReplicationInfo {
        self.store.replication_status()
    }

    fn replicator(&self) -> Option<Arc<crate::stores::replicator::replicator::Replicator>> {
        self.store.replicator()
    }

    fn cache(&self) -> Arc<dyn crate::data_store::Datastore> {
        self.store.cache()
    }

    async fn drop(&mut self) -> std::result::Result<(), Self::Error> {
        // Delega para BaseStore usando downcasting
        if let Some(_base_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            // BaseStore não tem um método drop específico, mas podemos fazer limpeza básica
            // ***Por enquanto, retorna sucesso pois a limpeza será feita automaticamente no Drop trait
            Ok(())
        } else {
            Err(GuardianError::Store(
                "Não foi possível fazer downcast para BaseStore".to_string(),
            ))
        }
    }

    async fn load(&mut self, amount: usize) -> std::result::Result<(), Self::Error> {
        // Delega para BaseStore usando downcasting
        if let Some(base_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            base_store.load(Some(amount as isize)).await
        } else {
            Err(GuardianError::Store(
                "Não foi possível fazer downcast para BaseStore".to_string(),
            ))
        }
    }

    async fn sync(
        &mut self,
        heads: Vec<crate::ipfs_log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        // Delega para BaseStore usando downcasting
        if let Some(base_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            base_store.sync(heads).await
        } else {
            Err(GuardianError::Store(
                "Não foi possível fazer downcast para BaseStore".to_string(),
            ))
        }
    }

    async fn load_more_from(&mut self, _amount: u64, entries: Vec<crate::ipfs_log::entry::Entry>) {
        // Delega para BaseStore usando downcasting
        if let Some(base_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            // BaseStore::load_more_from retorna Result<usize>, mas esta trait não espera retorno
            let _ = base_store.load_more_from(entries);
        } else {
            // Log do erro, mas não pode retornar erro porque a assinatura não permite
            eprintln!("Aviso: Não foi possível fazer downcast para BaseStore em load_more_from");
        }
    }

    async fn load_from_snapshot(&mut self) -> std::result::Result<(), Self::Error> {
        // Delega para BaseStore usando downcasting
        if let Some(base_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            base_store.load_from_snapshot().await
        } else {
            Err(GuardianError::Store(
                "Não foi possível fazer downcast para BaseStore".to_string(),
            ))
        }
    }

    fn op_log(&self) -> Arc<RwLock<crate::ipfs_log::log::Log>> {
        self.store.op_log()
    }

    fn ipfs(&self) -> Arc<IpfsClient> {
        unimplemented!("Adapta��o entre tipos de cliente IPFS pendente")
    }

    fn db_name(&self) -> &str {
        self.store.db_name()
    }

    fn identity(&self) -> &crate::ipfs_log::identity::Identity {
        self.store.identity()
    }

    fn access_controller(&self) -> &dyn crate::access_controller::traits::AccessController {
        self.store.access_controller()
    }

    async fn add_operation(
        &mut self,
        op: crate::stores::operation::operation::Operation,
        on_progress_callback: Option<ProgressCallback>,
    ) -> std::result::Result<crate::ipfs_log::entry::Entry, Self::Error> {
        // Faz downcast para BaseStore e usa o método add_operation(&self)
        if let Some(base_store) =
            self.store
                .as_any()
                .downcast_ref::<crate::stores::base_store::base_store::BaseStore>()
        {
            base_store.add_operation(op, on_progress_callback).await
        } else {
            Err(GuardianError::Store(
                "Não foi possível fazer downcast para BaseStore".to_string(),
            ))
        }
    }

    fn span(&self) -> Arc<tracing::Span> {
        self.store.span()
    }

    fn tracer(&self) -> Arc<crate::iface::TracerWrapper> {
        self.store.tracer()
    }

    fn event_bus(&self) -> Arc<crate::pubsub::event::EventBus> {
        self.store.event_bus()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait::async_trait]
impl DocumentStore for DocumentStoreWrapper {
    async fn put(
        &mut self,
        document: Document,
    ) -> std::result::Result<crate::stores::operation::operation::Operation, Self::Error> {
        // Serializa o documento para bytes
        let data = if let Some(bytes) = document.downcast_ref::<Vec<u8>>() {
            bytes.clone()
        } else {
            // Para outros tipos, usa uma serialização genérica
            format!("{:?}", document).into_bytes()
        };

        // Cria uma operação PUT para documento
        let operation = crate::stores::operation::operation::Operation::new(
            None, // Documents podem não ter chave específica
            "PUT".to_string(),
            Some(data),
        );

        self.add_operation(operation.clone(), None).await?;
        Ok(operation)
    }

    async fn delete(
        &mut self,
        key: &str,
    ) -> std::result::Result<crate::stores::operation::operation::Operation, Self::Error> {
        // Cria uma operação DEL para documento
        let operation = crate::stores::operation::operation::Operation::new(
            Some(key.to_string()),
            "DEL".to_string(),
            None,
        );

        self.add_operation(operation.clone(), None).await?;
        Ok(operation)
    }

    async fn put_batch(
        &mut self,
        values: Vec<Document>,
    ) -> std::result::Result<crate::stores::operation::operation::Operation, Self::Error> {
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
        let operation = crate::stores::operation::operation::Operation::new(
            None,
            "PUT_BATCH".to_string(),
            Some(batch_data),
        );

        self.add_operation(operation.clone(), None).await?;
        Ok(operation)
    }

    async fn put_all(
        &mut self,
        values: Vec<Document>,
    ) -> std::result::Result<crate::stores::operation::operation::Operation, Self::Error> {
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
        let operation = crate::stores::operation::operation::Operation::new(
            None,
            "PUT_ALL".to_string(),
            Some(batch_data),
        );

        self.add_operation(operation.clone(), None).await?;
        Ok(operation)
    }

    async fn get(
        &self,
        key: &str,
        opts: Option<crate::iface::DocumentStoreGetOptions>,
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
