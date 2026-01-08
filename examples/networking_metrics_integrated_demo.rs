/// Demonstração do NetworkingMetrics Integrado
///
/// Este exemplo mostra o NetworkingMetrics totalmente integrado no IrohBackend,
/// coletando métricas automaticamente durante operações de networking.
///
/// Execução:
/// ```bash
/// cargo run --example networking_metrics_integrated_demo
/// ```
use guardian_db::p2p::network::config::ClientConfig;
use guardian_db::p2p::network::core::IrohBackend;
use std::io::Cursor;
use std::pin::Pin;
use tokio::io::AsyncRead;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Inicializa logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("╔════════════════════════════════════════════════════════════╗");
    println!("║     NetworkingMetrics Integrado - Demonstração Completa   ║");
    println!("╚════════════════════════════════════════════════════════════╝\n");

    // Configuração do backend
    let temp_dir = std::env::temp_dir().join(format!(
        "iroh_metrics_demo_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    ));

    let client_config = ClientConfig {
        data_store_path: Some(temp_dir.clone()),
        ..Default::default()
    };

    println!("Inicializando IrohBackend com NetworkingMetrics...");
    let backend = IrohBackend::new(&client_config).await?;
    println!("✓ Backend inicializado com sucesso!\n");

    // === TESTE 1: Operações de ADD ===
    println!("═══════════════════════════════════════════════");
    println!("    TESTE 1: Operações ADD com Coleta de Métricas");
    println!("═══════════════════════════════════════════════\n");

    for i in 1..=5 {
        let data = format!("Dados de teste #{} - Networking Metrics Ativo", i);
        let reader: Pin<Box<dyn AsyncRead + Send>> =
            Box::pin(Cursor::new(data.as_bytes().to_vec()));

        let response = backend.add(reader).await?;
        println!(
            "✓ ADD #{}: Hash = {} ({} bytes)",
            i,
            &response.hash[..16],
            response.size
        );
    }

    // Exibe métricas após ADDs
    let metrics = backend.get_networking_metrics().await?;
    println!("\nMétricas após ADDs:");
    println!(
        "   • Operações add: {}",
        metrics.backend_metrics.add_operations
    );
    println!(
        "   • Tempo médio add: {:.2}ms",
        metrics.backend_metrics.avg_add_time_ms
    );

    // === TESTE 2: Operações de CAT ===
    println!("\n═══════════════════════════════════════════════");
    println!("    TESTE 2: Operações CAT com Coleta de Métricas");
    println!("═══════════════════════════════════════════════\n");

    // Re-adiciona dados para ter hashes conhecidos
    let test_data = b"Dados para teste CAT";
    let reader: Pin<Box<dyn AsyncRead + Send>> = Box::pin(Cursor::new(test_data.to_vec()));
    let add_response = backend.add(reader).await?;
    println!("✓ Dados preparados: Hash = {}", &add_response.hash[..16]);

    // Executa múltiplas operações CAT
    for i in 1..=5 {
        let _ = backend.cat(&add_response.hash).await?;
        println!("✓ CAT #{}: Recuperado com sucesso", i);
    }

    // Exibe métricas após CATs
    let metrics = backend.get_networking_metrics().await?;
    println!("\nMétricas após CATs:");
    println!(
        "   • Operações cat: {}",
        metrics.backend_metrics.cat_operations
    );
    println!(
        "   • Tempo médio cat: {:.2}ms",
        metrics.backend_metrics.avg_cat_time_ms
    );
    println!(
        "   • Cache hit rate: {:.1}%",
        metrics.backend_metrics.cache_hit_rate
    );

    // === TESTE 3: Conectividade ===
    println!("\n═══════════════════════════════════════════════");
    println!("    TESTE 3: Métricas de Conectividade");
    println!("═══════════════════════════════════════════════\n");

    let node_info = backend.id().await?;
    println!("✓ NodeId: {}", node_info.id);
    println!("✓ Agent: {}", node_info.agent_version);

    let metrics = backend.get_networking_metrics().await?;
    println!("\nMétricas de Conectividade:");
    println!(
        "   • Total conexões: {}",
        metrics.connectivity.total_connections
    );
    println!(
        "   • Peers conectados: {}",
        metrics.connectivity.connected_peers
    );
    println!(
        "   • Latência média: {:.2}ms",
        metrics.connectivity.avg_peer_latency_ms
    );

    // === RELATÓRIO COMPLETO ===
    println!("\n═══════════════════════════════════════════════");
    println!("    RELATÓRIO COMPLETO DE MÉTRICAS");
    println!("═══════════════════════════════════════════════");

    let report = backend.generate_networking_report().await;
    println!("{}", report);

    // === EXPORTAÇÃO JSON ===
    println!("═══════════════════════════════════════════════");
    println!("    EXPORTAÇÃO JSON");
    println!("═══════════════════════════════════════════════\n");

    let json = backend.export_networking_metrics_json().await?;
    println!("    Métricas exportadas como JSON:");
    println!("{}\n", json);

    // === MÉTRICAS BACKEND ===
    println!("═══════════════════════════════════════════════");
    println!("    MÉTRICAS DE PERFORMANCE DO BACKEND");
    println!("═══════════════════════════════════════════════\n");

    let backend_metrics = backend.metrics().await?;
    println!("   Performance Geral:");
    println!("   • Ops/segundo: {:.2}", backend_metrics.ops_per_second);
    println!(
        "   • Latência média: {:.2}ms",
        backend_metrics.avg_latency_ms
    );
    println!("   • Total operações: {}", backend_metrics.total_operations);
    println!(
        "   • Taxa de erro: {:.2}%",
        if backend_metrics.total_operations > 0 {
            (backend_metrics.error_count as f64 / backend_metrics.total_operations as f64) * 100.0
        } else {
            0.0
        }
    );

    // === CACHE STATISTICS ===
    let cache_stats = backend.get_cache_statistics().await?;
    println!("\nEstatísticas de Cache:");
    println!("   • Hit ratio: {:.1}%", cache_stats.hit_ratio * 100.0);
    println!(
        "   • Total bytes: {:.2}MB",
        cache_stats.total_size_bytes as f64 / 1_048_576.0
    );

    println!("\n╔════════════════════════════════════════════════════════════╗");
    println!("║          Demonstração Completa - NetworkingMetrics         ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!("\nRecursos Demonstrados:");
    println!("   ✓ Coleta automática de métricas em add()");
    println!("   ✓ Coleta automática de métricas em cat()");
    println!("   ✓ Coleta automática de métricas em connect()");
    println!("   ✓ Métricas de conectividade P2P");
    println!("   ✓ Métricas de performance Iroh");
    println!("   ✓ Relatório detalhado formatado");
    println!("   ✓ Exportação JSON para integração");
    println!("   ✓ Integração com BackendMetrics");
    println!("   ✓ Integração com OptimizedCache\n");

    // Limpeza
    if temp_dir.exists() {
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }

    Ok(())
}
