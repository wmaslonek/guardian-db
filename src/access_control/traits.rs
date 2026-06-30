use crate::access_control::manifest::ManifestParams;
use crate::guardian::error::Result;
use crate::log::access_control;
use crate::log::identity_provider::IdentityProvider;
use async_trait::async_trait;

/// Convenience alias for a log entry trait object.
pub type LogEntry = dyn access_control::LogEntry;
/// Convenience alias for the additional context passed to `can_append`.
pub type CanAppendAdditionalContext = dyn access_control::CanAppendAdditionalContext;

/// The trait that every GuardianDB access controller must implement.
#[async_trait]
pub trait AccessController: Send + Sync {
    /// Returns the access controller type as a string.
    fn get_type(&self) -> &str;

    /// Returns the list of keys authorized for a given permission ("role").
    async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>>;

    /// Grants a given permission to a new key.
    async fn grant(&self, capability: &str, key_id: &str) -> Result<()>;

    /// Removes a key's permission to perform an action.
    async fn revoke(&self, capability: &str, key_id: &str) -> Result<()>;

    /// Loads the access controller configuration from an address.
    async fn load(&self, address: &str) -> Result<()>;

    /// Saves/persists the controller configuration (its manifest).
    async fn save(&self) -> Result<Box<dyn ManifestParams>>;

    /// Closes the controller and releases any resources.
    async fn close(&self) -> Result<()>;

    /// Checks whether an entry may be appended to the log.
    async fn can_append(
        &self,
        entry: &dyn access_control::LogEntry,
        identity_provider: &dyn IdentityProvider,
        additional_context: &dyn access_control::CanAppendAdditionalContext,
    ) -> Result<()>;
}

/// A configuration callback that mutates an access controller in place,
/// used as a builder-style option when constructing controllers.
pub type Option = Box<dyn FnOnce(&mut dyn AccessController)>;
