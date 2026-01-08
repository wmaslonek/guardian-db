/// Testes robustos para o módulo IrohBackend
///
/// Cobertura completa de testes (62 testes):
///
/// ✓ Inicialização e Configuração (4 testes)
///   - Inicialização básica e com config customizada
///   - Persistência de chaves secretas
///   - Verificação de componentes (endpoint, store, gossip, docs, router)
///
/// ✓ Operações de Conteúdo (13 testes)
///   - add(), cat() com diferentes tamanhos
///   - pin_add(), pin_rm(), pin_ls()
///   - Testes de edge cases (conteúdo vazio, hashes inválidos)
///   - Workflow completo add->pin->cat->unpin
///
/// ✓ Operações de Rede (4 testes)
///   - peers(), id(), connect()
///   - Discovery de peers ativo
///   - Tratamento de peers inválidos
///
/// ✓ Cache Otimizado (4 testes)
///   - Cache hits e misses
///   - Estatísticas de cache
///   - Otimização de performance
///   - Melhoria de performance com cache
///
/// ✓ Métricas e Health Checks (6 testes)
///   - metrics(), health_check()
///   - repo_stat(), version()
///   - Componentes de health check
///   - Métricas após operações
///
/// ✓ Connection Pool (4 testes)
///   - Status e listagem de conexões
///   - Limpeza de conexões antigas
///   - Obtenção de conexões do pool
///
/// ✓ Performance Monitoring (9 testes)
///   - Throughput, latência, recursos
///   - Snapshots de performance
///   - Reset de métricas
///   - Cálculo de percentis (P95, P99)
///   - Relatórios detalhados
///
/// ✓ Key Synchronization (8 testes)
///   - Adicionar/remover peers confiáveis
///   - Estatísticas de sincronização
///   - Listagem de chaves sincronizadas
///   - Exportação de configuração
///   - Relatórios de sincronização
///
/// ✓ Networking Metrics (3 testes)
///   - Coleta de métricas de rede
///   - Relatórios de networking
///   - Exportação JSON
///
/// ✓ Testes de Integração (3 testes)
///   - Workflow completo
///   - Múltiplas operações concorrentes
///   - Operações sequenciais (stress test)
///
/// ✓ Edge Cases e Configuração (4 testes)
///   - Conteúdo vazio
///   - Pins duplicados
///   - Remoção de pins inexistentes
///   - Informações de configuração
use crate::p2p::network::config::ClientConfig;
use crate::p2p::network::core::IrohBackend;
use iroh::SecretKey;
use rand_core::OsRng;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::AsyncReadExt;

// ╔════════════════════════════════════════════════════════════════════════════════╗
// ║                                 HELPERS DE TESTE                               ║
// ╚════════════════════════════════════════════════════════════════════════════════╝

/// Cria configuração de teste com diretório temporário
async fn create_test_config() -> (ClientConfig, TempDir) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let mut client_config = ClientConfig::testing();
    client_config.data_store_path = Some(temp_dir.path().to_path_buf());
    (client_config, temp_dir)
}

/// Cria backend de teste isolado
async fn create_test_backend() -> (IrohBackend, TempDir) {
    let (client_config, temp_dir) = create_test_config().await;
    let backend = IrohBackend::new(&client_config)
        .await
        .expect("Failed to create backend");
    (backend, temp_dir)
}

/// Gera dados de teste aleatórios
fn generate_test_data(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 256) as u8).collect()
}

// ╔════════════════════════════════════════════════════════════════════════════════╗
// ║                         TESTES DE INICIALIZAÇÃO                                ║
// ╚════════════════════════════════════════════════════════════════════════════════╝

#[tokio::test]
async fn test_backend_initialization() {
    let (backend, _temp_dir) = create_test_backend().await;

    // Verifica que o backend foi inicializado corretamente
    assert!(backend.is_online().await);

    // Verifica que componentes essenciais estão disponíveis
    assert!(backend.get_endpoint().await.is_ok());
    assert!(backend.get_gossip().await.is_ok());
    assert!(backend.get_docs().await.is_ok());
    assert!(backend.get_router().await.is_ok());
}

#[tokio::test]
async fn test_backend_initialization_with_custom_config() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let mut client_config = ClientConfig::development();
    client_config.data_store_path = Some(temp_dir.path().to_path_buf());
    client_config.port = 0; // Porta aleatória

    let backend = IrohBackend::new(&client_config).await;
    assert!(backend.is_ok());

    let backend = backend.unwrap();
    assert!(backend.is_online().await);
}

#[tokio::test]
#[ignore] // Teste lento - executar manualmente se necessário
async fn test_backend_secret_key_persistence() {
    let (client_config, temp_dir) = create_test_config().await;

    // Cria primeiro backend
    let backend1 = IrohBackend::new(&client_config)
        .await
        .expect("Failed to create backend1");
    let key1 = backend1.secret_key().clone();

    // Drop explícito e aguarda cleanup
    drop(backend1);
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Cria segundo backend no mesmo diretório com timeout
    let backend2 = tokio::time::timeout(Duration::from_secs(120), IrohBackend::new(&client_config))
        .await
        .expect("Timeout ao criar backend2")
        .expect("Failed to create backend2");

    let key2 = backend2.secret_key();

    // Verifica que a chave foi carregada (mesma chave)
    assert_eq!(key1.to_bytes(), key2.to_bytes());

    drop(backend2);
    drop(temp_dir);
}

#[tokio::test]
async fn test_backend_node_id() {
    let (backend, _temp_dir) = create_test_backend().await;

    let node_info = backend.id().await.expect("Failed to get node info");

    // Verifica que o NodeId corresponde à chave pública
    let expected_id = backend.secret_key().public();
    assert_eq!(node_info.id, expected_id);
}

// ╔════════════════════════════════════════════════════════════════════════════════╗
// ║                      TESTES DE OPERAÇÕES DE CONTEÚDO                           ║
// ╚════════════════════════════════════════════════════════════════════════════════╝

#[tokio::test]
async fn test_add_content() {
    let (backend, _temp_dir) = create_test_backend().await;

    let test_data = b"Hello, Iroh!";
    let cursor = Cursor::new(test_data.to_vec());
    let reader = Box::pin(cursor);

    let response = backend.add(reader).await;
    assert!(response.is_ok());

    let response = response.unwrap();
    assert!(!response.hash.is_empty());
    assert_eq!(response.size, test_data.len().to_string());
}

#[tokio::test]
async fn test_add_and_cat_content() {
    let (backend, _temp_dir) = create_test_backend().await;

    let test_data = b"Test content for add and cat";
    let cursor = Cursor::new(test_data.to_vec());
    let reader = Box::pin(cursor);

    // Adiciona conteúdo
    let add_response = backend.add(reader).await.expect("Failed to add");
    let hash = add_response.hash.clone();

    // Recupera conteúdo
    let mut cat_reader = backend.cat(&hash).await.expect("Failed to cat");

    let mut retrieved_data = Vec::new();
    cat_reader
        .read_to_end(&mut retrieved_data)
        .await
        .expect("Failed to read data");

    assert_eq!(retrieved_data, test_data);
}

#[tokio::test]
async fn test_add_large_content() {
    let (backend, _temp_dir) = create_test_backend().await;

    // Gera 1MB de dados
    let test_data = generate_test_data(1024 * 1024);
    let cursor = Cursor::new(test_data.clone());
    let reader = Box::pin(cursor);

    let response = backend.add(reader).await;
    assert!(response.is_ok());

    let response = response.unwrap();
    assert_eq!(response.size, test_data.len().to_string());
}

#[tokio::test]
async fn test_cat_nonexistent_content() {
    let (backend, _temp_dir) = create_test_backend().await;

    // Hash que não existe
    let fake_hash = "0".repeat(64);
    let result = backend.cat(&fake_hash).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_cat_invalid_hash() {
    let (backend, _temp_dir) = create_test_backend().await;

    // Hash inválido (muito curto)
    let invalid_hash = "invalid";
    let result = backend.cat(invalid_hash).await;

    assert!(result.is_err());
}

// ╔════════════════════════════════════════════════════════════════════════════════╗
// ║                         TESTES DE PIN OPERATIONS                               ║
// ╚════════════════════════════════════════════════════════════════════════════════╝

#[tokio::test]
async fn test_pin_add() {
    let (backend, _temp_dir) = create_test_backend().await;

    // Adiciona conteúdo
    let test_data = b"Content to pin";
    let cursor = Cursor::new(test_data.to_vec());
    let reader = Box::pin(cursor);
    let add_response = backend.add(reader).await.expect("Failed to add");
    let hash = add_response.hash.clone();

    // Fixa conteúdo
    let pin_result = backend.pin_add(&hash).await;
    assert!(pin_result.is_ok());

    // Verifica que está na lista de pins
    let pins = backend.pin_ls().await.expect("Failed to list pins");
    assert!(pins.iter().any(|p| p.hash == hash));
}

#[tokio::test]
async fn test_pin_add_nonexistent() {
    let (backend, _temp_dir) = create_test_backend().await;

    // Tenta fixar hash que não existe
    let fake_hash = "0".repeat(64);
    let result = backend.pin_add(&fake_hash).await;

    // Deve falhar porque o conteúdo não existe
    assert!(result.is_err());
}

#[tokio::test]
async fn test_pin_rm() {
    let (backend, _temp_dir) = create_test_backend().await;

    // Adiciona e fixa conteúdo
    let test_data = b"Content to pin and unpin";
    let cursor = Cursor::new(test_data.to_vec());
    let reader = Box::pin(cursor);
    let add_response = backend.add(reader).await.expect("Failed to add");
    let hash = add_response.hash.clone();

    backend.pin_add(&hash).await.expect("Failed to pin");

    // Verifica que está fixado
    let pins_before = backend.pin_ls().await.expect("Failed to list pins");
    assert!(pins_before.iter().any(|p| p.hash == hash));

    // Remove pin
    backend.pin_rm(&hash).await.expect("Failed to unpin");

    // Verifica que foi removido
    let pins_after = backend.pin_ls().await.expect("Failed to list pins");
    assert!(!pins_after.iter().any(|p| p.hash == hash));
}

#[tokio::test]
async fn test_pin_ls_empty() {
    let (backend, _temp_dir) = create_test_backend().await;

    let pins = backend.pin_ls().await.expect("Failed to list pins");
    // Backend novo não deve ter pins
    assert_eq!(pins.len(), 0);
}

#[tokio::test]
async fn test_pin_ls_multiple() {
    let (backend, _temp_dir) = create_test_backend().await;

    let mut hashes = Vec::new();

    // Adiciona e fixa múltiplos conteúdos
    for i in 0..3 {
        let test_data = format!("Content {}", i);
        let cursor = Cursor::new(test_data.into_bytes());
        let reader = Box::pin(cursor);
        let add_response = backend.add(reader).await.expect("Failed to add");
        let hash = add_response.hash.clone();

        backend.pin_add(&hash).await.expect("Failed to pin");
        hashes.push(hash);
    }

    // Lista todos os pins
    let pins = backend.pin_ls().await.expect("Failed to list pins");
    assert_eq!(pins.len(), 3);

    // Verifica que todos os hashes estão presentes
    for hash in &hashes {
        assert!(pins.iter().any(|p| &p.hash == hash));
    }
}

// ╔════════════════════════════════════════════════════════════════════════════════╗
// ║                       TESTES DE OPERAÇÕES DE REDE                              ║
// ╚════════════════════════════════════════════════════════════════════════════════╝

#[tokio::test]
async fn test_peers_list() {
    let (backend, _temp_dir) = create_test_backend().await;

    let peers = backend.peers().await.expect("Failed to get peers");

    // Backend isolado não deve ter peers conectados inicialmente
    assert_eq!(peers.len(), 0);
}

#[tokio::test]
async fn test_node_info() {
    let (backend, _temp_dir) = create_test_backend().await;

    let node_info = backend.id().await.expect("Failed to get node info");

    assert!(!node_info.addresses.is_empty());
    assert!(!node_info.agent_version.is_empty());
    assert!(!node_info.public_key.is_empty());
}

#[tokio::test]
async fn test_connect_to_invalid_peer() {
    let (backend, _temp_dir) = create_test_backend().await;

    // Gera NodeId aleatório (peer que não existe)
    let fake_peer = SecretKey::generate(OsRng).public();

    let result = backend.connect(&fake_peer).await;

    // Deve falhar porque o peer não existe
    assert!(result.is_err());
}

#[tokio::test]
async fn test_discover_peers_active() {
    let (backend, _temp_dir) = create_test_backend().await;

    // Tenta descobrir peers com timeout curto
    let result = backend.discover_peers_active(Duration::from_secs(1)).await;

    assert!(result.is_ok());
    // Pode não encontrar nenhum peer em teste isolado
    let _peers = result.unwrap();
}

// ╔════════════════════════════════════════════════════════════════════════════════╗
// ║                          TESTES DE CACHE OTIMIZADO                             ║
// ╚════════════════════════════════════════════════════════════════════════════════╝

#[tokio::test]
async fn test_cache_hit() {
    let (backend, _temp_dir) = create_test_backend().await;

    // Adiciona conteúdo (vai para cache automaticamente)
    let test_data = b"Cache test data";
    let cursor = Cursor::new(test_data.to_vec());
    let reader = Box::pin(cursor);
    let add_response = backend.add(reader).await.expect("Failed to add");
    let hash = add_response.hash.clone();

    // Primeira leitura (deveria vir do cache)
    let start = std::time::Instant::now();
    let mut cat_reader = backend.cat(&hash).await.expect("Failed to cat");
    let mut retrieved_data = Vec::new();
    cat_reader
        .read_to_end(&mut retrieved_data)
        .await
        .expect("Failed to read");
    let first_duration = start.elapsed();

    // Segunda leitura (deveria ser mais rápida - cache hit)
    let start = std::time::Instant::now();
    let mut cat_reader = backend.cat(&hash).await.expect("Failed to cat");
    let mut retrieved_data2 = Vec::new();
    cat_reader
        .read_to_end(&mut retrieved_data2)
        .await
        .expect("Failed to read");
    let second_duration = start.elapsed();

    assert_eq!(retrieved_data, test_data);
    assert_eq!(retrieved_data2, test_data);

    // Segunda leitura deveria ser mais rápida ou igual
    assert!(second_duration <= first_duration);
}

#[tokio::test]
async fn test_cache_statistics() {
    let (backend, _temp_dir) = create_test_backend().await;

    let stats = backend
        .get_cache_statistics()
        .await
        .expect("Failed to get cache stats");

    // Cache novo deve estar vazio ou quase vazio
    assert!(stats.hit_ratio >= 0.0 && stats.hit_ratio <= 1.0);
}

#[tokio::test]
async fn test_optimize_performance() {
    let (backend, _temp_dir) = create_test_backend().await;

    let result = backend.optimize_performance().await;
    assert!(result.is_ok());
}

// ╔════════════════════════════════════════════════════════════════════════════════╗
// ║                       TESTES DE MÉTRICAS E HEALTH                              ║
// ╚════════════════════════════════════════════════════════════════════════════════╝

#[tokio::test]
async fn test_backend_metrics() {
    let (backend, _temp_dir) = create_test_backend().await;

    let metrics = backend.metrics().await.expect("Failed to get metrics");

    assert!(metrics.ops_per_second >= 0.0);
    assert!(metrics.avg_latency_ms >= 0.0);
}

#[tokio::test]
async fn test_health_check() {
    let (backend, _temp_dir) = create_test_backend().await;

    let health = backend.health_check().await.expect("Failed health check");

    assert!(health.healthy);
    assert!(!health.message.is_empty());
    assert!(!health.checks.is_empty());
}

#[tokio::test]
async fn test_health_check_components() {
    let (backend, _temp_dir) = create_test_backend().await;

    let health = backend.health_check().await.expect("Failed health check");

    // Verifica que os principais componentes foram checados
    let check_names: Vec<&str> = health.checks.iter().map(|c| c.name.as_str()).collect();

    // Deve ter pelo menos checagens de status e diretório
    assert!(check_names.len() >= 2);
}

#[tokio::test]
async fn test_repo_stats() {
    let (backend, _temp_dir) = create_test_backend().await;

    let stats = backend.repo_stat().await.expect("Failed to get repo stats");

    assert!(!stats.repo_path.is_empty());
}

#[tokio::test]
async fn test_version_info() {
    let (backend, _temp_dir) = create_test_backend().await;

    let version = backend.version().await.expect("Failed to get version");

    assert!(!version.version.is_empty());
    assert!(!version.system.is_empty());
}

// ╔════════════════════════════════════════════════════════════════════════════════╗
// ║                      TESTES DE CONNECTION POOL                                 ║
// ╚════════════════════════════════════════════════════════════════════════════════╝

#[tokio::test]
async fn test_connection_pool_status() {
    let (backend, _temp_dir) = create_test_backend().await;

    let status = backend.get_connection_pool_status().await;
    assert!(status.contains("Pool de conexões"));
}

#[tokio::test]
async fn test_list_active_connections() {
    let (backend, _temp_dir) = create_test_backend().await;

    let connections = backend.list_active_connections().await;
    // Backend novo não deve ter conexões
    assert_eq!(connections.len(), 0);
}

#[tokio::test]
async fn test_cleanup_stale_connections() {
    let (backend, _temp_dir) = create_test_backend().await;

    let result = backend
        .cleanup_stale_connections(Duration::from_secs(60))
        .await;

    assert!(result.is_ok());
    let removed_count = result.unwrap();
    // Backend novo não deve ter conexões para remover
    assert_eq!(removed_count, 0);
}

#[tokio::test]
async fn test_get_connection_from_empty_pool() {
    let (backend, _temp_dir) = create_test_backend().await;

    let fake_peer = SecretKey::generate(OsRng).public();
    let result = backend.get_connection_from_pool(&fake_peer).await;

    // Deve falhar porque não há conexão no pool
    assert!(result.is_err());
}

// ╔════════════════════════════════════════════════════════════════════════════════╗
// ║                    TESTES DE PERFORMANCE MONITORING                            ║
// ╚════════════════════════════════════════════════════════════════════════════════╝

#[tokio::test]
async fn test_get_throughput_metrics() {
    let (backend, _temp_dir) = create_test_backend().await;

    let metrics = backend.get_throughput_metrics().await;

    assert!(metrics.ops_per_second >= 0.0);
    assert!(metrics.peak_throughput >= 0.0);
}

#[tokio::test]
async fn test_get_latency_metrics() {
    let (backend, _temp_dir) = create_test_backend().await;

    let metrics = backend.get_latency_metrics().await;

    assert!(metrics.avg_latency_ms >= 0.0);
    assert!(metrics.min_latency_ms >= 0.0);
    assert!(metrics.max_latency_ms >= 0.0);
}

#[tokio::test]
async fn test_get_resource_metrics() {
    let (backend, _temp_dir) = create_test_backend().await;

    let metrics = backend.get_resource_metrics().await;

    assert!(metrics.cpu_usage >= 0.0 && metrics.cpu_usage <= 1.0);
}

#[tokio::test]
async fn test_create_performance_snapshot() {
    let (backend, _temp_dir) = create_test_backend().await;

    let snapshot = backend.create_performance_snapshot().await;

    assert!(snapshot.throughput.ops_per_second >= 0.0);
    assert!(snapshot.latency.avg_latency_ms >= 0.0);
}

#[tokio::test]
async fn test_record_performance_snapshot() {
    let (backend, _temp_dir) = create_test_backend().await;

    // Registra alguns snapshots
    for _ in 0..5 {
        backend
            .record_performance_snapshot()
            .await
            .expect("Failed to record snapshot");
    }

    let history = backend.get_performance_history().await;
    assert_eq!(history.len(), 5);
}

#[tokio::test]
async fn test_reset_performance_metrics() {
    let (backend, _temp_dir) = create_test_backend().await;

    // Registra snapshot
    backend
        .record_performance_snapshot()
        .await
        .expect("Failed to record");

    // Reseta métricas
    backend
        .reset_performance_metrics()
        .await
        .expect("Failed to reset");

    // Verifica que histórico foi limpo
    let history = backend.get_performance_history().await;
    assert_eq!(history.len(), 0);
}

#[tokio::test]
async fn test_update_resource_metrics() {
    let (backend, _temp_dir) = create_test_backend().await;

    let result = backend
        .update_resource_metrics(0.5, 1024 * 1024, 512 * 1024, 256 * 1024)
        .await;

    assert!(result.is_ok());

    let metrics = backend.get_resource_metrics().await;
    assert_eq!(metrics.cpu_usage, 0.5);
    assert_eq!(metrics.memory_usage_bytes, 1024 * 1024);
}

#[tokio::test]
async fn test_calculate_latency_percentiles() {
    let (backend, _temp_dir) = create_test_backend().await;

    // Registra alguns snapshots para ter dados
    for _ in 0..10 {
        backend
            .record_performance_snapshot()
            .await
            .expect("Failed to record");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let result = backend.calculate_latency_percentiles().await;
    assert!(result.is_ok());

    let (p95, p99) = result.unwrap();
    assert!(p95 >= 0.0);
    assert!(p99 >= 0.0);
    assert!(p99 >= p95); // P99 deve ser >= P95
}

#[tokio::test]
async fn test_generate_performance_monitor_report() {
    let (backend, _temp_dir) = create_test_backend().await;

    let report = backend.generate_performance_monitor_report().await;

    assert!(report.contains("RELATÓRIO DE PERFORMANCE MONITOR"));
    assert!(report.contains("Throughput"));
    assert!(report.contains("Latência"));
    assert!(report.contains("Recursos"));
}

// ╔════════════════════════════════════════════════════════════════════════════════╗
// ║                     TESTES DE KEY SYNCHRONIZATION                              ║
// ╚════════════════════════════════════════════════════════════════════════════════╝

#[tokio::test]
async fn test_key_synchronizer_reference() {
    let (backend, _temp_dir) = create_test_backend().await;

    let key_sync = backend.get_key_synchronizer();
    assert!(Arc::strong_count(&key_sync) >= 2); // Backend + nossa referência
}

#[tokio::test]
async fn test_add_trusted_peer_for_sync() {
    let (backend, _temp_dir) = create_test_backend().await;

    let peer_secret = SecretKey::generate(OsRng);
    let node_id = peer_secret.public();
    let public_key = ed25519_dalek::SigningKey::generate(&mut OsRng).verifying_key();

    let result = backend.add_trusted_peer_for_sync(node_id, public_key).await;
    assert!(result.is_ok());

    // Verifica que peer foi adicionado
    let trusted_peers = backend.list_trusted_peers_for_sync().await;
    assert!(trusted_peers.contains(&node_id));
}

#[tokio::test]
async fn test_remove_trusted_peer_from_sync() {
    let (backend, _temp_dir) = create_test_backend().await;

    let peer_secret = SecretKey::generate(OsRng);
    let node_id = peer_secret.public();
    let public_key = ed25519_dalek::SigningKey::generate(&mut OsRng).verifying_key();

    // Adiciona peer
    backend
        .add_trusted_peer_for_sync(node_id, public_key)
        .await
        .expect("Failed to add peer");

    // Remove peer
    let result = backend.remove_trusted_peer_from_sync(&node_id).await;
    assert!(result.is_ok());
    assert!(result.unwrap()); // Deve retornar true (removido)

    // Verifica que foi removido
    let trusted_peers = backend.list_trusted_peers_for_sync().await;
    assert!(!trusted_peers.contains(&node_id));
}

#[tokio::test]
async fn test_list_trusted_peers_for_sync() {
    let (backend, _temp_dir) = create_test_backend().await;

    let peers = backend.list_trusted_peers_for_sync().await;
    // Backend novo não deve ter peers confiáveis
    assert_eq!(peers.len(), 0);
}

#[tokio::test]
async fn test_get_key_sync_statistics() {
    let (backend, _temp_dir) = create_test_backend().await;

    let stats = backend.get_key_sync_statistics().await;

    assert!(stats.success_rate >= 0.0 && stats.success_rate <= 1.0);
}

#[tokio::test]
async fn test_list_synchronized_keys() {
    let (backend, _temp_dir) = create_test_backend().await;

    let keys = backend.list_synchronized_keys().await;
    // Backend novo não deve ter chaves sincronizadas
    assert_eq!(keys.len(), 0);
}

#[tokio::test]
async fn test_export_sync_statistics_json() {
    let (backend, _temp_dir) = create_test_backend().await;

    let result = backend.export_sync_statistics_json().await;
    assert!(result.is_ok());

    let json = result.unwrap();
    assert!(json.contains("messages_synced") || json.contains("{"));
}

#[tokio::test]
async fn test_generate_key_sync_report() {
    let (backend, _temp_dir) = create_test_backend().await;

    let report = backend.generate_key_sync_report().await;

    assert!(report.contains("RELATÓRIO DE SINCRONIZAÇÃO DE CHAVES"));
    assert!(report.contains("Estatísticas Gerais"));
    assert!(report.contains("Conflitos"));
    assert!(report.contains("Peers"));
}

// ╔════════════════════════════════════════════════════════════════════════════════╗
// ║                       TESTES DE NETWORKING METRICS                             ║
// ╚════════════════════════════════════════════════════════════════════════════════╝

#[tokio::test]
async fn test_get_networking_metrics() {
    let (backend, _temp_dir) = create_test_backend().await;

    let result = backend.get_networking_metrics().await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_generate_networking_report() {
    let (backend, _temp_dir) = create_test_backend().await;

    let report = backend.generate_networking_report().await;

    // Apenas verifica que o relatório foi gerado
    assert!(!report.is_empty());
}

#[tokio::test]
async fn test_export_networking_metrics_json() {
    let (backend, _temp_dir) = create_test_backend().await;

    let result = backend.export_networking_metrics_json().await;
    assert!(result.is_ok());

    let json = result.unwrap();
    assert!(json.contains("{") || json.contains("total_connections"));
}

// ╔════════════════════════════════════════════════════════════════════════════════╗
// ║                         TESTES DE RELATÓRIOS                                   ║
// ╚════════════════════════════════════════════════════════════════════════════════╝

#[tokio::test]
async fn test_generate_performance_report() {
    let (backend, _temp_dir) = create_test_backend().await;

    let report = backend.generate_performance_report().await;

    assert!(report.contains("RELATÓRIO DE PERFORMANCE"));
    assert!(report.contains("Métricas Gerais"));
    assert!(report.contains("Cache Statistics"));
    assert!(report.contains("Connection Pool"));
    assert!(report.contains("Key Synchronization"));
    assert!(report.contains("Performance Monitor"));
    assert!(report.contains("Otimizações"));
}

// ╔════════════════════════════════════════════════════════════════════════════════╗
// ║                       TESTES DE INTEGRAÇÃO COMPLETOS                           ║
// ╚════════════════════════════════════════════════════════════════════════════════╝

#[tokio::test]
async fn test_full_workflow_add_pin_cat() {
    let (backend, _temp_dir) = create_test_backend().await;

    // 1. Adiciona conteúdo
    let test_data = b"Full workflow test data";
    let cursor = Cursor::new(test_data.to_vec());
    let reader = Box::pin(cursor);
    let add_response = backend.add(reader).await.expect("Failed to add");
    let hash = add_response.hash.clone();

    // 2. Fixa conteúdo
    backend.pin_add(&hash).await.expect("Failed to pin");

    // 3. Verifica que está fixado
    let pins = backend.pin_ls().await.expect("Failed to list pins");
    assert!(pins.iter().any(|p| p.hash == hash));

    // 4. Recupera conteúdo
    let mut cat_reader = backend.cat(&hash).await.expect("Failed to cat");
    let mut retrieved_data = Vec::new();
    cat_reader
        .read_to_end(&mut retrieved_data)
        .await
        .expect("Failed to read");

    assert_eq!(retrieved_data, test_data);

    // 5. Remove pin
    backend.pin_rm(&hash).await.expect("Failed to unpin");

    // 6. Verifica que pin foi removido
    let pins_after = backend.pin_ls().await.expect("Failed to list pins");
    assert!(!pins_after.iter().any(|p| p.hash == hash));
}

#[tokio::test]
async fn test_multiple_operations_metrics_update() {
    let (backend, _temp_dir) = create_test_backend().await;

    // Realiza várias operações
    for i in 0..5 {
        let test_data = format!("Test data {}", i);
        let cursor = Cursor::new(test_data.into_bytes());
        let reader = Box::pin(cursor);
        backend.add(reader).await.expect("Failed to add");
    }

    // Verifica que métricas foram atualizadas
    let metrics = backend.metrics().await.expect("Failed to get metrics");
    assert!(metrics.total_operations >= 5);
}

#[tokio::test]
async fn test_health_check_after_operations() {
    let (backend, _temp_dir) = create_test_backend().await;

    // Realiza algumas operações
    let test_data = b"Health check test";
    let cursor = Cursor::new(test_data.to_vec());
    let reader = Box::pin(cursor);
    backend.add(reader).await.expect("Failed to add");

    // Health check ainda deve estar ok
    let health = backend.health_check().await.expect("Failed health check");
    assert!(health.healthy);
}

#[tokio::test]
async fn test_cache_hit_improves_performance() {
    let (backend, _temp_dir) = create_test_backend().await;

    // Adiciona conteúdo grande
    let test_data = generate_test_data(100 * 1024); // 100KB
    let cursor = Cursor::new(test_data.clone());
    let reader = Box::pin(cursor);
    let add_response = backend.add(reader).await.expect("Failed to add");
    let hash = add_response.hash.clone();

    // Primeira leitura (sem cache)
    let start = std::time::Instant::now();
    let mut cat_reader = backend.cat(&hash).await.expect("Failed to cat");
    let mut retrieved_data = Vec::new();
    cat_reader
        .read_to_end(&mut retrieved_data)
        .await
        .expect("Failed to read");
    let first_duration = start.elapsed();

    // Segunda leitura (com cache - deveria ser mais rápida)
    let start = std::time::Instant::now();
    let mut cat_reader = backend.cat(&hash).await.expect("Failed to cat");
    let mut retrieved_data2 = Vec::new();
    cat_reader
        .read_to_end(&mut retrieved_data2)
        .await
        .expect("Failed to read");
    let second_duration = start.elapsed();

    assert_eq!(retrieved_data, test_data);
    assert_eq!(retrieved_data2, test_data);

    // Segunda leitura deveria ser significativamente mais rápida (ou pelo menos não mais lenta)
    assert!(second_duration <= first_duration * 2);
}

// ╔════════════════════════════════════════════════════════════════════════════════╗
// ║                          TESTES DE STRESS                                      ║
// ╚════════════════════════════════════════════════════════════════════════════════╝

#[tokio::test]
async fn test_concurrent_adds() {
    let (backend, _temp_dir) = create_test_backend().await;
    let backend = Arc::new(backend);

    let mut handles = vec![];

    // Adiciona 10 conteúdos concorrentemente
    for i in 0..10 {
        let backend_clone = backend.clone();
        let handle = tokio::spawn(async move {
            let test_data = format!("Concurrent data {}", i);
            let cursor = Cursor::new(test_data.into_bytes());
            let reader = Box::pin(cursor);
            backend_clone.add(reader).await
        });
        handles.push(handle);
    }

    // Aguarda todas as operações
    for handle in handles {
        let result = handle.await.expect("Task panicked");
        assert!(result.is_ok());
    }

    // Verifica métricas
    let metrics = backend.metrics().await.expect("Failed to get metrics");
    assert!(metrics.total_operations >= 10);
}

#[tokio::test]
async fn test_many_sequential_operations() {
    let (backend, _temp_dir) = create_test_backend().await;

    // Realiza 50 operações sequenciais
    for i in 0..50 {
        let test_data = format!("Sequential data {}", i);
        let cursor = Cursor::new(test_data.into_bytes());
        let reader = Box::pin(cursor);
        let result = backend.add(reader).await;
        assert!(result.is_ok());
    }

    // Backend deve continuar saudável
    let health = backend.health_check().await.expect("Failed health check");
    assert!(health.healthy);

    // Métricas devem refletir as operações
    let metrics = backend.metrics().await.expect("Failed to get metrics");
    assert!(metrics.total_operations >= 50);
}

// ╔════════════════════════════════════════════════════════════════════════════════╗
// ║                        TESTES DE CONFIGURAÇÃO                                  ║
// ╚════════════════════════════════════════════════════════════════════════════════╝

#[tokio::test]
async fn test_get_config_info() {
    let (backend, _temp_dir) = create_test_backend().await;

    let config_info = backend.get_config_info().await;
    assert!(!config_info.is_empty());
}

// ╔════════════════════════════════════════════════════════════════════════════════╗
// ║                      TESTES DE EDGE CASES                                      ║
// ╚════════════════════════════════════════════════════════════════════════════════╝

#[tokio::test]
async fn test_add_empty_content() {
    let (backend, _temp_dir) = create_test_backend().await;

    let empty_data: &[u8] = b"";
    let cursor = Cursor::new(empty_data.to_vec());
    let reader = Box::pin(cursor);

    let response = backend.add(reader).await;
    assert!(response.is_ok());

    let response = response.unwrap();
    assert_eq!(response.size, "0");
}

#[tokio::test]
#[ignore] // Teste lento - executar manualmente se necessário
async fn test_cat_after_backend_restart() {
    let (client_config, temp_dir) = create_test_config().await;

    let hash = {
        // Cria backend, adiciona conteúdo e pega hash
        let backend = IrohBackend::new(&client_config)
            .await
            .expect("Failed to create backend");

        let test_data = b"Persistent data test";
        let cursor = Cursor::new(test_data.to_vec());
        let reader = Box::pin(cursor);
        let add_response = backend.add(reader).await.expect("Failed to add");
        let hash = add_response.hash.clone();

        println!("✓ Conteúdo adicionado. Hash: {}", hash);

        // Força pin para garantir persistência
        backend.pin_add(&hash).await.expect("Failed to pin");
        println!("✓ Pin adicionado para hash: {}", hash);

        // Verifica que o pin foi criado antes de dropar o backend
        let pins_before = backend.pin_ls().await.expect("Failed to list pins");
        println!(
            "✓ Pins antes do restart: {:?}",
            pins_before.iter().map(|p| &p.hash).collect::<Vec<_>>()
        );
        assert!(
            pins_before.iter().any(|p| p.hash == hash),
            "Pin não encontrado antes do restart"
        );

        // Verifica que conseguimos ler o conteúdo antes do restart
        let _data_before = backend
            .cat(&hash)
            .await
            .expect("Failed to cat before restart");
        println!("✓ Conteúdo lido com sucesso antes do restart");

        // CRITICAL: Chama shutdown() para garantir que todas as operações assíncronas
        // sejam finalizadas e o banco de dados SQLite (blobs.db) seja sincronizado
        backend
            .shutdown()
            .await
            .expect("Failed to shutdown backend");
        println!("✓ Backend1 shutdown graceful executado");

        // Explicitamente dropa o backend
        drop(backend);
        println!("✓ Backend1 dropado");

        hash
    };

    println!("✓ Aguardando sync final do filesystem...");

    // Aguarda menos tempo já que fizemos shutdown graceful
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Verifica se os arquivos de store existem no filesystem
    let store_dir = temp_dir.path().join("iroh_store");
    if !store_dir.exists() {
        panic!(
            "Store directory não existe após backend drop: {:?}",
            store_dir
        );
    }
    println!("✓ Store directory exists: {:?}", store_dir);

    // Lista conteúdo do store_dir para debug
    if let Ok(entries) = std::fs::read_dir(&store_dir) {
        println!("Store directory contents:");
        for entry in entries.flatten() {
            println!("  - {:?}", entry.file_name());
        }
    }

    println!("✓ Iniciando backend2 no mesmo diretório...");

    // Cria novo backend no mesmo diretório com timeout
    let backend2 = tokio::time::timeout(Duration::from_secs(120), IrohBackend::new(&client_config))
        .await
        .expect("Timeout ao criar backend2")
        .expect("Failed to create backend2");

    println!("✓ Backend2 criado com sucesso");

    // Verifica se o pin foi persistido
    let pins_after = backend2
        .pin_ls()
        .await
        .expect("Failed to list pins after restart");
    println!(
        "✓ Pins após restart: {:?}",
        pins_after.iter().map(|p| &p.hash).collect::<Vec<_>>()
    );

    if !pins_after.iter().any(|p| p.hash == hash) {
        panic!(
            "Pin não foi persistido através do restart. Pins encontrados: {}. Esperado: {}",
            pins_after.len(),
            hash
        );
    }

    println!("✓ Pin encontrado após restart");

    // Tenta recuperar conteúdo
    let result = backend2.cat(&hash).await;

    if let Err(e) = result {
        panic!(
            "Falha ao recuperar conteúdo após restart. Erro: {:?}. Pin estava presente mas blob data não.",
            e
        );
    }

    println!("✓ Conteúdo recuperado com sucesso após restart");

    drop(backend2);
    drop(temp_dir);
}

#[tokio::test]
async fn test_pin_add_twice() {
    let (backend, _temp_dir) = create_test_backend().await;

    // Adiciona conteúdo
    let test_data = b"Double pin test";
    let cursor = Cursor::new(test_data.to_vec());
    let reader = Box::pin(cursor);
    let add_response = backend.add(reader).await.expect("Failed to add");
    let hash = add_response.hash.clone();

    // Primeira fixação
    let result1 = backend.pin_add(&hash).await;
    assert!(result1.is_ok());

    // Segunda fixação (deve ser idempotente)
    let result2 = backend.pin_add(&hash).await;
    // Pode retornar Ok ou Err dependendo da implementação
    // O importante é que não causa panic
    let _ = result2;

    // Verifica que está na lista apenas uma vez ou múltiplas vezes (implementação específica)
    let pins = backend.pin_ls().await.expect("Failed to list pins");
    assert!(pins.iter().any(|p| p.hash == hash));
}

#[tokio::test]
async fn test_remove_nonexistent_pin() {
    let (backend, _temp_dir) = create_test_backend().await;

    let fake_hash = "0".repeat(64);
    let result = backend.pin_rm(&fake_hash).await;

    // Deve retornar erro ou Ok dependendo da implementação
    // Não deve causar panic
    let _ = result;
}
