use crate::access_controller::manifest::CreateAccessControllerOptions;
use crate::access_controller::{manifest::ManifestParams, utils};
use crate::address::Address;
use crate::error::{GuardianError, Result};
use crate::ipfs_log::{access_controller, identity_provider::IdentityProvider};
use crate::p2p::events::{Emitter, EventBus};
use crate::traits::{CreateDBOptions, GuardianDBKVStoreProvider, KeyValueStore};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{Span, debug, instrument, warn};

// Type alias para simplificar tipos complexos
type KVStoreType =
    RwLock<Option<Arc<tokio::sync::Mutex<Box<dyn KeyValueStore<Error = GuardianError>>>>>>;

// Simple string wrapper that implements Address for return values
#[derive(Debug, Clone)]
struct StringAddress(String);

impl std::fmt::Display for StringAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Address for StringAddress {
    fn get_root(&self) -> cid::Cid {
        cid::Cid::default() // Default CID for string addresses
    }

    fn get_path(&self) -> &str {
        &self.0
    }

    fn equals(&self, other: &dyn Address) -> bool {
        format!("{}", self) == format!("{}", other)
    }
}

#[derive(Debug, Clone)]
pub struct EventUpdated {
    pub controller_type: String,
    pub address: String,
    pub action: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl EventUpdated {
    pub fn new(controller_type: String, address: String, action: String) -> Self {
        Self {
            controller_type,
            address,
            action,
            timestamp: chrono::Utc::now(),
        }
    }
}

pub struct GuardianDBAccessController {
    /// EventBus para emitir eventos de access controller
    event_bus: EventBus,

    /// Emitter type-safe para eventos de atualização (com mutabilidade interior)
    event_emitter: Arc<tokio::sync::Mutex<Option<Emitter<EventUpdated>>>>,

    /// Uma referência compartilhada à instância principal do GuardianDB.
    guardian_db: Arc<dyn GuardianDBKVStoreProvider<Error = GuardianError>>,

    /// O armazém de chave-valor para as permissões. Envolto em RwLock e Option
    /// porque pode ser substituído dinamicamente pela função `load`.
    kv_store: KVStoreType,

    /// Opções de manifesto.
    options: Box<dyn ManifestParams>,

    /// Span para contexto de tracing estruturado.
    span: Span,
}

impl GuardianDBAccessController {
    /// Retorna uma referência ao span para contexto de tracing
    pub fn span(&self) -> &Span {
        &self.span
    }

    pub fn get_type(&self) -> &'static str {
        "GuardianDB"
    }

    pub async fn address(&self) -> Option<Box<dyn Address>> {
        let store_guard = self.kv_store.read().await;
        // Retorna o endereço do kv_store, se ele existir.
        if let Some(store_arc) = store_guard.as_ref() {
            let store = store_arc.lock().await;
            let addr = store.address();
            let addr_string = format!("{}", addr);
            Some(Box::new(StringAddress(addr_string)) as Box<dyn Address>)
        } else {
            None
        }
    }

    pub async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>> {
        let authorizations = self.get_authorizations().await?;

        // Retorna a lista de chaves para a "role", ou uma lista vazia se a "role" não existir.
        Ok(authorizations.get(role).cloned().unwrap_or_default())
    }

    async fn get_authorizations(&self) -> Result<HashMap<String, Vec<String>>> {
        let mut authorizations_set: HashMap<String, HashSet<String>> = HashMap::new();

        let store_guard = self.kv_store.read().await;
        let store = match store_guard.as_ref() {
            Some(s) => s,
            // Se não há store, não há autorizações.
            None => return Ok(HashMap::new()),
        };

        // Implementa store.all() para recuperar todas as autorizações persistidas
        let store_lock = store.lock().await;
        let all_data = store_lock.all();

        for (role, key_bytes) in all_data {
            let authorized_keys: Vec<String> = serde_json::from_slice(&key_bytes)?;

            let entry = authorizations_set.entry(role).or_default();
            for key in authorized_keys {
                entry.insert(key);
            }
        }

        // Se a permissão 'write' existe, concede as mesmas chaves para 'admin'.
        if let Some(write_keys) = authorizations_set.get("write").cloned() {
            let admin_keys = authorizations_set.entry("admin".to_string()).or_default();
            for key in write_keys.iter() {
                admin_keys.insert(key.clone());
            }
        }

        // Converte os valores de HashSet para Vec<String> no mapa final.
        let authorizations_list = authorizations_set
            .into_iter()
            .map(|(permission, keys)| (permission, keys.into_iter().collect()))
            .collect();

        Ok(authorizations_list)
    }

    #[instrument(skip(self, entry, identity_provider, _additional_context))]
    pub async fn can_append(
        &self,
        entry: &dyn access_controller::LogEntry,
        identity_provider: &dyn IdentityProvider,
        _additional_context: &dyn access_controller::CanAppendAdditionalContext,
    ) -> Result<()> {
        let write_access = self.get_authorized_by_role("write").await?;
        let admin_access = self.get_authorized_by_role("admin").await?;

        let access: HashSet<String> = write_access
            .into_iter()
            .chain(admin_access.into_iter())
            .collect();

        let entry_id = entry.get_identity().id();

        // Verifica se a chave universal ("*") ou a ID específica da entrada está presente.
        if access.contains(entry_id) || access.contains("*") {
            identity_provider
                .verify_identity(entry.get_identity())
                .await?;
            return Ok(());
        }

        Err(GuardianError::Store("Não autorizado".to_string()))
    }

    #[allow(dead_code)]
    #[instrument(skip(self), fields(capability = %capability, key_id = %key_id))]
    pub async fn grant(&self, capability: &str, key_id: &str) -> Result<()> {
        // Primeiro, executa as operações que precisam do store
        {
            let store_guard = self.kv_store.read().await;
            let store_arc = store_guard
                .as_ref()
                .ok_or_else(|| GuardianError::Store("kv_store não inicializado".to_string()))?;

            // Usa um HashSet para evitar duplicatas automaticamente.
            let mut capabilities: HashSet<String> = self
                .get_authorized_by_role(capability)
                .await?
                .into_iter()
                .collect();

            capabilities.insert(key_id.to_string());

            let capabilities_vec: Vec<String> = capabilities.into_iter().collect();

            let capabilities_json = serde_json::to_vec(&capabilities_vec)?;

            // Implementa operações de store para persistir as permissões
            let mut store = store_arc.lock().await;
            store
                .put(capability, capabilities_json)
                .await
                .map_err(|e| GuardianError::Store(format!("Erro ao salvar no store: {}", e)))?;
        } // store_guard é liberado aqui

        // Depois, emite evento de atualização usando EventBus
        self.on_update("grant", capability, key_id).await;

        Ok(())
    }

    #[allow(dead_code)]
    #[instrument(skip(self), fields(capability = %capability, key_id = %key_id))]
    pub async fn revoke(&self, capability: &str, key_id: &str) -> Result<()> {
        // Primeiro, executa as operações que precisam do store
        {
            let store_guard = self.kv_store.read().await;
            let store_arc = store_guard
                .as_ref()
                .ok_or_else(|| GuardianError::Store("kv_store não inicializado".to_string()))?;

            let mut capabilities: Vec<String> = self.get_authorized_by_role(capability).await?;

            // Remove a chave, se ela existir.
            capabilities.retain(|id| id != key_id);

            let mut store = store_arc.lock().await;
            if !capabilities.is_empty() {
                let capabilities_json = serde_json::to_vec(&capabilities)?;

                // Implementa operações de store para persistir as permissões
                store
                    .put(capability, capabilities_json)
                    .await
                    .map_err(|e| {
                        GuardianError::Store(format!("Erro ao persistir permissões: {}", e))
                    })?;
            } else {
                // Remove a entrada completamente se não há mais permissões
                store.delete(capability).await.map_err(|e| {
                    GuardianError::Store(format!("Erro ao remover permissões: {}", e))
                })?;
            }
        } // store_guard é liberado aqui

        // Depois, emite evento de atualização usando EventBus
        self.on_update("revoke", capability, key_id).await;

        Ok(())
    }

    #[instrument(skip(self), fields(address = %address))]
    pub async fn load(&self, address: &str) -> Result<()> {
        let mut store_guard = self.kv_store.write().await;
        // Fecha qualquer store existente antes de carregar um novo.
        if let Some(_store) = store_guard.take() {
            // Ignora erro no close por enquanto
        }

        let write_access = self.options.get_access("admin");
        let write_access = match write_access {
            Some(access) if !access.is_empty() => access,
            _ => {
                // Fix identity access - usa o ID da identidade do provedor
                vec!["*".to_string()] // Permite acesso universal como fallback
            }
        };

        let db_address = utils::ensure_address(address);

        let mut store_options = CreateDBOptions::default();
        // Configura o access controller para o store
        let ipfs_ac_params = CreateAccessControllerOptions::new_simple("ipfs".to_string(), {
            let mut access = HashMap::new();
            access.insert("write".to_string(), write_access);
            access
        });
        store_options.access_controller = Some(Box::new(ipfs_ac_params));

        // Operações de store para carregar o key-value store
        let store = self
            .guardian_db
            .key_value(&db_address, &mut store_options)
            .await
            .map_err(|e| GuardianError::Store(format!("Erro ao abrir key-value store: {}", e)))?;

        // Salva o novo store
        *store_guard = Some(Arc::new(tokio::sync::Mutex::new(store)));

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn save(&self) -> Result<Box<dyn ManifestParams>> {
        let store_guard = self.kv_store.read().await;
        let store_arc = store_guard
            .as_ref()
            .ok_or_else(|| GuardianError::Store("kv_store não inicializado".to_string()))?;

        let store = store_arc.lock().await;
        let addr = store.address();
        let addr_string = format!("{}", addr);

        debug!(target: "access_controller", address = %addr_string, "Save executado para o store");

        // Cria o manifesto baseado no endereço do store
        // Use default CID since string->CID conversion isn't trivial
        let cid = cid::Cid::default();

        // Assume um construtor que cria um manifesto 'GuardianDB' a partir de um CID.
        let params = CreateAccessControllerOptions::new(cid, false, "GuardianDB".to_string());
        Ok(Box::new(params))
    }

    #[instrument(skip(self))]
    pub async fn close(&self) -> Result<()> {
        let mut store_guard = self.kv_store.write().await;
        if let Some(store_arc) = store_guard.take() {
            // Fecha o store usando o método close() da trait Store
            let store = store_arc.lock().await;
            match store.close().await {
                Ok(_) => debug!(target: "access_controller", "Store fechado com sucesso"),
                Err(e) => warn!(target: "access_controller", error = %e, "Erro ao fechar o store"),
            }
        }
        Ok(())
    }

    async fn on_update(&self, action: &str, capability: &str, key_id: &str) {
        let mut emitter_guard = self.event_emitter.lock().await;

        // Inicializa o emitter se não existir
        if emitter_guard.is_none() {
            match self.event_bus.emitter::<EventUpdated>().await {
                Ok(emitter) => {
                    *emitter_guard = Some(emitter);
                }
                Err(e) => {
                    warn!(target: "GuardianDB::ac", error = %e, "Falha ao inicializar event emitter");
                    return;
                }
            }
        }

        // Emite o evento usando EventBus
        if let Some(emitter) = emitter_guard.as_ref() {
            let address = self
                .address()
                .await
                .map(|addr| format!("{}", addr))
                .unwrap_or_else(|| "unknown".to_string());

            let event = EventUpdated::new(
                "guardian".to_string(),
                address,
                format!("{}:{}:{}", action, capability, key_id),
            );

            if let Err(e) = emitter.emit(event) {
                warn!(target: "GuardianDB::ac", error = %e, "Falha ao emitir evento de atualização");
            } else {
                debug!(target: "GuardianDB::ac", action = %action, capability = %capability, key_id = %key_id, "Evento emitido com sucesso");
            }
        }
    }

    #[instrument(skip(guardian_db, params))]
    pub async fn new(
        guardian_db: Arc<dyn GuardianDBKVStoreProvider<Error = GuardianError>>,
        params: Box<dyn crate::access_controller::manifest::ManifestParams>,
    ) -> std::result::Result<Self, GuardianError> {
        let kv_provider = guardian_db;
        let addr_str = if !params.address().to_string().is_empty() {
            params.address().to_string()
        } else {
            "default-access-controller".to_string()
        };

        let mut opts = CreateDBOptions::default();
        let kv_store = kv_provider
            .key_value(&addr_str, &mut opts)
            .await
            .map_err(|e| {
                GuardianError::Store(format!("Erro ao inicializar key-value store: {}", e))
            })?;

        debug!(target: "access_controller", address = %addr_str, "Key-value store inicializada");

        // Usa nosso EventBus para emitir eventos type-safe
        let event_bus = EventBus::new();
        let _write_access = params.get_access("write");
        let controller = Self {
            event_bus,
            event_emitter: Arc::new(tokio::sync::Mutex::new(None)), // Será inicializado lazy quando necessário
            guardian_db: kv_provider,
            kv_store: RwLock::new(Some(Arc::new(tokio::sync::Mutex::new(kv_store)))), // Inicializa com o store
            options: params,
            // Cria um span para contexto de tracing
            span: tracing::info_span!("guardian_access_controller", address = %addr_str),
        };

        // Inicializa permissões básicas se necessário
        let write_access = controller.options.get_access("write");
        if let Some(access_keys) = write_access {
            for key in access_keys {
                controller.grant("write", &key).await?;
            }
        }

        Ok(controller)
    }
}

// Implementação da trait AccessController para GuardianDBAccessController
#[async_trait::async_trait]
impl crate::access_controller::traits::AccessController for GuardianDBAccessController {
    fn get_type(&self) -> &str {
        "guardian"
    }

    async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>> {
        self.get_authorized_by_role(role).await
    }

    async fn grant(&self, capability: &str, key_id: &str) -> Result<()> {
        self.grant(capability, key_id).await
    }

    async fn revoke(&self, capability: &str, key_id: &str) -> Result<()> {
        self.revoke(capability, key_id).await
    }

    async fn load(&self, address: &str) -> Result<()> {
        self.load(address).await
    }

    async fn save(&self) -> Result<Box<dyn crate::access_controller::manifest::ManifestParams>> {
        self.save().await
    }

    async fn close(&self) -> Result<()> {
        self.close().await
    }

    async fn can_append(
        &self,
        entry: &dyn crate::ipfs_log::access_controller::LogEntry,
        identity_provider: &dyn crate::ipfs_log::identity_provider::IdentityProvider,
        additional_context: &dyn crate::ipfs_log::access_controller::CanAppendAdditionalContext,
    ) -> Result<()> {
        self.can_append(entry, identity_provider, additional_context)
            .await
    }
}
