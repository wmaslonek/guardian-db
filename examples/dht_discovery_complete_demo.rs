use guardian_db::ipfs_core_api::{
    backends::{IpfsBackend, IrohBackend},
    config::ClientConfig,
};
use std::sync::Arc;
use tracing::{Level, info};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Inicializa logging
    tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .init();

    info!("Demonstração Completa de DHT e Discovery");

    // Configura diretório temporário para teste
    let temp_dir = std::env::temp_dir().join("guardian_dht_discovery_test");
    std::fs::create_dir_all(&temp_dir)?;

    info!("Diretório de teste: {}", temp_dir.display());

    // Configura cliente IPFS Iroh com DHT
    let config = ClientConfig {
        data_store_path: Some(temp_dir.clone()),
        enable_pubsub: true,
        enable_swarm: true,
        enable_kad: true,
        ..Default::default()
    };

    // Cria instância do backend Iroh
    let iroh_backend = Arc::new(IrohBackend::new(&config).await?);

    info!("✓ Backend Iroh com DHT integrado criado com sucesso");

    // Aguarda inicialização do DHT
    info!("Aguardando inicialização do DHT...");
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    // Teste 1: Informações do nó local
    info!("=== INFORMAÇÕES DO NÓ LOCAL ===");
    let node_info = iroh_backend.id().await?;
    info!("Node ID: {}", node_info.id);
    info!("Endereços: {:?}", node_info.addresses);
    info!("Agent: {}", node_info.agent_version);
    info!("Protocol: {}", node_info.protocol_version);

    // Teste 2: Listar peers descobertos via DHT
    info!("=== PEERS DESCOBERTOS VIA DHT ===");
    let peers = iroh_backend.peers().await?;

    if peers.is_empty() {
        info!("Nenhum peer descoberto ainda");
    } else {
        info!("Descobertos {} peers via DHT:", peers.len());
        for (i, peer) in peers.iter().enumerate() {
            info!("   {}. Peer ID: {}", i + 1, peer.id);
            info!("Endereços: {:?}", peer.addresses);
            info!("Protocolos: {:?}", peer.protocols);
            info!("Conectado: {}", peer.connected);
            info!("");
        }
    }

    // Teste 3: Verificar status online e métricas
    info!("=== STATUS E MÉTRICAS ===");
    let is_online = iroh_backend.is_online().await;
    info!("Backend online: {}", is_online);

    let metrics = iroh_backend.metrics().await?;
    info!("Métricas do sistema:");
    info!("   - Operações por segundo: {:.2}", metrics.ops_per_second);
    info!("   - Latência média: {:.2}ms", metrics.avg_latency_ms);
    info!("   - Total de operações: {}", metrics.total_operations);
    info!("   - Uso de memória: {} bytes", metrics.memory_usage_bytes);

    // Teste 4: Operações básicas de conteúdo
    info!("=== TESTE DE OPERAÇÕES BÁSICAS ===");

    let test_data = b"Teste de DHT e Discovery com Iroh";
    info!("Adicionando conteúdo de teste...");

    let data_reader = Box::pin(std::io::Cursor::new(test_data.to_vec()));
    let add_response = iroh_backend.add(data_reader).await?;
    info!("✓ Conteúdo adicionado: CID = {}", add_response.hash);

    // Recuperar conteúdo
    let mut content_stream = iroh_backend.cat(&add_response.hash).await?;
    let mut retrieved_data = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut content_stream, &mut retrieved_data)
        .await
        .expect("Erro ao ler conteúdo recuperado");

    if retrieved_data == test_data {
        info!("✓ Conteúdo recuperado com sucesso!");
    } else {
        info!("✗ Erro na recuperação do conteúdo");
    }

    // Teste 5: Fixar conteúdo (pin)
    info!("=== TESTE DE PINNING ===");

    match iroh_backend.pin_add(&add_response.hash).await {
        Ok(()) => {
            info!("✓ Conteúdo fixado com sucesso");

            // Listar pins
            match iroh_backend.pin_ls().await {
                Ok(pins) => {
                    info!("Objetos fixados: {}", pins.len());
                    for pin in pins {
                        info!("   - {}: {:?}", pin.cid, pin.pin_type);
                    }
                }
                Err(e) => info!("Erro listando pins: {}", e),
            }
        }
        Err(e) => info!("Erro fixando conteúdo: {}", e),
    }

    // Teste 6: Status final dos peers
    info!("=== STATUS FINAL ===");
    let final_peers = iroh_backend.peers().await?;
    info!("Total de peers conhecidos: {}", final_peers.len());

    let connected_count = final_peers.iter().filter(|p| p.connected).count();
    info!("Peers conectados: {}", connected_count);
    info!("Peers em cache: {}", final_peers.len() - connected_count);

    info!("Demonstração de Discovery e operações IPFS concluída!");
    info!("O sistema Iroh está funcionando com persistência e descoberta de peers");

    Ok(())
}
