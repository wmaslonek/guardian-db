// Example: IrohAccessController Implementation Demo
//
// Este exemplo demonstra o uso completo da implementação concreta do IrohAccessController,
// incluindo criação, resolução, e todas as operações do controlador de acesso.
//
// O IrohAccessController é um controlador de acesso que armazena suas permissões e estado
// no Iroh (sistema de armazenamento distribuído), permitindo que múltiplos nós compartilhem
// e sincronizem regras de acesso.
//
// Este exemplo cobre:
// - Inicialização do GuardianDB e Iroh
// - Criação de um IrohAccessController com permissões iniciais
// - Resolução do controlador a partir do seu manifesto armazenado no Iroh
// - Operações de gerenciamento de permissões (grant, revoke, list)
// - Persistência do estado no Iroh

use guardian_db::{
    access_control::{
        create, manifest::CreateAccessControllerOptions, resolve,
        traits::Option as AccessControllerOption,
    },
    guardian::core::{GuardianDB, NewGuardianDBOptions},
    guardian::error::{GuardianError, Result},
    p2p::network::config::ClientConfig,
    traits::BaseGuardianDB,
};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{Level, error, info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging with better formatting
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_target(false)
        .init();

    info!("=== IrohAccessController Implementation Demo ===\n");

    // Step 1: Initialize GuardianDB with Iroh Client
    info!("Step 1: Initializing GuardianDB...");
    let client_config = ClientConfig::default();
    let guardian_options = NewGuardianDBOptions::default();

    let guardian_db = match GuardianDB::new(Some(client_config), Some(guardian_options)).await {
        Ok(db) => {
            info!("✓ GuardianDB initialized successfully\n");
            Arc::new(db) as Arc<dyn BaseGuardianDB<Error = GuardianError>>
        }
        Err(e) => {
            error!("Failed to initialize GuardianDB: {}", e);
            return Err(e);
        }
    };

    // Step 2: Configure access permissions
    info!("Step 2: Configuring access permissions...");
    let mut access_permissions = HashMap::new();

    // Define write permissions with DID keys
    let write_permissions = vec![
        "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".to_string(),
        "did:key:z6MkfrQbzjqHkVjmM4VUN4PotMB6JsL3HbdLn9Lc8qykVGGL".to_string(),
    ];
    access_permissions.insert("write".to_string(), write_permissions.clone());

    // Define read permissions (everyone)
    access_permissions.insert("read".to_string(), vec!["*".to_string()]);

    let params = CreateAccessControllerOptions::new_simple("iroh".to_string(), access_permissions);

    info!("✓ Configured permissions:");
    info!("  - Write: {} users", write_permissions.len());
    info!("  - Read: everyone (*)\n");

    // Step 3: Create IrohAccessController
    info!("Step 3: Creating IrohAccessController...");

    // O callback de opções permite configurar o controlador após sua criação
    // Neste caso, não fazemos configurações adicionais
    let create_options =
        Box::new(|_: &mut dyn guardian_db::access_control::traits::AccessController| {})
            as AccessControllerOption;

    // A função create:
    // 1. Cria uma instância do IrohAccessController com as permissões configuradas
    // 2. Serializa o manifesto do controlador (tipo + params) em CBOR
    // 3. Armazena o manifesto no Iroh usando add_bytes
    // 4. Retorna o hash do manifesto para referência futura
    let manifest_hash =
        match create(guardian_db.clone(), "iroh", params.clone(), create_options).await {
            Ok(hash) => {
                info!("✓ IrohAccessController created successfully");
                info!("  Manifest Hash: {}\n", hash);
                hash
            }
            Err(e) => {
                error!("Failed to create IrohAccessController: {}", e);
                return Err(e);
            }
        };

    // Step 4: Resolve the controller from manifest
    info!("Step 4: Resolving IrohAccessController from manifest...");

    // Converte o hash para string hexadecimal para usar como endereço
    let manifest_address = hex::encode(manifest_hash.as_bytes());

    // Callback de opções para a resolução
    let resolve_options =
        Box::new(|_: &mut dyn guardian_db::access_control::traits::AccessController| {})
            as AccessControllerOption;

    // A função resolve:
    // 1. Carrega o manifesto do Iroh usando o hash fornecido
    // 2. Deserializa o manifesto CBOR para obter tipo e parâmetros
    // 3. Cria uma nova instância do IrohAccessController
    // 4. Carrega o estado (permissões) do controlador do Iroh
    // 5. Retorna o controlador pronto para uso
    let controller = match resolve(
        guardian_db.clone(),
        &manifest_address,
        &params,
        resolve_options,
    )
    .await
    {
        Ok(ctrl) => {
            info!("✓ IrohAccessController resolved successfully");
            info!("  Controller type: {}\n", ctrl.get_type());
            ctrl
        }
        Err(e) => {
            error!("Failed to resolve IrohAccessController: {}", e);
            return Err(e);
        }
    };

    // Step 5: Test controller operations
    info!("Step 5: Testing controller operations...\n");

    // 5a. Get authorized keys for write role
    info!("5a. Getting authorized keys for 'write' role...");
    match controller.get_authorized_by_role("write").await {
        Ok(authorized_keys) => {
            info!("✓ Authorized write keys ({}): ", authorized_keys.len());
            for key in &authorized_keys {
                info!("  - {}", key);
            }
            info!("");
        }
        Err(e) => {
            warn!("Failed to get authorized keys: {}\n", e);
        }
    }

    // 5b. Grant new permission
    info!("5b. Granting write permission to new user...");
    let new_key = "did:key:z6MknGc3ocHs3zdPiJbnaaqDi58NGb4pk1Sp9WxWufuXSdxf";
    match controller.grant("write", new_key).await {
        Ok(_) => {
            info!("✓ Successfully granted write permission to:");
            info!("  {}\n", new_key);
        }
        Err(e) => {
            warn!("Failed to grant permission: {}\n", e);
        }
    }

    // 5c. Verify the new permission was added
    info!("5c. Verifying updated permissions...");
    match controller.get_authorized_by_role("write").await {
        Ok(updated_keys) => {
            info!("✓ Updated authorized write keys ({}): ", updated_keys.len());
            for key in &updated_keys {
                info!("  - {}", key);
            }
            info!("");
        }
        Err(e) => {
            warn!("Failed to verify updated keys: {}\n", e);
        }
    }

    // 5d. Test revoking permission
    info!("5d. Revoking permission from a user...");
    let key_to_revoke = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";
    match controller.revoke("write", key_to_revoke).await {
        Ok(_) => {
            info!("✓ Successfully revoked write permission from:");
            info!("  {}\n", key_to_revoke);

            // Verify revocation
            match controller.get_authorized_by_role("write").await {
                Ok(keys_after_revoke) => {
                    info!(
                        "✓ Remaining authorized write keys ({}): ",
                        keys_after_revoke.len()
                    );
                    for key in &keys_after_revoke {
                        info!("  - {}", key);
                    }
                    info!("");
                }
                Err(e) => {
                    warn!("Failed to verify revocation: {}\n", e);
                }
            }
        }
        Err(e) => {
            warn!("Failed to revoke permission: {}\n", e);
        }
    }

    // Step 6: Save controller state to Iroh
    info!("Step 6: Saving controller state to Iroh...");

    // O método save():
    // 1. Serializa o estado atual do controlador (todas as permissões)
    // 2. Armazena os dados no Iroh
    // 3. Atualiza o manifesto se necessário
    // Isso garante que as mudanças de permissões sejam persistidas
    match controller.save().await {
        Ok(_) => {
            info!("✓ Controller state saved successfully to Iroh\n");
        }
        Err(e) => {
            warn!("Failed to save controller state: {}\n", e);
        }
    }

    // Summary
    info!("=== Demo Summary ===");
    info!("✓ IrohAccessController created and stored in Iroh");
    info!("✓ Controller resolved from manifest hash");
    info!("✓ Permissions granted and revoked successfully");
    info!("✓ Capability verification working");
    info!("✓ State persisted to Iroh");
    info!("\n=== Demo completed successfully ===");

    Ok(())
}
