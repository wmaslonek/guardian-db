use guardian_db::{
    error::Result,
    ipfs_core_api::{
        backends::{IpfsBackend, IrohBackend},
        config::ClientConfig,
    },
};
use std::sync::Arc;
use tempdir::TempDir;
use tracing::{Level, info};

#[tokio::main]
async fn main() -> Result<()> {
    // Inicializa logging
    tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .init();

    info!("Demonstração de Operações P2P via Iroh Endpoint");

    // Configura diretório temporário para teste
    let temp_dir = TempDir::new("guardian_p2p_test")
        .map_err(|e| guardian_db::error::GuardianError::Other(e.to_string()))?;

    info!("Diretório de teste: {}", temp_dir.path().display());

    // Configura cliente IPFS Iroh
    let config = ClientConfig {
        data_store_path: Some(temp_dir.path().to_path_buf()),
        enable_pubsub: true,
        enable_swarm: true,
        ..Default::default()
    };

    // Cria instância do backend Iroh
    let backend = Arc::new(IrohBackend::new(&config).await?);

    info!("✓ Backend Iroh criado com sucesso");

    // Teste 1: Obter informações do nó local
    info!("Obtendo informações do nó local...");
    let node_info = backend.id().await?;
    info!("Node ID: {}", node_info.id);
    info!("Public Key: {}", node_info.public_key);
    info!("Addresses: {:?}", node_info.addresses);
    info!("Agent: {}", node_info.agent_version);
    info!("Protocol: {}", node_info.protocol_version);

    // Teste 2: Listar peers conectados
    info!("Listando peers conectados...");
    let peers = backend.peers().await?;
    if peers.is_empty() {
        info!("Nenhum peer conectado no momento");
    } else {
        info!("Encontrados {} peers:", peers.len());
        for (i, peer) in peers.iter().enumerate() {
            info!("   {}. Peer ID: {}", i + 1, peer.id);
            info!("      Endereços: {:?}", peer.addresses);
            info!("      Protocolos: {:?}", peer.protocols);
            info!("      Conectado: {}", peer.connected);
        }
    }

    // Teste 3: Tentar conectar a um peer de exemplo
    info!("Testando conexão P2P...");
    let example_peer_id = libp2p::identity::Keypair::generate_ed25519()
        .public()
        .to_peer_id();

    info!("Tentando conectar ao peer: {}", example_peer_id);
    match backend.connect(&example_peer_id).await {
        Ok(_) => info!("✓ Conexão P2P iniciada com sucesso!"),
        Err(e) => info!("Conexão P2P (simulada): {}", e),
    }

    // Teste 4: Busca DHT
    info!("Testando busca DHT...");
    let search_peer_id = libp2p::identity::Keypair::generate_ed25519()
        .public()
        .to_peer_id();

    info!("Procurando peer no DHT: {}", search_peer_id);
    match backend.dht_find_peer(&search_peer_id).await {
        Ok(addresses) => {
            info!("✓ Busca DHT concluída!");
            info!("Endereços encontrados: {}", addresses.len());
            for (i, addr) in addresses.iter().enumerate() {
                info!("   {}. {}", i + 1, addr);
            }
        }
        Err(e) => info!("Erro na busca DHT: {}", e),
    }

    // Teste 5: Verificar status dos peers após operações
    info!("Verificando status final dos peers...");
    let final_peers = backend.peers().await?;
    info!("Peers finais conectados: {}", final_peers.len());

    // Teste 6: Health check final
    info!("Verificando saúde do sistema...");
    let health = backend.health_check().await?;
    info!("✓ Sistema saudável: {}", health.healthy);
    if !health.message.is_empty() {
        info!("Mensagem: {}", health.message);
    }

    info!("Demonstração de P2P concluída!");
    info!("As operações P2P foram executadas usando o Endpoint do Iroh");

    Ok(())
}
