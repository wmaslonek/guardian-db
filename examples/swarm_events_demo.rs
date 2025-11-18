//! Demonstração de eventos do Swarm
//!
//! Este exemplo mostra como simular e processar eventos do libp2p Swarm
//! para teste e demonstração da funcionalidade de rede P2P.

use guardian_db::error::Result;
use libp2p::{PeerId, gossipsub::TopicHash, identity::Keypair};
use std::time::Duration;
use tracing::{Level, debug, info};
use tracing_subscriber::FmtSubscriber;

/// Eventos do Swarm Manager (cópia para demonstração)
#[derive(Debug)]
pub enum SwarmManagerEvent {
    PeerConnected(PeerId),
    PeerDisconnected(PeerId),
    MessageReceived {
        topic: TopicHash,
        peer: PeerId,
        data: Vec<u8>,
    },
    TopicSubscribed(TopicHash),
    TopicUnsubscribed(TopicHash),
}

#[tokio::main]
async fn main() -> Result<()> {
    // Configura logging para o exemplo
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    info!("Iniciando demonstração de eventos do Swarm");

    // Cria keypair para o teste
    let keypair = Keypair::generate_ed25519();
    let local_peer_id = keypair.public().to_peer_id();
    info!("PeerId local: {}", local_peer_id);

    // Demonstra simulação de eventos
    demonstrate_event_simulation(&local_peer_id).await?;

    // Demonstra processamento de eventos
    demonstrate_event_processing().await?;

    info!("Demonstração concluída!");
    Ok(())
}

/// Demonstra simulação de eventos do swarm
async fn demonstrate_event_simulation(local_peer_id: &PeerId) -> Result<()> {
    info!("Demonstrando simulação de eventos do Swarm...");

    for i in 0..10 {
        if let Some(event) = simulate_swarm_event(local_peer_id).await {
            info!("Evento simulado #{}: {:?}", i + 1, event);

            // Processa o evento simulado
            process_simulated_event(event).await;
        }

        // Delay entre eventos
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    Ok(())
}

/// Simula eventos do swarm para fins de teste e demonstração
async fn simulate_swarm_event(local_peer_id: &PeerId) -> Option<SwarmManagerEvent> {
    use fastrand;

    debug!("Simulando evento do swarm para peer: {}", local_peer_id);

    // Gera evento aleatório baseado em probabilidades realistas
    let event_type = fastrand::u32(1..=100);

    match event_type {
        1..=25 => {
            // 25% - Peer conectado
            let peer_id = PeerId::random();
            debug!("Simulando conexão de peer: {}", peer_id);
            Some(SwarmManagerEvent::PeerConnected(peer_id))
        }
        26..=40 => {
            // 15% - Peer desconectado
            let peer_id = PeerId::random();
            debug!("Simulando desconexão de peer: {}", peer_id);
            Some(SwarmManagerEvent::PeerDisconnected(peer_id))
        }
        41..=70 => {
            // 30% - Mensagem recebida
            let peer_id = PeerId::random();
            let topic = TopicHash::from_raw("test_topic");
            let data = format!("test_message_{}", fastrand::u32(1..=1000)).into_bytes();
            debug!(
                "Simulando mensagem recebida de {}: {} bytes",
                peer_id,
                data.len()
            );
            Some(SwarmManagerEvent::MessageReceived {
                topic,
                peer: peer_id,
                data,
            })
        }
        71..=85 => {
            // 15% - Inscrição em tópico
            let topic = TopicHash::from_raw(format!("topic_{}", fastrand::u32(1..=10)));
            debug!("Simulando inscrição no tópico: {:?}", topic);
            Some(SwarmManagerEvent::TopicSubscribed(topic))
        }
        86..=95 => {
            // 10% - Desinscrição de tópico
            let topic = TopicHash::from_raw(format!("topic_{}", fastrand::u32(1..=10)));
            debug!("Simulando desinscrição do tópico: {:?}", topic);
            Some(SwarmManagerEvent::TopicUnsubscribed(topic))
        }
        _ => {
            // 5% - Sem evento
            debug!("Sem evento simulado nesta iteração");
            None
        }
    }
}

/// Processa evento simulado
async fn process_simulated_event(event: SwarmManagerEvent) {
    match event {
        SwarmManagerEvent::PeerConnected(peer_id) => {
            info!("✓ Processando conexão de peer: {}", peer_id);
            // Aqui seria onde o código real atualizaria a lista de peers conectados
        }
        SwarmManagerEvent::PeerDisconnected(peer_id) => {
            info!("✗ Processando desconexão de peer: {}", peer_id);
            // Aqui seria onde o código real removeria o peer da lista de conectados
        }
        SwarmManagerEvent::MessageReceived { topic, peer, data } => {
            info!(
                "📨 Processando mensagem de {}: {} bytes no tópico {:?}",
                peer,
                data.len(),
                topic
            );
            // Aqui seria onde o código real processaria a mensagem recebida
        }
        SwarmManagerEvent::TopicSubscribed(topic) => {
            info!("📢 Processando inscrição no tópico: {:?}", topic);
            // Aqui seria onde o código real atualizaria a lista de tópicos inscritos
        }
        SwarmManagerEvent::TopicUnsubscribed(topic) => {
            info!("🔇 Processando desinscrição do tópico: {:?}", topic);
            // Aqui seria onde o código real removeria o tópico da lista de inscritos
        }
    }
}

/// Demonstra processamento de eventos em lote
async fn demonstrate_event_processing() -> Result<()> {
    info!("Demonstrando processamento de eventos em lote...");

    // Simula recebimento de múltiplos eventos
    let keypair = Keypair::generate_ed25519();
    let local_peer_id = keypair.public().to_peer_id();
    let mut events = Vec::new();

    // Gera batch de eventos
    for _ in 0..5 {
        if let Some(event) = simulate_swarm_event(&local_peer_id).await {
            events.push(event);
        }
    }

    info!("Processando batch de {} eventos:", events.len());

    // Processa todos os eventos
    for (i, event) in events.into_iter().enumerate() {
        info!("Processando evento {} do batch:", i + 1);
        process_simulated_event(event).await;
    }

    Ok(())
}
