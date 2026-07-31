//! Reference AEAD implementation of [`PayloadCodec`], behind the `encryption` feature.
//!
//! Provides confidentiality and integrity for record values using
//! XChaCha20-Poly1305. This is a *batteries-included* implementation for
//! applications that want payload encryption without designing their own
//! construction; the [`PayloadCodec`] trait itself carries no dependency on it,
//! so applications with their own key schedule (ratcheting, per-group sender
//! keys, MLS) implement the trait directly instead.
//!
//! ## What this does and does not give you
//!
//! It gives confidentiality of record **values** against anyone without the key,
//! including a peer that holds a `DocTicket` for the namespace, plus tamper
//! detection and binding of each ciphertext to its store address and key.
//!
//! It does **not** give forward secrecy or post-compromise security: the key is
//! long-lived, so an attacker who obtains it can read every record ever written
//! with it, past and future. It also does not hide record keys, sizes, write
//! times or authorship. Applications needing more implement [`PayloadCodec`]
//! themselves.
//!
//! ## Key management is the caller's problem
//!
//! This type takes a key; it never generates, stores, distributes or rotates
//! one. Where the key comes from (OS keychain, passphrase KDF, a group key
//! agreement protocol) is application policy, which is precisely why GuardianDB
//! does not pick a scheme.
//!
//! ## Wire format
//!
//! ```text
//! ┌─────────┬──────────────┬───────────────────────────┐
//! │ version │ nonce        │ ciphertext ‖ Poly1305 tag │
//! │ 1 byte  │ 24 bytes     │ len(plaintext) + 16 bytes │
//! └─────────┴──────────────┴───────────────────────────┘
//! ```
//!
//! The version byte lets a future construction be introduced without a flag day:
//! a decoder can dispatch on it and keep reading records written by the old one.
//!
//! The 24-byte (extended) nonce is what makes XChaCha20 the right choice here
//! over ChaCha20: nonces are drawn at random on every write, and at 192 bits the
//! collision probability stays negligible even across many writers sharing a key
//! with no coordination — which is exactly the situation in a replicated
//! multi-writer namespace.

use super::{PayloadCodec, RecordCtx};
use crate::guardian::error::{GuardianError, Result};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, Generate, KeyInit, Payload},
};
use zeroize::Zeroize;

/// Format version for records written by [`XChaCha20Poly1305Codec`].
const VERSION_XCHACHA20POLY1305: u8 = 1;

/// Length of the extended nonce, in bytes.
const NONCE_LEN: usize = 24;

/// Poly1305 authentication tag length, in bytes.
const TAG_LEN: usize = 16;

/// Smallest possible encoded record: version + nonce + tag over empty plaintext.
const MIN_ENCODED_LEN: usize = 1 + NONCE_LEN + TAG_LEN;

/// Domain separator for [`XChaCha20Poly1305Codec::derive_from_secret`].
const DERIVE_CONTEXT: &str = "guardian-db payload-codec xchacha20poly1305 v1";

/// A [`PayloadCodec`] that encrypts record values with XChaCha20-Poly1305.
///
/// See the [module documentation](self) for the security properties, the wire
/// format, and what remains the caller's responsibility.
pub struct XChaCha20Poly1305Codec {
    cipher: XChaCha20Poly1305,
}

impl XChaCha20Poly1305Codec {
    /// Builds a codec from a 32-byte key.
    ///
    /// The caller's `key` is zeroized before returning, so the only remaining
    /// copy is the one held inside the cipher.
    pub fn new(mut key: [u8; 32]) -> Self {
        let cipher = XChaCha20Poly1305::new(&key.into());
        key.zeroize();
        Self { cipher }
    }

    /// Derives a codec key from arbitrary secret material using BLAKE3's KDF.
    ///
    /// `label` separates domains, so the same underlying secret can safely back
    /// several independent codecs (for example one per conversation) without
    /// their keys being related. Note this is a KDF, not a password hash: if
    /// `secret` is a human-chosen passphrase, stretch it with a memory-hard
    /// function first.
    pub fn derive_from_secret(label: &str, secret: &[u8]) -> Self {
        let context = format!("{DERIVE_CONTEXT} {label}");
        let mut key = blake3::derive_key(&context, secret);
        let codec = Self::new(key);
        key.zeroize();
        codec
    }
}

// Manual, so a key can never reach a log line through a derived Debug.
impl std::fmt::Debug for XChaCha20Poly1305Codec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XChaCha20Poly1305Codec")
            .field("key", &"<redacted>")
            .finish()
    }
}

impl PayloadCodec for XChaCha20Poly1305Codec {
    fn encode(&self, ctx: &RecordCtx<'_>, plaintext: &[u8]) -> Result<Vec<u8>> {
        // Fresh random nonce per write. With a 192-bit nonce this is safe without
        // coordinating a counter between the replicas sharing this key.
        let nonce = XNonce::try_generate()
            .map_err(|e| GuardianError::Other(format!("System RNG unavailable: {e}")))?;

        let aad = ctx.associated_data();
        let ciphertext = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            // The AEAD error is deliberately opaque; adding detail here would risk
            // turning it into an oracle.
            .map_err(|_| GuardianError::Other("Payload encryption failed".to_string()))?;

        let mut out = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
        out.push(VERSION_XCHACHA20POLY1305);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    fn decode(&self, ctx: &RecordCtx<'_>, stored: &[u8]) -> Result<Vec<u8>> {
        if stored.len() < MIN_ENCODED_LEN {
            return Err(GuardianError::Other(
                "Encrypted payload is too short to be well-formed".to_string(),
            ));
        }

        let version = stored[0];
        if version != VERSION_XCHACHA20POLY1305 {
            return Err(GuardianError::Other(format!(
                "Unsupported payload codec version: {version}"
            )));
        }

        // Length is guaranteed by the MIN_ENCODED_LEN check above.
        let nonce = XNonce::try_from(&stored[1..1 + NONCE_LEN])
            .map_err(|_| GuardianError::Other("Malformed payload nonce".to_string()))?;
        let ciphertext = &stored[1 + NONCE_LEN..];
        let aad = ctx.associated_data();

        self.cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: ciphertext,
                    aad: &aad,
                },
            )
            // Covers a wrong key, a corrupted record, and a ciphertext moved to a
            // different key or store. All are indistinguishable by design.
            .map_err(|_| {
                GuardianError::Other(
                    "Payload authentication failed (wrong key or tampered record)".to_string(),
                )
            })
    }

    fn codec_id(&self) -> &str {
        "xchacha20poly1305"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codec() -> XChaCha20Poly1305Codec {
        XChaCha20Poly1305Codec::new([7u8; 32])
    }

    #[test]
    fn round_trips_a_payload() {
        let codec = codec();
        let ctx = RecordCtx::new("/guardian/chat", "m/0001");
        let plaintext = b"ola mundo";

        let encoded = codec.encode(&ctx, plaintext).unwrap();
        assert_eq!(codec.decode(&ctx, &encoded).unwrap(), plaintext);
    }

    #[test]
    fn ciphertext_does_not_contain_plaintext() {
        let codec = codec();
        let ctx = RecordCtx::new("/guardian/chat", "m/0001");
        let plaintext = b"segredo muito secreto";

        let encoded = codec.encode(&ctx, plaintext).unwrap();
        assert!(
            !encoded.windows(plaintext.len()).any(|w| w == plaintext),
            "plaintext must not survive in the encoded record"
        );
    }

    #[test]
    fn encoding_is_randomised() {
        // Two encodings of the same payload must differ, or equal values would be
        // linkable across records.
        let codec = codec();
        let ctx = RecordCtx::new("/guardian/chat", "m/0001");
        let a = codec.encode(&ctx, b"same").unwrap();
        let b = codec.encode(&ctx, b"same").unwrap();
        assert_ne!(a, b);
        // ...but both still decode to the same plaintext.
        assert_eq!(codec.decode(&ctx, &a).unwrap(), b"same");
        assert_eq!(codec.decode(&ctx, &b).unwrap(), b"same");
    }

    #[test]
    fn round_trips_empty_and_large_payloads() {
        let codec = codec();
        let ctx = RecordCtx::new("/guardian/chat", "k");

        let empty = codec.encode(&ctx, b"").unwrap();
        assert_eq!(empty.len(), MIN_ENCODED_LEN);
        assert_eq!(codec.decode(&ctx, &empty).unwrap(), Vec::<u8>::new());

        let large = vec![0xABu8; 512 * 1024];
        let encoded = codec.encode(&ctx, &large).unwrap();
        assert_eq!(codec.decode(&ctx, &encoded).unwrap(), large);
    }

    #[test]
    fn rejects_a_wrong_key() {
        let ctx = RecordCtx::new("/guardian/chat", "m/0001");
        let encoded = codec().encode(&ctx, b"segredo").unwrap();

        let other = XChaCha20Poly1305Codec::new([9u8; 32]);
        assert!(other.decode(&ctx, &encoded).is_err());
    }

    #[test]
    fn rejects_tampering() {
        let codec = codec();
        let ctx = RecordCtx::new("/guardian/chat", "m/0001");
        let mut encoded = codec.encode(&ctx, b"transferir 100").unwrap();

        // Flip one bit in the ciphertext body.
        let last = encoded.len() - 1;
        encoded[last] ^= 0x01;
        assert!(codec.decode(&ctx, &encoded).is_err());
    }

    #[test]
    fn rejects_a_record_moved_to_another_key() {
        // The AAD binds a ciphertext to its key: relocating it must not verify.
        let codec = codec();
        let written = RecordCtx::new("/guardian/chat", "m/0001");
        let encoded = codec.encode(&written, b"segredo").unwrap();

        let elsewhere = RecordCtx::new("/guardian/chat", "m/0002");
        assert!(codec.decode(&elsewhere, &encoded).is_err());
    }

    #[test]
    fn rejects_a_record_moved_to_another_store() {
        let codec = codec();
        let written = RecordCtx::new("/guardian/chat", "m/0001");
        let encoded = codec.encode(&written, b"segredo").unwrap();

        let elsewhere = RecordCtx::new("/guardian/other", "m/0001");
        assert!(codec.decode(&elsewhere, &encoded).is_err());
    }

    #[test]
    fn rejects_truncated_and_unknown_version_records() {
        let codec = codec();
        let ctx = RecordCtx::new("/guardian/chat", "m/0001");

        assert!(codec.decode(&ctx, b"").is_err());
        assert!(codec.decode(&ctx, &[VERSION_XCHACHA20POLY1305; 8]).is_err());

        let mut encoded = codec.encode(&ctx, b"segredo").unwrap();
        encoded[0] = 0xFE; // unknown version
        assert!(codec.decode(&ctx, &encoded).is_err());
    }

    #[test]
    fn derive_from_secret_is_deterministic_and_label_separated() {
        let ctx = RecordCtx::new("/guardian/chat", "k");

        let a = XChaCha20Poly1305Codec::derive_from_secret("conv-1", b"master secret");
        let b = XChaCha20Poly1305Codec::derive_from_secret("conv-1", b"master secret");
        let encoded = a.encode(&ctx, b"segredo").unwrap();
        // Same label + same secret => interchangeable codecs.
        assert_eq!(b.decode(&ctx, &encoded).unwrap(), b"segredo");

        // A different label must yield an unrelated key.
        let c = XChaCha20Poly1305Codec::derive_from_secret("conv-2", b"master secret");
        assert!(c.decode(&ctx, &encoded).is_err());
    }

    #[test]
    fn debug_never_reveals_the_key() {
        let rendered = format!("{:?}", codec());
        assert!(rendered.contains("redacted"));
        assert!(!rendered.contains('7'));
    }
}
