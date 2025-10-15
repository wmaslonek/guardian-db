use guardian_db::ipfs_core_api::{
    backends::{IpfsBackend, IrohBackend},
    config::ClientConfig,
};

use tracing::{Level, info};

use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Inicializa logging
    tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .init();

    info!("Demonstração de Persistência FsStore do Iroh");

    // Configura diretório temporário para teste
    let temp_dir = std::env::temp_dir().join("guardian_fsstore_test");
    std::fs::create_dir_all(&temp_dir)?;

    info!("Diretório de teste: {}", temp_dir.display());

    // Configura cliente IPFS Iroh com FsStore
    let config = ClientConfig {
        data_store_path: Some(temp_dir.clone()),
        enable_pubsub: false,
        enable_swarm: false,
        ..Default::default()
    };

    // Cria instância do backend Iroh
    let iroh_backend = Arc::new(IrohBackend::new(&config).await?);

    info!("✓ Backend Iroh criado com sucesso");

    // Teste 1: Adicionar conteúdo
    let test_data = b"Hello, FsStore persistence!";
    info!(
        "Adicionando conteúdo: {}",
        String::from_utf8_lossy(test_data)
    );

    let data_reader = Box::pin(std::io::Cursor::new(test_data.to_vec()));
    let add_response = iroh_backend.add(data_reader).await?;
    info!("✓ Conteúdo adicionado com CID: {}", add_response.hash);

    // Teste 2: Recuperar conteúdo
    info!("Recuperando conteúdo...");
    let mut content_stream = iroh_backend.cat(&add_response.hash).await?;
    let mut recovered_data = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut content_stream, &mut recovered_data)
        .await
        .expect("Erro ao ler conteúdo recuperado");

    if recovered_data == test_data {
        info!("✓ Conteúdo recuperado com sucesso e corresponde ao original!");
    } else {
        return Err("Conteúdo recuperado não corresponde ao original".into());
    }

    // Teste 3: Estatísticas e métricas
    info!("Obtendo estatísticas do repositório...");
    let metrics = iroh_backend.metrics().await?;
    info!("Métricas do backend:");
    info!("   - Operações por segundo: {:.2}", metrics.ops_per_second);
    info!("   - Latência média: {:.2}ms", metrics.avg_latency_ms);
    info!("   - Total de operações: {}", metrics.total_operations);
    info!("   - Uso de memória: {} bytes", metrics.memory_usage_bytes);

    // Teste 4: Verificar persistência no filesystem
    info!("Verificando persistência no filesystem...");

    // Verifica se o diretório de dados foi criado
    if temp_dir.exists() {
        info!("✓ Diretório de dados criado: {}", temp_dir.display());

        // Lista conteúdo do diretório
        match std::fs::read_dir(&temp_dir) {
            Ok(entries) => {
                let files: Vec<_> = entries.collect::<Result<Vec<_>, _>>()?;
                info!("Arquivos/diretórios criados: {}", files.len());
                for (i, file) in files.iter().enumerate() {
                    let name = file.file_name().to_string_lossy().to_string();
                    let metadata = file.metadata()?;
                    let file_type = if metadata.is_dir() { "DIR" } else { "FILE" };
                    info!(
                        "   {}. {} [{}] - {} bytes",
                        i + 1,
                        name,
                        file_type,
                        metadata.len()
                    );
                }
            }
            Err(e) => info!("Erro ao listar conteúdo: {}", e),
        }
    } else {
        info!("Diretório de dados não foi encontrado");
    }

    // Teste 5: Operação de Pin
    info!("=== TESTE DE PIN ===");
    match iroh_backend.pin_add(&add_response.hash).await {
        Ok(()) => {
            info!("✓ Conteúdo fixado com sucesso");

            // Lista pins
            match iroh_backend.pin_ls().await {
                Ok(pins) => {
                    info!("Objetos fixados: {}", pins.len());
                    for pin in pins {
                        info!("   - {}: {:?}", pin.cid, pin.pin_type);
                    }
                }
                Err(e) => info!("Erro listando pins: {}", e),
            }
        }
        Err(e) => info!("Erro fixando conteúdo: {}", e),
    }

    info!("Demonstração concluída com sucesso!");
    info!("O conteúdo foi persistido no filesystem e pode ser recuperado em execuções futuras.");

    Ok(())
}
