/// Testes de integração do ciclo de vida completo do GuardianDB
///
/// Estes testes validam:
/// - Inicialização e configuração do GuardianDB
/// - Criação de diferentes tipos de stores
/// - Operações CRUD completas
/// - Cleanup e destruição apropriada de recursos
mod common;

use common::{TestNode, init_test_logging};
use guardian_db::traits::CreateDBOptions;
use serde_json::json;

#[tokio::test]
async fn test_guardiandb_initialization() {
    init_test_logging();

    let node = TestNode::new("init_test")
        .await
        .expect("Failed to create test node");

    // Verifica que o diretório foi criado
    assert!(node.path().exists());

    tracing::info!("✓ GuardianDB initialized successfully");
}

#[tokio::test]
async fn test_eventlog_lifecycle() {
    init_test_logging();

    let node = TestNode::new("eventlog_test")
        .await
        .expect("Failed to create test node");

    // Criar EventLog store
    let log = node
        .db
        .log("test-events", None)
        .await
        .expect("Failed to create log");

    // Adicionar entries
    let entries_data = [
        b"First event".to_vec(),
        b"Second event".to_vec(),
        b"Third event".to_vec(),
    ];

    for (i, data) in entries_data.iter().enumerate() {
        log.add(data.clone())
            .await
            .unwrap_or_else(|_| panic!("Failed to add entry {}", i));
        tracing::info!("Added entry {}", i);
    }

    // Verificar que todas as entries foram adicionadas
    let all_entries_ops = log.list(None).await.expect("Failed to get all entries");
    assert_eq!(all_entries_ops.len(), 3, "Expected 3 entries in log");

    // Verificar conteúdo das entries
    for (i, operation) in all_entries_ops.iter().enumerate() {
        assert_eq!(operation.value(), &entries_data[i]);
    }

    // list já retorna Vec<Operation>, não há iterator separado
    assert_eq!(all_entries_ops.len(), 3, "Should have 3 entries");

    tracing::info!("✓ EventLog lifecycle completed successfully");
}

#[tokio::test]
async fn test_keyvalue_lifecycle() {
    init_test_logging();

    let node = TestNode::new("kv_test")
        .await
        .expect("Failed to create test node");

    // Criar KeyValue store
    let kv = node
        .db
        .key_value("test-kv", None)
        .await
        .expect("Failed to create KV store");

    // Testar operação PUT
    kv.put("name", b"GuardianDB".to_vec())
        .await
        .expect("Failed to put name");
    kv.put("version", b"0.11.18".to_vec())
        .await
        .expect("Failed to put version");
    kv.put("language", b"Rust".to_vec())
        .await
        .expect("Failed to put language");

    tracing::info!("✓ PUT operations completed");

    // Testar operação GET
    let name = kv.get("name").await.expect("Failed to get name");
    assert!(name.is_some());
    assert_eq!(name.unwrap(), b"GuardianDB");

    let version = kv.get("version").await.expect("Failed to get version");
    assert!(version.is_some());
    assert_eq!(version.unwrap(), b"0.11.18");

    tracing::info!("✓ GET operations completed");

    // Testar operação ALL (listar todas as keys)
    let all_data = kv.all();
    assert_eq!(all_data.len(), 3, "Expected 3 keys");
    assert!(all_data.contains_key("name"));
    assert!(all_data.contains_key("version"));
    assert!(all_data.contains_key("language"));

    tracing::info!("✓ ALL operation completed");

    // Testar operação DEL
    kv.delete("version")
        .await
        .expect("Failed to delete version");

    let deleted = kv.get("version").await.expect("Failed to get deleted key");
    assert!(deleted.is_none(), "Deleted key should return None");

    let remaining_data = kv.all();
    assert_eq!(remaining_data.len(), 2, "Expected 2 keys after deletion");

    tracing::info!("✓ DEL operation completed");

    // Testar sobrescrever valor existente
    kv.put("name", b"GuardianDB v2".to_vec())
        .await
        .expect("Failed to update name");
    let updated = kv.get("name").await.expect("Failed to get updated name");
    assert_eq!(updated.unwrap(), b"GuardianDB v2");

    tracing::info!("✓ KeyValue lifecycle completed successfully");
}

#[tokio::test]
async fn test_document_store_lifecycle() {
    init_test_logging();

    let node = TestNode::new("docs_test")
        .await
        .expect("Failed to create test node");

    // Criar Document store
    let docs = node
        .db
        .docs("test-documents", None)
        .await
        .expect("Failed to create document store");

    // Criar documentos de teste como Box<serde_json::Value>
    let doc1: Box<serde_json::Value> = Box::new(json!({
        "_id": "project-1",
        "name": "GuardianDB",
        "type": "database",
        "stars": 100
    }));

    let doc2: Box<serde_json::Value> = Box::new(json!({
        "_id": "project-2",
        "name": "Iroh",
        "type": "network",
        "stars": 500
    }));

    let doc3: Box<serde_json::Value> = Box::new(json!({
        "_id": "project-3",
        "name": "LibP2P",
        "type": "network",
        "stars": 1000
    }));

    // Adicionar documentos
    docs.put(doc1).await.expect("Failed to put doc1");
    docs.put(doc2).await.expect("Failed to put doc2");
    docs.put(doc3).await.expect("Failed to put doc3");

    tracing::info!("✓ Document PUT operations completed");

    // Testar GET de documento específico
    let retrieved = docs
        .get("project-1", None)
        .await
        .expect("Failed to get doc");
    assert_eq!(retrieved.len(), 1);
    if let Some(value) = retrieved[0].downcast_ref::<serde_json::Value>() {
        assert_eq!(value["name"], "GuardianDB");
    }

    tracing::info!("✓ Document GET operation completed");

    // Testar QUERY - criar filtro que aceita apenas documentos do tipo "network"
    let filter: guardian_db::traits::AsyncDocumentFilter =
        Box::pin(|doc: &guardian_db::traits::Document| {
            // Extrair informação necessária do documento de forma síncrona
            let is_network = if let Some(value) = doc.downcast_ref::<serde_json::Value>() {
                value.get("type").and_then(|v| v.as_str()) == Some("network")
            } else {
                false
            };
            // Retornar Future com resultado
            Box::pin(async move { Ok(is_network) })
                as std::pin::Pin<
                    Box<
                        dyn std::future::Future<
                                Output = Result<bool, Box<dyn std::error::Error + Send + Sync>>,
                            > + Send,
                    >,
                >
        });
    let network_docs = docs.query(filter).await.expect("Failed to query documents");

    assert_eq!(network_docs.len(), 2, "Expected 2 network documents");

    tracing::info!("✓ Document QUERY operation completed");

    // Testar DELETE
    docs.delete("project-2")
        .await
        .expect("Failed to delete document");

    let after_delete = docs
        .get("project-2", None)
        .await
        .expect("Failed to get deleted doc");
    assert_eq!(
        after_delete.len(),
        0,
        "Deleted document should not be found"
    );

    // Verificar que outros documentos ainda existem
    let filter_all: guardian_db::traits::AsyncDocumentFilter =
        Box::pin(|_doc: &guardian_db::traits::Document| {
            Box::pin(async move { Ok(true) })
                as std::pin::Pin<
                    Box<
                        dyn std::future::Future<
                                Output = Result<bool, Box<dyn std::error::Error + Send + Sync>>,
                            > + Send,
                    >,
                >
        });
    let remaining = docs
        .query(filter_all)
        .await
        .expect("Failed to get all docs");
    assert_eq!(remaining.len(), 2, "Expected 2 documents after deletion");

    tracing::info!("✓ Document lifecycle completed successfully");
}

#[tokio::test]
async fn test_multiple_stores_same_database() {
    init_test_logging();

    let node = TestNode::new("multi_store_test")
        .await
        .expect("Failed to create test node");

    // Criar múltiplos stores do mesmo tipo
    let log1 = node
        .db
        .log("events-1", None)
        .await
        .expect("Failed to create log1");
    let log2 = node
        .db
        .log("events-2", None)
        .await
        .expect("Failed to create log2");

    let kv1 = node
        .db
        .key_value("config-1", None)
        .await
        .expect("Failed to create kv1");
    let kv2 = node
        .db
        .key_value("config-2", None)
        .await
        .expect("Failed to create kv2");

    // Adicionar dados em cada store
    log1.add(b"Log 1 event".to_vec())
        .await
        .expect("Failed to add to log1");
    log2.add(b"Log 2 event".to_vec())
        .await
        .expect("Failed to add to log2");

    kv1.put("key1", b"value1".to_vec())
        .await
        .expect("Failed to put in kv1");
    kv2.put("key2", b"value2".to_vec())
        .await
        .expect("Failed to put in kv2");

    // Verificar isolamento dos dados
    let log1_ops = log1.list(None).await.expect("Failed to get log1 entries");
    let log2_ops = log2.list(None).await.expect("Failed to get log2 entries");

    assert_eq!(log1_ops.len(), 1);
    assert_eq!(log2_ops.len(), 1);
    assert_ne!(log1_ops[0].value(), log2_ops[0].value());

    let kv1_data = kv1.all();
    let kv2_data = kv2.all();

    assert_eq!(kv1_data.get("key1").unwrap(), b"value1");
    assert_eq!(kv2_data.get("key2").unwrap(), b"value2");

    // Verificar que keys não vazam entre stores
    assert!(
        !kv1_data.contains_key("key2"),
        "key2 should not exist in kv1"
    );
    assert!(
        !kv2_data.contains_key("key1"),
        "key1 should not exist in kv2"
    );

    tracing::info!("✓ Multiple stores isolation verified successfully");
}

#[tokio::test]
async fn test_large_payload_handling() {
    init_test_logging();

    let node = TestNode::new("large_payload_test")
        .await
        .expect("Failed to create test node");

    let log = node
        .db
        .log("large-events", None)
        .await
        .expect("Failed to create log");

    // Criar payloads de diferentes tamanhos
    let small_payload = vec![0u8; 1024]; // 1 KB
    let medium_payload = vec![1u8; 100 * 1024]; // 100 KB
    let large_payload = vec![2u8; 1024 * 1024]; // 1 MB

    // Adicionar payloads
    log.add(small_payload.clone())
        .await
        .expect("Failed to add small payload");
    log.add(medium_payload.clone())
        .await
        .expect("Failed to add medium payload");
    log.add(large_payload.clone())
        .await
        .expect("Failed to add large payload");

    // Verificar que todos foram armazenados corretamente
    let ops = log.list(None).await.expect("Failed to retrieve entries");
    assert_eq!(ops.len(), 3);

    assert_eq!(ops[0].value().len(), 1024);
    assert_eq!(ops[1].value().len(), 100 * 1024);
    assert_eq!(ops[2].value().len(), 1024 * 1024);

    // Verificar integridade dos dados
    assert_eq!(ops[0].value(), &small_payload);
    assert_eq!(ops[1].value(), &medium_payload);
    assert_eq!(ops[2].value(), &large_payload);

    tracing::info!("✓ Large payload handling verified successfully");
}

#[tokio::test]
async fn test_concurrent_operations() {
    init_test_logging();

    let node = TestNode::new("concurrent_test")
        .await
        .expect("Failed to create test node");

    let log = node
        .db
        .log("concurrent-events", None)
        .await
        .expect("Failed to create log");

    // Criar múltiplas tasks que adicionam entries concorrentemente
    let mut handles = vec![];

    for i in 0..10 {
        let log_clone = log.clone();
        let handle = tokio::spawn(async move {
            let data = format!("Concurrent event {}", i).into_bytes();
            log_clone.add(data).await
        });
        handles.push(handle);
    }

    // Aguardar todas as tasks
    for handle in handles {
        handle
            .await
            .expect("Task panicked")
            .expect("Failed to add entry");
    }

    // Verificar que todas as entries foram adicionadas
    let ops = log.list(None).await.expect("Failed to get entries");
    assert_eq!(
        ops.len(),
        10,
        "Expected 10 entries from concurrent operations"
    );

    tracing::info!("✓ Concurrent operations completed successfully");
}

#[tokio::test]
async fn test_store_with_custom_options() {
    init_test_logging();

    let node = TestNode::new("custom_options_test")
        .await
        .expect("Failed to create test node");

    // Criar store com opções padrão (customização via CreateDBOptions funciona)
    let options = CreateDBOptions {
        create: Some(true),
        store_type: Some("eventlog".to_string()),
        ..Default::default()
    };

    let log = node
        .db
        .log("custom-log", Some(options))
        .await
        .expect("Failed to create log with options");

    log.add(b"Custom config event".to_vec())
        .await
        .expect("Failed to add entry");

    let ops = log.list(None).await.expect("Failed to get entries");
    assert_eq!(ops.len(), 1);

    tracing::info!("✓ Store with custom options created successfully");
}

#[tokio::test]
async fn test_empty_store_operations() {
    init_test_logging();

    let node = TestNode::new("empty_store_test")
        .await
        .expect("Failed to create test node");

    // Criar stores vazios
    let log = node
        .db
        .log("empty-log", None)
        .await
        .expect("Failed to create log");
    let kv = node
        .db
        .key_value("empty-kv", None)
        .await
        .expect("Failed to create kv");
    let docs = node
        .db
        .docs("empty-docs", None)
        .await
        .expect("Failed to create docs");

    // Testar operações em stores vazios
    let log_ops = log.list(None).await.expect("Failed to get log entries");
    assert_eq!(log_ops.len(), 0, "Empty log should have 0 entries");

    let kv_data = kv.all();
    assert_eq!(kv_data.len(), 0, "Empty KV should have 0 keys");

    let filter_all: guardian_db::traits::AsyncDocumentFilter =
        Box::pin(|_doc: &guardian_db::traits::Document| {
            Box::pin(async move { Ok(true) })
                as std::pin::Pin<
                    Box<
                        dyn std::future::Future<
                                Output = Result<bool, Box<dyn std::error::Error + Send + Sync>>,
                            > + Send,
                    >,
                >
        });
    let all_docs = docs.query(filter_all).await.expect("Failed to query docs");
    assert_eq!(all_docs.len(), 0, "Empty docs should have 0 documents");

    tracing::info!("✓ Empty store operations verified successfully");
}
