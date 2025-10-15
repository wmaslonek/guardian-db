use crate::access_controller::manifest::ManifestParams;
use crate::error::Result;
use crate::ipfs_log::access_controller;
use crate::ipfs_log::identity_provider::IdentityProvider;
use async_trait::async_trait;

pub type LogEntry = dyn access_controller::LogEntry;
pub type CanAppendAdditionalContext = dyn access_controller::CanAppendAdditionalContext;

/// A trait que todos os controladores de acesso do GuardianDB devem implementar.
#[async_trait]
pub trait AccessController: Send + Sync {
    /// Retorna o tipo do controlador de acesso como uma string.
    fn get_type(&self) -> &str;

    /// Retorna a lista de chaves autorizadas para uma dada permissão ("role").
    async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>>;

    /// Concede a uma nova chave uma determinada permissão.
    async fn grant(&self, capability: &str, key_id: &str) -> Result<()>;

    /// Remove a permissão de uma chave para realizar uma ação.
    async fn revoke(&self, capability: &str, key_id: &str) -> Result<()>;

    /// Carrega a configuração do controlador de acesso a partir de um endereço.
    async fn load(&self, address: &str) -> Result<()>;

    /// Salva/persiste a configuração do controlador (seu manifesto).
    async fn save(&self) -> Result<Box<dyn ManifestParams>>;

    /// Fecha o controlador e libera quaisquer recursos.
    async fn close(&self) -> Result<()>;

    /// Verifica se uma entrada pode ser adicionada ao log.
    async fn can_append(
        &self,
        entry: &dyn access_controller::LogEntry,
        identity_provider: &dyn IdentityProvider,
        additional_context: &dyn access_controller::CanAppendAdditionalContext,
    ) -> Result<()>;
}

pub type Option = Box<dyn FnOnce(&mut dyn AccessController)>;
