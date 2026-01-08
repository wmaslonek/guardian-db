/// Demonstração do Key Synchronizer do IrohBackend
///
/// Este exemplo mostra como usar o sistema de sincronização de chaves:
/// - Adicionar/remover peers confiáveis
/// - Sincronizar chaves específicas com peers
/// - Processar mensagens de sincronização
/// - Monitorar estatísticas e conflitos
/// - Forçar sincronização completa
use guardian_db::guardian::error::Result;
use guardian_db::p2p::network::config::ClientConfig;
use guardian_db::p2p::network::core::IrohBackend;
// use guardian_db::p2p::network::core::key_synchronizer::SyncOperation;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    // Inicializa logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("=== DEMONSTRAÇÃO: KEY SYNCHRONIZER ===\n");

    // Configuração do backend
    let config = ClientConfig {
        data_store_path: Some(PathBuf::from("./tmp/iroh_key_sync_demo")),
        ..Default::default()
    };

    // Cria backend Iroh com key synchronizer ativo
    println!("Inicializando IrohBackend com Key Synchronizer...");
    let backend = IrohBackend::new(&config).await?;
    println!("✓ Backend inicializado\n");

    // === FASE 1: INFORMAÇÕES DO NÓ ===
    println!("=== FASE 1: INFORMAÇÕES DO NÓ ===");
    let node_info = backend.id().await?;
    println!("Node ID: {}", node_info.id);

    let key_sync = backend.get_key_synchronizer();
    println!("Key Synchronizer Node ID: {}", key_sync.node_id());
    println!();

    // === FASE 2: ESTATÍSTICAS INICIAIS ===
    println!("=== FASE 2: ESTATÍSTICAS INICIAIS ===");
    let stats = backend.get_key_sync_statistics().await;
    println!("Estatísticas de Sincronização:");
    println!("   - Mensagens sincronizadas: {}", stats.messages_synced);
    println!("   - Mensagens pendentes: {}", stats.pending_messages);
    println!("   - Conflitos detectados: {}", stats.conflicts_detected);
    println!("   - Conflitos resolvidos: {}", stats.conflicts_resolved);
    println!("   - Taxa de sucesso: {:.1}%", stats.success_rate * 100.0);
    println!("   - Latência média: {:.2}ms", stats.avg_sync_latency_ms);
    println!("   - Peers ativos: {}", stats.active_peers);
    println!();

    // === FASE 3: GERENCIAMENTO DE PEERS CONFIÁVEIS ===
    println!("=== FASE 3: GERENCIAMENTO DE PEERS CONFIÁVEIS ===");

    // Lista peers confiáveis iniciais
    let trusted_peers = backend.list_trusted_peers_for_sync().await;
    println!("👥 Peers confiáveis: {}", trusted_peers.len());

    // Demonstração de adição de peer confiável (precisa de VerifyingKey)
    println!("\nPara adicionar peers confiáveis, use:");
    println!("   backend.add_trusted_peer_for_sync(node_id, verifying_key).await?;");
    println!("   Exemplo: peer deve ter chave pública Ed25519 válida");
    println!();

    // === FASE 4: LISTAGEM DE CHAVES SINCRONIZADAS ===
    println!("=== FASE 4: CHAVES SINCRONIZADAS ===");
    let synced_keys = backend.list_synchronized_keys().await;
    println!("Total de chaves sincronizadas: {}", synced_keys.len());

    if synced_keys.is_empty() {
        println!("   Nenhuma chave sincronizada ainda");
    } else {
        println!("   Chaves:");
        for (i, key_id) in synced_keys.iter().enumerate().take(5) {
            println!("   {}. {}", i + 1, key_id);

            // Verifica status de cada chave
            if let Some(status) = backend.get_key_sync_status(key_id).await {
                println!("      Status: {:?}", status);
            }
        }

        if synced_keys.len() > 5 {
            println!("   ... e {} mais", synced_keys.len() - 5);
        }
    }
    println!();

    // === FASE 5: SINCRONIZAÇÃO DE CHAVES ===
    println!("=== FASE 5: SINCRONIZAÇÃO DE CHAVES ===");
    println!("Para sincronizar uma chave específica:");
    println!("   backend.sync_key_with_peers(\"my_key_id\", SyncOperation::Create).await?;");
    println!();
    println!("Operações disponíveis:");
    println!("   - SyncOperation::Create      - Criar nova chave");
    println!("   - SyncOperation::Update      - Atualizar chave existente");
    println!("   - SyncOperation::Delete      - Deletar chave");
    println!("   - SyncOperation::MetadataSync - Sincronizar apenas metadados");
    println!();

    // Exemplo de sincronização (comentado pois precisa de key_id)
    // println!("Sincronizando chave de exemplo...");
    // backend.sync_key_with_peers("example_key", SyncOperation::MetadataSync).await?;
    // println!("✓ Sincronização iniciada");
    // println!();

    // === FASE 6: SINCRONIZAÇÃO COMPLETA ===
    println!("=== FASE 6: SINCRONIZAÇÃO COMPLETA ===");
    println!("Forçando sincronização completa de todas as chaves...");
    match backend.force_full_key_sync().await {
        Ok(_) => {
            println!("✓ Sincronização completa iniciada");
            println!("   Todas as chaves do keystore serão sincronizadas com peers");
        }
        Err(e) => {
            println!("⚠ Erro ao forçar sincronização: {}", e);
        }
    }
    println!();

    // === FASE 7: EXPORTAÇÃO DE CONFIGURAÇÃO ===
    println!("=== FASE 7: EXPORTAÇÃO DE CONFIGURAÇÃO ===");
    match backend.export_key_sync_config().await {
        Ok(config_bytes) => {
            println!("✓ Configuração de sincronização exportada");
            println!("   Tamanho: {} bytes", config_bytes.len());
            println!("   Contém: peers confiáveis, chaves sincronizadas, estatísticas");
        }
        Err(e) => {
            println!("⚠ Erro ao exportar configuração: {}", e);
        }
    }
    println!();

    // === FASE 8: RELATÓRIO DE SINCRONIZAÇÃO ===
    println!("=== FASE 8: RELATÓRIO DE SINCRONIZAÇÃO ===");
    let sync_report = backend.generate_key_sync_report().await;
    println!("{}", sync_report);

    // === FASE 9: ESTATÍSTICAS JSON ===
    println!("=== FASE 9: EXPORTAÇÃO JSON ===");
    match backend.export_sync_statistics_json().await {
        Ok(json) => {
            println!("✓ Estatísticas exportadas como JSON:");
            println!("{}", json);
        }
        Err(e) => {
            println!("⚠ Erro ao exportar JSON: {}", e);
        }
    }
    println!();

    // === FASE 10: RELATÓRIO DE PERFORMANCE INTEGRADO ===
    println!("=== FASE 10: PERFORMANCE REPORT (COM KEY SYNC) ===");
    let performance_report = backend.generate_performance_report().await;
    println!("{}", performance_report);

    // === RESUMO ===
    println!("=== RESUMO DA DEMONSTRAÇÃO ===");
    println!("✓ Key Synchronizer totalmente integrado ao backend");
    println!("✓ Gerenciamento de peers confiáveis disponível");
    println!("✓ Sincronização de chaves específicas e completa");
    println!("✓ Monitoramento de estatísticas e conflitos");
    println!("✓ Exportação de configuração e relatórios");
    println!("✓ Integrado ao relatório de performance");

    println!("\nRECURSOS PRINCIPAIS:");
    println!("   • Sincronização criptograficamente segura (Ed25519)");
    println!("   • Prevenção de replay attacks");
    println!("   • Detecção e resolução de conflitos");
    println!("   • Controle de versão de chaves");
    println!("   • Peers confiáveis com verificação de assinaturas");
    println!("   • Métricas detalhadas de sincronização");

    println!("\nSEGURANÇA:");
    println!("   • Todas mensagens assinadas com Ed25519");
    println!("   • Validação de timestamps (anti-replay)");
    println!("   • Apenas peers confiáveis podem sincronizar");
    println!("   • Verificação de integridade de metadados");

    println!("\nDemonstração concluída!");

    Ok(())
}
