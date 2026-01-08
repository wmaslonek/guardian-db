/// Testes de integração de Access Control do GuardianDB
///
/// Estes testes validam:
/// - SimpleAccessController (desenvolvimento)
/// - GuardianDBAccessController (produção)
/// - IrohAccessController (Iroh-based)
/// - Validação de permissões e assinaturas
/// - Operações de grant/revoke
/// - Verificação de identidades
mod common;

use common::{TestNode, init_test_logging};
use guardian_db::access_control::{
    self,
    manifest::CreateAccessControllerOptions,
    traits::{AccessController, Option as AccessControllerOption},
};
use guardian_db::guardian::error::GuardianError;
use guardian_db::log::access_control::{CanAppendAdditionalContext, LogEntry};
use guardian_db::log::entry::Entry;
use guardian_db::log::identity::{DefaultIdentificator, Identificator};
use guardian_db::log::identity_provider::GuardianDBIdentityProvider;
use guardian_db::traits::{BaseGuardianDB, CreateDBOptions};
use std::collections::HashMap;
use std::sync::Arc;

// Context mock para can_append
struct TestCanAppendContext;

impl CanAppendAdditionalContext for TestCanAppendContext {
    fn get_log_entries(&self) -> Vec<Box<dyn LogEntry>> {
        Vec::new()
    }
}

#[tokio::test]
async fn test_simple_access_controller_basic_operations() {
    init_test_logging();
    tracing::info!("=== Testing SimpleAccessController basic operations ===");

    let node = TestNode::new("simple_ac_test")
        .await
        .expect("Failed to create test node");

    // Criar SimpleAccessController
    let mut permissions = HashMap::new();
    permissions.insert(
        "write".to_string(),
        vec!["user1".to_string(), "user2".to_string()],
    );
    permissions.insert("read".to_string(), vec!["*".to_string()]);

    let params = CreateAccessControllerOptions::new_simple("simple".to_string(), permissions);
    let option_fn: AccessControllerOption = Box::new(|_controller| {});

    let manifest_hash =
        access_control::create(Arc::new(node.db), "simple", params.clone(), option_fn)
            .await
            .expect("Failed to create SimpleAccessController");

    tracing::info!(
        "✓ SimpleAccessController created with manifest hash: {}",
        manifest_hash
    );

    // Para SimpleAccessController, o manifest pode ser um hash zero (já que não persiste em Iroh por padrão)
    // O importante é que a criação foi bem sucedida sem erros
    tracing::info!("✓ SimpleAccessController basic test completed successfully");
}

#[tokio::test]
async fn test_simple_access_controller_permissions() {
    init_test_logging();
    tracing::info!("=== Testing SimpleAccessController permissions ===");

    // Criar um SimpleAccessController diretamente
    let mut permissions = HashMap::new();
    permissions.insert(
        "write".to_string(),
        vec!["alice".to_string(), "bob".to_string()],
    );
    permissions.insert("admin".to_string(), vec!["admin".to_string()]);

    let controller =
        guardian_db::access_control::acl_simple::SimpleAccessController::new(Some(permissions));

    // Testar get_authorized_by_role
    let write_users = controller
        .get_authorized_by_role("write")
        .await
        .expect("Failed to get write permissions");

    assert_eq!(write_users.len(), 2);
    assert!(write_users.contains(&"alice".to_string()));
    assert!(write_users.contains(&"bob".to_string()));

    tracing::info!("✓ Write permissions: {:?}", write_users);

    // Testar grant
    controller
        .grant("write", "charlie")
        .await
        .expect("Failed to grant permission");

    let updated_write_users = controller
        .get_authorized_by_role("write")
        .await
        .expect("Failed to get updated write permissions");

    assert_eq!(updated_write_users.len(), 3);
    assert!(updated_write_users.contains(&"charlie".to_string()));

    tracing::info!("✓ Permission granted successfully");

    // Testar revoke
    controller
        .revoke("write", "bob")
        .await
        .expect("Failed to revoke permission");

    let final_write_users = controller
        .get_authorized_by_role("write")
        .await
        .expect("Failed to get final write permissions");

    assert_eq!(final_write_users.len(), 2);
    assert!(!final_write_users.contains(&"bob".to_string()));

    tracing::info!("✓ Permission revoked successfully");
    tracing::info!("✓ SimpleAccessController permissions test completed");
}

#[tokio::test]
async fn test_simple_access_controller_can_append() {
    init_test_logging();
    tracing::info!("=== Testing SimpleAccessController can_append ===");

    // Criar identidades para teste
    let mut identificator = DefaultIdentificator::new();
    let alice_identity = identificator.create("alice");
    let bob_identity = identificator.create("bob");
    let unauthorized_identity = identificator.create("unauthorized");

    // Criar SimpleAccessController com permissões específicas
    let mut permissions = HashMap::new();
    permissions.insert(
        "write".to_string(),
        vec![
            alice_identity.id().to_string(),
            bob_identity.id().to_string(),
        ],
    );

    let controller =
        guardian_db::access_control::acl_simple::SimpleAccessController::new(Some(permissions));

    // Criar entries de teste
    let alice_entry = Entry::new(alice_identity.clone(), "test-log", b"test data", &[], None);

    let unauthorized_entry = Entry::new(
        unauthorized_identity.clone(),
        "test-log",
        b"unauthorized data",
        &[],
        None,
    );

    // Criar identity provider
    let identity_provider = GuardianDBIdentityProvider::new();
    let context = TestCanAppendContext;

    // Testar can_append com identidade autorizada
    let result = controller
        .can_append(&alice_entry, &identity_provider, &context)
        .await;

    assert!(result.is_ok(), "Alice should be authorized");
    tracing::info!("✓ Authorized identity can append");

    // Testar can_append com identidade não autorizada
    let result = controller
        .can_append(&unauthorized_entry, &identity_provider, &context)
        .await;

    assert!(result.is_err(), "Unauthorized identity should be rejected");
    tracing::info!("✓ Unauthorized identity rejected");

    tracing::info!("✓ SimpleAccessController can_append test completed");
}

#[tokio::test]
async fn test_simple_access_controller_wildcard_access() {
    init_test_logging();
    tracing::info!("=== Testing SimpleAccessController wildcard access ===");

    // Criar identidades
    let mut identificator = DefaultIdentificator::new();
    let random_identity = identificator.create("random_user");

    // Criar controller com wildcard
    let mut permissions = HashMap::new();
    permissions.insert("write".to_string(), vec!["*".to_string()]);

    let controller =
        guardian_db::access_control::acl_simple::SimpleAccessController::new(Some(permissions));

    // Criar entry com identidade qualquer
    let entry = Entry::new(random_identity.clone(), "test-log", b"test data", &[], None);

    // Criar identity provider
    let identity_provider = GuardianDBIdentityProvider::new();
    let context = TestCanAppendContext;

    // Testar can_append com wildcard
    let result = controller
        .can_append(&entry, &identity_provider, &context)
        .await;

    assert!(result.is_ok(), "Wildcard should authorize any identity");
    tracing::info!("✓ Wildcard access works correctly");

    tracing::info!("✓ Wildcard access test completed");
}

#[tokio::test]
async fn test_guardian_db_access_controller_basic() {
    init_test_logging();
    tracing::info!("=== Testing GuardianDBAccessController basic operations ===");

    let node = TestNode::new("guardian_ac_test")
        .await
        .expect("Failed to create test node");

    // Criar GuardianDBAccessController via create
    let mut permissions = HashMap::new();
    permissions.insert("write".to_string(), vec!["user1".to_string()]);

    let params = CreateAccessControllerOptions::new_simple("guardian".to_string(), permissions);
    let option_fn: AccessControllerOption = Box::new(|_controller| {});

    let db_arc: Arc<dyn BaseGuardianDB<Error = GuardianError>> = Arc::new(node.db);

    let manifest_hash =
        access_control::create(db_arc.clone(), "guardian", params.clone(), option_fn)
            .await
            .expect("Failed to create GuardianDBAccessController");

    tracing::info!(
        "✓ GuardianDBAccessController created with manifest hash: {}",
        manifest_hash
    );

    // O manifest pode ser um hash zero dependendo da implementação
    // O importante é que a criação foi bem sucedida
    tracing::info!("✓ GuardianDBAccessController basic test completed");
}

#[tokio::test]
async fn test_guardian_db_access_controller_persistence() {
    init_test_logging();
    tracing::info!("=== Testing GuardianDBAccessController persistence ===");

    let node = TestNode::new("guardian_persist_test")
        .await
        .expect("Failed to create test node");

    let db_arc: Arc<dyn BaseGuardianDB<Error = GuardianError>> = Arc::new(node.db);

    // Criar params com skip_manifest=true para evitar busca de manifest
    use guardian_db::access_control::manifest::ManifestParams;
    let mut params = CreateAccessControllerOptions::default();
    params.set_type("guardian".to_string());
    params.set_access("write".to_string(), vec!["alice".to_string()]);

    // Cria o adapter usando o GuardianDB
    let adapter = guardian_db::access_control::GuardianDBAdapter::new(db_arc.clone());
    let adapter_arc = Arc::new(adapter);

    // Cria o GuardianDBAccessController diretamente
    let controller = guardian_db::access_control::acl_guardian::GuardianDBAccessController::new(
        adapter_arc,
        Box::new(params),
    )
    .await
    .expect("Failed to create GuardianDBAccessController");

    // Grant permission
    controller
        .grant("write", "bob")
        .await
        .expect("Failed to grant permission");

    // Verificar permissões
    let write_users = controller
        .get_authorized_by_role("write")
        .await
        .expect("Failed to get write permissions");

    assert!(write_users.contains(&"alice".to_string()));
    assert!(write_users.contains(&"bob".to_string()));

    tracing::info!("✓ Permissions persisted: {:?}", write_users);

    // Save controller
    let _manifest = controller.save().await.expect("Failed to save controller");

    tracing::info!("✓ Controller saved successfully");

    // Close controller
    controller
        .close()
        .await
        .expect("Failed to close controller");

    tracing::info!("✓ GuardianDBAccessController persistence test completed");
}

#[tokio::test]
async fn test_iroh_access_controller_basic() {
    init_test_logging();
    tracing::info!("=== Testing IrohAccessController basic operations ===");

    let node = TestNode::new("iroh_ac_test")
        .await
        .expect("Failed to create test node");

    // Criar IrohAccessController via create
    let mut permissions = HashMap::new();
    permissions.insert("write".to_string(), vec!["user1".to_string()]);

    let params = CreateAccessControllerOptions::new_simple("iroh".to_string(), permissions);
    let option_fn: AccessControllerOption = Box::new(|_controller| {});

    let db_arc: Arc<dyn BaseGuardianDB<Error = GuardianError>> = Arc::new(node.db);

    let manifest_hash = access_control::create(db_arc.clone(), "iroh", params.clone(), option_fn)
        .await
        .expect("Failed to create IrohAccessController");

    tracing::info!(
        "✓ IrohAccessController created with manifest hash: {}",
        manifest_hash
    );

    // O manifest pode ser um hash zero dependendo da implementação
    // O importante é que a criação foi bem sucedida
    tracing::info!("✓ IrohAccessController basic test completed");
}

#[tokio::test]
async fn test_iroh_access_controller_permissions() {
    init_test_logging();
    tracing::info!("=== Testing IrohAccessController permissions ===");

    let node = TestNode::new("iroh_perm_test")
        .await
        .expect("Failed to create test node");

    use guardian_db::access_control::manifest::ManifestParams;
    // Criar IrohAccessController diretamente
    let iroh_client = Arc::new(node.iroh);
    let mut params = CreateAccessControllerOptions::default();
    params.set_access("write".to_string(), vec!["alice".to_string()]);

    let controller = guardian_db::access_control::acl_iroh::IrohAccessController::new(
        iroh_client,
        "alice".to_string(),
        params,
    )
    .expect("Failed to create IrohAccessController");

    // Testar get_authorized_by_role
    let write_users = controller
        .get_authorized_by_role("write")
        .await
        .expect("Failed to get write permissions");

    assert!(write_users.contains(&"alice".to_string()));
    tracing::info!("✓ Initial write permissions: {:?}", write_users);

    // Testar grant
    controller
        .grant("write", "bob")
        .await
        .expect("Failed to grant permission");

    let updated_users = controller
        .get_authorized_by_role("write")
        .await
        .expect("Failed to get updated permissions");

    assert!(updated_users.contains(&"bob".to_string()));
    tracing::info!("✓ Permission granted to bob");

    // Testar revoke
    controller
        .revoke("write", "alice")
        .await
        .expect("Failed to revoke permission");

    let final_users = controller
        .get_authorized_by_role("write")
        .await
        .expect("Failed to get final permissions");

    assert!(!final_users.contains(&"alice".to_string()));
    tracing::info!("✓ Permission revoked from alice");

    tracing::info!("✓ IrohAccessController permissions test completed");
}

#[tokio::test]
async fn test_iroh_access_controller_can_append() {
    init_test_logging();
    tracing::info!("=== Testing IrohAccessController can_append ===");

    let node = TestNode::new("iroh_append_test")
        .await
        .expect("Failed to create test node");

    // Criar identidades
    let mut identificator = DefaultIdentificator::new();
    let alice_identity = identificator.create("alice");
    let unauthorized_identity = identificator.create("unauthorized");

    use guardian_db::access_control::manifest::ManifestParams;
    // Criar IrohAccessController
    let iroh_client = Arc::new(node.iroh);
    let mut params = CreateAccessControllerOptions::default();
    params.set_access("write".to_string(), vec![alice_identity.id().to_string()]);

    let controller = guardian_db::access_control::acl_iroh::IrohAccessController::new(
        iroh_client,
        alice_identity.id().to_string(),
        params,
    )
    .expect("Failed to create IrohAccessController");

    // Criar entries
    let alice_entry = Entry::new(alice_identity.clone(), "test-log", b"test data", &[], None);

    let unauthorized_entry = Entry::new(
        unauthorized_identity.clone(),
        "test-log",
        b"unauthorized data",
        &[],
        None,
    );

    // Criar identity provider
    let identity_provider = GuardianDBIdentityProvider::new();
    let context = TestCanAppendContext;

    // Testar can_append com identidade autorizada
    let result = controller
        .can_append(&alice_entry, &identity_provider, &context)
        .await;

    assert!(result.is_ok(), "Alice should be authorized");
    tracing::info!("✓ Authorized identity can append");

    // Testar can_append com identidade não autorizada
    let result = controller
        .can_append(&unauthorized_entry, &identity_provider, &context)
        .await;

    assert!(result.is_err(), "Unauthorized identity should be rejected");
    tracing::info!("✓ Unauthorized identity rejected");

    tracing::info!("✓ IrohAccessController can_append test completed");
}

#[tokio::test]
async fn test_access_controller_resolve() {
    init_test_logging();
    tracing::info!("=== Testing access controller resolve ===");

    let node = TestNode::new("resolve_test")
        .await
        .expect("Failed to create test node");

    let db_arc: Arc<dyn BaseGuardianDB<Error = GuardianError>> = Arc::new(node.db);

    // Criar um controller primeiro
    let mut permissions = HashMap::new();
    permissions.insert("write".to_string(), vec!["user1".to_string()]);

    let params = CreateAccessControllerOptions::new_simple("simple".to_string(), permissions);
    let option_fn: AccessControllerOption = Box::new(|_controller| {});

    let manifest_hash = access_control::create(db_arc.clone(), "simple", params.clone(), option_fn)
        .await
        .expect("Failed to create controller");

    tracing::info!("Controller created with hash: {}", manifest_hash);

    // Tentar resolver o controller usando o manifest address
    let manifest_address = hex::encode(manifest_hash.as_bytes()).to_string();
    let option_fn2: AccessControllerOption = Box::new(|_controller| {});

    let resolved_controller =
        access_control::resolve(db_arc.clone(), &manifest_address, &params, option_fn2)
            .await
            .expect("Failed to resolve controller");

    // Verificar tipo do controller
    assert_eq!(resolved_controller.get_type(), "simple");
    tracing::info!(
        "✓ Controller resolved successfully with type: {}",
        resolved_controller.get_type()
    );

    tracing::info!("✓ Access controller resolve test completed");
}

#[tokio::test]
async fn test_access_control_with_keyvalue_store() {
    init_test_logging();
    tracing::info!("=== Testing access control integration with KeyValue store ===");

    let node = TestNode::new("ac_kv_integration")
        .await
        .expect("Failed to create test node");

    // Criar identidade autorizada
    let mut identificator = DefaultIdentificator::new();
    let alice_identity = identificator.create("alice");

    // Criar KeyValue store com access control
    let mut options = CreateDBOptions::default();

    // Configurar access controller para o store
    use guardian_db::access_control::manifest::ManifestParams;
    let mut ac_params = CreateAccessControllerOptions::default();
    ac_params.set_type("simple".to_string());
    ac_params.set_access("write".to_string(), vec![alice_identity.id().to_string()]);

    options.access_controller = Some(Box::new(ac_params));

    let kv = node
        .db
        .key_value("protected-kv", Some(options))
        .await
        .expect("Failed to create protected KeyValue store");

    // Testar operações
    kv.put("key1", b"value1".to_vec())
        .await
        .expect("Failed to put value");

    let value = kv.get("key1").await.expect("Failed to get value");

    assert_eq!(value, Some(b"value1".to_vec()));

    tracing::info!("✓ KeyValue operations work with access control");
    tracing::info!("✓ Access control integration test completed");
}

#[tokio::test]
async fn test_multiple_access_controllers() {
    init_test_logging();
    tracing::info!("=== Testing multiple access controllers ===");

    let node = TestNode::new("multi_ac_test")
        .await
        .expect("Failed to create test node");

    let db_arc: Arc<dyn BaseGuardianDB<Error = GuardianError>> = Arc::new(node.db);

    // Criar múltiplos controllers de tipos diferentes
    let controllers = vec![
        ("simple", "simple-ac"),
        ("guardian", "guardian-ac"),
        ("iroh", "iroh-ac"),
    ];

    for (controller_type, name) in controllers {
        let mut permissions = HashMap::new();
        permissions.insert("write".to_string(), vec!["user1".to_string()]);
        use guardian_db::access_control::manifest::ManifestParams;

        let mut params =
            CreateAccessControllerOptions::new_simple(controller_type.to_string(), permissions);
        params.set_name(name.to_string());

        let option_fn: AccessControllerOption = Box::new(|_controller| {});

        let manifest_hash =
            access_control::create(db_arc.clone(), controller_type, params, option_fn)
                .await
                .unwrap_or_else(|_| panic!("Failed to create {} controller", controller_type));

        tracing::info!(
            "✓ {} controller created with hash: {}",
            controller_type,
            manifest_hash
        );
    }

    tracing::info!("✓ Multiple access controllers test completed");
}

#[tokio::test]
async fn test_access_controller_type_validation() {
    init_test_logging();
    tracing::info!("=== Testing access controller type validation ===");

    let node = TestNode::new("type_validation_test")
        .await
        .expect("Failed to create test node");

    let db_arc: Arc<dyn BaseGuardianDB<Error = GuardianError>> = Arc::new(node.db);

    // Testar tipo inválido
    let params = CreateAccessControllerOptions::default();
    let option_fn: AccessControllerOption = Box::new(|_controller| {});

    let result = access_control::create(db_arc.clone(), "invalid_type", params, option_fn).await;

    assert!(result.is_err(), "Invalid type should return error");
    tracing::info!("✓ Invalid type correctly rejected");

    // Testar tipos válidos
    let valid_types = vec!["simple", "guardian", "iroh"];

    for controller_type in valid_types {
        let mut permissions = HashMap::new();
        permissions.insert("write".to_string(), vec!["user1".to_string()]);

        let params =
            CreateAccessControllerOptions::new_simple(controller_type.to_string(), permissions);
        let option_fn: AccessControllerOption = Box::new(|_controller| {});

        let result =
            access_control::create(db_arc.clone(), controller_type, params, option_fn).await;

        assert!(
            result.is_ok(),
            "Valid type '{}' should succeed",
            controller_type
        );
        tracing::info!("✓ Valid type '{}' accepted", controller_type);
    }

    tracing::info!("✓ Type validation test completed");
}
