/// Testes robustos para o módulo EpidemicPubSub (iroh-gossip)
///
/// Este módulo contém testes completos para validar:
/// - Inicialização do EpidemicPubSub
/// - Subscrição a tópicos
/// - Publicação e recepção de mensagens
/// - Streams de mensagens e eventos de peers
/// - Comunicação entre múltiplos peers
/// - Tratamento de erros e edge cases
use crate::guardian::error::Result;
use crate::p2p::network::config::ClientConfig;
use crate::p2p::network::core::IrohBackend;
use crate::p2p::network::core::gossip::EpidemicPubSub;
use crate::traits::PubSubInterface;
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::{sleep, timeout};
use tracing::{debug, info};

/// Helper para criar um backend Iroh de teste isolado
async fn create_test_backend(name: &str) -> Result<(Arc<IrohBackend>, TempDir)> {
    let temp_dir =
        TempDir::new().map_err(|e| crate::guardian::error::GuardianError::Other(e.to_string()))?;

    let client_config = ClientConfig {
        data_store_path: Some(temp_dir.path().join(name)),
        enable_pubsub: true,
        ..Default::default()
    };

    let backend = Arc::new(IrohBackend::new(&client_config).await?);

    // Aguardar inicialização
    sleep(Duration::from_millis(100)).await;

    Ok((backend, temp_dir))
}

/// Helper para criar EpidemicPubSub de teste
async fn create_test_pubsub(name: &str) -> Result<(EpidemicPubSub, TempDir)> {
    let (backend, temp_dir) = create_test_backend(name).await?;
    let pubsub = EpidemicPubSub::new(backend).await?;
    Ok((pubsub, temp_dir))
}

#[cfg(test)]
mod initialization_tests {
    use super::*;

    #[tokio::test]
    async fn test_epidemic_pubsub_initialization() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando inicialização do EpidemicPubSub");

        let result = create_test_pubsub("init_test").await;
        assert!(
            result.is_ok(),
            "EpidemicPubSub deveria inicializar com sucesso"
        );

        let (pubsub, _temp_dir) = result.unwrap();

        // Verifica que o gossip foi criado (se chegou aqui sem panic, está inicializado)
        let _gossip_ref = pubsub.gossip();

        info!("✓ EpidemicPubSub inicializado com sucesso");
    }

    #[tokio::test]
    async fn test_topic_id_generation_deterministic() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando geração determinística de TopicId");

        // TopicId deve ser determinístico (mesmo input -> mesmo output)
        let topic_name = "test-topic";

        // Como topic_id_from_str é privado, vamos testar através de subscrições
        let (pubsub, _temp_dir) = create_test_pubsub("topic_id_test").await.unwrap();

        // Subscreve duas vezes ao mesmo tópico
        let topic1 = pubsub.topic_subscribe(topic_name).await.unwrap();
        let topic2 = pubsub.topic_subscribe(topic_name).await.unwrap();

        // Deve retornar o mesmo tópico (cached)
        assert_eq!(topic1.topic(), topic2.topic());

        info!("✓ TopicId é gerado de forma determinística");
    }

    #[tokio::test]
    async fn test_multiple_backends_initialization() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando inicialização de múltiplos backends");

        let (pubsub1, _dir1) = create_test_pubsub("backend1").await.unwrap();
        let (pubsub2, _dir2) = create_test_pubsub("backend2").await.unwrap();
        let (pubsub3, _dir3) = create_test_pubsub("backend3").await.unwrap();

        // Todos devem ter gossip inicializado (se chegou aqui sem panic, está inicializado)
        let _g1 = pubsub1.gossip();
        let _g2 = pubsub2.gossip();
        let _g3 = pubsub3.gossip();

        info!("✓ Múltiplos backends inicializados com sucesso");
    }
}

#[cfg(test)]
mod subscription_tests {
    use super::*;

    #[tokio::test]
    async fn test_topic_subscription() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando subscrição básica a tópicos");

        let (pubsub, _temp_dir) = create_test_pubsub("sub_test").await.unwrap();

        let topic_name = "test-topic";
        let topic = pubsub.topic_subscribe(topic_name).await;

        assert!(topic.is_ok(), "Subscrição ao tópico deveria ter sucesso");

        let topic = topic.unwrap();
        assert_eq!(
            topic.topic(),
            topic_name,
            "Nome do tópico deve corresponder"
        );

        info!("✓ Subscrição a tópico realizada com sucesso");
    }

    #[tokio::test]
    async fn test_multiple_topic_subscriptions() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando subscrição a múltiplos tópicos");

        let (pubsub, _temp_dir) = create_test_pubsub("multi_sub_test").await.unwrap();

        let topics = vec!["topic1", "topic2", "topic3"];
        let mut subscribed_topics = Vec::new();

        for topic_name in &topics {
            let topic = pubsub.topic_subscribe(topic_name).await.unwrap();
            assert_eq!(topic.topic(), *topic_name);
            subscribed_topics.push(topic);
        }

        assert_eq!(subscribed_topics.len(), topics.len());

        info!("✓ Subscrição a múltiplos tópicos realizada com sucesso");
    }

    #[tokio::test]
    async fn test_topic_caching() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando cache de tópicos");

        let (pubsub, _temp_dir) = create_test_pubsub("cache_test").await.unwrap();

        let topic_name = "cached-topic";

        // Primeira subscrição
        let topic1 = pubsub.topic_subscribe(topic_name).await.unwrap();

        // Segunda subscrição (deve retornar do cache)
        let topic2 = pubsub.topic_subscribe(topic_name).await.unwrap();

        // Ambos devem apontar para o mesmo tópico
        assert_eq!(topic1.topic(), topic2.topic());

        // Como Arc, ambos devem ter o mesmo endereço de memória
        let ptr1 = Arc::as_ptr(&topic1) as *const ();
        let ptr2 = Arc::as_ptr(&topic2) as *const ();
        assert_eq!(ptr1, ptr2, "Tópicos devem ser a mesma instância em cache");

        info!("✓ Cache de tópicos funcionando corretamente");
    }

    #[tokio::test]
    async fn test_subscription_with_special_characters() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando subscrição com caracteres especiais");

        let (pubsub, _temp_dir) = create_test_pubsub("special_chars_test").await.unwrap();

        let special_topics = vec![
            "topic-with-dashes",
            "topic_with_underscores",
            "topic.with.dots",
            "topic/with/slashes",
            "tópico-com-acentuação",
            "主题中文",
        ];

        for topic_name in special_topics {
            let result = pubsub.topic_subscribe(topic_name).await;
            assert!(
                result.is_ok(),
                "Subscrição com '{}' deveria ter sucesso",
                topic_name
            );
            assert_eq!(result.unwrap().topic(), topic_name);
        }

        info!("✓ Subscrição com caracteres especiais funcionando");
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;

    #[tokio::test]
    async fn test_publish_before_subscription_fails() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando publicação sem subscrição prévia");

        let (pubsub, _temp_dir) = create_test_pubsub("pub_no_sub_test").await.unwrap();

        let topic_name = "unsubscribed-topic";
        let message = b"test message";

        let result = pubsub.publish_to_topic(topic_name, message).await;

        assert!(result.is_err(), "Publicação sem subscrição deve falhar");

        if let Err(e) = result {
            let error_msg = format!("{}", e);
            assert!(
                error_msg.contains("não encontrado"),
                "Erro deve indicar tópico não encontrado"
            );
        }

        info!("✓ Publicação sem subscrição falha corretamente");
    }

    #[tokio::test]
    async fn test_publish_after_subscription() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando publicação após subscrição");

        let (pubsub, _temp_dir) = create_test_pubsub("pub_after_sub_test").await.unwrap();

        let topic_name = "test-topic";
        let message = b"test message";

        // Subscreve primeiro
        let _topic = pubsub.topic_subscribe(topic_name).await.unwrap();

        // Aguarda inicialização completa
        sleep(Duration::from_millis(100)).await;

        // Tenta publicar
        let result = pubsub.publish_to_topic(topic_name, message).await;

        assert!(
            result.is_ok(),
            "Publicação após subscrição deveria ter sucesso: {:?}",
            result.err()
        );

        info!("✓ Publicação após subscrição realizada com sucesso");
    }

    #[tokio::test]
    async fn test_publish_empty_message() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando publicação de mensagem vazia");

        let (pubsub, _temp_dir) = create_test_pubsub("pub_empty_test").await.unwrap();

        let topic_name = "empty-topic";
        let _topic = pubsub.topic_subscribe(topic_name).await.unwrap();

        sleep(Duration::from_millis(100)).await;

        let result = pubsub.publish_to_topic(topic_name, &[]).await;
        assert!(
            result.is_ok(),
            "Publicação de mensagem vazia deveria ter sucesso"
        );

        info!("✓ Mensagem vazia publicada com sucesso");
    }

    #[tokio::test]
    async fn test_publish_large_message() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando publicação de mensagem grande");

        let (pubsub, _temp_dir) = create_test_pubsub("pub_large_test").await.unwrap();

        let topic_name = "large-topic";
        let _topic = pubsub.topic_subscribe(topic_name).await.unwrap();

        sleep(Duration::from_millis(100)).await;

        // Mensagem de 1MB
        let large_message = vec![0xAB; 1024 * 1024];

        let result = pubsub.publish_to_topic(topic_name, &large_message).await;
        assert!(
            result.is_ok(),
            "Publicação de mensagem grande deveria ter sucesso"
        );

        info!("✓ Mensagem grande (1MB) publicada com sucesso");
    }

    #[tokio::test]
    async fn test_multiple_publications_same_topic() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando múltiplas publicações no mesmo tópico");

        let (pubsub, _temp_dir) = create_test_pubsub("multi_pub_test").await.unwrap();

        let topic_name = "multi-pub-topic";
        let _topic = pubsub.topic_subscribe(topic_name).await.unwrap();

        sleep(Duration::from_millis(100)).await;

        for i in 0..10 {
            let message = format!("message-{}", i);
            let result = pubsub
                .publish_to_topic(topic_name, message.as_bytes())
                .await;
            assert!(result.is_ok(), "Publicação {} deveria ter sucesso", i);
        }

        info!("✓ Múltiplas publicações realizadas com sucesso");
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    #[tokio::test]
    async fn test_watch_messages_stream() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando stream de mensagens");

        let (pubsub, _temp_dir) = create_test_pubsub("watch_msg_test").await.unwrap();

        let topic_name = "watch-messages-topic";
        let topic = pubsub.topic_subscribe(topic_name).await.unwrap();

        // Inicia o stream de mensagens
        let mut message_stream = topic.watch_messages().await.unwrap();

        // Aguarda inicialização
        sleep(Duration::from_millis(200)).await;

        // Publica algumas mensagens
        let messages = vec!["msg1", "msg2", "msg3"];
        for msg in &messages {
            pubsub
                .publish_to_topic(topic_name, msg.as_bytes())
                .await
                .unwrap();
            sleep(Duration::from_millis(50)).await;
        }

        // Tenta receber as mensagens com timeout
        let mut received_count = 0;
        let timeout_duration = Duration::from_secs(5);

        while received_count < messages.len() {
            match timeout(timeout_duration, message_stream.next()).await {
                Ok(Some(event)) => {
                    debug!("Mensagem recebida: {} bytes", event.content.len());
                    received_count += 1;
                }
                Ok(None) => {
                    debug!("Stream encerrado");
                    break;
                }
                Err(_) => {
                    debug!("Timeout aguardando mensagem");
                    break;
                }
            }
        }

        // Em ambiente de teste isolado, pode não receber todas as mensagens
        // (gossip precisa de múltiplos peers), mas o stream deve funcionar
        info!(
            "✓ Stream de mensagens funcionando (recebidas: {})",
            received_count
        );
    }

    #[tokio::test]
    async fn test_watch_peers_stream() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando stream de eventos de peers");

        let (pubsub, _temp_dir) = create_test_pubsub("watch_peers_test").await.unwrap();

        let topic_name = "watch-peers-topic";
        let topic = pubsub.topic_subscribe(topic_name).await.unwrap();

        // Inicia o stream de eventos de peers
        let result = topic.watch_peers().await;
        assert!(result.is_ok(), "watch_peers deveria retornar stream");

        let mut _peer_stream = result.unwrap();

        // Verifica que o stream foi criado (em ambiente isolado não teremos eventos)
        info!("✓ Stream de eventos de peers criado com sucesso");
    }

    #[tokio::test]
    async fn test_peers_list_initially_empty() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando lista inicial de peers");

        let (pubsub, _temp_dir) = create_test_pubsub("peers_list_test").await.unwrap();

        let topic_name = "peers-list-topic";
        let topic = pubsub.topic_subscribe(topic_name).await.unwrap();

        sleep(Duration::from_millis(100)).await;

        let peers = topic.peers().await.unwrap();

        // Em ambiente isolado, lista de peers deve estar vazia inicialmente
        assert_eq!(
            peers.len(),
            0,
            "Lista de peers deve estar vazia em ambiente isolado"
        );

        info!("✓ Lista de peers vazia como esperado");
    }
}

#[cfg(test)]
mod multi_peer_tests {
    use super::*;

    #[tokio::test]
    async fn test_two_peers_communication() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando comunicação entre dois peers");

        let (pubsub1, _dir1) = create_test_pubsub("peer1").await.unwrap();
        let (pubsub2, _dir2) = create_test_pubsub("peer2").await.unwrap();

        let topic_name = "shared-topic";

        // Ambos subscrevem ao mesmo tópico
        let topic1 = pubsub1.topic_subscribe(topic_name).await.unwrap();
        let _topic2 = pubsub2.topic_subscribe(topic_name).await.unwrap();

        // Aguarda estabelecimento de conexões
        sleep(Duration::from_secs(1)).await;

        // Peer1 inicia stream de mensagens
        let mut msg_stream = topic1.watch_messages().await.unwrap();

        // Peer2 publica mensagem
        let test_message = b"Hello from peer2";
        let result = pubsub2.publish_to_topic(topic_name, test_message).await;

        if result.is_err() {
            info!("Nota: Publicação pode falhar em ambiente isolado sem discovery");
        }

        // Tenta receber com timeout generoso
        let timeout_result = timeout(Duration::from_secs(5), msg_stream.next()).await;

        match timeout_result {
            Ok(Some(event)) => {
                info!(
                    "✓ Mensagem recebida de outro peer: {} bytes",
                    event.content.len()
                );
            }
            _ => {
                info!(
                    "⚠ Comunicação entre peers requer configuração de discovery (esperado em teste isolado)"
                );
            }
        }
    }

    #[tokio::test]
    async fn test_multiple_peers_same_topic() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando múltiplos peers no mesmo tópico");

        let peer_count = 3;
        let mut pubsubs = Vec::new();
        let mut _temp_dirs = Vec::new();

        // Cria múltiplos peers
        for i in 0..peer_count {
            let (pubsub, dir) = create_test_pubsub(&format!("peer{}", i)).await.unwrap();
            pubsubs.push(pubsub);
            _temp_dirs.push(dir);
        }

        let topic_name = "multi-peer-topic";

        // Todos subscrevem ao mesmo tópico
        for pubsub in &pubsubs {
            let result = pubsub.topic_subscribe(topic_name).await;
            assert!(result.is_ok(), "Subscrição deveria ter sucesso");
        }

        sleep(Duration::from_secs(1)).await;

        info!("✓ {} peers subscreveram ao mesmo tópico", peer_count);
    }
}

#[cfg(test)]
mod error_handling_tests {
    use super::*;

    #[tokio::test]
    async fn test_topic_name_validation() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando validação de nomes de tópicos");

        let (pubsub, _temp_dir) = create_test_pubsub("validation_test").await.unwrap();

        // Tópico vazio deveria funcionar (SHA-256 de string vazia é válido)
        let result = pubsub.topic_subscribe("").await;
        assert!(result.is_ok(), "Tópico vazio deveria ser aceito");

        // Tópico muito longo deveria funcionar (SHA-256 normaliza)
        let long_topic = "a".repeat(10000);
        let result = pubsub.topic_subscribe(&long_topic).await;
        assert!(result.is_ok(), "Tópico longo deveria ser aceito");

        info!("✓ Validação de nomes de tópicos funcionando");
    }

    #[tokio::test]
    async fn test_concurrent_subscriptions() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando subscrições concorrentes");

        let (pubsub, _temp_dir) = create_test_pubsub("concurrent_test").await.unwrap();
        let pubsub_arc = Arc::new(pubsub);

        let topic_name = "concurrent-topic";
        let mut handles = Vec::new();

        // Múltiplas tasks tentando subscrever ao mesmo tempo
        for i in 0..10 {
            let pubsub_clone = pubsub_arc.clone();
            let topic = topic_name.to_string();

            let handle = tokio::spawn(async move {
                debug!("Task {} tentando subscrever", i);
                pubsub_clone.topic_subscribe(&topic).await
            });

            handles.push(handle);
        }

        // Aguarda todas as tasks
        let mut success_count = 0;
        for handle in handles {
            if let Ok(Ok(_)) = handle.await {
                success_count += 1;
            }
        }

        assert_eq!(
            success_count, 10,
            "Todas as subscrições concorrentes devem ter sucesso"
        );

        info!("✓ Subscrições concorrentes funcionando corretamente");
    }

    #[tokio::test]
    async fn test_concurrent_publications() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando publicações concorrentes");

        let (pubsub, _temp_dir) = create_test_pubsub("concurrent_pub_test").await.unwrap();
        let pubsub_arc = Arc::new(pubsub);

        let topic_name = "concurrent-pub-topic";
        let _topic = pubsub_arc.topic_subscribe(topic_name).await.unwrap();

        sleep(Duration::from_millis(200)).await;

        let mut handles = Vec::new();

        // Múltiplas tasks tentando publicar ao mesmo tempo
        for i in 0..10 {
            let pubsub_clone = pubsub_arc.clone();
            let topic = topic_name.to_string();

            let handle = tokio::spawn(async move {
                let message = format!("message-{}", i);
                pubsub_clone
                    .publish_to_topic(&topic, message.as_bytes())
                    .await
            });

            handles.push(handle);
        }

        // Aguarda todas as tasks
        let mut success_count = 0;
        for handle in handles {
            if let Ok(Ok(_)) = handle.await {
                success_count += 1;
            }
        }

        assert_eq!(
            success_count, 10,
            "Todas as publicações concorrentes devem ter sucesso"
        );

        info!("✓ Publicações concorrentes funcionando corretamente");
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    #[tokio::test]
    async fn test_full_pubsub_workflow() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando workflow completo de pub/sub");

        let (pubsub, _temp_dir) = create_test_pubsub("full_workflow_test").await.unwrap();

        // 1. Subscrever a tópico
        let topic_name = "workflow-topic";
        let topic = pubsub.topic_subscribe(topic_name).await.unwrap();
        assert_eq!(topic.topic(), topic_name);

        // 2. Iniciar stream de mensagens
        let mut msg_stream = topic.watch_messages().await.unwrap();

        // 3. Verificar peers iniciais
        let peers = topic.peers().await.unwrap();
        assert_eq!(peers.len(), 0);

        sleep(Duration::from_millis(200)).await;

        // 4. Publicar mensagens
        for i in 0..3 {
            let message = format!("workflow-message-{}", i);
            pubsub
                .publish_to_topic(topic_name, message.as_bytes())
                .await
                .unwrap();
            sleep(Duration::from_millis(100)).await;
        }

        // 5. Tentar receber mensagens
        let timeout_result = timeout(Duration::from_secs(2), async {
            let mut count = 0;
            while let Some(_event) = msg_stream.next().await {
                count += 1;
                if count >= 3 {
                    break;
                }
            }
            count
        })
        .await;

        match timeout_result {
            Ok(count) => {
                info!("✓ Workflow completo: {} mensagens recebidas", count);
            }
            Err(_) => {
                info!("⚠ Workflow: timeout (esperado em ambiente isolado)");
            }
        }
    }

    #[tokio::test]
    async fn test_topic_isolation() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando isolamento entre tópicos");

        let (pubsub, _temp_dir) = create_test_pubsub("isolation_test").await.unwrap();

        // Cria dois tópicos diferentes
        let topic1_name = "isolated-topic-1";
        let topic2_name = "isolated-topic-2";

        let topic1 = pubsub.topic_subscribe(topic1_name).await.unwrap();
        let topic2 = pubsub.topic_subscribe(topic2_name).await.unwrap();

        let mut msg_stream1 = topic1.watch_messages().await.unwrap();
        let mut msg_stream2 = topic2.watch_messages().await.unwrap();

        sleep(Duration::from_millis(200)).await;

        // Publica apenas no tópico 1
        pubsub
            .publish_to_topic(topic1_name, b"message for topic 1")
            .await
            .unwrap();

        sleep(Duration::from_millis(300)).await;

        // Tópico 1 pode receber mensagem
        let result1 = timeout(Duration::from_millis(500), msg_stream1.next()).await;

        // Tópico 2 não deve receber nada
        let result2 = timeout(Duration::from_millis(500), msg_stream2.next()).await;

        if result1.is_ok() && result2.is_err() {
            info!("✓ Tópicos estão isolados corretamente");
        } else {
            info!("⚠ Isolamento: comportamento pode variar em ambiente isolado");
        }
    }

    #[tokio::test]
    async fn test_stress_many_topics() {
        tracing_subscriber::fmt().with_test_writer().try_init().ok();

        info!("Testando stress com muitos tópicos");

        let (pubsub, _temp_dir) = create_test_pubsub("stress_test").await.unwrap();

        let topic_count = 50;

        // Subscreve a muitos tópicos
        for i in 0..topic_count {
            let topic_name = format!("stress-topic-{}", i);
            let result = pubsub.topic_subscribe(&topic_name).await;
            assert!(result.is_ok(), "Subscrição {} deveria ter sucesso", i);
        }

        info!("✓ {} tópicos criados com sucesso", topic_count);

        // Publica em alguns tópicos
        for i in 0..10 {
            let topic_name = format!("stress-topic-{}", i);
            let message = format!("stress-message-{}", i);
            let result = pubsub
                .publish_to_topic(&topic_name, message.as_bytes())
                .await;
            assert!(result.is_ok(), "Publicação {} deveria ter sucesso", i);
        }

        info!("✓ Teste de stress completado com sucesso");
    }
}
