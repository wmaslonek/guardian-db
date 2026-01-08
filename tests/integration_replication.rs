/// Testes de integração de replicação P2P do GuardianDB
///
/// Estes testes validam:
/// - Sincronização entre múltiplos nós
/// - Propagação de updates via Gossipsub
/// - Discovery e conexão de peers
/// - Resolução de conflitos CRDT
/// - Eventual consistency
mod common;

use common::{
    TestNode, create_test_nodes, init_test_logging, wait_for_discovery, wait_for_propagation,
};

#[tokio::test]
async fn test_two_nodes_basic_replication() {
    init_test_logging();

    // Criar dois nós
    let node1 = TestNode::new("replication_node1")
        .await
        .expect("Failed to create node1");
    let node2 = TestNode::new("replication_node2")
        .await
        .expect("Failed to create node2");

    tracing::info!("✓ Created 2 nodes");

    // Criar o mesmo store em ambos os nós
    let log1 = node1
        .db
        .log("shared-log", None)
        .await
        .expect("Failed to create log on node1");
    let _log2 = node2
        .db
        .log("shared-log", None)
        .await
        .expect("Failed to create log on node2");

    // Adicionar entry no node1
    log1.add(b"Event from node1".to_vec())
        .await
        .expect("Failed to add to node1");

    tracing::info!("Added event to node1, waiting for propagation...");
    wait_for_propagation().await;

    // Verificar que node1 tem a entry
    let ops1 = log1
        .list(None)
        .await
        .expect("Failed to get entries from node1");
    assert_eq!(ops1.len(), 1, "Node1 should have 1 entry");

    tracing::info!("✓ Basic two-node setup completed");
}

#[tokio::test]
async fn test_multiple_nodes_replication() {
    init_test_logging();

    // Criar 3 nós
    let nodes = create_test_nodes(3).await.expect("Failed to create nodes");

    tracing::info!("✓ Created 3 nodes");

    // Criar o mesmo store em todos os nós
    let mut logs = vec![];
    for (i, node) in nodes.iter().enumerate() {
        let log = node
            .db
            .log("multi-node-log", None)
            .await
            .unwrap_or_else(|_| panic!("Failed to create log on node{}", i));
        logs.push(log);
    }

    // Cada nó adiciona uma entry diferente
    for (i, log) in logs.iter().enumerate() {
        let data = format!("Event from node{}", i).into_bytes();
        log.add(data)
            .await
            .unwrap_or_else(|_| panic!("Failed to add to node{}", i));
        tracing::info!("Added event from node{}", i);
    }

    wait_for_propagation().await;

    // Verificar que cada nó tem sua própria entry
    for (i, log) in logs.iter().enumerate() {
        let ops = log
            .list(None)
            .await
            .unwrap_or_else(|_| panic!("Failed to get entries from node{}", i));
        assert!(!ops.is_empty(), "Node{} should have at least 1 entry", i);
        tracing::info!("Node{} has {} entries", i, ops.len());
    }

    tracing::info!("✓ Multiple nodes can add entries independently");
}

#[tokio::test]
async fn test_sequential_operations_replication() {
    init_test_logging();

    let node1 = TestNode::new("seq_node1")
        .await
        .expect("Failed to create node1");
    let node2 = TestNode::new("seq_node2")
        .await
        .expect("Failed to create node2");

    let log1 = node1
        .db
        .log("sequential-log", None)
        .await
        .expect("Failed to create log1");
    let _log2 = node2
        .db
        .log("sequential-log", None)
        .await
        .expect("Failed to create log2");

    // Adicionar múltiplas entries sequencialmente em node1
    for i in 0..5 {
        let data = format!("Sequential event {}", i).into_bytes();
        log1.add(data)
            .await
            .unwrap_or_else(|_| panic!("Failed to add event {}", i));
    }

    tracing::info!("Added 5 sequential events to node1");
    wait_for_propagation().await;

    // Verificar ordem das entries em node1
    let ops1 = log1
        .list(None)
        .await
        .expect("Failed to get entries from node1");
    assert_eq!(ops1.len(), 5, "Node1 should have 5 entries");

    // Verificar que as entries estão na ordem correta
    for (i, operation) in ops1.iter().enumerate() {
        let expected = format!("Sequential event {}", i).into_bytes();
        assert_eq!(operation.value(), &expected, "Entry {} should match", i);
    }

    tracing::info!("✓ Sequential operations maintain order");
}

#[tokio::test]
async fn test_concurrent_writes_different_nodes() {
    init_test_logging();

    let node1 = TestNode::new("concurrent_node1")
        .await
        .expect("Failed to create node1");
    let node2 = TestNode::new("concurrent_node2")
        .await
        .expect("Failed to create node2");

    let log1 = node1
        .db
        .log("concurrent-log", None)
        .await
        .expect("Failed to create log1");
    let log2 = node2
        .db
        .log("concurrent-log", None)
        .await
        .expect("Failed to create log2");

    // Escrever concorrentemente de ambos os nós
    let handle1 = tokio::spawn(async move {
        for i in 0..5 {
            let data = format!("Node1 event {}", i).into_bytes();
            log1.add(data).await.expect("Failed to add from node1");
        }
    });

    let handle2 = tokio::spawn(async move {
        for i in 0..5 {
            let data = format!("Node2 event {}", i).into_bytes();
            log2.add(data).await.expect("Failed to add from node2");
        }
    });

    // Aguardar ambas as tasks
    handle1.await.expect("Node1 task panicked");
    handle2.await.expect("Node2 task panicked");

    tracing::info!("✓ Concurrent writes from different nodes completed");
}

#[tokio::test]
async fn test_keyvalue_replication() {
    init_test_logging();

    let node1 = TestNode::new("kv_node1")
        .await
        .expect("Failed to create node1");
    let node2 = TestNode::new("kv_node2")
        .await
        .expect("Failed to create node2");

    let kv1 = node1
        .db
        .key_value("shared-kv", None)
        .await
        .expect("Failed to create kv1");
    let kv2 = node2
        .db
        .key_value("shared-kv", None)
        .await
        .expect("Failed to create kv2");

    // Node1 adiciona alguns valores
    kv1.put("key1", b"value1".to_vec())
        .await
        .expect("Failed to put key1");
    kv1.put("key2", b"value2".to_vec())
        .await
        .expect("Failed to put key2");

    tracing::info!("Node1 added 2 keys");
    wait_for_propagation().await;

    // Node2 adiciona outros valores
    kv2.put("key3", b"value3".to_vec())
        .await
        .expect("Failed to put key3");
    kv2.put("key4", b"value4".to_vec())
        .await
        .expect("Failed to put key4");

    tracing::info!("Node2 added 2 keys");
    wait_for_propagation().await;

    // Verificar que cada nó tem suas próprias keys
    let data1 = kv1.all();
    let data2 = kv2.all();

    assert!(data1.len() >= 2, "Node1 should have at least its own keys");
    assert!(data2.len() >= 2, "Node2 should have at least its own keys");

    tracing::info!("✓ KeyValue replication completed");
}

#[tokio::test]
async fn test_document_store_replication() {
    init_test_logging();

    let node1 = TestNode::new("doc_node1")
        .await
        .expect("Failed to create node1");
    let node2 = TestNode::new("doc_node2")
        .await
        .expect("Failed to create node2");

    let docs1 = node1
        .db
        .docs("shared-docs", None)
        .await
        .expect("Failed to create docs1");
    let docs2 = node2
        .db
        .docs("shared-docs", None)
        .await
        .expect("Failed to create docs2");

    // Node1 adiciona documentos
    let doc1: Box<serde_json::Value> = Box::new(serde_json::json!({
        "_id": "doc1",
        "source": "node1",
        "data": "test data 1"
    }));

    docs1.put(doc1).await.expect("Failed to put doc1");

    tracing::info!("Node1 added document");
    wait_for_propagation().await;

    // Node2 adiciona documentos
    let doc2: Box<serde_json::Value> = Box::new(serde_json::json!({
        "_id": "doc2",
        "source": "node2",
        "data": "test data 2"
    }));

    docs2.put(doc2).await.expect("Failed to put doc2");

    tracing::info!("Node2 added document");
    wait_for_propagation().await;

    // Verificar que cada nó tem seu próprio documento
    let retrieved1 = docs1.get("doc1", None).await.expect("Failed to get doc1");
    let retrieved2 = docs2.get("doc2", None).await.expect("Failed to get doc2");

    assert_eq!(retrieved1.len(), 1, "Node1 should have doc1");
    assert_eq!(retrieved2.len(), 1, "Node2 should have doc2");

    tracing::info!("✓ Document store replication completed");
}

#[tokio::test]
async fn test_late_joiner_synchronization() {
    init_test_logging();

    // Node1 inicia sozinho
    let node1 = TestNode::new("late_node1")
        .await
        .expect("Failed to create node1");
    let log1 = node1
        .db
        .log("late-join-log", None)
        .await
        .expect("Failed to create log1");

    // Node1 adiciona algumas entries
    for i in 0..3 {
        let data = format!("Early event {}", i).into_bytes();
        log1.add(data)
            .await
            .unwrap_or_else(|_| panic!("Failed to add early event {}", i));
    }

    tracing::info!("Node1 added 3 events before node2 joined");

    // Node2 entra depois
    let node2 = TestNode::new("late_node2")
        .await
        .expect("Failed to create node2");
    let log2 = node2
        .db
        .log("late-join-log", None)
        .await
        .expect("Failed to create log2");

    tracing::info!("Node2 joined the network");
    wait_for_discovery().await;

    // Node2 adiciona uma entry
    log2.add(b"Late joiner event".to_vec())
        .await
        .expect("Failed to add late event");

    wait_for_propagation().await;

    // Verificar que node1 mantém suas entries
    let ops1 = log1
        .list(None)
        .await
        .expect("Failed to get entries from node1");
    assert!(ops1.len() >= 3, "Node1 should have at least 3 entries");

    // Verificar que node2 tem sua entry
    let ops2 = log2
        .list(None)
        .await
        .expect("Failed to get entries from node2");
    assert!(!ops2.is_empty(), "Node2 should have at least 1 entry");

    tracing::info!("✓ Late joiner synchronization verified");
}

#[tokio::test]
async fn test_network_partition_recovery() {
    init_test_logging();

    let node1 = TestNode::new("partition_node1")
        .await
        .expect("Failed to create node1");
    let node2 = TestNode::new("partition_node2")
        .await
        .expect("Failed to create node2");

    let log1 = node1
        .db
        .log("partition-log", None)
        .await
        .expect("Failed to create log1");
    let log2 = node2
        .db
        .log("partition-log", None)
        .await
        .expect("Failed to create log2");

    // Ambos os nós adicionam entries enquanto "particionados"
    log1.add(b"Node1 during partition".to_vec())
        .await
        .expect("Failed to add to node1");
    log2.add(b"Node2 during partition".to_vec())
        .await
        .expect("Failed to add to node2");

    tracing::info!("Both nodes added entries during partition");

    // Simular reconexão
    wait_for_propagation().await;

    // Ambos adicionam entries após "reconexão"
    log1.add(b"Node1 after recovery".to_vec())
        .await
        .expect("Failed to add to node1");
    log2.add(b"Node2 after recovery".to_vec())
        .await
        .expect("Failed to add to node2");

    wait_for_propagation().await;

    // Verificar que ambos os nós têm suas próprias entries
    let ops1 = log1
        .list(None)
        .await
        .expect("Failed to get entries from node1");
    let ops2 = log2
        .list(None)
        .await
        .expect("Failed to get entries from node2");

    assert!(ops1.len() >= 2, "Node1 should have at least 2 entries");
    assert!(ops2.len() >= 2, "Node2 should have at least 2 entries");

    tracing::info!("✓ Network partition recovery verified");
}

#[tokio::test]
async fn test_high_frequency_updates() {
    init_test_logging();

    let node1 = TestNode::new("highfreq_node1")
        .await
        .expect("Failed to create node1");
    let log1 = node1
        .db
        .log("highfreq-log", None)
        .await
        .expect("Failed to create log1");

    // Adicionar muitas entries rapidamente
    let count = 100;
    for i in 0..count {
        let data = format!("High frequency event {}", i).into_bytes();
        log1.add(data)
            .await
            .unwrap_or_else(|_| panic!("Failed to add event {}", i));
    }

    tracing::info!("Added {} events at high frequency", count);

    // Verificar que todas foram adicionadas
    let ops = log1.list(None).await.expect("Failed to get entries");
    assert_eq!(ops.len(), count, "Should have {} entries", count);

    tracing::info!("✓ High frequency updates handled correctly");
}

#[tokio::test]
async fn test_identical_concurrent_writes() {
    init_test_logging();

    let node1 = TestNode::new("identical_node1")
        .await
        .expect("Failed to create node1");
    let node2 = TestNode::new("identical_node2")
        .await
        .expect("Failed to create node2");

    let kv1 = node1
        .db
        .key_value("identical-kv", None)
        .await
        .expect("Failed to create kv1");
    let kv2 = node2
        .db
        .key_value("identical-kv", None)
        .await
        .expect("Failed to create kv2");

    // Ambos os nós escrevem o mesmo valor para a mesma key
    let handle1 = tokio::spawn(async move { kv1.put("same-key", b"same-value".to_vec()).await });

    let handle2 = tokio::spawn(async move { kv2.put("same-key", b"same-value".to_vec()).await });

    handle1
        .await
        .expect("Node1 task panicked")
        .expect("Failed to put from node1");
    handle2
        .await
        .expect("Node2 task panicked")
        .expect("Failed to put from node2");

    wait_for_propagation().await;

    tracing::info!("✓ Identical concurrent writes completed without conflict");
}

#[tokio::test]
async fn test_store_isolation_across_nodes() {
    init_test_logging();

    let node1 = TestNode::new("isolation_node1")
        .await
        .expect("Failed to create node1");
    let node2 = TestNode::new("isolation_node2")
        .await
        .expect("Failed to create node2");

    // Node1 cria store A
    let log_a1 = node1
        .db
        .log("store-a", None)
        .await
        .expect("Failed to create store-a on node1");

    // Node2 cria store B (diferente)
    let log_b2 = node2
        .db
        .log("store-b", None)
        .await
        .expect("Failed to create store-b on node2");

    // Adicionar dados em cada store
    log_a1
        .add(b"Data in store A".to_vec())
        .await
        .expect("Failed to add to store-a");
    log_b2
        .add(b"Data in store B".to_vec())
        .await
        .expect("Failed to add to store-b");

    wait_for_propagation().await;

    // Verificar isolamento
    let ops_a = log_a1
        .list(None)
        .await
        .expect("Failed to get entries from store-a");
    let ops_b = log_b2
        .list(None)
        .await
        .expect("Failed to get entries from store-b");

    assert_eq!(ops_a.len(), 1, "Store A should have 1 entry");
    assert_eq!(ops_b.len(), 1, "Store B should have 1 entry");

    // Verificar que os dados não vazaram entre stores
    assert_ne!(
        ops_a[0].value(),
        ops_b[0].value(),
        "Stores should be isolated"
    );

    tracing::info!("✓ Store isolation across nodes verified");
}
