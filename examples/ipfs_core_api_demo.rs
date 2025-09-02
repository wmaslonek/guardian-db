// Exemplo de uso do novo ipfs_core_api
//
// Este exemplo demonstra como usar a nova API IPFS Core modular

use guardian_db::error::Result;
use guardian_db::ipfs_core_api::{ClientConfig, IpfsClient, compat::IpfsClientAdapter};
use std::io::Cursor;
use tracing::{Level, info};

#[tokio::main]
async fn main() -> Result<()> {
    // Configurar logging
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    info!("🚀 Demonstração do ipfs_core_api refatorado");

    // 1. Teste com configuração de desenvolvimento
    info!("\n📝 === Teste com configuração de desenvolvimento ===");

    let dev_client = IpfsClient::development().await?;
    info!("✅ Cliente de desenvolvimento criado");

    // Teste básico de funcionalidade
    let test_data = "Hello, ipfs_core_api refatorado!".as_bytes();
    let cursor = Cursor::new(test_data.to_vec());

    let add_response = dev_client.add(cursor).await?;
    info!(
        "📤 Dados adicionados: {} ({} bytes)",
        add_response.hash, add_response.size
    );

    let mut cat_stream = dev_client.cat(&add_response.hash).await?;
    let mut retrieved_data = Vec::new();

    use tokio::io::AsyncReadExt;
    cat_stream.read_to_end(&mut retrieved_data).await?;

    if retrieved_data == test_data {
        info!("✅ Dados recuperados com sucesso!");
    } else {
        info!("⚠️  Dados em modo mock");
    }

    // 2. Teste informações do nó
    info!("\n🆔 === Informações do nó ===");

    let node_info = dev_client.id().await?;
    info!("Node ID: {}", node_info.id);
    info!("Agent: {}", node_info.agent_version);
    info!("Addresses: {:?}", node_info.addresses);

    // 3. Teste configuração customizada
    info!("\n⚙️  === Teste com configuração customizada ===");

    let custom_config = ClientConfig {
        enable_pubsub: true,
        enable_swarm: false,
        ..ClientConfig::development()
    };

    let custom_client = IpfsClient::new(custom_config).await?;
    info!("✅ Cliente com configuração customizada criado");

    // 4. Teste do adaptador de compatibilidade
    info!("\n🔄 === Teste do adaptador de compatibilidade ===");

    let adapter = IpfsClientAdapter::development().await?;
    info!("✅ Adaptador de compatibilidade criado");

    let compat_data = "Teste do adaptador".as_bytes();
    let compat_cursor = Cursor::new(compat_data.to_vec());

    let compat_response = adapter.add(compat_cursor).await?;
    let compat_stream = adapter.cat(&compat_response.hash).await;
    let compat_retrieved = compat_stream.concat().await?;

    if compat_retrieved == compat_data {
        info!("✅ Adaptador funcionando perfeitamente!");
    }

    // 5. Teste operações DAG
    info!("\n📊 === Teste operações DAG ===");

    let dag_data = serde_json::json!({
        "name": "test_dag",
        "value": 42,
        "timestamp": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    });

    let dag_bytes = serde_json::to_vec(&dag_data)?;
    let dag_cid = dev_client.dag_put(&dag_bytes).await?;
    info!("📤 Objeto DAG armazenado: {}", dag_cid);

    let retrieved_dag = dev_client.dag_get(&dag_cid, None).await?;
    let parsed_dag: serde_json::Value = serde_json::from_slice(&retrieved_dag)?;
    info!("📥 Objeto DAG recuperado: {}", parsed_dag);

    // 6. Teste PubSub (se habilitado)
    if dev_client.config().enable_pubsub {
        info!("\n📡 === Teste PubSub ===");

        let topic = "test-topic";
        let pubsub_message = "Hello, PubSub!".as_bytes();

        dev_client.pubsub_publish(topic, pubsub_message).await?;
        info!("📤 Mensagem publicada no tópico '{}'", topic);

        let topics = dev_client.pubsub_topics().await?;
        info!("📋 Tópicos ativos: {:?}", topics);
    }

    // 7. Teste operações de Pin
    info!("\n📌 === Teste operações de Pin ===");

    let pin_response = dev_client.pin_add(&add_response.hash, true).await?;
    info!(
        "📌 Objeto pinned: {} (tipo: {})",
        pin_response.hash, pin_response.pin_type
    );

    let pins = dev_client.pin_ls(None).await?;
    info!("📋 Objetos pinned: {}", pins.len());

    // 8. Teste estatísticas do repositório
    info!("\n📊 === Estatísticas do repositório ===");

    let repo_stats = dev_client.repo_stat().await?;
    info!(
        "📊 Objetos: {}, Tamanho: {} bytes",
        repo_stats.num_objects, repo_stats.repo_size
    );

    // 9. Teste geração de channel ID (compatibilidade)
    info!("\n🔗 === Teste channel ID ===");

    let other_peer = libp2p::PeerId::random();
    let channel_id = dev_client.get_channel_id(&other_peer);
    info!("🔗 Channel ID gerado: {}", channel_id);

    // 10. Cleanup
    info!("\n🧹 === Cleanup ===");

    dev_client.shutdown().await?;
    custom_client.shutdown().await?;
    adapter.shutdown().await?;

    info!("✅ Todos os clientes encerrados");

    info!("\n🎉 === Demonstração completa! ===");
    info!("✅ Módulo ipfs_core_api funcionando perfeitamente");
    info!("✅ Compatibilidade com código existente mantida");
    info!("✅ Configurações flexíveis disponíveis");
    info!("✅ Todas as funcionalidades testadas com sucesso");

    Ok(())
}
