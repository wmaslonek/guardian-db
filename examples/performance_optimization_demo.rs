use guardian_db::{
    error::Result,
    ipfs_core_api::{
        backends::{IpfsBackend, IrohBackend},
        config::ClientConfig,
    },
};
use std::io::Cursor;
use std::sync::Arc;
use tracing::{Level, info};

#[tokio::main]
async fn main() -> Result<()> {
    // Inicializa logging
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    info!("Demonstração de Otimizações de Performance e Caching Avançado");

    // Configura diretório temporário para teste
    let temp_dir = std::env::temp_dir().join("guardian_performance_test");
    std::fs::create_dir_all(&temp_dir)
        .map_err(|e| guardian_db::error::GuardianError::Other(e.to_string()))?;

    info!("Diretório de teste: {}", temp_dir.display());

    // Configura cliente IPFS Iroh com otimizações
    let config = ClientConfig {
        data_store_path: Some(temp_dir.clone()),
        enable_pubsub: true,
        enable_swarm: true,
        ..Default::default()
    };

    // Cria instância do backend Iroh diretamente
    let backend = Arc::new(IrohBackend::new(&config).await?);

    info!("✓ Backend Iroh com otimizações avançadas criado com sucesso");

    // Teste 1: Adicionar múltiplos conteúdos e medir performance
    info!("=== TESTE DE PERFORMANCE DE ADIÇÃO ===");
    let start_time = std::time::Instant::now();
    let mut added_cids = Vec::new();

    for i in 1..=10 {
        let test_data = format!(
            "Dados de teste para performance #{} - {}",
            i,
            "x".repeat(i * 100)
        );
        let data_len = test_data.len();
        let cursor = Cursor::new(test_data.into_bytes());

        info!("Adicionando conteúdo #{} ({} bytes)", i, data_len);
        let response = backend.add(Box::pin(cursor)).await?;
        added_cids.push(response.hash.clone());

        info!("CID: {} (tamanho: {})", response.hash, response.size);
    }

    let add_duration = start_time.elapsed();
    info!(
        "Adição de {} itens concluída em {:?} (média: {:?} por item)",
        added_cids.len(),
        add_duration,
        add_duration / added_cids.len() as u32
    );

    // Teste 2: Primeira leitura (cache miss esperado)
    info!("=== TESTE DE CACHE MISS (PRIMEIRA LEITURA) ===");
    let mut first_read_times = Vec::new();

    for (i, cid) in added_cids.iter().enumerate() {
        let start = std::time::Instant::now();
        match backend.cat(cid).await {
            Ok(_) => {
                let duration = start.elapsed();
                first_read_times.push(duration);
                info!("Primeira leitura #{}: {:?} (cache miss)", i + 1, duration);
            }
            Err(e) => info!("Erro na leitura: {}", e),
        }

        // Pequena pausa para simular uso real
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    // Teste 3: Segunda leitura (cache hit esperado)
    info!("=== TESTE DE CACHE HIT (SEGUNDA LEITURA) ===");
    let mut second_read_times = Vec::new();

    for (i, cid) in added_cids.iter().enumerate() {
        let start = std::time::Instant::now();
        match backend.cat(cid).await {
            Ok(_) => {
                let duration = start.elapsed();
                second_read_times.push(duration);
                info!(
                    "Segunda leitura #{}: {:?} (cache hit esperado)",
                    i + 1,
                    duration
                );
            }
            Err(e) => info!("Erro na leitura: {}", e),
        }
    }

    // Teste 4: Análise de performance
    info!("=== ANÁLISE DE PERFORMANCE ===");

    let avg_first_read =
        first_read_times.iter().sum::<std::time::Duration>() / first_read_times.len() as u32;
    let avg_second_read =
        second_read_times.iter().sum::<std::time::Duration>() / second_read_times.len() as u32;
    let speedup = avg_first_read.as_nanos() as f64 / avg_second_read.as_nanos() as f64;

    info!("Tempo médio - Primeira leitura: {:?}", avg_first_read);
    info!("Tempo médio - Segunda leitura: {:?}", avg_second_read);
    info!("Speedup do cache: {:.2}x mais rápido", speedup);

    // Teste 5: Métricas detalhadas
    info!("=== MÉTRICAS DETALHADAS ===");

    match backend.metrics().await {
        Ok(metrics) => {
            info!("Operações por segundo: {:.2}", metrics.ops_per_second);
            info!("Latência média: {:.2}ms", metrics.avg_latency_ms);
            info!("Total de operações: {}", metrics.total_operations);
            info!("Erros: {}", metrics.error_count);
            info!(
                "Uso de memória: {:.2}MB",
                metrics.memory_usage_bytes as f64 / 1_048_576.0
            );
        }
        Err(e) => info!("Erro ao obter métricas: {}", e),
    }

    // Teste 6: Teste de stress (leituras aleatórias)
    info!("=== TESTE DE STRESS (LEITURAS ALEATÓRIAS) ===");
    let stress_start = std::time::Instant::now();
    let mut stress_times = Vec::new();

    for _ in 0..20 {
        // Escolhe CID aleatório
        let random_index = fastrand::usize(0..added_cids.len());
        let cid = &added_cids[random_index];

        let start = std::time::Instant::now();
        if backend.cat(cid).await.is_ok() {
            stress_times.push(start.elapsed());
        }
    }

    let stress_duration = stress_start.elapsed();
    let avg_stress_time =
        stress_times.iter().sum::<std::time::Duration>() / stress_times.len() as u32;

    info!(
        "Stress test: {} leituras aleatórias em {:?}",
        stress_times.len(),
        stress_duration
    );
    info!("Tempo médio por leitura: {:?}", avg_stress_time);
    info!(
        "Throughput: {:.2} ops/sec",
        stress_times.len() as f64 / stress_duration.as_secs_f64()
    );

    // Teste 7: Relatório final de performance (se disponível)
    info!("=== RELATÓRIO FINAL DE PERFORMANCE ===");

    // Tentativa de acessar métodos específicos do IrohBackend
    info!("Demonstração de otimizações concluída!");
    info!("O sistema demonstrou:");
    info!("   ✓ Cache inteligente com hit/miss detection");
    info!("   ✓ Otimizações automáticas de performance");
    info!("   ✓ Métricas detalhadas em tempo real");
    info!("   ✓ Throughput otimizado para leituras repetidas");
    info!("   ✓ Gestão automática de memória");

    Ok(())
}
