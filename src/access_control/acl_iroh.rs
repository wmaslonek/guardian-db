use crate::access_control::{
    manifest::CreateAccessControllerOptions, manifest::Manifest, manifest::ManifestParams,
};
use crate::address::Address;
use crate::guardian::error::{GuardianError, Result};
use crate::log::{access_control::LogEntry, identity_provider::IdentityProvider};
use crate::p2p::network::client::IrohClient;
use async_trait::async_trait;
use iroh_blobs::Hash;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{Span, debug, instrument, warn};

/// CBOR-serializable representation of the write-access list, persisted to Iroh.
#[derive(Debug, Serialize, Deserialize)]
struct CborWriteAccess {
    #[serde(rename = "write")]
    write: Vec<String>,
}

/// In-memory state of the controller: the list of keys allowed to write.
struct ControllerState {
    write_access: Vec<String>,
}

/// Main structure of the Iroh access controller.
///
/// Permissions consist of a single write-access list. They are kept in memory
/// and can be persisted to / loaded from Iroh as CBOR blobs.
pub struct IrohAccessController {
    client: Arc<IrohClient>,
    state: RwLock<ControllerState>,
    span: Span,
}

impl IrohAccessController {
    /// Returns the controller type identifier.
    pub fn get_type(&self) -> &'static str {
        "iroh"
    }

    /// This controller has no address of its own, so it returns `None`.
    pub fn address(&self) -> Option<Box<dyn Address>> {
        None
    }

    /// Decides whether a log entry may be appended: the entry's identity must
    /// be in the write-access list (or the list must contain the universal
    /// `"*"` key), in which case the identity is also verified.
    #[instrument(skip(self, entry, identity_provider, _additional_context))]
    pub async fn can_append(
        &self,
        entry: &dyn LogEntry,
        identity_provider: &dyn IdentityProvider,
        _additional_context: &dyn crate::log::access_control::CanAppendAdditionalContext,
    ) -> Result<()> {
        let state = self.state.read().await;
        let key = entry.get_identity().id();

        for allowed_key in state.write_access.iter() {
            if allowed_key == key || allowed_key == "*" {
                // If the key is authorized, verify the identity.
                return identity_provider
                    .verify_identity(entry.get_identity())
                    .await;
            }
        }

        Err(GuardianError::Store(
            "Key does not have write permission".to_string(),
        ))
    }

    /// Returns the keys authorized for the given role. For this controller,
    /// `admin` and `write` map to the same write-access list; any other role
    /// returns an empty list.
    pub async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>> {
        let state = self.state.read().await;
        // 'admin' and 'write' are the same thing for this controller.
        if role == "admin" || role == "write" {
            Ok(state.write_access.clone())
        } else {
            Ok(vec![])
        }
    }

    /// Grants write access to `key_id`. Only the `write` capability is
    /// supported; the key is added only if not already present.
    #[instrument(skip(self))]
    pub async fn grant(&self, capability: &str, key_id: &str) -> Result<()> {
        if capability != "write" {
            return Err(GuardianError::Store(format!(
                "IrohAccessController only supports 'write' capability, got '{}'",
                capability
            )));
        }

        let mut state = self.state.write().await;
        if !state.write_access.contains(&key_id.to_string()) {
            state.write_access.push(key_id.to_string());
            debug!(target: "iroh_access_controller",
                capability = %capability,
                key_id = %key_id,
                total_keys = state.write_access.len(),
                "Permission granted successfully"
            );
        } else {
            debug!(target: "iroh_access_controller",
                capability = %capability,
                key_id = %key_id,
                "Permission already exists"
            );
        }
        Ok(())
    }

    /// Revokes write access from `key_id`. Only the `write` capability is
    /// supported.
    #[instrument(skip(self))]
    pub async fn revoke(&self, capability: &str, key_id: &str) -> Result<()> {
        if capability != "write" {
            return Err(GuardianError::Store(format!(
                "IrohAccessController only supports 'write' capability, got '{}'",
                capability
            )));
        }

        let mut state = self.state.write().await;
        let initial_len = state.write_access.len();
        state.write_access.retain(|k| k != key_id);

        if state.write_access.len() < initial_len {
            debug!(target: "iroh_access_controller",
                capability = %capability,
                key_id = %key_id,
                remaining_keys = state.write_access.len(),
                "Permission revoked successfully"
            );
        } else {
            debug!(target: "iroh_access_controller",
                capability = %capability,
                key_id = %key_id,
                "Permission not found for revocation"
            );
        }
        Ok(())
    }

    /// Loads the controller's permissions from Iroh at the given address.
    ///
    /// The address is a hex-encoded 32-byte hash pointing to the CBOR manifest;
    /// the manifest in turn references the CBOR blob holding the write-access
    /// list, which becomes the new internal state.
    #[instrument(skip(self), fields(address = %address))]
    pub async fn load(&self, address: &str) -> Result<()> {
        let state = self.state.read().await;
        debug!(target: "iroh_access_controller", address = %address, "Reading permissions from the Iroh access controller");
        drop(state); // Release the read lock before the write operations.

        // Parse the hex string into a Hash.
        let hash_bytes = hex::decode(address)
            .map_err(|e| GuardianError::InvalidHash(format!("Invalid hex hash: {}", e)))?;

        if hash_bytes.len() != 32 {
            return Err(GuardianError::InvalidHash(format!(
                "Hash must be 32 bytes, found {}",
                hash_bytes.len()
            )));
        }

        let mut hash_array = [0u8; 32];
        hash_array.copy_from_slice(&hash_bytes);
        let hash = Hash::from_bytes(hash_array);

        let client = self.client.clone();
        let hash_string = hex::encode(hash.as_bytes());

        // Spawn a blocking task to handle the non-Send Iroh operations
        let manifest_data = tokio::task::spawn_blocking(move || {
            // Use tokio runtime handle to run async code in blocking context
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                // 1. Read the main CBOR manifest using cat_bytes.
                let manifest_data = client
                    .cat_bytes(&hash_string)
                    .await
                    .map_err(|e| crate::guardian::error::GuardianError::Io(e.to_string()))?;

                Ok::<Vec<u8>, crate::guardian::error::GuardianError>(manifest_data)
            })
        })
        .await
        .map_err(|e| GuardianError::Store(format!("Task join error: {}", e)))??;

        let manifest: Manifest = serde_cbor::from_slice(&manifest_data)?;

        // 2. Read the permissions content using the manifest's address.
        let access_data_hash = manifest.params.address();
        let client_clone = self.client.clone();
        let access_data_hash_string = access_data_hash.to_string();

        // Spawn another blocking task for the second Iroh operation
        let access_data_bytes = tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                let access_data_bytes = client_clone
                    .cat_bytes(&access_data_hash_string)
                    .await
                    .map_err(|e| crate::guardian::error::GuardianError::Io(e.to_string()))?;

                Ok::<Vec<u8>, crate::guardian::error::GuardianError>(access_data_bytes)
            })
        })
        .await
        .map_err(|e| GuardianError::Store(format!("Task join error: {}", e)))??;

        let write_access_data: CborWriteAccess = serde_cbor::from_slice(&access_data_bytes)?;

        // 3. Extract the permissions directly from the CBOR.
        let write_access = write_access_data.write;

        // 4. Update the internal state with the new permissions.
        let mut state = self.state.write().await;
        state.write_access = write_access;

        Ok(())
    }

    /// Persists the current write-access list to Iroh as a CBOR blob and
    /// returns the manifest options referencing it.
    #[instrument(skip(self))]
    pub async fn save(&self) -> Result<CreateAccessControllerOptions> {
        let state = self.state.read().await;
        let cbor_data = CborWriteAccess {
            write: state.write_access.clone(),
        };
        // Serialize the CBOR structure into bytes.
        let cbor_bytes = serde_cbor::to_vec(&cbor_data)?;

        let client = self.client.clone();
        // Spawn a blocking task to handle the non-Send Iroh operations
        let response = tokio::task::spawn_blocking(move || {
            // Use tokio runtime handle to run async code in blocking context
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                // Store the bytes using Iroh.
                client.add_bytes(cbor_bytes).await
            })
        })
        .await
        .map_err(|e| GuardianError::Store(format!("Task join error: {}", e)))??;

        // Convert the hex hash string into a Hash.
        let hash_bytes = hex::decode(&response.hash)
            .map_err(|e| GuardianError::InvalidHash(format!("Error decoding hash: {}", e)))?;

        if hash_bytes.len() != 32 {
            return Err(GuardianError::InvalidHash(format!(
                "Invalid hash: expected 32 bytes, found {}",
                hash_bytes.len()
            )));
        }

        let mut hash_array = [0u8; 32];
        hash_array.copy_from_slice(&hash_bytes);
        let hash = Hash::from_bytes(hash_array);

        debug!(target: "iroh_access_controller", hash = %hex::encode(hash.as_bytes()), "Iroh access controller saved");
        // Build and return the parameters for the new manifest.
        Ok(CreateAccessControllerOptions::new(
            hash,
            false,
            "iroh".to_string(),
        ))
    }

    /// Closes the controller. This is a no-op since it is Iroh-based: the state
    /// lives in Iroh and there are no local resources to release.
    #[instrument(skip(self))]
    pub async fn close(&self) -> Result<()> {
        debug!(target: "iroh_access_controller", "Closing Iroh access controller");

        let state = self.state.read().await;
        debug!(target: "iroh_access_controller",
            write_access_count = state.write_access.len(),
            "Iroh access controller closed successfully"
        );

        Ok(())
    }

    /// Creates a new controller. If no `write` access is configured in the
    /// params, the provided `identity_id` is granted write access by default.
    #[instrument(skip(client, params), fields(identity_id = %identity_id))]
    pub fn new(
        client: Arc<IrohClient>,
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
            client,
            state: RwLock::new(initial_state),
            span: tracing::info_span!("iroh_access_controller", controller_type = "iroh"),
        })
    }

    /// Returns a reference to the span used for tracing context.
    pub fn span(&self) -> &Span {
        &self.span
    }
}

// AccessController trait implementation for IrohAccessController.
#[async_trait]
impl crate::access_control::traits::AccessController for IrohAccessController {
    fn get_type(&self) -> &str {
        "iroh"
    }

    async fn get_authorized_by_role(&self, role: &str) -> Result<Vec<String>> {
        let state = self.state.read().await;

        match role {
            "write" => Ok(state.write_access.clone()),
            "read" => Ok(state.write_access.clone()), // By default, whoever can write can read.
            "admin" => Ok(state.write_access.clone()), // By default, uses the same permissions.
            _ => Ok(Vec::new()),
        }
    }

    async fn grant(&self, capability: &str, key_id: &str) -> Result<()> {
        if capability != "write" {
            return Err(GuardianError::Store(format!(
                "IrohAccessController only supports 'write' capability, got '{}'",
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
                "IrohAccessController only supports 'write' capability, got '{}'",
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

    async fn save(&self) -> Result<Box<dyn crate::access_control::manifest::ManifestParams>> {
        let options = self.save().await?;
        Ok(Box::new(options))
    }

    async fn close(&self) -> Result<()> {
        IrohAccessController::close(self).await
    }

    async fn can_append(
        &self,
        entry: &dyn crate::log::access_control::LogEntry,
        _identity_provider: &dyn crate::log::identity_provider::IdentityProvider,
        _additional_context: &dyn crate::log::access_control::CanAppendAdditionalContext,
    ) -> Result<()> {
        let state = self.state.read().await;
        let entry_identity = entry.get_identity();
        let entry_id = entry_identity.id();

        // Check whether the identity has write permission.
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
