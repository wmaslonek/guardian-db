/// Demonstração do Performance Monitor do Guardian DB
///
/// Este exemplo mostra como usar o sistema de monitoramento de performance em tempo real
/// para coletar e analisar métricas de throughput, latência e recursos do IrohBackend.
///
/// Recursos demonstrados:
/// - Coleta automática de métricas de throughput e latência
/// - Atualização manual de métricas de recursos (CPU, memória, I/O)
/// - Criação de snapshots de performance
/// - Cálculo de percentis de latência (P95, P99)
/// - Geração de relatórios detalhados
/// - Histórico de performance
use guardian_db::guardian::error::Result;
use guardian_db::p2p::network::config::ClientConfig;
use guardian_db::p2p::network::core::IrohBackend;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{Level, info};

#[tokio::main]
async fn main() -> Result<()> {
    // Configura logging
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    info!("=== DEMO: Performance Monitor ===\n");

    // Cria diretório temporário para testes
    let temp_dir = std::env::temp_dir().join("guardian_perf_monitor_demo");
    tokio::fs::create_dir_all(&temp_dir).await?;

    // Configura backend
    let client_config = ClientConfig {
        data_store_path: Some(temp_dir.clone()),
        ..Default::default()
    };

    info!("1. Inicializando IrohBackend com performance monitor...");
    let backend = IrohBackend::new(&client_config).await?;
    info!("   ✓ Backend inicializado com monitor ativo\n");

    // === DEMONSTRAÇÃO 1: Métricas Automáticas ===
    info!("2. Operações com coleta automática de métricas...");

    // Executa algumas operações para gerar métricas
    for i in 0..10 {
        let data = format!("Test data {}", i);
        let data_bytes = data.into_bytes();
        let data_len = data_bytes.len();

        let reader = Box::pin(std::io::Cursor::new(data_bytes));
        let response = backend.add(reader).await?;

        info!("   • Adicionado: {} ({} bytes)", response.hash, data_len);

        // Pequeno delay entre operações
        sleep(Duration::from_millis(100)).await;
    }
    info!("   ✓ 10 operações concluídas com métricas coletadas\n");

    // Obtém métricas de throughput
    info!("3. Consultando métricas de throughput...");
    let throughput = backend.get_throughput_metrics().await;
    info!("   • Operações/segundo: {:.2}", throughput.ops_per_second);
    info!("   • Bytes/segundo: {}", throughput.bytes_per_second);
    info!(
        "   • Pico de throughput: {:.2} ops/s",
        throughput.peak_throughput
    );
    info!(
        "   • Throughput médio: {:.2} ops/s\n",
        throughput.avg_throughput
    );

    // Obtém métricas de latência
    info!("4. Consultando métricas de latência...");
    let latency = backend.get_latency_metrics().await;
    info!("   • Latência média: {:.2}ms", latency.avg_latency_ms);
    info!("   • Latência mínima: {:.2}ms", latency.min_latency_ms);
    info!("   • Latência máxima: {:.2}ms", latency.max_latency_ms);
    info!("   • Latência P95: {:.2}ms", latency.p95_latency_ms);
    info!("   • Latência P99: {:.2}ms\n", latency.p99_latency_ms);

    // === DEMONSTRAÇÃO 2: Atualização Manual de Recursos ===
    info!("5. Atualizando métricas de recursos manualmente...");

    // Simula uso de recursos
    backend
        .update_resource_metrics(
            0.45,              // CPU: 45%
            256 * 1024 * 1024, // Memória: 256MB
            50 * 1024 * 1024,  // I/O: 50MB/s
            100 * 1024 * 1024, // Network: 100MB/s
        )
        .await?;
    info!("   ✓ Recursos atualizados\n");

    // Obtém métricas de recursos
    info!("6. Consultando métricas de recursos...");
    let resources = backend.get_resource_metrics().await;
    info!("   • Uso de CPU: {:.1}%", resources.cpu_usage * 100.0);
    info!(
        "   • Uso de memória: {:.2}MB",
        resources.memory_usage_bytes as f64 / 1_048_576.0
    );
    info!(
        "   • I/O de disco: {:.2}MB/s",
        resources.disk_io_bps as f64 / 1_048_576.0
    );
    info!(
        "   • Largura de banda: {:.2}MB/s\n",
        resources.network_bandwidth_bps as f64 / 1_048_576.0
    );

    // === DEMONSTRAÇÃO 3: Snapshots de Performance ===
    info!("7. Criando snapshots de performance...");

    // Cria vários snapshots ao longo do tempo
    for i in 0..5 {
        // Executa algumas operações
        let data = format!("Snapshot test {}", i);
        let reader = Box::pin(std::io::Cursor::new(data.into_bytes()));
        backend.add(reader).await?;

        // Registra snapshot
        backend.record_performance_snapshot().await?;
        info!("   • Snapshot {} registrado", i + 1);

        sleep(Duration::from_millis(200)).await;
    }
    info!("   ✓ 5 snapshots criados\n");

    // Obtém histórico de snapshots
    info!("8. Consultando histórico de snapshots...");
    let history = backend.get_performance_history().await;
    info!("   • Total de snapshots: {}", history.len());

    if let Some(first) = history.first() {
        info!("   • Primeiro snapshot:");
        info!(
            "      - Throughput: {:.2} ops/s",
            first.throughput.ops_per_second
        );
        info!("      - Latência: {:.2}ms", first.latency.avg_latency_ms);
    }

    if let Some(last) = history.last() {
        info!("   • Último snapshot:");
        info!(
            "      - Throughput: {:.2} ops/s",
            last.throughput.ops_per_second
        );
        info!("      - Latência: {:.2}ms\n", last.latency.avg_latency_ms);
    }

    // === DEMONSTRAÇÃO 4: Cálculo de Percentis ===
    info!("9. Calculando percentis de latência...");
    let (p95, p99) = backend.calculate_latency_percentiles().await?;
    info!("   • P95 (95% das requisições): {:.2}ms", p95);
    info!("   • P99 (99% das requisições): {:.2}ms\n", p99);

    // === DEMONSTRAÇÃO 5: Relatório Detalhado ===
    info!("10. Gerando relatório detalhado de performance...");
    let report = backend.generate_performance_monitor_report().await;
    info!("{}", report);

    // === DEMONSTRAÇÃO 6: Snapshot Manual ===
    info!("11. Criando snapshot manual...");
    let snapshot = backend.create_performance_snapshot().await;
    info!("   • Timestamp: {:?}", snapshot.timestamp);
    info!(
        "   • Throughput: {:.2} ops/s",
        snapshot.throughput.ops_per_second
    );
    info!("   • Latência: {:.2}ms", snapshot.latency.avg_latency_ms);
    info!("   • CPU: {:.1}%", snapshot.resources.cpu_usage * 100.0);
    info!(
        "   • Memória: {:.2}MB\n",
        snapshot.resources.memory_usage_bytes as f64 / 1_048_576.0
    );

    // === DEMONSTRAÇÃO 7: Reset de Métricas ===
    info!("12. Resetando métricas de performance...");
    backend.reset_performance_metrics().await?;
    info!("   ✓ Métricas resetadas\n");

    // Verifica reset
    info!("13. Verificando estado após reset...");
    let throughput_after = backend.get_throughput_metrics().await;
    let latency_after = backend.get_latency_metrics().await;
    info!(
        "   • Throughput: {:.2} ops/s (resetado)",
        throughput_after.ops_per_second
    );
    info!(
        "   • Latência: {:.2}ms (resetada)",
        latency_after.avg_latency_ms
    );
    info!(
        "   • Histórico: {} snapshots (limpo)\n",
        backend.get_performance_history().await.len()
    );

    // === DEMONSTRAÇÃO 8: Integração com Relatório Geral ===
    info!("14. Gerando relatório completo de performance do backend...");
    let full_report = backend.generate_performance_report().await;
    info!("{}", full_report);

    // Cleanup
    info!("15. Limpeza...");
    tokio::fs::remove_dir_all(&temp_dir).await.ok();
    info!("   ✓ Diretório temporário removido\n");

    info!("=== DEMO CONCLUÍDA ===");
    info!("\nRecursos demonstrados:");
    info!("  ✓ Coleta automática de métricas");
    info!("  ✓ Atualização manual de recursos");
    info!("  ✓ Snapshots de performance");
    info!("  ✓ Cálculo de percentis");
    info!("  ✓ Relatórios detalhados");
    info!("  ✓ Histórico de performance");
    info!("  ✓ Reset de métricas");
    info!("  ✓ Integração com relatório geral");

    Ok(())
}
