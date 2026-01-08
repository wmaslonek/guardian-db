use crate::guardian::error::{GuardianError, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Serializa um valor para bytes usando postcard (formato binário determinístico)
///
/// Este módulo encapsula postcard para fornecer:
/// - Error handling unificado com GuardianError
/// - Logging/tracing centralizado
/// - Ponto único para adicionar features (compressão, validação, etc)
/// - Facilita migração futura se necessário
///
/// # Argumentos
/// * `value` - Qualquer tipo que implemente `Serialize`
///
/// # Retorna
/// * `Result<Vec<u8>>` - Bytes serializados ou erro
///
/// # Exemplo
/// ```ignore
/// use guardian_db::serialization::serialize;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Serialize, Deserialize)]
/// struct MyData {
///     id: String,
///     value: i32,
/// }
///
/// let data = MyData { id: "test".to_string(), value: 42 };
/// let bytes = serialize(&data)?;
/// ```
pub fn serialize<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let result = postcard::to_allocvec(value)
        .map_err(|e| GuardianError::Serialization(format!("Postcard serialization failed: {}", e)));

    if let Ok(ref bytes) = result {
        debug!("Serialized {} bytes", bytes.len());
    } else if let Err(ref e) = result {
        warn!("Serialization failed: {}", e);
    }

    result
}

/// Serializa com tamanho máximo (proteção contra estruturas muito grandes)
///
/// Útil para prevenir OOM em sistemas com recursos limitados.
///
/// # Argumentos
/// * `value` - Valor a serializar
/// * `max_bytes` - Tamanho máximo permitido
///
/// # Retorna
/// * `Result<Vec<u8>>` - Bytes ou erro se exceder limite
pub fn serialize_with_limit<T: Serialize>(value: &T, max_bytes: usize) -> Result<Vec<u8>> {
    let bytes = serialize(value)?;

    if bytes.len() > max_bytes {
        warn!(
            "Serialized data exceeds limit: {} > {} bytes",
            bytes.len(),
            max_bytes
        );
        return Err(GuardianError::Serialization(format!(
            "Serialized size {} exceeds limit of {} bytes",
            bytes.len(),
            max_bytes
        )));
    }

    Ok(bytes)
}

/// Deserializa bytes para um valor usando postcard
///
/// # Argumentos
/// * `bytes` - Slice de bytes previamente serializado com `serialize()`
///
/// # Retorna
/// * `Result<T>` - Valor deserializado ou erro
///
/// # Exemplo
/// ```ignore
/// use guardian_db::serialization::{serialize, deserialize};
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Serialize, Deserialize, PartialEq, Debug)]
/// struct MyData {
///     id: String,
///     value: i32,
/// }
///
/// let data = MyData { id: "test".to_string(), value: 42 };
/// let bytes = serialize(&data)?;
/// let decoded: MyData = deserialize(&bytes)?;
/// assert_eq!(data, decoded);
/// ```
pub fn deserialize<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T> {
    if bytes.is_empty() {
        warn!("Attempted to deserialize empty byte array");
        return Err(GuardianError::Serialization(
            "Cannot deserialize empty byte array".to_string(),
        ));
    }

    let result = postcard::from_bytes(bytes).map_err(|e| {
        GuardianError::Serialization(format!("Postcard deserialization failed: {}", e))
    });

    if result.is_ok() {
        debug!("Deserialized {} bytes", bytes.len());
    } else if let Err(ref e) = result {
        warn!("Deserialization failed: {}", e);
    }

    result
}

/// Calcula o hash BLAKE3 dos bytes serializados
///
/// Útil para verificação de integridade e geração de identificadores.
///
/// # Argumentos
/// * `value` - Valor a serializar e hashear
///
/// # Retorna
/// * `Result<[u8; 32]>` - Hash BLAKE3 (32 bytes)
pub fn serialize_and_hash<T: Serialize>(value: &T) -> Result<[u8; 32]> {
    let bytes = serialize(value)?;
    let hash = blake3::hash(&bytes);
    Ok(*hash.as_bytes())
}

/// Estatísticas de serialização (para debugging/monitoring)
#[derive(Debug, Clone)]
pub struct SerializationStats {
    pub original_size: usize,
    pub serialized_size: usize,
    pub compression_ratio: f64,
}

impl SerializationStats {
    pub fn new(original_size: usize, serialized_size: usize) -> Self {
        let compression_ratio = if original_size > 0 {
            serialized_size as f64 / original_size as f64
        } else {
            0.0
        };

        Self {
            original_size,
            serialized_size,
            compression_ratio,
        }
    }
}

/// Serializa e retorna estatísticas (útil para benchmarking)
pub fn serialize_with_stats<T: Serialize>(value: &T) -> Result<(Vec<u8>, SerializationStats)> {
    let bytes = serialize(value)?;

    // Estimativa do tamanho original (JSON como baseline)
    let json_size = serde_json::to_vec(value).map(|v| v.len()).unwrap_or(0);

    let stats = SerializationStats::new(json_size, bytes.len());

    debug!(
        "Serialization: {} bytes (postcard) vs {} bytes (JSON) - {:.1}% reduction",
        bytes.len(),
        json_size,
        (1.0 - stats.compression_ratio) * 100.0
    );

    Ok((bytes, stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct TestData {
        id: String,
        value: i32,
        nested: NestedData,
    }

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct NestedData {
        flag: bool,
        items: Vec<String>,
    }

    #[test]
    fn test_serialize_with_limit() {
        let small_data = TestData {
            id: "small".to_string(),
            value: 42,
            nested: NestedData {
                flag: true,
                items: vec!["a".to_string()],
            },
        };

        // Deve passar - limite generoso
        let result = serialize_with_limit(&small_data, 1000);
        assert!(result.is_ok());

        // Deve falhar - limite muito baixo
        let result = serialize_with_limit(&small_data, 10);
        assert!(result.is_err());

        if let Err(GuardianError::Serialization(msg)) = result {
            assert!(msg.contains("exceeds limit"));
        }
    }

    #[test]
    fn test_deserialize_empty_bytes() {
        let empty_bytes: Vec<u8> = vec![];
        let result: Result<TestData> = deserialize(&empty_bytes);

        assert!(result.is_err());
        if let Err(GuardianError::Serialization(msg)) = result {
            assert!(msg.contains("empty"));
        }
    }

    #[test]
    fn test_serialize_and_hash() {
        let data = TestData {
            id: "hash_test".to_string(),
            value: 999,
            nested: NestedData {
                flag: true,
                items: vec!["x".to_string()],
            },
        };

        // Hash deve ser determinístico
        let hash1 = serialize_and_hash(&data).expect("Hash failed");
        let hash2 = serialize_and_hash(&data).expect("Hash failed");

        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 32); // BLAKE3 = 32 bytes

        println!("✅ BLAKE3 hash (hex): {}", hex::encode(hash1));
    }

    #[test]
    fn test_serialize_with_stats() {
        let data = TestData {
            id: "stats_test".to_string(),
            value: 12345,
            nested: NestedData {
                flag: false,
                items: vec![
                    "item1".to_string(),
                    "item2".to_string(),
                    "item3".to_string(),
                ],
            },
        };

        let (bytes, stats) = serialize_with_stats(&data).expect("Serialization with stats failed");

        assert!(!bytes.is_empty());
        assert!(stats.serialized_size > 0);
        assert!(stats.original_size > 0);
        assert!(
            stats.compression_ratio < 1.0,
            "Postcard should be smaller than JSON"
        );

        println!("📊 Stats:");
        println!("   Original (JSON): {} bytes", stats.original_size);
        println!("   Postcard:        {} bytes", stats.serialized_size);
        println!("   Ratio:           {:.2}", stats.compression_ratio);
        println!(
            "   Reduction:       {:.1}%",
            (1.0 - stats.compression_ratio) * 100.0
        );
    }

    #[test]
    fn test_roundtrip() {
        let data = TestData {
            id: "test123".to_string(),
            value: 42,
            nested: NestedData {
                flag: true,
                items: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            },
        };

        let bytes = serialize(&data).expect("Serialization failed");
        let decoded: TestData = deserialize(&bytes).expect("Deserialization failed");

        assert_eq!(data, decoded);
    }

    #[test]
    fn test_determinism() {
        let data = TestData {
            id: "determinism_test".to_string(),
            value: 999,
            nested: NestedData {
                flag: false,
                items: vec!["x".to_string(), "y".to_string()],
            },
        };

        // Serializa 10 vezes e verifica que os bytes são idênticos
        let mut all_bytes = Vec::new();
        for _ in 0..10 {
            let bytes = serialize(&data).expect("Serialization failed");
            all_bytes.push(bytes);
        }

        // Verifica que todos os resultados são idênticos
        let first = &all_bytes[0];
        for bytes in &all_bytes[1..] {
            assert_eq!(first, bytes, "Serialization is not deterministic!");
        }
    }

    #[test]
    fn test_determinism_with_hash() {
        use blake3;

        let data = TestData {
            id: "hash_test".to_string(),
            value: 12345,
            nested: NestedData {
                flag: true,
                items: vec!["item1".to_string(), "item2".to_string()],
            },
        };

        // Serializa 10 vezes e verifica que o hash BLAKE3 é sempre o mesmo
        let hashes: Vec<String> = (0..10)
            .map(|_| {
                let bytes = serialize(&data).expect("Serialization failed");
                blake3::hash(&bytes).to_hex().to_string()
            })
            .collect();

        // Todos os hashes devem ser idênticos
        let first_hash = &hashes[0];
        for hash in &hashes[1..] {
            assert_eq!(
                first_hash, hash,
                "BLAKE3 hash varies - serialization is not deterministic!"
            );
        }

        println!("✅ Determinism verified - BLAKE3 hash: {}", first_hash);
    }

    #[test]
    fn test_empty_vec() {
        let data = TestData {
            id: String::new(),
            value: 0,
            nested: NestedData {
                flag: false,
                items: vec![],
            },
        };

        let bytes = serialize(&data).expect("Serialization failed");
        let decoded: TestData = deserialize(&bytes).expect("Deserialization failed");

        assert_eq!(data, decoded);
    }

    #[test]
    fn test_large_strings() {
        let large_string = "x".repeat(10000);
        let data = TestData {
            id: large_string.clone(),
            value: i32::MAX,
            nested: NestedData {
                flag: true,
                items: vec![large_string.clone(), large_string],
            },
        };

        let bytes = serialize(&data).expect("Serialization failed");
        let decoded: TestData = deserialize(&bytes).expect("Deserialization failed");

        assert_eq!(data, decoded);
    }

    #[test]
    fn test_invalid_bytes() {
        let invalid_bytes = vec![0xFF, 0xFF, 0xFF, 0xFF];
        let result: Result<TestData> = deserialize(&invalid_bytes);

        assert!(result.is_err(), "Should fail with invalid bytes");
    }

    #[test]
    fn test_size_comparison_with_json() {
        let data = TestData {
            id: "size_test".to_string(),
            value: 42,
            nested: NestedData {
                flag: true,
                items: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            },
        };

        // Postcard
        let postcard_bytes = serialize(&data).expect("Postcard serialization failed");

        // JSON (para comparação)
        let json_bytes = serde_json::to_vec(&data).expect("JSON serialization failed");

        println!("Postcard size: {} bytes", postcard_bytes.len());
        println!("JSON size: {} bytes", json_bytes.len());
        println!(
            "Reduction: {:.1}%",
            (1.0 - (postcard_bytes.len() as f64 / json_bytes.len() as f64)) * 100.0
        );

        // Postcard deve ser significativamente menor
        assert!(
            postcard_bytes.len() < json_bytes.len(),
            "Postcard should be smaller than JSON"
        );
    }
}
