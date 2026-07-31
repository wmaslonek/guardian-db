//! # Payload codec
//!
//! A transformation applied to record payloads on the path between a store's
//! public API and the underlying storage/replication layer (iroh-docs).
//!
//! The motivating problem: iroh-docs has no read-side access control. A
//! `DocTicket` is a bearer capability — whoever holds it replicates the whole
//! namespace and can read every value in it. There is no `can_read` counterpart
//! to [`AccessController::can_append`]. For applications that need
//! confidentiality from a ticket holder, encrypting the payload is therefore the
//! *only* available mechanism.
//!
//! GuardianDB deliberately does **not** pick an encryption scheme. Key
//! management is application policy (who may read, what happens to history when
//! membership changes, whether forward secrecy is required, how many devices
//! share an identity), and a fixed scheme would also disable the features that
//! need to read record contents — `sql`, `vector-index`, `embedding`, `rag` and
//! `compute` all operate on plaintext. Instead, this trait provides the single
//! place where an application can plug its own scheme in, so the "always encrypt
//! before writing" invariant is enforced by the store rather than by discipline
//! at every call site.
//!
//! ## Default behaviour
//!
//! The default is [`IdentityCodec`], a no-op. A store with no configured codec
//! behaves exactly as it did before this trait existed: bytes are stored
//! verbatim. Nothing in GuardianDB's existing feature set is affected unless an
//! application opts in.
//!
//! ## Where it applies
//!
//! Codecs are applied by iroh-docs-backed stores (`keyvalue`, `document`) to
//! record *values*. They are **not** applied to:
//!
//! - **Record keys**, which stay plaintext so that range scans, key enumeration
//!   and the in-memory index keep working. Choose keys that do not leak
//!   information.
//! - **Blobs added directly** via the client's blob API. iroh-blobs is
//!   content-addressed, so the BLAKE3 hash of a plaintext file *is* an
//!   identifier for that file: anyone who guesses the content can confirm the
//!   guess from the hash alone. Applications that need attachment
//!   confidentiality must encrypt before adding the blob.
//! - **Replication metadata** (namespace ids, author ids, entry timestamps,
//!   content lengths). A codec provides confidentiality of content, not
//!   metadata privacy.
//!
//! ## Interoperability
//!
//! A codec changes the bytes that cross the wire. Every replica of a store must
//! be configured with a codec that can decode what the others produce —
//! typically the same codec with the same key material. A peer holding a
//! `DocTicket` but not the codec key will replicate the namespace and be unable
//! to read it, which is precisely the intent.
//!
//! [`AccessController::can_append`]: crate::access_control::traits::AccessController::can_append

use crate::guardian::error::Result;
use std::sync::Arc;

#[cfg(feature = "encryption")]
pub mod aead;

#[cfg(feature = "encryption")]
pub use aead::XChaCha20Poly1305Codec;

/// Context describing the record a codec is being applied to.
///
/// Passed to both [`PayloadCodec::encode`] and [`PayloadCodec::decode`] so an
/// implementation can derive per-record key material or bind the ciphertext to
/// its location (for example by using `store_address` and `key` as AEAD
/// associated data, which prevents a ciphertext from being moved to a different
/// key or a different store undetected).
#[derive(Debug, Clone, Copy)]
pub struct RecordCtx<'a> {
    /// Full address of the store the record belongs to.
    pub store_address: &'a str,
    /// The record's key, as written by the application. Always plaintext.
    pub key: &'a str,
}

impl<'a> RecordCtx<'a> {
    /// Creates a context for a record.
    pub fn new(store_address: &'a str, key: &'a str) -> Self {
        Self { store_address, key }
    }

    /// Associated data binding a ciphertext to its store and key.
    ///
    /// Provided as a convenience so independent implementations bind the same
    /// fields in the same order, and therefore stay compatible with each other.
    pub fn associated_data(&self) -> Vec<u8> {
        let mut aad = Vec::with_capacity(self.store_address.len() + self.key.len() + 1);
        aad.extend_from_slice(self.store_address.as_bytes());
        aad.push(0x1f); // unit separator: unambiguous split between the fields
        aad.extend_from_slice(self.key.as_bytes());
        aad
    }
}

/// Transforms record payloads between the store API and the storage layer.
///
/// Implementations must be deterministic in the sense that
/// `decode(ctx, encode(ctx, x)) == x` for every `ctx` and `x`. They need not be
/// deterministic in the output of `encode` itself — a randomised nonce per call
/// is expected of any sound AEAD construction.
///
/// See the [module documentation](self) for what a codec does and does not
/// cover.
pub trait PayloadCodec: Send + Sync + std::fmt::Debug {
    /// Transforms an application payload into the bytes to be stored.
    fn encode(&self, ctx: &RecordCtx<'_>, plaintext: &[u8]) -> Result<Vec<u8>>;

    /// Recovers the application payload from stored bytes.
    ///
    /// Returns an error when the payload cannot be recovered — wrong key,
    /// truncated data, or a failed authentication tag. Callers that read many
    /// records at once (`all()`, index refresh) skip records that fail to
    /// decode rather than failing the whole operation, since a namespace may
    /// legitimately contain records this node cannot read.
    fn decode(&self, ctx: &RecordCtx<'_>, stored: &[u8]) -> Result<Vec<u8>>;

    /// Short identifier for diagnostics and logging. Must not include key
    /// material.
    fn codec_id(&self) -> &str;
}

/// The default codec: stores payloads verbatim.
///
/// Used whenever no codec is configured, so that the default behaviour of every
/// store is byte-for-byte what it was before payload codecs existed.
#[derive(Debug, Clone, Copy, Default)]
pub struct IdentityCodec;

impl PayloadCodec for IdentityCodec {
    fn encode(&self, _ctx: &RecordCtx<'_>, plaintext: &[u8]) -> Result<Vec<u8>> {
        Ok(plaintext.to_vec())
    }

    fn decode(&self, _ctx: &RecordCtx<'_>, stored: &[u8]) -> Result<Vec<u8>> {
        Ok(stored.to_vec())
    }

    fn codec_id(&self) -> &str {
        "identity"
    }
}

/// Convenience alias for a shared codec handle.
pub type SharedPayloadCodec = Arc<dyn PayloadCodec>;

/// Returns the codec to use, falling back to the no-op [`IdentityCodec`].
///
/// Stores call this once at construction so the rest of their code can treat a
/// codec as always present, without an `Option` check on every read and write.
pub fn codec_or_identity(codec: Option<SharedPayloadCodec>) -> SharedPayloadCodec {
    codec.unwrap_or_else(|| Arc::new(IdentityCodec))
}

/// Whether a codec actually transforms payloads.
///
/// Lets callers skip work (such as re-encoding on a read-modify-write path)
/// when the configured codec is the no-op one.
pub fn is_identity(codec: &SharedPayloadCodec) -> bool {
    codec.codec_id() == "identity"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_codec_round_trips_verbatim() {
        let codec = IdentityCodec;
        let ctx = RecordCtx::new("/guardian/store", "k1");
        let payload = b"hello world";

        let encoded = codec.encode(&ctx, payload).unwrap();
        // The defining property: the default must not alter stored bytes.
        assert_eq!(encoded, payload);
        assert_eq!(codec.decode(&ctx, &encoded).unwrap(), payload);
    }

    #[test]
    fn identity_codec_handles_empty_payload() {
        let codec = IdentityCodec;
        let ctx = RecordCtx::new("/guardian/store", "k1");
        assert_eq!(codec.encode(&ctx, b"").unwrap(), Vec::<u8>::new());
        assert_eq!(codec.decode(&ctx, b"").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn associated_data_distinguishes_store_and_key() {
        // The separator must make ("ab", "c") and ("a", "bc") different, or a
        // ciphertext could be relocated between keys without detection.
        let a = RecordCtx::new("ab", "c").associated_data();
        let b = RecordCtx::new("a", "bc").associated_data();
        assert_ne!(a, b);
    }

    #[test]
    fn associated_data_is_stable_for_same_inputs() {
        let a = RecordCtx::new("/guardian/s", "k").associated_data();
        let b = RecordCtx::new("/guardian/s", "k").associated_data();
        assert_eq!(a, b);
    }

    #[test]
    fn codec_or_identity_defaults_to_no_op() {
        let codec = codec_or_identity(None);
        assert_eq!(codec.codec_id(), "identity");
        assert!(is_identity(&codec));
    }

    #[test]
    fn codec_or_identity_preserves_supplied_codec() {
        let supplied: SharedPayloadCodec = Arc::new(IdentityCodec);
        let codec = codec_or_identity(Some(supplied));
        assert!(is_identity(&codec));
    }
}
