use crate::address::Address;
/// Testes para KeyValueStore
///
/// Valida todas as operações principais do KV store usando IrohClient
use crate::address::GuardianDBAddress;
use crate::log::identity::{Identity, Signatures};
use crate::message_marshaler::PostcardMarshaler;
use crate::p2p::EventBus;
use crate::p2p::messaging::one_on_one_channel::new_channel_factory;
use crate::p2p::network::client::IrohClient;
use crate::p2p::network::config::ClientConfig;
use crate::stores::kv_store::GuardianDBKeyValue;
use crate::traits::NewStoreOptions;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::TempDir;

// Contador atômico global para garantir endereços únicos em testes paralelos
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

// ============= TEST HELPERS =============

/// Helper para criar uma identidade de teste
fn test_identity() -> Arc<Identity> {
    Arc::new(Identity::new(
        "test-user",
        "test-public-key",
        Signatures::new("test-id-sig", "test-pub-sig"),
    ))
}

/// Helper para criar um endereço de teste único para cada chamada
async fn test_address() -> Arc<dyn Address + Send + Sync> {
    use blake3;
    use std::time::{SystemTime, UNIX_EPOCH};

    // Cria um Hash único baseado no timestamp + contador atômico
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let unique_data = format!("test-kvstore-{}-{}", timestamp, counter);
    let hash_bytes: [u8; 32] = blake3::hash(unique_data.as_bytes()).into();
    let hash = iroh_blobs::Hash::from(hash_bytes);

    Arc::new(GuardianDBAddress::new(
        hash,
        format!("test-kvstore-{}-{}", timestamp, counter),
    ))
}

/// Helper para criar um KeyValueStore de teste completo
async fn create_test_store() -> Result<(GuardianDBKeyValue, TempDir), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let client_config = ClientConfig {
        data_store_path: Some(temp_dir.path().to_path_buf()),
        ..ClientConfig::development()
    };

    let client = Arc::new(IrohClient::new(client_config).await?);
    let identity = test_identity();
    let address = test_address().await;

    // Cria componentes necessários para o store
    let event_bus = EventBus::new();
    let backend = client.backend().clone();
    let pubsub = Arc::new(backend.create_pubsub_interface().await?);
    let message_marshaler = Arc::new(PostcardMarshaler::new());

    // Cria DirectChannel usando o factory
    let channel_factory = new_channel_factory(client.clone()).await?;
    let payload_emitter = crate::p2p::PayloadEmitter::new(&event_bus).await?;
    let direct_channel = channel_factory(Arc::new(payload_emitter), None)
        .await
        .map_err(|e| format!("Failed to create DirectChannel: {}", e))?;

    let options = NewStoreOptions {
        event_bus: Some(event_bus),
        pubsub: Some(pubsub),
        message_marshaler: Some(message_marshaler),
        direct_channel: Some(direct_channel),
        directory: temp_dir.path().join("cache").to_string_lossy().to_string(),
        ..Default::default()
    };

    let store = GuardianDBKeyValue::new(client, identity, address, Some(options)).await?;

    Ok((store, temp_dir))
}

// ============= TESTES =============

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_kv_store_creation() {
        let result = create_test_store().await;
        assert!(result.is_ok(), "Should create KeyValueStore successfully");

        let (store, _temp_dir) = result.unwrap();
        assert_eq!(store.get_type(), "keyvalue");
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[tokio::test]
    async fn test_put_and_get() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        // Put a value
        let key = "test_key";
        let value = b"test_value".to_vec();
        let result = store.put(key.to_string(), value.clone()).await;
        assert!(
            result.is_ok(),
            "Should put value successfully: {:?}",
            result
        );

        // Get the value
        let retrieved = store.get(key).expect("Failed to get value");
        assert!(retrieved.is_some(), "Should find the key");
        assert_eq!(retrieved.unwrap(), value);
    }

    #[tokio::test]
    async fn test_put_updates_existing_key() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        let key = "update_key";

        // Put initial value
        store
            .put(key.to_string(), b"initial".to_vec())
            .await
            .expect("Failed to put initial value");

        // Update with new value
        store
            .put(key.to_string(), b"updated".to_vec())
            .await
            .expect("Failed to update value");

        // Verify updated value
        let retrieved = store.get(key).expect("Failed to get value");
        assert_eq!(retrieved.unwrap(), b"updated");
    }

    #[tokio::test]
    async fn test_delete() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        let key = "delete_key";
        let value = b"delete_value".to_vec();

        // Put a value
        store
            .put(key.to_string(), value)
            .await
            .expect("Failed to put value");

        // Verify it exists
        assert!(store.contains_key(key));

        // Delete it
        let result = store.delete(key.to_string()).await;
        assert!(result.is_ok(), "Should delete successfully: {:?}", result);

        // Verify it's gone
        assert!(!store.contains_key(key));
        let retrieved = store.get(key).expect("Failed to get value");
        assert!(retrieved.is_none(), "Key should not exist after deletion");
    }

    #[tokio::test]
    async fn test_delete_nonexistent_key() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        let result = store.delete("nonexistent".to_string()).await;
        assert!(
            result.is_err(),
            "Should error when deleting nonexistent key"
        );
    }

    #[tokio::test]
    async fn test_empty_key_validation() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        // Test PUT with empty key
        let result = store.put("".to_string(), b"value".to_vec()).await;
        assert!(result.is_err(), "Should error on empty key for PUT");

        // Test GET with empty key
        let result = store.get("");
        assert!(result.is_err(), "Should error on empty key for GET");

        // Test DELETE with empty key
        let result = store.delete("".to_string()).await;
        assert!(result.is_err(), "Should error on empty key for DELETE");
    }

    #[tokio::test]
    async fn test_empty_value_validation() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        let result = store.put("key".to_string(), Vec::new()).await;
        assert!(result.is_err(), "Should error on empty value");
    }

    #[tokio::test]
    async fn test_contains_key() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        let key = "contains_key";

        // Should not exist initially
        assert!(!store.contains_key(key));

        // Put a value
        store
            .put(key.to_string(), b"value".to_vec())
            .await
            .expect("Failed to put value");

        // Should exist now
        assert!(store.contains_key(key));
    }

    #[tokio::test]
    async fn test_keys() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        // Initially empty
        assert!(store.keys().is_empty());

        // Add some keys
        store
            .put("key1".to_string(), b"value1".to_vec())
            .await
            .expect("Failed to put key1");
        store
            .put("key2".to_string(), b"value2".to_vec())
            .await
            .expect("Failed to put key2");
        store
            .put("key3".to_string(), b"value3".to_vec())
            .await
            .expect("Failed to put key3");

        // Verify keys
        let keys = store.keys();
        assert_eq!(keys.len(), 3);
        assert!(keys.contains(&"key1".to_string()));
        assert!(keys.contains(&"key2".to_string()));
        assert!(keys.contains(&"key3".to_string()));
    }

    #[tokio::test]
    async fn test_all() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        // Add multiple key-value pairs
        store
            .put("key1".to_string(), b"value1".to_vec())
            .await
            .expect("Failed to put key1");
        store
            .put("key2".to_string(), b"value2".to_vec())
            .await
            .expect("Failed to put key2");
        store
            .put("key3".to_string(), b"value3".to_vec())
            .await
            .expect("Failed to put key3");

        // Get all pairs
        let all = store.all();
        assert_eq!(all.len(), 3);
        assert_eq!(all.get("key1").unwrap(), b"value1");
        assert_eq!(all.get("key2").unwrap(), b"value2");
        assert_eq!(all.get("key3").unwrap(), b"value3");
    }

    #[tokio::test]
    async fn test_len_and_is_empty() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        // Initially empty
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);

        // Add one item
        store
            .put("key1".to_string(), b"value1".to_vec())
            .await
            .expect("Failed to put key1");
        assert!(!store.is_empty());
        assert_eq!(store.len(), 1);

        // Add more items
        store
            .put("key2".to_string(), b"value2".to_vec())
            .await
            .expect("Failed to put key2");
        store
            .put("key3".to_string(), b"value3".to_vec())
            .await
            .expect("Failed to put key3");
        assert_eq!(store.len(), 3);

        // Delete one
        store
            .delete("key2".to_string())
            .await
            .expect("Failed to delete key2");
        assert_eq!(store.len(), 2);
    }

    #[tokio::test]
    async fn test_multiple_operations_sequence() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        // PUT
        store
            .put("seq_key".to_string(), b"value1".to_vec())
            .await
            .expect("Failed to put");
        assert_eq!(store.get("seq_key").unwrap().unwrap(), b"value1");

        // UPDATE
        store
            .put("seq_key".to_string(), b"value2".to_vec())
            .await
            .expect("Failed to update");
        assert_eq!(store.get("seq_key").unwrap().unwrap(), b"value2");

        // DELETE
        store
            .delete("seq_key".to_string())
            .await
            .expect("Failed to delete");
        assert!(store.get("seq_key").unwrap().is_none());

        // PUT AGAIN
        store
            .put("seq_key".to_string(), b"value3".to_vec())
            .await
            .expect("Failed to put again");
        assert_eq!(store.get("seq_key").unwrap().unwrap(), b"value3");
    }

    #[tokio::test]
    async fn test_binary_values() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        // Test with binary data including null bytes
        let binary_data = vec![0x00, 0xFF, 0x42, 0x00, 0xAB, 0xCD, 0xEF];

        store
            .put("binary_key".to_string(), binary_data.clone())
            .await
            .expect("Failed to put binary data");

        let retrieved = store.get("binary_key").expect("Failed to get binary data");
        assert_eq!(retrieved.unwrap(), binary_data);
    }

    #[tokio::test]
    async fn test_large_value() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        // Test with a large value (1MB)
        let large_value = vec![0x42u8; 1024 * 1024];

        store
            .put("large_key".to_string(), large_value.clone())
            .await
            .expect("Failed to put large value");

        let retrieved = store.get("large_key").expect("Failed to get large value");
        assert_eq!(retrieved.unwrap(), large_value);
    }

    #[tokio::test]
    async fn test_special_characters_in_keys() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        let special_keys = vec![
            "key-with-dash",
            "key_with_underscore",
            "key.with.dots",
            "key/with/slashes",
            "key:with:colons",
            "key with spaces",
            "key@with#special$chars",
        ];

        for key in &special_keys {
            let value = format!("value for {}", key).into_bytes();
            store
                .put((*key).to_string(), value.clone())
                .await
                .unwrap_or_else(|_| panic!("Failed to put key: {}", key));

            let retrieved = store
                .get(key)
                .unwrap_or_else(|_| panic!("Failed to get key: {}", key));
            assert_eq!(retrieved.unwrap(), value);
        }
    }

    #[tokio::test]
    async fn test_integration_workflow() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        // Scenario: User session management

        // 1. Create multiple sessions
        store
            .put("session:user1".to_string(), b"token123".to_vec())
            .await
            .expect("Failed to create session1");
        store
            .put("session:user2".to_string(), b"token456".to_vec())
            .await
            .expect("Failed to create session2");
        store
            .put("session:user3".to_string(), b"token789".to_vec())
            .await
            .expect("Failed to create session3");

        // 2. Verify all sessions exist
        assert_eq!(store.len(), 3);
        let all_sessions = store.all();
        assert_eq!(all_sessions.len(), 3);

        // 3. Update one session
        store
            .put("session:user2".to_string(), b"new_token456".to_vec())
            .await
            .expect("Failed to update session");
        assert_eq!(
            store.get("session:user2").unwrap().unwrap(),
            b"new_token456"
        );

        // 4. Delete expired session
        store
            .delete("session:user1".to_string())
            .await
            .expect("Failed to delete session");
        assert_eq!(store.len(), 2);

        // 5. Verify remaining sessions
        let remaining_keys = store.keys();
        assert_eq!(remaining_keys.len(), 2);
        assert!(remaining_keys.contains(&"session:user2".to_string()));
        assert!(remaining_keys.contains(&"session:user3".to_string()));
        assert!(!remaining_keys.contains(&"session:user1".to_string()));
    }

    #[tokio::test]
    async fn test_concurrent_operations() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        // Add multiple items rapidly
        for i in 0..10 {
            let key = format!("concurrent_key_{}", i);
            let value = format!("value_{}", i).into_bytes();
            store
                .put(key, value)
                .await
                .unwrap_or_else(|_| panic!("Failed to put key: {}", i));
        }

        // Verify all items were added
        assert_eq!(store.len(), 10);

        // Verify all values are correct
        for i in 0..10 {
            let key = format!("concurrent_key_{}", i);
            let expected_value = format!("value_{}", i).into_bytes();
            let retrieved = store
                .get(&key)
                .unwrap_or_else(|_| panic!("Failed to get key: {}", key));
            assert_eq!(retrieved.unwrap(), expected_value);
        }
    }

    #[tokio::test]
    async fn test_utf8_values() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        // Test with UTF-8 strings
        let utf8_values = [
            ("english", "Hello World"),
            ("portuguese", "Olá Mundo"),
            ("chinese", "你好世界"),
            ("arabic", "مرحبا بالعالم"),
            ("emoji", "🚀🎉✨"),
        ];

        for (key, value) in utf8_values {
            store
                .put(key.to_string(), value.as_bytes().to_vec())
                .await
                .unwrap_or_else(|_| panic!("Failed to put {}", key));

            let retrieved = store
                .get(key)
                .unwrap_or_else(|_| panic!("Failed to get {}", key));
            let retrieved_str = String::from_utf8(retrieved.unwrap()).expect("Invalid UTF-8");
            assert_eq!(retrieved_str, value);
        }
    }
}
