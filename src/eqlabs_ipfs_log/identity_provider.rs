use std::sync::Arc;
use async_trait::async_trait;
use libp2p_identity::PublicKey;
use crate::eqlabs_ipfs_log::identity::Identity;
use crate::error::Result;

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
