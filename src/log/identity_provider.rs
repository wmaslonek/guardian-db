use crate::guardian::error::Result;
use crate::log::identity::Identity;
use async_trait::async_trait;
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use iroh::{NodeId, SecretKey};
use std::sync::Arc;

/// Opções para criar uma identidade.
pub struct CreateIdentityOptions {
    pub identity_keys_path: String,
    pub id_type: String,
    pub keystore: Arc<dyn Keystore>,
    pub id: String,
}

/// Trait para o Keystore
#[async_trait]
pub trait Keystore: Send + Sync {
    async fn put(&self, key: &str, value: &[u8]) -> Result<()>;
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;
    async fn has(&self, key: &str) -> Result<bool>;
    async fn delete(&self, key: &str) -> Result<()>;
}

/// Trait principal do IdentityProvider
#[async_trait]
pub trait IdentityProvider: Send + Sync {
    /// Retorna o ID da identidade.
    async fn get_id(&self, opts: &CreateIdentityOptions) -> Result<String>;

    /// Assina os dados de uma identidade (GuardianDB public key signature).
    async fn sign_identity(&self, data: &[u8], id: &str) -> Result<Vec<u8>>;

    /// Retorna o tipo do provider (ex: "GuardianDB", "ethereum", etc).
    fn get_type(&self) -> String;

    /// Verifica a identidade recebida.
    async fn verify_identity(&self, identity: &Identity) -> Result<()>;

    /// Assina um valor genérico com a identidade.
    async fn sign(&self, identity: &Identity, bytes: &[u8]) -> Result<Vec<u8>>;

    /// Reconstrói uma chave pública a partir dos bytes.
    fn unmarshal_public_key(&self, data: &[u8]) -> Result<VerifyingKey>;
}

/// Implementação concreta do IdentityProvider para GuardianDB
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
        use rand_core::OsRng;
        Self {
            secret_key: SecretKey::generate(OsRng),
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

    /// Cria uma instância para uso em testes
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
        // Retorna o NodeId como string
        let node_id = self.secret_key.public();
        Ok(node_id.to_string())
    }

    async fn sign_identity(&self, data: &[u8], _id: &str) -> Result<Vec<u8>> {
        // Assina os dados com a chave secreta usando ed25519
        let signing_key = self.get_signing_key();
        let signature = signing_key.sign(data);
        Ok(signature.to_bytes().to_vec())
    }

    fn get_type(&self) -> String {
        self.provider_type.clone()
    }

    async fn verify_identity(&self, identity: &Identity) -> Result<()> {
        // Verifica se a identidade tem uma assinatura válida
        let public_key = identity.public_key().ok_or_else(|| {
            crate::guardian::error::GuardianError::Store("Identity missing public key".to_string())
        })?;

        // Usa o HashMap de assinaturas ao invés de acessar a struct Signatures diretamente
        let signatures_map = identity.signatures_map();
        let signature_bytes = signatures_map.get("publicKey").ok_or_else(|| {
            crate::guardian::error::GuardianError::Store(
                "Identity missing publicKey signature".to_string(),
            )
        })?;

        // Reconstrói a assinatura
        let signature = Signature::from_slice(signature_bytes).map_err(|e| {
            crate::guardian::error::GuardianError::Store(format!("Invalid signature format: {}", e))
        })?;

        // Reconstrói os dados que foram assinados
        let signed_data = format!("{}{}", identity.id(), identity.get_type());

        // Verifica a assinatura usando ed25519_dalek
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
        // Assina dados genéricos com a chave secreta
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

/// Implementação de Keystore em memória para desenvolvimento e testes
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
