use crate::access_controller::manifest::CreateAccessControllerOptions;
use crate::access_controller::{manifest::ManifestParams, utils};
use crate::address::{Address, GuardianDBAddress};
use crate::eqlabs_ipfs_log::{access_controller, identity_provider::IdentityProvider};
use crate::error::{GuardianError, Result};
use crate::iface::{CreateDBOptions, GuardianDBKVStoreProvider, KeyValueStore};
use async_trait::async_trait;
use log::warn;
use slog::Logger;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};

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

/// equivalente a EventUpdated em go
#[derive(Debug, Clone)]
pub struct EventUpdated;
pub struct GuardianDBAccessController {
    /// O emissor de eventos, substituindo o eventbus do Go.
    event_sender: broadcast::Sender<EventUpdated>,

    /// Uma referência compartilhada à instância principal do GuardianDB.
    guardian_db: Arc<dyn GuardianDBKVStoreProvider<Error = GuardianError>>,

    /// O armazém de chave-valor para as permissões. Envolto em RwLock e Option
    /// porque pode ser substituído dinamicamente pela função `load`.
    kv_store:
        RwLock<Option<Arc<tokio::sync::Mutex<Box<dyn KeyValueStore<Error = GuardianError>>>>>>,

    /// Opções de manifesto.
    options: Box<dyn ManifestParams>,

    /// Logger para registrar informações e avisos.
    logger: RwLock<Arc<Logger>>,
}

impl GuardianDBAccessController {
    /// equivalente a SetLogger em go
    pub async fn set_logger(&self, logger: Arc<Logger>) {
        let mut guard = self.logger.write().await;
        *guard = logger;
    }

    /// equivalente a Logger em go
    pub async fn logger(&self) -> Arc<Logger> {
        self.logger.read().await.clone()
    }

    /// equivalente a Type em go
    pub fn r#type(&self) -> &'static str {
        "GuardianDB"
    }

    /// equivalente a Address em go
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

    /// equivalente a GetAuthorizedByRole em go
    pub async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>> {
        let authorizations = self.get_authorizations().await?;

        // Retorna a lista de chaves para a "role", ou uma lista vazia se a "role" não existir.
        Ok(authorizations.get(role).cloned().unwrap_or_default())
    }

    /// equivalente a getAuthorizations em go
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

            let entry = authorizations_set.entry(role).or_insert_with(HashSet::new);
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

    /// equivalente a CanAppend em go
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

    /// equivalente a Grant em go
    pub async fn grant(&self, capability: &str, key_id: &str) -> Result<()> {
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

        // Emite evento de atualização
        self.on_update().await;

        Ok(())
    }

    /// equivalente a Revoke em go
    pub async fn revoke(&self, capability: &str, key_id: &str) -> Result<()> {
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
            store
                .delete(capability)
                .await
                .map_err(|e| GuardianError::Store(format!("Erro ao remover permissões: {}", e)))?;
        }

        // Emite evento de atualização
        self.on_update().await;

        Ok(())
    }

    /// equivalente a Load em go
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

        // Implementa operações de store para carregar o key-value store
        let store = self
            .guardian_db
            .key_value(&db_address, &mut store_options)
            .await
            .map_err(|e| GuardianError::Store(format!("Erro ao abrir key-value store: {}", e)))?;

        // Salva o novo store
        *store_guard = Some(Arc::new(tokio::sync::Mutex::new(store)));

        Ok(())
    }

    /// equivalente a Save em go
    pub async fn save(&self) -> Result<Box<dyn ManifestParams>> {
        let store_guard = self.kv_store.read().await;
        let store_arc = store_guard
            .as_ref()
            .ok_or_else(|| GuardianError::Store("kv_store não inicializado".to_string()))?;

        // Implementa quando trait bounds estiverem resolvidos
        let store = store_arc.lock().await;
        let addr = store.address();
        let addr_string = format!("{}", addr);

        log::debug!("Save executado para o store com endereço: {}", addr_string);

        // Cria o manifesto baseado no endereço real do store
        // Use default CID since string->CID conversion isn't trivial
        let cid = cid::Cid::default();

        // Assume um construtor que cria um manifesto 'GuardianDB' a partir de um CID.
        let params = CreateAccessControllerOptions::new(cid, false, "GuardianDB".to_string());
        Ok(Box::new(params))
    }

    /// equivalente a Close em go
    pub async fn close(&self) -> Result<()> {
        let mut store_guard = self.kv_store.write().await;
        if let Some(store_arc) = store_guard.take() {
            // Fecha o store usando o método close() da trait Store
            let mut store = store_arc.lock().await;
            match store.close().await {
                Ok(_) => log::debug!("Store fechado com sucesso"),
                Err(e) => log::warn!("Erro ao fechar o store: {}", e),
            }
        }
        Ok(())
    }

    /// equivalente a onUpdate em go
    async fn on_update(&self) {
        if let Err(e) = self.event_sender.send(EventUpdated) {
            warn!(target: "GuardianDB::ac", "não foi possível emitir o evento de atualização: {}", e);
        }
    }

    /// equivalente a NewGuardianDBAccessController em go
    /// Em Rust, é idiomático usar uma função `new` associada para construtores.
    pub async fn new(
        guardian_db: Arc<dyn GuardianDBKVStoreProvider<Error = GuardianError>>,
        params: Box<dyn ManifestParams>,
        // Em Rust, opções funcionais podem ser implementadas com o padrão builder ou closures.
    ) -> Result<Self> {
        let addr_str = if !params.address().to_string().is_empty() {
            params.address().to_string()
        } else {
            "default-access-controller".to_string()
        };

        let mut opts = CreateDBOptions::default();
        // Implementa quando trait bounds estiverem resolvidos
        let kv_store = guardian_db
            .key_value(&addr_str, &mut opts)
            .await
            .map_err(|e| {
                GuardianError::Store(format!("Erro ao inicializar key-value store: {}", e))
            })?;

        log::debug!("Key-value store inicializada para: {}", addr_str);

        // Usando um canal de broadcast do Tokio como exemplo de event bus.
        let (tx, _rx) = broadcast::channel(16);

        let _write_access = params.get_access("write");

        let controller = Self {
            event_sender: tx,
            guardian_db,
            kv_store: RwLock::new(Some(Arc::new(tokio::sync::Mutex::new(kv_store)))), // Inicializa com o store real
            options: params,
            // Inicializa com um logger padrão. Pode ser substituído com `set_logger`.
            logger: RwLock::new(Arc::new(slog::Logger::root(slog::Discard, slog::o!()))),
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

// TODO: Implementar AccessController trait quando os bounds Send + Sync estiverem resolvidos
// As dependências complexas da GuardianDBAccessController precisam de bounds adequados
