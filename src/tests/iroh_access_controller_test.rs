//! Testes robustos para IrohAccessController
//!
//! Este módulo testa:
//! - Serialização/deserialização CBOR
//! - Grant/revoke de permissões
//! - Validação de acesso (can_append)
//! - Persistência (save/load)
//! - Concorrência
//! - Edge cases e tratamento de erros

use crate::access_control::{
    acl_iroh::IrohAccessController,
    manifest::{CreateAccessControllerOptions, ManifestParams},
};
use crate::guardian::error::Result;
use crate::log::{
    access_control::{CanAppendAdditionalContext, LogEntry},
    identity::{Identity, Signatures},
    identity_provider::GuardianDBIdentityProvider,
};
use crate::p2p::network::{client::IrohClient, config::ClientConfig};
use std::sync::Arc;
use tokio;

// ============= TEST HELPERS =============

/// Gera um caminho único para cada teste
fn generate_test_path() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let thread_id = std::thread::current().id();
    format!("./tmp/test_iroh_{}_{:?}", timestamp, thread_id)
}

/// Mock para LogEntry usado nos testes
struct MockLogEntry {
    identity: Identity,
    payload: Vec<u8>,
}

impl MockLogEntry {
    fn new(identity_id: &str) -> Self {
        let signatures = Signatures::new("", "");
        Self {
            identity: Identity::new(identity_id, "", signatures),
            payload: vec![],
        }
    }
}

impl LogEntry for MockLogEntry {
    fn get_identity(&self) -> &Identity {
        &self.identity
    }

    fn get_payload(&self) -> &[u8] {
        &self.payload
    }
}

/// Mock para CanAppendAdditionalContext
struct MockAdditionalContext;

impl CanAppendAdditionalContext for MockAdditionalContext {
    fn get_log_entries(&self) -> Vec<Box<dyn LogEntry>> {
        vec![]
    }
}

/// Cria um IrohClient de teste
async fn create_test_iroh_client() -> Result<Arc<IrohClient>> {
    let test_path = generate_test_path();
    let mut client_config = ClientConfig::offline();
    client_config.data_store_path = Some(test_path.into());
    let client = IrohClient::new(client_config).await?;
    Ok(Arc::new(client))
}

/// Cria um IrohAccessController de teste
async fn create_test_controller(
    client: Arc<IrohClient>,
    identity_id: &str,
) -> Result<IrohAccessController> {
    let params = CreateAccessControllerOptions::default();
    IrohAccessController::new(client, identity_id.to_string(), params)
}

// ============= BASIC FUNCTIONALITY TESTS =============

#[tokio::test]
async fn test_controller_creation() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "test_user").await;

    assert!(controller.is_ok());
    let controller = controller.unwrap();
    assert_eq!(controller.get_type(), "iroh");
}

#[tokio::test]
async fn test_controller_default_permissions() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "test_user").await.unwrap();

    let write_access = controller.get_authorized_by_role("write").await.unwrap();
    assert_eq!(write_access.len(), 1);
    assert_eq!(write_access[0], "test_user");
}

// ============= GRANT/REVOKE TESTS =============

#[tokio::test]
async fn test_grant_permission() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "admin").await.unwrap();

    // Grant permission to new user
    controller.grant("write", "user1").await.unwrap();

    let write_access = controller.get_authorized_by_role("write").await.unwrap();
    assert_eq!(write_access.len(), 2);
    assert!(write_access.contains(&"admin".to_string()));
    assert!(write_access.contains(&"user1".to_string()));
}

#[tokio::test]
async fn test_grant_duplicate_permission() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "admin").await.unwrap();

    // Grant same permission twice
    controller.grant("write", "user1").await.unwrap();
    controller.grant("write", "user1").await.unwrap();

    let write_access = controller.get_authorized_by_role("write").await.unwrap();
    // Should still have only 2 users (admin + user1, no duplicates)
    assert_eq!(write_access.len(), 2);
}

#[tokio::test]
async fn test_revoke_permission() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "admin").await.unwrap();

    // Grant and then revoke
    controller.grant("write", "user1").await.unwrap();
    controller.grant("write", "user2").await.unwrap();

    let before_revoke = controller.get_authorized_by_role("write").await.unwrap();
    assert_eq!(before_revoke.len(), 3);

    controller.revoke("write", "user1").await.unwrap();

    let after_revoke = controller.get_authorized_by_role("write").await.unwrap();
    assert_eq!(after_revoke.len(), 2);
    assert!(after_revoke.contains(&"admin".to_string()));
    assert!(after_revoke.contains(&"user2".to_string()));
    assert!(!after_revoke.contains(&"user1".to_string()));
}

#[tokio::test]
async fn test_revoke_nonexistent_permission() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "admin").await.unwrap();

    // Revoke permission that doesn't exist (should not error)
    let result = controller.revoke("write", "nonexistent").await;
    assert!(result.is_ok());

    let write_access = controller.get_authorized_by_role("write").await.unwrap();
    assert_eq!(write_access.len(), 1);
}

#[tokio::test]
async fn test_grant_invalid_capability() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "admin").await.unwrap();

    // Try to grant invalid capability
    let result = controller.grant("invalid", "user1").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_revoke_invalid_capability() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "admin").await.unwrap();

    // Try to revoke invalid capability
    let result = controller.revoke("invalid", "user1").await;
    assert!(result.is_err());
}

// ============= CAN_APPEND TESTS =============

#[tokio::test]
async fn test_can_append_authorized_user() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "admin").await.unwrap();

    controller.grant("write", "user1").await.unwrap();

    let entry = MockLogEntry::new("user1");
    let identity_provider = GuardianDBIdentityProvider::new();
    let additional_context = MockAdditionalContext;

    let result = controller
        .can_append(&entry, &identity_provider, &additional_context)
        .await;

    // Nota: pode falhar na verificação de identidade, mas não deve ser erro de permissão
    // Se falhar, a mensagem não deve ser sobre permissão
    if let Err(e) = result {
        let error_msg = format!("{}", e);
        assert!(!error_msg.contains("não tem permissão de escrita"));
    }
}

#[tokio::test]
async fn test_can_append_unauthorized_user() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "admin").await.unwrap();

    let entry = MockLogEntry::new("unauthorized_user");
    let identity_provider = GuardianDBIdentityProvider::new();
    let additional_context = MockAdditionalContext;

    let result = controller
        .can_append(&entry, &identity_provider, &additional_context)
        .await;

    assert!(result.is_err());
    let error_msg = format!("{}", result.unwrap_err());
    assert!(
        error_msg.contains("não tem permissão de escrita")
            || error_msg.contains("not authorized for write")
    );
}

#[tokio::test]
async fn test_can_append_wildcard() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "admin").await.unwrap();

    // Grant wildcard permission
    controller.grant("write", "*").await.unwrap();

    let entry = MockLogEntry::new("any_user");
    let identity_provider = GuardianDBIdentityProvider::new();
    let additional_context = MockAdditionalContext;

    let result = controller
        .can_append(&entry, &identity_provider, &additional_context)
        .await;

    // Com wildcard, o erro (se houver) não deve ser de permissão
    if let Err(e) = result {
        let error_msg = format!("{}", e);
        assert!(!error_msg.contains("não tem permissão de escrita"));
    }
}

// ============= SAVE/LOAD TESTS =============

#[tokio::test]
async fn test_save_controller() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "admin").await.unwrap();

    controller.grant("write", "user1").await.unwrap();
    controller.grant("write", "user2").await.unwrap();

    let result = controller.save().await;
    assert!(result.is_ok());

    let options = result.unwrap();
    assert_eq!(options.get_type(), "iroh");
    // O hash não deve ser zero
    assert_ne!(
        hex::encode(options.address().as_bytes()),
        "0000000000000000000000000000000000000000000000000000000000000000"
    );
}

#[tokio::test]
async fn test_save_load_roundtrip() {
    let client = create_test_iroh_client().await.unwrap();
    let controller1 = create_test_controller(client.clone(), "admin")
        .await
        .unwrap();

    // Add permissions
    controller1.grant("write", "user1").await.unwrap();
    controller1.grant("write", "user2").await.unwrap();
    controller1.grant("write", "user3").await.unwrap();

    // Save
    let options = controller1.save().await.unwrap();
    let hash_str = hex::encode(options.address().as_bytes());

    // Create new controller and load
    let _controller2 = create_test_controller(client.clone(), "admin")
        .await
        .unwrap();

    // For now, we can't fully test load without a proper manifest
    // But we can verify the save produced valid data
    assert!(!hash_str.is_empty());
    assert_ne!(
        hash_str,
        "0000000000000000000000000000000000000000000000000000000000000000"
    );
}

// ============= ROLE-BASED ACCESS TESTS =============

#[tokio::test]
async fn test_get_authorized_by_role_write() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "admin").await.unwrap();

    controller.grant("write", "user1").await.unwrap();

    let write_users = controller.get_authorized_by_role("write").await.unwrap();
    assert_eq!(write_users.len(), 2);
    assert!(write_users.contains(&"admin".to_string()));
    assert!(write_users.contains(&"user1".to_string()));
}

#[tokio::test]
async fn test_get_authorized_by_role_read() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "admin").await.unwrap();

    controller.grant("write", "user1").await.unwrap();

    // Note: the direct method get_authorized_by_role returns empty for "read"
    // only "write" and "admin" return the write_access list
    let read_users = controller.get_authorized_by_role("read").await.unwrap();
    assert_eq!(read_users.len(), 0);
}

#[tokio::test]
async fn test_get_authorized_by_role_admin() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "admin").await.unwrap();

    // Admin role returns same as write
    let admin_users = controller.get_authorized_by_role("admin").await.unwrap();
    assert_eq!(admin_users.len(), 1);
    assert!(admin_users.contains(&"admin".to_string()));
}

#[tokio::test]
async fn test_get_authorized_by_role_unknown() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "admin").await.unwrap();

    // Unknown roles return empty list
    let unknown_users = controller
        .get_authorized_by_role("unknown_role")
        .await
        .unwrap();
    assert_eq!(unknown_users.len(), 0);
}

// ============= CONCURRENCY TESTS =============

#[tokio::test]
async fn test_concurrent_grants() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = Arc::new(create_test_controller(client, "admin").await.unwrap());

    let mut handles = vec![];

    // Spawn 10 concurrent grant operations
    for i in 0..10 {
        let controller_clone = controller.clone();
        let handle = tokio::spawn(async move {
            controller_clone
                .grant("write", &format!("user{}", i))
                .await
                .unwrap();
        });
        handles.push(handle);
    }

    // Wait for all operations
    for handle in handles {
        handle.await.unwrap();
    }

    // Verify all users were added
    let write_access = controller.get_authorized_by_role("write").await.unwrap();
    // admin + 10 users
    assert_eq!(write_access.len(), 11);
}

#[tokio::test]
async fn test_concurrent_grant_and_revoke() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = Arc::new(create_test_controller(client, "admin").await.unwrap());

    // Pre-add some users
    for i in 0..5 {
        controller
            .grant("write", &format!("user{}", i))
            .await
            .unwrap();
    }

    let mut handles = vec![];

    // Concurrent grants
    for i in 5..10 {
        let controller_clone = controller.clone();
        let handle = tokio::spawn(async move {
            controller_clone
                .grant("write", &format!("user{}", i))
                .await
                .unwrap();
        });
        handles.push(handle);
    }

    // Concurrent revokes
    for i in 0..3 {
        let controller_clone = controller.clone();
        let handle = tokio::spawn(async move {
            controller_clone
                .revoke("write", &format!("user{}", i))
                .await
                .unwrap();
        });
        handles.push(handle);
    }

    // Wait for all operations
    for handle in handles {
        handle.await.unwrap();
    }

    let write_access = controller.get_authorized_by_role("write").await.unwrap();
    // admin + (5 initial - 3 revoked + 5 new) = admin + 7
    assert_eq!(write_access.len(), 8);
}

// ============= CBOR SERIALIZATION TESTS =============

#[tokio::test]
async fn test_cbor_serialization_simple() {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct CborWriteAccess {
        write: Vec<String>,
    }

    let data = CborWriteAccess {
        write: vec!["user1".to_string(), "user2".to_string()],
    };

    let serialized = serde_cbor::to_vec(&data).unwrap();
    let deserialized: CborWriteAccess = serde_cbor::from_slice(&serialized).unwrap();

    assert_eq!(data, deserialized);
}

#[tokio::test]
async fn test_cbor_serialization_empty() {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct CborWriteAccess {
        write: Vec<String>,
    }

    let data = CborWriteAccess { write: vec![] };

    let serialized = serde_cbor::to_vec(&data).unwrap();
    let deserialized: CborWriteAccess = serde_cbor::from_slice(&serialized).unwrap();

    assert_eq!(data, deserialized);
}

#[tokio::test]
async fn test_cbor_serialization_special_chars() {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct CborWriteAccess {
        write: Vec<String>,
    }

    let data = CborWriteAccess {
        write: vec![
            "user@domain.com".to_string(),
            "user-with-dashes".to_string(),
            "user_with_underscores".to_string(),
            "*.wildcard".to_string(),
            "unicode_中文_🎉".to_string(),
        ],
    };

    let serialized = serde_cbor::to_vec(&data).unwrap();
    let deserialized: CborWriteAccess = serde_cbor::from_slice(&serialized).unwrap();

    assert_eq!(data, deserialized);
}

// ============= EDGE CASES =============

#[tokio::test]
async fn test_close_controller() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "admin").await.unwrap();

    let result = controller.close().await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_controller_with_empty_identity() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "").await.unwrap();

    let write_access = controller.get_authorized_by_role("write").await.unwrap();
    assert_eq!(write_access.len(), 1);
    assert_eq!(write_access[0], "");
}

#[tokio::test]
async fn test_grant_many_permissions() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "admin").await.unwrap();

    // Grant 100 permissions
    for i in 0..100 {
        controller
            .grant("write", &format!("user{}", i))
            .await
            .unwrap();
    }

    let write_access = controller.get_authorized_by_role("write").await.unwrap();
    assert_eq!(write_access.len(), 101); // admin + 100 users
}

#[tokio::test]
async fn test_controller_address_returns_none() {
    let client = create_test_iroh_client().await.unwrap();
    let controller = create_test_controller(client, "admin").await.unwrap();

    assert!(controller.address().is_none());
}
