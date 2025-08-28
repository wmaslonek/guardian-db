use crate::error::{GuardianError, Result};
use serde::{Deserialize, Serialize};
use std::convert::TryFrom;
use std::sync::Arc;
use tokio::sync::RwLock;
use log::debug;
use slog::Logger;
use ipfs_api_backend_hyper::{IpfsClient, IpfsApi};
use cid::Cid;
use futures::TryStreamExt;
use crate::access_controller::{manifest::Manifest, manifest::ManifestParams, manifest::CreateAccessControllerOptions};
use crate::address::Address;
use crate::eqlabs_ipfs_log::{access_controller::LogEntry, identity_provider::IdentityProvider};

/// Equivalente à struct cborWriteAccess em go.
/// Usamos atributos da crate serde para mapear o nome do campo para "write" em minúsculas
/// durante a serialização/desserialização, garantindo compatibilidade com a versão Go/JS.
#[derive(Debug, Serialize, Deserialize)]
struct CborWriteAccess {
    #[serde(rename = "write")]
    write: String,
}

/// Agrupa o estado mutável do controlador para ser protegido por um único RwLock,
/// espelhando o comportamento do `sync.RWMutex` no código Go.
struct ControllerState {
    write_access: Vec<String>,
    logger: Arc<Logger>,
}

/// Estrutura principal do controlador de acesso IPFS.
pub struct IpfsAccessController {
    ipfs: Arc<IpfsClient>,
    state: RwLock<ControllerState>,
}

impl IpfsAccessController {
    /// equivalente a Type em go
    pub fn r#type(&self) -> &'static str {
        "ipfs"
    }

    /// equivalente a Address em go
    /// Este controlador não tem um endereço próprio, então retorna None.
    pub fn address(&self) -> Option<Box<dyn Address>> {
        None
    }

    /// equivalente a CanAppend em go
    pub async fn can_append(
        &self,
        entry: &dyn LogEntry,
        identity_provider: &dyn IdentityProvider,
        _additional_context: &dyn crate::eqlabs_ipfs_log::access_controller::CanAppendAdditionalContext,
    ) -> Result<()> {
        let state = self.state.read().await;
        let key = entry.get_identity().id();

        for allowed_key in state.write_access.iter() {
            if allowed_key == key || allowed_key == "*" {
                // Se a chave for autorizada, verifica a identidade
                return identity_provider.verify_identity(entry.get_identity()).await;
            }
        }

        Err(GuardianError::Store("Chave não tem permissão de escrita".to_string()))
    }

    /// equivalente a GetAuthorizedByRole em go
    pub async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>> {
        let state = self.state.read().await;
        
        // Emulando a lógica Go: 'admin' e 'write' são a mesma coisa para este controlador.
        if role == "admin" || role == "write" {
            Ok(state.write_access.clone())
        } else {
            Ok(vec![])
        }
    }

    /// equivalente a Grant em go
    pub async fn grant(&self, _capability: &str, _key_id: &str) -> Result<()> {
        Err(GuardianError::Store("Não implementado - não existe na versão JS".to_string()))
    }

    /// equivalente a Revoke em go
    pub async fn revoke(&self, _capability: &str, _key_id: &str) -> Result<()> {
        Err(GuardianError::Store("Não implementado - não existe na versão JS".to_string()))
    }

    /// equivalente a Load em go
    pub async fn load(&self, address: &str) -> Result<()> {
        let state = self.state.read().await;
        debug!(target: "GuardianDB::ac::ipfs", "Lendo permissões do controlador de acesso IPFS no hash {}", address);
        drop(state); // Liberamos o lock de leitura antes das operações de escrita

        let cid = Cid::try_from(address)?;
        
        // 1. Lê o manifesto CBOR principal
        let manifest_stream = self.ipfs.cat(&cid.to_string());
        let manifest_bytes_vec = manifest_stream.try_collect::<Vec<_>>().await?;
        let manifest_data: Vec<u8> = manifest_bytes_vec.iter()
            .flat_map(|bytes| bytes.iter())
            .copied()
            .collect();
        
        let manifest: Manifest = serde_cbor::from_slice(&manifest_data)?;

        // 2. Lê o conteúdo real das permissões usando o endereço do manifesto
        let access_data_cid = manifest.params.address();
        let access_stream = self.ipfs.cat(&access_data_cid.to_string());
        let access_bytes_vec = access_stream.try_collect::<Vec<_>>().await?;
        let access_data_bytes: Vec<u8> = access_bytes_vec.iter()
            .flat_map(|bytes| bytes.iter())
            .copied()
            .collect();
        
        let write_access_data: CborWriteAccess = serde_cbor::from_slice(&access_data_bytes)?;

        // 3. O conteúdo é uma string JSON, que precisa ser deserializada também
        let write_access: Vec<String> = serde_json::from_str(&write_access_data.write)?;

        // 4. Atualiza o estado interno com as novas permissões
        let mut state = self.state.write().await;
        state.write_access = write_access;

        Ok(())
    }

    /// equivalente a Save em go
    pub async fn save(&self) -> Result<CreateAccessControllerOptions> {
        let state = self.state.read().await;

        let write_access_json = serde_json::to_string(&state.write_access)?;
        
        let cbor_data = CborWriteAccess { write: write_access_json };
        
        // Serializa a estrutura CBOR em bytes
        let cbor_bytes = serde_cbor::to_vec(&cbor_data)?;
        
        // Salva os bytes no IPFS
        let response = self.ipfs.add(std::io::Cursor::new(cbor_bytes)).await?;
        
        let cid = Cid::try_from(response.hash.as_str())?;

        debug!(target: "GuardianDB::ac::ipfs", "Controlador de acesso IPFS salvo no hash {}", cid);
        
        // Cria e retorna os parâmetros do novo manifesto
        Ok(CreateAccessControllerOptions::new(cid, false, "ipfs".to_string()))
    }

    /// equivalente a Close em go
    pub async fn close(&self) -> Result<()> {
         Err(GuardianError::Store("Não implementado - não existe na versão JS".to_string()))
    }

    /// equivalente a NewIPFSAccessController em go
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
            logger: Arc::new(slog::Logger::root(slog::Discard, slog::o!())), // Logger padrão slog
        };

        Ok(Self {
            ipfs: ipfs_client,
            state: RwLock::new(initial_state),
        })
    }
    
    /// equivalente a SetLogger em go
    pub async fn set_logger(&self, logger: Arc<Logger>) {
        let mut state = self.state.write().await;
        state.logger = logger;
    }

    /// equivalente a Logger em go
    pub async fn logger(&self) -> Arc<Logger> {
        let state = self.state.read().await;
        state.logger.clone()
    }
}