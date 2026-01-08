//! Testes robustos para o módulo de serialização
//!
//! Este módulo contém testes abrangentes para validar:
//! - Serialização e deserialização básica
//! - Determinismo e reprodutibilidade
//! - Limites de tamanho e proteção contra OOM
//! - Hashing com BLAKE3
//! - Estatísticas de compressão
//! - Casos de erro e validação
//! - Compatibilidade entre tipos
//! - Testes de stress e performance

use crate::guardian::error::{GuardianError, Result};
use crate::guardian::serializer::{
    SerializationStats, deserialize, serialize, serialize_and_hash, serialize_with_limit,
    serialize_with_stats,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

// ============================================================================
// ESTRUTURAS DE TESTE
// ============================================================================

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
struct SimpleData {
    id: u64,
    name: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
struct NestedData {
    simple: SimpleData,
    tags: Vec<String>,
    metadata: HashMap<String, String>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
struct ComplexData {
    nested: NestedData,
    flags: HashSet<String>,
    optional: Option<String>,
    numbers: Vec<i64>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
enum DataType {
    Text(String),
    Number(i64),
    Binary(Vec<u8>),
    Nested(Box<NestedData>),
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
struct GenericContainer<T> {
    data: T,
    timestamp: u64,
    version: u32,
}

// ============================================================================
// TESTES DE SERIALIZAÇÃO/DESERIALIZAÇÃO BÁSICA
// ============================================================================

#[test]
fn test_simple_roundtrip() {
    let data = SimpleData {
        id: 42,
        name: "test".to_string(),
    };

    let bytes = serialize(&data).expect("Serialization failed");
    let decoded: SimpleData = deserialize(&bytes).expect("Deserialization failed");

    assert_eq!(data, decoded);
    assert!(!bytes.is_empty());
}

#[test]
fn test_nested_roundtrip() {
    let mut metadata = HashMap::new();
    metadata.insert("key1".to_string(), "value1".to_string());
    metadata.insert("key2".to_string(), "value2".to_string());

    let data = NestedData {
        simple: SimpleData {
            id: 100,
            name: "nested_test".to_string(),
        },
        tags: vec!["tag1".to_string(), "tag2".to_string(), "tag3".to_string()],
        metadata,
    };

    let bytes = serialize(&data).expect("Serialization failed");
    let decoded: NestedData = deserialize(&bytes).expect("Deserialization failed");

    assert_eq!(data, decoded);
}

#[test]
fn test_complex_roundtrip() {
    let mut metadata = HashMap::new();
    metadata.insert("author".to_string(), "tester".to_string());

    let mut flags = HashSet::new();
    flags.insert("flag_a".to_string());
    flags.insert("flag_b".to_string());

    let data = ComplexData {
        nested: NestedData {
            simple: SimpleData {
                id: 999,
                name: "complex".to_string(),
            },
            tags: vec!["a".to_string(), "b".to_string()],
            metadata,
        },
        flags,
        optional: Some("optional_value".to_string()),
        numbers: vec![-100, 0, 100, 1000, i64::MAX],
    };

    let bytes = serialize(&data).expect("Serialization failed");
    let decoded: ComplexData = deserialize(&bytes).expect("Deserialization failed");

    assert_eq!(data, decoded);
}

#[test]
fn test_enum_variants() {
    let variants = vec![
        DataType::Text("hello".to_string()),
        DataType::Number(42),
        DataType::Binary(vec![1, 2, 3, 4, 5]),
        DataType::Nested(Box::new(NestedData {
            simple: SimpleData {
                id: 1,
                name: "enum_test".to_string(),
            },
            tags: vec![],
            metadata: HashMap::new(),
        })),
    ];

    for variant in variants {
        let bytes = serialize(&variant).expect("Serialization failed");
        let decoded: DataType = deserialize(&bytes).expect("Deserialization failed");
        assert_eq!(variant, decoded);
    }
}

#[test]
fn test_generic_container() {
    let container = GenericContainer {
        data: SimpleData {
            id: 123,
            name: "generic".to_string(),
        },
        timestamp: 1234567890,
        version: 1,
    };

    let bytes = serialize(&container).expect("Serialization failed");
    let decoded: GenericContainer<SimpleData> =
        deserialize(&bytes).expect("Deserialization failed");

    assert_eq!(container, decoded);
}

// ============================================================================
// TESTES DE DETERMINISMO
// ============================================================================

#[test]
fn test_determinism_simple() {
    let data = SimpleData {
        id: 42,
        name: "determinism".to_string(),
    };

    let mut all_bytes = Vec::new();
    for _ in 0..100 {
        all_bytes.push(serialize(&data).expect("Serialization failed"));
    }

    // Todos devem ser idênticos
    let first = &all_bytes[0];
    for bytes in &all_bytes[1..] {
        assert_eq!(
            first, bytes,
            "Serialization is not deterministic for simple data"
        );
    }
}

#[test]
fn test_determinism_complex() {
    let mut metadata = HashMap::new();
    metadata.insert("k1".to_string(), "v1".to_string());
    metadata.insert("k2".to_string(), "v2".to_string());

    let data = NestedData {
        simple: SimpleData {
            id: 100,
            name: "complex_determinism".to_string(),
        },
        tags: vec!["x".to_string(), "y".to_string()],
        metadata,
    };

    let hashes: Vec<String> = (0..50)
        .map(|_| {
            let bytes = serialize(&data).expect("Serialization failed");
            blake3::hash(&bytes).to_hex().to_string()
        })
        .collect();

    // Todos os hashes devem ser idênticos
    let first = &hashes[0];
    for hash in &hashes[1..] {
        assert_eq!(
            first, hash,
            "BLAKE3 hash varies - serialization is not deterministic"
        );
    }
}

#[test]
fn test_determinism_with_collections() {
    // HashMap não garante ordem, mas postcard deve ser determinístico
    let mut map1 = HashMap::new();
    map1.insert("a".to_string(), 1);
    map1.insert("b".to_string(), 2);
    map1.insert("c".to_string(), 3);

    let bytes1 = serialize(&map1).expect("Serialization failed");
    let bytes2 = serialize(&map1).expect("Serialization failed");

    // Mesma instância do HashMap sempre serializa identicamente
    assert_eq!(bytes1, bytes2);
}

// ============================================================================
// TESTES DE LIMITES E VALIDAÇÃO
// ============================================================================

#[test]
fn test_serialize_with_limit_success() {
    let data = SimpleData {
        id: 1,
        name: "small".to_string(),
    };

    // Limite generoso deve passar
    let result = serialize_with_limit(&data, 10000);
    assert!(result.is_ok());
}

#[test]
fn test_serialize_with_limit_failure() {
    let data = SimpleData {
        id: 1,
        name: "test".to_string(),
    };

    // Limite muito baixo deve falhar
    let result = serialize_with_limit(&data, 5);
    assert!(result.is_err());

    match result {
        Err(GuardianError::Serialization(msg)) => {
            assert!(msg.contains("exceeds limit"));
        }
        _ => panic!("Expected Serialization error"),
    }
}

#[test]
fn test_serialize_with_limit_exact_boundary() {
    let data = SimpleData {
        id: 1,
        name: "x".to_string(),
    };

    let bytes = serialize(&data).expect("Serialization failed");
    let exact_size = bytes.len();

    // Exato deve passar
    assert!(serialize_with_limit(&data, exact_size).is_ok());

    // Um byte a menos deve falhar
    assert!(serialize_with_limit(&data, exact_size - 1).is_err());
}

#[test]
fn test_deserialize_empty_bytes() {
    let empty: Vec<u8> = vec![];
    let result: Result<SimpleData> = deserialize(&empty);

    assert!(result.is_err());
    match result {
        Err(GuardianError::Serialization(msg)) => {
            assert!(msg.contains("empty"));
        }
        _ => panic!("Expected Serialization error for empty bytes"),
    }
}

#[test]
fn test_deserialize_invalid_bytes() {
    let invalid = vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
    let result: Result<SimpleData> = deserialize(&invalid);

    assert!(result.is_err());
    match result {
        Err(GuardianError::Serialization(_)) => {
            // Expected
        }
        _ => panic!("Expected Serialization error for invalid bytes"),
    }
}

#[test]
fn test_deserialize_truncated_bytes() {
    let data = SimpleData {
        id: 42,
        name: "truncated_test".to_string(),
    };

    let bytes = serialize(&data).expect("Serialization failed");

    // Trunca os bytes
    let truncated = &bytes[..bytes.len() / 2];
    let result: Result<SimpleData> = deserialize(truncated);

    assert!(result.is_err());
}

#[test]
fn test_deserialize_wrong_type() {
    let data1 = SimpleData {
        id: 1,
        name: "test".to_string(),
    };

    let bytes = serialize(&data1).expect("Serialization failed");

    // Tenta deserializar como tipo diferente
    let result: Result<NestedData> = deserialize(&bytes);
    assert!(result.is_err());
}

// ============================================================================
// TESTES DE HASHING
// ============================================================================

#[test]
fn test_serialize_and_hash_deterministic() {
    let data = SimpleData {
        id: 42,
        name: "hash_test".to_string(),
    };

    let hash1 = serialize_and_hash(&data).expect("Hash failed");
    let hash2 = serialize_and_hash(&data).expect("Hash failed");

    assert_eq!(hash1, hash2);
    assert_eq!(hash1.len(), 32); // BLAKE3 sempre retorna 32 bytes
}

#[test]
fn test_serialize_and_hash_different_data() {
    let data1 = SimpleData {
        id: 1,
        name: "test1".to_string(),
    };

    let data2 = SimpleData {
        id: 2,
        name: "test2".to_string(),
    };

    let hash1 = serialize_and_hash(&data1).expect("Hash failed");
    let hash2 = serialize_and_hash(&data2).expect("Hash failed");

    assert_ne!(hash1, hash2);
}

#[test]
fn test_hash_collision_resistance() {
    // Testa que pequenas mudanças resultam em hashes completamente diferentes
    let data1 = SimpleData {
        id: 1,
        name: "test".to_string(),
    };

    let data2 = SimpleData {
        id: 1,
        name: "Test".to_string(), // Apenas capitalização diferente
    };

    let hash1 = serialize_and_hash(&data1).expect("Hash failed");
    let hash2 = serialize_and_hash(&data2).expect("Hash failed");

    assert_ne!(hash1, hash2);

    // Verifica que os hashes são bem diferentes (efeito avalanche)
    let diff_bits: u32 = hash1
        .iter()
        .zip(hash2.iter())
        .map(|(a, b)| (a ^ b).count_ones())
        .sum();

    // Deve ter pelo menos 25% de bits diferentes (64 de 256)
    assert!(
        diff_bits >= 64,
        "Hash difference too small: {} bits",
        diff_bits
    );
}

// ============================================================================
// TESTES DE ESTATÍSTICAS
// ============================================================================

#[test]
fn test_serialize_with_stats() {
    let data = SimpleData {
        id: 123,
        name: "stats_test".to_string(),
    };

    let (bytes, stats) = serialize_with_stats(&data).expect("Serialization failed");

    assert!(!bytes.is_empty());
    assert_eq!(stats.serialized_size, bytes.len());
    assert!(stats.original_size > 0);
    assert!(stats.compression_ratio > 0.0);

    // Postcard geralmente é mais compacto que JSON
    assert!(
        stats.compression_ratio < 1.0,
        "Expected compression, got ratio: {}",
        stats.compression_ratio
    );
}

#[test]
fn test_stats_compression_ratio() {
    let stats = SerializationStats::new(100, 50);

    assert_eq!(stats.original_size, 100);
    assert_eq!(stats.serialized_size, 50);
    assert_eq!(stats.compression_ratio, 0.5);
}

#[test]
fn test_stats_no_compression() {
    let stats = SerializationStats::new(100, 100);
    assert_eq!(stats.compression_ratio, 1.0);
}

#[test]
fn test_stats_zero_original() {
    let stats = SerializationStats::new(0, 50);
    assert_eq!(stats.compression_ratio, 0.0);
}

// ============================================================================
// TESTES DE CASOS ESPECIAIS
// ============================================================================

#[test]
fn test_empty_collections() {
    let data = NestedData {
        simple: SimpleData {
            id: 0,
            name: String::new(),
        },
        tags: vec![],
        metadata: HashMap::new(),
    };

    let bytes = serialize(&data).expect("Serialization failed");
    let decoded: NestedData = deserialize(&bytes).expect("Deserialization failed");

    assert_eq!(data, decoded);
}

#[test]
fn test_large_strings() {
    let large_string = "x".repeat(100_000);
    let data = SimpleData {
        id: 999,
        name: large_string.clone(),
    };

    let bytes = serialize(&data).expect("Serialization failed");
    let decoded: SimpleData = deserialize(&bytes).expect("Deserialization failed");

    assert_eq!(data, decoded);
    assert_eq!(decoded.name.len(), 100_000);
}

#[test]
fn test_large_collections() {
    let tags: Vec<String> = (0..10_000).map(|i| format!("tag_{}", i)).collect();

    let data = NestedData {
        simple: SimpleData {
            id: 1,
            name: "large_collection".to_string(),
        },
        tags: tags.clone(),
        metadata: HashMap::new(),
    };

    let bytes = serialize(&data).expect("Serialization failed");
    let decoded: NestedData = deserialize(&bytes).expect("Deserialization failed");

    assert_eq!(data.tags.len(), decoded.tags.len());
    assert_eq!(data, decoded);
}

#[test]
fn test_unicode_strings() {
    let data = SimpleData {
        id: 42,
        name: "Hello 世界 🌍 Привет مرحبا".to_string(),
    };

    let bytes = serialize(&data).expect("Serialization failed");
    let decoded: SimpleData = deserialize(&bytes).expect("Deserialization failed");

    assert_eq!(data, decoded);
}

#[test]
fn test_special_characters() {
    let special = "!@#$%^&*()_+-=[]{}|;':\",./<>?`~\n\r\t\0";
    let data = SimpleData {
        id: 1,
        name: special.to_string(),
    };

    let bytes = serialize(&data).expect("Serialization failed");
    let decoded: SimpleData = deserialize(&bytes).expect("Deserialization failed");

    assert_eq!(data, decoded);
}

#[test]
fn test_option_some() {
    let data = ComplexData {
        nested: NestedData {
            simple: SimpleData {
                id: 1,
                name: "test".to_string(),
            },
            tags: vec![],
            metadata: HashMap::new(),
        },
        flags: HashSet::new(),
        optional: Some("present".to_string()),
        numbers: vec![],
    };

    let bytes = serialize(&data).expect("Serialization failed");
    let decoded: ComplexData = deserialize(&bytes).expect("Deserialization failed");

    assert_eq!(data, decoded);
    assert_eq!(decoded.optional, Some("present".to_string()));
}

#[test]
fn test_option_none() {
    let data = ComplexData {
        nested: NestedData {
            simple: SimpleData {
                id: 1,
                name: "test".to_string(),
            },
            tags: vec![],
            metadata: HashMap::new(),
        },
        flags: HashSet::new(),
        optional: None,
        numbers: vec![],
    };

    let bytes = serialize(&data).expect("Serialization failed");
    let decoded: ComplexData = deserialize(&bytes).expect("Deserialization failed");

    assert_eq!(data, decoded);
    assert_eq!(decoded.optional, None);
}

#[test]
fn test_extreme_numbers() {
    let numbers = vec![
        i64::MIN,
        i64::MIN + 1,
        -1000000,
        -1,
        0,
        1,
        1000000,
        i64::MAX - 1,
        i64::MAX,
    ];

    let data = ComplexData {
        nested: NestedData {
            simple: SimpleData {
                id: u64::MAX,
                name: "extremes".to_string(),
            },
            tags: vec![],
            metadata: HashMap::new(),
        },
        flags: HashSet::new(),
        optional: None,
        numbers,
    };

    let bytes = serialize(&data).expect("Serialization failed");
    let decoded: ComplexData = deserialize(&bytes).expect("Deserialization failed");

    assert_eq!(data, decoded);
}

// ============================================================================
// TESTES DE PERFORMANCE E STRESS
// ============================================================================

#[test]
fn test_performance_simple_data() {
    let data = SimpleData {
        id: 42,
        name: "performance".to_string(),
    };

    let iterations = 10_000;
    let start = std::time::Instant::now();

    for _ in 0..iterations {
        let bytes = serialize(&data).expect("Serialization failed");
        let _decoded: SimpleData = deserialize(&bytes).expect("Deserialization failed");
    }

    let duration = start.elapsed();
    let per_iteration = duration.as_nanos() / iterations as u128;

    println!(
        "Performance: {} iterations in {:?} ({} ns per iteration)",
        iterations, duration, per_iteration
    );

    // Deve ser razoavelmente rápido (menos de 100 microsegundos por iteração)
    assert!(per_iteration < 100_000);
}

#[test]
fn test_stress_concurrent_serialization() {
    use std::sync::Arc;
    use std::thread;

    let data = Arc::new(SimpleData {
        id: 42,
        name: "concurrent".to_string(),
    });

    let handles: Vec<_> = (0..10)
        .map(|_| {
            let data = Arc::clone(&data);
            thread::spawn(move || {
                for _ in 0..1000 {
                    let bytes = serialize(&*data).expect("Serialization failed");
                    let _decoded: SimpleData = deserialize(&bytes).expect("Deserialization failed");
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("Thread panicked");
    }
}

#[test]
fn test_size_comparison_detailed() {
    let data = NestedData {
        simple: SimpleData {
            id: 123456789,
            name: "size_comparison_test".to_string(),
        },
        tags: vec![
            "tag1".to_string(),
            "tag2".to_string(),
            "tag3".to_string(),
            "tag4".to_string(),
            "tag5".to_string(),
        ],
        metadata: {
            let mut map = HashMap::new();
            map.insert("author".to_string(), "tester".to_string());
            map.insert("version".to_string(), "1.0".to_string());
            map.insert("description".to_string(), "Test data".to_string());
            map
        },
    };

    let postcard_bytes = serialize(&data).expect("Postcard serialization failed");
    let json_bytes = serde_json::to_vec(&data).expect("JSON serialization failed");

    let reduction = 1.0 - (postcard_bytes.len() as f64 / json_bytes.len() as f64);

    println!("=== Size Comparison ===");
    println!("Postcard: {} bytes", postcard_bytes.len());
    println!("JSON:     {} bytes", json_bytes.len());
    println!("Reduction: {:.1}%", reduction * 100.0);

    assert!(
        postcard_bytes.len() < json_bytes.len(),
        "Postcard should be more compact than JSON"
    );
}

// ============================================================================
// TESTES DE INTEGRIDADE
// ============================================================================

#[test]
fn test_data_integrity_after_many_operations() {
    let original = ComplexData {
        nested: NestedData {
            simple: SimpleData {
                id: 999,
                name: "integrity_test".to_string(),
            },
            tags: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            metadata: {
                let mut map = HashMap::new();
                map.insert("key1".to_string(), "value1".to_string());
                map
            },
        },
        flags: {
            let mut set = HashSet::new();
            set.insert("flag1".to_string());
            set
        },
        optional: Some("test".to_string()),
        numbers: vec![1, 2, 3, 4, 5],
    };

    // Múltiplas operações de serialização/deserialização
    let mut current = original.clone();
    for _ in 0..100 {
        let bytes = serialize(&current).expect("Serialization failed");
        current = deserialize(&bytes).expect("Deserialization failed");
    }

    assert_eq!(original, current);
}

#[test]
fn test_hash_consistency_after_roundtrip() {
    let data = SimpleData {
        id: 42,
        name: "hash_consistency".to_string(),
    };

    let hash_before = serialize_and_hash(&data).expect("Hash failed");

    // Roundtrip
    let bytes = serialize(&data).expect("Serialization failed");
    let decoded: SimpleData = deserialize(&bytes).expect("Deserialization failed");

    let hash_after = serialize_and_hash(&decoded).expect("Hash failed");

    assert_eq!(hash_before, hash_after);
}

// ============================================================================
// TESTES DE COMPATIBILIDADE
// ============================================================================

#[test]
fn test_backward_compatibility_simulation() {
    // Simula dados "antigos" sendo deserializados
    // Em produção, você pode manter exemplos de bytes serializados de versões antigas

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct V1Data {
        id: u64,
        name: String,
    }

    let v1 = V1Data {
        id: 1,
        name: "v1".to_string(),
    };

    let bytes = serialize(&v1).expect("V1 serialization failed");

    // Deve ser capaz de deserializar como V1
    let decoded: V1Data = deserialize(&bytes).expect("V1 deserialization failed");
    assert_eq!(v1, decoded);
}

#[test]
fn test_cross_type_serialization() {
    // Verifica que cada tipo pode ser serializado e deserializado
    macro_rules! test_type {
        ($val:expr, $t:ty) => {{
            let bytes = serialize(&$val).expect("Serialization failed");
            let decoded: $t = deserialize(&bytes).expect("Deserialization failed");
            assert_eq!($val, decoded);
        }};
    }

    test_type!(true, bool);
    test_type!(42u8, u8);
    test_type!(42u16, u16);
    test_type!(42u32, u32);
    test_type!(42u64, u64);
    test_type!(42i8, i8);
    test_type!(42i16, i16);
    test_type!(42i32, i32);
    test_type!(42i64, i64);
    test_type!("string".to_string(), String);
}

// ============================================================================
// TESTES DE EDGE CASES
// ============================================================================

#[test]
fn test_deeply_nested_structures() {
    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Deep {
        value: i32,
        next: Option<Box<Deep>>,
    }

    let mut deep = Deep {
        value: 0,
        next: None,
    };

    // Cria uma estrutura com 100 níveis de profundidade
    for i in 1..100 {
        deep = Deep {
            value: i,
            next: Some(Box::new(deep)),
        };
    }

    let bytes = serialize(&deep).expect("Serialization failed");
    let decoded: Deep = deserialize(&bytes).expect("Deserialization failed");

    assert_eq!(deep, decoded);
}

#[test]
fn test_single_byte_optimization() {
    // Testa se valores pequenos são otimizados
    let small_values = vec![0u8, 1u8, 255u8];

    for val in small_values {
        let bytes = serialize(&val).expect("Serialization failed");
        println!("Value {} serialized to {} bytes", val, bytes.len());
        assert!(bytes.len() <= 2, "Single byte should be very compact");
    }
}

#[test]
fn test_binary_data() {
    let binary = vec![0u8, 1, 2, 255, 254, 253, 128, 127];
    let bytes = serialize(&binary).expect("Serialization failed");
    let decoded: Vec<u8> = deserialize(&bytes).expect("Deserialization failed");

    assert_eq!(binary, decoded);
}

#[test]
fn test_maximum_serialization_limits() {
    // Testa com estruturas grandes mas ainda manejáveis
    let large_data = SimpleData {
        id: u64::MAX,
        name: "x".repeat(1_000_000), // 1MB string
    };

    let result = serialize(&large_data);
    assert!(result.is_ok(), "Should handle large but valid data");

    let bytes = result.unwrap();
    assert!(bytes.len() > 1_000_000);

    let decoded: SimpleData = deserialize(&bytes).expect("Deserialization failed");
    assert_eq!(large_data, decoded);
}
