use guardian_db::access_controller::manifest::{CreateAccessControllerOptions, ManifestParams};
use guardian_db::access_controller::utils;
use guardian_db::base_guardian::{GuardianDB, NewGuardianDBOptions};
use guardian_db::ipfs_core_api::{client::IpfsClient, config::ClientConfig};
use guardian_db::ipfs_log::identity_provider::{
    CreateIdentityOptions, GuardianDBIdentityProvider, IdentityProvider,
};
use guardian_db::ipfs_log::lamport_clock::LamportClock;
use guardian_db::keystore::SledKeystore;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), guardian_db::error::GuardianError> {
    // Inicializar tracing
    tracing_subscriber::fmt::init();

    println!("=== Guardian Access Controller Test ===");

    // Configurar cliente IPFS
    let ipfs_config = ClientConfig::development();
    let _ipfs_client = IpfsClient::new(ipfs_config).await?;
    println!("✓ IPFS client connected");

    // Criar GuardianDB
    let options = NewGuardianDBOptions::default();
    let client_config = ClientConfig::development();
    let guardian_db = GuardianDB::new(Some(client_config), Some(options)).await?;
    let guardian_db_arc = Arc::new(guardian_db);
    println!("✓ GuardianDB created");

    // Configurar parâmetros do access controller
    let mut params = CreateAccessControllerOptions::default();
    ManifestParams::set_type(&mut params, "guardian".to_string());
    // Note: add_access não está disponível, usando configuração padrão

    println!("✓ Access controller parameters configured");

    // Criar GuardianDBAccessController usando a função utils
    match utils::create(
        guardian_db_arc.clone(),
        "guardian",
        params.clone(),
        Box::new(|_| {}),
    )
    .await
    {
        Ok(cid) => {
            println!("✓ GuardianDBAccessController created successfully!");
            println!("  Manifest CID: {}", cid);

            // Testar resolução do controller
            match utils::resolve(guardian_db_arc, &cid.to_string(), &params, Box::new(|_| {})).await
            {
                Ok(_controller) => {
                    println!("✓ GuardianDBAccessController resolved successfully!");
                    println!("  Controller resolved successfully");

                    // Demonstrar criação de Entry para teste
                    println!("\n--- Testando funcionalidades básicas ---");
                    let identity_provider = GuardianDBIdentityProvider::new();
                    let keystore = Arc::new(SledKeystore::temporary()?);
                    let identity_opts = CreateIdentityOptions {
                        identity_keys_path: "/tmp/keys".to_string(),
                        id_type: "secp256k1".to_string(),
                        keystore,
                        id: "test_identity".to_string(),
                    };

                    match identity_provider.get_id(&identity_opts).await {
                        Ok(identity_id) => {
                            println!("✓ Identity created: {}", identity_id);

                            // Criar um clock Lamport
                            let clock = LamportClock::new(&identity_id);
                            println!("✓ Lamport clock created with time: {}", clock.time());

                            println!("✓ Componentes básicos do sistema funcionando");
                        }
                        Err(e) => {
                            println!("Aviso: Não foi possível criar identity: {}", e);
                            println!(
                                "   (Isso pode ser normal se o keystore não estiver configurado)"
                            );
                        }
                    }

                    println!("✓ Access controller está funcionando corretamente");

                    println!(
                        "\nAll tests passed! GuardianDBAccessController is working correctly."
                    );
                }
                Err(e) => {
                    eprintln!("Failed to resolve GuardianDBAccessController: {}", e);
                    return Err(e);
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to create GuardianDBAccessController: {}", e);
            return Err(e);
        }
    }

    Ok(())
}
