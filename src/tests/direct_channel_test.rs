use super::{init_direct_channel_factory, iface, pubsub};
use crate::error::GuardianError;
use futures::stream::StreamExt;
use libp2p::core::identity;
use libp2p::swarm::{Swarm, SwarmBuilder};
use libp2p::{memory_transport, PeerId};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::Level;

// --- Mocks e Stubs para o Teste ---

// Um NetworkBehaviour simples para o teste. Em um cenário real,
// ele conteria a lógica para o protocolo do canal direto.
#[derive(libp2p::swarm::NetworkBehaviour)]
#[behaviour(out_event = "()")]
struct TestBehaviour {}

// Um Emitter que envia eventos para um canal MPSC para que o teste possa recebê-los.
// Isso substitui o `EventBus` do teste em Go.
struct MpscEmitter {
    sender: mpsc::Sender<pubsub::EventPayload>,
}

#[async_trait::async_trait]
impl iface::DirectChannelEmitter for MpscEmitter {
    type Error = GuardianError;

    async fn emit(&self, event: pubsub::EventPayload) -> Result<(), Self::Error> {
        self.sender.send(event).await.map_err(|e| GuardianError::Other(format!("Emit error: {}", e).into()))
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

    let peer_count = 10;
    let mut swarms: Vec<Swarm<TestBehaviour>> = Vec::new();
    let mut direct_channels: Vec<Arc<dyn iface::DirectChannel<Error = GuardianError>>> = Vec::new();
    let mut peer_ids: Vec<PeerId> = Vec::new();
    let mut receivers: Vec<mpsc::Receiver<pubsub::EventPayload>> = Vec::new();

    // 1. Fase de Configuração: Criar todos os pares (swarms) e canais
    for _ in 0..peer_count {
        let local_key = identity::Keypair::generate_ed25519();
        let local_peer_id = PeerId::from(local_key.public());

        // O Swarm é o principal componente do libp2p em Rust, gerenciando a rede.
        let swarm = SwarmBuilder::with_tokio_executor(
            memory_transport::MemoryTransport::new()
                .upgrade(libp2p::core::upgrade::Version::V1)
                .boxed(),
            TestBehaviour {},
            local_peer_id,
        ).build();
        
        peer_ids.push(local_peer_id);

        // Criar um par de canais (emitter/receiver) para este par.
        let (tx, rx) = mpsc::channel(peer_count * 2);
        let emitter = Box::new(MpscEmitter { sender: tx });
        receivers.push(rx);
        
        // Criar o DirectChannel usando a factory.
        // O `host` do Go é análogo ao `Swarm` em Rust.
        // A factory precisa ser adaptada para aceitar o que o Swarm pode fornecer.
        // Aqui, passamos um logger no-op.
        let logger = tracing::Logger::new("test_logger", Level::OFF);
        let factory = init_direct_channel_factory(logger, Arc::new(swarm.host().clone()));
        
        let dc = factory(
            &Duration::from_secs(5), // Contexto placeholder
            emitter,
            None
        ).expect("Failed to create direct channel");

        direct_channels.push(dc);
        swarms.push(swarm);
    }

    // Ligar os swarms para que eles comecem a escutar em endereços de memória.
    for (i, swarm) in swarms.iter_mut().enumerate() {
        let listen_addr = format!("/memory/{}", i + 1);
        swarm.listen_on(listen_addr.parse().unwrap()).unwrap();
        // Em um teste real, também é preciso executar cada swarm em uma tarefa separada.
        tokio::spawn(async move {
            loop {
                let _ = swarm.select_next_some().await;
            }
        });
    }

    // Conectar todos os pares entre si.
    for i in 0..peer_count {
        for j in 0..peer_count {
            if i == j { continue; }
            let addr = swarms[j].listeners().next().unwrap().clone();
            swarms[i].dial(addr).unwrap();
        }
    }
    
    // Aguardar um pouco para as conexões se estabelecerem.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 2. Fase de Envio: Cada par envia uma mensagem para todos os outros.
    let mut messages_to_send = HashSet::new();
    for i in 0..peer_count {
        for j in 0..peer_count {
            if i == j { continue; }

            let message = format!("test-{}-{}", i, j);
            messages_to_send.insert(message.clone());
            
            let target_peer_id = peer_ids[j];
            let dc = &direct_channels[i];
            
            dc.send(&Duration::from_secs(5), target_peer_id, message.as_bytes())
                .await
                .expect("Send should not fail");
        }
    }

    // 3. Fase de Verificação: Coletar e verificar todas as mensagens recebidas.
    let total_messages_expected = peer_count * (peer_count - 1);
    let mut received_count = 0;
    
    // Usar um timeout geral para evitar que o teste trave.
    let verification_timeout = Duration::from_secs(5);
    tokio::time::timeout(verification_timeout, async {
        while received_count < total_messages_expected {
            for (i, rx) in receivers.iter_mut().enumerate() {
                if let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(10), rx.recv()).await {
                    let received_message = String::from_utf8(event.payload).unwrap();
                    
                    // Verifica se a mensagem era esperada e remove do set.
                    assert!(
                        messages_to_send.remove(&received_message),
                        "Received unexpected or duplicate message: {}",
                        received_message
                    );

                    received_count += 1;
                }
            }
        }
    }).await.expect("Test timed out while waiting for messages");

    // Ao final, todas as mensagens enviadas devem ter sido removidas do set.
    assert!(messages_to_send.is_empty(), "Not all messages were received");
    assert_eq!(received_count, total_messages_expected);
}