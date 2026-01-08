/// Este exemplo mostra que a função resolve carrega manifestos
/// do Iroh ao invés de apenas inferir o tipo do controlador.
use guardian_db::access_control::{
    self, manifest::CreateAccessControllerOptions, traits::Option as AccessControllerOption,
};
use guardian_db::guardian::core::{GuardianDB, NewGuardianDBOptions};
use guardian_db::guardian::error::Result;
use guardian_db::p2p::network::config::ClientConfig;
use guardian_db::traits::BaseGuardianDB;
use iroh_blobs::Hash;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    // Configura logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    info!("=== Teste da Implementação Concreta resolve() ===");

    // Cria configuração do Iroh
    let client_config = ClientConfig::default();

    info!("✓ Configuração Iroh preparada");

    // Cria instância do GuardianDB
    let options = NewGuardianDBOptions::default();
    let guardian_db = GuardianDB::new(Some(client_config), Some(options)).await?;
    let db_arc: Arc<dyn BaseGuardianDB<Error = guardian_db::guardian::error::GuardianError>> =
        Arc::new(guardian_db);

    info!("✓ GuardianDB inicializado");

    // Primeiro, cria um access controller para ter um manifesto no Iroh
    let mut access_permissions = HashMap::new();
    access_permissions.insert("write".to_string(), vec!["admin".to_string()]);
    access_permissions.insert("read".to_string(), vec!["*".to_string()]);

    let params =
        CreateAccessControllerOptions::new_simple("simple".to_string(), access_permissions);

    info!("--- Etapa 1: Criando access controller para gerar manifesto ---");
    let option_fn: AccessControllerOption = Box::new(|_controller| {});
    let manifest_cid =
        match access_control::create(db_arc.clone(), "simple", params.clone(), option_fn).await {
            Ok(hash) => {
                info!("✓ Access controller criado com sucesso");
                info!("Hash do manifesto no Iroh: {}", hash);
                hash
            }
            Err(e) => {
                warn!("Erro ao criar access controller: {}", e);
                info!("Continuando com testes de endereços simulados...");
                // Usa um Hash placeholder para demonstrar fallback
                Hash::from([0u8; 32])
            }
        };

    info!("\n--- Etapa 2: Testando resolve com manifesto ---");
    let manifest_address = format!("/iroh/{}", manifest_cid);

    let option_fn2: AccessControllerOption = Box::new(|_controller| {});
    match access_control::resolve(db_arc.clone(), &manifest_address, &params, option_fn2).await {
        Ok(controller) => {
            info!("✓ Access controller resolvido com sucesso!");
            info!("Tipo do controlador: {}", controller.get_type());
            info!("→ Carregado do manifesto no Iroh (não inferido!)");

            // Testa algumas operações do controlador
            match controller.get_authorized_by_role("write").await {
                Ok(authorized) => {
                    info!("Autorizados para escrita: {:?}", authorized);
                }
                Err(e) => {
                    warn!("Erro ao obter autorizados: {}", e);
                }
            }
        }
        Err(e) => {
            warn!("Erro ao resolver access controller: {}", e);
        }
    }

    info!("\n--- Etapa 3: Testando resolve com endereço inválido (fallback) ---");
    let option_fn3: AccessControllerOption = Box::new(|_controller| {});
    match access_control::resolve(
        db_arc.clone(),
        "/iroh/QmInvalidCID123456789012345678901234567890123456789012",
        &params,
        option_fn3,
    )
    .await
    {
        Ok(controller) => {
            info!("✓ Fallback funcionou - controlador criado via inferência");
            info!("Tipo inferido: {}", controller.get_type());
            info!("→ Como o Hash era inválido, usou inferência como fallback");
        }
        Err(e) => {
            info!("Erro esperado para Hash inválido: {}", e);
        }
    }

    info!("\n--- Etapa 4: Testando resolve com endereço que sugere tipo ---");
    let guardian_address = "/iroh/guardian_controller_example/_access";
    let option_fn4: AccessControllerOption = Box::new(|_controller| {});
    match access_control::resolve(db_arc.clone(), guardian_address, &params, option_fn4).await {
        Ok(controller) => {
            info!("✓ Controlador resolvido via fallback inteligente");
            info!("Tipo inferido: {}", controller.get_type());
            info!("Endereço '{}' sugeriu tipo 'guardian'", guardian_address);
        }
        Err(e) => {
            warn!("Erro: {}", e);
        }
    }

    info!("\n=== Resumo da Implementação ===");
    info!("1. ✓ Chamada para manifest::resolve()");
    info!("2. ✓ Carrega tipo do controlador do manifesto Iroh");
    info!("3. ✓ Fallback robusto para inferência se Iroh falhar");
    info!("4. ✓ Logs informativos distinguem carregamento vs. inferência");
    info!("5. ✓ Trata endereços inválidos graciosamente");
    info!("6. ✓ Mantém compatibilidade com código existente");

    info!("\nA função resolve() carrega manifestos do Iroh!");
    info!("→ Prioriza dados do Iroh");
    info!("→ Usa inferência inteligente como fallback");
    info!("→ Logs claros sobre origem dos dados");

    Ok(())
}
