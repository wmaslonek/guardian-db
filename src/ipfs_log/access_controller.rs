use crate::ipfs_log::identity::Identity;
use crate::ipfs_log::identity_provider::IdentityProvider;

/// Representa uma entrada de log.
/// Equivalente ao `LogEntry` em Go.
pub trait LogEntry: Send + Sync {
    fn get_payload(&self) -> &[u8];
    fn get_identity(&self) -> &Identity;
}

/// Representa um contexto adicional para a verificação do append.
/// Equivalente ao `CanAppendAdditionalContext` em Go.
pub trait CanAppendAdditionalContext: Send + Sync {
    fn get_log_entries(&self) -> Vec<Box<dyn LogEntry>>;
}

/// Equivalente ao `Interface` em Go.
/// Define a regra de negócio para verificar se um `LogEntry` pode ser anexado.
pub trait CanAppend {
    fn can_append(
        &self,
        entry: &dyn LogEntry,
        identity_provider: &dyn IdentityProvider,
        context: &dyn CanAppendAdditionalContext,
    ) -> Result<(), Box<dyn std::error::Error>>;
}
