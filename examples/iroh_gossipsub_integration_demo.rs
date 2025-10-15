// LibP2P Swarm com Gossipsub integrado ao Iroh
//
// Este exemplo mostra como usar o backend Iroh com funcionalidade completa
// de LibP2P Gossipsub para comunicação pub/sub distribuída.
//
// NOTA: Em execução com peer único, erros "NoPeersSubscribedToTopic" são
// NORMAIS e ESPERADOS, pois Gossipsub precisa de múltiplos peers conectados.

use guardian_db::{
    error::Result,
    iface::PubSubInterface,
    ipfs_core_api::{
        backends::{IpfsBackend, IrohBackend},
        config::ClientConfig,
    },
};
use std::{path::PathBuf, sync::Arc};
use tokio::time::{Duration, sleep};
use tracing::{debug, info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    // Inicializa logging
    tracing_subscriber::fmt().with_env_filter("debug").init();

    info!("Iniciando demonstração: LibP2P Swarm com Gossipsub no Iroh");

    // === CONFIGURAÇÃO DO BACKEND IROH ===

    let config = ClientConfig {
        data_store_path: Some(PathBuf::from("./tmp/demo_data_gossipsub")),
        enable_pubsub: true,
        enable_swarm: true,
        ..Default::default()
    };

    info!("Criando backend Iroh com LibP2P Swarm integrado...");
    let iroh_backend = Arc::new(IrohBackend::new(&config).await?);

    // Aguarda um momento para garantir inicialização completa
    sleep(Duration::from_secs(2)).await;

    // === DEMONSTRAÇÃO DE FUNCIONALIDADE BÁSICA IPFS ===

    info!("Testando operações básicas do IPFS...");

    // Adiciona conteúdo ao IPFS
    let test_data = "Ola do LibP2P Swarm com Gossipsub!".as_bytes();
    let data_reader = Box::pin(std::io::Cursor::new(test_data));

    let add_response = iroh_backend.add(data_reader).await?;
    info!(
        "✓ Conteúdo adicionado: CID = {}, Size = {}",
        add_response.hash, add_response.size
    );

    // Recupera conteúdo do IPFS
    let mut content_stream = iroh_backend.cat(&add_response.hash).await?;
    let mut retrieved_data = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut content_stream, &mut retrieved_data)
        .await
        .expect("Erro ao ler conteúdo recuperado");

    info!("✓ Conteúdo recuperado: {} bytes", retrieved_data.len());
    assert_eq!(retrieved_data, test_data);

    // === DEMONSTRAÇÃO DO GOSSIPSUB ===

    info!("Criando interface PubSub com Gossipsub...");
    let mut pubsub_interface = iroh_backend.clone().create_pubsub_interface();

    // Subscreve a tópicos de demonstração
    let topic1 = "guardian-db/demo/news";
    let topic2 = "guardian-db/demo/events";

    info!("Subscrevendo aos tópicos: {} e {}", topic1, topic2);

    let news_topic = pubsub_interface.topic_subscribe(topic1).await?;
    let events_topic = pubsub_interface.topic_subscribe(topic2).await?;

    info!("✓ Subscrições ativas para ambos os tópicos");

    // === DEMONSTRAÇÃO DE PUBLICAÇÃO ===

    info!("Publicando mensagens nos tópicos...");

    // Publica notícias (com tratamento de erro para peer único)
    let news_msg = "Breaking: Guardian DB Phase 6 com Gossipsub esta funcionando!".as_bytes();
    match news_topic.publish(news_msg.to_vec()).await {
        Ok(()) => info!("✓ Notícia publicada: {} bytes", news_msg.len()),
        Err(e) => {
            if e.to_string().contains("NoPeersSubscribedToTopic") {
                info!("⚠️ Nenhum peer conectado para receber a notícia (normal em teste único)");
            } else {
                return Err(e);
            }
        }
    }

    // Publica eventos (com tratamento de erro para peer único)
    let event_msg = "Event: Sistema LibP2P totalmente integrado ao Iroh".as_bytes();
    match events_topic.publish(event_msg.to_vec()).await {
        Ok(()) => info!("✓ Evento publicado: {} bytes", event_msg.len()),
        Err(e) => {
            if e.to_string().contains("NoPeersSubscribedToTopic") {
                info!("⚠️ Nenhum peer conectado para receber o evento (normal em teste único)");
            } else {
                return Err(e);
            }
        }
    }

    // === DEMONSTRAÇÃO DE MÚLTIPLAS MENSAGENS ===

    info!("Enviando múltiplas mensagens para demonstrar throughput...");

    for i in 1..=5 {
        let message = format!("Mensagem de teste #{} - LibP2P + Iroh funcionando!", i);
        match news_topic.publish(message.as_bytes().to_vec()).await {
            Ok(()) => debug!("✓ Enviado: {}", message),
            Err(e) if e.to_string().contains("NoPeersSubscribedToTopic") => {
                debug!("⚠️ Nenhum peer para receber: {}", message);
            }
            Err(e) => {
                warn!("Erro enviando mensagem {}: {}", i, e);
            }
        }

        // Pequena pausa entre mensagens
        sleep(Duration::from_millis(100)).await;
    }

    // === DEMONSTRAÇÃO DE RECEPÇÃO DE MENSAGENS ===
    info!("Demonstrando recepção de mensagens (simulado)...");

    // Simula listener de mensagens
    tokio::spawn(async move {
        info!("Listener para o tópico de notícias iniciado");
        info!("Note: Em implementação real, usaria receive() ou similar");
        sleep(Duration::from_millis(500)).await;
        info!("✓ Listener simulado finalizado");
    });

    // === DEMONSTRAÇÃO DE MÉTRICAS E STATUS ===

    info!("Verificando métricas do sistema...");

    let is_online = iroh_backend.is_online().await;
    info!("Backend online: {}", is_online);

    let metrics = iroh_backend.metrics().await?;
    info!("Métricas do backend:");
    info!("   - Operações por segundo: {:.2}", metrics.ops_per_second);
    info!("   - Latência média: {:.2}ms", metrics.avg_latency_ms);
    info!("   - Total de operações: {}", metrics.total_operations);
    info!("   - Uso de memória: {} bytes", metrics.memory_usage_bytes);

    // === DEMONSTRAÇÃO DE PEERS ===

    info!("Listando peers conectados...");
    let peers = iroh_backend.peers().await?;
    info!("Total de peers conectados: {}", peers.len());

    for peer in peers {
        debug!("   Peer: {} ({})", peer.id, peer.addresses.join(", "));
    }

    // === DEMONSTRAÇÃO DE PERSISTENCE ===

    info!("Testando persistência de dados...");

    // Lista objetos fixados
    let pinned_objects = iroh_backend.pin_ls().await?;
    info!("Objetos fixados: {}", pinned_objects.len());

    // Fixa o objeto que criamos
    iroh_backend.pin_add(&add_response.hash).await?;
    info!("Objeto {} fixado com sucesso", add_response.hash);

    // === CLEANUP E FINALIZAÇÃO ===

    info!("Executando limpeza e finalização...");

    // Aguarda um pouco para permitir processamento final
    sleep(Duration::from_secs(2)).await;

    // Nota: Garbage collection é automático no iroh-blobs 0.35.0 via sistema de tags
    info!("Limpeza automática gerenciada pelo sistema de tags do iroh-blobs");

    info!("Demonstração concluída com sucesso!");
    info!("LibP2P Swarm com Gossipsub totalmente integrado ao Iroh Backend");

    Ok(())
}

/// Função auxiliar para demonstrar uso avançado do Gossipsub
#[allow(dead_code)]
async fn advanced_gossipsub_demo(iroh_backend: Arc<IrohBackend>) -> Result<()> {
    info!("Demonstração avançada do Gossipsub");

    let mut pubsub = iroh_backend.create_pubsub_interface();

    // Múltiplos tópicos especializados
    let topics = vec![
        "guardian-db/replication",
        "guardian-db/events",
        "guardian-db/discovery",
        "guardian-db/sync",
    ];

    let mut subscriptions = Vec::new();

    for topic in topics {
        info!("Subscrevendo ao tópico especializado: {}", topic);
        let subscription = pubsub.topic_subscribe(topic).await?;
        subscriptions.push(subscription);
    }

    // Demonstra publicação em diferentes tópicos
    for (i, topic) in subscriptions.into_iter().enumerate() {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let message = format!(
            "Dados especializados para tópico {} - timestamp: {}",
            i, timestamp
        );

        match topic.publish(message.as_bytes().to_vec()).await {
            Ok(()) => info!("✓ Publicado no tópico {}: {} bytes", i, message.len()),
            Err(e) if e.to_string().contains("NoPeersSubscribedToTopic") => {
                info!(
                    "⚠️ Tópico {} sem peers conectados (normal em teste único)",
                    i
                );
            }
            Err(e) => return Err(e),
        }
    }

    info!("✓ Demonstração avançada concluída");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_iroh_gossipsub_integration() {
        let config = ClientConfig {
            data_store_path: Some(PathBuf::from("./tmp/test_data_gossipsub")),
            enable_pubsub: true,
            enable_swarm: true,
            ..Default::default()
        };

        let iroh_backend = Arc::new(IrohBackend::new(&config).await.unwrap());
        let mut pubsub = iroh_backend.clone().create_pubsub_interface();

        // Testa subscrição
        let mut topic = pubsub.topic_subscribe("test/topic").await.unwrap();

        // Testa publicação
        let test_msg = "Test message for Gossipsub".as_bytes();
        topic.publish(test_msg.to_vec()).await.unwrap();

        // Verifica que o sistema não falha
        assert!(iroh_backend.is_online().await);
    }

    #[tokio::test]
    async fn test_multiple_topics() {
        let config = ClientConfig {
            data_store_path: Some(PathBuf::from("./tmp/test_data_multi_topics")),
            enable_pubsub: true,
            enable_swarm: true,
            ..Default::default()
        };

        let iroh_backend = Arc::new(IrohBackend::new(&config).await.unwrap());
        let mut pubsub = iroh_backend.clone().create_pubsub_interface();

        // Múltiplas subscrições
        let mut topic1 = pubsub.topic_subscribe("test/topic1").await.unwrap();
        let mut topic2 = pubsub.topic_subscribe("test/topic2").await.unwrap();

        // Publica em ambos
        topic1
            .publish("Message for topic 1".as_bytes().to_vec())
            .await
            .unwrap();
        topic2
            .publish("Message for topic 2".as_bytes().to_vec())
            .await
            .unwrap();

        // Sistema deve permanecer estável
        assert!(iroh_backend.is_online().await);
    }
}
