//! Teste para verificar o registro de access controllers sem mock
//!
//! Este teste verifica que:
//! 1. O mock MockGuardianDB foi removido
//! 2. O registro de access controllers funciona com tipos explícitos
//! 3. Não há dependência de execução de construtores para determinar tipos

use crate::access_controller::{
    manifest::CreateAccessControllerOptions,
    simple::SimpleAccessController,
    traits::{AccessController, Option as AccessControllerOption},
};
use crate::base_guardian::GuardianDB;
use crate::error::{GuardianError, Result};
use crate::iface::BaseGuardianDB;
use crate::ipfs_core_api::config::ClientConfig;
use std::sync::Arc;

#[tokio::test]
async fn test_access_controller_registration_without_mock() -> Result<()> {
    // Criar configuração IPFS para teste com diretório único
    let unique_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let ipfs_config = ClientConfig {
        data_store_path: Some(std::path::PathBuf::from(format!(
            "./tmp/test_ac_reg_{}",
            unique_id
        ))),
        ..ClientConfig::development()
    };

    // Criar instância do GuardianDB
    let guardian_db = GuardianDB::new(Some(ipfs_config), None).await?;

    // Verificar que não há access controllers registrados inicialmente
    let initial_types = guardian_db.access_controller_types_names();
    assert!(
        initial_types.is_empty(),
        "Deve começar sem access controllers registrados"
    );

    // Testar registro com tipo explícito
    let test_constructor = Arc::new(
        |_base_guardian: Arc<dyn BaseGuardianDB<Error = GuardianError>>,
         _options: &CreateAccessControllerOptions,
         _access_controller_options: Option<Vec<AccessControllerOption>>| {
            Box::pin(async move {
                // Mock de um access controller simples para teste
                let options = CreateAccessControllerOptions::new_empty();
                let access_controller = SimpleAccessController::from_options(options)
                    .map_err(|e| GuardianError::Store(e.to_string()))?;
                Ok(Arc::new(access_controller) as Arc<dyn AccessController>)
            })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<Arc<dyn AccessController>>> + Send>,
                >
        },
    );

    // Registrar access controller com tipo explícito (novo método)
    guardian_db
        .register_access_controller_type_with_name("test_simple", test_constructor.clone())?;

    // Verificar que foi registrado
    let registered_types = guardian_db.access_controller_types_names();
    assert_eq!(registered_types.len(), 1);
    assert!(registered_types.contains(&"test_simple".to_string()));

    // Verificar que podemos recuperar o constructor
    let retrieved = guardian_db.get_access_controller_type("test_simple");
    assert!(
        retrieved.is_some(),
        "Deve poder recuperar o constructor registrado"
    );

    // Testar método legado (deve usar tipo padrão "simple")
    let legacy_constructor = Arc::new(
        |_base_guardian: Arc<dyn BaseGuardianDB<Error = GuardianError>>,
         _options: &CreateAccessControllerOptions,
         _access_controller_options: Option<Vec<AccessControllerOption>>| {
            Box::pin(async move {
                let options = CreateAccessControllerOptions::new_empty();
                let access_controller = SimpleAccessController::from_options(options)
                    .map_err(|e| GuardianError::Store(e.to_string()))?;
                Ok(Arc::new(access_controller) as Arc<dyn AccessController>)
            })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<Arc<dyn AccessController>>> + Send>,
                >
        },
    );

    // Usar método legado
    guardian_db
        .register_access_controller_type(legacy_constructor)
        .await?;

    // Verificar que agora temos 2 tipos (test_simple + simple)
    let final_types = guardian_db.access_controller_types_names();
    assert_eq!(final_types.len(), 2);
    assert!(final_types.contains(&"test_simple".to_string()));
    assert!(final_types.contains(&"simple".to_string()));

    println!("✅ Teste passou - access controllers podem ser registrados sem mock");
    println!("   Tipos registrados: {:?}", final_types);

    Ok(())
}

#[tokio::test]
async fn test_no_mock_dependency() -> Result<()> {
    // Este teste verifica que não há mais dependência do mock MockGuardianDB

    let unique_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let ipfs_config = ClientConfig {
        data_store_path: Some(std::path::PathBuf::from(format!(
            "./tmp/test_no_mock_{}",
            unique_id
        ))),
        ..ClientConfig::development()
    };
    let guardian_db = GuardianDB::new(Some(ipfs_config), None).await?;

    // Registrar os access controllers padrão
    guardian_db
        .register_default_access_controller_types()
        .await?;

    // Verificar que o tipo "simple" foi registrado
    let types = guardian_db.access_controller_types_names();
    assert!(
        types.contains(&"simple".to_string()),
        "Tipo 'simple' deve ser registrado por padrão"
    );

    // Verificar que podemos obter o constructor
    let simple_constructor = guardian_db.get_access_controller_type("simple");
    assert!(
        simple_constructor.is_some(),
        "Deve poder obter constructor do tipo 'simple'"
    );

    println!("✅ Access controllers padrão registrados sem dependência de mock");
    println!("   Tipos disponíveis: {:?}", types);

    Ok(())
}
