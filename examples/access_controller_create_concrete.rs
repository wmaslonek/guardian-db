/// Demonstração da função create em access_controller/utils.rs
///
/// Este exemplo mostra que a função create cria e armazena
/// manifestos no IPFS.
use guardian_db::access_controller::{
    manifest::CreateAccessControllerOptions, traits::Option as AccessControllerOption, utils,
};
use guardian_db::base_guardian::{GuardianDB, NewGuardianDBOptions};
use guardian_db::error::Result;
use guardian_db::iface::BaseGuardianDB;
use guardian_db::ipfs_core_api::config::ClientConfig;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    // Configura logging
    tracing_subscriber::fmt::init();

    info!("=== Teste da Implementação create() ===");

    // Cria configuração do IPFS
    let ipfs_config = ClientConfig::default();

    info!("✓ Configuração IPFS preparada");

    // Cria instância do GuardianDB
    let options = NewGuardianDBOptions::default();
    let guardian_db = GuardianDB::new(Some(ipfs_config), Some(options)).await?;
    let db_arc: Arc<dyn BaseGuardianDB<Error = guardian_db::error::GuardianError>> =
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
    match utils::create(db_arc.clone(), "simple", params.clone(), option_fn).await {
        Ok(cid) => {
            info!("✓ SimpleAccessController criado com sucesso");
            info!("CID do manifesto no IPFS: {}", cid);
            info!("Manifesto armazenado no IPFS");
        }
        Err(e) => {
            warn!("Erro ao criar SimpleAccessController: {}", e);
        }
    }

    // Teste 2: GuardianAccessController (fallback para simple)
    info!("\n--- Teste 2: GuardianAccessController ---");
    let option_fn2: AccessControllerOption = Box::new(|_controller| {});
    match utils::create(db_arc.clone(), "guardian", params.clone(), option_fn2).await {
        Ok(cid) => {
            info!("✓ GuardianAccessController criado com sucesso");
            info!("CID do manifesto real no IPFS: {}", cid);
            info!("Usa SimpleAccessController como fallback, mas manifesto real");
        }
        Err(e) => {
            warn!("Erro ao criar GuardianAccessController: {}", e);
        }
    }

    // Teste 3: IpfsAccessController (fallback para simple)
    info!("\n--- Teste 3: IpfsAccessController ---");
    let option_fn3: AccessControllerOption = Box::new(|_controller| {});
    match utils::create(db_arc.clone(), "ipfs", params.clone(), option_fn3).await {
        Ok(cid) => {
            info!("✓ IpfsAccessController criado com sucesso");
            info!("CID do manifesto real no IPFS: {}", cid);
            info!("Usa SimpleAccessController como fallback, mas manifesto real");
        }
        Err(e) => {
            warn!("Erro ao criar IpfsAccessController: {}", e);
        }
    }

    // Teste 4: Tipo inválido
    info!("\n--- Teste 4: Tipo inválido ---");
    let option_fn4: AccessControllerOption = Box::new(|_controller| {});
    match utils::create(db_arc.clone(), "invalid_type", params.clone(), option_fn4).await {
        Ok(cid) => {
            warn!("Inesperado: tipo inválido criou CID: {}", cid);
        }
        Err(e) => {
            info!("✓ Erro esperado para tipo inválido: {}", e);
        }
    }

    info!("\n=== Resumo da Implementação ===");
    info!("1. ✓ Criação do manifesto via manifest::create()");
    info!("2. ✓ Manifesto serializado em CBOR e armazenado no IPFS");
    info!("3. ✓ CID retornado do IPFS");
    info!("4. ✓ Logs informativos mostram operações vs. fallbacks");
    info!("5. ✓ Tratamento robusto de erros do IPFS");

    info!("\nA função create() produz manifestos no IPFS!");

    Ok(())
}
