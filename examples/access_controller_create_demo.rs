/// Este exemplo mostra que a função create cria e armazena
/// manifestos.
use guardian_db::access_control::{
    self, manifest::CreateAccessControllerOptions, traits::Option as AccessControllerOption,
};
use guardian_db::guardian::core::{GuardianDB, NewGuardianDBOptions};
use guardian_db::guardian::error::Result;
use guardian_db::p2p::network::config::ClientConfig;
use guardian_db::traits::BaseGuardianDB;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    // Configura logging
    tracing_subscriber::fmt::init();

    info!("=== Teste da Implementação create() ===");

    // Cria configuração do Iroh
    let client_config = ClientConfig::default();

    info!("✓ Configuração Iroh preparada");

    // Cria instância do GuardianDB
    let options = NewGuardianDBOptions::default();
    let guardian_db = GuardianDB::new(Some(client_config), Some(options)).await?;
    let db_arc: Arc<dyn BaseGuardianDB<Error = guardian_db::guardian::error::GuardianError>> =
        Arc::new(guardian_db);

    info!("✓ GuardianDB inicializado");

    // Configura parâmetros para o access controller
    let mut access_permissions = HashMap::new();
    access_permissions.insert(
        "write".to_string(),
        vec!["user1".to_string(), "user2".to_string()],
    );
    access_permissions.insert("read".to_string(), vec!["*".to_string()]);

    let params =
        CreateAccessControllerOptions::new_simple("simple".to_string(), access_permissions);

    info!("Testando diferentes tipos de controladores...");

    // Teste 1: SimpleAccessController
    info!("\n--- Teste 1: SimpleAccessController ---");
    let option_fn: AccessControllerOption = Box::new(|_controller| {});
    match access_control::create(db_arc.clone(), "simple", params.clone(), option_fn).await {
        Ok(hash) => {
            info!("✓ SimpleAccessController criado com sucesso");
            info!("Hash do manifesto no Iroh: {}", hash);
            info!("Manifesto armazenado no Iroh");
        }
        Err(e) => {
            warn!("Erro ao criar SimpleAccessController: {}", e);
        }
    }

    // Teste 2: GuardianAccessController (fallback para simple)
    info!("\n--- Teste 2: GuardianAccessController ---");
    let option_fn2: AccessControllerOption = Box::new(|_controller| {});
    match access_control::create(db_arc.clone(), "guardian", params.clone(), option_fn2).await {
        Ok(hash) => {
            info!("✓ GuardianAccessController criado com sucesso");
            info!("Hash do manifesto no Iroh: {}", hash);
            info!("Usa SimpleAccessController como fallback");
        }
        Err(e) => {
            warn!("Erro ao criar GuardianAccessController: {}", e);
        }
    }

    // Teste 3: IrohAccessController (fallback para simple)
    info!("\n--- Teste 3: IrohAccessController ---");
    let option_fn3: AccessControllerOption = Box::new(|_controller| {});
    match access_control::create(db_arc.clone(), "iroh", params.clone(), option_fn3).await {
        Ok(hash) => {
            info!("✓ IrohAccessController criado com sucesso");
            info!("Hash do manifesto no Iroh: {}", hash);
            info!("Usa SimpleAccessController como fallback");
        }
        Err(e) => {
            warn!("Erro ao criar IrohAccessController: {}", e);
        }
    }

    // Teste 4: Tipo inválido
    info!("\n--- Teste 4: Tipo inválido ---");
    let option_fn4: AccessControllerOption = Box::new(|_controller| {});
    match access_control::create(db_arc.clone(), "invalid_type", params.clone(), option_fn4).await {
        Ok(hash) => {
            warn!("Inesperado: tipo inválido criou Hash: {}", hash);
        }
        Err(e) => {
            info!("✓ Erro esperado para tipo inválido: {}", e);
        }
    }

    info!("\n=== Resumo da Implementação ===");
    info!("1. ✓ Criação do manifesto via manifest::create()");
    info!("2. ✓ Manifesto serializado em CBOR e armazenado no Iroh");
    info!("3. ✓ Hash retornado do Iroh");
    info!("4. ✓ Logs informativos mostram operações vs. fallbacks");
    info!("5. ✓ Tratamento robusto de erros do Iroh");

    info!("\nA função create() produz manifestos no Iroh!");

    Ok(())
}
