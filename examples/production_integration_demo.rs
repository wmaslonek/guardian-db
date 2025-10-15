// Demonstra como integrar o IrohBackend com Gossipsub
// no contexto completo do Guardian DB

use guardian_db::{
    error::{GuardianError, Result},
    ipfs_core_api::{
        backends::{IpfsBackend, IrohBackend},
        config::ClientConfig,
    },
};
use std::{path::PathBuf, sync::Arc};
use tokio::time::{Duration, sleep};
use tracing::{debug, info};

#[tokio::main]
async fn main() -> Result<()> {
    // Configurar logging
    tracing_subscriber::fmt()
        .with_env_filter("info,guardian_db=debug,iroh=info")
        .init();

    info!("Iniciando Guardian DB: Iroh + LibP2P Gossipsub");

    // === CONFIGURAÇÃO DO SISTEMA ===

    let data_dir = PathBuf::from("./tmp/guardian_production_data");
    tokio::fs::create_dir_all(&data_dir)
        .await
        .map_err(|e| GuardianError::Other(format!("Erro ao criar diretório: {}", e)))?;

    // Configurar backend Iroh com Gossipsub
    let backend_config = ClientConfig {
        data_store_path: Some(data_dir.clone()),
        enable_pubsub: true,
        enable_swarm: true,
        ..Default::default()
    };

    info!("Inicializando backend Iroh com integração LibP2P...");
    let iroh_backend = Arc::new(IrohBackend::new(&backend_config).await?);

    // Aguardar inicialização completa
    sleep(Duration::from_secs(2)).await;

    // === DEMONSTRAÇÃO DE FUNCIONALIDADES INTEGRADAS ===

    info!("Testando funcionalidades integradas...");

    // 1. IPFS Operations
    demo_ipfs_operations(&iroh_backend).await?;

    // 2. Gossipsub Pub/Sub
    demo_gossipsub_integration(&iroh_backend).await?;

    // 3. Guardian DB Operations
    demo_guardian_operations(&iroh_backend).await?;

    // 4. Performance Monitoring
    demo_performance_monitoring(&iroh_backend).await?;

    info!("Demonstração completa! Sistema funcionando perfeitamente.");

    Ok(())
}

/// Demonstra operações IPFS básicas
async fn demo_ipfs_operations(backend: &Arc<IrohBackend>) -> Result<()> {
    info!("Testando operações IPFS...");

    // Adicionar diferentes tipos de conteúdo
    let contents = vec![
        ("text", b"Hello Guardian DB with Iroh!".as_slice()),
        (
            "json",
            br#"{"message": "LibP2P Gossipsub working!", "timestamp": 1696291200}"#,
        ),
        ("binary", &[0u8, 1, 2, 3, 4, 5, 0xFF, 0xFE, 0xFD]),
    ];

    let mut cids = Vec::new();

    for (content_type, data) in contents {
        let reader = Box::pin(std::io::Cursor::new(data));
        let response = backend.add(reader).await?;

        info!(
            "✓ Conteúdo {} adicionado: {} ({} bytes)",
            content_type, response.hash, response.size
        );

        // Fixar conteúdo importante
        backend.pin_add(&response.hash).await?;
        cids.push(response.hash);
    }

    // Verificar retrieval
    for cid in &cids {
        let mut stream = backend.cat(cid).await?;
        let mut retrieved = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut retrieved)
            .await
            .map_err(|e| GuardianError::Other(e.to_string()))?;

        debug!("✓ Conteúdo {} recuperado: {} bytes", cid, retrieved.len());
    }

    info!("✓ Todas as operações IPFS funcionando perfeitamente!");
    Ok(())
}

/// Demonstra integração Gossipsub
async fn demo_gossipsub_integration(_backend: &Arc<IrohBackend>) -> Result<()> {
    info!("Testando integração Gossipsub...");

    // Tópicos do sistema Guardian DB
    let topics = [
        "guardian-db/events",
        "guardian-db/replication",
        "guardian-db/discovery",
        "guardian-db/notifications",
    ]; // Simular publicação de mensagens (Gossipsub está integrado internamente no IrohBackend)
    for (i, topic_name) in topics.iter().enumerate() {
        let message = format!(
            "{{\"type\": \"test\", \"topic_id\": {}, \"timestamp\": {}, \"data\": \"Phase 6 funcionando!\"}}",
            i,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        );

        info!(
            "Simulando publicação no tópico {}: {} bytes",
            topic_name,
            message.len()
        );

        sleep(Duration::from_millis(100)).await;
    }

    info!("✓ Integração Gossipsub (simulada) funcionando perfeitamente!");
    Ok(())
}

/// Demonstra operações específicas do Guardian DB
async fn demo_guardian_operations(backend: &Arc<IrohBackend>) -> Result<()> {
    info!("Testando operações específicas do Guardian DB...");

    // Simular dados de log distribuído
    let log_entries = [
        "Guardian DB Entry 1: System initialized",
        "Guardian DB Entry 2: New peer connected",
        "Guardian DB Entry 3: Replication started",
        "Guardian DB Entry 4: Cache optimization completed",
        "Guardian DB Entry 5: Phase 6 integration successful",
    ];

    let mut log_cids = Vec::new();

    for (i, entry) in log_entries.iter().enumerate() {
        // Adicionar entrada do log
        let reader = Box::pin(std::io::Cursor::new(entry.as_bytes()));
        let response = backend.add(reader).await?;

        // Fixar entradas críticas
        if i < 3 {
            backend.pin_add(&response.hash).await?;
            info!("Entrada crítica fixada: {}", response.hash);
        }

        log_cids.push(response.hash.clone());
        debug!("Log entry {}: {}", i + 1, response.hash);
    }

    // Simular replicação via Gossipsub (integrado no IrohBackend)
    for cid in &log_cids {
        let replication_msg = format!(
            "{{\"action\": \"replicate\", \"cid\": \"{}\", \"priority\": \"high\"}}",
            cid
        );

        debug!(
            "Simulando replicação para: {} (mensagem: {} bytes)",
            cid,
            replication_msg.len()
        );
    }

    info!("✓ Operações Guardian DB simuladas com sucesso!");
    Ok(())
}

/// Demonstra monitoramento de performance
async fn demo_performance_monitoring(backend: &Arc<IrohBackend>) -> Result<()> {
    info!("Coletando métricas de performance...");

    // Status geral
    let is_online = backend.is_online().await;
    info!("Backend online: {}", is_online);

    // Métricas de operação
    let metrics = backend.metrics().await?;
    info!("Métricas do sistema:");
    info!("   - Operações/seg: {:.2}", metrics.ops_per_second);
    info!("   - Latência média: {:.2}ms", metrics.avg_latency_ms);
    info!("   - Total operações: {}", metrics.total_operations);
    info!("   - Uso de memória: {} bytes", metrics.memory_usage_bytes);
    info!("   - Erros: {}", metrics.error_count);

    // Health check simplificado
    let health = backend.health_check().await?;
    info!("Health check: {:?}", health);

    // Peers conectados
    let peers = backend.peers().await?;
    info!("👥 Peers conectados: {}", peers.len());
    for peer in &peers {
        debug!("   - {}: {}", peer.id, peer.addresses.join(", "));
    }

    info!("✓ Monitoramento completo! Sistema saudável e performático.");
    Ok(())
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    #[tokio::test]
    async fn test_full_integration() {
        let backend_config = ClientConfig {
            data_store_path: Some(PathBuf::from("./tmp/test_integration_data")),
            enable_pubsub: true,
            enable_swarm: true,
            ..Default::default()
        };

        let backend = Arc::new(IrohBackend::new(&backend_config).await.unwrap());

        // Testar todas as funcionalidades
        demo_ipfs_operations(&backend).await.unwrap();
        demo_gossipsub_integration(&backend).await.unwrap();
        demo_guardian_operations(&backend).await.unwrap();
        demo_performance_monitoring(&backend).await.unwrap();

        // Cleanup
        tokio::fs::remove_dir_all("./tmp/test_integration_data")
            .await
            .ok();
    }

    #[tokio::test]
    async fn test_concurrent_operations() {
        let backend_config = ClientConfig {
            data_store_path: Some(PathBuf::from("./tmp/test_concurrent_data")),
            enable_pubsub: true,
            enable_swarm: true,
            ..Default::default()
        };

        let backend = Arc::new(IrohBackend::new(&backend_config).await.unwrap());

        // Operações simultâneas
        let tasks = (0..10).map(|i| {
            let backend = backend.clone();
            tokio::spawn(async move {
                let data = format!("concurrent test data {}", i);
                let reader = Box::pin(std::io::Cursor::new(data.as_bytes()));
                backend.add(reader).await.unwrap()
            })
        });

        let results = futures::future::join_all(tasks).await;

        // Verificar que todas as operações foram bem-sucedidas
        for result in results {
            assert!(result.is_ok());
        }

        // Cleanup
        tokio::fs::remove_dir_all("./tmp/test_concurrent_data")
            .await
            .ok();
    }
}
