use crate::address::Address;
/// Testes para DocumentStore
///
/// Valida todas as operações principais do document store usando IrohClient
use crate::address::GuardianDBAddress;
use crate::log::identity::{Identity, Signatures};
use crate::message_marshaler::PostcardMarshaler;
use crate::p2p::EventBus;
use crate::p2p::messaging::one_on_one_channel::new_channel_factory;
use crate::p2p::network::client::IrohClient;
use crate::p2p::network::config::ClientConfig;
use crate::stores::document_store::GuardianDBDocumentStore;
use crate::traits::{NewStoreOptions, Store};
use serde_json::json;
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
    let unique_data = format!("test-docstore-{}-{}", timestamp, counter);
    let hash_bytes: [u8; 32] = blake3::hash(unique_data.as_bytes()).into();
    let hash = iroh_blobs::Hash::from(hash_bytes);

    Arc::new(GuardianDBAddress::new(
        hash,
        format!("test-docstore-{}-{}", timestamp, counter),
    ))
}

/// Helper para criar um DocumentStore de teste completo
async fn create_test_store()
-> Result<(GuardianDBDocumentStore, TempDir), Box<dyn std::error::Error>> {
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

    let mut options = NewStoreOptions::default();
    options.event_bus = Some(event_bus);
    options.pubsub = Some(pubsub);
    options.message_marshaler = Some(message_marshaler);
    options.direct_channel = Some(direct_channel);
    options.directory = temp_dir.path().join("cache").to_string_lossy().to_string();

    let store = GuardianDBDocumentStore::new(client, identity, address, options).await?;

    Ok((store, temp_dir))
}

/// Helper para criar um documento de teste simples
fn create_test_document(id: &str, name: &str) -> serde_json::Value {
    json!({
        "_id": id,
        "name": name,
        "type": "test"
    })
}

// ============= TESTES =============

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_document_store_creation() {
        let result = create_test_store().await;
        assert!(result.is_ok(), "Should create DocumentStore successfully");

        let (store, _temp_dir) = result.unwrap();
        assert_eq!(store.store_type(), "document");
        assert!(!store.db_name().is_empty());
    }

    #[tokio::test]
    async fn test_put_document() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        let doc = create_test_document("doc1", "Test Document");
        let result = store.put(doc).await;
        assert!(
            result.is_ok(),
            "Should put document successfully: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_get_document() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        // Put a document
        let doc = create_test_document("doc1", "Test Document");
        store
            .put(doc.clone())
            .await
            .expect("Failed to put document");

        // Get the document using query
        let result = store.query(|d| Ok(d["_id"] == "doc1"));
        assert!(
            result.is_ok(),
            "Should get document successfully: {:?}",
            result
        );

        let docs = result.unwrap();
        assert!(!docs.is_empty(), "Should find the document");
        assert_eq!(docs[0]["_id"], "doc1");
        assert_eq!(docs[0]["name"], "Test Document");
    }

    #[tokio::test]
    async fn test_delete_document() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        // Put a document
        let doc = create_test_document("doc1", "Test Document");
        store
            .put(doc.clone())
            .await
            .expect("Failed to put document");

        // Delete the document
        let result = store.delete("doc1").await;
        assert!(
            result.is_ok(),
            "Should delete document successfully: {:?}",
            result
        );

        // Verify it's deleted
        let get_result = store.query(|d| Ok(d["_id"] == "doc1"));
        assert!(get_result.is_ok());
        assert!(
            get_result.unwrap().is_empty(),
            "Document should not exist after deletion"
        );
    }

    #[tokio::test]
    async fn test_delete_nonexistent_document() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        let result = store.delete("nonexistent").await;
        assert!(
            result.is_err(),
            "Should error when deleting nonexistent document"
        );
    }

    #[tokio::test]
    async fn test_document_without_id() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        let doc = json!({
            "name": "No ID Document",
            "type": "test"
        });

        let result = store.put(doc).await;
        assert!(result.is_err(), "Should error on document without _id");
    }

    #[tokio::test]
    async fn test_document_with_empty_id() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        let doc = json!({
            "_id": "",
            "name": "Empty ID Document"
        });

        let result = store.put(doc).await;
        assert!(result.is_err(), "Should error on document with empty _id");
    }

    #[tokio::test]
    async fn test_query_simple() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        // Put multiple documents
        store
            .put(create_test_document("doc1", "Alice"))
            .await
            .expect("Failed to put doc1");
        store
            .put(create_test_document("doc2", "Bob"))
            .await
            .expect("Failed to put doc2");
        store
            .put(create_test_document("doc3", "Charlie"))
            .await
            .expect("Failed to put doc3");

        // Query all documents
        let result = store.query(|_| Ok(true));
        assert!(
            result.is_ok(),
            "Should query documents successfully: {:?}",
            result
        );

        let docs = result.unwrap();
        assert!(docs.len() >= 3, "Should return at least 3 documents");
    }

    #[tokio::test]
    async fn test_query_with_prefix() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        // Put documents with different prefixes
        store
            .put(create_test_document("user1", "Alice"))
            .await
            .expect("Failed to put user1");
        store
            .put(create_test_document("user2", "Bob"))
            .await
            .expect("Failed to put user2");
        store
            .put(create_test_document("admin1", "Charlie"))
            .await
            .expect("Failed to put admin1");

        // Query with prefix filter
        let result = store.query(|doc| {
            if let Some(id) = doc["_id"].as_str() {
                Ok(id.starts_with("user"))
            } else {
                Ok(false)
            }
        });
        assert!(
            result.is_ok(),
            "Should query with prefix successfully: {:?}",
            result
        );

        let docs = result.unwrap();
        assert_eq!(docs.len(), 2, "Should find 2 user documents");
        for doc in docs {
            let id = doc["_id"].as_str().unwrap();
            assert!(id.starts_with("user"), "All results should match prefix");
        }
    }

    #[tokio::test]
    async fn test_put_batch() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        let docs = vec![
            create_test_document("batch1", "Doc 1"),
            create_test_document("batch2", "Doc 2"),
            create_test_document("batch3", "Doc 3"),
        ];

        let result = store.put_batch(docs).await;
        assert!(
            result.is_ok(),
            "Should put batch successfully: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_put_batch_empty() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        let docs: Vec<serde_json::Value> = vec![];
        let result = store.put_batch(docs).await;
        assert!(result.is_err(), "Should error on empty batch");
    }

    #[tokio::test]
    async fn test_store_type() {
        let (store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        assert_eq!(store.store_type(), "document");
    }

    #[tokio::test]
    async fn test_multiple_operations() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        // Put
        let doc = create_test_document("multi1", "Multi Test");
        store
            .put(doc.clone())
            .await
            .expect("Failed to put document");

        // Get
        let retrieved = store
            .query(|d| Ok(d["_id"] == "multi1"))
            .expect("Failed to get document");
        assert!(!retrieved.is_empty());
        assert_eq!(retrieved[0]["_id"], "multi1");

        // Update (put with same ID)
        let updated = json!({
            "_id": "multi1",
            "name": "Updated Multi Test",
            "type": "test"
        });
        store.put(updated).await.expect("Failed to update document");

        // Get updated
        let retrieved_updated = store
            .query(|d| Ok(d["_id"] == "multi1"))
            .expect("Failed to get updated document");
        assert!(!retrieved_updated.is_empty());
        assert_eq!(retrieved_updated[0]["name"], "Updated Multi Test");

        // Delete
        store
            .delete("multi1")
            .await
            .expect("Failed to delete document");

        // Verify deletion
        let get_after_delete = store.query(|d| Ok(d["_id"] == "multi1"));
        assert!(
            get_after_delete.unwrap().is_empty(),
            "Document should not exist after deletion"
        );
    }

    #[tokio::test]
    async fn test_complex_document_structure() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        let complex_doc = json!({
            "_id": "complex1",
            "name": "Complex Document",
            "nested": {
                "level1": {
                    "level2": {
                        "value": "deep value"
                    }
                }
            },
            "array": [1, 2, 3, 4, 5],
            "mixed": [
                {"key": "value1"},
                {"key": "value2"}
            ]
        });

        store
            .put(complex_doc.clone())
            .await
            .expect("Failed to put complex document");
        let docs = store
            .query(|d| Ok(d["_id"] == "complex1"))
            .expect("Failed to get complex document");
        assert!(!docs.is_empty());
        let retrieved = &docs[0];

        assert_eq!(retrieved["_id"], "complex1");
        assert_eq!(
            retrieved["nested"]["level1"]["level2"]["value"],
            "deep value"
        );
        assert_eq!(retrieved["array"][0], 1);
    }

    #[tokio::test]
    async fn test_integration_complete_workflow() {
        let (mut store, _temp_dir) = create_test_store()
            .await
            .expect("Failed to create test store");

        // 1. Criar múltiplos documentos
        let docs = vec![
            create_test_document("workflow1", "Document 1"),
            create_test_document("workflow2", "Document 2"),
            create_test_document("workflow3", "Document 3"),
        ];

        for doc in &docs {
            store
                .put(doc.clone())
                .await
                .expect("Failed to put document in workflow");
        }

        // 2. Query todos
        let all_docs = store
            .query(|_| Ok(true))
            .expect("Failed to query all documents");
        assert!(all_docs.len() >= 3, "Should have at least 3 documents");

        // 3. Atualizar um
        let updated = json!({
            "_id": "workflow2",
            "name": "Updated Document 2",
            "type": "test",
            "updated": true
        });
        store.put(updated).await.expect("Failed to update document");

        // 4. Verificar atualização
        let retrieved_docs = store
            .query(|d| Ok(d["_id"] == "workflow2"))
            .expect("Failed to get updated document");
        assert!(!retrieved_docs.is_empty());
        assert_eq!(retrieved_docs[0]["updated"], true);

        // 5. Deletar um
        store
            .delete("workflow3")
            .await
            .expect("Failed to delete document");

        // 6. Verificar que foi deletado
        let get_deleted = store.query(|d| Ok(d["_id"] == "workflow3"));
        assert!(
            get_deleted.unwrap().is_empty(),
            "Deleted document should not exist"
        );

        // 7. Query final
        let final_docs = store
            .query(|_| Ok(true))
            .expect("Failed to query final documents");
        let workflow_docs: Vec<_> = final_docs
            .iter()
            .filter(|d| d["_id"].as_str().unwrap().starts_with("workflow"))
            .collect();
        assert_eq!(
            workflow_docs.len(),
            2,
            "Should have 2 workflow documents remaining"
        );
    }
}
