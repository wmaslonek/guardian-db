use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hex;
use rand::RngCore;
use std::cmp::Ordering;
use std::collections::HashMap;

/// Size of an ed25519 secret key in bytes
const SECRET_KEY_SIZE: usize = 32;

/// A struct holding identifier and public key signatures for an identity.
#[derive(Debug, Eq, PartialEq, Clone, serde::Serialize, serde::Deserialize)]
pub struct Signatures {
    id: String,
    pub_key: String,
}

impl Signatures {
    /// Constructs a signatures struct holding identifier signature `id`
    /// and public key signature `pub_key`.
    ///
    /// Should be called only by specialized [identificators],
    /// e.g. [DefaultIdentificator].
    ///
    /// [identificators]: ./trait.Identificator.html
    /// [DefaultIdentificator]: ./struct.DefaultIdentificator.html
    pub fn new(id_sign: &str, pub_sign: &str) -> Signatures {
        Signatures {
            id: id_sign.to_owned(),
            pub_key: pub_sign.to_owned(),
        }
    }

    /// Return the identifier signature.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Return the public key signature.
    pub fn pub_key(&self) -> &str {
        &self.pub_key
    }
}

/// An identity to determine ownership of the data stored in the log.
#[derive(Debug, Eq, PartialEq, Clone, serde::Serialize, serde::Deserialize)]
pub struct Identity {
    pub id: String,
    pub pub_key: String,
    pub signatures: Signatures,
    //type,
    //provider,
}

impl Identity {
    /// Constructs a new identity with the identifier `id`,
    /// public key `pub_key` and signatures `signatures`.
    ///
    /// Should be called only by specialized [identificators],
    /// e.g. [DefaultIdentificator].
    ///
    /// [identificators]: ./trait.Identificator.html
    /// [DefaultIdentificator]: ./struct.DefaultIdentificator.html
    pub fn new(id: &str, pub_key: &str, signatures: Signatures) -> Identity {
        Identity {
            id: id.to_owned(),
            pub_key: pub_key.to_owned(),
            signatures,
        }
    }

    /// Return the identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Return the public key.
    pub fn pub_key(&self) -> &str {
        &self.pub_key
    }

    /// Return the signatures.
    pub fn signatures(&self) -> &Signatures {
        &self.signatures
    }

    /// Retorna o tipo da identidade (compatibilidade com IdentityProvider)
    pub fn get_type(&self) -> &str {
        "GuardianDB" // Tipo padrão
    }

    /// Retorna as assinaturas como HashMap para compatibilidade
    pub fn signatures_map(&self) -> HashMap<String, Vec<u8>> {
        let mut map = HashMap::new();
        // As assinaturas agora são hex strings, então precisamos decodificar
        if let Ok(id_bytes) = hex::decode(self.signatures.id()) {
            map.insert("id".to_string(), id_bytes);
        }
        if let Ok(pub_key_bytes) = hex::decode(self.signatures.pub_key()) {
            map.insert("publicKey".to_string(), pub_key_bytes);
        }
        map
    }

    /// Retorna a chave pública como VerifyingKey do ed25519_dalek (se possível)
    pub fn public_key(&self) -> Option<VerifyingKey> {
        // Tenta decodificar a chave pública do formato hex para ed25519
        if let Ok(key_bytes) = hex::decode(&self.pub_key)
            && key_bytes.len() == 32
            && let Ok(bytes_array) = key_bytes.as_slice().try_into()
        {
            return VerifyingKey::from_bytes(bytes_array).ok();
        }
        None
    }
}

impl Ord for Identity {
    fn cmp(&self, other: &Self) -> Ordering {
        self.id.cmp(&other.id)
    }
}

impl PartialOrd for Identity {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A secret key—public key pair.
pub struct Keys {
    sec_key: String,
    pub_key: String,
}

impl Keys {
    /// Construct a new secret key—public key pair.
    pub fn new(sk: &str, pk: &str) -> Keys {
        Keys {
            sec_key: sk.to_owned(),
            pub_key: pk.to_owned(),
        }
    }

    /// Return the secret key.
    pub fn sec_key(&self) -> &str {
        &self.sec_key
    }

    /// Return the public key.
    pub fn pub_key(&self) -> &str {
        &self.pub_key
    }
}

/// An identity provider, or *identificator*, to create identities,
/// store keys, and use them to sign and verify messages.
pub trait Identificator {
    /// Create a new identity from a cleartext identifier. Store the keys associated with the created identity in the identificator.
    ///
    /// Currently **does not store the created identity** anywhere.
    fn create(&mut self, id: &str) -> Identity;

    /// Return the secret key—public key pair stored under the store key `key`.
    fn get(&self, key: &str) -> Option<&Keys>;

    /// Verify from the signature `sig` that the message `msg` was signed with the public key `pk`.
    ///
    /// Returns `true` if the signature is valid, otherwise returns `false`.
    fn verify(&self, msg: &str, sig: &str, pk: &str) -> bool;

    /// Sign the message `msg` with the secret key in `keys`.
    ///
    /// Returns the produced signature as a string, or an empty string if signing fails.
    fn sign(&self, msg: &str, keys: &Keys) -> String;
}
/// A default identificator implementation using ed25519 keys.
pub struct DefaultIdentificator {
    keystore: HashMap<String, Keys>,
}

impl Default for DefaultIdentificator {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultIdentificator {
    /// Constructs a new default identificator.
    pub fn new() -> DefaultIdentificator {
        DefaultIdentificator {
            keystore: HashMap::new(),
        }
    }

    fn put(&mut self, k: &str, v: Keys) {
        self.keystore.insert(k.to_owned(), v);
    }
}

impl Identificator for DefaultIdentificator {
    fn create(&mut self, id: &str) -> Identity {
        // Generate first keypair using Ed25519
        let mut secret_bytes = [0u8; SECRET_KEY_SIZE];
        rand::rng().fill_bytes(&mut secret_bytes);
        let signing_key = SigningKey::from_bytes(&secret_bytes);
        let verifying_key = signing_key.verifying_key();

        let sk = hex::encode(signing_key.to_bytes());
        let ih = hex::encode(verifying_key.to_bytes());

        self.put(id, Keys::new(&sk, &ih));

        // Generate second keypair using Ed25519 (this is the public key that will be stored)
        let mut middle_bytes = [0u8; SECRET_KEY_SIZE];
        rand::rng().fill_bytes(&mut middle_bytes);
        let middle_signing_key = SigningKey::from_bytes(&middle_bytes);
        let middle_verifying_key = middle_signing_key.verifying_key();

        let mk = hex::encode(middle_signing_key.to_bytes());
        let pk = hex::encode(middle_verifying_key.to_bytes());

        self.put(&ih, Keys::new(&mk, &pk));

        // Sign the identity hash with the middle key (id signature)
        let id_sign = middle_signing_key.sign(ih.as_bytes());

        // Create the signed data that will be verified: id + type
        let identity_type = "GuardianDB";
        let signed_data = format!("{}{}", ih, identity_type);

        // Sign the data with the middle key to create the publicKey signature
        let pub_sign = middle_signing_key.sign(signed_data.as_bytes());

        Identity::new(
            &ih,
            &pk,
            Signatures::new(
                &hex::encode(id_sign.to_bytes()),
                &hex::encode(pub_sign.to_bytes()),
            ),
        )
    }

    fn get(&self, key: &str) -> Option<&Keys> {
        self.keystore.get(key)
    }

    fn verify(&self, msg: &str, sig: &str, pk: &str) -> bool {
        // Parse the signature from hex
        let sig_bytes = match hex::decode(sig) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        };

        if sig_bytes.len() != 64 {
            return false;
        }

        let signature = match Signature::from_slice(&sig_bytes) {
            Ok(s) => s,
            Err(_) => return false,
        };

        // Parse the public key from hex
        let pk_bytes = match hex::decode(pk) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        };

        if pk_bytes.len() != 32 {
            return false;
        }

        let mut pk_array = [0u8; 32];
        pk_array.copy_from_slice(&pk_bytes);

        let verifying_key = match VerifyingKey::from_bytes(&pk_array) {
            Ok(vk) => vk,
            Err(_) => return false,
        };

        // Verify the signature
        verifying_key.verify(msg.as_bytes(), &signature).is_ok()
    }

    fn sign(&self, msg: &str, keys: &Keys) -> String {
        // Parse secret key from hex
        let sk_bytes = match hex::decode(keys.sec_key()) {
            Ok(bytes) => bytes,
            Err(_) => return String::new(),
        };

        if sk_bytes.len() != 32 {
            return String::new();
        }

        let mut sk_array = [0u8; 32];
        sk_array.copy_from_slice(&sk_bytes);

        let signing_key = SigningKey::from_bytes(&sk_array);
        let signature = signing_key.sign(msg.as_bytes());

        hex::encode(signature.to_bytes())
    }
}
