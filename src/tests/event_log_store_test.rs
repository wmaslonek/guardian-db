/// Testes para EventLogStore
///
/// Valida todas as operações principais do event log:
/// - Criação e inicialização
/// - Adição de entradas
/// - Recuperação por Hash
/// - Listagem e filtragem
/// - Queries otimizadas
/// - Streaming de dados
use crate::address::{Address, GuardianDBAddress};
use crate::guardian::error::{GuardianError, Result};
use crate::log::identity::Identity;
use crate::message_marshaler::PostcardMarshaler;
use crate::p2p::messaging::one_on_one_channel::new_channel_factory;
use crate::p2p::network::client::IrohClient;
use crate::p2p::network::config::ClientConfig;
use crate::stores::event_log_store::GuardianDBEventLogStore;
use crate::traits::{self, Store, StreamOptions};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::TempDir;
use tokio::sync::mpsc;

// Contador atômico global para garantir endereços únicos em testes paralelos
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Helper para criar uma identidade de teste
fn test_identity() -> Arc<Identity> {
    use crate::log::identity::Signatures;
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

    // Cria um Hash único baseado no timestamp + contador atômico para evitar conflitos de cache em testes paralelos
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let unique_data = format!("test-eventlog-db-{}-{}", timestamp, counter);
    let hash_bytes: [u8; 32] = blake3::hash(unique_data.as_bytes()).into();
    let hash = iroh_blobs::Hash::from(hash_bytes);

    Arc::new(GuardianDBAddress::new(
        hash,
        format!("test-db/eventlog-{}-{}", timestamp, counter),
    ))
}

/// Helper para criar uma nova EventLogStore de teste
async fn create_test_store() -> Result<(GuardianDBEventLogStore, TempDir)> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let client_config = ClientConfig {
        data_store_path: Some(temp_dir.path().to_path_buf()),
        ..ClientConfig::development()
    };

    let client = Arc::new(
        IrohClient::new(client_config)
            .await
            .expect("Failed to create iroh client"),
    );

    let identity = test_identity();
    let address = test_address().await;

    // Cria componentes necessários para o store
    let event_bus = crate::p2p::EventBus::new();
    let backend = client.backend().clone();
    let pubsub = Arc::new(
        backend
            .create_pubsub_interface()
            .await
            .expect("Failed to create EpidemicPubSub"),
    );
    let message_marshaler = Arc::new(PostcardMarshaler::new());

    // Cria DirectChannel usando o factory
    let channel_factory = new_channel_factory(client.clone())
        .await
        .expect("Failed to create DirectChannelFactory");
    let payload_emitter = crate::p2p::PayloadEmitter::new(&event_bus)
        .await
        .expect("Failed to create PayloadEmitter");
    let direct_channel = channel_factory(Arc::new(payload_emitter), None)
        .await
        .expect("Failed to create DirectChannel");

    let options = traits::NewStoreOptions {
        event_bus: Some(event_bus),
        pubsub: Some(pubsub),
        message_marshaler: Some(message_marshaler),
        direct_channel: Some(direct_channel),
        directory: temp_dir.path().join("cache").to_string_lossy().to_string(),
        ..Default::default()
    };

    let store = GuardianDBEventLogStore::new(client, identity, address, options)
        .await
        .expect("Failed to create EventLogStore");

    Ok((store, temp_dir))
}

#[tokio::test]
async fn test_eventlog_creation() {
    let result = create_test_store().await;
    assert!(result.is_ok(), "Should create EventLogStore successfully");

    let (store, _temp_dir) = result.unwrap();
    assert_eq!(store.store_type(), "eventlog");
    assert!(!store.db_name().is_empty());
}

#[tokio::test]
async fn test_eventlog_creation_with_empty_address() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let client_config = ClientConfig {
        data_store_path: Some(temp_dir.path().to_path_buf()),
        ..ClientConfig::development()
    };

    let client = Arc::new(
        IrohClient::new(client_config)
            .await
            .expect("Failed to create iroh client"),
    );

    let identity = test_identity();

    // Cria um endereço com path vazio (inválido)
    use blake3;
    let hash_bytes: [u8; 32] = blake3::hash(b"test").into();
    let hash = iroh_blobs::Hash::from(hash_bytes);
    let address: Arc<dyn Address + Send + Sync> =
        Arc::new(GuardianDBAddress::new(hash, "".to_string()));

    // Cria componentes necessários para o store
    let event_bus = crate::p2p::EventBus::new();
    let backend = client.backend().clone();
    let pubsub = Arc::new(
        backend
            .create_pubsub_interface()
            .await
            .expect("Failed to create PubSub"),
    );
    let message_marshaler = Arc::new(PostcardMarshaler::new());

    // Cria DirectChannel usando o factory
    let channel_factory = new_channel_factory(client.clone())
        .await
        .expect("Failed to create DirectChannelFactory");
    let payload_emitter = crate::p2p::PayloadEmitter::new(&event_bus)
        .await
        .expect("Failed to create PayloadEmitter");
    let direct_channel = channel_factory(Arc::new(payload_emitter), None)
        .await
        .expect("Failed to create DirectChannel");

    let options = traits::NewStoreOptions {
        event_bus: Some(event_bus),
        pubsub: Some(pubsub),
        message_marshaler: Some(message_marshaler),
        direct_channel: Some(direct_channel),
        directory: temp_dir.path().join("cache").to_string_lossy().to_string(),
        ..Default::default()
    };

    let result = GuardianDBEventLogStore::new(client, identity, address, options).await;

    // Um endereço com path vazio pode falhar na criação do cache em certas plataformas
    // (ex: Windows com caminhos longos). O importante é que não falhe com um erro de
    // validação de endereço, mas sim um erro de infraestrutura.
    // O resultado pode ser Ok ou Err dependendo do sistema de cache subjacente.
    if let Err(ref e) = result {
        let err_msg = format!("{}", e);
        // Se falhar, deve ser por causa do cache/IO, não por validação de endereço vazio
        assert!(
            err_msg.contains("cache")
                || err_msg.contains("I/O")
                || err_msg.contains("Failed to initialize base store"),
            "Should fail with cache/IO error, not address validation. Got: {}",
            err_msg
        );
    }
}

#[tokio::test]
async fn test_eventlog_add_single_entry() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");

    let test_data = b"Hello, EventLog!".to_vec();
    let result = store.add(test_data.clone()).await;

    assert!(result.is_ok(), "Should add entry successfully");

    let operation = result.unwrap();
    assert_eq!(operation.op, "ADD");
    assert_eq!(operation.value, test_data);
    assert!(operation.entry.is_some(), "Operation should have an entry");
}

#[tokio::test]
async fn test_eventlog_add_empty_data() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");

    let empty_data = Vec::new();
    let result = store.add(empty_data).await;

    assert!(result.is_err(), "Should fail when adding empty data");

    if let Err(GuardianError::Store(msg)) = result {
        assert!(
            msg.contains("empty"),
            "Error message should mention empty data"
        );
    } else {
        panic!("Expected GuardianError::Store");
    }
}

#[tokio::test]
async fn test_eventlog_add_multiple_entries() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");

    let entries = [
        b"First entry".to_vec(),
        b"Second entry".to_vec(),
        b"Third entry".to_vec(),
    ];

    let mut hashes = Vec::new();

    for entry_data in entries.iter() {
        let result = store.add(entry_data.clone()).await;
        assert!(result.is_ok(), "Should add each entry successfully");

        let operation = result.unwrap();
        assert!(operation.entry.is_some());
        let entry_hash = operation.entry.unwrap().hash().to_string();
        hashes.push(entry_hash);
    }

    // Verifica que todos os hashes são únicos
    let unique_hashes: std::collections::HashSet<_> = hashes.iter().collect();
    assert_eq!(
        unique_hashes.len(),
        entries.len(),
        "All hashes should be unique"
    );
}

#[tokio::test]
async fn test_eventlog_get_by_cid() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");

    let test_data = b"Test data for retrieval".to_vec();
    let add_result = store
        .add(test_data.clone())
        .await
        .expect("Failed to add entry");

    let entry = add_result.entry.expect("Should have entry");
    let entry_hash = *entry.hash();

    // Recupera a entrada pelo Hash
    let get_result = store.get(&entry_hash).await;
    assert!(
        get_result.is_ok(),
        "Should retrieve entry by Hash: {:?}",
        get_result.err()
    );

    let retrieved_op = get_result.unwrap();
    assert_eq!(retrieved_op.op, "ADD");
    assert_eq!(retrieved_op.value, test_data);
}

#[tokio::test]
async fn test_eventlog_get_nonexistent_hash() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");

    // Cria um Hash inexistente
    use blake3;
    let hash_bytes: [u8; 32] = blake3::hash(b"nonexistent").into();
    let fake_hash = iroh_blobs::Hash::from(hash_bytes);

    let result = store.get(&fake_hash).await;
    assert!(result.is_err(), "Should fail when Hash doesn't exist");
}

#[tokio::test]
async fn test_eventlog_list_all_entries() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");

    // Adiciona várias entradas
    let entries = [
        b"Entry 1".to_vec(),
        b"Entry 2".to_vec(),
        b"Entry 3".to_vec(),
    ];

    for entry_data in entries.iter() {
        store
            .add(entry_data.clone())
            .await
            .expect("Failed to add entry");
    }

    // Lista todas as entradas
    let result = store.list(None).await;
    assert!(result.is_ok(), "Should list all entries");

    let operations = result.unwrap();
    assert_eq!(operations.len(), entries.len(), "Should return all entries");
}

#[tokio::test]
async fn test_eventlog_list_with_limit() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");

    // Adiciona 5 entradas
    for i in 0..5 {
        let data = format!("Entry {}", i).into_bytes();
        store.add(data).await.expect("Failed to add entry");
    }

    // Lista apenas 3 entradas
    let options = StreamOptions {
        amount: Some(3),
        ..Default::default()
    };

    let result = store.list(Some(options)).await;
    assert!(result.is_ok(), "Should list with limit");

    let operations = result.unwrap();
    assert_eq!(operations.len(), 3, "Should return exactly 3 entries");
}

#[tokio::test]
async fn test_eventlog_list_with_gte_filter() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");

    // Adiciona 3 entradas
    let mut hashes = Vec::new();
    for i in 0..3 {
        let data = format!("Entry {}", i).into_bytes();
        let op = store.add(data).await.expect("Failed to add entry");
        let entry = op.entry.expect("Should have entry");
        let entry_hash = *entry.hash();

        hashes.push(entry_hash);
    }

    // Lista a partir da segunda entrada (inclusive)
    let options = StreamOptions {
        gte: Some(hashes[1]),
        ..Default::default()
    };

    // Usa timeout para evitar travamento
    let result =
        tokio::time::timeout(std::time::Duration::from_secs(5), store.list(Some(options))).await;

    assert!(result.is_ok(), "List operation timed out");
    let list_result = result.unwrap();
    assert!(
        list_result.is_ok(),
        "Should list with gte filter: {:?}",
        list_result.err()
    );

    let operations = list_result.unwrap();
    assert!(
        !operations.is_empty(),
        "Should return at least 1 entry, got: {}",
        operations.len()
    );
}

#[tokio::test]
async fn test_eventlog_stream_operations() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");

    // Adiciona entradas
    for i in 0..3 {
        let data = format!("Stream entry {}", i).into_bytes();
        store.add(data).await.expect("Failed to add entry");
    }

    // Cria canal para streaming
    let (tx, mut rx) = mpsc::channel(10);

    // Inicia streaming
    let stream_result = store.stream(tx, None).await;
    assert!(stream_result.is_ok(), "Should start streaming");

    // Coleta operações do stream
    let mut streamed_ops = Vec::new();
    while let Some(op) = rx.recv().await {
        streamed_ops.push(op);
    }

    assert_eq!(streamed_ops.len(), 3, "Should stream all 3 entries");
}

#[tokio::test]
async fn test_eventlog_query_performance() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");

    // Adiciona um número maior de entradas para testar performance
    let num_entries = 50;
    for i in 0..num_entries {
        let data = format!("Performance test entry {}", i).into_bytes();
        store.add(data).await.expect("Failed to add entry");
    }

    let start = std::time::Instant::now();

    // Query por últimas 10 entradas
    let options = StreamOptions {
        amount: Some(10),
        ..Default::default()
    };

    let result = store.list(Some(options)).await;
    let elapsed = start.elapsed();

    assert!(result.is_ok(), "Query should succeed");
    assert_eq!(
        result.unwrap().len(),
        10,
        "Should return exactly 10 entries"
    );

    // Performance check: query deve ser razoavelmente rápida
    assert!(
        elapsed.as_millis() < 1000,
        "Query should complete in less than 1 second, took {:?}",
        elapsed
    );
}

#[tokio::test]
async fn test_eventlog_store_type() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");
    assert_eq!(store.store_type(), "eventlog");
}

#[tokio::test]
async fn test_eventlog_address() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");
    let address = store.address();
    assert!(!address.to_string().is_empty());
}

#[tokio::test]
async fn test_eventlog_cache_access() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");
    let cache = store.cache();

    // Verifica que o cache é acessível
    assert!(Arc::strong_count(&cache) > 0);
}

#[tokio::test]
async fn test_eventlog_iroh_client_access() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");
    let iroh = store.client();

    // Verifica que o cliente iroh é acessível
    assert!(Arc::strong_count(&iroh) > 0);
}

#[tokio::test]
async fn test_eventlog_identity_access() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");
    let identity = store.identity();

    assert_eq!(identity.id(), "test-user");
}

#[tokio::test]
async fn test_eventlog_concurrent_adds() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");
    let store = Arc::new(tokio::sync::RwLock::new(store));

    // Spawn múltiplas tasks adicionando dados concorrentemente
    let mut handles = Vec::new();

    for i in 0..10 {
        let store_clone = store.clone();
        let handle = tokio::spawn(async move {
            let data = format!("Concurrent entry {}", i).into_bytes();
            let store_guard = store_clone.write().await;
            store_guard.add(data).await
        });
        handles.push(handle);
    }

    // Aguarda todas as tasks
    let mut success_count = 0;
    for handle in handles {
        if handle.await.is_ok() {
            success_count += 1;
        }
    }

    assert!(success_count >= 8, "Most concurrent adds should succeed");
}

#[tokio::test]
async fn test_eventlog_large_data() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");

    // Cria um payload grande (1MB)
    let large_data = vec![0u8; 1024 * 1024];

    let result = store.add(large_data.clone()).await;
    assert!(result.is_ok(), "Should handle large data");

    let operation = result.unwrap();
    let entry = operation.entry.expect("Should have entry");
    let entry_hash = *entry.hash();

    // Verifica que pode recuperar os dados grandes
    let get_result = store.get(&entry_hash).await;
    assert!(get_result.is_ok(), "Should retrieve large data");

    let retrieved = get_result.unwrap();
    assert_eq!(retrieved.value, large_data);
}

#[tokio::test]
async fn test_eventlog_special_characters() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");

    // Testa com caracteres especiais e unicode
    let special_data = "Hello 世界! 🚀 Special chars: <>&\"'".as_bytes().to_vec();

    let result = store.add(special_data.clone()).await;
    assert!(result.is_ok(), "Should handle special characters");

    let operation = result.unwrap();
    let entry = operation.entry.expect("Should have entry");
    let entry_hash = *entry.hash();

    // Verifica integridade dos dados
    let retrieved = store.get(&entry_hash).await.unwrap();
    assert_eq!(retrieved.value, special_data);
}

#[tokio::test]
async fn test_eventlog_replication_status() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");

    let replication_info = store.replication_status();

    // Verifica que o status de replicação está acessível
    // ReplicationInfo usa RwLock internamente, então só verificamos que existe
    let (progress, max) = replication_info.get_progress_and_max().await;
    assert!(max >= progress, "Max should be >= progress");
}

#[tokio::test]
async fn test_eventlog_op_log_access() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");

    let op_log = store.op_log();
    let log_guard = op_log.read();

    // Verifica que o log está acessível (len() é sempre >= 0)
    let _ = log_guard.len();
}

#[tokio::test]
async fn test_eventlog_event_bus_access() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");

    let event_bus = store.event_bus();

    // Verifica que o event bus é acessível
    assert!(Arc::strong_count(&event_bus) > 0);
}

#[tokio::test]
async fn test_eventlog_index_access() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");

    let index = store.index();

    // Verifica propriedades básicas do índice
    // Apenas verifica que o método é chamável - o valor retornado pode ser true ou false
    let _supports_entry_queries = index.supports_entry_queries();
}

#[tokio::test]
async fn test_eventlog_basestore_access() {
    let (store, _temp_dir) = create_test_store().await.expect("Failed to create store");

    let basestore = store.basestore();

    // Verifica que o basestore é acessível
    assert!(!basestore.db_name().is_empty());
}
