// Exemplo de uso do kubo_core_api
//
// Este exemplo mostra como usar a nova API IPFS nativa em Rust

use guardian_db::error::{GuardianError, Result};
use guardian_db::kubo_core_api::{
    client::{KuboCoreApiClient, ClientConfig},
    compat::IpfsClientAdapter,
};

#[tokio::main]
async fn main() -> Result<()> {
    // Inicializa logging
    println!("🚀 Iniciando exemplo kubo_core_api");

    // === Exemplo 1: Cliente Nativo ===
    println!("\n📦 Testando cliente nativo...");
    
    let config = ClientConfig {
        enable_pubsub: true,
        enable_swarm: true,
        enable_mdns: true,
        enable_kad: true,
        ..Default::default()
    };
    
    let client = KuboCoreApiClient::new(config).await?;
    
    // Adicionar dados
    let test_data = "Ola, IPFS nativo!".as_bytes();
    let response = client.add(test_data).await?;
    
    println!("✅ Dados adicionados: {}", response.hash);
    
    // Recuperar dados
    let mut stream = client.cat(&response.hash).await?;
    let mut buffer = Vec::new();
    use tokio::io::AsyncReadExt;
    stream.read_to_end(&mut buffer).await
        .map_err(|e| GuardianError::Io(e))?;
    
    println!("✅ Dados recuperados: {}", String::from_utf8_lossy(&buffer));
    
    // Informações do nó
    let info = client.id().await?;
    println!("✅ Nó ID: {}", info.id);
    
    // === Exemplo 2: Modo Compatibilidade ===
    println!("\n🔄 Testando modo compatibilidade...");
    
    let native_client = KuboCoreApiClient::default().await?;
    let compat_client = IpfsClientAdapter::new(native_client);
    
    // Usa a mesma interface do ipfs_api_backend_hyper
    let test_data_2 = "Teste compatibilidade".as_bytes();
    let response_2 = compat_client.add(test_data_2).await?;
    
    println!("✅ Compatibilidade: {}", response_2.hash);
    
    let stream_2 = compat_client.cat(&response_2.hash).await;
    let bytes_2 = stream_2.concat().await?;
    
    println!("✅ Dados recuperados via compatibilidade: {}", 
             String::from_utf8_lossy(&bytes_2));
    
    // === Exemplo 3: Operações Avançadas ===
    println!("\n🔧 Testando operações avançadas...");
    
    // PubSub
    client.pubsub_publish("test-topic", "mensagem de teste".as_bytes()).await?;
    println!("✅ Mensagem publicada no pubsub");
    
    let peers = client.pubsub_peers("test-topic").await?;
    println!("✅ Peers no tópico: {}", peers.len());
    
    // Encerramento
    client.shutdown().await?;
    println!("✅ Cliente encerrado com sucesso");
    
    println!("\n🎉 Todos os testes concluídos!");
    
    Ok(())
}
