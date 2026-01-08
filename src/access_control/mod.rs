use crate::access_control::manifest::{CreateAccessControllerOptions, ManifestParams};
use crate::access_control::{
    acl_simple::SimpleAccessController, traits::AccessController,
    traits::Option as AccessControllerOption,
};
use crate::guardian::error::{GuardianError, Result};
use crate::traits::BaseGuardianDB;
use iroh_blobs::Hash;
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

pub mod acl_guardian;
pub mod acl_iroh;
pub mod acl_simple;
pub mod manifest;
pub mod traits;

/// Cria um novo controlador de acesso e retorna o Hash do seu manifesto.
///
/// # Argumentos
/// * `db` - Instância do BaseGuardianDB
/// * `controller_type` - Tipo do controlador ("simple", "guardian", "iroh")
/// * `params` - Parâmetros de configuração do controlador
/// * `options` - Opções adicionais para criação
///
/// # Retorna
/// * `Ok(Hash)` - Hash do manifesto criado
/// * `Err(GuardianError)` - Erro durante a criação
#[instrument(skip(db, params, _options), fields(controller_type = %controller_type))]
pub async fn create(
    db: Arc<dyn BaseGuardianDB<Error = GuardianError>>,
    controller_type: &str,
    params: CreateAccessControllerOptions,
    _options: AccessControllerOption,
) -> Result<Hash> {
    info!(target: "access_control_utils", controller_type = %controller_type, "Creating access controller");

    // Validação do tipo de controlador
    let controller_type_normalized = controller_type.to_lowercase();
    match controller_type_normalized.as_str() {
        "simple" | "guardian" | "iroh" => {}
        _ => {
            warn!(target: "access_control_utils", controller_type = %controller_type, "Unknown access controller type");
            return Err(GuardianError::Store(format!(
                "Unknown access controller type: {}",
                controller_type
            )));
        }
    }

    // Cria o controlador baseado no tipo
    let controller = create_controller(
        &controller_type_normalized,
        params.clone(),
        Some(db.client().as_ref()),
        Some(db.clone()),
    )
    .await?;

    // Salva o controlador e obtém o manifesto
    let _manifest_params = controller.save().await?;

    // Garante que o endereço termine com "/_access"
    let access_address = ensure_address(&controller_type_normalized);

    debug!(target: "access_control_utils",
        controller_type = %controller_type,
        address = %access_address,
        "Access controller created successfully"
    );
    let client = db.client();

    // Cria o manifesto
    let manifest_hash = crate::access_control::manifest::create(
        client,
        controller_type_normalized,
        &params,
    )
    .await
    .map_err(|e| {
        error!(target: "access_control_utils", error = %e, "Failed to create manifest in iroh");
        GuardianError::Store(format!(
            "Failed to create access controller manifest: {}",
            e
        ))
    })?;

    info!(target: "access_control_utils",
        hash = %hex::encode(manifest_hash.as_bytes()),
        controller_type = %controller_type,
        address = %access_address,
        "Access controller manifest created in iroh"
    );

    Ok(manifest_hash)
}

/// Resolve um controlador de acesso usando o endereço do seu manifesto.
///
/// # Argumentos
/// * `db` - Instância do BaseGuardianDB
/// * `manifest_address` - Endereço do manifesto do controlador
/// * `params` - Parâmetros de configuração
/// * `options` - Opções adicionais para resolução
///
/// # Retorna
/// * `Ok(Arc<dyn AccessController>)` - Controlador de acesso resolvido
/// * `Err(GuardianError)` - Erro durante a resolução
#[instrument(skip(db, params, _options), fields(manifest_address = %manifest_address))]
pub async fn resolve(
    db: Arc<dyn BaseGuardianDB<Error = GuardianError>>,
    manifest_address: &str,
    params: &CreateAccessControllerOptions,
    _options: AccessControllerOption,
) -> Result<Arc<dyn AccessController>> {
    info!(target: "access_control_utils", manifest_address = %manifest_address, "Resolving access controller");

    // Garante que o endereço termine com "/_access"
    let access_address = ensure_address(manifest_address);

    // Valida o endereço
    if access_address.is_empty() {
        return Err(GuardianError::Store(
            "Manifest address cannot be empty".to_string(),
        ));
    }

    debug!(target: "access_control_utils", address = %access_address, "Loading access controller manifest");

    // Carrega manifesto Client
    let client = db.client();

    // Tenta carregar o manifesto Client
    let manifest_result =
        crate::access_control::manifest::resolve(client, &access_address, params).await;

    let controller_type = match manifest_result {
        Ok(manifest) => {
            debug!(target: "access_control_utils",
                controller_type = %manifest.get_type,
                address = %access_address,
                "Loaded controller type from manifest"
            );
            manifest.get_type
        }
        Err(e) => {
            warn!(target: "access_control_utils",
                error = %e,
                address = %access_address,
                "Failed to load manifest, falling back to inference"
            );
            // Fallback: infere o tipo como antes se não conseguir carregar do Iroh
            infer_controller_type(&access_address, params)
        }
    };

    debug!(target: "access_control_utils",
        controller_type = %controller_type,
        address = %access_address,
        "Controller type determined"
    );

    // Cria o controlador baseado no tipo ou inferido
    let controller = create_controller(
        &controller_type,
        params.clone(),
        Some(db.client().as_ref()),
        Some(db.clone()),
    )
    .await?;

    // Carrega o estado do controlador usando o endereço
    if let Err(e) = controller.load(&access_address).await {
        warn!(target: "access_control_utils",
            error = %e,
            address = %access_address,
            "Failed to load controller state, using defaults"
        );
    }

    info!(target: "access_control_utils",
        controller_type = %controller_type,
        address = %access_address,
        "Access controller resolved successfully"
    );

    Ok(controller)
}

/// Garante que um endereço de controlador de acesso termine com "/_access".
/// Se o sufixo não estiver presente, ele é adicionado.
///
/// # Argumentos
/// * `address` - Endereço a ser validado/corrigido
///
/// # Retorna
/// * `String` - Endereço com sufixo "/_access" garantido
pub fn ensure_address(address: &str) -> String {
    // Remove espaços em branco das extremidades
    let address = address.trim();
    // Se o endereço está vazio, retorna apenas "_access"
    if address.is_empty() {
        return "_access".to_string();
    }
    // Verifica a última parte.
    // `split('/').next_back()` é mais eficiente que last() para DoubleEndedIterator.
    // Ex: "foo/bar/_access".split('/').next_back() -> Some("_access")
    // Ex: "foo/bar/_access/".split('/').next_back() -> Some("")
    if address.split('/').next_back() == Some("_access") {
        return address.to_string();
    }
    // Lida com a presença ou ausência de uma barra no final.
    if address.ends_with('/') {
        format!("{}{}", address, "_access")
    } else {
        format!("{}/{}", address, "_access")
    }
}

/// Função auxiliar para criar um controlador baseado no tipo
///
/// # Argumentos
/// * `controller_type` - Tipo do controlador ("simple", "guardian", "iroh")
/// * `params` - Parâmetros de configuração
/// * `client` - Cliente Iroh (opcional, necessário para tipo "iroh")
/// * `guardian_db` - Instância do GuardianDB (opcional, necessário para tipo "guardian")
///
/// # Retorna
/// * `Ok(Arc<dyn AccessController>)` - Controlador criado
/// * `Err(GuardianError)` - Erro durante a criação
#[instrument(skip(params, client, guardian_db))]
async fn create_controller(
    controller_type: &str,
    params: CreateAccessControllerOptions,
    client: Option<&crate::p2p::network::client::IrohClient>,
    guardian_db: Option<Arc<dyn BaseGuardianDB<Error = GuardianError>>>,
) -> Result<Arc<dyn AccessController>> {
    debug!(target: "access_control_utils", controller_type = %controller_type, "Creating access controller instance");

    match controller_type {
        "simple" => {
            let initial_keys = if params.get_all_access().is_empty() {
                // Se não há permissões definidas, cria permissões padrão
                let mut default_permissions = std::collections::HashMap::new();
                default_permissions.insert("write".to_string(), vec!["*".to_string()]);
                Some(default_permissions)
            } else {
                Some(params.get_all_access())
            };
            let controller = SimpleAccessController::new(initial_keys);
            Ok(Arc::new(controller) as Arc<dyn AccessController>)
        }
        "iroh" => {
            debug!(target: "access_control_utils", "Creating irohAccessController");

            // Verifica se o cliente foi fornecido
            let client = client.ok_or_else(|| {
                GuardianError::Store("Iroh client is required for IrohAccessController".to_string())
            })?;

            // Determina identity_id a partir dos parâmetros ou usa padrão
            let identity_id = if let Some(write_keys) = params.get_access("write") {
                if !write_keys.is_empty() {
                    write_keys[0].clone()
                } else {
                    "*".to_string()
                }
            } else {
                "*".to_string()
            };

            debug!(target: "access_control_utils",
                identity_id = %identity_id,
                "Creating irohAccessController with identity"
            );

            // Cria IrohAccessController
            let controller = crate::access_control::acl_iroh::IrohAccessController::new(
                Arc::new(client.clone()),
                identity_id,
                params,
            ).map_err(|e| {
                error!(target: "access_control_utils", error = %e, "Failed to create irohAccessController");
                GuardianError::Store(format!("Failed to create irohAccessController: {}", e))
            })?;

            info!(target: "access_control_utils", "irohAccessController created successfully");
            Ok(Arc::new(controller) as Arc<dyn AccessController>)
        }
        "guardian" => {
            debug!(target: "access_control_utils", "Creating GuardianDBAccessController");

            // Verifica se o GuardianDB foi fornecido
            let guardian_db_instance = guardian_db.ok_or_else(|| {
                GuardianError::Store(
                    "GuardianDB instance is required for GuardianDBAccessController".to_string(),
                )
            })?;

            // Cria um adapter que implementa GuardianDBKVStoreProvider
            let kv_provider = GuardianDBAdapter::new(guardian_db_instance);

            debug!(target: "access_control_utils", "Creating GuardianDBAccessController with adapter");

            // Cria GuardianDBAccessController
            let controller = crate::access_control::acl_guardian::GuardianDBAccessController::new(
                Arc::new(kv_provider),
                Box::new(params),
            ).await.map_err(|e| {
                error!(target: "access_control_utils", error = %e, "Failed to create GuardianDBAccessController");
                GuardianError::Store(format!("Failed to create GuardianDBAccessController: {}", e))
            })?;

            info!(target: "access_control_utils", "GuardianDBAccessController created successfully");
            Ok(Arc::new(controller) as Arc<dyn AccessController>)
        }
        _ => {
            error!(target: "access_control_utils", controller_type = %controller_type, "Unsupported access controller type");
            Err(GuardianError::Store(format!(
                "Unsupported access controller type: {}",
                controller_type
            )))
        }
    }
}

/// Função auxiliar para inferir o tipo de controlador baseado no endereço/parâmetros
///
/// # Argumentos
/// * `address` - Endereço do manifesto
/// * `params` - Parâmetros de configuração
///
/// # Retorna
/// * `String` - Tipo do controlador inferido
pub fn infer_controller_type(address: &str, params: &CreateAccessControllerOptions) -> String {
    // Verifica se há um tipo explícito nos parâmetros
    let explicit_type = params.get_type();
    if !explicit_type.is_empty() {
        return explicit_type.to_string();
    }
    // Infere baseado no endereço
    if address.contains("/guardian/") || address.contains("guardian_") {
        return "guardian".to_string();
    }
    if address.contains("/iroh/") || address.contains("iroh_") {
        return "iroh".to_string();
    }
    // Padrão para SimpleAccessController
    "simple".to_string()
}

/// Valida um endereço de controlador de acesso
///
/// # Argumentos
/// * `address` - Endereço a ser validado
///
/// # Retorna
/// * `Ok(())` - Endereço válido
/// * `Err(GuardianError)` - Endereço inválido
pub fn validate_address(address: &str) -> Result<()> {
    if address.trim().is_empty() {
        return Err(GuardianError::Store("Address cannot be empty".to_string()));
    }
    // Verifica caracteres inválidos
    if address.contains("..") || address.contains("//") {
        return Err(GuardianError::Store(
            "Address contains invalid path components".to_string(),
        ));
    }
    // Verifica comprimento máximo
    if address.len() > 1000 {
        return Err(GuardianError::Store(
            "Address is too long (max 1000 characters)".to_string(),
        ));
    }

    Ok(())
}

/// Lista os tipos de controladores de acesso disponíveis
///
/// # Retorna
/// * `Vec<String>` - Lista dos tipos disponíveis
pub fn list_available_types() -> Vec<String> {
    vec![
        "simple".to_string(),
        "guardian".to_string(),
        "iroh".to_string(),
    ]
}

/// Verifica se um tipo de controlador é suportado
///
/// # Argumentos
/// * `controller_type` - Tipo a ser verificado
///
/// # Retorna
/// * `bool` - true se suportado, false caso contrário
pub fn is_supported_type(controller_type: &str) -> bool {
    list_available_types().contains(&controller_type.to_lowercase())
}

/// Adapter que permite usar BaseGuardianDB onde GuardianDBKVStoreProvider é esperado
pub struct GuardianDBAdapter {
    base_db: Arc<dyn BaseGuardianDB<Error = GuardianError>>,
}

impl GuardianDBAdapter {
    pub fn new(base_db: Arc<dyn BaseGuardianDB<Error = GuardianError>>) -> Self {
        Self { base_db }
    }
}

#[async_trait::async_trait]
impl crate::traits::GuardianDBKVStoreProvider for GuardianDBAdapter {
    type Error = GuardianError;

    async fn key_value(
        &self,
        address: &str,
        options: &mut crate::traits::CreateDBOptions,
    ) -> std::result::Result<
        Box<dyn crate::traits::KeyValueStore<Error = GuardianError>>,
        Self::Error,
    > {
        // Usa o método create do BaseGuardianDB para criar um KeyValueStore
        let store = self.base_db.create(address, "keyvalue", options).await?;

        // Converte para KeyValueStore usando um wrapper
        Ok(Box::new(KeyValueStoreAdapter::new(store)))
    }
}

/// Adapter que converte Store genérico para KeyValueStore específico
pub struct KeyValueStoreAdapter {
    store: Arc<dyn crate::traits::Store<Error = GuardianError>>,
}

impl KeyValueStoreAdapter {
    pub fn new(store: Arc<dyn crate::traits::Store<Error = GuardianError>>) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl crate::traits::Store for KeyValueStoreAdapter {
    type Error = GuardianError;

    fn address(&self) -> &dyn crate::address::Address {
        self.store.address()
    }

    fn store_type(&self) -> &str {
        self.store.store_type()
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        // Fechamento usando interior mutability
        // Sinalizar fechamento através do event bus
        let event_bus = self.store.event_bus();

        // Cria evento de fechamento
        let close_event = serde_json::json!({
            "event": "store_closed",
            "address": self.store.address().to_string(),
            "timestamp": chrono::Utc::now().to_rfc3339()
        });

        // Emite evento de fechamento (não crítico se falhar)
        if let Ok(emitter) = event_bus.emitter::<serde_json::Value>().await {
            let _ = emitter.emit(close_event);
        }

        // Log do fechamento
        tracing::info!("Store adapter closed: {}", self.store.address());
        Ok(())
    }

    async fn drop(&self) -> std::result::Result<(), Self::Error> {
        // Drop com limpeza de recursos
        // Primeiro fecha normalmente
        self.close().await?;

        // Realiza limpeza adicional específica do drop
        let op_log = self.store.op_log();

        // Força flush do log se possível (usando try_write para evitar deadlock)
        if let Some(log_guard) = op_log.try_write() {
            // Garante que todas as operações pendentes sejam persistidas
            // (O log já gerencia sua própria persistência, apenas sinalizamos)
            drop(log_guard);
        }

        tracing::debug!("Store adapter dropped: {}", self.store.address());
        Ok(())
    }

    fn events(&self) -> &dyn crate::events::EmitterInterface {
        // events() está deprecated
        unimplemented!("events() is deprecated, use event_bus() instead")
    }

    fn index(&self) -> Box<dyn crate::traits::StoreIndex<Error = Self::Error> + Send + Sync> {
        self.store.index()
    }

    fn replication_status(&self) -> crate::stores::replicator::replication_info::ReplicationInfo {
        self.store.replication_status()
    }

    fn cache(&self) -> Arc<dyn crate::data_store::Datastore> {
        self.store.cache()
    }

    async fn load(&self, amount: usize) -> std::result::Result<(), Self::Error> {
        // Implementação do load usando o Client do store
        let client = self.store.client();
        let op_log = self.store.op_log();

        // Carrega entradas do Iroh até o limite especificado
        let mut loaded_count = 0;

        // Obtém heads atuais do log
        let heads = {
            let log_guard = op_log.read();
            log_guard.heads().clone()
        };

        // Para cada head, carrega entradas do Iroh
        for head_entry in heads {
            if loaded_count >= amount {
                break;
            }

            // Tenta carregar entrada do Iroh usando cat_bytes com hash string
            let head_hash = head_entry.hash();
            let head_hash_str = hex::encode(head_hash.as_bytes());
            if let Ok(data) = client.cat_bytes(&head_hash_str).await {
                // Processa dados carregados usando postcard
                if let Ok(entry) =
                    crate::guardian::serializer::deserialize::<crate::log::entry::Entry>(&data)
                {
                    // Adiciona entrada ao log se ainda não existe
                    let entry_hash = entry.hash();
                    {
                        let mut log_guard = op_log.write();
                        if !log_guard.has(entry_hash) {
                            // Serializa entry para adicionar ao log
                            let entry_bytes =
                                crate::guardian::serializer::serialize(&entry).unwrap_or_default();
                            log_guard.append(&String::from_utf8_lossy(&entry_bytes), None);
                            loaded_count += 1;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn sync(
        &self,
        heads: Vec<crate::log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        // Implementação do sync com as heads fornecidas
        let op_log = self.store.op_log();
        let client = self.store.client();

        // Para cada head fornecida, sincroniza as entradas
        for head_entry in heads {
            // Verifica se já temos esta entrada
            {
                let log_guard = op_log.read();
                if log_guard.has(head_entry.hash()) {
                    continue; // Já temos esta entrada
                }
            }

            // Carrega entrada e suas dependências do Iroh
            let mut entries_to_add = Vec::new();
            let mut queue = vec![head_entry.clone()];

            while let Some(entry) = queue.pop() {
                entries_to_add.push(entry.clone());

                // Carrega entradas pai (next)
                for next_hash in &entry.next {
                    // Usa hash string diretamente com cat_bytes
                    let next_hash_str = hex::encode(next_hash.as_bytes());
                    if let Ok(data) = client.cat_bytes(&next_hash_str).await
                        && let Ok(parent_entry) = crate::guardian::serializer::deserialize::<
                            crate::log::entry::Entry,
                        >(&data)
                    {
                        let log_guard = op_log.read();
                        if !log_guard.has(parent_entry.hash()) {
                            drop(log_guard);
                            queue.push(parent_entry);
                        }
                    }
                }
            }

            // Adiciona todas as entradas ao log em ordem reversa
            {
                let mut log_guard = op_log.write();
                for entry in entries_to_add.iter().rev() {
                    if !log_guard.has(entry.hash()) {
                        let entry_bytes =
                            crate::guardian::serializer::serialize(entry).unwrap_or_default();
                        log_guard.append(&String::from_utf8_lossy(&entry_bytes), None);
                    }
                }
            }
        }

        Ok(())
    }

    async fn load_more_from(&self, amount: u64, entries: Vec<crate::log::entry::Entry>) {
        // Implementação do load_more_from partindo das entradas fornecidas
        let op_log = self.store.op_log();
        let client = self.store.client();
        let mut loaded_count = 0u64;

        for entry in entries {
            if loaded_count >= amount {
                break;
            }

            // Carrega entradas anteriores (next) recursivamente
            for next_hash in &entry.next {
                if loaded_count >= amount {
                    break;
                }

                // Usa hash string diretamente com cat_bytes
                let next_hash_str = hex::encode(next_hash.as_bytes());
                if let Ok(data) = client.cat_bytes(&next_hash_str).await
                    && let Ok(parent_entry) =
                        crate::guardian::serializer::deserialize::<crate::log::entry::Entry>(&data)
                {
                    // Verifica se já temos esta entrada
                    let should_add = {
                        let log_guard = op_log.read();
                        !log_guard.has(parent_entry.hash())
                    };

                    if should_add {
                        // Adiciona entrada ao log usando try_write
                        if let Some(mut log_guard) = op_log.try_write() {
                            let entry_bytes = crate::guardian::serializer::serialize(&parent_entry)
                                .unwrap_or_default();
                            log_guard.append(&String::from_utf8_lossy(&entry_bytes), None);
                            loaded_count += 1;
                        }
                    }
                }
            }
        }
    }

    async fn load_from_snapshot(&self) -> std::result::Result<(), Self::Error> {
        // Implementação do load_from_snapshot
        let client = self.store.client();
        let op_log = self.store.op_log();
        let store_address = self.store.address();
        let snapshot_path = format!("{}/snapshot", store_address);

        // Tenta carregar snapshot do Iroh usando cat_bytes
        if let Ok(snapshot_data) = client.cat_bytes(&snapshot_path).await
            && let Ok(snapshot) = crate::guardian::serializer::deserialize::<
                Vec<crate::log::entry::Entry>,
            >(&snapshot_data)
        {
            // Carrega todas as entradas do snapshot
            let mut log_guard = op_log.write();
            for entry in &snapshot {
                if !log_guard.has(entry.hash()) {
                    let entry_bytes =
                        crate::guardian::serializer::serialize(entry).unwrap_or_default();
                    log_guard.append(&String::from_utf8_lossy(&entry_bytes), None);
                }
            }

            drop(log_guard);

            // Log do sucesso do carregamento
            tracing::info!(
                "Successfully loaded {} entries from snapshot",
                snapshot.len()
            );

            return Ok(());
        }

        // Se não há snapshot, retornar OK (não é erro)
        Ok(())
    }

    fn op_log(&self) -> Arc<parking_lot::RwLock<crate::log::Log>> {
        self.store.op_log()
    }

    fn client(&self) -> Arc<crate::p2p::network::client::IrohClient> {
        self.store.client()
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
        on_progress_callback: Option<crate::traits::ProgressCallback>,
    ) -> std::result::Result<crate::log::entry::Entry, Self::Error> {
        // Implementação do add_operation
        // Como temos Arc<Store>, usamos interior mutability através do oplog.
        let op_log = self.store.op_log();
        let identity = self.store.identity();
        let client = self.store.client();

        // Serializar operação usando postcard
        let payload = crate::guardian::serializer::serialize(&op)
            .map_err(|e| GuardianError::Store(format!("Failed to serialize operation: {}", e)))?;

        // Obtem heads atuais
        let heads = {
            let log_guard = op_log.read();
            log_guard.heads()
        };

        let store_id = self.store.db_name();
        let next_hashes: Vec<crate::log::entry::EntryOrHash> = heads
            .iter()
            .map(|entry| crate::log::entry::EntryOrHash::Entry(entry.as_ref()))
            .collect();

        let entry = crate::log::entry::Entry::new(
            identity.clone(),
            store_id,
            &payload,
            &next_hashes,
            None, // clock
        );

        // Armazena entrada no Iroh usando add_bytes
        let entry_data = crate::guardian::serializer::serialize(&entry)
            .map_err(|e| GuardianError::Store(format!("Failed to serialize entry: {}", e)))?;

        let _add_response = client
            .add_bytes(entry_data.clone())
            .await
            .map_err(|e| GuardianError::Store(format!("Failed to store entry: {}", e)))?;

        // Adiciona entrada ao log usando append correto
        {
            let mut log_guard = op_log.write();
            let entry_str = String::from_utf8_lossy(&entry_data).to_string();
            log_guard.append(&entry_str, None);
        }

        // Chamar callback de progresso se fornecido
        if let Some(callback) = on_progress_callback {
            // Envia entrada através do canal
            if (callback.send(entry.clone()).await).is_err() {
                // Se falhar, apenas avisa
                tracing::warn!("Failed to send progress callback");
            }
        }

        Ok(entry)
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
impl crate::traits::KeyValueStore for KeyValueStoreAdapter {
    async fn put(
        &self,
        key: &str,
        value: Vec<u8>,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Operação put
        let operation = crate::stores::operation::Operation::new(
            Some(key.to_string()),
            "PUT".to_string(),
            Some(value),
        );

        // Como temos Arc<Store>, usamos interior mutability através do oplog.
        // Operação persistindo diretamente no log do store.
        let op_log = self.store.op_log();

        // Serializa operação com postcard e converte para string lossy
        let operation_bytes = crate::guardian::serializer::serialize(&operation)
            .map_err(|e| GuardianError::Store(format!("Failed to serialize operation: {}", e)))?;
        let entry_data = String::from_utf8_lossy(&operation_bytes).to_string();

        {
            let mut log_guard = op_log.write();
            log_guard.append(&entry_data, None);
        }

        Ok(operation)
    }

    async fn get(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
        // Busca no oplog do store por entradas que contenham a chave
        let op_log = self.store.op_log();
        let log_guard = op_log.read();

        // Procura pela entrada mais recente com a chave
        for entry in log_guard.values().into_iter().rev() {
            let operation_data = entry.payload();
            // Desserializa a operação usando postcard
            if let Ok(operation) = crate::guardian::serializer::deserialize::<
                crate::stores::operation::Operation,
            >(operation_data)
                && let Some(op_key) = &operation.key
            {
                if op_key == key && operation.op == "PUT" {
                    return Ok(Some(operation.value));
                } else if op_key == key && operation.op == "DELETE" {
                    return Ok(None);
                }
            }
        }

        Ok(None)
    }

    async fn delete(
        &self,
        key: &str,
    ) -> std::result::Result<crate::stores::operation::Operation, Self::Error> {
        // Operação delete
        let operation = crate::stores::operation::Operation::new(
            Some(key.to_string()),
            "DELETE".to_string(),
            None,
        );

        // Como temos Arc<Store>, usamos interior mutability através do oplog.
        // Operação persistindo diretamente no log do store
        let op_log = self.store.op_log();

        // Serializa operação com postcard e converte para string lossy
        let operation_bytes = crate::guardian::serializer::serialize(&operation)
            .map_err(|e| GuardianError::Store(format!("Failed to serialize operation: {}", e)))?;
        let entry_data = String::from_utf8_lossy(&operation_bytes).to_string();

        {
            let mut log_guard = op_log.write();
            log_guard.append(&entry_data, None);
        }

        Ok(operation)
    }

    fn all(&self) -> std::collections::HashMap<String, Vec<u8>> {
        // Constrói HashMap com todos os pares chave-valor do store
        let mut result = std::collections::HashMap::new();
        let op_log = self.store.op_log();
        let log_guard = op_log.read();

        // Processa todas as entradas do oplog
        for entry in log_guard.values() {
            let operation_data = entry.payload();
            // Desserializa a operação usando postcard
            if let Ok(operation) = crate::guardian::serializer::deserialize::<
                crate::stores::operation::Operation,
            >(operation_data)
                && let Some(key) = &operation.key
            {
                match operation.op.as_str() {
                    "PUT" => {
                        result.insert(key.clone(), operation.value);
                    }
                    "DELETE" => {
                        result.remove(key);
                    }
                    _ => {} // Ignora outras operações
                }
            }
        }

        result
    }
}
