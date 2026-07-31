//! Thin CBOR codec helpers over `ciborium` (the maintained replacement for
//! the unmaintained `serde_cbor`), keeping the familiar `to_vec`/`from_slice`
//! call shape used across the codebase.
//!
//! Wire compatibility: everything GuardianDB serializes as CBOR (manifests,
//! access lists) is made of strings, bools, sequences and maps — for those the
//! `ciborium` encoding is byte-identical to the old `serde_cbor` one, so
//! content-addressed hashes and previously persisted data are unaffected.

use crate::guardian::error::{GuardianError, Result};
use serde::{Serialize, de::DeserializeOwned};

/// Serializes `value` to CBOR bytes.
pub fn to_vec<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(value, &mut buf).map_err(|e| GuardianError::Cbor(e.to_string()))?;
    Ok(buf)
}

/// Deserializes a `T` from CBOR bytes.
pub fn from_slice<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    ciborium::de::from_reader(bytes).map_err(|e| GuardianError::Cbor(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Sample {
        name: String,
        flag: bool,
        items: Vec<String>,
    }

    #[test]
    fn roundtrip() {
        let sample = Sample {
            name: "guardian".to_string(),
            flag: true,
            items: vec!["a".to_string(), "b".to_string()],
        };
        let bytes = to_vec(&sample).unwrap();
        let back: Sample = from_slice(&bytes).unwrap();
        assert_eq!(sample, back);
    }

    // Guards the serde_cbor → ciborium migration: for the struct shapes
    // GuardianDB persists (strings/bools/sequences/maps), the encoding must
    // stay byte-identical to serde_cbor's, or content-addressed manifest
    // hashes would change.
    #[test]
    fn encoding_matches_serde_cbor_layout() {
        let sample = Sample {
            name: "x".to_string(),
            flag: false,
            items: vec![],
        };
        // Hand-computed serde_cbor encoding:
        // a3                       map(3)
        //   64 6e616d65            text(4) "name"
        //   61 78                  text(1) "x"
        //   64 666c6167            text(4) "flag"
        //   f4                     false
        //   65 6974656d73          text(5) "items"
        //   80                     array(0)
        let expected: &[u8] = &[
            0xa3, 0x64, b'n', b'a', b'm', b'e', 0x61, b'x', 0x64, b'f', b'l', b'a', b'g', 0xf4,
            0x65, b'i', b't', b'e', b'm', b's', 0x80,
        ];
        assert_eq!(to_vec(&sample).unwrap(), expected);
    }

    #[test]
    fn invalid_input_errors() {
        let result: Result<Sample> = from_slice(&[0xff, 0x00]);
        assert!(result.is_err());
    }
}
