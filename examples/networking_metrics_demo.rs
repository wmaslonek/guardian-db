// Exemplo demonstrando métricas avançadas em ação
//
// Mostra como as métricas orientam decisões de otimização

use guardian_db::{
    guardian::error::Result,
    p2p::network::{config::ClientConfig, core::IrohBackend},
};
use std::{sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio::time::sleep;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info,guardian_db=debug")
        .init();

    info!("Demonstração: Métricas Avançadas Orientando Otimizações");

    // === SETUP ===
    let temp_dir = TempDir::new()
        .map_err(|e| guardian_db::guardian::error::GuardianError::Other(e.to_string()))?;

    let client_config = ClientConfig {
        data_store_path: Some(temp_dir.path().to_path_buf()),
        enable_pubsub: true,
        ..Default::default()
    };

    let backend = Arc::new(IrohBackend::new(&client_config).await?);
    sleep(Duration::from_secs(2)).await;

    // === CENÁRIO 1: BASELINE - SISTEMA NOVO ===
    info!("CENÁRIO 1: Baseline - Sistema recém-inicializado");

    let node_info = backend.id().await?;
    info!("Node ID: {}", node_info.id);
    info!("Status: Inicializado");

    // === CENÁRIO 2: CARGA DE TRABALHO TÍPICA ===
    info!("CENÁRIO 2: Simulando carga de trabalho típica...");

    simulate_typical_workload(&backend).await?;

    // Verificar estatísticas do repositório
    let repo_stats = backend.repo_stat().await?;
    info!(
        "Repo Stats - Objetos: {}, Tamanho: {} bytes",
        repo_stats.num_objects, repo_stats.repo_size
    );

    // === ANÁLISE AUTOMATIZADA DE PERFORMANCE ===
    info!("CENÁRIO 3: Análise automatizada de performance");

    let performance_recommendations = analyze_performance(&backend).await?;
    println!(
        "\nRECOMENDAÇÕES BASEADAS EM MÉTRICAS:\n{}",
        performance_recommendations
    );

    // === CENÁRIO 4: MONITORAMENTO CONTÍNUO ===
    info!("CENÁRIO 4: Monitoramento contínuo (30 segundos)");

    for i in 1..=6 {
        sleep(Duration::from_secs(5)).await;

        // Simular atividade contínua
        let data = format!("Continuous monitoring data {}", i);
        let data_bytes = data.into_bytes();
        let reader = Box::pin(std::io::Cursor::new(data_bytes));
        backend.add(reader).await?;

        // Verificar peers e status
        let peers = backend.peers().await?;
        let health = backend.health_check().await?;

        info!(
            "  T+{}s - Peers: {}, Saudável: {}",
            i * 5,
            peers.len(),
            health.healthy
        );
    }

    // === EXPORTAR DADOS PARA ANÁLISE ===
    info!("Exportando métricas para análise externa...");

    let final_stats = backend.repo_stat().await?;
    let health_status = backend.health_check().await?;

    let export_data = format!(
        r#"{{"repo_size": {}, "num_objects": {}, "healthy": {}}}"#,
        final_stats.repo_size, final_stats.num_objects, health_status.healthy
    );

    tokio::fs::write("metrics_export.json", export_data)
        .await
        .map_err(|e| guardian_db::guardian::error::GuardianError::Other(e.to_string()))?;

    info!("Métricas exportadas para metrics_export.json");

    info!("Demonstração completa! Métricas fornecendo insights valiosos.");

    Ok(())
}

/// Simula uma carga de trabalho típica para gerar métricas realistas
async fn simulate_typical_workload(backend: &Arc<IrohBackend>) -> Result<()> {
    info!("Simulando operações Iroh variadas...");

    // Diferentes tamanhos de dados
    let datasets = vec![
        ("small", vec![0u8; 1024]),        // 1KB
        ("medium", vec![1u8; 64 * 1024]),  // 64KB
        ("large", vec![2u8; 1024 * 1024]), // 1MB
    ];

    for (name, data) in datasets {
        let data_len = data.len();
        let reader = Box::pin(std::io::Cursor::new(data));
        let response = backend.add(reader).await?;

        // Simular retrieval
        let mut stream = backend.cat(&response.hash).await?;
        let mut retrieved = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut retrieved)
            .await
            .map_err(|e| guardian_db::guardian::error::GuardianError::Other(e.to_string()))?;

        info!("Processado dataset {}: {} bytes", name, data_len);

        // Pequena pausa entre operações
        sleep(Duration::from_millis(100)).await;
    }

    // Simular operações de rede
    info!("Simulando operações de rede...");

    let peers = backend.peers().await?;
    info!("Peers conectados: {}", peers.len());

    // Testar health check
    let health = backend.health_check().await?;
    info!(
        "Health Status: {}",
        if health.healthy {
            "Saudável"
        } else {
            "Degradado"
        }
    );

    Ok(())
}

/// Analisa métricas e fornece recomendações automatizadas
async fn analyze_performance(backend: &Arc<IrohBackend>) -> Result<String> {
    let stats = backend.repo_stat().await?;
    let health = backend.health_check().await?;
    let peers = backend.peers().await?;

    let mut recommendations = Vec::new();

    // Análise de conectividade
    if peers.len() < 3 {
        recommendations.push(format!(
            "⚠ POUCOS PEERS: {}\n   > Implementar discovery protocol customizado\n   > Adicionar bootstrap nodes",
            peers.len()
        ));
    }

    // Análise de health
    if health.healthy {
        recommendations
            .push("✓ SISTEMA SAUDÁVEL\n   > Continuar monitoramento preventivo".to_string());
    } else {
        recommendations.push(format!(
            "⚠ SISTEMA COM PROBLEMAS: {}\n   > Investigar componentes com falha\n   > Verificar conectividade",
            health.message
        ));
    }

    // Análise de armazenamento
    if stats.repo_size > 1_000_000_000 {
        // > 1GB
        recommendations.push(format!(
            "⚠ REPOSITÓRIO GRANDE: {} bytes\n   > Considerar garbage collection\n   > Implementar políticas de retenção", 
            stats.repo_size
        ));
    }

    if stats.num_objects > 10_000 {
        recommendations.push(format!(
            "⚠ MUITOS OBJETOS: {}\n   > Otimizar indexação\n   > Considerar compactação",
            stats.num_objects
        ));
    }

    // Priorização estratégica
    if recommendations.len() <= 1 {
        recommendations.push("✓ PERFORMANCE EXCELENTE! Sistema otimizado.\n   > Considerar métricas avançadas de networking para monitoramento preventivo".to_string());
    } else {
        recommendations.insert(0, "PRÓXIMAS OTIMIZAÇÕES POR PRIORIDADE:".to_string());

        // Adicionar prioridade baseada em impacto
        if peers.len() < 2 {
            recommendations.insert(
                1,
                "\nPRIORIDADE MÁXIMA: Melhorar conectividade P2P".to_string(),
            );
        } else if stats.repo_size > 1_000_000_000 {
            recommendations.insert(
                1,
                "\nPRIORIDADE ALTA: Otimização de armazenamento".to_string(),
            );
        }
    }

    Ok(recommendations.join("\n"))
}

#[cfg(test)]
mod metrics_tests {
    use super::*;

    #[tokio::test]
    async fn test_metrics_collection() {
        let temp_dir = TempDir::new().unwrap();

        let client_config = ClientConfig {
            data_store_path: Some(temp_dir.path().to_path_buf()),
            enable_pubsub: true,
            ..Default::default()
        };

        let backend = Arc::new(IrohBackend::new(&client_config).await.unwrap());

        // Executar carga de trabalho
        simulate_typical_workload(&backend).await.unwrap();

        // Verificar métricas básicas
        let stats = backend.repo_stat().await.unwrap();
        let health = backend.health_check().await.unwrap();

        assert!(stats.num_objects >= 0);
        assert!(health.healthy);
    }
}
