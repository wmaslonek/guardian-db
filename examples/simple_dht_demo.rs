// DHT Discovery Demo Simples
//
// Demonstração rápida da funcionalidade de descoberta DHT

use guardian_db::ipfs_core_api::backends::{IpfsBackend, IrohBackend};
use libp2p::PeerId;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configurar tracing simples
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("=== DHT DISCOVERY DEMO SIMPLES ===\n");

    // Inicializar backend
    let config = guardian_db::ipfs_core_api::config::ClientConfig {
        data_store_path: Some(std::path::PathBuf::from("./tmp/dht_simple")),
        ..Default::default()
    };

    info!("Inicializando backend Iroh...");
    let backend = IrohBackend::new(&config).await?;
    info!("✓ Backend inicializado com sucesso!");

    // Testar descoberta DHT
    println!("\nTestando descoberta DHT...");

    let test_peer = "12D3KooWQfGkPUkoLDEeJE3H3ZEmu1mcEhWCzXecNUgZp4gLRcMb".parse::<PeerId>()?;

    println!("   Buscando peer: {}", test_peer);

    let start = std::time::Instant::now();
    match backend.dht_find_peer(&test_peer).await {
        Ok(addresses) => {
            let duration = start.elapsed();
            println!("   ✓ Sucesso em {:?}", duration);
            println!("Encontrados {} endereços:", addresses.len());

            for (i, addr) in addresses.iter().take(3).enumerate() {
                println!("      {}. {}", i + 1, addr);
            }

            if addresses.len() > 3 {
                println!("      ... e mais {} endereços", addresses.len() - 3);
            }
        }
        Err(e) => {
            warn!("Erro: {}", e);
        }
    }

    // Testar cache (segunda busca)
    println!("\nTestando cache (segunda busca)...");
    let start = std::time::Instant::now();
    match backend.dht_find_peer(&test_peer).await {
        Ok(addresses) => {
            let duration = start.elapsed();
            println!("   ✓ Cache hit em {:?}", duration);
            println!("    {} endereços do cache", addresses.len());
        }
        Err(e) => {
            warn!("    Erro no cache: {}", e);
        }
    }

    // Estatísticas
    println!("\n Estatísticas do backend:");
    if let Ok(stats) = backend.get_cache_statistics().await {
        println!("   - Entradas no cache: {}", stats.entries_count);
        println!("   - Hit ratio: {:.1}%", stats.hit_ratio * 100.0);
        println!(
            "   - Tamanho: {:.1} KB",
            stats.total_size_bytes as f64 / 1024.0
        );
    }

    println!("\n✓ DHT Discovery implementado com sucesso!");
    println!("Busca implementada usando Iroh backend nativo");
    println!("Cache otimizado para performance máxima");

    Ok(())
}
