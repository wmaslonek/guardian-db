use crate::access_control::manifest::ManifestParams;
use crate::guardian::error::Result;
use crate::log::access_control;
use crate::log::identity_provider::IdentityProvider;
use async_trait::async_trait;

pub type LogEntry = dyn access_control::LogEntry;
pub type CanAppendAdditionalContext = dyn access_control::CanAppendAdditionalContext;

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
        entry: &dyn access_control::LogEntry,
        identity_provider: &dyn IdentityProvider,
        additional_context: &dyn access_control::CanAppendAdditionalContext,
    ) -> Result<()>;
}

pub type Option = Box<dyn FnOnce(&mut dyn AccessController)>;
