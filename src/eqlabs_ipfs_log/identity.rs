use std::collections::HashMap;
use std::cmp::Ordering;
use std::str::FromStr;

use sha2::{Sha256,Digest};
use secp256k1::{Secp256k1, Message, All, ecdsa::Signature, PublicKey, SecretKey};
use rand::{rngs::OsRng, RngCore};
use hex;

/// A struct holding identifier and public key signatures for an identity.
#[derive(Eq,PartialEq,Clone)]
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
	pub fn new (id_sign: &str, pub_sign: &str) -> Signatures {
		Signatures {
			id: id_sign.to_owned(),
			pub_key: pub_sign.to_owned(),
		}
	}

	/// Return the identifier signature.
	pub fn id (&self) -> &str {
		&self.id
	}

	/// Return the public key signature.
	pub fn pub_key (&self) -> &str {
		&self.pub_key
	}
}

/// An identity to determine ownership of the data stored in the log.
#[derive(Eq,PartialEq,Clone)]
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
	pub fn new (id: &str, pub_key: &str, signatures: Signatures) -> Identity {
		Identity {
			id: id.to_owned(),
			pub_key: pub_key.to_owned(),
			signatures: signatures,
		}
	}

	/// Return the identifier.
	pub fn id (&self) -> &str {
		&self.id
	}

	/// Return the public key.
	pub fn pub_key (&self) -> &str {
		&self.pub_key
	}

	/// Return the signatures.
	pub fn signatures (&self) -> &Signatures {
		&self.signatures
	}
}

impl Ord for Identity {
	fn cmp (&self, other: &Self) -> Ordering {
		self.id.cmp(&other.id)
	}
}

impl PartialOrd for Identity {
	fn partial_cmp (&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

///A secret key&mdash;public key pair.
pub struct Keys {
	sec_key: String,
	pub_key: String,
}

impl Keys {
	/// Construct a new secret key&mdash;public key pair.
	pub fn new (sk: &str, pk: &str) -> Keys {
		Keys {
			sec_key: sk.to_owned(),
			pub_key: pk.to_owned(),
		}
	}

	/// Return the secret key.
	pub fn sec_key (&self) -> &str {
		&self.sec_key
	}

	/// Return the public key.
	pub fn pub_key (&self) -> &str {
		&self.pub_key
	}
}

/// An identity provider, or *identificator*, to create identities,
/// store keys, and use them to sign and verify messages.
pub trait Identificator {
	/// Create a new identity from a cleartext identifier. Store the keys associated with the created identity in the identificator.
	///
	/// Currently **does not store the created identity** anywhere.
	fn create (&mut self, id: &str) -> Identity;

	/// Return the secret key&mdash;public key pair stored under the store key `key`.
	fn get (&self, key: &str) -> Option<&Keys>;

	/// Sign the message `msg` with the secret key in `keys`.
	///
	/// Returns the produced signature as a string.
	fn verify (&self, msg: &str, sig: &str, pk: &str) -> bool;

	/// Verify from the signature `sig` that the message `msg` was signed with the public key `pk`.
	///
	/// Returns `true` if it was, otherwise returns `false`.
	fn sign (&self, msg: &str, keys: &Keys) -> String;
}

/// The default identity provider, or [*identificator*],
/// modeled after OrbitDB's identity provider [implementation].
///
/// [*identificator*]: ./trait.Identificator.html
/// [implementation]: https://github.com/orbitdb/orbit-db-identity-provider/blob/master/src/orbit-db-identity-provider.js
pub struct DefaultIdentificator {
	secp: Secp256k1<All>,
	keystore: HashMap<String,Keys>,
}

impl DefaultIdentificator {
	/// Constructs a new default identificator.
	pub fn new () -> DefaultIdentificator {
		DefaultIdentificator {
			secp: Secp256k1::new(),
			keystore: HashMap::new(),
		}
	}

	fn put (&mut self, k: &str, v: Keys) {
		self.keystore.insert(k.to_owned(),v);
	}
}

impl Identificator for DefaultIdentificator {
	fn create (&mut self, id: &str) -> Identity {
		// Generate first keypair 
		let mut secret_bytes = [0u8; 32];
		OsRng.fill_bytes(&mut secret_bytes);
		let secret_key = SecretKey::from_byte_array(secret_bytes).expect("Valid secret key");
		let public_key = PublicKey::from_secret_key(&self.secp, &secret_key);
		let (sk,ih) = (&secret_key.display_secret().to_string(),&public_key.serialize_uncompressed().iter().map(|&x| format!("{:02x}",x)).collect::<String>());
		self.put(id,Keys::new(sk,ih));

		// Generate second keypair
		let mut middle_bytes = [0u8; 32];
		OsRng.fill_bytes(&mut middle_bytes);
		let middle_key = SecretKey::from_byte_array(middle_bytes).expect("Valid secret key");
		let public_key2 = PublicKey::from_secret_key(&self.secp, &middle_key);
		let (mk,pk) = (&middle_key.display_secret().to_string(),&public_key2.serialize_uncompressed().iter().map(|&x| format!("{:02x}",x)).collect::<String>());
		self.put(ih,Keys::new(mk,pk));

		let mut hasher = Sha256::new();
		Digest::update(&mut hasher, ih.as_bytes());
		let dig = hasher.finalize();
		let dig_array: [u8; 32] = dig.into();
		let message = Message::from_digest(dig_array);
		let id_sign = self.secp.sign_ecdsa(message, &middle_key);
		let mut pkis = pk.to_owned();
		pkis.push_str(&id_sign.to_string());
		let mut hasher = Sha256::new();
		Digest::update(&mut hasher, pkis.as_bytes());
		let dig = hasher.finalize();
		let dig_array: [u8; 32] = dig.into();
		let message = Message::from_digest(dig_array);
		let pub_sign = self.secp.sign_ecdsa(message, &secret_key);

		Identity::new(ih,pk,Signatures::new(&id_sign.to_string(),&pub_sign.to_string()))
	}

	fn get (&self, key: &str) -> Option<&Keys> {
		self.keystore.get(key)
	}

	fn verify (&self, msg: &str, sig: &str, pk: &str) -> bool {
		let mut hasher = Sha256::new();
		Digest::update(&mut hasher, msg.as_bytes());
		let dig = hasher.finalize();
		let dig_array: [u8; 32] = dig.into();
		let message = Message::from_digest(dig_array);
		match self.secp.verify_ecdsa(message,
		&Signature::from_str(sig).unwrap(),
		&PublicKey::from_slice(&hex::decode(pk).unwrap()).unwrap()) {
			Ok(_)	=>	true,
			_		=>	false,
		}
	}

	fn sign (&self, msg: &str, keys: &Keys) -> String {
		let mut hasher = Sha256::new();
		Digest::update(&mut hasher, msg.as_bytes());
		let dig = hasher.finalize();
		let dig_array: [u8; 32] = dig.into();
		let message = Message::from_digest(dig_array);
		let hex_decoded = hex::decode(keys.sec_key()).unwrap();
		let byte_array: [u8; 32] = hex_decoded.try_into().unwrap();
		let secret_key = SecretKey::from_byte_array(byte_array).unwrap();
		self.secp.sign_ecdsa(message, &secret_key).to_string()
	}
}
