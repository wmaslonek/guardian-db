use crate::error::Result;
use async_trait::async_trait;
use std::sync::Arc;
use slog::Logger;
use crate::access_controller::manifest::ManifestParams;
use crate::eqlabs_ipfs_log::access_controller;
use crate::eqlabs_ipfs_log::identity_provider::IdentityProvider;

/// equivalente a LogEntry em go
pub type LogEntry = dyn access_controller::LogEntry;

/// equivalente a CanAppendAdditionalContext em go
pub type CanAppendAdditionalContext = dyn access_controller::CanAppendAdditionalContext;

/// equivalente a Interface em go
/// A trait que todos os controladores de acesso do GuardianDB devem implementar.
#[async_trait]
pub trait AccessController: Send + Sync {
    
    /// Retorna o tipo do controlador de acesso como uma string.
    fn r#type(&self) -> &str;

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

    /// Define a instância do logger a ser usada.
    async fn set_logger(&self, logger: Arc<Logger>);
    
    /// Retorna a instância atual do logger.
    async fn logger(&self) -> Arc<Logger>;
    
    /// Verifica se uma entrada pode ser adicionada ao log.
    /// Este método é específico para o GuardianDB e não está presente na interface original do OrbitDB.
    async fn can_append(
        &self,
        entry: &dyn access_controller::LogEntry,
        identity_provider: &dyn IdentityProvider,
        additional_context: &dyn access_controller::CanAppendAdditionalContext,
    ) -> Result<()>;
}

/// equivalente a Option em go
/// Define o tipo para uma "opção funcional", um padrão comum em Go para
/// configurar instâncias. Em Rust, isso pode ser traduzido como um closure.
pub type Option = Box<dyn FnOnce(&mut dyn AccessController)>;