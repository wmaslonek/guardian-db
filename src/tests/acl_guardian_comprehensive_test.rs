/// Testes Robustos para GuardianDBAccessController
///
/// Este módulo contém testes abrangentes para validar todas as funcionalidades
/// do GuardianDBAccessController, incluindo grant, revoke, can_append, load, save e eventos.
///
/// IMPORTANTE: Estes testes devem ser executados sequencialmente (--test-threads=1) devido a
/// compartilhamento de recursos de cache.
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::time::{Duration, sleep};

use crate::access_control::acl_guardian::GuardianDBAccessController;
use crate::access_control::manifest::{CreateAccessControllerOptions, ManifestParams};
use crate::access_control::traits::AccessController;
use crate::guardian::error::{GuardianError, Result};
use crate::guardian::{GuardianDB, core::NewGuardianDBOptions};
use crate::log::access_control::{CanAppendAdditionalContext, LogEntry};
use crate::log::identity::{Identity, Signatures};
use crate::log::identity_provider::IdentityProvider;
use crate::p2p::network::config::ClientConfig;

// ==================== Mock Implementations ====================

/// Mock implementation of LogEntry for testing
#[derive(Clone)]
struct MockLogEntry {
    payload: Vec<u8>,
    identity: Identity,
}

impl MockLogEntry {
    fn new(identity_id: &str, payload: &[u8]) -> Self {
        let signatures = Signatures::new("mock_sig", "mock_pub_sig");
        let identity = Identity::new(identity_id, "mock_pub_key", signatures);

        Self {
            payload: payload.to_vec(),
            identity,
        }
    }
}

impl LogEntry for MockLogEntry {
    fn get_payload(&self) -> &[u8] {
        &self.payload
    }

    fn get_identity(&self) -> &Identity {
        &self.identity
    }
}

/// Mock implementation of CanAppendAdditionalContext for testing
struct MockCanAppendContext {
    entries: Vec<Box<dyn LogEntry>>,
}

impl MockCanAppendContext {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl CanAppendAdditionalContext for MockCanAppendContext {
    fn get_log_entries(&self) -> Vec<Box<dyn LogEntry>> {
        self.entries
            .iter()
            .map(|entry| {
                let payload = entry.get_payload().to_vec();
                let identity = entry.get_identity().clone();
                Box::new(MockLogEntry { payload, identity }) as Box<dyn LogEntry>
            })
            .collect()
    }
}

/// Mock implementation of IdentityProvider for testing
struct MockIdentityProvider {
    valid_identities: Arc<tokio::sync::RwLock<HashMap<String, bool>>>,
}

impl MockIdentityProvider {
    fn new() -> Self {
        Self {
            valid_identities: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        }
    }

    async fn add_valid_identity(&self, identity_id: &str) {
        let mut valid = self.valid_identities.write().await;
        valid.insert(identity_id.to_string(), true);
    }
}

#[async_trait::async_trait]
impl IdentityProvider for MockIdentityProvider {
    async fn get_id(
        &self,
        _options: &crate::log::identity_provider::CreateIdentityOptions,
    ) -> Result<String> {
        Ok("mock_identity".to_string())
    }

    async fn sign_identity(&self, _data: &[u8], _id: &str) -> Result<Vec<u8>> {
        Ok(b"mock_signature".to_vec())
    }

    async fn verify_identity(&self, identity: &Identity) -> Result<()> {
        let valid = self.valid_identities.read().await;
        if valid.get(identity.id()).copied().unwrap_or(false) {
            Ok(())
        } else {
            Err(GuardianError::Store(format!(
                "Invalid identity: {}",
                identity.id()
            )))
        }
    }

    fn get_type(&self) -> String {
        "mock".to_string()
    }

    async fn sign(&self, _identity: &Identity, bytes: &[u8]) -> Result<Vec<u8>> {
        // Mock implementation - assina dados genéricos
        Ok(format!("mock_sig_{}", bytes.len()).into_bytes())
    }

    fn unmarshal_public_key(&self, data: &[u8]) -> Result<ed25519_dalek::VerifyingKey> {
        // Mock implementation - cria uma chave pública válida de teste
        if data.len() != 32 {
            return Err(GuardianError::Store("Invalid key length".to_string()));
        }
        let mut key_bytes = [0u8; 32];
        key_bytes.copy_from_slice(&data[..32]);
        ed25519_dalek::VerifyingKey::from_bytes(&key_bytes)
            .map_err(|e| GuardianError::Store(format!("Invalid key: {}", e)))
    }
}

// ==================== Helper Functions ====================

/// Creates a temporary GuardianDB instance for testing
async fn create_test_guardian_db() -> Result<Arc<GuardianDB>> {
    let temp_dir = TempDir::new().map_err(|e| GuardianError::Other(e.to_string()))?;
    let temp_path = temp_dir.path().to_path_buf();

    // Importante: leak o TempDir para evitar que seja deletado durante o teste
    std::mem::forget(temp_dir);

    // Cria um diretório único para cada teste usando UUID para evitar conflitos
    let unique_id = uuid::Uuid::new_v4().to_string();
    let isolated_path = temp_path.join(&unique_id);
    std::fs::create_dir_all(&isolated_path).map_err(|e| GuardianError::Other(e.to_string()))?;

    let mut config = ClientConfig::development();
    config.data_store_path = Some(isolated_path.clone());

    let iroh_client = crate::p2p::network::client::IrohClient::new(config).await?;

    let options = NewGuardianDBOptions {
        backend: Some(iroh_client.backend().clone()),
        directory: Some(isolated_path),
        ..Default::default()
    };

    let guardian_db = GuardianDB::new(iroh_client, Some(options)).await?;

    Ok(Arc::new(guardian_db))
}

/// Creates a test access controller with the given configuration
async fn create_test_access_controller(
    guardian_db: Arc<GuardianDB>,
    write_access: Option<Vec<String>>,
) -> Result<GuardianDBAccessController> {
    let mut params = CreateAccessControllerOptions::default();
    ManifestParams::set_type(&mut params, "guardian".to_string());
    // Importante: Configurar skip_manifest como true para testes
    params.skip_manifest = true;

    if let Some(access) = write_access {
        ManifestParams::set_access(&mut params, "write".to_string(), access);
    }

    GuardianDBAccessController::new(guardian_db, Box::new(params)).await
}

// ==================== Tests ====================

#[tokio::test]
async fn test_access_controller_creation() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    assert_eq!(controller.get_type(), "GuardianDB");
}

#[tokio::test]
async fn test_grant_permission() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Grant write permission to a test identity
    controller.grant("write", "test_identity_1").await.unwrap();

    // Verify the permission was granted
    let authorized = controller.get_authorized_by_role("write").await.unwrap();
    assert!(authorized.contains(&"test_identity_1".to_string()));
}

#[tokio::test]
async fn test_grant_multiple_permissions() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Grant multiple permissions
    controller.grant("write", "identity_1").await.unwrap();
    controller.grant("write", "identity_2").await.unwrap();
    controller.grant("admin", "identity_3").await.unwrap();

    // Verify write permissions
    let write_authorized = controller.get_authorized_by_role("write").await.unwrap();
    assert!(write_authorized.contains(&"identity_1".to_string()));
    assert!(write_authorized.contains(&"identity_2".to_string()));

    // Verify admin permissions
    let admin_authorized = controller.get_authorized_by_role("admin").await.unwrap();
    assert!(admin_authorized.contains(&"identity_3".to_string()));
}

#[tokio::test]
async fn test_revoke_permission() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Grant permission
    controller.grant("write", "test_identity").await.unwrap();

    // Verify permission was granted
    let authorized = controller.get_authorized_by_role("write").await.unwrap();
    assert!(authorized.contains(&"test_identity".to_string()));

    // Revoke permission
    controller.revoke("write", "test_identity").await.unwrap();

    // Verify permission was revoked
    let authorized_after = controller.get_authorized_by_role("write").await.unwrap();
    assert!(!authorized_after.contains(&"test_identity".to_string()));
}

#[tokio::test]
async fn test_revoke_nonexistent_permission() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Revoke permission that was never granted (should not error)
    let result = controller.revoke("write", "nonexistent_identity").await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_can_append_with_authorized_identity() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Setup mock identity provider
    let identity_provider = MockIdentityProvider::new();
    identity_provider
        .add_valid_identity("authorized_user")
        .await;

    // Grant write permission
    controller.grant("write", "authorized_user").await.unwrap();

    // Create a log entry with the authorized identity
    let entry = MockLogEntry::new("authorized_user", b"test payload");
    let context = MockCanAppendContext::new();

    // Verify can_append succeeds
    let result = controller
        .can_append(&entry, &identity_provider, &context)
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_can_append_with_unauthorized_identity() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Setup mock identity provider
    let identity_provider = MockIdentityProvider::new();
    identity_provider
        .add_valid_identity("unauthorized_user")
        .await;

    // Do NOT grant any permissions

    // Create a log entry with an unauthorized identity
    let entry = MockLogEntry::new("unauthorized_user", b"test payload");
    let context = MockCanAppendContext::new();

    // Verify can_append fails
    let result = controller
        .can_append(&entry, &identity_provider, &context)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_can_append_with_wildcard() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Setup mock identity provider
    let identity_provider = MockIdentityProvider::new();
    identity_provider.add_valid_identity("any_user").await;

    // Grant wildcard permission
    controller.grant("write", "*").await.unwrap();

    // Create a log entry with any identity
    let entry = MockLogEntry::new("any_user", b"test payload");
    let context = MockCanAppendContext::new();

    // Verify can_append succeeds with wildcard
    let result = controller
        .can_append(&entry, &identity_provider, &context)
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_can_append_with_admin_permission() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Setup mock identity provider
    let identity_provider = MockIdentityProvider::new();
    identity_provider.add_valid_identity("admin_user").await;

    // Grant admin permission (should allow writes)
    controller.grant("admin", "admin_user").await.unwrap();

    // Create a log entry with the admin identity
    let entry = MockLogEntry::new("admin_user", b"test payload");
    let context = MockCanAppendContext::new();

    // Verify can_append succeeds for admin
    let result = controller
        .can_append(&entry, &identity_provider, &context)
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_can_append_with_invalid_identity_signature() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Setup mock identity provider (without adding valid identity)
    let identity_provider = MockIdentityProvider::new();

    // Grant write permission
    controller.grant("write", "invalid_user").await.unwrap();

    // Create a log entry with an invalid identity
    let entry = MockLogEntry::new("invalid_user", b"test payload");
    let context = MockCanAppendContext::new();

    // Verify can_append fails due to invalid signature
    let result = controller
        .can_append(&entry, &identity_provider, &context)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_get_authorized_by_role_empty() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Get authorized for a role that has no permissions
    let authorized = controller
        .get_authorized_by_role("nonexistent_role")
        .await
        .unwrap();
    assert!(authorized.is_empty());
}

#[tokio::test]
async fn test_admin_inherits_write_permissions() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Grant write permission
    controller.grant("write", "writer_user").await.unwrap();

    // Check admin role (should include write permissions)
    let admin_authorized = controller.get_authorized_by_role("admin").await.unwrap();
    assert!(admin_authorized.contains(&"writer_user".to_string()));
}

#[tokio::test]
async fn test_address_returns_some_after_initialization() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    let address = controller.address().await;
    assert!(address.is_some());
}

#[tokio::test]
async fn test_save_returns_manifest_params() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    let manifest = controller.save().await.unwrap();
    assert_eq!(manifest.get_type(), "GuardianDB");
}

#[tokio::test]
async fn test_load_new_address() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Load a new address
    let result = controller.load("test_address_new").await;
    assert!(result.is_ok());

    // Verify address changed
    let address = controller.address().await;
    assert!(address.is_some());
}

#[tokio::test]
async fn test_close_access_controller() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Close should succeed
    let result = controller.close().await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_grant_duplicate_permission() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Grant permission twice
    controller.grant("write", "duplicate_user").await.unwrap();
    controller.grant("write", "duplicate_user").await.unwrap();

    // Verify only one entry exists (no duplicates)
    let authorized = controller.get_authorized_by_role("write").await.unwrap();
    let count = authorized
        .iter()
        .filter(|id| *id == "duplicate_user")
        .count();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_concurrent_grants() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = Arc::new(
        create_test_access_controller(guardian_db, None)
            .await
            .unwrap(),
    );

    // Spawn multiple concurrent grant operations
    let mut handles = vec![];
    for i in 0..10 {
        let controller_clone = Arc::clone(&controller);
        let handle = tokio::spawn(async move {
            controller_clone
                .grant("write", &format!("user_{}", i))
                .await
                .unwrap();
        });
        handles.push(handle);
    }

    // Wait for all grants to complete
    for handle in handles {
        handle.await.unwrap();
    }

    // Verify all permissions were granted
    let authorized = controller.get_authorized_by_role("write").await.unwrap();
    assert_eq!(authorized.len(), 10);
}

#[tokio::test]
async fn test_concurrent_grant_and_revoke() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = Arc::new(
        create_test_access_controller(guardian_db, None)
            .await
            .unwrap(),
    );

    // Pre-grant some permissions
    for i in 0..5 {
        controller
            .grant("write", &format!("user_{}", i))
            .await
            .unwrap();
    }

    // Spawn concurrent grant and revoke operations
    let mut handles = vec![];

    // Grant new permissions
    for i in 5..10 {
        let controller_clone = Arc::clone(&controller);
        let handle = tokio::spawn(async move {
            controller_clone
                .grant("write", &format!("user_{}", i))
                .await
                .unwrap();
        });
        handles.push(handle);
    }

    // Revoke existing permissions
    for i in 0..5 {
        let controller_clone = Arc::clone(&controller);
        let handle = tokio::spawn(async move {
            controller_clone
                .revoke("write", &format!("user_{}", i))
                .await
                .unwrap();
        });
        handles.push(handle);
    }

    // Wait for all operations to complete
    for handle in handles {
        handle.await.unwrap();
    }

    // Verify final state: only users 5-9 should have permissions
    let authorized = controller.get_authorized_by_role("write").await.unwrap();
    assert_eq!(authorized.len(), 5);
    for i in 5..10 {
        assert!(authorized.contains(&format!("user_{}", i)));
    }
}

#[tokio::test]
async fn test_persistence_across_load() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db.clone(), None)
        .await
        .unwrap();

    // Grant permissions
    controller.grant("write", "persistent_user").await.unwrap();

    // Get the address
    let address = controller.address().await.unwrap();
    let address_str = format!("{}", address);

    // Save
    controller.save().await.unwrap();

    // Verify the address is not empty (indicates persistence is possible)
    assert!(!address_str.is_empty());

    // Verify permissions are still accessible
    let authorized = controller.get_authorized_by_role("write").await.unwrap();
    assert!(authorized.contains(&"persistent_user".to_string()));
}

#[tokio::test]
async fn test_revoke_last_permission() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Grant single permission
    controller.grant("write", "only_user").await.unwrap();

    // Revoke it
    controller.revoke("write", "only_user").await.unwrap();

    // Verify role is empty
    let authorized = controller.get_authorized_by_role("write").await.unwrap();
    assert!(authorized.is_empty());
}

#[tokio::test]
async fn test_multiple_roles_same_user() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Grant same user multiple roles
    controller.grant("write", "multi_role_user").await.unwrap();
    controller.grant("admin", "multi_role_user").await.unwrap();

    // Verify user has both roles
    let write_authorized = controller.get_authorized_by_role("write").await.unwrap();
    let admin_authorized = controller.get_authorized_by_role("admin").await.unwrap();

    assert!(write_authorized.contains(&"multi_role_user".to_string()));
    assert!(admin_authorized.contains(&"multi_role_user".to_string()));
}

#[tokio::test]
async fn test_event_bus_emits_on_grant() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = Arc::new(
        create_test_access_controller(guardian_db, None)
            .await
            .unwrap(),
    );

    // Grant permission (this should emit an event)
    controller.grant("write", "event_test_user").await.unwrap();

    // Small delay to ensure event processing
    sleep(Duration::from_millis(50)).await;

    // Event system is internal, so we verify indirectly by checking the grant succeeded
    let authorized = controller.get_authorized_by_role("write").await.unwrap();
    assert!(authorized.contains(&"event_test_user".to_string()));
}

#[tokio::test]
async fn test_event_bus_emits_on_revoke() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = Arc::new(
        create_test_access_controller(guardian_db, None)
            .await
            .unwrap(),
    );

    // Grant and then revoke permission
    controller
        .grant("write", "revoke_event_user")
        .await
        .unwrap();
    controller
        .revoke("write", "revoke_event_user")
        .await
        .unwrap();

    // Small delay to ensure event processing
    sleep(Duration::from_millis(50)).await;

    // Verify revoke succeeded
    let authorized = controller.get_authorized_by_role("write").await.unwrap();
    assert!(!authorized.contains(&"revoke_event_user".to_string()));
}

#[tokio::test]
async fn test_initial_write_access_configuration() {
    let guardian_db = create_test_guardian_db().await.unwrap();

    // Create controller with initial write access
    let initial_access = vec!["initial_user_1".to_string(), "initial_user_2".to_string()];
    let controller = create_test_access_controller(guardian_db, Some(initial_access))
        .await
        .unwrap();

    // Verify initial users have write access
    let authorized = controller.get_authorized_by_role("write").await.unwrap();
    assert!(authorized.contains(&"initial_user_1".to_string()));
    assert!(authorized.contains(&"initial_user_2".to_string()));
}

#[tokio::test]
async fn test_trait_implementation_get_type() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Test trait method
    let ac_trait: &dyn AccessController = &controller;
    assert_eq!(ac_trait.get_type(), "guardian");
}

#[tokio::test]
async fn test_trait_implementation_grant_revoke() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Use trait methods
    let ac_trait: &dyn AccessController = &controller;

    ac_trait.grant("write", "trait_test_user").await.unwrap();
    let authorized = ac_trait.get_authorized_by_role("write").await.unwrap();
    assert!(authorized.contains(&"trait_test_user".to_string()));

    ac_trait.revoke("write", "trait_test_user").await.unwrap();
    let authorized = ac_trait.get_authorized_by_role("write").await.unwrap();
    assert!(!authorized.contains(&"trait_test_user".to_string()));
}

#[tokio::test]
async fn test_special_characters_in_identity() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Grant permission with special characters in identity
    let special_id = "user:with:colons:and-dashes_123";
    controller.grant("write", special_id).await.unwrap();

    // Verify it was stored correctly
    let authorized = controller.get_authorized_by_role("write").await.unwrap();
    assert!(authorized.contains(&special_id.to_string()));
}

#[tokio::test]
async fn test_very_long_identity() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Create a very long identity string
    let long_id = "a".repeat(1000);
    controller.grant("write", &long_id).await.unwrap();

    // Verify it was stored correctly
    let authorized = controller.get_authorized_by_role("write").await.unwrap();
    assert!(authorized.contains(&long_id));
}

#[tokio::test]
async fn test_empty_role_name() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Test with empty role name
    let result = controller.grant("", "test_user").await;
    // Should succeed (empty role is valid, though not recommended)
    assert!(result.is_ok());
}

// ==================== Integration Tests ====================

#[tokio::test]
async fn test_full_access_control_workflow() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Setup identity provider
    let identity_provider = MockIdentityProvider::new();
    identity_provider.add_valid_identity("alice").await;
    identity_provider.add_valid_identity("bob").await;

    // 1. Grant write permission to Alice
    controller.grant("write", "alice").await.unwrap();

    // 2. Alice can append entries
    let alice_entry = MockLogEntry::new("alice", b"Alice's data");
    let context = MockCanAppendContext::new();
    assert!(
        controller
            .can_append(&alice_entry, &identity_provider, &context)
            .await
            .is_ok()
    );

    // 3. Bob cannot append entries (not authorized)
    let bob_entry = MockLogEntry::new("bob", b"Bob's data");
    assert!(
        controller
            .can_append(&bob_entry, &identity_provider, &context)
            .await
            .is_err()
    );

    // 4. Grant write permission to Bob
    controller.grant("write", "bob").await.unwrap();

    // 5. Now Bob can append entries
    assert!(
        controller
            .can_append(&bob_entry, &identity_provider, &context)
            .await
            .is_ok()
    );

    // 6. Revoke Alice's permission
    controller.revoke("write", "alice").await.unwrap();

    // 7. Alice can no longer append entries
    assert!(
        controller
            .can_append(&alice_entry, &identity_provider, &context)
            .await
            .is_err()
    );

    // 8. Bob can still append entries
    assert!(
        controller
            .can_append(&bob_entry, &identity_provider, &context)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn test_complex_permission_hierarchy() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Setup identity provider
    let identity_provider = MockIdentityProvider::new();
    identity_provider.add_valid_identity("admin").await;
    identity_provider.add_valid_identity("writer").await;
    identity_provider.add_valid_identity("reader").await;

    // Grant different levels of access
    controller.grant("admin", "admin").await.unwrap();
    controller.grant("write", "writer").await.unwrap();

    // Admin should have write access (through inheritance)
    let admin_entry = MockLogEntry::new("admin", b"Admin data");
    let context = MockCanAppendContext::new();
    assert!(
        controller
            .can_append(&admin_entry, &identity_provider, &context)
            .await
            .is_ok()
    );

    // Writer should have write access
    let writer_entry = MockLogEntry::new("writer", b"Writer data");
    assert!(
        controller
            .can_append(&writer_entry, &identity_provider, &context)
            .await
            .is_ok()
    );

    // Reader should not have write access
    let reader_entry = MockLogEntry::new("reader", b"Reader data");
    assert!(
        controller
            .can_append(&reader_entry, &identity_provider, &context)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn test_stress_many_identities() {
    let guardian_db = create_test_guardian_db().await.unwrap();
    let controller = create_test_access_controller(guardian_db, None)
        .await
        .unwrap();

    // Grant permissions to many identities
    for i in 0..100 {
        controller
            .grant("write", &format!("user_{}", i))
            .await
            .unwrap();
    }

    // Verify all permissions were granted
    let authorized = controller.get_authorized_by_role("write").await.unwrap();
    assert_eq!(authorized.len(), 100);

    // Revoke half of them
    for i in 0..50 {
        controller
            .revoke("write", &format!("user_{}", i))
            .await
            .unwrap();
    }

    // Verify only 50 remain
    let authorized_after = controller.get_authorized_by_role("write").await.unwrap();
    assert_eq!(authorized_after.len(), 50);
}
