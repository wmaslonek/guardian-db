use crate::access_controller::manifest::{CreateAccessControllerOptions, ManifestParams};
use crate::access_controller::traits::AccessController;
use crate::address::Address;
use crate::error::{GuardianError, Result};
use crate::ipfs_log::{access_controller::LogEntry, identity_provider::IdentityProvider};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{Span, debug, info, instrument, warn};

/// Estado interno do SimpleAccessController
struct SimpleAccessControllerState {
    allowed_keys: HashMap<String, Vec<String>>,
}

/// Estrutura principal do controlador de acesso simples.
/// Mantém uma lista de chaves autorizadas em memória.
pub struct SimpleAccessController {
    state: Arc<RwLock<SimpleAccessControllerState>>,
    span: Span,
}

impl SimpleAccessController {
    /// Cria um novo SimpleAccessController com configuração inicial opcional
    #[instrument(skip(initial_keys))]
    pub fn new(initial_keys: Option<HashMap<String, Vec<String>>>) -> Self {
        let mut allowed_keys = initial_keys.unwrap_or_default();

        // Garante que pelo menos as categorias básicas existam
        allowed_keys.entry("read".to_string()).or_default();
        allowed_keys.entry("write".to_string()).or_default();
        allowed_keys.entry("admin".to_string()).or_default();

        info!(target: "simple_access_controller",
            categories = ?allowed_keys.keys().collect::<Vec<_>>(),
            total_permissions = allowed_keys.values().map(|v| v.len()).sum::<usize>(),
            "Created SimpleAccessController"
        );

        Self {
            state: Arc::new(RwLock::new(SimpleAccessControllerState { allowed_keys })),
            span: tracing::info_span!("simple_access_controller"),
        }
    }

    /// Cria um novo SimpleAccessController simples
    #[allow(dead_code)]
    pub fn new_simple() -> Self {
        Self::new(None)
    }

    /// Retorna uma referência ao span de tracing para instrumentação
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Lista todas as chaves de uma capacidade
    pub async fn list_keys(&self, capability: &str) -> Vec<String> {
        let state = self.state.read().await;
        state
            .allowed_keys
            .get(capability)
            .cloned()
            .unwrap_or_default()
    }

    /// Lista todas as capacidades disponíveis
    #[allow(dead_code)]
    pub async fn list_capabilities(&self) -> Vec<String> {
        let state = self.state.read().await;
        state.allowed_keys.keys().cloned().collect()
    }

    /// Verifica se uma chave tem uma capacidade específica
    #[allow(dead_code)]
    pub async fn has_capability(&self, capability: &str, key_id: &str) -> bool {
        let state = self.state.read().await;

        if let Some(keys) = state.allowed_keys.get(capability) {
            keys.contains(&"*".to_string()) || keys.contains(&key_id.to_string())
        } else {
            false
        }
    }

    /// Remove todas as chaves de uma capacidade
    pub async fn clear_capability(&self, capability: &str) -> Result<()> {
        if capability.is_empty() {
            return Err(GuardianError::Store(
                "Capability cannot be empty".to_string(),
            ));
        }

        let mut state = self.state.write().await;

        if let Some(keys) = state.allowed_keys.get_mut(capability) {
            let count = keys.len();
            keys.clear();

            info!(target: "simple_access_controller",
                capability = %capability,
                removed_keys = count,
                "Capability cleared"
            );
        } else {
            warn!(target: "simple_access_controller",
                capability = %capability,
                "Capability not found for clearing"
            );
        }

        Ok(())
    }

    /// Obtém estatísticas das permissões
    pub async fn get_stats(&self) -> HashMap<String, usize> {
        let state = self.state.read().await;
        state
            .allowed_keys
            .iter()
            .map(|(capability, keys)| (capability.clone(), keys.len()))
            .collect()
    }

    /// Verifica se uma capacidade está vazia
    pub async fn is_capability_empty(&self, capability: &str) -> bool {
        let state = self.state.read().await;
        state
            .allowed_keys
            .get(capability)
            .map(|keys| keys.is_empty())
            .unwrap_or(true)
    }

    /// Conta o total de permissões em todas as capacidades
    pub async fn total_permissions(&self) -> usize {
        let state = self.state.read().await;
        state.allowed_keys.values().map(|keys| keys.len()).sum()
    }

    /// Exporta todas as permissões para um HashMap
    pub async fn export_permissions(&self) -> HashMap<String, Vec<String>> {
        let state = self.state.read().await;
        state.allowed_keys.clone()
    }

    /// Importa permissões de um HashMap (substitui todas as existentes)
    pub async fn import_permissions(
        &self,
        permissions: HashMap<String, Vec<String>>,
    ) -> Result<()> {
        let mut state = self.state.write().await;

        info!(target: "simple_access_controller", "Importing permissions: capabilities_count={}, total_permissions={}",
            permissions.len(),
            permissions.values().map(|v| v.len()).sum::<usize>()
        );

        state.allowed_keys = permissions;
        Ok(())
    }

    /// Adiciona múltiplas chaves a uma capacidade de uma vez
    pub async fn grant_multiple(&self, capability: &str, key_ids: Vec<&str>) -> Result<()> {
        let _entered = self.span.enter();

        if capability.is_empty() {
            return Err(GuardianError::Store(
                "Capability cannot be empty".to_string(),
            ));
        }

        let mut state = self.state.write().await;
        let keys = state
            .allowed_keys
            .entry(capability.to_string())
            .or_insert_with(Vec::new);

        let mut added_count = 0;
        for key_id in key_ids {
            if !key_id.is_empty() && !keys.contains(&key_id.to_string()) {
                keys.push(key_id.to_string());
                added_count += 1;
            }
        }

        let total_keys = keys.len();
        let capability_name = capability.to_string();

        info!(target: "simple_access_controller", "Multiple permissions granted: capability={}, added_keys={}, total_keys={}",
            capability_name, added_count, total_keys
        );

        Ok(())
    }

    /// Remove múltiplas chaves de uma capacidade de uma vez
    pub async fn revoke_multiple(&self, capability: &str, key_ids: Vec<&str>) -> Result<()> {
        let _entered = self.span.enter();

        if capability.is_empty() {
            return Err(GuardianError::Store(
                "Capability cannot be empty".to_string(),
            ));
        }

        let mut state = self.state.write().await;

        if let Some(keys) = state.allowed_keys.get_mut(capability) {
            let initial_len = keys.len();

            for key_id in key_ids {
                keys.retain(|k| k != key_id);
            }

            let removed_count = initial_len - keys.len();
            let remaining_keys = keys.len();
            let capability_name = capability.to_string();
            let should_remove_capability = keys.is_empty();

            info!(target: "simple_access_controller", "Multiple permissions revoked: capability={}, removed_keys={}, remaining_keys={}",
                capability_name, removed_count, remaining_keys
            );

            // Remove a capacidade completamente se não há mais chaves
            if should_remove_capability {
                state.allowed_keys.remove(capability);
                debug!(target: "simple_access_controller", "Capability removed completely: capability={}",
                    capability
                );
            }
        }

        Ok(())
    }

    /// Clona as permissões de uma capacidade para outra
    pub async fn clone_capability(
        &self,
        source_capability: &str,
        target_capability: &str,
    ) -> Result<()> {
        if source_capability.is_empty() || target_capability.is_empty() {
            return Err(GuardianError::Store(
                "Source and target capabilities cannot be empty".to_string(),
            ));
        }

        let mut state = self.state.write().await;

        if let Some(source_keys) = state.allowed_keys.get(source_capability) {
            let cloned_keys = source_keys.clone();
            let keys_count = cloned_keys.len();

            state
                .allowed_keys
                .insert(target_capability.to_string(), cloned_keys);

            info!(target: "simple_access_controller", "Capability cloned: source_capability={}, target_capability={}, cloned_keys={}",
                source_capability, target_capability, keys_count
            );
        } else {
            return Err(GuardianError::Store(format!(
                "Source capability '{}' not found",
                source_capability
            )));
        }

        Ok(())
    }

    /// Este controlador não tem um endereço, pois não é persistido.
    pub fn address(&self) -> Option<Box<dyn Address>> {
        None
    }

    /// Método factory alternativo
    #[instrument(skip(params))]
    pub fn from_options(params: CreateAccessControllerOptions) -> Result<Self> {
        // As permissões são extraídas diretamente dos parâmetros de criação.
        let allowed_keys = params.get_all_access();
        Ok(Self {
            state: Arc::new(RwLock::new(SimpleAccessControllerState { allowed_keys })),
            span: tracing::info_span!("simple_access_controller", controller_type = "simple"),
        })
    }
}

#[async_trait]
impl AccessController for SimpleAccessController {
    fn get_type(&self) -> &str {
        "simple"
    }

    async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>> {
        let _entered = self.span.enter();

        // Validação de parâmetros
        if role.is_empty() {
            return Err(GuardianError::Store("Role cannot be empty".to_string()));
        }

        let state = self.state.read().await;

        // Log da consulta
        debug!(target: "simple_access_controller", "Getting authorized keys by role: role={}",
            role
        );

        let keys = state.allowed_keys.get(role).cloned().unwrap_or_default();

        debug!(target: "simple_access_controller", "Retrieved authorized keys: role={}, key_count={}",
            role, keys.len()
        );

        Ok(keys)
    }

    async fn grant(&self, capability: &str, key_id: &str) -> Result<()> {
        let _entered = self.span.enter();

        // Validação de parâmetros
        if capability.is_empty() {
            return Err(GuardianError::Store(
                "Capability cannot be empty".to_string(),
            ));
        }
        if key_id.is_empty() {
            return Err(GuardianError::Store("Key ID cannot be empty".to_string()));
        }

        let mut state = self.state.write().await;

        // Log da operação
        info!(target: "simple_access_controller", "Granting permission: capability={}, key_id={}",
            capability, key_id
        );

        // Adiciona a chave à lista de permissões para a capacidade especificada
        let entry = state
            .allowed_keys
            .entry(capability.to_string())
            .or_insert_with(Vec::new);

        // Verifica se a chave já existe para evitar duplicatas
        if !entry.contains(&key_id.to_string()) {
            entry.push(key_id.to_string());
            let total_keys = entry.len();
            let capability_name = capability.to_string();
            let key_id_name = key_id.to_string();

            debug!(target: "simple_access_controller", "Permission granted successfully: capability={}, key_id={}, total_keys={}",
                capability_name, key_id_name, total_keys
            );
        } else {
            debug!(target: "simple_access_controller", "Permission already exists: capability={}, key_id={}",
                capability, key_id
            );
        }

        Ok(())
    }

    async fn revoke(&self, capability: &str, key_id: &str) -> Result<()> {
        let _entered = self.span.enter();

        // Validação de parâmetros
        if capability.is_empty() {
            return Err(GuardianError::Store(
                "Capability cannot be empty".to_string(),
            ));
        }
        if key_id.is_empty() {
            return Err(GuardianError::Store("Key ID cannot be empty".to_string()));
        }

        let mut state = self.state.write().await;

        // Log da operação
        info!(target: "simple_access_controller", "Revoking permission: capability={}, key_id={}",
            capability, key_id
        );

        // Remove a chave da lista de permissões para a capacidade especificada
        if let Some(keys) = state.allowed_keys.get_mut(capability) {
            let initial_len = keys.len();
            keys.retain(|k| k != key_id);

            if keys.len() < initial_len {
                let remaining_keys = keys.len();
                let capability_name = capability.to_string();
                let key_id_name = key_id.to_string();
                let should_remove_capability = keys.is_empty();

                debug!(target: "simple_access_controller", "Permission revoked successfully: capability={}, key_id={}, remaining_keys={}",
                    capability_name, key_id_name, remaining_keys
                );

                // Remove a entrada completamente se não há mais chaves
                if should_remove_capability {
                    state.allowed_keys.remove(capability);
                    debug!(target: "simple_access_controller", "Capability removed completely: capability={}",
                        capability
                    );
                }
            } else {
                debug!(target: "simple_access_controller", "Permission not found for revocation: capability={}, key_id={}",
                    capability, key_id
                );
            }
        } else {
            debug!(target: "simple_access_controller", "Capability not found for revocation: capability={}",
                capability
            );
        }

        Ok(())
    }

    async fn load(&self, address: &str) -> Result<()> {
        // Validação de parâmetros
        if address.is_empty() {
            return Err(GuardianError::Store("Address cannot be empty".to_string()));
        }

        // Log da operação
        info!(target: "simple_access_controller", "Loading access controller configuration: address={}",
            address
        );

        // Para SimpleAccessController, load é uma operação no-op já que é baseado em memória
        // Em uma implementação mais avançada, isso poderia carregar de um arquivo ou rede
        debug!(target: "simple_access_controller", "Load operation completed (no-op for simple controller): address={}",
            address
        );

        Ok(())
    }

    async fn save(&self) -> Result<Box<dyn ManifestParams>> {
        let state = self.state.read().await;

        // Log da operação
        info!(target: "simple_access_controller", "Saving access controller configuration");

        // Cria opções com as permissões atuais
        let mut options = CreateAccessControllerOptions::new_empty();
        options.set_type("simple".to_string());

        // Copia todas as permissões atuais para o manifesto
        for (capability, keys) in &state.allowed_keys {
            options.set_access(capability.clone(), keys.clone());
        }

        debug!(target: "simple_access_controller", "Save operation completed: capabilities_count={}",
            state.allowed_keys.len()
        );

        Ok(Box::new(options))
    }

    async fn close(&self) -> Result<()> {
        let state = self.state.read().await;

        // Log da operação de fechamento
        info!(target: "simple_access_controller", "Closing simple access controller");

        // Para SimpleAccessController, close é uma operação no-op já que é baseado em memória
        // Em uma implementação mais avançada, isso poderia fechar conexões ou salvar estado
        debug!(target: "simple_access_controller", "Close operation completed: capabilities_count={}",
            state.allowed_keys.len()
        );

        Ok(())
    }

    async fn can_append(
        &self,
        entry: &dyn LogEntry,
        identity_provider: &dyn IdentityProvider,
        _additional_context: &dyn crate::ipfs_log::access_controller::CanAppendAdditionalContext,
    ) -> Result<()> {
        let _entered = self.span.enter();
        let state = self.state.read().await;

        // Obtém o ID da identidade da entrada
        let entry_identity = entry.get_identity();
        let entry_id = entry_identity.id();

        debug!(target: "simple_access_controller", "Checking append permission: entry_id={}",
            entry_id
        );

        // Verifica primeiro as chaves com permissão de escrita
        if let Some(write_keys) = state.allowed_keys.get("write") {
            // Verifica se há um wildcard que permite qualquer identidade
            if write_keys.contains(&"*".to_string()) {
                debug!(target: "simple_access_controller", "Wildcard permission found, verifying identity: entry_id={}",
                    entry_id
                );

                // Ainda assim, verifica a identidade para garantir que é válida
                if let Err(e) = identity_provider
                    .verify_identity(entry.get_identity())
                    .await
                {
                    warn!(target: "simple_access_controller", "Invalid identity signature for wildcard access: entry_id={}, error={}",
                        entry_id, e
                    );
                    return Err(GuardianError::Store(format!(
                        "Invalid identity signature: {}",
                        e
                    )));
                }

                debug!(target: "simple_access_controller", "Append permission granted (wildcard): entry_id={}",
                    entry_id
                );
                return Ok(());
            }

            // Verifica se o ID da entrada está na lista de chaves autorizadas para escrita
            if write_keys.contains(&entry_id.to_string()) {
                // Verifica a assinatura da identidade
                if let Err(e) = identity_provider.verify_identity(entry_identity).await {
                    warn!(target: "simple_access_controller", "Invalid identity signature for authorized key: entry_id={}, error={}",
                        entry_id, e
                    );
                    return Err(GuardianError::Store(format!(
                        "Invalid identity signature for authorized key {}: {}",
                        entry_id, e
                    )));
                }

                debug!(target: "simple_access_controller", "Append permission granted (write key): entry_id={}",
                    entry_id
                );
                return Ok(());
            }
        }

        // Verifica também permissões de admin (admin pode escrever)
        if let Some(admin_keys) = state.allowed_keys.get("admin")
            && (admin_keys.contains(&"*".to_string()) || admin_keys.contains(&entry_id.to_string()))
        {
            // Verifica a assinatura da identidade
            if let Err(e) = identity_provider.verify_identity(entry_identity).await {
                warn!(target: "simple_access_controller", "Invalid identity signature for admin key: entry_id={}, error={}",
                    entry_id, e
                );
                return Err(GuardianError::Store(format!(
                    "Invalid identity signature for admin key {}: {}",
                    entry_id, e
                )));
            }

            debug!(target: "simple_access_controller", "Append permission granted (admin key): entry_id={}",
                entry_id
            );
            return Ok(());
        }

        warn!(target: "simple_access_controller", "Access denied for append operation: entry_id={}, available_write_keys={:?}, available_admin_keys={:?}",
            entry_id, state.allowed_keys.get("write"), state.allowed_keys.get("admin")
        );

        Err(GuardianError::Store(format!(
            "Access denied: identity {} not authorized for write operations",
            entry_id
        )))
    }
}
