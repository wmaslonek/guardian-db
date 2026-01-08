/// Teste de determinismo: verifica que BTreeMap corrige o problema de HashMap
///
/// Este teste valida que:
/// 1. Snapshots com mesmo conteúdo geram mesmo BLAKE3 hash
/// 2. A ordem de serialização é determinística
/// 3. Log.entries usa BTreeMap (ordem garantida)
use guardian_db::guardian::serializer::{deserialize, serialize};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct MockEntry {
    id: String,
    value: Vec<u8>,
    timestamp: u64,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct MockSnapshot {
    id: String,
    entries: BTreeMap<String, MockEntry>, // BTreeMap garante ordem
    version: u32,
}

#[test]
fn test_btreemap_determinism() {
    // Cria snapshot com 50 entries em ordem aleatória
    let mut entries1 = BTreeMap::new();
    for i in (0..50).rev() {
        // Insere em ordem reversa
        entries1.insert(
            format!("key_{:03}", i),
            MockEntry {
                id: format!("entry_{}", i),
                value: vec![i as u8; 100],
                timestamp: 1000 + i as u64,
            },
        );
    }

    let snapshot1 = MockSnapshot {
        id: "test_snapshot".to_string(),
        entries: entries1,
        version: 1,
    };

    // Cria segundo snapshot com mesmas entries, mas insere em ordem diferente
    let mut entries2 = BTreeMap::new();
    for i in 0..50 {
        // Insere em ordem normal
        entries2.insert(
            format!("key_{:03}", i),
            MockEntry {
                id: format!("entry_{}", i),
                value: vec![i as u8; 100],
                timestamp: 1000 + i as u64,
            },
        );
    }

    let snapshot2 = MockSnapshot {
        id: "test_snapshot".to_string(),
        entries: entries2,
        version: 1,
    };

    // Serializa ambos
    let bytes1 = serialize(&snapshot1).expect("Serialization failed");
    let bytes2 = serialize(&snapshot2).expect("Serialization failed");

    // ✅ Bytes devem ser IDÊNTICOS (BTreeMap mantém ordem)
    assert_eq!(
        bytes1, bytes2,
        "BTreeMap serialization should be deterministic!"
    );

    // Calcula BLAKE3 hash
    let hash1 = blake3::hash(&bytes1);
    let hash2 = blake3::hash(&bytes2);

    // ✅ Hashes devem ser IDÊNTICOS
    assert_eq!(hash1, hash2, "BLAKE3 hashes should match for same content!");

    println!("✅ Determinism verified:");
    println!("   Bytes: {} (both snapshots)", bytes1.len());
    println!("   BLAKE3: {}", hash1.to_hex());
}

#[test]
fn test_multiple_serializations_same_hash() {
    let mut entries = BTreeMap::new();
    for i in 0..20 {
        entries.insert(
            format!("entry_{}", i),
            MockEntry {
                id: format!("id_{}", i),
                value: vec![i as u8; 50],
                timestamp: i as u64,
            },
        );
    }

    let snapshot = MockSnapshot {
        id: "multi_test".to_string(),
        entries,
        version: 1,
    };

    // Serializa 10 vezes
    let hashes: Vec<String> = (0..10)
        .map(|_| {
            let bytes = serialize(&snapshot).expect("Serialization failed");
            blake3::hash(&bytes).to_hex().to_string()
        })
        .collect();

    // Todos os hashes devem ser iguais
    let first_hash = &hashes[0];
    for (i, hash) in hashes.iter().enumerate() {
        assert_eq!(
            first_hash, hash,
            "Hash mismatch at iteration {}: expected {}, got {}",
            i, first_hash, hash
        );
    }

    println!(
        "✅ Multiple serializations produce same hash: {}",
        first_hash
    );
}

#[test]
fn test_roundtrip_preserves_order() {
    let mut entries = BTreeMap::new();

    // Insere em ordem específica
    let keys = ["zebra", "alpha", "beta", "delta", "charlie"];
    for (i, key) in keys.iter().enumerate() {
        entries.insert(
            key.to_string(),
            MockEntry {
                id: format!("entry_{}", i),
                value: vec![i as u8; 10],
                timestamp: i as u64,
            },
        );
    }

    let original = MockSnapshot {
        id: "order_test".to_string(),
        entries,
        version: 1,
    };

    // Serializa e deserializa
    let bytes = serialize(&original).expect("Serialization failed");
    let decoded: MockSnapshot = deserialize(&bytes).expect("Deserialization failed");

    // Verifica que a ordem foi preservada (alfabética, não de inserção)
    let original_keys: Vec<_> = original.entries.keys().collect();
    let decoded_keys: Vec<_> = decoded.entries.keys().collect();

    assert_eq!(original_keys, decoded_keys);

    // BTreeMap deve ordenar alfabeticamente
    let expected_order = vec!["alpha", "beta", "charlie", "delta", "zebra"];
    let actual_order: Vec<_> = decoded.entries.keys().map(|s| s.as_str()).collect();

    assert_eq!(
        expected_order, actual_order,
        "BTreeMap should maintain sorted order"
    );

    println!("✅ Order preserved after roundtrip: {:?}", actual_order);
}

#[test]
fn test_size_efficiency() {
    let mut entries = BTreeMap::new();
    for i in 0..100 {
        entries.insert(
            format!("key_{:04}", i),
            MockEntry {
                id: format!("entry_{}", i),
                value: vec![i as u8; 200],
                timestamp: 1704240000 + i as u64,
            },
        );
    }

    let snapshot = MockSnapshot {
        id: "size_test".to_string(),
        entries,
        version: 1,
    };

    // Postcard
    let postcard_bytes = serialize(&snapshot).expect("Postcard serialization failed");

    // JSON (para comparação)
    let json_bytes = serde_json::to_vec(&snapshot).expect("JSON serialization failed");

    let reduction = (1.0 - (postcard_bytes.len() as f64 / json_bytes.len() as f64)) * 100.0;

    println!("📊 Size comparison (100 entries):");
    println!("   Postcard: {} bytes", postcard_bytes.len());
    println!("   JSON:     {} bytes", json_bytes.len());
    println!("   Reduction: {:.1}%", reduction);

    // Postcard deve ser pelo menos 50% menor
    assert!(
        postcard_bytes.len() < json_bytes.len() / 2,
        "Postcard should be at least 50% smaller than JSON"
    );
}
