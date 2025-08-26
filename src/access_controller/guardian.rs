use crate::error::{GuardianError, Result};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use log::warn;
use slog::Logger;
use crate::iface::{CreateDBOptions, KeyValueStore, GuardianDBKVStoreProvider};
use crate::access_controller::{manifest::ManifestParams, utils};
use crate::address::Address;
use crate::eqlabs_ipfs_log::{access_controller, identity_provider::IdentityProvider};
use crate::access_controller::manifest::CreateAccessControllerOptions;

/// equivalente a EventUpdated em go
#[derive(Debug, Clone)]
pub struct EventUpdated;
pub struct GuardianDBAccessController {
    /// O emissor de eventos, substituindo o eventbus do Go.
    event_sender: broadcast::Sender<EventUpdated>,
    
    /// Uma referência compartilhada à instância principal do GuardianDB.
    GuardianDB: Arc<dyn GuardianDBKVStoreProvider<Error = GuardianError>>,
    
    /// O armazém de chave-valor para as permissões. Envolto em RwLock e Option 
    /// porque pode ser substituído dinamicamente pela função `load`.
    kv_store: RwLock<Option<Arc<dyn KeyValueStore<Error = GuardianError>>>>,
    
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
        // Como address() retorna &dyn Address, precisamos converter para concreto
        store_guard.as_ref().map(|store| {
            // TODO: Implementar conversão para tipo concreto de Address
            // Por enquanto retorna None como placeholder
            let _addr_string = format!("{:p}", store.as_ref()); // Usar pointer format para evitar trait bound
            None::<Box<dyn Address>>
        }).flatten()
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
        let _store = match store_guard.as_ref() {
            Some(s) => s,
            // Se não há store, não há autorizações.
            None => return Ok(HashMap::new()),
        };
        
        // TODO: Implementar store.all() quando trait bounds estiverem resolvidos
        // let all_data = store.all();
        let all_data: std::collections::HashMap<String, Vec<u8>> = HashMap::new(); // Placeholder

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

        let access: HashSet<String> = write_access.into_iter().chain(admin_access.into_iter()).collect();

        let entry_id = entry.get_identity().id();

        // Verifica se a chave universal ("*") ou a ID específica da entrada está presente.
        if access.contains(entry_id) || access.contains("*") {
            identity_provider.verify_identity(entry.get_identity()).await?;
            return Ok(());
        }

        Err(GuardianError::Store("Não autorizado".to_string()))
    }
    
    /// equivalente a Grant em go
    pub async fn grant(&self, capability: &str, key_id: &str) -> Result<()> {
        let mut store_guard = self.kv_store.write().await;
        let _store = store_guard.as_mut().ok_or_else(|| GuardianError::Store("kv_store não inicializado".to_string()))?;

        // Usa um HashSet para evitar duplicatas automaticamente.
        let mut capabilities: HashSet<String> = self.get_authorized_by_role(capability).await?
            .into_iter()
            .collect();
        
        capabilities.insert(key_id.to_string());
        
        let capabilities_vec: Vec<String> = capabilities.into_iter().collect();
        
        let capabilities_json = serde_json::to_vec(&capabilities_vec)?;
            
        // TODO: Implementar operações de store quando trait bounds estiverem resolvidos
        // store.put(capability, capabilities_json).await
        //     .map_err(|e| anyhow!("Erro ao salvar no store: {}", e.to_string()))?;
        
        // Por enquanto, apenas logamos a operação que seria realizada
        log::debug!("Operação put seria executada: {} -> {} bytes", capability, capabilities_json.len());
            
        Ok(())
    }

    /// equivalente a Revoke em go
    pub async fn revoke(&self, capability: &str, key_id: &str) -> Result<()> {
        let mut store_guard = self.kv_store.write().await;
        let _store = store_guard.as_mut().ok_or_else(|| GuardianError::Store("kv_store não inicializado".to_string()))?;
        
        let mut capabilities: Vec<String> = self.get_authorized_by_role(capability).await?;

        // Remove a chave, se ela existir.
        capabilities.retain(|id| id != key_id);

        if !capabilities.is_empty() {
            let capabilities_json = serde_json::to_vec(&capabilities)?;
            
            // TODO: Implementar operações de store quando trait bounds estiverem resolvidos
            // store.put(capability, capabilities_json).await
            //     .map_err(|e| anyhow!("Erro ao persistir permissões: {}", e.to_string()))?;
            log::debug!("Operação put seria executada: {} -> {} bytes", capability, capabilities_json.len());
        } else {
            // TODO: Implementar operações de store quando trait bounds estiverem resolvidos
            // store.delete(capability).await
            //     .map_err(|e| anyhow!("Erro ao remover permissões: {}", e.to_string()))?;
            log::debug!("Operação delete seria executada: {}", capability);
        }
        
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
                // TODO: Fix identity access - this would require casting GuardianDB to BaseGuardianDB
                vec!["default_identity".to_string()]
            }
        };
        
        let _ipfs_ac_params = CreateAccessControllerOptions::new_simple("ipfs".to_string(), {
            let mut access = HashMap::new();
            access.insert("write".to_string(), write_access);
            access
        });
        
        let db_address = utils::ensure_address(address);
        
        let _store_options = CreateDBOptions::default();
        // Temporariamente comentado até resolver tipos
        // store_options.access_controller = Some(Box::new(ipfs_ac_params));

        // TODO: Implementar operações de store quando trait bounds estiverem resolvidos
        // let store = self.GuardianDB.key_value(&db_address, &mut store_options).await
        //     .map_err(|e| anyhow!("Erro ao abrir key-value store: {}", e.to_string()))?;
        
        // Por enquanto, criar um placeholder
        log::debug!("Store seria carregada para endereço: {}", db_address);
        
        // TODO: Coloca o novo store quando implementação estiver completa
        // *store_guard = Some(Arc::from(store));
        
        Ok(())
    }

    /// equivalente a Save em go
    pub async fn save(&self) -> Result<Box<dyn ManifestParams>> {
        let store_guard = self.kv_store.read().await;
        let _store = store_guard.as_ref().ok_or_else(|| GuardianError::Store("kv_store não inicializado".to_string()))?;
        
        // TODO: Implementar quando trait bounds estiverem resolvidos
        // let addr = store.address();
        // let _addr_string = addr.to_string();
        
        log::debug!("Save seria executado para o store");
        
        // Cria um CID padrão para testes
        let default_cid = cid::Cid::default();
        
        // Assume um construtor que cria um manifesto 'GuardianDB' a partir de um CID.
        let params = CreateAccessControllerOptions::new(default_cid, false, "GuardianDB".to_string());
        Ok(Box::new(params))
    }

    /// equivalente a Close em go
    pub async fn close(&self) -> Result<()> {
        let mut store_guard = self.kv_store.write().await;
        if let Some(_store) = store_guard.take() {
            // Temporariamente apenas remove a referência
            // store.close().await.with_context(|| "Erro ao fechar o store")?;
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
        GuardianDB: Arc<dyn GuardianDBKVStoreProvider<Error = GuardianError>>,
        params: Box<dyn ManifestParams>,
        // Em Rust, opções funcionais podem ser implementadas com o padrão builder ou closures.
    ) -> Result<Self> {
        let addr_str = if !params.address().to_string().is_empty() {
            params.address().to_string()
        } else {
            "default-access-controller".to_string()
        };

        let _opts = CreateDBOptions::default();
        // TODO: Implementar quando trait bounds estiverem resolvidos
        // let kv_store = GuardianDB.key_value(&addr_str, &mut opts).await
        //     .map_err(|e| anyhow!("Erro ao inicializar key-value store: {}", e.to_string()))?;
        
        log::debug!("Key-value store seria inicializada para: {}", addr_str);
            
        // Usando um canal de broadcast do Tokio como exemplo de event bus.
        let (tx, _rx) = broadcast::channel(16);

        let _write_access = params.get_access("write");

        let controller = Self {
            event_sender: tx,
            GuardianDB,
            kv_store: RwLock::new(None), // Por enquanto inicializa como None até resolver trait bounds
            options: params,
            // Inicializa com um logger padrão. Pode ser substituído com `set_logger`.
            logger: RwLock::new(Arc::new(slog::Logger::root(slog::Discard, slog::o!()))), 
        };

        // Não há permissões de escrita a conceder no momento da criação
        // pois options é uma trait, não possui get_access
        
        Ok(controller)
    }
}