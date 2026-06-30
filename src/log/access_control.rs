use crate::log::identity::Identity;
use crate::log::identity_provider::IdentityProvider;

/// Represents a log entry.
pub trait LogEntry: Send + Sync {
    fn get_payload(&self) -> &[u8];
    fn get_identity(&self) -> &Identity;
}

/// Represents additional context for the append check.
pub trait CanAppendAdditionalContext: Send + Sync {
    fn get_log_entries(&self) -> Vec<Box<dyn LogEntry>>;
}

/// Defines the business rule for checking whether a `LogEntry` can be appended.
pub trait CanAppend {
    fn can_append(
        &self,
        entry: &dyn LogEntry,
        identity_provider: &dyn IdentityProvider,
        context: &dyn CanAppendAdditionalContext,
    ) -> Result<(), Box<dyn std::error::Error>>;
}
