/// Demonstração do Backend Híbrido (Iroh + LibP2P)
///
/// Este exemplo mostra como usar o novo backend híbrido que combina:
/// - Iroh para operações de conteúdo IPFS (add, cat, pin)
/// - LibP2P dedicado para comunicação P2P (PubSub, Gossipsub)
///
/// Benefícios:
/// - Eliminação de dependências externas
/// - Performance nativa em Rust
/// - Controle total sobre a rede P2P
/// - Compartilhamento de PeerId entre sistemas
use guardian_db::{
    error::{GuardianError, Result},
    ipfs_core_api::{
        backends::{BackendFactory, BackendType, HybridBackend, IpfsBackend},
        config::ClientConfig,
    },
};
use std::io::Cursor;
use std::path::PathBuf;
use tokio::io::AsyncReadExt;
use tracing::{Level, error, info, warn};
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> Result<()> {
    // Configurar logging para ver a interação entre Iroh e LibP2P
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::DEBUG)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Configuração de logging");

    info!("=== Demonstração do Backend Híbrido Iroh+LibP2P ===");

    // Demonstrar diferentes modos de criação do backend
    demo_backend_creation().await?;

    // Demonstrar operações de conteúdo via Iroh
    demo_content_operations().await?;

    // Demonstrar operações de rede híbridas
    demo_network_operations().await?;

    // Demonstrar sincronização de PeerId
    demo_peer_id_sync().await?;

    // Demonstrar health checks combinados
    demo_health_monitoring().await?;

    info!("=== Demonstração Concluída ===");
    Ok(())
}

/// Demonstra diferentes formas de criar o backend híbrido
async fn demo_backend_creation() -> Result<()> {
    info!("\n--- Criação do Backend Híbrido ---");

    // Configuração para backend híbrido
    let config = ClientConfig {
        data_store_path: Some(PathBuf::from("./tmp/hybrid_demo_data")), // Storage local
        enable_pubsub: true,
        enable_swarm: true,
        ..Default::default()
    };

    // Método 1: Criação explícita do backend híbrido
    info!("Criando backend híbrido explicitamente...");
    match HybridBackend::new(&config).await {
        Ok(backend) => {
            let info = backend.id().await?;
            info!(
                "Backend híbrido criado! PeerId: {}, Agent: {}",
                info.id, info.agent_version
            );

            // Verificar que está usando os dois sistemas
            let health = backend.health_check().await?;
            info!("Health check: {} checks realizados", health.checks.len());
            for check in &health.checks {
                info!(
                    "  - {}: {} - {}",
                    check.name,
                    if check.passed { "✓" } else { "✗" },
                    check.message
                );
            }
        }
        Err(e) => {
            warn!("Backend híbrido não disponível ainda: {}", e);
            info!("Em Desenvolvimento");
        }
    }

    // Método 2: Auto-detecção (prioriza híbrido)
    info!("Testando auto-detecção de backend...");
    match BackendFactory::auto_detect_backend(&config).await {
        Ok(backend) => {
            let backend_type = backend.backend_type();
            info!("Auto-detecção escolheu: {}", backend_type.as_str());
        }
        Err(e) => {
            warn!("Auto-detecção falhou: {}", e);
            info!("Isso é esperado durante o desenvolvimento");
        }
    }

    // Método 3: Criação via factory com tipo específico
    info!("Testando criação via factory...");
    match BackendFactory::create_backend(BackendType::Hybrid, &config).await {
        Ok(backend) => {
            info!("Backend híbrido criado via factory");
            let metrics = backend.metrics().await?;
            info!("Métricas iniciais: {} operações", metrics.total_operations);
        }
        Err(e) => {
            warn!("Factory falhou: {}", e);
        }
    }

    Ok(())
}

/// Demonstra operações de conteúdo delegadas para Iroh
async fn demo_content_operations() -> Result<()> {
    info!("\n--- Operações de Conteúdo via Iroh ---");

    let config = ClientConfig {
        data_store_path: Some(PathBuf::from("./tmp/hybrid_demo_content")),
        enable_pubsub: true,
        enable_swarm: true,
        ..Default::default()
    };

    // Tentar criar backend híbrido
    match HybridBackend::new(&config).await {
        Ok(backend) => {
            info!("Backend híbrido disponível, testando operações de conteúdo...");

            // Adicionar conteúdo
            let test_data = b"Hello from Hybrid Backend!";
            let cursor = Box::pin(Cursor::new(test_data.to_vec()));

            match backend.add(cursor).await {
                Ok(response) => {
                    info!("Conteúdo adicionado com CID: {}", response.hash);

                    // Recuperar conteúdo
                    match backend.cat(&response.hash).await {
                        Ok(mut reader) => {
                            let mut content = Vec::new();
                            reader.read_to_end(&mut content).await.map_err(|e| {
                                GuardianError::Other(format!("Erro lendo conteúdo: {}", e))
                            })?;

                            let content_str = String::from_utf8_lossy(&content);
                            info!("Conteúdo recuperado: {}", content_str);

                            // Pin do conteúdo
                            if let Err(e) = backend.pin_add(&response.hash).await {
                                warn!("Erro fazendo pin: {}", e);
                            } else {
                                info!("Conteúdo fixado com sucesso");

                                // Listar pins
                                match backend.pin_ls().await {
                                    Ok(pins) => {
                                        info!("Pins ativos: {}", pins.len());
                                        for pin in pins {
                                            info!("  - {}: {:?}", pin.cid, pin.pin_type);
                                        }
                                    }
                                    Err(e) => warn!("Erro listando pins: {}", e),
                                }
                            }
                        }
                        Err(e) => warn!("Erro recuperando conteúdo: {}", e),
                    }
                }
                Err(e) => warn!("Erro adicionando conteúdo: {}", e),
            }
        }
        Err(e) => {
            warn!(
                "Backend híbrido não disponível para teste de conteúdo: {}",
                e
            );
        }
    }

    Ok(())
}

/// Demonstra operações de rede híbridas (Iroh + LibP2P)
async fn demo_network_operations() -> Result<()> {
    info!("\n--- Operações de Rede Híbridas ---");

    let config = ClientConfig {
        data_store_path: Some(PathBuf::from("./tmp/hybrid_demo_network")),
        enable_pubsub: true,
        enable_swarm: true,
        ..Default::default()
    };

    match HybridBackend::new(&config).await {
        Ok(backend) => {
            info!("Testando funcionalidades de rede híbridas...");

            // Obter informações do nó
            let node_info = backend.id().await?;
            info!("Nó híbrido - PeerId: {}", node_info.id);
            info!("Agent: {}", node_info.agent_version);
            info!("Endereços: {:?}", node_info.addresses);

            // Listar peers (combinação de Iroh + LibP2P)
            match backend.peers().await {
                Ok(peers) => {
                    info!("Peers conectados: {}", peers.len());
                    for peer in peers {
                        info!("  - {}: {} endereços", peer.id, peer.addresses.len());
                    }
                }
                Err(e) => warn!("Erro obtendo peers: {}", e),
            }

            // Verificar status online
            let is_online = backend.is_online().await;
            info!("Status online: {}", if is_online { "✓" } else { "✗" });
        }
        Err(e) => {
            warn!("Backend híbrido não disponível para teste de rede: {}", e);
        }
    }

    Ok(())
}

/// Demonstra a sincronização de PeerId entre Iroh e LibP2P
async fn demo_peer_id_sync() -> Result<()> {
    info!("\n--- Sincronização de PeerId ---");

    let config = ClientConfig {
        data_store_path: Some(PathBuf::from("./tmp/hybrid_demo_sync")),
        enable_pubsub: true,
        enable_swarm: true,
        ..Default::default()
    };

    match HybridBackend::new(&config).await {
        Ok(backend) => {
            info!("Verificando sincronização de PeerId...");

            // O health check já verifica sincronização
            let health = backend.health_check().await?;

            // Procurar especificamente pelo check de sincronização
            if let Some(sync_check) = health
                .checks
                .iter()
                .find(|check| check.name == "peer_id_sync")
            {
                if sync_check.passed {
                    info!("✓ PeerId sincronizado entre Iroh e LibP2P");
                } else {
                    error!("✗ Problema de sincronização: {}", sync_check.message);
                }
            } else {
                warn!("Check de sincronização não encontrado");
            }

            // Obter PeerId atual
            let node_info = backend.id().await?;
            info!("PeerId ativo: {}", node_info.id);
        }
        Err(e) => {
            warn!(
                "Backend híbrido não disponível para teste de sincronização: {}",
                e
            );
        }
    }

    Ok(())
}

/// Demonstra monitoramento de saúde combinado
async fn demo_health_monitoring() -> Result<()> {
    info!("\n--- Monitoramento de Saúde Combinado ---");

    let config = ClientConfig {
        data_store_path: Some(PathBuf::from("./tmp/hybrid_demo_health")),
        enable_pubsub: true,
        enable_swarm: true,
        ..Default::default()
    };

    match HybridBackend::new(&config).await {
        Ok(backend) => {
            info!("Realizando health check completo...");

            let health = backend.health_check().await?;

            info!("=== Status Geral ===");
            info!(
                "Saúde: {}",
                if health.healthy {
                    "✓ Saudável"
                } else {
                    "✗ Problemas"
                }
            );
            info!("Tempo de resposta: {}ms", health.response_time_ms);
            info!("Mensagem: {}", health.message);

            info!("=== Checks Individuais ===");
            for check in &health.checks {
                let status = if check.passed { "✓" } else { "✗" };
                info!("{} {}: {}", status, check.name, check.message);
            }

            // Obter métricas combinadas
            let metrics = backend.metrics().await?;
            info!("=== Métricas Combinadas ===");
            info!("Operações/segundo: {:.2}", metrics.ops_per_second);
            info!("Latência média: {:.2}ms", metrics.avg_latency_ms);
            info!("Total de operações: {}", metrics.total_operations);
            info!("Erros: {}", metrics.error_count);
            info!("Uso de memória: {} bytes", metrics.memory_usage_bytes);
        }
        Err(e) => {
            warn!("Backend híbrido não disponível para monitoramento: {}", e);
            info!(
                "Nota: Durante o desenvolvimento, alguns componentes podem não estar implementados"
            );
        }
    }

    Ok(())
}

/// Configuração de exemplo para produção
#[allow(dead_code)]
fn production_config_example() -> ClientConfig {
    ClientConfig {
        // Diretório persistente para dados
        data_store_path: Some(PathBuf::from("/tmp/var/lib/guardian-db/ipfs")),

        // Configurações apropriadas para produção
        enable_pubsub: true,
        enable_swarm: true,
        enable_mdns: false, // Desabilitar em produção
        enable_kad: true,
        ..Default::default()
    }
}

/// Configuração de exemplo para desenvolvimento
#[allow(dead_code)]
fn development_config_example() -> ClientConfig {
    ClientConfig {
        // Diretório temporário para desenvolvimento
        data_store_path: Some(PathBuf::from("./tmp/dev_data")),

        // Configurações para desenvolvimento
        enable_pubsub: true,
        enable_swarm: true,
        enable_mdns: true, // Facilita descoberta local
        enable_kad: true,
        ..Default::default()
    }
}
