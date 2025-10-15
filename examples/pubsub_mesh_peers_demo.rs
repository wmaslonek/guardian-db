// Demonstração da implementação concreta de pubsub_peers
// que agora retorna peers do mesh do Gossipsub

use guardian_db::ipfs_core_api::{client::IpfsClient, config::ClientConfig};
use tracing::{debug, info};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Inicializa logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    info!("=== Demonstração: pubsub_peers ===");

    // Cria cliente IPFS com PubSub habilitado
    let config = ClientConfig {
        enable_pubsub: true,
        enable_swarm: true,
        enable_mdns: true,
        enable_kad: true,
        ..Default::default()
    };

    let client = IpfsClient::new(config).await?;
    info!("Cliente IPFS inicializado com PubSub habilitado");

    // Testa diferentes cenários
    let test_topic = "test-gossipsub-mesh";

    info!("--- Teste 1: Peers antes de qualquer subscrição ---");
    match client.pubsub_peers(test_topic).await {
        Ok(peers) => {
            info!(
                "Peers encontrados: {} (esperado: 0 - sem subscrições)",
                peers.len()
            );
            for peer in peers {
                debug!("  Peer: {}", peer);
            }
        }
        Err(e) => info!("Erro esperado sem SwarmManager ativo: {}", e),
    }

    info!("--- Teste 2: Simulação de subscrição ---");
    match client.pubsub_subscribe(test_topic).await {
        Ok(_stream) => {
            info!("Subscrito ao tópico: {}", test_topic);

            // Aguarda um pouco para processamento
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            // Testa novamente após subscrição
            match client.pubsub_peers(test_topic).await {
                Ok(peers) => {
                    info!(
                        "Peers após subscrição: {} (mostra fallback ou mesh real)",
                        peers.len()
                    );
                    for peer in peers {
                        debug!("  Peer conectado: {}", peer);
                    }
                }
                Err(e) => info!("Erro: {}", e),
            }
        }
        Err(e) => info!("Erro na subscrição: {}", e),
    }

    info!("--- Teste 3: Verificação de tópicos ativos ---");
    match client.pubsub_topics().await {
        Ok(topics) => {
            info!("Tópicos ativos: {}", topics.len());
            for topic in topics {
                debug!("  Tópico: {}", topic);
            }
        }
        Err(e) => info!("Erro listando tópicos: {}", e),
    }

    info!("=== Demonstração concluída ===");
    info!("  1. SwarmManager.get_topic_mesh_peers() para acesso ao mesh Gossipsub");
    info!("  2. Fallback para peers conectados se SwarmManager não estiver disponível");

    Ok(())
}
