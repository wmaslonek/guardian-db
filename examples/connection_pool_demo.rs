/// Demonstração do Connection Pool do IrohBackend
///
/// Este exemplo mostra como o connection pool gerencia conexões ativas
/// com peers, incluindo:
/// - Conexão automática e adição ao pool
/// - Listagem de conexões ativas
/// - Atualização de métricas de latência
/// - Cleanup de conexões antigas
/// - Estatísticas do pool
use guardian_db::guardian::error::Result;
use guardian_db::p2p::network::config::ClientConfig;
use guardian_db::p2p::network::core::IrohBackend;
use std::path::PathBuf;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    // Inicializa logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("=== DEMONSTRAÇÃO: CONNECTION POOL ===\n");

    // Configuração do backend
    let config = ClientConfig {
        data_store_path: Some(PathBuf::from("./tmp/iroh_pool_demo")),
        ..Default::default()
    };

    // Cria backend Iroh com connection pool ativo
    println!("Inicializando IrohBackend com Connection Pool...");
    let backend = IrohBackend::new(&config).await?;
    println!("✓ Backend inicializado\n");

    // Obtém informações do nó
    let node_info = backend.id().await?;
    println!("Node ID: {}", node_info.id);
    println!("Endereços: {:?}\n", node_info.addresses);

    // === FASE 1: STATUS INICIAL DO POOL ===
    println!("=== FASE 1: STATUS INICIAL ===");
    let initial_status = backend.get_connection_pool_status().await;
    println!("Status do pool: {}", initial_status);

    let connections = backend.list_active_connections().await;
    println!("Conexões ativas: {}\n", connections.len());

    // === FASE 2: SIMULAÇÃO DE CONEXÕES ===
    println!("=== FASE 2: SIMULAÇÃO DE CONEXÕES ===");
    println!("O connection pool é populado automaticamente quando connect() é chamado");
    println!("Cada conexão bem-sucedida adiciona o peer ao pool\n");

    // Demonstra tentativa de conexão (falhará se não houver peer, mas mostra o fluxo)
    println!("Tentando descobrir peers via discovery...");
    match backend.discover_peers_active(Duration::from_secs(3)).await {
        Ok(discovered) => {
            println!("✓ Descobertos {} peers", discovered.len());

            // Tenta conectar aos primeiros peers descobertos
            for (i, node_addr) in discovered.iter().take(3).enumerate() {
                println!(
                    "\nConectando ao peer {} ({})...",
                    i + 1,
                    node_addr.node_id.fmt_short()
                );
                match backend.connect(&node_addr.node_id).await {
                    Ok(_) => {
                        println!("   ✓ Conexão estabelecida - peer adicionado ao pool!");

                        // Simula atualização de latência
                        let latency = 50.0 + (i as f64 * 10.0);
                        if let Err(e) = backend
                            .update_connection_latency(&node_addr.node_id, latency)
                            .await
                        {
                            println!("   ⚠ Erro ao atualizar latência: {}", e);
                        } else {
                            println!("   Latência registrada: {:.2}ms", latency);
                        }
                    }
                    Err(e) => {
                        println!("   ✗ Falha na conexão: {}", e);
                    }
                }

                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        Err(e) => {
            println!("⚠ Nenhum peer descoberto: {}", e);
            println!("Em produção, peers são descobertos via Pkarr/DNS/mDNS");
        }
    }

    // === FASE 3: STATUS APÓS CONEXÕES ===
    println!("\n=== FASE 3: STATUS DO POOL ===");
    let connections = backend.list_active_connections().await;
    println!("Conexões ativas no pool: {}", connections.len());

    for (i, conn) in connections.iter().enumerate() {
        println!("\n   Conexão {}:", i + 1);
        println!("   - Node ID: {}", conn.node_id.fmt_short());
        println!("   - Endereço: {}", conn.address);
        println!("   - Conectado há: {:?}", conn.connected_at.elapsed());
        println!("   - Último uso: {:?} atrás", conn.last_used.elapsed());
        println!("   - Latência média: {:.2}ms", conn.avg_latency_ms);
        println!("   - Operações: {}", conn.operations_count);
    }

    // === FASE 4: USO DO POOL ===
    println!("\n=== FASE 4: OBTENDO CONEXÕES DO POOL ===");
    if !connections.is_empty() {
        let first_peer = connections[0].node_id;
        println!("Obtendo conexão do pool para: {}", first_peer.fmt_short());

        match backend.get_connection_from_pool(&first_peer).await {
            Ok(conn_info) => {
                println!("✓ Conexão obtida do pool:");
                println!("   - Último uso atualizado automaticamente");
                println!("   - Contador de operações: {}", conn_info.operations_count);
                println!("   - Latência: {:.2}ms", conn_info.avg_latency_ms);
            }
            Err(e) => {
                println!("✗ Erro ao obter conexão: {}", e);
            }
        }
    }

    // === FASE 5: LISTAGEM DE PEERS ===
    println!("\n=== FASE 5: LISTAGEM DE PEERS (USA POOL) ===");
    println!("Listando peers (connection pool + discovery cache)...");
    match backend.peers().await {
        Ok(peers) => {
            println!("✓ {} peers encontrados", peers.len());
            for (i, peer) in peers.iter().enumerate() {
                println!("\n   Peer {}:", i + 1);
                println!("   - ID: {}", peer.id.fmt_short());
                println!("   - Conectado: {}", if peer.connected { "✓" } else { "✗" });
                println!("   - Endereços: {}", peer.addresses.len());
                println!("   - Protocolos: {}", peer.protocols.join(", "));
            }
        }
        Err(e) => {
            println!("✗ Erro ao listar peers: {}", e);
        }
    }

    // === FASE 6: CLEANUP DE CONEXÕES ANTIGAS ===
    println!("\n=== FASE 6: CLEANUP DE CONEXÕES ANTIGAS ===");
    println!("Executando cleanup (timeout: 120s)...");

    match backend
        .cleanup_stale_connections(Duration::from_secs(120))
        .await
    {
        Ok(removed) => {
            println!("✓ Cleanup concluído: {} conexões removidas", removed);
        }
        Err(e) => {
            println!("✗ Erro no cleanup: {}", e);
        }
    }

    // === FASE 7: RELATÓRIO DE PERFORMANCE ===
    println!("\n=== FASE 7: RELATÓRIO DE PERFORMANCE ===");
    let report = backend.generate_performance_report().await;
    println!("{}", report);

    // === FASE 8: MÉTRICAS DO POOL ===
    println!("=== FASE 8: MÉTRICAS DETALHADAS ===");
    let final_connections = backend.list_active_connections().await;
    let pool_status = backend.get_connection_pool_status().await;

    println!("Status do pool: {}", pool_status);

    if !final_connections.is_empty() {
        let total_ops: u64 = final_connections.iter().map(|c| c.operations_count).sum();
        let avg_latency: f64 = final_connections
            .iter()
            .map(|c| c.avg_latency_ms)
            .sum::<f64>()
            / final_connections.len() as f64;

        println!("\nEstatísticas do Pool:");
        println!("   - Total de operações: {}", total_ops);
        println!("   - Latência média: {:.2}ms", avg_latency);
        println!(
            "   - Média de ops/conexão: {:.1}",
            total_ops as f64 / final_connections.len() as f64
        );
    }

    // === RESUMO ===
    println!("\n=== RESUMO DA DEMONSTRAÇÃO ===");
    println!("✓ Connection Pool integrado e funcional");
    println!("✓ Conexões são adicionadas automaticamente no connect()");
    println!("✓ Peers listados via pool + discovery cache");
    println!("✓ Métricas de latência e uso rastreadas");
    println!("✓ Cleanup automático de conexões antigas disponível");
    println!("✓ Relatório de performance inclui estatísticas do pool");

    println!("\nBENEFÍCIOS:");
    println!("   • Reutilização eficiente de conexões");
    println!("   • Rastreamento de métricas por peer");
    println!("   • Identificação de conexões problemáticas");
    println!("   • Otimização de recursos de rede");
    println!("   • Visibilidade completa do estado de conexões");

    println!("\nDemonstração concluída!");

    Ok(())
}
