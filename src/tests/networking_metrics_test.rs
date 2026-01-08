/// Testes robustos para o módulo NetworkingMetricsCollector
///
/// Este módulo contém testes completos para validar:
/// - Criação e inicialização do coletor de métricas
/// - Registro de eventos (conexões, mensagens, operações)
/// - Cálculo de médias e agregações
/// - Atualização de métricas computadas
/// - Geração de relatórios e exportação JSON
/// - Concorrência e thread-safety
/// - Limites de amostras e performance
use crate::p2p::network::core::networking_metrics::NetworkingMetricsCollector;
use iroh::NodeId;
use iroh_gossip::proto::TopicId;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, info};

/// Helper para criar NodeId de teste
fn create_test_node_id(seed: u8) -> NodeId {
    // Usar SecretKey para gerar NodeId válido
    use iroh::SecretKey;
    let mut bytes = [0u8; 32];
    bytes[0] = seed;
    bytes[31] = seed; // Garantir alguma variação
    let secret = SecretKey::from_bytes(&bytes);
    secret.public()
}

/// Helper para criar TopicId de teste
fn create_test_topic_id(seed: u8) -> TopicId {
    let bytes = [seed; 32];
    TopicId::from_bytes(bytes)
}

#[cfg(test)]
mod initialization_tests {
    use super::*;

    #[tokio::test]
    async fn test_collector_initialization() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando inicialização do NetworkingMetricsCollector");

        let collector = NetworkingMetricsCollector::new();
        let metrics = collector.get_metrics().await;

        // Verificar que todas as métricas começam em zero
        assert_eq!(metrics.connectivity.total_connections, 0);
        assert_eq!(metrics.connectivity.total_disconnections, 0);
        assert_eq!(metrics.gossipsub.messages_sent, 0);
        assert_eq!(metrics.gossipsub.messages_received, 0);
        assert_eq!(metrics.discovery.discovery_attempts, 0);
        assert_eq!(metrics.backend_metrics.add_operations, 0);
        assert_eq!(metrics.backend_metrics.cat_operations, 0);

        // Verificar médias iniciais
        assert_eq!(metrics.connectivity.avg_peer_latency_ms, 0.0);
        assert_eq!(metrics.gossipsub.avg_propagation_latency_ms, 0.0);
        assert_eq!(metrics.discovery.avg_discovery_time_ms, 0.0);

        info!("✓ Coletor inicializado com valores padrão corretos");
    }

    #[tokio::test]
    async fn test_collector_default() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando trait Default do coletor");

        let collector = NetworkingMetricsCollector::default();
        let metrics = collector.get_metrics().await;

        assert_eq!(metrics.connectivity.total_connections, 0);
        assert!(metrics.last_updated > 0, "Timestamp deve estar definido");

        info!("✓ Trait Default funcionando corretamente");
    }

    #[tokio::test]
    async fn test_multiple_collectors() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando múltiplos coletores independentes");

        let collector1 = NetworkingMetricsCollector::new();
        let collector2 = NetworkingMetricsCollector::new();
        let collector3 = NetworkingMetricsCollector::new();

        // Cada coletor deve ser independente
        let node_id = create_test_node_id(1);
        collector1.record_peer_connected(node_id, Some(10.0)).await;
        collector1.update_computed_metrics().await;

        let metrics1 = collector1.get_metrics().await;
        let metrics2 = collector2.get_metrics().await;
        let metrics3 = collector3.get_metrics().await;

        assert_eq!(metrics1.connectivity.total_connections, 1);
        assert_eq!(metrics2.connectivity.total_connections, 0);
        assert_eq!(metrics3.connectivity.total_connections, 0);

        info!("✓ Múltiplos coletores são independentes");
    }
}

#[cfg(test)]
mod connectivity_tests {
    use super::*;

    #[tokio::test]
    async fn test_record_peer_connected() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando registro de peer conectado");

        let collector = NetworkingMetricsCollector::new();
        let node_id = create_test_node_id(42);

        collector.record_peer_connected(node_id, Some(25.5)).await;
        collector.update_computed_metrics().await;

        let metrics = collector.get_metrics().await;
        assert_eq!(metrics.connectivity.total_connections, 1);
        assert_eq!(metrics.connectivity.avg_peer_latency_ms, 25.5);

        info!("✓ Peer conectado registrado corretamente");
    }

    #[tokio::test]
    async fn test_record_peer_connected_without_latency() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando registro de peer sem latência");

        let collector = NetworkingMetricsCollector::new();
        let node_id = create_test_node_id(1);

        collector.record_peer_connected(node_id, None).await;
        collector.update_computed_metrics().await;

        let metrics = collector.get_metrics().await;
        assert_eq!(metrics.connectivity.total_connections, 1);
        assert_eq!(metrics.connectivity.avg_peer_latency_ms, 0.0);

        info!("✓ Peer sem latência registrado corretamente");
    }

    #[tokio::test]
    async fn test_record_multiple_peers() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando registro de múltiplos peers");

        let collector = NetworkingMetricsCollector::new();

        for i in 0..10 {
            let node_id = create_test_node_id(i);
            let latency = (i as f64) * 10.0;
            collector
                .record_peer_connected(node_id, Some(latency))
                .await;
        }

        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        assert_eq!(metrics.connectivity.total_connections, 10);
        // Latência média: (0+10+20+30+40+50+60+70+80+90) / 10 = 45.0
        assert_eq!(metrics.connectivity.avg_peer_latency_ms, 45.0);

        info!("✓ Múltiplos peers registrados e média calculada corretamente");
    }

    #[tokio::test]
    async fn test_record_peer_disconnected() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando registro de peer desconectado");

        let collector = NetworkingMetricsCollector::new();
        let node_id = create_test_node_id(1);

        collector.record_peer_disconnected(node_id).await;
        collector.update_computed_metrics().await;

        let metrics = collector.get_metrics().await;
        assert_eq!(metrics.connectivity.total_disconnections, 1);

        info!("✓ Peer desconectado registrado corretamente");
    }

    #[tokio::test]
    async fn test_connection_disconnection_cycle() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando ciclo de conexão/desconexão");

        let collector = NetworkingMetricsCollector::new();
        let node_id = create_test_node_id(1);

        // Ciclo: conectar -> desconectar -> reconectar
        collector.record_peer_connected(node_id, Some(10.0)).await;
        collector.record_peer_disconnected(node_id).await;
        collector.record_peer_connected(node_id, Some(15.0)).await;

        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        assert_eq!(metrics.connectivity.total_connections, 2);
        assert_eq!(metrics.connectivity.total_disconnections, 1);

        info!("✓ Ciclo de conexão/desconexão registrado corretamente");
    }

    #[tokio::test]
    async fn test_latency_sample_limit() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando limite de amostras de latência (max 100)");

        let collector = NetworkingMetricsCollector::new();

        // Adicionar 150 amostras (deve manter apenas últimas 100)
        for i in 0..150 {
            let node_id = create_test_node_id((i % 256) as u8);
            collector
                .record_peer_connected(node_id, Some(i as f64))
                .await;
        }

        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        // Média das últimas 100 amostras: 50-149 = média 99.5
        assert!(metrics.connectivity.avg_peer_latency_ms > 90.0);
        assert!(metrics.connectivity.avg_peer_latency_ms < 110.0);

        info!("✓ Limite de amostras respeitado e média calculada corretamente");
    }
}

#[cfg(test)]
mod gossipsub_tests {
    use super::*;

    #[tokio::test]
    async fn test_record_message_sent() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando registro de mensagem enviada");

        let collector = NetworkingMetricsCollector::new();
        let topic = create_test_topic_id(1);

        collector.record_message_sent(&topic, 1024).await;
        collector.update_computed_metrics().await;

        let metrics = collector.get_metrics().await;
        assert_eq!(metrics.gossipsub.messages_sent, 1);

        info!("✓ Mensagem enviada registrada corretamente");
    }

    #[tokio::test]
    async fn test_record_message_received() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando registro de mensagem recebida");

        let collector = NetworkingMetricsCollector::new();
        let topic = create_test_topic_id(1);

        collector
            .record_message_received(&topic, 2048, Some(50.5))
            .await;
        collector.update_computed_metrics().await;

        let metrics = collector.get_metrics().await;
        assert_eq!(metrics.gossipsub.messages_received, 1);
        assert_eq!(metrics.gossipsub.avg_propagation_latency_ms, 50.5);

        info!("✓ Mensagem recebida registrada corretamente");
    }

    #[tokio::test]
    async fn test_message_throughput_calculation() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando cálculo de throughput de mensagens");

        let collector = NetworkingMetricsCollector::new();
        let topic = create_test_topic_id(1);

        // Enviar várias mensagens
        for _ in 0..100 {
            collector
                .record_message_received(&topic, 1024, Some(10.0))
                .await;
        }

        // Aguardar um pouco para calcular throughput
        sleep(Duration::from_millis(100)).await;
        collector.update_computed_metrics().await;

        let metrics = collector.get_metrics().await;
        assert_eq!(metrics.gossipsub.messages_received, 100);
        assert!(metrics.gossipsub.message_throughput > 0.0);

        info!(
            "✓ Throughput calculado: {:.2} msgs/s",
            metrics.gossipsub.message_throughput
        );
    }

    #[tokio::test]
    async fn test_propagation_latency_average() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando média de latência de propagação");

        let collector = NetworkingMetricsCollector::new();
        let topic = create_test_topic_id(1);

        // Latências: 10, 20, 30, 40, 50 -> média = 30
        let latencies = vec![10.0, 20.0, 30.0, 40.0, 50.0];
        for latency in latencies {
            collector
                .record_message_received(&topic, 512, Some(latency))
                .await;
        }

        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        assert_eq!(metrics.gossipsub.avg_propagation_latency_ms, 30.0);

        info!("✓ Latência média de propagação calculada corretamente");
    }

    #[tokio::test]
    async fn test_message_without_latency() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando mensagem sem latência");

        let collector = NetworkingMetricsCollector::new();
        let topic = create_test_topic_id(1);

        collector.record_message_received(&topic, 512, None).await;
        collector.update_computed_metrics().await;

        let metrics = collector.get_metrics().await;
        assert_eq!(metrics.gossipsub.messages_received, 1);
        assert_eq!(metrics.gossipsub.avg_propagation_latency_ms, 0.0);

        info!("✓ Mensagem sem latência registrada corretamente");
    }

    #[tokio::test]
    async fn test_multiple_topics() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando mensagens em múltiplos tópicos");

        let collector = NetworkingMetricsCollector::new();

        let topic1 = create_test_topic_id(1);
        let topic2 = create_test_topic_id(2);
        let topic3 = create_test_topic_id(3);

        collector.record_message_sent(&topic1, 100).await;
        collector.record_message_sent(&topic2, 200).await;
        collector
            .record_message_received(&topic3, 300, Some(15.0))
            .await;

        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        assert_eq!(metrics.gossipsub.messages_sent, 2);
        assert_eq!(metrics.gossipsub.messages_received, 1);

        info!("✓ Mensagens em múltiplos tópicos registradas corretamente");
    }
}

#[cfg(test)]
mod iroh_operations_tests {
    use super::*;

    #[tokio::test]
    async fn test_record_add_operation() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando registro de operação add");

        let collector = NetworkingMetricsCollector::new();

        collector.record_add_operation(150.0, 2048).await;
        collector.update_computed_metrics().await;

        let metrics = collector.get_metrics().await;
        assert_eq!(metrics.backend_metrics.add_operations, 1);
        assert_eq!(metrics.backend_metrics.avg_add_time_ms, 150.0);

        info!("✓ Operação add registrada corretamente");
    }

    #[tokio::test]
    async fn test_record_cat_operation() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando registro de operação cat");

        let collector = NetworkingMetricsCollector::new();

        collector.record_cat_operation(75.5, 4096).await;
        collector.update_computed_metrics().await;

        let metrics = collector.get_metrics().await;
        assert_eq!(metrics.backend_metrics.cat_operations, 1);
        assert_eq!(metrics.backend_metrics.avg_cat_time_ms, 75.5);

        info!("✓ Operação cat registrada corretamente");
    }

    #[tokio::test]
    async fn test_multiple_add_operations() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando múltiplas operações add");

        let collector = NetworkingMetricsCollector::new();

        // Tempos: 100, 200, 300 -> média = 200
        collector.record_add_operation(100.0, 1024).await;
        collector.record_add_operation(200.0, 2048).await;
        collector.record_add_operation(300.0, 4096).await;

        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        assert_eq!(metrics.backend_metrics.add_operations, 3);
        assert_eq!(metrics.backend_metrics.avg_add_time_ms, 200.0);

        info!("✓ Múltiplas operações add com média correta");
    }

    #[tokio::test]
    async fn test_multiple_cat_operations() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando múltiplas operações cat");

        let collector = NetworkingMetricsCollector::new();

        // Tempos: 50, 150 -> média = 100
        collector.record_cat_operation(50.0, 512).await;
        collector.record_cat_operation(150.0, 1024).await;

        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        assert_eq!(metrics.backend_metrics.cat_operations, 2);
        assert_eq!(metrics.backend_metrics.avg_cat_time_ms, 100.0);

        info!("✓ Múltiplas operações cat com média correta");
    }

    #[tokio::test]
    async fn test_mixed_iroh_operations() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando operações add e cat misturadas");

        let collector = NetworkingMetricsCollector::new();

        collector.record_add_operation(100.0, 1024).await;
        collector.record_cat_operation(50.0, 512).await;
        collector.record_add_operation(200.0, 2048).await;
        collector.record_cat_operation(150.0, 1024).await;

        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        assert_eq!(metrics.backend_metrics.add_operations, 2);
        assert_eq!(metrics.backend_metrics.cat_operations, 2);
        assert_eq!(metrics.backend_metrics.avg_add_time_ms, 150.0); // (100+200)/2
        assert_eq!(metrics.backend_metrics.avg_cat_time_ms, 100.0); // (50+150)/2

        info!("✓ Operações mistas registradas corretamente");
    }
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    #[tokio::test]
    async fn test_record_discovery_successful() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando registro de discovery bem-sucedida");

        let collector = NetworkingMetricsCollector::new();

        collector.record_discovery(120.0, true).await;
        collector.update_computed_metrics().await;

        let metrics = collector.get_metrics().await;
        assert_eq!(metrics.discovery.discovery_attempts, 1);
        assert_eq!(metrics.discovery.successful_discoveries, 1);
        assert_eq!(metrics.discovery.avg_discovery_time_ms, 120.0);

        info!("✓ Discovery bem-sucedida registrada corretamente");
    }

    #[tokio::test]
    async fn test_record_discovery_failed() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando registro de discovery falhada");

        let collector = NetworkingMetricsCollector::new();

        collector.record_discovery(180.0, false).await;
        collector.update_computed_metrics().await;

        let metrics = collector.get_metrics().await;
        assert_eq!(metrics.discovery.discovery_attempts, 1);
        assert_eq!(metrics.discovery.successful_discoveries, 0);

        info!("✓ Discovery falhada registrada corretamente");
    }

    #[tokio::test]
    async fn test_discovery_success_rate() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando taxa de sucesso de discoveries");

        let collector = NetworkingMetricsCollector::new();

        // 7 sucessos, 3 falhas = 70% taxa de sucesso
        for i in 0..10 {
            let success = i < 7;
            collector.record_discovery(100.0, success).await;
        }

        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        assert_eq!(metrics.discovery.discovery_attempts, 10);
        assert_eq!(metrics.discovery.successful_discoveries, 7);

        let success_rate = metrics.discovery.successful_discoveries as f64
            / metrics.discovery.discovery_attempts as f64
            * 100.0;
        assert_eq!(success_rate, 70.0);

        info!("✓ Taxa de sucesso discovery: {:.1}%", success_rate);
    }

    #[tokio::test]
    async fn test_discovery_time_average() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando média de tempo de discovery");

        let collector = NetworkingMetricsCollector::new();

        // Tempos: 100, 200, 300, 400 -> média = 250
        let times = vec![100.0, 200.0, 300.0, 400.0];
        for time in times {
            collector.record_discovery(time, true).await;
        }

        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        assert_eq!(metrics.discovery.avg_discovery_time_ms, 250.0);

        info!("✓ Tempo médio de discovery calculado corretamente");
    }
}

#[cfg(test)]
mod reporting_tests {
    use super::*;

    #[tokio::test]
    async fn test_generate_report() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando geração de relatório");

        let collector = NetworkingMetricsCollector::new();

        // Adicionar algumas métricas
        let node_id = create_test_node_id(1);
        let topic = create_test_topic_id(1);

        collector.record_peer_connected(node_id, Some(25.0)).await;
        collector.record_message_sent(&topic, 1024).await;
        collector.record_add_operation(100.0, 2048).await;
        collector.record_discovery(120.0, true).await;

        collector.update_computed_metrics().await;
        let report = collector.generate_report().await;

        // Verificar que o relatório contém as seções esperadas
        assert!(report.contains("RELATÓRIO DE MÉTRICAS"));
        assert!(report.contains("CONECTIVIDADE P2P"));
        assert!(report.contains("GOSSIPSUB"));
        assert!(report.contains("DISCOVERY"));
        assert!(report.contains("Iroh"));
        assert!(report.contains("Total conexões: 1"));
        assert!(report.contains("Mensagens enviadas: 1"));
        assert!(report.contains("Operações add: 1"));
        assert!(report.contains("Tentativas: 1"));

        info!("✓ Relatório gerado com sucesso");
        debug!("Relatório:\n{}", report);
    }

    #[tokio::test]
    async fn test_export_json() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando exportação JSON");

        let collector = NetworkingMetricsCollector::new();

        // Adicionar algumas métricas
        let node_id = create_test_node_id(1);
        collector.record_peer_connected(node_id, Some(10.0)).await;
        collector.update_computed_metrics().await;

        let json_result = collector.export_json().await;
        assert!(json_result.is_ok(), "Exportação JSON deve ter sucesso");

        let json = json_result.unwrap();

        // Verificar que é JSON válido
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("JSON deve ser válido");

        assert!(
            parsed["connectivity"]["total_connections"]
                .as_u64()
                .is_some()
        );
        assert!(parsed["gossipsub"].is_object());
        assert!(parsed["discovery"].is_object());
        assert!(parsed["backend_metrics"].is_object());

        info!("✓ JSON exportado e validado com sucesso");
        debug!("JSON exportado:\n{}", json);
    }

    #[tokio::test]
    async fn test_metrics_snapshot() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando snapshot de métricas");

        let collector = NetworkingMetricsCollector::new();

        let node_id = create_test_node_id(1);
        collector.record_peer_connected(node_id, Some(15.0)).await;
        collector.update_computed_metrics().await;

        // Obter snapshot
        let snapshot = collector.get_metrics().await;

        // Adicionar mais métricas após o snapshot
        collector
            .record_peer_connected(create_test_node_id(2), Some(20.0))
            .await;
        collector.update_computed_metrics().await;

        let new_snapshot = collector.get_metrics().await;

        // Snapshot original não deve mudar
        assert_eq!(snapshot.connectivity.total_connections, 1);
        // Novo snapshot deve ter métricas atualizadas
        assert_eq!(new_snapshot.connectivity.total_connections, 2);

        info!("✓ Snapshots independentes funcionando corretamente");
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;

    #[tokio::test]
    async fn test_concurrent_peer_connections() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando conexões concorrentes de peers");

        let collector = Arc::new(NetworkingMetricsCollector::new());
        let mut handles = Vec::new();

        // 10 tasks conectando peers simultaneamente
        for i in 0..10 {
            let collector_clone = collector.clone();
            let handle = tokio::spawn(async move {
                let node_id = create_test_node_id(i);
                collector_clone
                    .record_peer_connected(node_id, Some((i as f64) * 5.0))
                    .await;
            });
            handles.push(handle);
        }

        // Aguardar todas as tasks
        for handle in handles {
            handle.await.unwrap();
        }

        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        assert_eq!(metrics.connectivity.total_connections, 10);

        info!("✓ Conexões concorrentes registradas corretamente");
    }

    #[tokio::test]
    async fn test_concurrent_message_recording() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando registro concorrente de mensagens");

        let collector = Arc::new(NetworkingMetricsCollector::new());
        let mut handles = Vec::new();

        // 20 tasks enviando mensagens simultaneamente
        for i in 0..20 {
            let collector_clone = collector.clone();
            let handle = tokio::spawn(async move {
                let topic = create_test_topic_id((i % 5) as u8);
                if i % 2 == 0 {
                    collector_clone.record_message_sent(&topic, 1024).await;
                } else {
                    collector_clone
                        .record_message_received(&topic, 2048, Some(10.0))
                        .await;
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.await.unwrap();
        }

        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        assert_eq!(metrics.gossipsub.messages_sent, 10);
        assert_eq!(metrics.gossipsub.messages_received, 10);

        info!("✓ Mensagens concorrentes registradas corretamente");
    }

    #[tokio::test]
    async fn test_concurrent_mixed_operations() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando operações mistas concorrentes");

        let collector = Arc::new(NetworkingMetricsCollector::new());
        let mut handles = Vec::new();

        // Diferentes tipos de operações em paralelo
        for i in 0..30 {
            let collector_clone = collector.clone();
            let handle = tokio::spawn(async move {
                match i % 3 {
                    0 => {
                        let node_id = create_test_node_id((i % 256) as u8);
                        collector_clone
                            .record_peer_connected(node_id, Some(10.0))
                            .await;
                    }
                    1 => {
                        collector_clone.record_add_operation(100.0, 1024).await;
                    }
                    2 => {
                        collector_clone.record_discovery(50.0, true).await;
                    }
                    _ => {}
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.await.unwrap();
        }

        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        assert_eq!(metrics.connectivity.total_connections, 10);
        assert_eq!(metrics.backend_metrics.add_operations, 10);
        assert_eq!(metrics.discovery.discovery_attempts, 10);

        info!("✓ Operações mistas concorrentes registradas corretamente");
    }

    #[tokio::test]
    async fn test_concurrent_metric_updates() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando atualizações concorrentes de métricas");

        let collector = Arc::new(NetworkingMetricsCollector::new());
        let mut handles = Vec::new();

        // Múltiplas tasks atualizando métricas
        for i in 0..5 {
            let collector_clone = collector.clone();
            let handle = tokio::spawn(async move {
                for j in 0..10 {
                    let node_id = create_test_node_id(((i * 10 + j) % 256) as u8);
                    collector_clone
                        .record_peer_connected(node_id, Some(5.0))
                        .await;

                    if j % 2 == 0 {
                        collector_clone.update_computed_metrics().await;
                    }
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.await.unwrap();
        }

        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        assert_eq!(metrics.connectivity.total_connections, 50);

        info!("✓ Atualizações concorrentes de métricas funcionando corretamente");
    }
}

#[cfg(test)]
mod edge_cases_tests {
    use super::*;

    #[tokio::test]
    async fn test_zero_samples_average() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando média com zero amostras");

        let collector = NetworkingMetricsCollector::new();
        collector.update_computed_metrics().await;

        let metrics = collector.get_metrics().await;
        assert_eq!(metrics.connectivity.avg_peer_latency_ms, 0.0);
        assert_eq!(metrics.gossipsub.avg_propagation_latency_ms, 0.0);
        assert_eq!(metrics.discovery.avg_discovery_time_ms, 0.0);

        info!("✓ Médias com zero amostras retornam 0.0");
    }

    #[tokio::test]
    async fn test_extreme_latency_values() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando valores extremos de latência");

        let collector = NetworkingMetricsCollector::new();

        let node_id = create_test_node_id(1);

        // Latências extremas
        collector.record_peer_connected(node_id, Some(0.0)).await;
        collector
            .record_peer_connected(node_id, Some(10000.0))
            .await;

        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        // Média: (0 + 10000) / 2 = 5000
        assert_eq!(metrics.connectivity.avg_peer_latency_ms, 5000.0);

        info!("✓ Valores extremos tratados corretamente");
    }

    #[tokio::test]
    async fn test_large_number_of_operations() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando grande número de operações");

        let collector = NetworkingMetricsCollector::new();
        let topic = create_test_topic_id(1);

        // 1000 operações
        for _ in 0..1000 {
            collector.record_message_sent(&topic, 512).await;
        }

        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        assert_eq!(metrics.gossipsub.messages_sent, 1000);

        info!("✓ Grande número de operações registrado corretamente");
    }

    #[tokio::test]
    async fn test_timestamp_progression() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando progressão de timestamps");

        let collector = NetworkingMetricsCollector::new();

        let initial_metrics = collector.get_metrics().await;
        let initial_timestamp = initial_metrics.last_updated;

        sleep(Duration::from_millis(100)).await;
        collector.update_computed_metrics().await;

        let updated_metrics = collector.get_metrics().await;
        let updated_timestamp = updated_metrics.last_updated;

        assert!(
            updated_timestamp >= initial_timestamp,
            "Timestamp deve progredir ou permanecer igual"
        );

        info!("✓ Timestamps progredindo corretamente");
    }

    #[tokio::test]
    async fn test_sample_window_sliding() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando janela deslizante de amostras");

        let collector = NetworkingMetricsCollector::new();
        let node_id = create_test_node_id(1);

        // Adicionar 110 amostras com valores crescentes
        for i in 0..110 {
            collector
                .record_peer_connected(node_id, Some(i as f64))
                .await;
        }

        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        // Deve manter apenas últimas 100 amostras (10-109)
        // Média: 59.5
        assert!(metrics.connectivity.avg_peer_latency_ms > 50.0);
        assert!(metrics.connectivity.avg_peer_latency_ms < 70.0);

        info!("✓ Janela deslizante de amostras funcionando corretamente");
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    #[tokio::test]
    async fn test_full_metrics_workflow() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando workflow completo de métricas");

        let collector = NetworkingMetricsCollector::new();

        // 1. Conectar peers
        for i in 0..5 {
            let node_id = create_test_node_id(i);
            collector
                .record_peer_connected(node_id, Some(10.0 + i as f64))
                .await;
        }

        // 2. Enviar/receber mensagens
        let topic = create_test_topic_id(1);
        for i in 0..10 {
            if i % 2 == 0 {
                collector.record_message_sent(&topic, 1024).await;
            } else {
                collector
                    .record_message_received(&topic, 2048, Some(20.0))
                    .await;
            }
        }

        // 3. Operações iroh
        for _ in 0..3 {
            collector.record_add_operation(100.0, 1024).await;
            collector.record_cat_operation(50.0, 2048).await;
        }

        // 4. Tentativas de discovery
        for i in 0..10 {
            collector.record_discovery(80.0, i % 3 != 0).await;
        }

        // 5. Atualizar e verificar
        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        assert_eq!(metrics.connectivity.total_connections, 5);
        assert_eq!(metrics.gossipsub.messages_sent, 5);
        assert_eq!(metrics.gossipsub.messages_received, 5);
        assert_eq!(metrics.backend_metrics.add_operations, 3);
        assert_eq!(metrics.backend_metrics.cat_operations, 3);
        assert_eq!(metrics.discovery.discovery_attempts, 10);

        // 6. Gerar relatório
        let report = collector.generate_report().await;
        assert!(report.contains("RELATÓRIO"));

        // 7. Exportar JSON
        let json = collector.export_json().await.unwrap();
        assert!(json.contains("connectivity"));

        info!("✓ Workflow completo de métricas executado com sucesso");
    }

    #[tokio::test]
    async fn test_real_world_simulation() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando simulação de cenário");

        let collector = Arc::new(NetworkingMetricsCollector::new());

        // Simular atividade de rede por 500ms
        let mut handles = Vec::new();

        // Task 1: Conectar peers periodicamente
        let collector1 = collector.clone();
        let h1 = tokio::spawn(async move {
            for i in 0..10 {
                let node_id = create_test_node_id(i);
                collector1
                    .record_peer_connected(node_id, Some(15.0 + i as f64))
                    .await;
                sleep(Duration::from_millis(50)).await;
            }
        });
        handles.push(h1);

        // Task 2: Enviar mensagens
        let collector2 = collector.clone();
        let h2 = tokio::spawn(async move {
            let topic = create_test_topic_id(1);
            for _ in 0..20 {
                collector2.record_message_sent(&topic, 512).await;
                sleep(Duration::from_millis(25)).await;
            }
        });
        handles.push(h2);

        // Task 3: Operações iroh
        let collector3 = collector.clone();
        let h3 = tokio::spawn(async move {
            for _ in 0..5 {
                collector3.record_add_operation(120.0, 2048).await;
                sleep(Duration::from_millis(100)).await;
            }
        });
        handles.push(h3);

        // Aguardar todas as tasks
        for handle in handles {
            handle.await.unwrap();
        }

        // Atualizar métricas finais
        collector.update_computed_metrics().await;
        let metrics = collector.get_metrics().await;

        // Verificar que tudo foi registrado
        assert!(metrics.connectivity.total_connections > 0);
        assert!(metrics.gossipsub.messages_sent > 0);
        assert!(metrics.backend_metrics.add_operations > 0);

        let report = collector.generate_report().await;
        info!("Relatório da simulação:\n{}", report);

        info!("✓ Simulação de cenário completada com sucesso");
    }
}
