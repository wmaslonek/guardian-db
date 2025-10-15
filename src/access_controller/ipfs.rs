use crate::access_controller::{
    manifest::CreateAccessControllerOptions, manifest::Manifest, manifest::ManifestParams,
};
use crate::address::Address;
use crate::error::{GuardianError, Result};
use crate::ipfs_core_api::client::IpfsClient;
use crate::ipfs_log::{access_controller::LogEntry, identity_provider::IdentityProvider};
use async_trait::async_trait;
use cid::Cid;
use serde::{Deserialize, Serialize};
use std::convert::TryFrom;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{Span, debug, instrument, warn};

#[derive(Debug, Serialize, Deserialize)]
struct CborWriteAccess {
    #[serde(rename = "write")]
    write: String,
}
struct ControllerState {
    write_access: Vec<String>,
}

/// Estrutura principal do controlador de acesso IPFS.
pub struct IpfsAccessController {
    ipfs: Arc<IpfsClient>,
    state: RwLock<ControllerState>,
    span: Span,
}

impl IpfsAccessController {
    pub fn get_type(&self) -> &'static str {
        "ipfs"
    }

    /// Este controlador não tem um endereço próprio, então retorna None.
    pub fn address(&self) -> Option<Box<dyn Address>> {
        None
    }

    #[instrument(skip(self, entry, identity_provider, _additional_context))]
    pub async fn can_append(
        &self,
        entry: &dyn LogEntry,
        identity_provider: &dyn IdentityProvider,
        _additional_context: &dyn crate::ipfs_log::access_controller::CanAppendAdditionalContext,
    ) -> Result<()> {
        let state = self.state.read().await;
        let key = entry.get_identity().id();

        for allowed_key in state.write_access.iter() {
            if allowed_key == key || allowed_key == "*" {
                // Se a chave for autorizada, verifica a identidade
                return identity_provider
                    .verify_identity(entry.get_identity())
                    .await;
            }
        }

        Err(GuardianError::Store(
            "Chave não tem permissão de escrita".to_string(),
        ))
    }

    pub async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>> {
        let state = self.state.read().await;
        // 'admin' e 'write' são a mesma coisa para este controlador.
        if role == "admin" || role == "write" {
            Ok(state.write_access.clone())
        } else {
            Ok(vec![])
        }
    }

    #[instrument(skip(self))]
    pub async fn grant(&self, capability: &str, key_id: &str) -> Result<()> {
        if capability != "write" {
            return Err(GuardianError::Store(format!(
                "IpfsAccessController only supports 'write' capability, got '{}'",
                capability
            )));
        }

        let mut state = self.state.write().await;
        if !state.write_access.contains(&key_id.to_string()) {
            state.write_access.push(key_id.to_string());
            debug!(target: "ipfs_access_controller",
                capability = %capability,
                key_id = %key_id,
                total_keys = state.write_access.len(),
                "Permission granted successfully"
            );
        } else {
            debug!(target: "ipfs_access_controller",
                capability = %capability,
                key_id = %key_id,
                "Permission already exists"
            );
        }
        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn revoke(&self, capability: &str, key_id: &str) -> Result<()> {
        if capability != "write" {
            return Err(GuardianError::Store(format!(
                "IpfsAccessController only supports 'write' capability, got '{}'",
                capability
            )));
        }

        let mut state = self.state.write().await;
        let initial_len = state.write_access.len();
        state.write_access.retain(|k| k != key_id);

        if state.write_access.len() < initial_len {
            debug!(target: "ipfs_access_controller",
                capability = %capability,
                key_id = %key_id,
                remaining_keys = state.write_access.len(),
                "Permission revoked successfully"
            );
        } else {
            debug!(target: "ipfs_access_controller",
                capability = %capability,
                key_id = %key_id,
                "Permission not found for revocation"
            );
        }
        Ok(())
    }

    #[instrument(skip(self), fields(address = %address))]
    pub async fn load(&self, address: &str) -> Result<()> {
        let state = self.state.read().await;
        debug!(target: "ipfs_access_controller", address = %address, "Lendo permissões do controlador de acesso IPFS");
        drop(state); // Liberamos o lock de leitura antes das operações de escrita

        let cid = Cid::try_from(address)?;
        let ipfs = self.ipfs.clone();
        let cid_string = cid.to_string();

        // Spawn a blocking task to handle the non-Send IPFS operations
        let manifest_data = tokio::task::spawn_blocking(move || {
            // Use tokio runtime handle to run async code in blocking context
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                // 1. Lê o manifesto CBOR principal
                let mut manifest_reader = ipfs.cat(&cid_string).await?;
                let mut manifest_data = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut manifest_reader, &mut manifest_data)
                    .await
                    .map_err(|e| crate::error::GuardianError::Io(e.to_string()))?;

                Ok::<Vec<u8>, crate::error::GuardianError>(manifest_data)
            })
        })
        .await
        .map_err(|e| GuardianError::Store(format!("Task join error: {}", e)))??;

        let manifest: Manifest = serde_cbor::from_slice(&manifest_data)?;

        // 2. Lê o conteúdo das permissões usando o endereço do manifesto
        let access_data_cid = manifest.params.address();
        let ipfs_clone = self.ipfs.clone();
        let access_data_cid_string = access_data_cid.to_string();

        // Spawn another blocking task for the second IPFS operation
        let access_data_bytes = tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                let mut access_reader = ipfs_clone.cat(&access_data_cid_string).await?;
                let mut access_data_bytes = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut access_reader, &mut access_data_bytes)
                    .await
                    .map_err(|e| crate::error::GuardianError::Io(e.to_string()))?;

                Ok::<Vec<u8>, crate::error::GuardianError>(access_data_bytes)
            })
        })
        .await
        .map_err(|e| GuardianError::Store(format!("Task join error: {}", e)))??;

        let write_access_data: CborWriteAccess = serde_cbor::from_slice(&access_data_bytes)?;

        // 3. O conteúdo é uma string JSON, que precisa ser deserializada também
        let write_access: Vec<String> = serde_json::from_str(&write_access_data.write)?;

        // 4. Atualiza o estado interno com as novas permissões
        let mut state = self.state.write().await;
        state.write_access = write_access;

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn save(&self) -> Result<CreateAccessControllerOptions> {
        let state = self.state.read().await;
        let write_access_json = serde_json::to_string(&state.write_access)?;
        let cbor_data = CborWriteAccess {
            write: write_access_json,
        };
        // Serializa a estrutura CBOR em bytes
        let cbor_bytes = serde_cbor::to_vec(&cbor_data)?;

        let ipfs = self.ipfs.clone();
        // Spawn a blocking task to handle the non-Send IPFS operations
        let response = tokio::task::spawn_blocking(move || {
            // Use tokio runtime handle to run async code in blocking context
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                // Salva os bytes no IPFS usando helper method
                ipfs.add_bytes(cbor_bytes).await
            })
        })
        .await
        .map_err(|e| GuardianError::Store(format!("Task join error: {}", e)))??;
        let cid = Cid::try_from(response.hash.as_str())?;
        debug!(target: "ipfs_access_controller", cid = %cid, "Controlador de acesso IPFS salvo");
        // Cria e retorna os parâmetros do novo manifesto
        Ok(CreateAccessControllerOptions::new(
            cid,
            false,
            "ipfs".to_string(),
        ))
    }

    #[instrument(skip(self))]
    pub async fn close(&self) -> Result<()> {
        // Para IpfsAccessController, close é uma operação no-op já que é baseado em IPFS
        // O estado é mantido no IPFS e não há recursos locais para fechar
        debug!(target: "ipfs_access_controller", "Closing IPFS access controller");

        let state = self.state.read().await;
        debug!(target: "ipfs_access_controller",
            write_access_count = state.write_access.len(),
            "IPFS access controller closed successfully"
        );

        Ok(())
    }

    #[instrument(skip(ipfs_client, params), fields(identity_id = %identity_id))]
    pub fn new(
        ipfs_client: Arc<IpfsClient>,
        identity_id: String,
        mut params: CreateAccessControllerOptions,
    ) -> Result<Self> {
        if params.get_access("write").is_none() {
            params.set_access("write".to_string(), vec![identity_id]);
        }

        let initial_state = ControllerState {
            write_access: params.get_access("write").unwrap_or_default(),
        };

        Ok(Self {
            ipfs: ipfs_client,
            state: RwLock::new(initial_state),
            span: tracing::info_span!("ipfs_access_controller", controller_type = "ipfs"),
        })
    }

    /// Retorna uma referência ao span para contexto de tracing
    pub fn span(&self) -> &Span {
        &self.span
    }
}

#[async_trait]
impl crate::access_controller::traits::AccessController for IpfsAccessController {
    fn get_type(&self) -> &str {
        "ipfs"
    }

    async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>> {
        let state = self.state.read().await;

        match role {
            "write" => Ok(state.write_access.clone()),
            "read" => Ok(state.write_access.clone()), // Por padrão, quem pode escrever pode ler
            "admin" => Ok(state.write_access.clone()), // Por padrão, usa mesmas permissões
            _ => Ok(Vec::new()),
        }
    }

    async fn grant(&self, capability: &str, key_id: &str) -> Result<()> {
        if capability != "write" {
            return Err(GuardianError::Store(format!(
                "IpfsAccessController only supports 'write' capability, got '{}'",
                capability
            )));
        }

        let mut state = self.state.write().await;
        if !state.write_access.contains(&key_id.to_string()) {
            state.write_access.push(key_id.to_string());
        }
        Ok(())
    }

    async fn revoke(&self, capability: &str, key_id: &str) -> Result<()> {
        if capability != "write" {
            return Err(GuardianError::Store(format!(
                "IpfsAccessController only supports 'write' capability, got '{}'",
                capability
            )));
        }

        let mut state = self.state.write().await;
        state.write_access.retain(|k| k != key_id);
        Ok(())
    }

    async fn load(&self, address: &str) -> Result<()> {
        self.load(address).await
    }

    async fn save(&self) -> Result<Box<dyn crate::access_controller::manifest::ManifestParams>> {
        let options = self.save().await?;
        Ok(Box::new(options))
    }

    async fn close(&self) -> Result<()> {
        IpfsAccessController::close(self).await
    }

    async fn can_append(
        &self,
        entry: &dyn crate::ipfs_log::access_controller::LogEntry,
        _identity_provider: &dyn crate::ipfs_log::identity_provider::IdentityProvider,
        _additional_context: &dyn crate::ipfs_log::access_controller::CanAppendAdditionalContext,
    ) -> Result<()> {
        let state = self.state.read().await;
        let entry_identity = entry.get_identity();
        let entry_id = entry_identity.id();

        // Verifica se a identidade tem permissão de escrita
        if state.write_access.contains(&"*".to_string())
            || state.write_access.contains(&entry_id.to_string())
        {
            Ok(())
        } else {
            Err(GuardianError::Store(format!(
                "Access denied: identity {} not authorized for write operations",
                entry_id
            )))
        }
    }
}
