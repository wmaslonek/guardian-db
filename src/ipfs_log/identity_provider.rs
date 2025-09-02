use crate::error::Result;
use crate::ipfs_log::identity::Identity;
use async_trait::async_trait;
use libp2p_identity::{Keypair, PublicKey};
use std::sync::Arc;

/// Opções para criar uma identidade.
pub struct CreateIdentityOptions {
    pub identity_keys_path: String,
    pub id_type: String,
    pub keystore: Arc<dyn Keystore>, // equivalente ao keystore.Interface
    pub id: String,
}

/// Trait para o Keystore (equivalente ao go-ipfs-log/keystore.Interface)
#[async_trait]
pub trait Keystore: Send + Sync {
    async fn put(&self, key: &str, value: &[u8]) -> Result<()>;
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;
    async fn has(&self, key: &str) -> Result<bool>;
    async fn delete(&self, key: &str) -> Result<()>;
}

/// Trait principal do IdentityProvider (equivalente ao Go Interface)
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
    fn unmarshal_public_key(&self, data: &[u8]) -> Result<PublicKey>;
}

/// Implementação concreta do IdentityProvider para GuardianDB
pub struct GuardianDBIdentityProvider {
    keypair: Keypair,
    provider_type: String,
}

impl GuardianDBIdentityProvider {
    pub fn new() -> Self {
        Self {
            keypair: Keypair::generate_ed25519(),
            provider_type: "GuardianDB".to_string(),
        }
    }

    pub fn new_with_keypair(keypair: Keypair) -> Self {
        Self {
            keypair,
            provider_type: "GuardianDB".to_string(),
        }
    }

    pub fn public_key(&self) -> PublicKey {
        self.keypair.public()
    }
}

#[async_trait]
impl IdentityProvider for GuardianDBIdentityProvider {
    async fn get_id(&self, _opts: &CreateIdentityOptions) -> Result<String> {
        // Gera um ID baseado na chave pública
        let public_key = self.keypair.public();
        let peer_id = libp2p_identity::PeerId::from_public_key(&public_key);
        Ok(peer_id.to_string())
    }

    async fn sign_identity(&self, data: &[u8], _id: &str) -> Result<Vec<u8>> {
        // Assina os dados com a chave privada
        let signature = self.keypair.sign(data).map_err(|e| {
            crate::error::GuardianError::Store(format!("Failed to sign identity data: {}", e))
        })?;
        Ok(signature)
    }

    fn get_type(&self) -> String {
        self.provider_type.clone()
    }

    async fn verify_identity(&self, identity: &Identity) -> Result<()> {
        // Verifica se a identidade tem uma assinatura válida
        let public_key = identity.public_key().ok_or_else(|| {
            crate::error::GuardianError::Store("Identity missing public key".to_string())
        })?;

        // Usa o HashMap de assinaturas ao invés de acessar a struct Signatures diretamente
        let signatures_map = identity.signatures_map();
        let signature = signatures_map.get("publicKey").ok_or_else(|| {
            crate::error::GuardianError::Store("Identity missing publicKey signature".to_string())
        })?;

        // Reconstrói os dados que foram assinados
        let signed_data = format!("{}{}", identity.id(), identity.r#type());

        // Verifica a assinatura
        let is_valid = public_key.verify(signed_data.as_bytes(), signature);

        if is_valid {
            Ok(())
        } else {
            Err(crate::error::GuardianError::Store(
                "Invalid identity signature".to_string(),
            ))
        }
    }

    async fn sign(&self, _identity: &Identity, bytes: &[u8]) -> Result<Vec<u8>> {
        // Assina dados genéricos com a chave privada
        let signature = self.keypair.sign(bytes).map_err(|e| {
            crate::error::GuardianError::Store(format!("Failed to sign data: {}", e))
        })?;
        Ok(signature)
    }

    fn unmarshal_public_key(&self, data: &[u8]) -> Result<PublicKey> {
        PublicKey::try_decode_protobuf(data).map_err(|e| {
            crate::error::GuardianError::Store(format!("Failed to unmarshal public key: {}", e))
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
