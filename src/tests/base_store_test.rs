use crate::address::Address;
/// Testes para BaseStore
///
/// Valida o componente CORE do sistema - todos os stores herdam dele.
/// Testa especialmente os bugs críticos que foram corrigidos:
/// 1. Base64 encoding de operações (linha 2021)
/// 2. Serialização/desserialização de Operation
/// 3. Index synchronization
/// 4. Persistência de operações
use crate::address::GuardianDBAddress;
use crate::log::identity::{Identity, Signatures};
use crate::message_marshaler::PostcardMarshaler;
use crate::p2p::EventBus;
use crate::p2p::messaging::one_on_one_channel::new_channel_factory;
use crate::p2p::network::client::IrohClient;
use crate::p2p::network::config::ClientConfig;
use crate::stores::base_store::BaseStore;
use crate::stores::operation::Operation;
use crate::traits::NewStoreOptions;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::TempDir;

// Contador atômico global para garantir endereços únicos
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

// ============= TEST HELPERS =============

/// Helper para criar uma identidade de teste
fn test_identity() -> Arc<Identity> {
    Arc::new(Identity::new(
        "test-basestore-user",
        "test-basestore-pubkey",
        Signatures::new("test-id-sig", "test-pub-sig"),
    ))
}

/// Helper para criar um endereço de teste único
async fn test_address() -> Arc<dyn Address + Send + Sync> {
    use blake3;
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let unique_data = format!("test-basestore-{}-{}", timestamp, counter);
    let hash_bytes: [u8; 32] = blake3::hash(unique_data.as_bytes()).into();
    let hash = iroh_blobs::Hash::from(hash_bytes);

    Arc::new(GuardianDBAddress::new(
        hash,
        format!("test-basestore-{}-{}", timestamp, counter),
    ))
}

/// Helper para criar um BaseStore de teste completo
async fn create_test_base_store() -> Result<(Arc<BaseStore>, TempDir), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let client_config = ClientConfig {
        data_store_path: Some(temp_dir.path().to_path_buf()),
        ..ClientConfig::development()
    };

    let client = Arc::new(IrohClient::new(client_config).await?);
    let identity = test_identity();
    let address = test_address().await;

    // Cria componentes necessários
    let event_bus = EventBus::new();
    let backend = client.backend().clone();
    let pubsub = Arc::new(backend.create_pubsub_interface().await?);
    let message_marshaler = Arc::new(PostcardMarshaler::new());

    // Cria DirectChannel
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
        index: Some(Box::new(
            |_data: &[u8]| -> Box<
                dyn crate::traits::StoreIndex<Error = crate::guardian::error::GuardianError>,
            > { Box::new(crate::stores::base_store::noop_index::NoopIndex) },
        )),
        ..Default::default()
    };

    let base_store = BaseStore::new(client, identity, address, Some(options)).await?;

    Ok((base_store, temp_dir))
}

// ============= TESTES =============

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_base_store_creation() {
        let result = create_test_base_store().await;
        assert!(result.is_ok(), "Should create BaseStore successfully");

        let (store, _temp_dir) = result.unwrap();
        assert_eq!(store.store_type(), "store");

        // Verifica que o log foi criado vazio
        let log_len = store.with_oplog(|log| log.values().len());
        assert_eq!(log_len, 0, "Log should be empty on creation");
    }

    #[tokio::test]
    async fn test_add_operation_single() {
        let (store, _temp_dir) = create_test_base_store()
            .await
            .expect("Failed to create test store");

        // Cria uma operação simples
        let op = Operation::new(
            Some("test_key".to_string()),
            "PUT".to_string(),
            Some(b"test_value".to_vec()),
        );

        // Adiciona a operação
        let result = store.add_operation(op.clone(), None).await;
        assert!(
            result.is_ok(),
            "Should add operation successfully: {:?}",
            result
        );

        // Verifica que a operação foi adicionada ao log
        let log_len = store.with_oplog(|log| log.values().len());
        assert_eq!(log_len, 1, "Log should have 1 entry");
    }

    #[tokio::test]
    async fn test_add_operation_with_binary_data() {
        let (store, _temp_dir) = create_test_base_store()
            .await
            .expect("Failed to create test store");

        // Cria operação com dados binários incluindo bytes nulos
        let binary_data = vec![0x00, 0xFF, 0x42, 0x00, 0xAB, 0xCD, 0xEF, 0x00];
        let op = Operation::new(
            Some("binary_key".to_string()),
            "PUT".to_string(),
            Some(binary_data.clone()),
        );

        // Adiciona a operação
        let result = store.add_operation(op.clone(), None).await;
        assert!(result.is_ok(), "Should add binary operation: {:?}", result);

        // Verifica no log
        let log_len = store.with_oplog(|log| log.values().len());
        assert_eq!(log_len, 1);

        // Recupera e deserializa a operação do log
        let retrieved_op = store.with_oplog(|log| {
            let values = log.values();
            let entry = values.first().expect("Should have entry");
            crate::stores::operation::parse_operation((**entry).clone())
        });

        assert!(
            retrieved_op.is_ok(),
            "Should deserialize operation: {:?}",
            retrieved_op
        );
        let retrieved_op = retrieved_op.unwrap();

        // Valida que os dados binários foram preservados corretamente
        assert_eq!(retrieved_op.key(), op.key());
        assert_eq!(retrieved_op.op(), op.op());
        assert_eq!(retrieved_op.value(), binary_data.as_slice());
    }

    #[tokio::test]
    async fn test_add_multiple_operations() {
        let (store, _temp_dir) = create_test_base_store()
            .await
            .expect("Failed to create test store");

        // Adiciona múltiplas operações
        for i in 0..5 {
            let op = Operation::new(
                Some(format!("key_{}", i)),
                "PUT".to_string(),
                Some(format!("value_{}", i).into_bytes()),
            );

            let result = store.add_operation(op, None).await;
            assert!(result.is_ok(), "Should add operation {}: {:?}", i, result);
        }

        // Verifica que todas as operações foram adicionadas
        let log_len = store.with_oplog(|log| log.values().len());
        assert_eq!(log_len, 5, "Log should have 5 entries");
    }

    #[tokio::test]
    async fn test_operation_serialization_roundtrip() {
        let (store, _temp_dir) = create_test_base_store()
            .await
            .expect("Failed to create test store");

        // Cria operação com todos os campos preenchidos
        let original_op = Operation::new(
            Some("roundtrip_key".to_string()),
            "PUT".to_string(),
            Some(b"roundtrip_value".to_vec()),
        );

        // Adiciona ao log (isso força serialização)
        store
            .add_operation(original_op.clone(), None)
            .await
            .expect("Failed to add operation");

        // Recupera do log (isso força desserialização)
        let deserialized_op = store
            .with_oplog(|log| {
                let values = log.values();
                let entry = values.first().expect("Should have entry");
                crate::stores::operation::parse_operation((**entry).clone())
            })
            .expect("Failed to deserialize");

        // Valida que os dados são idênticos após o roundtrip
        assert_eq!(deserialized_op.key(), original_op.key());
        assert_eq!(deserialized_op.op(), original_op.op());
        assert_eq!(deserialized_op.value(), original_op.value());
    }

    #[tokio::test]
    async fn test_operation_with_none_key() {
        let (store, _temp_dir) = create_test_base_store()
            .await
            .expect("Failed to create test store");

        // Operação sem chave (None)
        let op = Operation::new(
            None,
            "SPECIAL".to_string(),
            Some(b"value_without_key".to_vec()),
        );

        let result = store.add_operation(op.clone(), None).await;
        assert!(result.is_ok(), "Should handle operation with None key");

        // Verifica deserialização
        let deserialized = store
            .with_oplog(|log| {
                let values = log.values();
                let entry = values.first().expect("Should have entry");
                crate::stores::operation::parse_operation((**entry).clone())
            })
            .expect("Should deserialize operation with None key");

        assert_eq!(deserialized.key(), None);
        assert_eq!(deserialized.op(), "SPECIAL");
    }

    #[tokio::test]
    async fn test_operation_with_none_value() {
        let (store, _temp_dir) = create_test_base_store()
            .await
            .expect("Failed to create test store");

        // Operação sem valor (tipicamente DELETE)
        let op = Operation::new(Some("delete_key".to_string()), "DEL".to_string(), None);

        let result = store.add_operation(op.clone(), None).await;
        assert!(result.is_ok(), "Should handle operation with None value");

        // Verifica deserialização
        let deserialized = store
            .with_oplog(|log| {
                let values = log.values();
                let entry = values.first().expect("Should have entry");
                crate::stores::operation::parse_operation((**entry).clone())
            })
            .expect("Should deserialize operation with None value");

        assert_eq!(deserialized.key(), Some(&"delete_key".to_string()));
        assert_eq!(deserialized.op(), "DEL");
        assert!(deserialized.value().is_empty());
    }

    #[tokio::test]
    async fn test_large_operation_payload() {
        let (store, _temp_dir) = create_test_base_store()
            .await
            .expect("Failed to create test store");

        // Cria operação com payload grande (1MB)
        let large_data = vec![0x42u8; 1024 * 1024];
        let op = Operation::new(
            Some("large_key".to_string()),
            "PUT".to_string(),
            Some(large_data.clone()),
        );

        let result = store.add_operation(op, None).await;
        assert!(result.is_ok(), "Should handle large operation payload");

        // Verifica que o payload foi preservado corretamente
        let deserialized = store
            .with_oplog(|log| {
                let values = log.values();
                let entry = values.first().expect("Should have entry");
                crate::stores::operation::parse_operation((**entry).clone())
            })
            .expect("Should deserialize large operation");

        assert_eq!(deserialized.value(), large_data.as_slice());
    }

    #[tokio::test]
    async fn test_utf8_data_in_operation() {
        let (store, _temp_dir) = create_test_base_store()
            .await
            .expect("Failed to create test store");

        // Dados UTF-8 de várias línguas
        let utf8_data = "Hello 世界 مرحبا Привет 🚀🎉".as_bytes().to_vec();
        let op = Operation::new(
            Some("utf8_key".to_string()),
            "PUT".to_string(),
            Some(utf8_data.clone()),
        );

        store
            .add_operation(op, None)
            .await
            .expect("Failed to add UTF-8 operation");

        // Verifica que UTF-8 foi preservado
        let deserialized = store
            .with_oplog(|log| {
                let values = log.values();
                let entry = values.first().expect("Should have entry");
                crate::stores::operation::parse_operation((**entry).clone())
            })
            .expect("Should deserialize UTF-8 operation");

        let retrieved_string =
            String::from_utf8(deserialized.value().to_vec()).expect("Should be valid UTF-8");
        let original_string = String::from_utf8(utf8_data).expect("Should be valid UTF-8");

        assert_eq!(retrieved_string, original_string);
    }

    #[tokio::test]
    async fn test_update_index_empty_log() {
        let (store, _temp_dir) = create_test_base_store()
            .await
            .expect("Failed to create test store");

        // Atualiza índice com log vazio - não deve dar erro
        let result = store.update_index();
        assert!(result.is_ok(), "Should update index on empty log");

        let count = result.unwrap();
        assert_eq!(count, 0, "Should process 0 entries from empty log");
    }

    #[tokio::test]
    async fn test_update_index_after_operations() {
        let (store, _temp_dir) = create_test_base_store()
            .await
            .expect("Failed to create test store");

        // Adiciona operações
        for i in 0..3 {
            let op = Operation::new(
                Some(format!("index_key_{}", i)),
                "PUT".to_string(),
                Some(format!("index_value_{}", i).into_bytes()),
            );
            store
                .add_operation(op, None)
                .await
                .expect("Failed to add operation");
        }

        // Força atualização do índice
        let result = store.update_index();
        assert!(result.is_ok(), "Should update index successfully");

        // Verifica que alguma entrada foi processada
        let count = result.unwrap();
        assert!(count > 0, "Should process entries from log");
    }

    #[tokio::test]
    async fn test_concurrent_add_operations() {
        let (store, _temp_dir) = create_test_base_store()
            .await
            .expect("Failed to create test store");

        // Adiciona operações rapidamente (simula concorrência)
        let mut handles = vec![];
        for i in 0..10 {
            let store_clone = store.clone();
            let handle = tokio::spawn(async move {
                let op = Operation::new(
                    Some(format!("concurrent_{}", i)),
                    "PUT".to_string(),
                    Some(format!("value_{}", i).into_bytes()),
                );
                store_clone.add_operation(op, None).await
            });
            handles.push(handle);
        }

        // Aguarda todas as operações
        for handle in handles {
            let result = handle.await.expect("Task panicked");
            assert!(result.is_ok(), "Concurrent operation should succeed");
        }

        // Verifica que todas foram adicionadas
        let log_len = store.with_oplog(|log| log.values().len());
        assert_eq!(log_len, 10, "Should have all 10 concurrent operations");
    }

    #[tokio::test]
    async fn test_operations_sequence_put_delete() {
        let (store, _temp_dir) = create_test_base_store()
            .await
            .expect("Failed to create test store");

        // Sequência: PUT → DELETE
        let put_op = Operation::new(
            Some("seq_key".to_string()),
            "PUT".to_string(),
            Some(b"seq_value".to_vec()),
        );
        store.add_operation(put_op, None).await.expect("Failed PUT");

        let del_op = Operation::new(Some("seq_key".to_string()), "DEL".to_string(), None);
        store
            .add_operation(del_op, None)
            .await
            .expect("Failed DELETE");

        // Verifica que ambas as operações estão no log
        let log_len = store.with_oplog(|log| log.values().len());
        assert_eq!(log_len, 2, "Should have both PUT and DELETE in log");
    }

    #[tokio::test]
    async fn test_special_characters_in_keys() {
        let (store, _temp_dir) = create_test_base_store()
            .await
            .expect("Failed to create test store");

        let special_keys = vec![
            "key-with-dash",
            "key_with_underscore",
            "key.with.dots",
            "key/with/slashes",
            "key:with:colons",
            "key with spaces",
            "key@#$%",
        ];

        for key in special_keys {
            let op = Operation::new(
                Some(key.to_string()),
                "PUT".to_string(),
                Some(format!("value for {}", key).into_bytes()),
            );

            let result = store.add_operation(op, None).await;
            assert!(
                result.is_ok(),
                "Should handle special characters in key: {}",
                key
            );
        }

        let log_len = store.with_oplog(|log| log.values().len());
        assert_eq!(log_len, 7, "Should have all operations with special keys");
    }

    #[tokio::test]
    async fn test_base_store_address() {
        let (store, _temp_dir) = create_test_base_store()
            .await
            .expect("Failed to create test store");

        let address = store.address();
        assert!(
            !address.to_string().is_empty(),
            "Address should not be empty"
        );
    }

    #[tokio::test]
    async fn test_base_store_store_type() {
        let (store, _temp_dir) = create_test_base_store()
            .await
            .expect("Failed to create test store");

        assert_eq!(store.store_type(), "store");
    }

    #[tokio::test]
    async fn test_operation_with_empty_value() {
        let (store, _temp_dir) = create_test_base_store()
            .await
            .expect("Failed to create test store");

        // Operação com Vec vazio (diferente de None)
        let op = Operation::new(
            Some("empty_value_key".to_string()),
            "PUT".to_string(),
            Some(Vec::new()),
        );

        let result = store.add_operation(op, None).await;
        assert!(result.is_ok(), "Should handle empty Vec value");

        // Verifica deserialização
        let deserialized = store
            .with_oplog(|log| {
                let values = log.values();
                let entry = values.first().expect("Should have entry");
                crate::stores::operation::parse_operation((**entry).clone())
            })
            .expect("Should deserialize");

        assert!(deserialized.value().is_empty());
    }

    #[tokio::test]
    async fn test_multiple_stores_independent() {
        // Cria dois stores independentes
        let (store1, _temp1) = create_test_base_store()
            .await
            .expect("Failed to create store1");
        let (store2, _temp2) = create_test_base_store()
            .await
            .expect("Failed to create store2");

        // Adiciona operação em store1
        let op1 = Operation::new(
            Some("store1_key".to_string()),
            "PUT".to_string(),
            Some(b"store1_value".to_vec()),
        );
        store1
            .add_operation(op1, None)
            .await
            .expect("Failed on store1");

        // Adiciona operação em store2
        let op2 = Operation::new(
            Some("store2_key".to_string()),
            "PUT".to_string(),
            Some(b"store2_value".to_vec()),
        );
        store2
            .add_operation(op2, None)
            .await
            .expect("Failed on store2");

        // Verifica que os stores são independentes
        let log1_len = store1.with_oplog(|log| log.values().len());
        let log2_len = store2.with_oplog(|log| log.values().len());

        assert_eq!(log1_len, 1, "Store1 should have 1 entry");
        assert_eq!(log2_len, 1, "Store2 should have 1 entry");
    }
}
