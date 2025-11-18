//! Demonstração da funcionalidade do DirectChannelBehaviour
//!
//! Este exemplo mostra como testar a funcionalidade básica do behaviour
//! incluindo publicação de mensagens, inscrição em tópicos e conexões de peers.

use guardian_db::error::{GuardianError, Result};
use guardian_db::p2p::pubsub::{
    PROTOCOL,
    direct_channel::{DirectChannelNetwork, SwarmBridge},
};
use libp2p::identity::Keypair;
use tracing::Span;
use tracing::{Level, debug, info};
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> Result<()> {
    // Configura logging para o exemplo
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    info!("Iniciando demonstração da funcionalidade do DirectChannelBehaviour");

    // Cria keypair para o teste
    let keypair = Keypair::generate_ed25519();
    let local_peer_id = keypair.public().to_peer_id();
    info!("PeerId local: {}", local_peer_id);

    // Executa testes de funcionalidade
    test_behaviour_creation(&keypair).await?;
    test_behaviour_functionality(&keypair).await?;

    info!("Demonstração concluída com sucesso!");
    Ok(())
}

/// Testa a criação do SwarmBridge
async fn test_behaviour_creation(_keypair: &Keypair) -> Result<()> {
    info!("Testando criação do SwarmBridge...");

    let span = Span::current();
    let _swarm_bridge = SwarmBridge::new(span)
        .await
        .map_err(|e| GuardianError::Other(format!("Erro ao criar SwarmBridge: {}", e)))?;

    info!("SwarmBridge criado com sucesso");
    debug!("SwarmBridge configurado com protocolo: {}", PROTOCOL);

    Ok(())
}

/// Testa funcionalidade básica do SwarmBridge
async fn test_behaviour_functionality(_keypair: &Keypair) -> Result<()> {
    debug!("Testando funcionalidade do SwarmBridge...");

    let span = Span::current();
    let swarm_bridge = SwarmBridge::new(span)
        .await
        .map_err(|e| GuardianError::Other(format!("Erro ao criar SwarmBridge: {}", e)))?;

    // Inicia o SwarmBridge
    swarm_bridge.start().await?;

    // Testa publicação de mensagem de teste
    let test_topic = format!("{}/test", PROTOCOL);
    let test_message = b"behaviour_test_message".to_vec();
    let topic_hash = libp2p::gossipsub::IdentTopic::new(&test_topic).hash();

    info!("Publicando mensagem de teste no tópico: {}", test_topic);

    // Primeiro inscreve no tópico
    swarm_bridge
        .subscribe_topic(&topic_hash)
        .map_err(|e| GuardianError::Other(format!("Erro ao inscrever no tópico: {}", e)))?;

    // Depois publica a mensagem
    swarm_bridge
        .publish_message(&topic_hash, &test_message)
        .map_err(|e| GuardianError::Other(format!("Erro ao publicar mensagem: {}", e)))?;

    // Verifica peers conectados
    let connected_peers = swarm_bridge.get_connected_peers();
    info!("Peers conectados: {}", connected_peers.len());

    let test_result = format!(
        "TestResult[topic={}, message_size={}, connected_peers={}]",
        test_topic,
        test_message.len(),
        connected_peers.len()
    );

    info!(
        "Teste de funcionalidade concluído: tópico={}, peers={}",
        test_topic,
        connected_peers.len()
    );

    info!("Resultado do teste: {}", test_result);

    Ok(())
}
