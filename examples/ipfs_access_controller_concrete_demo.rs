// Example: IpfsAccessController Implementation Demo
//
// Este exemplo demonstra o uso da implementação concreta do IpfsAccessController
// que foi implementada para substituir o fallback temporário.

use guardian_db::{
    access_controller::{
        manifest::{CreateAccessControllerOptions, ManifestParams},
        traits::Option as AccessControllerOption,
        utils::{create, resolve},
    },
    base_guardian::GuardianDB,
    error::{GuardianError, Result},
    ipfs_core_api::client::IpfsClient,
    traits::BaseGuardianDB,
};
use std::sync::Arc;
use tracing::{Level, info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    info!("=== IpfsAccessController Implementation Demo ===");

    // Configure IPFS client
    let _ipfs_client = match IpfsClient::development().await {
        Ok(client) => {
            info!("✓ IPFS client initialized successfully");
            client
        }
        Err(e) => {
            warn!(
                "Failed to initialize IPFS client: {}, using mock for demo",
                e
            );
            // Para demo, continue mesmo se IPFS falhar
            return demonstrate_functionality().await;
        }
    };

    // Configure GuardianDB
    let guardian_db = match GuardianDB::new(None, None).await {
        Ok(db) => {
            info!("✓ GuardianDB initialized successfully");
            Arc::new(db)
        }
        Err(e) => {
            warn!(
                "Failed to initialize GuardianDB: {}, using mock for demo",
                e
            );
            return demonstrate_functionality().await;
        }
    };

    // Test IpfsAccessController creation
    info!("--- Testing IpfsAccessController creation ---");

    let mut params = CreateAccessControllerOptions::default();
    ManifestParams::set_type(&mut params, "ipfs".to_string());

    // Set initial write permissions
    let write_permissions = vec![
        "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".to_string(),
        "did:key:z6MkfrQbzjqHkVjmM4VUN4PotMB6JsL3HbdLn9Lc8qykVGGL".to_string(),
    ];
    ManifestParams::set_access(&mut params, "write".to_string(), write_permissions.clone());

    let options =
        Box::new(|_: &mut dyn guardian_db::access_controller::traits::AccessController| {})
            as AccessControllerOption;

    match create(
        guardian_db.clone() as Arc<dyn BaseGuardianDB<Error = GuardianError>>,
        "ipfs",
        params.clone(),
        options,
    )
    .await
    {
        Ok(manifest_cid) => {
            info!("✓ IpfsAccessController created successfully");
            info!("  Manifest CID: {}", manifest_cid);

            // Test resolving the controller
            info!("--- Testing IpfsAccessController resolution ---");

            let resolve_options =
                Box::new(|_: &mut dyn guardian_db::access_controller::traits::AccessController| {})
                    as AccessControllerOption;
            match resolve(
                guardian_db.clone() as Arc<dyn BaseGuardianDB<Error = GuardianError>>,
                &manifest_cid.to_string(),
                &params,
                resolve_options,
            )
            .await
            {
                Ok(controller) => {
                    info!("✓ IpfsAccessController resolved successfully");
                    info!("  Controller type: {}", controller.get_type());

                    // Test controller functionality
                    info!("--- Testing controller functionality ---");

                    // Test getting authorized keys
                    match controller.get_authorized_by_role("write").await {
                        Ok(authorized_keys) => {
                            info!("✓ Authorized write keys: {:?}", authorized_keys);
                        }
                        Err(e) => {
                            warn!("Failed to get authorized keys: {}", e);
                        }
                    }

                    // Test granting permission
                    let new_key = "did:key:z6MknGc3ocHs3zdPiJbnaaqDi58NGb4pk1Sp9WxWufuXSdxf";
                    match controller.grant("write", new_key).await {
                        Ok(_) => {
                            info!("✓ Successfully granted write permission to {}", new_key);

                            // Verify the permission was added
                            match controller.get_authorized_by_role("write").await {
                                Ok(updated_keys) => {
                                    info!("✓ Updated authorized keys: {:?}", updated_keys);
                                }
                                Err(e) => {
                                    warn!("Failed to verify updated keys: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Failed to grant permission: {}", e);
                        }
                    }

                    // Test saving controller state
                    match controller.save().await {
                        Ok(_) => {
                            info!("✓ Controller state saved successfully to IPFS");
                        }
                        Err(e) => {
                            warn!("Failed to save controller state: {}", e);
                        }
                    }

                    info!("=== Demo completed successfully ===");
                }
                Err(e) => {
                    warn!("Failed to resolve IpfsAccessController: {}", e);
                }
            }
        }
        Err(e) => {
            warn!("Failed to create IpfsAccessController: {}", e);
            info!("This might be expected if IPFS is not available");
        }
    }

    Ok(())
}

async fn demonstrate_functionality() -> Result<()> {
    info!("--- Demonstrating functionality concepts (mock mode) ---");
    info!("=== Mock demo completed ===");
    Ok(())
}
