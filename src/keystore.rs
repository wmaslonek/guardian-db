use crate::guardian::error::{GuardianError, Result};
use crate::log::identity_provider::{KeyMeta, Keystore as KeystoreInterface};
use async_trait::async_trait;
use iroh::SecretKey;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use std::sync::Arc;

#[cfg(feature = "encryption")]
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, Generate, KeyInit, Payload},
};
#[cfg(feature = "encryption")]
use zeroize::Zeroize;

const KEYSTORE_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("keystore");
/// Parallel table holding per-key lifecycle metadata JSON (D2). Kept separate
/// from `KEYSTORE_TABLE` so `get`/`enumerate_keys` over secrets are untouched.
const KEYMETA_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("keystore_meta");

/// Marker recorded in `KEYMETA_TABLE` when a keystore file holds encrypted
/// secrets. Its presence is what lets `new`/`new_encrypted` refuse a mismatched
/// mode instead of silently returning garbage key material.
#[cfg(feature = "encryption")]
const ENCRYPTION_MARKER_KEY: &str = "__guardian_encrypted__";

/// Value stored under [`ENCRYPTION_MARKER_KEY`]: the at-rest format version.
#[cfg(feature = "encryption")]
const ENCRYPTION_MARKER_VALUE: &[u8] = b"xchacha20poly1305-v1";

/// Domain separator for keystore at-rest encryption, kept distinct from the one
/// used by the payload codec so the same secret can back both without their keys
/// being related.
#[cfg(feature = "encryption")]
const KEYSTORE_DERIVE_CONTEXT: &str = "guardian-db keystore at-rest xchacha20poly1305 v1";

/// Keystore implementation that uses redb as the persistence backend
/// and is compatible with the internal 'log' interface.
///
/// # Encryption at rest
///
/// By default the stored values are the raw secrets, which is what every
/// existing keystore file on disk contains. With the `encryption` feature,
/// [`RedbKeystore::new_encrypted`] wraps each secret in XChaCha20-Poly1305
/// before it reaches redb, so a stolen device file yields nothing without the
/// master key. Key identifiers stay in the clear so `enumerate_keys` keeps
/// working without the key.
#[derive(Debug)]
pub struct RedbKeystore {
    db: Database,
    /// Encrypts secrets on the way into redb and back. `None` means values are
    /// stored verbatim — the historical behaviour and the default.
    #[cfg(feature = "encryption")]
    cipher: Option<KeystoreCipher>,
}

// Send + Sync is safe because redb::Database is thread-safe.
unsafe impl Send for RedbKeystore {}
unsafe impl Sync for RedbKeystore {}

/// Wraps secrets in XChaCha20-Poly1305 on their way into redb.
///
/// The record layout matches the payload codec's: a 24-byte random nonce
/// followed by ciphertext and tag. The key identifier is bound in as associated
/// data, so a stored secret cannot be relocated to a different identifier
/// without the authentication failing.
#[cfg(feature = "encryption")]
struct KeystoreCipher {
    cipher: XChaCha20Poly1305,
}

#[cfg(feature = "encryption")]
impl KeystoreCipher {
    const NONCE_LEN: usize = 24;
    const TAG_LEN: usize = 16;
    const MIN_LEN: usize = Self::NONCE_LEN + Self::TAG_LEN;

    fn new(mut master_key: [u8; 32]) -> Self {
        let cipher = XChaCha20Poly1305::new(&master_key.into());
        master_key.zeroize();
        Self { cipher }
    }

    fn derive(mut secret: Vec<u8>) -> Self {
        let mut key = blake3::derive_key(KEYSTORE_DERIVE_CONTEXT, &secret);
        secret.zeroize();
        let cipher = Self::new(key);
        key.zeroize();
        cipher
    }

    fn seal(&self, key_id: &str, secret: &[u8]) -> Result<Vec<u8>> {
        let nonce = XNonce::try_generate()
            .map_err(|e| GuardianError::Other(format!("System RNG unavailable: {e}")))?;
        let ciphertext = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: secret,
                    aad: key_id.as_bytes(),
                },
            )
            .map_err(|_| GuardianError::Other("Keystore encryption failed".to_string()))?;

        let mut out = Vec::with_capacity(Self::NONCE_LEN + ciphertext.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    fn open(&self, key_id: &str, stored: &[u8]) -> Result<Vec<u8>> {
        if stored.len() < Self::MIN_LEN {
            return Err(GuardianError::Other(
                "Stored key material is too short to be well-formed".to_string(),
            ));
        }
        let nonce = XNonce::try_from(&stored[..Self::NONCE_LEN])
            .map_err(|_| GuardianError::Other("Malformed keystore nonce".to_string()))?;
        self.cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &stored[Self::NONCE_LEN..],
                    aad: key_id.as_bytes(),
                },
            )
            .map_err(|_| {
                GuardianError::Other(
                    "Keystore authentication failed (wrong master key or tampered file)"
                        .to_string(),
                )
            })
    }
}

// Manual, so the master key can never reach a log line via the parent's derived Debug.
#[cfg(feature = "encryption")]
impl std::fmt::Debug for KeystoreCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeystoreCipher")
            .field("master_key", &"<redacted>")
            .finish()
    }
}

impl RedbKeystore {
    /// Creates a new RedbKeystore with secrets stored verbatim.
    /// If path is None, creates a temporary in-memory database.
    ///
    /// Errors if the file was written by [`new_encrypted`](Self::new_encrypted),
    /// since reading it without the master key would yield ciphertext in place of
    /// key material.
    pub fn new(path: Option<std::path::PathBuf>) -> Result<Self> {
        let db = Self::open_database(path)?;

        let store = Self {
            db,
            #[cfg(feature = "encryption")]
            cipher: None,
        };

        // Opening an encrypted keystore without a key would hand back ciphertext as
        // if it were key material. Refuse instead.
        #[cfg(feature = "encryption")]
        if store.read_encryption_marker()?.is_some() {
            return Err(GuardianError::Other(
                "This keystore is encrypted at rest; open it with RedbKeystore::new_encrypted"
                    .to_string(),
            ));
        }

        Ok(store)
    }

    /// Opens a keystore whose secrets are encrypted at rest with `master_key`.
    ///
    /// Key identifiers and lifecycle metadata stay in the clear — only the secret
    /// material is encrypted — so [`enumerate_keys`](KeystoreInterface::enumerate_keys)
    /// and [`key_meta`](KeystoreInterface::key_meta) work without the master key.
    ///
    /// A new file is marked as encrypted on creation. Opening an existing
    /// plaintext keystore this way, or an encrypted one with [`new`](Self::new),
    /// is an error rather than a silent misread. There is deliberately no
    /// automatic migration: converting an existing keystore means re-reading every
    /// secret with the old mode and writing it back with the new one, which the
    /// application must do explicitly.
    ///
    /// `master_key` is zeroized once the cipher is built. Where it comes from — OS
    /// keychain, Android Keystore, iOS Secure Enclave, a passphrase stretched with
    /// a memory-hard KDF — is the application's decision.
    #[cfg(feature = "encryption")]
    pub fn new_encrypted(path: Option<std::path::PathBuf>, master_key: [u8; 32]) -> Result<Self> {
        Self::new_with_cipher(path, KeystoreCipher::new(master_key))
    }

    /// Like [`new_encrypted`](Self::new_encrypted), but derives the at-rest key
    /// from arbitrary secret material with BLAKE3's KDF.
    ///
    /// This is a KDF, not a password hash: if `secret` is a human-chosen
    /// passphrase, stretch it with a memory-hard function first.
    #[cfg(feature = "encryption")]
    pub fn new_encrypted_from_secret(
        path: Option<std::path::PathBuf>,
        secret: Vec<u8>,
    ) -> Result<Self> {
        Self::new_with_cipher(path, KeystoreCipher::derive(secret))
    }

    #[cfg(feature = "encryption")]
    fn new_with_cipher(path: Option<std::path::PathBuf>, cipher: KeystoreCipher) -> Result<Self> {
        // Reuse the plain constructor for file/table setup, but bypass its
        // "encrypted, refuse to open" guard — here having a key is the point.
        let existing_marker = {
            let probe = Self::open_raw(path.clone())?;
            probe.read_encryption_marker()?
        };

        let store = Self {
            db: Self::open_database(path)?,
            cipher: Some(cipher),
        };

        match existing_marker {
            Some(marker) if marker == ENCRYPTION_MARKER_VALUE => {}
            Some(_) => {
                return Err(GuardianError::Other(
                    "Keystore was written with an unsupported at-rest encryption format"
                        .to_string(),
                ));
            }
            None => {
                // Either a brand-new file, or an existing plaintext one. Only the
                // former may be claimed; overwriting the latter's mode would make
                // its existing secrets undecryptable.
                if store.has_any_secret()? {
                    return Err(GuardianError::Other(
                        "This keystore holds unencrypted secrets; migrate them explicitly before \
                         opening it as encrypted"
                            .to_string(),
                    ));
                }
                store.write_encryption_marker()?;
            }
        }

        Ok(store)
    }

    /// Opens the redb file and ensures both tables exist, with no mode checks.
    fn open_database(path: Option<std::path::PathBuf>) -> Result<Database> {
        let db = match path {
            Some(p) => {
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        GuardianError::Other(format!("Error creating directory: {}", e))
                    })?;
                }
                Database::create(&p)
                    .map_err(|e| GuardianError::Other(format!("Error opening redb: {}", e)))?
            }
            None => Database::builder()
                .create_with_backend(redb::backends::InMemoryBackend::new())
                .map_err(|e| {
                    GuardianError::Other(format!("Error creating temporary redb: {}", e))
                })?,
        };

        let write_txn = db
            .begin_write()
            .map_err(|e| GuardianError::Other(format!("Error starting transaction: {}", e)))?;
        {
            let _ = write_txn
                .open_table(KEYSTORE_TABLE)
                .map_err(|e| GuardianError::Other(format!("Error creating table: {}", e)))?;
            let _ = write_txn
                .open_table(KEYMETA_TABLE)
                .map_err(|e| GuardianError::Other(format!("Error creating meta table: {}", e)))?;
        }
        write_txn
            .commit()
            .map_err(|e| GuardianError::Other(format!("Error committing table: {}", e)))?;

        Ok(db)
    }

    /// Opens without any encryption-mode check, for probing an existing file.
    #[cfg(feature = "encryption")]
    fn open_raw(path: Option<std::path::PathBuf>) -> Result<Self> {
        Ok(Self {
            db: Self::open_database(path)?,
            cipher: None,
        })
    }

    /// Reads the at-rest format marker, if the file carries one.
    #[cfg(feature = "encryption")]
    fn read_encryption_marker(&self) -> Result<Option<Vec<u8>>> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| GuardianError::Other(format!("Error opening keystore: {}", e)))?;
        let table = match read_txn.open_table(KEYMETA_TABLE) {
            Ok(t) => t,
            Err(_) => return Ok(None),
        };
        match table.get(ENCRYPTION_MARKER_KEY) {
            Ok(Some(v)) => Ok(Some(v.value().to_vec())),
            Ok(None) => Ok(None),
            Err(e) => Err(GuardianError::Other(format!(
                "Error reading keystore marker: {}",
                e
            ))),
        }
    }

    #[cfg(feature = "encryption")]
    fn write_encryption_marker(&self) -> Result<()> {
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| GuardianError::Other(format!("Error opening keystore: {}", e)))?;
        {
            let mut table = write_txn
                .open_table(KEYMETA_TABLE)
                .map_err(|e| GuardianError::Other(format!("Error opening meta table: {}", e)))?;
            table
                .insert(ENCRYPTION_MARKER_KEY, ENCRYPTION_MARKER_VALUE)
                .map_err(|e| GuardianError::Other(format!("Error writing marker: {}", e)))?;
        }
        write_txn
            .commit()
            .map_err(|e| GuardianError::Other(format!("Error committing marker: {}", e)))?;
        Ok(())
    }

    /// Whether the secret table holds at least one entry.
    #[cfg(feature = "encryption")]
    fn has_any_secret(&self) -> Result<bool> {
        Ok(!self.enumerate_keys_inner()?.is_empty())
    }

    /// Lists the secret identifiers. Shared by the trait method and the internal
    /// mode checks, so neither has to go through trait dispatch.
    fn enumerate_keys_inner(&self) -> Result<Vec<String>> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| GuardianError::Other(format!("Error opening keystore: {}", e)))?;
        let table = match read_txn.open_table(KEYSTORE_TABLE) {
            Ok(t) => t,
            Err(_) => return Ok(Vec::new()), // table not created yet
        };
        let mut keys = Vec::new();
        for item in table
            .iter()
            .map_err(|e| GuardianError::Other(format!("Error iterating keystore: {}", e)))?
        {
            let (k, _v) = item.map_err(|e| {
                GuardianError::Other(format!("Error reading keystore entry: {}", e))
            })?;
            keys.push(k.value().to_string());
        }
        Ok(keys)
    }

    /// Transforms a secret on its way into redb.
    fn seal_value(&self, _key_id: &str, value: &[u8]) -> Result<Vec<u8>> {
        #[cfg(feature = "encryption")]
        if let Some(cipher) = &self.cipher {
            return cipher.seal(_key_id, value);
        }
        Ok(value.to_vec())
    }

    /// Recovers a secret read from redb.
    fn open_value(&self, _key_id: &str, stored: Vec<u8>) -> Result<Vec<u8>> {
        #[cfg(feature = "encryption")]
        if let Some(cipher) = &self.cipher {
            return cipher.open(_key_id, &stored);
        }
        Ok(stored)
    }

    /// Creates a temporary in-memory keystore for testing.
    pub fn temporary() -> Result<Self> {
        Self::new(None)
    }

    /// Stores an Iroh SecretKey as bytes.
    pub async fn put_keypair(&self, key: &str, secret_key: &SecretKey) -> Result<()> {
        let encoded = secret_key.to_bytes();
        self.put(key, &encoded).await
    }

    /// Retrieves an Iroh SecretKey from bytes.
    pub async fn get_keypair(&self, key: &str) -> Result<Option<SecretKey>> {
        match self.get(key).await? {
            Some(bytes) => {
                if bytes.len() != 32 {
                    return Err(GuardianError::Other("Invalid secret key size".to_string()));
                }
                let secret_key = SecretKey::try_from(&bytes[..32]).map_err(|e| {
                    GuardianError::Other(format!("Error decoding secret key: {}", e))
                })?;
                Ok(Some(secret_key))
            }
            None => Ok(None),
        }
    }

    /// Lists all stored keys.
    pub async fn list_keys(&self) -> Result<Vec<String>> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| GuardianError::Other(format!("Error starting read: {}", e)))?;
        let table = read_txn
            .open_table(KEYSTORE_TABLE)
            .map_err(|e| GuardianError::Other(format!("Error opening table: {}", e)))?;

        let mut keys = Vec::new();
        let iter = table
            .iter()
            .map_err(|e| GuardianError::Other(format!("Error iterating: {}", e)))?;

        for entry_result in iter {
            let entry = entry_result
                .map_err(|e| GuardianError::Other(format!("Error listing keys: {}", e)))?;
            keys.push(entry.0.value().to_string());
        }

        Ok(keys)
    }

    /// Closes the database.
    pub async fn close(&self) -> Result<()> {
        // Data is already persisted via write transactions in redb.
        Ok(())
    }
}

#[async_trait]
impl KeystoreInterface for Arc<RedbKeystore> {
    async fn put(&self, key: &str, value: &[u8]) -> Result<()> {
        (**self).put(key, value).await
    }
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        (**self).get(key).await
    }
    async fn has(&self, key: &str) -> Result<bool> {
        (**self).has(key).await
    }
    async fn delete(&self, key: &str) -> Result<()> {
        (**self).delete(key).await
    }
    fn enumerate_keys(&self) -> Result<Vec<String>> {
        (**self).enumerate_keys()
    }
    fn public_key(&self, key_id: &str) -> Result<Option<String>> {
        (**self).public_key(key_id)
    }
    fn generate_key(&self, key_id: &str) -> Result<String> {
        (**self).generate_key(key_id)
    }
    fn key_meta(&self, key_id: &str) -> Result<Option<KeyMeta>> {
        (**self).key_meta(key_id)
    }
}

#[async_trait]
impl KeystoreInterface for RedbKeystore {
    async fn put(&self, key: &str, value: &[u8]) -> Result<()> {
        // Seal before the value reaches redb; a no-op unless this keystore is encrypted.
        let stored = self.seal_value(key, value)?;
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| GuardianError::Other(format!("Error inserting into keystore: {}", e)))?;
        {
            let mut table = write_txn
                .open_table(KEYSTORE_TABLE)
                .map_err(|e| GuardianError::Other(format!("Error opening table: {}", e)))?;
            table.insert(key, &stored[..]).map_err(|e| {
                GuardianError::Other(format!("Error inserting into keystore: {}", e))
            })?;
        }
        write_txn
            .commit()
            .map_err(|e| GuardianError::Other(format!("Error committing insertion: {}", e)))?;
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| GuardianError::Other(format!("Error retrieving from keystore: {}", e)))?;
        let table = read_txn
            .open_table(KEYSTORE_TABLE)
            .map_err(|e| GuardianError::Other(format!("Error opening table: {}", e)))?;
        match table.get(key) {
            Ok(Some(value)) => self.open_value(key, value.value().to_vec()).map(Some),
            Ok(None) => Ok(None),
            Err(e) => Err(GuardianError::Other(format!(
                "Error retrieving from keystore: {}",
                e
            ))),
        }
    }

    async fn has(&self, key: &str) -> Result<bool> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| GuardianError::Other(format!("Error checking key in keystore: {}", e)))?;
        let table = read_txn
            .open_table(KEYSTORE_TABLE)
            .map_err(|e| GuardianError::Other(format!("Error opening table: {}", e)))?;
        match table.get(key) {
            Ok(Some(_)) => Ok(true),
            Ok(None) => Ok(false),
            Err(e) => Err(GuardianError::Other(format!(
                "Error checking key in keystore: {}",
                e
            ))),
        }
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| GuardianError::Other(format!("Error removing from keystore: {}", e)))?;
        {
            let mut table = write_txn
                .open_table(KEYSTORE_TABLE)
                .map_err(|e| GuardianError::Other(format!("Error opening table: {}", e)))?;
            table.remove(key).map_err(|e| {
                GuardianError::Other(format!("Error removing from keystore: {}", e))
            })?;
        }
        write_txn
            .commit()
            .map_err(|e| GuardianError::Other(format!("Error committing removal: {}", e)))?;
        Ok(())
    }

    fn enumerate_keys(&self) -> Result<Vec<String>> {
        // Identifiers are never encrypted, so this works with or without the master key.
        self.enumerate_keys_inner()
    }

    fn public_key(&self, key_id: &str) -> Result<Option<String>> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| GuardianError::Other(format!("Error opening keystore: {}", e)))?;
        let table = match read_txn.open_table(KEYSTORE_TABLE) {
            Ok(t) => t,
            Err(_) => return Ok(None),
        };
        match table.get(key_id) {
            // Must go through open_value: on an encrypted keystore the raw bytes are
            // ciphertext, and deriving a public key from those would be nonsense.
            Ok(Some(v)) => {
                let secret = self.open_value(key_id, v.value().to_vec())?;
                Ok(crate::log::identity_provider::derive_public(&secret))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(GuardianError::Other(format!(
                "Error reading keystore: {}",
                e
            ))),
        }
    }

    fn generate_key(&self, key_id: &str) -> Result<String> {
        use zeroize::Zeroize;
        let (mut bytes, public) = crate::log::identity_provider::new_secret_bytes();
        // Do the fallible write in a closure so the transient secret is always
        // wiped afterwards, on both the success and error paths.
        let stored: Result<()> = (|| {
            // Seal inside the closure so a failure here still hits the zeroize below.
            let sealed = self.seal_value(key_id, &bytes)?;
            let write_txn = self
                .db
                .begin_write()
                .map_err(|e| GuardianError::Other(format!("Error opening keystore: {}", e)))?;
            {
                let mut table = write_txn
                    .open_table(KEYSTORE_TABLE)
                    .map_err(|e| GuardianError::Other(format!("Error opening table: {}", e)))?;
                table
                    .insert(key_id, &sealed[..])
                    .map_err(|e| GuardianError::Other(format!("Error inserting key: {}", e)))?;
            }
            {
                // Record/refresh lifecycle metadata in the parallel table (D2),
                // in the same transaction so secret + metadata stay consistent.
                let mut meta_table = write_txn.open_table(KEYMETA_TABLE).map_err(|e| {
                    GuardianError::Other(format!("Error opening meta table: {}", e))
                })?;
                let now = KeyMeta::now_secs();
                let prior = meta_table
                    .get(key_id)
                    .ok()
                    .flatten()
                    .and_then(|v| KeyMeta::from_json(v.value()));
                let meta = match prior {
                    Some(mut m) => {
                        m.rotated_count = m.rotated_count.saturating_add(1);
                        m.updated_at = now;
                        m
                    }
                    None => KeyMeta {
                        created_at: now,
                        updated_at: now,
                        kind: "ed25519".to_string(),
                        rotated_count: 0,
                    },
                };
                meta_table
                    .insert(key_id, &meta.to_json()[..])
                    .map_err(|e| GuardianError::Other(format!("Error inserting meta: {}", e)))?;
            }
            write_txn
                .commit()
                .map_err(|e| GuardianError::Other(format!("Error committing key: {}", e)))?;
            Ok(())
        })();
        bytes.zeroize();
        stored.map(|()| public)
    }

    fn key_meta(&self, key_id: &str) -> Result<Option<KeyMeta>> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| GuardianError::Other(format!("Error opening keystore: {}", e)))?;
        let table = match read_txn.open_table(KEYMETA_TABLE) {
            Ok(t) => t,
            Err(_) => return Ok(None),
        };
        match table.get(key_id) {
            Ok(Some(v)) => Ok(KeyMeta::from_json(v.value())),
            Ok(None) => Ok(None),
            Err(e) => Err(GuardianError::Other(format!("Error reading meta: {}", e))),
        }
    }
}

/// Factory function to create keystores based on configuration.
pub fn create_keystore(
    directory: Option<std::path::PathBuf>,
) -> Result<Arc<dyn KeystoreInterface + Send + Sync>> {
    let keystore = RedbKeystore::new(directory)?;
    Ok(Arc::new(keystore))
}

/// Creates a temporary in-memory keystore.
pub fn create_temp_keystore() -> Result<Arc<dyn KeystoreInterface + Send + Sync>> {
    let keystore = RedbKeystore::temporary()?;
    Ok(Arc::new(keystore))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_redb_keystore_basic_operations() {
        let keystore = RedbKeystore::temporary().unwrap();

        // Test put/get/has
        let key = "test_key";
        let value = b"test_value";

        assert!(!keystore.has(key).await.unwrap());

        keystore.put(key, value).await.unwrap();
        assert!(keystore.has(key).await.unwrap());

        let retrieved = keystore.get(key).await.unwrap().unwrap();
        assert_eq!(retrieved, value);

        // Test delete
        keystore.delete(key).await.unwrap();
        assert!(!keystore.has(key).await.unwrap());
    }

    #[test]
    fn test_generate_key_persists_across_reopen() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("admin_keystore");

        // Generate a key, then drop the store (releases the redb lock).
        let public = {
            let ks = RedbKeystore::new(Some(path.clone())).unwrap();
            let p = ks.generate_key("k1").unwrap();
            assert!(!p.is_empty());
            // The derived public matches what public_key reports.
            assert_eq!(ks.public_key("k1").unwrap().as_deref(), Some(p.as_str()));
            p
        };

        // Reopen the same path: the generated key must still be there.
        let ks2 = RedbKeystore::new(Some(path)).unwrap();
        assert_eq!(
            ks2.public_key("k1").unwrap().as_deref(),
            Some(public.as_str())
        );
        assert!(ks2.enumerate_keys().unwrap().iter().any(|k| k == "k1"));
    }

    #[tokio::test]
    async fn test_keypair_storage() {
        let keystore = RedbKeystore::temporary().unwrap();
        let key_name = "test_keypair";

        // Generate a secret key
        let original_secret = SecretKey::generate();

        // Store it
        keystore
            .put_keypair(key_name, &original_secret)
            .await
            .unwrap();

        // Retrieve it
        let retrieved_secret = keystore.get_keypair(key_name).await.unwrap().unwrap();

        // Compare public keys
        assert_eq!(original_secret.public(), retrieved_secret.public());
    }

    #[test]
    fn test_key_meta_tracks_creation_and_rotation() {
        let ks = RedbKeystore::temporary().unwrap();
        // No metadata before the key exists.
        assert!(ks.key_meta("k1").unwrap().is_none());

        // First generation: rotated_count == 0, kind ed25519, timestamps set.
        ks.generate_key("k1").unwrap();
        let m1 = ks.key_meta("k1").unwrap().expect("meta after generate");
        assert_eq!(m1.kind, "ed25519");
        assert_eq!(m1.rotated_count, 0);
        assert!(m1.created_at > 0);

        // Regenerating the same id counts as a rotation and preserves created_at.
        ks.generate_key("k1").unwrap();
        let m2 = ks.key_meta("k1").unwrap().unwrap();
        assert_eq!(m2.rotated_count, 1);
        assert_eq!(m2.created_at, m1.created_at);

        // The secret table stays a plain 32-byte secret (metadata is a sidecar).
        assert!(ks.public_key("k1").unwrap().is_some());
        assert!(
            !ks.enumerate_keys()
                .unwrap()
                .iter()
                .any(|k| k == "keystore_meta")
        );
    }

    // ── At-rest encryption (feature = "encryption") ──

    #[cfg(feature = "encryption")]
    mod encrypted {
        use super::*;

        const MASTER: [u8; 32] = [42u8; 32];

        #[tokio::test]
        async fn round_trips_secrets_through_encryption() {
            let ks = RedbKeystore::new_encrypted(None, MASTER).unwrap();
            ks.put("k1", b"segredo").await.unwrap();
            assert_eq!(ks.get("k1").await.unwrap().unwrap(), b"segredo");
            assert!(ks.has("k1").await.unwrap());
        }

        #[tokio::test]
        async fn secret_is_not_stored_verbatim() {
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join("ks");
            let secret = b"muito-secreto-mesmo";

            {
                let ks = RedbKeystore::new_encrypted(Some(path.clone()), MASTER).unwrap();
                ks.put("k1", secret).await.unwrap();
            }

            // The point of the whole exercise: the raw file must not contain the secret.
            let raw = std::fs::read(&path).unwrap();
            assert!(
                !raw.windows(secret.len()).any(|w| w == secret),
                "secret must not appear verbatim in the keystore file"
            );
        }

        #[tokio::test]
        async fn persists_across_reopen_with_same_key() {
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join("ks");

            let public = {
                let ks = RedbKeystore::new_encrypted(Some(path.clone()), MASTER).unwrap();
                ks.generate_key("k1").unwrap()
            };

            let ks2 = RedbKeystore::new_encrypted(Some(path), MASTER).unwrap();
            assert_eq!(
                ks2.public_key("k1").unwrap().as_deref(),
                Some(public.as_str())
            );
        }

        #[tokio::test]
        async fn rejects_a_wrong_master_key() {
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join("ks");
            {
                let ks = RedbKeystore::new_encrypted(Some(path.clone()), MASTER).unwrap();
                ks.put("k1", b"segredo").await.unwrap();
            }

            // Opening succeeds (the marker matches) but reads must fail closed rather
            // than hand back garbage.
            let wrong = RedbKeystore::new_encrypted(Some(path), [7u8; 32]).unwrap();
            assert!(wrong.get("k1").await.is_err());
            assert!(wrong.public_key("k1").is_err());
        }

        #[tokio::test]
        async fn key_identifiers_remain_readable_without_the_master_key() {
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join("ks");
            {
                let ks = RedbKeystore::new_encrypted(Some(path.clone()), MASTER).unwrap();
                ks.put("k1", b"a").await.unwrap();
                ks.put("k2", b"b").await.unwrap();
            }

            let wrong = RedbKeystore::new_encrypted(Some(path), [7u8; 32]).unwrap();
            let mut keys = wrong.enumerate_keys().unwrap();
            keys.sort();
            assert_eq!(keys, vec!["k1", "k2"]);
        }

        #[tokio::test]
        async fn refuses_to_open_an_encrypted_store_in_plaintext_mode() {
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join("ks");
            {
                let ks = RedbKeystore::new_encrypted(Some(path.clone()), MASTER).unwrap();
                ks.put("k1", b"segredo").await.unwrap();
            }

            // Without this guard, `get` would return ciphertext as if it were a secret.
            assert!(RedbKeystore::new(Some(path)).is_err());
        }

        #[tokio::test]
        async fn refuses_to_claim_an_existing_plaintext_store() {
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join("ks");
            {
                let ks = RedbKeystore::new(Some(path.clone())).unwrap();
                ks.put("k1", b"segredo").await.unwrap();
            }

            // Marking it encrypted would leave the existing plaintext secret
            // permanently unreadable through the normal path.
            assert!(RedbKeystore::new_encrypted(Some(path), MASTER).is_err());
        }

        #[tokio::test]
        async fn empty_plaintext_store_can_be_adopted_as_encrypted() {
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join("ks");
            drop(RedbKeystore::new(Some(path.clone())).unwrap());

            // Nothing to lose, so adopting the file is safe and must be allowed.
            let ks = RedbKeystore::new_encrypted(Some(path), MASTER).unwrap();
            ks.put("k1", b"segredo").await.unwrap();
            assert_eq!(ks.get("k1").await.unwrap().unwrap(), b"segredo");
        }

        #[tokio::test]
        async fn keypair_helpers_work_under_encryption() {
            let ks = RedbKeystore::new_encrypted(None, MASTER).unwrap();
            let original = SecretKey::generate();
            ks.put_keypair("kp", &original).await.unwrap();
            let recovered = ks.get_keypair("kp").await.unwrap().unwrap();
            assert_eq!(original.public(), recovered.public());
        }

        #[test]
        fn derived_master_key_is_deterministic() {
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join("ks");

            let public = {
                let ks =
                    RedbKeystore::new_encrypted_from_secret(Some(path.clone()), b"pass".to_vec())
                        .unwrap();
                ks.generate_key("k1").unwrap()
            };

            let ks2 =
                RedbKeystore::new_encrypted_from_secret(Some(path), b"pass".to_vec()).unwrap();
            assert_eq!(
                ks2.public_key("k1").unwrap().as_deref(),
                Some(public.as_str())
            );
        }

        #[tokio::test]
        async fn key_meta_is_tracked_under_encryption() {
            let ks = RedbKeystore::new_encrypted(None, MASTER).unwrap();
            ks.generate_key("k1").unwrap();
            let meta = ks.key_meta("k1").unwrap().expect("meta after generate");
            assert_eq!(meta.kind, "ed25519");
            assert_eq!(meta.rotated_count, 0);
        }
    }

    #[tokio::test]
    async fn test_list_keys() {
        let keystore = RedbKeystore::temporary().unwrap();

        // Add some keys
        keystore.put("key1", b"value1").await.unwrap();
        keystore.put("key2", b"value2").await.unwrap();
        keystore.put("key3", b"value3").await.unwrap();

        // List keys
        let mut keys = keystore.list_keys().await.unwrap();
        keys.sort();

        assert_eq!(keys, vec!["key1", "key2", "key3"]);
    }
}
