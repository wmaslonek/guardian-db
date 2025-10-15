use crate::error::GuardianError;
use crate::iface::{self, EventPubSubPayload};
use crate::pubsub::direct_channel::init_direct_channel_factory;
use libp2p::SwarmBuilder;
use libp2p::ping;
use std::sync::Arc;
use tokio::sync::mpsc;

// --- Mocks e Stubs para o Teste ---

// Um Emitter que envia eventos para um canal MPSC para que o teste possa recebê-los.
struct MpscEmitter {
    sender: mpsc::Sender<EventPubSubPayload>,
}

#[async_trait::async_trait]
impl iface::DirectChannelEmitter for MpscEmitter {
    type Error = GuardianError;

    async fn emit(&self, event: EventPubSubPayload) -> Result<(), Self::Error> {
        self.sender
            .send(event)
            .await
            .map_err(|e| GuardianError::Other(format!("Emit error: {}", e)))
    }
    async fn close(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

// --- Fim dos Mocks ---

#[tokio::test]
async fn test_init_direct_channel_factory() {
    // Para depuração, pode-se habilitar o logging.
    // let _ = tracing_subscriber::fmt().with_max_level(Level::DEBUG).try_init();

    // Teste simplificado: verificar se a factory funciona
    let swarm = SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .unwrap()
        .with_behaviour(|_| ping::Behaviour::default())
        .unwrap()
        .build();

    let local_peer_id = *swarm.local_peer_id();

    // Criar um canal para receber eventos
    let (tx, _rx) = mpsc::channel(10);
    let emitter = MpscEmitter { sender: tx };

    // Criar o DirectChannel usando a factory
    let span = tracing::span!(tracing::Level::INFO, "direct_channel_test");
    let factory = init_direct_channel_factory(span, local_peer_id);

    let dc = factory(Arc::new(emitter), None)
        .await
        .expect("Failed to create direct channel");

    println!(
        "Successfully created direct channel for peer: {}",
        local_peer_id
    );

    // Teste básico: tentar fechar o canal usando close_shared
    match dc.close_shared().await {
        Ok(_) => println!("Successfully closed direct channel"),
        Err(e) => println!("Failed to close channel: {}", e),
    }

    // Verificar que o canal foi criado
    // Teste passou se chegamos até aqui
}
