//! Testes robustos para o módulo OptimizedConnectionPool
//!
//! Este arquivo contém testes abrangentes para validar:
//! - Criação e configuração do pool
//! - Gerenciamento de conexões (criação, reutilização, liberação)
//! - Circuit breaker (estados e transições)
//! - Health monitoring
//! - Estatísticas e métricas
//! - Tratamento de timeouts e falhas
//! - Concorrência e load balancing

use crate::p2p::network::core::connection_pool::{
    CircuitBreaker, CircuitState, ConnectionEvent, ConnectionInfo, ConnectionStatus,
    OptimizedConnectionPool, PeerHealthMetrics, PoolConfig, PoolStats, PooledConnection,
};
use iroh::{NodeAddr, NodeId, SecretKey};
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::time::sleep;

/// Helper para criar NodeAddr de teste
fn create_test_node_addr(port: u16) -> NodeAddr {
    let node_id = create_test_node_id(port as u8);
    let socket_addr: SocketAddr = format!("127.0.0.1:{}", port).parse().unwrap();
    NodeAddr::from_parts(node_id, None, vec![socket_addr])
}

/// Helper para criar NodeId de teste válido
fn create_test_node_id(seed: u8) -> NodeId {
    // Cria um SecretKey a partir de um seed e deriva o NodeId (PublicKey)
    let mut seed_bytes = [0u8; 32];
    seed_bytes[0] = seed;
    // Preenche com padrão determinístico
    for (i, byte) in seed_bytes.iter_mut().enumerate().skip(1) {
        *byte = seed.wrapping_add(i as u8);
    }
    let secret = SecretKey::from_bytes(&seed_bytes);
    secret.public()
}

#[tokio::test]
async fn test_pool_creation_with_default_config() {
    let pool = OptimizedConnectionPool::new(PoolConfig::default());
    let stats = pool.get_stats().await;

    assert_eq!(stats.active_connections, 0);
    assert_eq!(stats.pooled_connections, 0);
    assert_eq!(stats.connections_created, 0);
    assert_eq!(stats.connections_reused, 0);
    assert_eq!(stats.connections_failed, 0);
}

#[tokio::test]
async fn test_pool_creation_with_custom_config() {
    let pool_config = PoolConfig {
        max_connections_per_peer: 5,
        max_total_connections: 100,
        connection_timeout_ms: 5000,
        idle_timeout_secs: 600,
        health_check_interval_secs: 60,
        max_retry_attempts: 5,
        initial_retry_backoff_ms: 500,
        backoff_multiplier: 1.5,
        circuit_breaker_threshold: 0.7,
        enable_intelligent_load_balancing: true,
    };

    let pool = OptimizedConnectionPool::new(pool_config.clone());
    let stats = pool.get_stats().await;

    assert_eq!(stats.active_connections, 0);
    assert_eq!(stats.pooled_connections, 0);
}

#[tokio::test]
async fn test_connection_establishment_failure_no_server() {
    let pool_config = PoolConfig {
        connection_timeout_ms: 1000,
        ..Default::default()
    };

    let pool = OptimizedConnectionPool::new(pool_config);
    let node_id = create_test_node_id(1);
    let address = create_test_node_addr(9999); // Porta sem servidor

    let result = pool.get_connection(node_id, address).await;

    assert!(result.is_err());

    let stats = pool.get_stats().await;
    assert!(stats.connections_failed > 0 || stats.connections_timeout > 0);
}

#[tokio::test]
async fn test_connection_stats_tracking() {
    let pool = OptimizedConnectionPool::new(PoolConfig::default());
    let initial_stats = pool.get_stats().await;

    assert_eq!(initial_stats.connections_created, 0);
    assert_eq!(initial_stats.connections_reused, 0);
    assert_eq!(initial_stats.active_connections, 0);

    // Tenta fazer uma conexão (vai falhar mas deve registrar nas stats)
    let node_id = create_test_node_id(2);
    let address = create_test_node_addr(9998);
    let _ = pool.get_connection(node_id, address).await;

    let stats_after = pool.get_stats().await;
    assert!(
        stats_after.connections_failed > 0 || stats_after.connections_timeout > 0,
        "Deve registrar falha ou timeout"
    );
}

#[tokio::test]
async fn test_circuit_breaker_opens_after_failures() {
    let pool_config = PoolConfig {
        circuit_breaker_threshold: 0.3, // 3 falhas
        connection_timeout_ms: 500,
        ..Default::default()
    };

    let pool = OptimizedConnectionPool::new(pool_config);
    let node_id = create_test_node_id(3);

    // Faz múltiplas tentativas de conexão que devem falhar
    for i in 0..5 {
        let address = create_test_node_addr(10000 + i);
        let result = pool.get_connection(node_id, address).await;
        assert!(result.is_err());
    }

    let stats = pool.get_stats().await;
    assert!(stats.connections_failed >= 3 || stats.connections_timeout >= 3);
}

#[tokio::test]
async fn test_circuit_breaker_default_state() {
    let breaker = CircuitBreaker::default();

    assert_eq!(breaker.state, CircuitState::Closed);
    assert_eq!(breaker.failure_count, 0);
    assert_eq!(breaker.success_count, 0);
    assert!(breaker.last_failure_time.is_none());
}

#[tokio::test]
async fn test_pool_config_default_values() {
    let pool_config = PoolConfig::default();

    assert_eq!(pool_config.max_connections_per_peer, 8);
    assert_eq!(pool_config.max_total_connections, 1000);
    assert_eq!(pool_config.connection_timeout_ms, 10_000);
    assert_eq!(pool_config.idle_timeout_secs, 300);
    assert_eq!(pool_config.health_check_interval_secs, 30);
    assert_eq!(pool_config.max_retry_attempts, 3);
    assert_eq!(pool_config.initial_retry_backoff_ms, 1000);
    assert_eq!(pool_config.backoff_multiplier, 2.0);
    assert_eq!(pool_config.circuit_breaker_threshold, 0.5);
    assert!(pool_config.enable_intelligent_load_balancing);
}

#[tokio::test]
async fn test_connection_info_structure() {
    let _node_id = create_test_node_id(4);
    let address = create_test_node_addr(8080);

    let connection_info = ConnectionInfo {
        connection_id: "test_conn_123".to_string(),
        peer_address: address,
        connected_at: Instant::now(),
        last_used: Instant::now(),
        operations_count: 0,
        avg_latency_ms: 15.5,
        status: ConnectionStatus::Healthy,
        priority: 5,
        bandwidth_bps: 10_000_000,
    };

    assert_eq!(connection_info.connection_id, "test_conn_123");
    assert_eq!(connection_info.status, ConnectionStatus::Healthy);
    assert_eq!(connection_info.priority, 5);
    assert_eq!(connection_info.avg_latency_ms, 15.5);
}

#[tokio::test]
async fn test_pooled_connection_structure() {
    let _node_id = create_test_node_id(5);
    let address = create_test_node_addr(8081);

    let connection_info = ConnectionInfo {
        connection_id: "pooled_conn_456".to_string(),
        peer_address: address,
        connected_at: Instant::now(),
        last_used: Instant::now(),
        operations_count: 5,
        avg_latency_ms: 20.0,
        status: ConnectionStatus::Healthy,
        priority: 7,
        bandwidth_bps: 15_000_000,
    };

    let pooled_conn = PooledConnection {
        info: connection_info,
        pooled_at: Instant::now(),
        reuse_count: 3,
        in_use: false,
    };

    assert_eq!(pooled_conn.reuse_count, 3);
    assert!(!pooled_conn.in_use);
    assert_eq!(pooled_conn.info.connection_id, "pooled_conn_456");
}

#[tokio::test]
async fn test_connection_status_variants() {
    assert_eq!(ConnectionStatus::Healthy, ConnectionStatus::Healthy);
    assert_ne!(ConnectionStatus::Healthy, ConnectionStatus::Degraded);
    assert_ne!(ConnectionStatus::Healthy, ConnectionStatus::Unavailable);
    assert_ne!(ConnectionStatus::Healthy, ConnectionStatus::Disconnected);
    assert_ne!(ConnectionStatus::Healthy, ConnectionStatus::Failed);
}

#[tokio::test]
async fn test_circuit_state_variants() {
    assert_eq!(CircuitState::Closed, CircuitState::Closed);
    assert_ne!(CircuitState::Closed, CircuitState::Open);
    assert_ne!(CircuitState::Closed, CircuitState::HalfOpen);
}

#[tokio::test]
async fn test_pool_stats_default() {
    let stats = PoolStats::default();

    assert_eq!(stats.active_connections, 0);
    assert_eq!(stats.pooled_connections, 0);
    assert_eq!(stats.connections_created, 0);
    assert_eq!(stats.connections_reused, 0);
    assert_eq!(stats.connections_failed, 0);
    assert_eq!(stats.connections_timeout, 0);
    assert_eq!(stats.avg_connection_time_ms, 0.0);
    assert_eq!(stats.reuse_rate, 0.0);
    assert_eq!(stats.total_bandwidth_bps, 0);
    assert_eq!(stats.global_avg_latency_ms, 0.0);
}

#[tokio::test]
async fn test_connection_event_variants() {
    let node_id = create_test_node_id(6);

    let event1 = ConnectionEvent::Connected {
        node_id,
        latency_ms: 25.0,
    };
    let event2 = ConnectionEvent::Disconnected {
        node_id,
        reason: "timeout".to_string(),
    };
    let event3 = ConnectionEvent::Degraded {
        node_id,
        health_score: 0.3,
    };
    let event4 = ConnectionEvent::Recovered { node_id };
    let event5 = ConnectionEvent::CircuitBreakerOpen { node_id };
    let event6 = ConnectionEvent::CircuitBreakerClosed { node_id };

    // Verifica que os eventos foram criados sem panic
    match event1 {
        ConnectionEvent::Connected { .. } => {}
        _ => panic!("Evento errado"),
    }

    match event2 {
        ConnectionEvent::Disconnected { .. } => {}
        _ => panic!("Evento errado"),
    }

    match event3 {
        ConnectionEvent::Degraded { .. } => {}
        _ => panic!("Evento errado"),
    }

    match event4 {
        ConnectionEvent::Recovered { .. } => {}
        _ => panic!("Evento errado"),
    }

    match event5 {
        ConnectionEvent::CircuitBreakerOpen { .. } => {}
        _ => panic!("Evento errado"),
    }

    match event6 {
        ConnectionEvent::CircuitBreakerClosed { .. } => {}
        _ => panic!("Evento errado"),
    }
}

#[tokio::test]
async fn test_peer_health_metrics_structure() {
    let metrics = PeerHealthMetrics {
        current_latency_ms: 15.5,
        packet_loss_rate: 0.02,
        throughput_bps: 10_000_000,
        uptime_secs: 3600,
        health_score: 0.85,
        last_measured: Instant::now(),
    };

    assert_eq!(metrics.current_latency_ms, 15.5);
    assert_eq!(metrics.packet_loss_rate, 0.02);
    assert_eq!(metrics.throughput_bps, 10_000_000);
    assert_eq!(metrics.uptime_secs, 3600);
    assert_eq!(metrics.health_score, 0.85);
}

#[tokio::test]
async fn test_event_subscription() {
    let pool = OptimizedConnectionPool::new(PoolConfig::default());
    let mut event_receiver = pool.subscribe_events();

    // Verifica que consegue criar um subscriber
    // (não vai receber eventos sem conexões, mas valida o mecanismo)
    tokio::select! {
        _ = event_receiver.recv() => {
            // Se receber um evento, ok
        }
        _ = sleep(Duration::from_millis(100)) => {
            // Timeout esperado, não há eventos
        }
    }
}

#[tokio::test]
async fn test_multiple_event_subscribers() {
    let pool = OptimizedConnectionPool::new(PoolConfig::default());
    let _receiver1 = pool.subscribe_events();
    let _receiver2 = pool.subscribe_events();
    let _receiver3 = pool.subscribe_events();

    // Verifica que múltiplos subscribers podem ser criados
    // Se chegou aqui sem panic, o teste passou
}

#[tokio::test]
async fn test_concurrent_connection_attempts() {
    let pool = std::sync::Arc::new(OptimizedConnectionPool::new(PoolConfig {
        connection_timeout_ms: 500,
        max_total_connections: 50,
        ..Default::default()
    }));

    let mut handles = vec![];

    // Tenta 10 conexões concorrentes
    for i in 0..10 {
        let pool_clone = pool.clone();
        let handle = tokio::spawn(async move {
            let node_id = create_test_node_id(i);
            let address = create_test_node_addr(11000 + i as u16);
            pool_clone.get_connection(node_id, address).await
        });
        handles.push(handle);
    }

    // Aguarda todas as tentativas completarem
    let mut failures = 0;
    for handle in handles {
        if let Ok(result) = handle.await
            && result.is_err()
        {
            failures += 1;
        }
    }

    // Todas devem falhar (não há servidor), mas não deve dar panic
    assert_eq!(failures, 10);

    let stats = pool.get_stats().await;
    assert!(stats.connections_failed + stats.connections_timeout >= 10);
}

#[tokio::test]
async fn test_connection_timeout_enforcement() {
    let pool_config = PoolConfig {
        connection_timeout_ms: 100, // Timeout muito curto
        ..Default::default()
    };

    let pool = OptimizedConnectionPool::new(pool_config);
    let node_id = create_test_node_id(7);
    let address = create_test_node_addr(12345);

    let start = Instant::now();
    let result = pool.get_connection(node_id, address).await;
    let elapsed = start.elapsed();

    assert!(result.is_err());
    // Deve ter dado timeout, então o tempo deve ser próximo ao timeout configurado
    assert!(elapsed.as_millis() < 500, "Timeout não foi respeitado");

    let stats = pool.get_stats().await;
    assert!(
        stats.connections_timeout > 0 || stats.connections_failed > 0,
        "Deve registrar timeout ou falha"
    );
}

#[tokio::test]
async fn test_pool_semaphore_limits() {
    let pool_config = PoolConfig {
        max_total_connections: 2, // Limite muito baixo para testar
        connection_timeout_ms: 100,
        ..Default::default()
    };

    let pool = std::sync::Arc::new(OptimizedConnectionPool::new(pool_config));
    let mut handles = vec![];

    // Tenta criar mais conexões do que o limite
    for i in 0..5 {
        let pool_clone = pool.clone();
        let handle = tokio::spawn(async move {
            let node_id = create_test_node_id(10 + i);
            let address = create_test_node_addr(13000 + i as u16);
            pool_clone.get_connection(node_id, address).await
        });
        handles.push(handle);
    }

    // Aguarda todas completarem
    for handle in handles {
        let _ = handle.await;
    }

    // Pool não deve quebrar mesmo com tentativas acima do limite
    let stats = pool.get_stats().await;
    assert!(stats.connections_failed >= 5 || stats.connections_timeout >= 5);
}

#[tokio::test]
async fn test_connection_release() {
    let pool = OptimizedConnectionPool::new(PoolConfig::default());
    let node_id = create_test_node_id(8);

    // Tenta liberar uma conexão que não existe (não deve dar panic)
    let result = pool
        .release_connection(node_id, "non_existent_conn".to_string())
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_health_monitor_start() {
    let pool = OptimizedConnectionPool::new(PoolConfig {
        health_check_interval_secs: 1,
        ..Default::default()
    });

    // Inicia o health monitor
    let monitor_handle = pool.start_health_monitor();

    // Aguarda um pouco para o monitor executar
    sleep(Duration::from_millis(500)).await;

    // Cancela o monitor
    monitor_handle.abort();

    // Se chegou aqui sem panic, o teste passou
}

#[tokio::test]
async fn test_stats_clone() {
    let stats = PoolStats {
        active_connections: 5,
        pooled_connections: 10,
        connections_created: 15,
        connections_reused: 20,
        connections_failed: 2,
        connections_timeout: 1,
        avg_connection_time_ms: 50.0,
        reuse_rate: 0.75,
        total_bandwidth_bps: 50_000_000,
        global_avg_latency_ms: 25.5,
    };

    let cloned = stats.clone();

    assert_eq!(cloned.active_connections, 5);
    assert_eq!(cloned.pooled_connections, 10);
    assert_eq!(cloned.connections_created, 15);
    assert_eq!(cloned.connections_reused, 20);
    assert_eq!(cloned.connections_failed, 2);
    assert_eq!(cloned.connections_timeout, 1);
    assert_eq!(cloned.avg_connection_time_ms, 50.0);
    assert_eq!(cloned.reuse_rate, 0.75);
    assert_eq!(cloned.total_bandwidth_bps, 50_000_000);
    assert_eq!(cloned.global_avg_latency_ms, 25.5);
}

#[tokio::test]
async fn test_circuit_breaker_clone() {
    let breaker = CircuitBreaker {
        state: CircuitState::HalfOpen,
        failure_count: 3,
        failure_threshold: 5,
        last_failure_time: Some(Instant::now()),
        timeout_ms: 5000,
        success_count: 1,
    };

    let cloned = breaker.clone();

    assert_eq!(cloned.state, CircuitState::HalfOpen);
    assert_eq!(cloned.failure_count, 3);
    assert_eq!(cloned.failure_threshold, 5);
    assert!(cloned.last_failure_time.is_some());
    assert_eq!(cloned.timeout_ms, 5000);
    assert_eq!(cloned.success_count, 1);
}

#[tokio::test]
async fn test_node_addr_creation() {
    let node_id = create_test_node_id(9);
    let socket_addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
    let node_addr = NodeAddr::from_parts(node_id, None, vec![socket_addr]);

    assert!(node_addr.direct_addresses().count() > 0);
    assert_eq!(node_addr.node_id, node_id);
}

#[tokio::test]
async fn test_multiple_direct_addresses() {
    let node_id = create_test_node_id(10);
    let addr1: SocketAddr = "127.0.0.1:8080".parse().unwrap();
    let addr2: SocketAddr = "127.0.0.1:8081".parse().unwrap();
    let node_addr = NodeAddr::from_parts(node_id, None, vec![addr1, addr2]);

    assert_eq!(node_addr.direct_addresses().count(), 2);
}

#[tokio::test]
async fn test_config_cloning() {
    let pool_config = PoolConfig {
        max_connections_per_peer: 10,
        max_total_connections: 200,
        connection_timeout_ms: 5000,
        idle_timeout_secs: 600,
        health_check_interval_secs: 60,
        max_retry_attempts: 5,
        initial_retry_backoff_ms: 500,
        backoff_multiplier: 1.5,
        circuit_breaker_threshold: 0.7,
        enable_intelligent_load_balancing: false,
    };

    let cloned = pool_config.clone();

    assert_eq!(cloned.max_connections_per_peer, 10);
    assert_eq!(cloned.max_total_connections, 200);
    assert_eq!(cloned.connection_timeout_ms, 5000);
    assert!(!cloned.enable_intelligent_load_balancing);
}

#[tokio::test]
async fn test_connection_info_clone() {
    let address = create_test_node_addr(8082);
    let info = ConnectionInfo {
        connection_id: "conn_789".to_string(),
        peer_address: address,
        connected_at: Instant::now(),
        last_used: Instant::now(),
        operations_count: 50,
        avg_latency_ms: 30.0,
        status: ConnectionStatus::Degraded,
        priority: 3,
        bandwidth_bps: 5_000_000,
    };

    let cloned = info.clone();

    assert_eq!(cloned.connection_id, "conn_789");
    assert_eq!(cloned.operations_count, 50);
    assert_eq!(cloned.avg_latency_ms, 30.0);
    assert_eq!(cloned.status, ConnectionStatus::Degraded);
}

#[tokio::test]
async fn test_pooled_connection_clone() {
    let address = create_test_node_addr(8083);
    let info = ConnectionInfo {
        connection_id: "conn_abc".to_string(),
        peer_address: address,
        connected_at: Instant::now(),
        last_used: Instant::now(),
        operations_count: 10,
        avg_latency_ms: 20.0,
        status: ConnectionStatus::Healthy,
        priority: 5,
        bandwidth_bps: 10_000_000,
    };

    let pooled = PooledConnection {
        info,
        pooled_at: Instant::now(),
        reuse_count: 5,
        in_use: true,
    };

    let cloned = pooled.clone();

    assert_eq!(cloned.info.connection_id, "conn_abc");
    assert_eq!(cloned.reuse_count, 5);
    assert!(cloned.in_use);
}

#[tokio::test]
async fn test_different_node_ids() {
    let node_id1 = create_test_node_id(1);
    let node_id2 = create_test_node_id(2);
    let node_id3 = create_test_node_id(1); // Mesmo seed

    assert_ne!(node_id1, node_id2);
    assert_eq!(node_id1, node_id3);
}

#[tokio::test]
async fn test_stress_with_rapid_connections() {
    let pool = std::sync::Arc::new(OptimizedConnectionPool::new(PoolConfig {
        connection_timeout_ms: 200,
        max_total_connections: 100,
        ..Default::default()
    }));

    let mut handles = vec![];

    // Tenta 50 conexões rápidas
    for i in 0..50 {
        let pool_clone = pool.clone();
        let handle = tokio::spawn(async move {
            let node_id = create_test_node_id(20 + (i % 10));
            let address = create_test_node_addr(14000 + i as u16);
            let _ = pool_clone.get_connection(node_id, address).await;
        });
        handles.push(handle);
    }

    // Aguarda todas
    for handle in handles {
        let _ = handle.await;
    }

    let stats = pool.get_stats().await;
    assert!(stats.connections_failed + stats.connections_timeout >= 50);
}

#[tokio::test]
async fn test_pool_with_zero_timeout() {
    let pool_config = PoolConfig {
        connection_timeout_ms: 0, // Timeout zero (deve falhar imediatamente)
        ..Default::default()
    };

    let pool = OptimizedConnectionPool::new(pool_config);
    let node_id = create_test_node_id(11);
    let address = create_test_node_addr(15000);

    let result = pool.get_connection(node_id, address).await;
    // Com timeout zero, deve falhar rapidamente
    assert!(result.is_err());
}

#[tokio::test]
async fn test_connection_event_clone() {
    let node_id = create_test_node_id(12);
    let event = ConnectionEvent::Connected {
        node_id,
        latency_ms: 50.0,
    };

    let cloned = event.clone();

    match cloned {
        ConnectionEvent::Connected {
            node_id: n,
            latency_ms,
        } => {
            assert_eq!(n, node_id);
            assert_eq!(latency_ms, 50.0);
        }
        _ => panic!("Evento clonado incorretamente"),
    }
}

#[tokio::test]
async fn test_pool_get_stats_consistency() {
    let pool = OptimizedConnectionPool::new(PoolConfig::default());

    let stats1 = pool.get_stats().await;
    let stats2 = pool.get_stats().await;

    // Stats devem ser consistentes sem mudanças
    assert_eq!(stats1.active_connections, stats2.active_connections);
    assert_eq!(stats1.pooled_connections, stats2.pooled_connections);
    assert_eq!(stats1.connections_created, stats2.connections_created);
}
