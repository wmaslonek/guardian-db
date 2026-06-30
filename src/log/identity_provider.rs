use crate::guardian::error::Result;
use crate::log::identity::Identity;
use async_trait::async_trait;
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use iroh::{EndpointId as NodeId, SecretKey};
use std::sync::Arc;

/// Options for creating an identity.
pub struct CreateIdentityOptions {
    pub identity_keys_path: String,
    pub id_type: String,
    pub keystore: Arc<dyn Keystore>,
    pub id: String,
}

/// Trait for the Keystore.
#[async_trait]
pub trait Keystore: Send + Sync {
    async fn put(&self, key: &str, value: &[u8]) -> Result<()>;
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;
    async fn has(&self, key: &str) -> Result<bool>;
    async fn delete(&self, key: &str) -> Result<()>;
}

/// Main IdentityProvider trait.
#[async_trait]
pub trait IdentityProvider: Send + Sync {
    /// Returns the identity ID.
    async fn get_id(&self, opts: &CreateIdentityOptions) -> Result<String>;

    /// Signs an identity's data (GuardianDB public key signature).
    async fn sign_identity(&self, data: &[u8], id: &str) -> Result<Vec<u8>>;

    /// Returns the provider type (e.g. "GuardianDB", "ethereum", etc.).
    fn get_type(&self) -> String;

    /// Verifies the received identity.
    async fn verify_identity(&self, identity: &Identity) -> Result<()>;

    /// Signs a generic value with the identity.
    async fn sign(&self, identity: &Identity, bytes: &[u8]) -> Result<Vec<u8>>;

    /// Reconstructs a public key from bytes.
    fn unmarshal_public_key(&self, data: &[u8]) -> Result<VerifyingKey>;
}

/// Concrete IdentityProvider implementation for GuardianDB.
pub struct GuardianDBIdentityProvider {
    secret_key: SecretKey,
    provider_type: String,
}

impl Default for GuardianDBIdentityProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl GuardianDBIdentityProvider {
    pub fn new() -> Self {
        Self {
            secret_key: SecretKey::generate(),
            provider_type: "GuardianDB".to_string(),
        }
    }

    pub fn new_with_secret_key(secret_key: SecretKey) -> Self {
        Self {
            secret_key,
            provider_type: "GuardianDB".to_string(),
        }
    }

    pub fn public_key(&self) -> NodeId {
        self.secret_key.public()
    }

    /// Creates an instance for use in tests.
    #[cfg(test)]
    pub fn new_for_testing() -> Self {
        Self::new()
    }

    fn get_signing_key(&self) -> ed25519_dalek::SigningKey {
        let bytes = self.secret_key.to_bytes();
        ed25519_dalek::SigningKey::from_bytes(&bytes)
    }
}

#[async_trait]
impl IdentityProvider for GuardianDBIdentityProvider {
    async fn get_id(&self, _opts: &CreateIdentityOptions) -> Result<String> {
        // Return the NodeId as a string.
        let node_id = self.secret_key.public();
        Ok(node_id.to_string())
    }

    async fn sign_identity(&self, data: &[u8], _id: &str) -> Result<Vec<u8>> {
        // Sign the data with the secret key using ed25519.
        let signing_key = self.get_signing_key();
        let signature = signing_key.sign(data);
        Ok(signature.to_bytes().to_vec())
    }

    fn get_type(&self) -> String {
        self.provider_type.clone()
    }

    async fn verify_identity(&self, identity: &Identity) -> Result<()> {
        // Check whether the identity has a valid signature.
        let public_key = identity.public_key().ok_or_else(|| {
            crate::guardian::error::GuardianError::Store("Identity missing public key".to_string())
        })?;

        // Use the signatures HashMap instead of accessing the Signatures struct directly.
        let signatures_map = identity.signatures_map();
        let signature_bytes = signatures_map.get("publicKey").ok_or_else(|| {
            crate::guardian::error::GuardianError::Store(
                "Identity missing publicKey signature".to_string(),
            )
        })?;

        // Reconstruct the signature.
        let signature = Signature::from_slice(signature_bytes).map_err(|e| {
            crate::guardian::error::GuardianError::Store(format!("Invalid signature format: {}", e))
        })?;

        // Reconstruct the data that was signed.
        let signed_data = format!("{}{}", identity.id(), identity.get_type());

        // Verify the signature using ed25519_dalek.
        public_key
            .verify(signed_data.as_bytes(), &signature)
            .map_err(|e| {
                crate::guardian::error::GuardianError::Store(format!(
                    "Invalid identity signature: {}",
                    e
                ))
            })
    }

    async fn sign(&self, _identity: &Identity, bytes: &[u8]) -> Result<Vec<u8>> {
        // Sign generic data with the secret key.
        let signing_key = self.get_signing_key();
        let signature = signing_key.sign(bytes);
        Ok(signature.to_bytes().to_vec())
    }

    fn unmarshal_public_key(&self, data: &[u8]) -> Result<VerifyingKey> {
        if data.len() != 32 {
            return Err(crate::guardian::error::GuardianError::Store(
                "Invalid public key length".to_string(),
            ));
        }
        VerifyingKey::from_bytes(data.try_into().map_err(|_| {
            crate::guardian::error::GuardianError::Store("Failed to convert bytes".to_string())
        })?)
        .map_err(|e| {
            crate::guardian::error::GuardianError::Store(format!(
                "Failed to unmarshal public key: {}",
                e
            ))
        })
    }
}

/// In-memory Keystore implementation for development and testing.
use std::collections::HashMap;
use tokio::sync::RwLock;

pub struct InMemoryKeystore {
    store: RwLock<HashMap<String, Vec<u8>>>,
}

impl InMemoryKeystore {
    pub fn new() -> Self {
        Self {
            store: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryKeystore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Keystore for InMemoryKeystore {
    async fn put(&self, key: &str, value: &[u8]) -> Result<()> {
        let mut store = self.store.write().await;
        store.insert(key.to_string(), value.to_vec());
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let store = self.store.read().await;
        Ok(store.get(key).cloned())
    }

    async fn has(&self, key: &str) -> Result<bool> {
        let store = self.store.read().await;
        Ok(store.contains_key(key))
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let mut store = self.store.write().await;
        store.remove(key);
        Ok(())
    }
}
