use crate::error::{GuardianError, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use slog::{self, Logger};
use crate::access_controller::manifest::{CreateAccessControllerOptions, ManifestParams};
use crate::access_controller::traits::AccessController;
use crate::address::Address;
use crate::eqlabs_ipfs_log::{access_controller::LogEntry, identity_provider::IdentityProvider};
use async_trait::async_trait;

/// Agrupa o estado mutável do controlador para ser protegido por um único RwLock,
/// espelhando o comportamento do `sync.RWMutex` no código Go.
struct ControllerState {
    allowed_keys: HashMap<String, Vec<String>>,
    logger: Arc<Logger>,
}

/// Estrutura principal do controlador de acesso simples.
/// Mantém uma lista de chaves autorizadas em memória.
pub struct SimpleAccessController {
    state: RwLock<ControllerState>,
}

impl SimpleAccessController {
    /// equivalente a Address em go
    /// Este controlador não tem um endereço, pois não é persistido.
    pub fn address(&self) -> Option<Box<dyn Address>> {
        None
    }

    /// equivalente a NewSimpleAccessController em go
    pub fn new(params: CreateAccessControllerOptions) -> Result<Self> {
        let initial_state = ControllerState {
            // As permissões são extraídas diretamente dos parâmetros de criação.
            allowed_keys: params.get_all_access(), 
            logger: Arc::new(slog::Logger::root(slog::Discard, slog::o!())), // Logger padrão slog
        };
        
        // Em Go, as `options` são aplicadas aqui. Um padrão similar (ex: Builder)
        // pode ser usado em Rust se necessário.
        
        Ok(Self {
            state: RwLock::new(initial_state),
        })
    }
}

#[async_trait]
impl AccessController for SimpleAccessController {
    fn r#type(&self) -> &str {
        "simple"
    }

    async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>> {
        let state = self.state.read().await;
        Ok(state.allowed_keys.get(role).cloned().unwrap_or_default())
    }

    async fn grant(&self, _capability: &str, _key_id: &str) -> Result<()> {
        Ok(())
    }

    async fn revoke(&self, _capability: &str, _key_id: &str) -> Result<()> {
        Ok(())
    }

    async fn load(&self, _address: &str) -> Result<()> {
        Ok(())
    }

    async fn save(&self) -> Result<Box<dyn ManifestParams>> {
        let options = CreateAccessControllerOptions::new_empty();
        Ok(Box::new(options))
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }

    async fn set_logger(&self, logger: Arc<Logger>) {
        let mut state = self.state.write().await;
        state.logger = logger;
    }

    async fn logger(&self) -> Arc<Logger> {
        let state = self.state.read().await;
        state.logger.clone()
    }

    async fn can_append(
        &self,
        entry: &dyn LogEntry,
        identity_provider: &dyn IdentityProvider,
        _additional_context: &dyn crate::eqlabs_ipfs_log::access_controller::CanAppendAdditionalContext,
    ) -> Result<()> {
        let state = self.state.read().await;
        
        // Busca as chaves com permissão de escrita
        if let Some(write_keys) = state.allowed_keys.get("write") {
            // Verifica se há um wildcard que permite qualquer identidade
            if write_keys.contains(&"*".to_string()) {
                // Ainda assim, verifica a identidade para garantir que é válida
                if let Err(e) = identity_provider.verify_identity(entry.get_identity()).await {
                    return Err(GuardianError::Store(format!("Invalid identity signature: {}", e)));
                }
                return Ok(());
            }
            
            // Obtém o ID da identidade da entrada
            let entry_identity = entry.get_identity();
            let entry_id = entry_identity.id();
            
            // Verifica se o ID da entrada está na lista de chaves autorizadas
            for authorized_id in write_keys {
                if entry_id == authorized_id {
                    // Verifica a assinatura da identidade
                    if let Err(e) = identity_provider.verify_identity(entry_identity).await {
                        return Err(GuardianError::Store(format!("Invalid identity signature for authorized key {}: {}", entry_id, e)));
                    }
                    return Ok(());
                }
            }
        }
        
        Err(GuardianError::Store(format!("Access denied: identity {} not authorized for write operations", entry.get_identity().id())))
    }
}