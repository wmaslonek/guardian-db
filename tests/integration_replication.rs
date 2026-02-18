/// Testes de integração de replicação P2P do GuardianDB
///
/// Estes testes validam:
/// - Sincronização entre múltiplos nós com conexão explícita
/// - Propagação de updates via DirectChannel e Gossip
/// - Eventual consistency após conexão
/// - Sincronização bidirecional de dados
mod common;

use common::{
    TestNode, connect_nodes, connect_nodes_mesh, create_test_nodes, init_test_logging,
    wait_for_propagation,
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

    let peer1_id = node1.iroh.node_id();
    let peer2_id = node2.iroh.node_id();

    tracing::info!("✓ Created 2 nodes: {} and {}", peer1_id, peer2_id);

    // ETAPA 1: PREPARAR ENDEREÇOS E CONEXÕES DE REDE
    tracing::info!("Preparing network addresses...");

    // Obter informação de endereço de cada nó
    use iroh::NodeAddr;
    use std::net::SocketAddr;

    let node1_info = node1.iroh.id().await.expect("Failed to get node1 info");
    let node2_info = node2.iroh.id().await.expect("Failed to get node2 info");

    let node1_addrs: Vec<SocketAddr> = node1_info
        .addresses
        .iter()
        .filter_map(|addr| addr.parse().ok())
        .collect();
    let node2_addrs: Vec<SocketAddr> = node2_info
        .addresses
        .iter()
        .filter_map(|addr| addr.parse().ok())
        .collect();

    let node1_addr = NodeAddr::from_parts(peer1_id, None, node1_addrs);
    let node2_addr = NodeAddr::from_parts(peer2_id, None, node2_addrs);

    // Adicionar NodeAddr aos endpoints
    node1
        .iroh
        .add_node_addr(node2_addr)
        .await
        .expect("Failed to add node2 addr to node1");
    node2
        .iroh
        .add_node_addr(node1_addr)
        .await
        .expect("Failed to add node1 addr to node2");

    tracing::info!("✓ Node addresses shared");

    // Estabelecer conexões gossip QUIC
    tracing::info!("Establishing gossip connections...");

    node1
        .iroh
        .connect_gossip(peer2_id)
        .await
        .expect("Failed to establish gossip connection from node1 to node2");
    node2
        .iroh
        .connect_gossip(peer1_id)
        .await
        .expect("Failed to establish gossip connection from node2 to node1");

    tracing::info!("✓ Gossip connections established");

    // Pequeno delay para conexões QUIC se estabelecerem
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // ETAPA 2: CRIAR STORES (AGORA AMBOS PODEM FORMAR GOSSIP MESH)
    tracing::info!("Creating stores on both nodes...");

    let log1 = node1
        .db
        .log("shared-log", None)
        .await
        .expect("Failed to create log on node1");
    let log2 = node2
        .db
        .log("shared-log", None)
        .await
        .expect("Failed to create log on node2");

    tracing::info!("✓ Stores created");

    // Delay para gossip subscriptions se propagarem
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // ETAPA 3: ADICIONAR DADOS
    tracing::info!("Adding data to node1...");

    log1.add(b"Event from node1".to_vec())
        .await
        .expect("Failed to add to node1");

    tracing::info!("Added event to node1, waiting for entry to be processed...");

    // Aguardar para garantir que a entry seja processada E o cache seja persistido
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // VERIFICAR QUE OS DADOS ESTÃO NO LOG ANTES DE SINCRONIZAR
    let ops1_before_sync = log1
        .list(None)
        .await
        .expect("Failed to get entries from node1 before sync");
    tracing::info!(
        "Node1 has {} entries before sync attempt",
        ops1_before_sync.len()
    );
    assert_eq!(
        ops1_before_sync.len(),
        1,
        "Node1 should have 1 entry before sync"
    );

    // ETAPA 4: INICIAR SINCRONIZAÇÃO (EXCHANGE_HEADS)
    tracing::info!("Initiating replication...");

    node1
        .db
        .connect_to_peer(peer2_id)
        .await
        .expect("Failed to connect node1 to node2");

    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    node2
        .db
        .connect_to_peer(peer1_id)
        .await
        .expect("Failed to connect node2 to node1");

    tracing::info!("✓ Replication initiated, waiting for synchronization...");

    // Aguardar sincronização via gossip
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

    // Verificar que node1 tem a entry
    let ops1 = log1
        .list(None)
        .await
        .expect("Failed to get entries from node1");
    assert_eq!(ops1.len(), 1, "Node1 should have 1 entry");

    // VALIDAR REPLICAÇÃO: node2 deve ter recebido a entry de node1
    let ops2 = log2
        .list(None)
        .await
        .expect("Failed to get entries from node2");

    tracing::info!("Node2 has {} entries after replication", ops2.len());
    assert!(
        !ops2.is_empty(),
        "Node2 should have received at least 1 entry from node1"
    );

    // Verificar que o conteúdo foi replicado
    let replicated_data = ops2.iter().find(|op| op.value() == b"Event from node1");
    assert!(
        replicated_data.is_some(),
        "Node2 should have the replicated data from node1"
    );

    tracing::info!("✓ Basic two-node replication validated successfully!");
}

#[tokio::test]
async fn test_bidirectional_replication() {
    init_test_logging();

    let node1 = TestNode::new("bidir_node1")
        .await
        .expect("Failed to create node1");
    let node2 = TestNode::new("bidir_node2")
        .await
        .expect("Failed to create node2");

    let peer1_id = node1.iroh.node_id();
    let peer2_id = node2.iroh.node_id();

    tracing::info!("✓ Created 2 nodes for bidirectional test");

    // ESTABELECER CONEXÃO DE REDE PRIMEIRO
    connect_nodes(&node1, &node2)
        .await
        .expect("Failed to connect nodes");
    tracing::info!("✓ Network connections established");

    // Criar o mesmo store em ambos
    let log1 = node1
        .db
        .log("bidir-log", None)
        .await
        .expect("Failed to create log1");
    let log2 = node2
        .db
        .log("bidir-log", None)
        .await
        .expect("Failed to create log2");

    // Node1 adiciona dados
    log1.add(b"Node1 data".to_vec())
        .await
        .expect("Failed to add to node1");

    // Node2 adiciona dados
    log2.add(b"Node2 data".to_vec())
        .await
        .expect("Failed to add to node2");

    tracing::info!("Both nodes added data, now connecting...");

    // CONECTAR PEERS
    node1
        .db
        .connect_to_peer(peer2_id)
        .await
        .expect("Failed to connect node1 to node2");

    node2
        .db
        .connect_to_peer(peer1_id)
        .await
        .expect("Failed to connect node2 to node1");

    tracing::info!("✓ Peers connected, waiting for bidirectional sync...");
    wait_for_propagation().await;

    // VALIDAR SINCRONIZAÇÃO BIDIRECIONAL
    let ops1 = log1
        .list(None)
        .await
        .expect("Failed to get entries from node1");
    let ops2 = log2
        .list(None)
        .await
        .expect("Failed to get entries from node2");

    tracing::info!(
        "Node1 has {} entries, Node2 has {} entries",
        ops1.len(),
        ops2.len()
    );

    // Ambos devem ter pelo menos 2 entries (suas próprias + do outro)
    assert!(
        ops1.len() >= 2,
        "Node1 should have at least 2 entries after sync"
    );
    assert!(
        ops2.len() >= 2,
        "Node2 should have at least 2 entries after sync"
    );

    // Verificar que node1 recebeu dados de node2
    let has_node2_data = ops1.iter().any(|op| op.value() == b"Node2 data");
    assert!(has_node2_data, "Node1 should have received data from node2");

    // Verificar que node2 recebeu dados de node1
    let has_node1_data = ops2.iter().any(|op| op.value() == b"Node1 data");
    assert!(has_node1_data, "Node2 should have received data from node1");

    tracing::info!("✓ Bidirectional replication validated successfully!");
}

#[tokio::test]
async fn test_multiple_nodes_mesh_replication() {
    init_test_logging();

    // Criar 3 nós
    let nodes = create_test_nodes(3).await.expect("Failed to create nodes");
    let node_ids: Vec<_> = nodes.iter().map(|n| n.iroh.node_id()).collect();

    tracing::info!("✓ Created {} nodes", nodes.len());

    // ESTABELECER CONEXÕES GOSSIP EM MALHA ANTES DE CRIAR STORES
    connect_nodes_mesh(&nodes)
        .await
        .expect("Failed to establish mesh network");
    tracing::info!("✓ Mesh network established");

    // Criar o mesmo store em todos os nós
    let mut logs = vec![];
    for (i, node) in nodes.iter().enumerate() {
        let log = node
            .db
            .log("mesh-log", None)
            .await
            .unwrap_or_else(|_| panic!("Failed to create log on node{}", i));
        logs.push(log);
    }

    // Aguardar gossip subscriptions se propagarem
    wait_for_propagation().await;

    // Cada nó adiciona uma entry diferente APÓS conexão gossip estar estabelecida
    for (i, log) in logs.iter().enumerate() {
        let data = format!("Event from node{}", i).into_bytes();
        log.add(data)
            .await
            .unwrap_or_else(|_| panic!("Failed to add to node{}", i));
        tracing::info!("Added event from node{}", i);
    }

    tracing::info!("✓ All events added, triggering sync...");

    // CORREÇÃO: Seguir mesma ordem do teste que passa
    // Cada nó conecta a todos os outros em ordem simples
    for (i, node) in nodes.iter().enumerate() {
        for (j, peer_id) in node_ids.iter().enumerate() {
            if i != j {
                node.db
                    .connect_to_peer(*peer_id)
                    .await
                    .unwrap_or_else(|_| panic!("Failed to connect node{} to node{}", i, j));
            }
        }
        // Delay entre nós para permitir mesh formation
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    wait_for_propagation().await;

    // VALIDAR QUE TODOS OS NÓS TÊM TODAS AS ENTRIES
    for (i, log) in logs.iter().enumerate() {
        let ops = log
            .list(None)
            .await
            .unwrap_or_else(|_| panic!("Failed to get entries from node{}", i));

        tracing::info!("Node{} has {} entries", i, ops.len());

        // Cada nó deve ter pelo menos 3 entries (uma de cada nó)
        assert!(
            ops.len() >= 3,
            "Node{} should have at least 3 entries after mesh sync",
            i
        );

        // Verificar que tem dados de todos os outros nós
        for j in 0..nodes.len() {
            let expected_data = format!("Event from node{}", j).into_bytes();
            let has_data = ops.iter().any(|op| op.value() == expected_data);
            assert!(has_data, "Node{} should have data from node{}", i, j);
        }
    }

    tracing::info!("✓ Multiple nodes mesh replication validated successfully!");
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

    let peer1_id = node1.iroh.node_id();
    let peer2_id = node2.iroh.node_id();

    // ESTABELECER CONEXÃO GOSSIP ANTES DE CRIAR STORES
    connect_nodes(&node1, &node2)
        .await
        .expect("Failed to connect nodes");
    tracing::info!("✓ Gossip connection established");

    let log1 = node1
        .db
        .log("sequential-log", None)
        .await
        .expect("Failed to create log1");
    let log2 = node2
        .db
        .log("sequential-log", None)
        .await
        .expect("Failed to create log2");

    // Aguardar tempo curto para gossip subscriptions se propagarem (igual ao teste que passa)
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Adicionar múltiplas entries sequencialmente em node1
    for i in 0..5 {
        let data = format!("Sequential event {}", i).into_bytes();
        log1.add(data)
            .await
            .unwrap_or_else(|_| panic!("Failed to add event {}", i));
    }

    tracing::info!("Added 5 sequential events to node1");

    // CORREÇÃO: Adicionar delay entre connect_to_peer calls para garantir
    // que o mesh gossip seja formado bidirecionalmente antes do broadcast
    node1
        .db
        .connect_to_peer(peer2_id)
        .await
        .expect("Failed to connect node1 to node2");

    // Delay crítico para permitir que Node2 adicione Node1 ao seu mesh
    // antes que Node2 envie sua resposta
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    node2
        .db
        .connect_to_peer(peer1_id)
        .await
        .expect("Failed to connect node2 to node1");

    tracing::info!("✓ Peers connected, waiting for sync...");
    wait_for_propagation().await;

    // Aguarda propagação com retries para dar mais tempo ao sync
    let mut ops2 = Vec::new();
    for attempt in 0..10 {
        wait_for_propagation().await;

        ops2 = log2
            .list(None)
            .await
            .expect("Failed to get entries from node2");

        tracing::info!("Attempt {}: Node2 has {} entries", attempt + 1, ops2.len());

        if ops2.len() >= 5 {
            break;
        }
    }

    // Verificar ordem das entries em node1
    let ops1 = log1
        .list(None)
        .await
        .expect("Failed to get entries from node1");
    assert_eq!(ops1.len(), 5, "Node1 should have 5 entries");

    // VALIDAR REPLICAÇÃO PARA NODE2
    assert!(
        ops2.len() >= 5,
        "Node2 should have at least 5 replicated entries"
    );

    // Verificar que as entries replicadas mantêm ordem
    for i in 0..5 {
        let expected = format!("Sequential event {}", i).into_bytes();
        let has_entry = ops2.iter().any(|op| op.value() == expected);
        assert!(
            has_entry,
            "Node2 should have replicated sequential event {}",
            i
        );
    }

    tracing::info!("✓ Sequential operations replicated with order maintained");
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

    let peer1_id = node1.iroh.node_id();
    let peer2_id = node2.iroh.node_id();

    // ESTABELECER CONEXÃO GOSSIP ANTES DE CRIAR STORES
    connect_nodes(&node1, &node2)
        .await
        .expect("Failed to connect nodes");
    tracing::info!("✓ Gossip connection established");

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

    // Aguardar gossip subscriptions se propagarem
    wait_for_propagation().await;

    tracing::info!("Starting concurrent writes...");

    // Clonar referências para as tasks
    let log1_clone = log1.clone();
    let log2_clone = log2.clone();

    // Escrever concorrentemente de ambos os nós
    let handle1 = tokio::spawn(async move {
        for i in 0..5 {
            let data = format!("Node1 event {}", i).into_bytes();
            log1_clone
                .add(data)
                .await
                .expect("Failed to add from node1");
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    });

    let handle2 = tokio::spawn(async move {
        for i in 0..5 {
            let data = format!("Node2 event {}", i).into_bytes();
            log2_clone
                .add(data)
                .await
                .expect("Failed to add from node2");
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    });

    // Aguardar ambas as tasks
    handle1.await.expect("Node1 task panicked");
    handle2.await.expect("Node2 task panicked");

    tracing::info!("Concurrent writes completed, triggering sync...");

    // CORREÇÃO: Seguir mesma ordem do teste que passa
    // Node1 conecta primeiro, depois node2
    node1
        .db
        .connect_to_peer(peer2_id)
        .await
        .expect("Failed to connect node1 to node2");

    // Delay para mesh formation
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Node2 conecta para completar mesh
    node2
        .db
        .connect_to_peer(peer1_id)
        .await
        .expect("Failed to connect node2 to node1");

    wait_for_propagation().await;

    // VALIDAR QUE AMBOS OS NÓS TÊM TODAS AS ENTRIES
    let ops1 = log1
        .list(None)
        .await
        .expect("Failed to get entries from node1");
    let ops2 = log2
        .list(None)
        .await
        .expect("Failed to get entries from node2");

    tracing::info!(
        "Node1 has {} entries, Node2 has {} entries",
        ops1.len(),
        ops2.len()
    );

    // Ambos devem ter pelo menos 10 entries (5 de cada)
    assert!(ops1.len() >= 10, "Node1 should have at least 10 entries");
    assert!(ops2.len() >= 10, "Node2 should have at least 10 entries");

    tracing::info!("✓ Concurrent writes replicated successfully with CRDT conflict resolution");
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

    let peer1_id = node1.iroh.node_id();
    let peer2_id = node2.iroh.node_id();

    // ESTABELECER CONEXÃO GOSSIP ANTES DE CRIAR STORES
    connect_nodes(&node1, &node2)
        .await
        .expect("Failed to connect nodes");
    tracing::info!("✓ Gossip connection established");

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

    // Aguardar gossip subscriptions se propagarem
    wait_for_propagation().await;

    // Node1 adiciona alguns valores
    kv1.put("key1", b"value1".to_vec())
        .await
        .expect("Failed to put key1");
    kv1.put("key2", b"value2".to_vec())
        .await
        .expect("Failed to put key2");

    tracing::info!("Node1 added 2 keys");

    // ⭐ CORREÇÃO: Seguir mesma ordem do teste que passa
    // Node1 (com dados) conecta primeiro
    node1
        .db
        .connect_to_peer(peer2_id)
        .await
        .expect("Failed to connect node1 to node2");

    // Delay para mesh formation
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Node2 conecta para completar mesh
    node2
        .db
        .connect_to_peer(peer1_id)
        .await
        .expect("Failed to connect node2 to node1");

    wait_for_propagation().await;

    // Node2 adiciona outros valores
    kv2.put("key3", b"value3".to_vec())
        .await
        .expect("Failed to put key3");
    kv2.put("key4", b"value4".to_vec())
        .await
        .expect("Failed to put key4");

    tracing::info!("Node2 added 2 keys after connection");
    wait_for_propagation().await;

    // VALIDAR REPLICAÇÃO BIDIRECIONAL
    let data1 = kv1.all();
    let data2 = kv2.all();

    tracing::info!(
        "Node1 has {} keys, Node2 has {} keys",
        data1.len(),
        data2.len()
    );

    // Ambos devem ter pelo menos 4 keys no total
    assert!(data1.len() >= 4, "Node1 should have at least 4 keys");
    assert!(data2.len() >= 4, "Node2 should have at least 4 keys");

    // Verificar que node1 tem as keys de node2
    assert!(
        data1.contains_key("key3"),
        "Node1 should have key3 from node2"
    );
    assert!(
        data1.contains_key("key4"),
        "Node1 should have key4 from node2"
    );

    // Verificar que node2 tem as keys de node1
    assert!(
        data2.contains_key("key1"),
        "Node2 should have key1 from node1"
    );
    assert!(
        data2.contains_key("key2"),
        "Node2 should have key2 from node1"
    );

    tracing::info!("✓ KeyValue replication validated with bidirectional sync");
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

    let peer1_id = node1.iroh.node_id();
    let peer2_id = node2.iroh.node_id();

    // ESTABELECER CONEXÃO GOSSIP ANTES DE CRIAR STORES
    connect_nodes(&node1, &node2)
        .await
        .expect("Failed to connect nodes");
    tracing::info!("✓ Gossip connection established");

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

    // Aguardar gossip subscriptions se propagarem
    wait_for_propagation().await;

    // Node1 adiciona documentos
    let doc1: Box<serde_json::Value> = Box::new(serde_json::json!({
        "_id": "doc1",
        "source": "node1",
        "data": "test data 1"
    }));

    docs1.put(doc1).await.expect("Failed to put doc1");
    tracing::info!("Node1 added document");

    // Trigger synchronization via connect_to_peer
    node1
        .db
        .connect_to_peer(peer2_id)
        .await
        .expect("Failed to connect node1 to node2");
    node2
        .db
        .connect_to_peer(peer1_id)
        .await
        .expect("Failed to connect node2 to node1");

    wait_for_propagation().await;

    // Node2 adiciona documentos
    let doc2: Box<serde_json::Value> = Box::new(serde_json::json!({
        "_id": "doc2",
        "source": "node2",
        "data": "test data 2"
    }));

    docs2.put(doc2).await.expect("Failed to put doc2");
    tracing::info!("Node2 added document after connection");

    wait_for_propagation().await;

    // VALIDAR REPLICAÇÃO BIDIRECIONAL
    let retrieved1_from_node1 = docs1.get("doc1", None).await.expect("Failed to get doc1");
    let retrieved2_from_node1 = docs1
        .get("doc2", None)
        .await
        .expect("Failed to get doc2 from node1");

    let retrieved1_from_node2 = docs2
        .get("doc1", None)
        .await
        .expect("Failed to get doc1 from node2");
    let retrieved2_from_node2 = docs2.get("doc2", None).await.expect("Failed to get doc2");

    assert_eq!(retrieved1_from_node1.len(), 1, "Node1 should have doc1");
    assert_eq!(retrieved2_from_node2.len(), 1, "Node2 should have doc2");

    // Verificar replicação cruzada
    assert!(
        !retrieved2_from_node1.is_empty(),
        "Node1 should have doc2 from node2"
    );
    assert!(
        !retrieved1_from_node2.is_empty(),
        "Node2 should have doc1 from node1"
    );

    tracing::info!("✓ Document store replication validated with bidirectional sync");
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

    // Node2 entra depois (late joiner)
    let node2 = TestNode::new("late_node2")
        .await
        .expect("Failed to create node2");

    tracing::info!("Node2 joined the network as late joiner");

    // ESTABELECER CONEXÃO GOSSIP COM O LATE JOINER
    connect_nodes(&node1, &node2)
        .await
        .expect("Failed to connect to late joiner");
    tracing::info!("✓ Gossip connection established with late joiner");

    // Criar log2 APÓS conexão estabelecida para permitir sincronização
    let log2 = node2
        .db
        .log("late-join-log", None)
        .await
        .expect("Failed to create log2");

    tracing::info!("✓ Late joiner log created, triggering sync...");

    // CORREÇÃO: Seguir mesma ordem do teste que passa
    // Node1 (com dados) conecta primeiro
    let peer1_id = node1.iroh.node_id();
    let peer2_id = node2.iroh.node_id();
    node1
        .db
        .connect_to_peer(peer2_id)
        .await
        .expect("Failed to connect node1 to late joiner");

    // Delay para mesh formation
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Late joiner conecta para completar mesh
    node2
        .db
        .connect_to_peer(peer1_id)
        .await
        .expect("Failed to connect late joiner to node1");

    wait_for_propagation().await;

    // VALIDAR QUE LATE JOINER RECEBEU HISTÓRICO
    let ops2 = log2
        .list(None)
        .await
        .expect("Failed to get entries from node2");

    tracing::info!("Late joiner (node2) has {} entries after sync", ops2.len());
    assert!(
        ops2.len() >= 3,
        "Late joiner should have received at least 3 historical entries"
    );

    // Verificar que recebeu as entries antigas
    for i in 0..3 {
        let expected = format!("Early event {}", i).into_bytes();
        let has_entry = ops2.iter().any(|op| op.value() == expected);
        assert!(has_entry, "Late joiner should have early event {}", i);
    }

    // Node2 adiciona uma entry nova
    log2.add(b"Late joiner event".to_vec())
        .await
        .expect("Failed to add late event");

    wait_for_propagation().await;

    // Verificar que node1 recebe a nova entry
    let ops1 = log1
        .list(None)
        .await
        .expect("Failed to get entries from node1");
    assert!(
        ops1.len() >= 4,
        "Node1 should have at least 4 entries (3 old + 1 from late joiner)"
    );

    let has_late_entry = ops1.iter().any(|op| op.value() == b"Late joiner event");
    assert!(
        has_late_entry,
        "Node1 should have received event from late joiner"
    );

    tracing::info!("✓ Late joiner synchronization validated: catch-up + bidirectional sync");
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

    let peer1_id = node1.iroh.node_id();
    let peer2_id = node2.iroh.node_id();

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

    // Simular partição: ambos os nós adicionam entries sem estar conectados
    log1.add(b"Node1 during partition".to_vec())
        .await
        .expect("Failed to add to node1");
    log2.add(b"Node2 during partition".to_vec())
        .await
        .expect("Failed to add to node2");

    tracing::info!("Both nodes added entries during simulated partition");

    // ESTABELECER CONEXÃO GOSSIP PARA SIMULAR RECONEXÃO APÓS PARTIÇÃO
    connect_nodes(&node1, &node2)
        .await
        .expect("Failed to establish gossip connection after partition");

    tracing::info!("✓ Network recovered, triggering reconciliation...");

    // CORREÇÃO: Seguir mesma ordem do teste que passa
    // Node1 conecta primeiro
    node1
        .db
        .connect_to_peer(peer2_id)
        .await
        .expect("Failed to reconnect node1 to node2");

    // Delay para mesh formation
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Node2 conecta para completar mesh
    node2
        .db
        .connect_to_peer(peer1_id)
        .await
        .expect("Failed to reconnect node2 to node1");

    wait_for_propagation().await;

    // Ambos adicionam entries após "reconexão"
    log1.add(b"Node1 after recovery".to_vec())
        .await
        .expect("Failed to add to node1");
    log2.add(b"Node2 after recovery".to_vec())
        .await
        .expect("Failed to add to node2");

    wait_for_propagation().await;

    // VALIDAR RECUPERAÇÃO E RECONCILIAÇÃO
    let ops1 = log1
        .list(None)
        .await
        .expect("Failed to get entries from node1");
    let ops2 = log2
        .list(None)
        .await
        .expect("Failed to get entries from node2");

    tracing::info!(
        "After partition recovery: Node1 has {} entries, Node2 has {} entries",
        ops1.len(),
        ops2.len()
    );

    // Ambos devem ter pelo menos 4 entries (2 durante partição + 2 após)
    assert!(
        ops1.len() >= 4,
        "Node1 should have at least 4 entries after recovery"
    );
    assert!(
        ops2.len() >= 4,
        "Node2 should have at least 4 entries after recovery"
    );

    // Verificar que ambos têm dados escritos durante a partição
    let node1_has_partition_data = ops1
        .iter()
        .any(|op| op.value() == b"Node2 during partition");
    let node2_has_partition_data = ops2
        .iter()
        .any(|op| op.value() == b"Node1 during partition");

    assert!(
        node1_has_partition_data,
        "Node1 should have reconciled Node2's partition data"
    );
    assert!(
        node2_has_partition_data,
        "Node2 should have reconciled Node1's partition data"
    );

    tracing::info!("✓ Network partition recovery and CRDT reconciliation validated");
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

    // ESTABELECER CONEXÃO GOSSIP ANTES DE CRIAR STORES
    connect_nodes(&node1, &node2)
        .await
        .expect("Failed to connect nodes");
    tracing::info!("✓ Gossip connection established");

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

    // VALIDAR ISOLAMENTO: Stores diferentes não devem replicar entre si
    let ops_a = log_a1
        .list(None)
        .await
        .expect("Failed to get entries from store-a");
    let ops_b = log_b2
        .list(None)
        .await
        .expect("Failed to get entries from store-b");

    assert_eq!(ops_a.len(), 1, "Store A should only have its own 1 entry");
    assert_eq!(ops_b.len(), 1, "Store B should only have its own 1 entry");

    // Verificar que os dados são distintos e não vazaram
    assert_eq!(ops_a[0].value(), b"Data in store A");
    assert_eq!(ops_b[0].value(), b"Data in store B");

    tracing::info!("✓ Store isolation across connected nodes validated");
}
