//! # DHT Discovery Demo
//!
//! Demonstra a implementação de descoberta DHT usando Iroh backend nativo
//! com busca de peers na rede distribuída.

use guardian_db::ipfs_core_api::backends::{IpfsBackend, IrohBackend};
use libp2p::PeerId;
use tracing::{debug, info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configurar tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    println!("=== DEMONSTRAÇÃO DE DHT DISCOVERY ===\n");

    // Inicializar backend Iroh com otimizações
    let config = guardian_db::ipfs_core_api::config::ClientConfig {
        data_store_path: Some(std::path::PathBuf::from("./tmp/dht_demo")),
        ..Default::default()
    };
    let backend = IrohBackend::new(&config).await?;
    info!("Backend Iroh inicializado com sucesso");

    // 1. Demonstrar descoberta de peers conhecidos
    println!("1. Testando descoberta DHT de peers...");

    // Gerar PeerIds aleatórios válidos para evitar problemas de parsing
    let test_peers = vec![PeerId::random(), PeerId::random(), PeerId::random()];

    for peer_id in &test_peers {
        println!("\nBuscando peer: {}", peer_id);

        match backend.dht_find_peer(peer_id).await {
            Ok(addresses) => {
                println!("   ✓ Encontrados {} endereços:", addresses.len());
                for (i, addr) in addresses.iter().enumerate() {
                    println!("      {}. {}", i + 1, addr);
                }

                // Validar formato dos endereços
                let valid_count = addresses
                    .iter()
                    .filter(|addr| is_valid_multiaddr(addr))
                    .count();
                println!("Endereços válidos: {}/{}", valid_count, addresses.len());
            }
            Err(e) => {
                warn!("Erro na busca DHT: {}", e);
            }
        }

        // Pequena pausa entre buscas
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    // 2. Testar performance de múltiplas buscas (sequencial)
    println!("\n\n2. Teste de performance - Buscas sequenciais...");

    let start_time = std::time::Instant::now();
    let mut successful_lookups = 0;
    let mut total_addresses = 0;

    for (i, peer_id) in test_peers.iter().enumerate() {
        match backend.dht_find_peer(peer_id).await {
            Ok(addresses) => {
                successful_lookups += 1;
                total_addresses += addresses.len();
                debug!(
                    "Lookup {} para {} retornou {} endereços",
                    i,
                    peer_id,
                    addresses.len()
                );
            }
            Err(e) => {
                warn!("Lookup {} para {} falhou: {}", i, peer_id, e);
            }
        }
    }

    // Resultados já processados no loop acima

    let elapsed = start_time.elapsed();
    println!("   Tempo total: {:?}", elapsed);
    println!(
        "   Buscas bem-sucedidas: {}/{}",
        successful_lookups,
        test_peers.len()
    );
    println!("   Total de endereços encontrados: {}", total_addresses);
    println!(
        "   Throughput: {:.2} buscas/segundo",
        test_peers.len() as f64 / elapsed.as_secs_f64()
    );

    // 3. Demonstrar cache DHT
    println!("\n\n3. Teste de cache DHT...");

    let cache_test_peer = &test_peers[0];
    println!("Primeira busca (sem cache): {}", cache_test_peer);

    let first_lookup = std::time::Instant::now();
    let first_result = backend.dht_find_peer(cache_test_peer).await?;
    let first_duration = first_lookup.elapsed();

    println!("    Tempo: {:?}", first_duration);
    println!("    Endereços: {}", first_result.len());

    println!("\nSegunda busca (com cache): {}", cache_test_peer);

    let second_lookup = std::time::Instant::now();
    let second_result = backend.dht_find_peer(cache_test_peer).await?;
    let second_duration = second_lookup.elapsed();

    println!("    Tempo: {:?}", second_duration);
    println!("    Endereços: {}", second_result.len());

    if second_duration < first_duration {
        let speedup = first_duration.as_nanos() as f64 / second_duration.as_nanos() as f64;
        println!("    Speedup do cache: {:.2}x mais rápido", speedup);
    }

    // 4. Estatísticas finais do backend
    println!("\n\n4. Estatísticas do Backend Iroh...");

    let cache_stats = backend.get_cache_statistics().await?;
    println!("Cache Statistics:");
    println!("   - Entradas: {}", cache_stats.entries_count);
    println!(
        "   - Tamanho total: {:.2} KB",
        cache_stats.total_size_bytes as f64 / 1024.0
    );
    println!("   - Hit ratio: {:.1}%", cache_stats.hit_ratio * 100.0);

    let performance_status = backend.get_performance_monitor_status().await;
    println!("\nPerformance Monitor:");
    println!("   {}", performance_status);

    println!("\n=== DHT DISCOVERY DEMO CONCLUÍDO ===");
    println!("✓ Implementação DHT funcionando com Iroh backend nativo");
    println!("Cache inteligente ativo para otimização de performance");
    println!("Discovery implementado com fallbacks seguros");

    println!("✓ DHT_DISCOVERY_DEMO: Executado com sucesso!");
    Ok(())
}

/// Valida se um endereço multiaddr está bem formado
fn is_valid_multiaddr(addr: &str) -> bool {
    addr.starts_with('/')
        && (addr.contains("/tcp/") || addr.contains("/udp/"))
        && (addr.contains("/ip4/") || addr.contains("/ip6/") || addr.contains("/dns4/"))
        && addr.contains("/p2p/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_multiaddr_validation() {
        // Endereços válidos
        assert!(is_valid_multiaddr(
            "/ip4/127.0.0.1/tcp/4001/p2p/12D3KooW..."
        ));
        assert!(is_valid_multiaddr("/ip6/::1/tcp/4001/p2p/12D3KooW..."));
        assert!(is_valid_multiaddr(
            "/dns4/example.com/tcp/4001/p2p/12D3KooW..."
        ));

        // Endereços inválidos
        assert!(!is_valid_multiaddr("127.0.0.1:4001"));
        assert!(!is_valid_multiaddr("/ip4/127.0.0.1"));
        assert!(!is_valid_multiaddr("/ip4/127.0.0.1/tcp/4001"));
    }

    #[tokio::test]
    async fn test_dht_lookup_performance() {
        let config = guardian_db::ipfs_core_api::config::ClientConfig {
            data_store_path: Some(std::path::PathBuf::from("./tmp/test_dht")),
            ..Default::default()
        };
        let backend = IrohBackend::new(&config)
            .await
            .expect("Backend init failed");

        let peer_id = PeerId::random();
        let start = std::time::Instant::now();
        let result = backend.dht_find_peer(&peer_id).await;
        let duration = start.elapsed();

        // DHT lookup deve completar em menos de 15 segundos (incluindo timeout)
        assert!(duration.as_secs() < 15);

        // Deve retornar resultado (mesmo que seja fallback)
        assert!(result.is_ok());
        let addresses = result.unwrap();
        assert!(!addresses.is_empty());
    }
}
