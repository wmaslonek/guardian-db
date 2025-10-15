use guardian_db::error::Result;
/// Demonstração  da API Iroh
///
/// Este exemplo mostra como usar:
/// - MapEntry.data_reader() para ler dados reais
/// - AsyncSliceReaderExt.read_to_end() para recuperar conteúdo completo
/// - API nativa do Iroh sem placeholders
use guardian_db::ipfs_core_api::IpfsClient;
use tokio::io::AsyncReadExt;
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<()> {
    // Configurar tracing para mostrar logs INFO no console
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    info!("=== Demonstração da API Iroh ===");

    // Criar cliente usando configuração de desenvolvimento
    let client = IpfsClient::development().await?;
    info!("✓ Cliente Iroh inicializado com API concreta");

    // Teste 1: Adicionar dados pequenos
    info!("\n=== Teste 1: Dados Pequenos ===");
    let small_data = b"Dados de teste para API concreta Iroh 0.92.0";
    let add_response = client.add_bytes(small_data.to_vec()).await?;
    info!(
        "Dados pequenos adicionados: {} ({} bytes)",
        add_response.hash,
        small_data.len()
    );

    // Recuperar usando a nova implementação concreta
    match client.cat(&add_response.hash).await {
        Ok(mut retrieved_stream) => {
            let mut retrieved_data = Vec::new();
            retrieved_stream
                .read_to_end(&mut retrieved_data)
                .await
                .map_err(|e| {
                    guardian_db::error::GuardianError::Other(format!("Erro ao ler stream: {}", e))
                })?;

            let retrieved_str = String::from_utf8(retrieved_data).map_err(|e| {
                guardian_db::error::GuardianError::Other(format!("Erro UTF-8: {}", e))
            })?;
            info!("✓ Dados recuperados com API concreta: '{}'", retrieved_str);

            if retrieved_str.as_bytes() == small_data {
                info!("✓ Verificação bem-sucedida: dados idênticos!");
            } else {
                error!("Dados recuperados diferem dos originais");
            }
        }
        Err(e) => {
            error!("Erro na recuperação com API concreta: {}", e);
        }
    }

    // Teste 2: Dados maiores
    info!("\n=== Teste 2: Dados Maiores ===");
    let large_data = b"x".repeat(1024); // 1KB de dados
    let large_add_response = client.add_bytes(large_data.clone()).await?;
    info!(
        "Dados grandes adicionados: {} ({} bytes)",
        large_add_response.hash,
        large_data.len()
    );

    // Verificar recuperação de dados grandes
    match client.cat(&large_add_response.hash).await {
        Ok(mut retrieved_stream) => {
            let mut retrieved_data = Vec::new();
            retrieved_stream
                .read_to_end(&mut retrieved_data)
                .await
                .map_err(|e| {
                    guardian_db::error::GuardianError::Other(format!("Erro ao ler stream: {}", e))
                })?;
            info!(
                "✓ Dados grandes recuperados: {} bytes",
                retrieved_data.len()
            );

            if retrieved_data == large_data {
                info!("✓ Verificação de dados grandes bem-sucedida!");
            } else {
                error!("Dados grandes recuperados diferem dos originais");
            }
        }
        Err(e) => {
            error!("Erro na recuperação de dados grandes: {}", e);
        }
    }

    // Teste 3: Verificar implementação sem placeholder
    info!("\n=== Teste 3: Verificação Anti-Placeholder ===");
    let test_data = b"API_CONCRETA_IROH_SEM_PLACEHOLDER";
    let test_add_response = client.add_bytes(test_data.to_vec()).await?;

    match client.cat(&test_add_response.hash).await {
        Ok(mut retrieved_stream) => {
            let mut retrieved_data = Vec::new();
            retrieved_stream
                .read_to_end(&mut retrieved_data)
                .await
                .map_err(|e| {
                    guardian_db::error::GuardianError::Other(format!("Erro ao ler stream: {}", e))
                })?;

            let retrieved_str = String::from_utf8(retrieved_data).map_err(|e| {
                guardian_db::error::GuardianError::Other(format!("Erro UTF-8: {}", e))
            })?;

            // Verificar se os dados correspondem ao que foi armazenado (teste de integridade)
            if retrieved_str == String::from_utf8_lossy(test_data) {
                info!("✓ SUCESSO: API Iroh funcionando!");
                info!("✓ Dados íntegros usando MapEntry.data_reader() + AsyncSliceReaderExt");
                info!("Dados recuperados: '{}'", retrieved_str);

                // Verificar se não está usando a antiga implementação de placeholder formatado
                if retrieved_str.starts_with("IROH_PLACEHOLDER_DATA_FOR_CID_") {
                    error!("ERRO: Ainda usando implementação antiga de placeholder!");
                } else {
                    info!("✓ CONFIRMADO: Usando API Iroh!");
                }
            } else {
                error!("FALHA: Dados corrompidos durante armazenamento/recuperação!");
                error!("Esperado: '{}'", String::from_utf8_lossy(test_data));
                error!("Recuperado: '{}'", retrieved_str);
            }
        }
        Err(e) => {
            error!("Erro no teste anti-placeholder: {}", e);
        }
    }

    // Teste 4: Verificar informações do nó
    info!("\n=== Teste 4: Informações do Nó ===");
    match client.id().await {
        Ok(node_info) => {
            info!("Informações do nó:");
            info!("  - Node ID: {}", node_info.id);
            info!("  - Agent: {}", node_info.agent_version);
            info!("  - Protocol: {}", node_info.protocol_version);
            info!("  - Addresses: {:?}", node_info.addresses);
        }
        Err(e) => {
            error!("Erro ao obter informações do nó: {}", e);
        }
    }

    info!("\nDemonstração da API Iroh concluída!");
    info!("Implementação usando MapEntry.data_reader() + AsyncSliceReaderExt");

    Ok(())
}
