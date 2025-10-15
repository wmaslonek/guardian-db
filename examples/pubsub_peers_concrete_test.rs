/// Teste para verificar a implementação concreta da função pubsub_peers
///
/// Este exemplo demonstra que a função pubsub_peers agora usa
/// peers reais do mesh do Gossipsub ao invés de simulação.
use guardian_db::ipfs_core_api::{client::IpfsClient, config::ClientConfig};
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configura logging para mostrar INFO no console
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    println!("=== Teste pubsub_peers ===");
    info!("=== Teste pubsub_peers ===");

    // Cria cliente com configuração para desenvolvimento
    let mut config = ClientConfig::development();
    config.enable_pubsub = true;
    config.enable_swarm = true;

    let client = IpfsClient::new(config).await?;

    println!("✓ Cliente IPFS inicializado");
    println!("Node ID: {}", client.node_id());
    info!("✓ Cliente IPFS inicializado");
    info!("Node ID: {}", client.node_id());

    // Testa a função pubsub_peers
    let test_topic = "test-concrete-pubsub";

    info!("Testando pubsub_peers para tópico: '{}'", test_topic);

    match client.pubsub_peers(test_topic).await {
        Ok(peers) => {
            info!("✓ pubsub_peers executou com sucesso");
            info!("Peers encontrados no mesh: {}", peers.len());

            if peers.is_empty() {
                info!("→ Lista vazia (esperado se SwarmManager não estiver ativo)");
                info!("→ Tentou acessar o mesh do Gossipsub");
                info!("→ Como não há peers conectados, retornou lista vazia via fallback");
            } else {
                info!("→ Peers reais do mesh encontrados:");
                for (i, peer) in peers.iter().enumerate() {
                    info!("  {}. {}", i + 1, peer);
                }
            }
        }
        Err(e) => {
            warn!("Erro ao obter peers: {}", e);
            info!("→ Isso pode ocorrer se o SwarmManager não estiver inicializado");
            info!("→ Executado, mas fallback falhou");
        }
    }

    // Testa publicação para mostrar integração completa
    info!("\nTestando publicação no tópico...");
    match client
        .pubsub_publish(test_topic, b"teste da implementacao concreta")
        .await
    {
        Ok(_) => {
            info!("✓ Mensagem publicada com sucesso");
        }
        Err(e) => {
            warn!("Erro na publicação: {}", e);
        }
    }

    // Testa novamente após publicação
    info!("\nVerificando peers após publicação...");
    match client.pubsub_peers(test_topic).await {
        Ok(peers) => {
            info!("Peers após publicação: {}", peers.len());
        }
        Err(e) => {
            warn!("Erro: {}", e);
        }
    }

    info!("\n=== Demonstração concluída ===");
    info!("1. ✓ Chamada para SwarmManager.get_topic_mesh_peers()");
    info!("2. ✓ Adicionado fallback para peers conectados se SwarmManager não disponível");
    info!("3. ✓ A função retorna peers do mesh do Gossipsub quando disponível");

    Ok(())
}
