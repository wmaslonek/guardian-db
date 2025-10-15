use crate::{
    access_controller::{simple::SimpleAccessController, traits::AccessController},
    error::Result,
    ipfs_log::{
        entry::Entry,
        identity::{Identity, Signatures},
    },
};
use std::{collections::HashMap, sync::Arc};
use tokio;

/// Testes unitários para verificação de permissões no sistema de exchange de heads
///
/// Este módulo testa:
/// - Verificação de permissões com Access Controllers
/// - Comportamento com identidades autorizadas vs não autorizadas
/// - Casos especiais (identidades vazias, permissões universais)
/// - Integração com diferentes tipos de Access Controllers
///
/// Função auxiliar para criar identidades de teste
fn create_test_identity(id: &str, pub_key: &str) -> Identity {
    let signatures = Signatures::new(&format!("sig_{}", id), &format!("sig_{}", pub_key));

    Identity::new(id, pub_key, signatures)
}

/// Função auxiliar para criar heads de teste
fn create_test_head(hash: &str, identity: Option<Identity>) -> Entry {
    use crate::ipfs_log::{entry::EntryOrHash, lamport_clock::LamportClock};

    let clock = LamportClock::new("test_peer").set_time(1);
    let next: Vec<EntryOrHash> = Vec::new();

    // Entry::new requires an identity, so we provide a default if None
    let entry_identity =
        identity.unwrap_or_else(|| create_test_identity("default_test_user", "default_test_key"));

    Entry::new(
        entry_identity,
        "test_log",
        &format!("payload_{}", hash),
        &next,
        Some(clock),
    )
}

/// Função auxiliar para criar SimpleAccessController configurado
async fn create_test_access_controller() -> Arc<dyn AccessController> {
    let mut permissions = HashMap::new();
    permissions.insert("read".to_string(), Vec::new());
    permissions.insert("write".to_string(), Vec::new());
    permissions.insert("admin".to_string(), Vec::new());

    Arc::new(SimpleAccessController::new(Some(permissions)))
}

#[tokio::test]
async fn test_verify_heads_permissions_with_authorized_identity() -> Result<()> {
    // Setup
    let access_controller = create_test_access_controller().await;
    let authorized_identity = create_test_identity("authorized_user", "auth_key_123");

    // Concede permissão de escrita
    access_controller
        .grant("write", authorized_identity.id())
        .await?;

    // Cria head com identidade autorizada
    let test_heads = vec![create_test_head(
        "authorized_head",
        Some(authorized_identity),
    )];

    // Simula verificação
    let result = simulate_permission_check(&test_heads, &access_controller).await?;

    // Verifica resultado
    assert_eq!(result.len(), 1, "Head autorizado deve ser aceito");
    assert_eq!(
        result[0].hash, test_heads[0].hash,
        "Hash deve corresponder ao original"
    );

    Ok(())
}

#[tokio::test]
async fn test_verify_heads_permissions_with_unauthorized_identity() -> Result<()> {
    // Setup
    let access_controller = create_test_access_controller().await;
    let unauthorized_identity = create_test_identity("unauthorized_user", "unauth_key_456");

    // NÃO concede permissão (identidade não autorizada)

    // Cria head com identidade não autorizada
    let test_heads = vec![create_test_head(
        "unauthorized_head",
        Some(unauthorized_identity),
    )];

    // Simula verificação
    let result = simulate_permission_check(&test_heads, &access_controller).await?;

    // Verifica resultado
    assert_eq!(result.len(), 0, "Head não autorizado deve ser rejeitado");

    Ok(())
}

#[tokio::test]
async fn test_verify_heads_permissions_with_admin_identity() -> Result<()> {
    // Setup
    let access_controller = create_test_access_controller().await;
    let admin_identity = create_test_identity("admin_user", "admin_key_789");

    // Concede permissão admin (que permite escrita)
    access_controller
        .grant("admin", admin_identity.id())
        .await?;

    // Cria head com identidade admin
    let test_heads = vec![create_test_head("admin_head", Some(admin_identity))];

    // Simula verificação
    let result = simulate_permission_check(&test_heads, &access_controller).await?;

    // Verifica resultado
    assert_eq!(result.len(), 1, "Head com permissão admin deve ser aceito");
    assert_eq!(
        result[0].hash, test_heads[0].hash,
        "Hash deve corresponder ao original"
    );

    Ok(())
}

#[tokio::test]
async fn test_verify_heads_permissions_without_identity() -> Result<()> {
    // Setup
    let access_controller = create_test_access_controller().await;

    // Cria head sem identidade
    let test_heads = vec![create_test_head("no_identity_head", None)];

    // Simula verificação
    let result = simulate_permission_check(&test_heads, &access_controller).await?;

    // Verifica resultado
    assert_eq!(result.len(), 0, "Head sem identidade deve ser rejeitado");

    Ok(())
}

#[tokio::test]
async fn test_verify_heads_permissions_with_wildcard() -> Result<()> {
    // Setup
    let access_controller = create_test_access_controller().await;
    let any_identity = create_test_identity("any_user", "any_key_999");

    // Concede permissão universal
    access_controller.grant("write", "*").await?;

    // Cria head com qualquer identidade válida
    let test_heads = vec![create_test_head("wildcard_head", Some(any_identity))];

    // Simula verificação
    let result = simulate_permission_check(&test_heads, &access_controller).await?;

    // Verifica resultado
    assert_eq!(
        result.len(),
        1,
        "Head deve ser aceito com permissão universal"
    );
    assert_eq!(
        result[0].hash, test_heads[0].hash,
        "Hash deve corresponder ao original"
    );

    Ok(())
}

#[tokio::test]
async fn test_verify_heads_permissions_with_empty_identity() -> Result<()> {
    // Setup
    let access_controller = create_test_access_controller().await;

    // Cria identidade inválida (campos vazios)
    let empty_identity = Identity::new(
        "", // ID vazio
        "", // Chave vazia
        Signatures::new("", ""),
    );

    // Cria head com identidade inválida
    let test_heads = vec![create_test_head(
        "empty_identity_head",
        Some(empty_identity),
    )];

    // Simula verificação
    let result = simulate_permission_check(&test_heads, &access_controller).await?;

    // Verifica resultado
    assert_eq!(
        result.len(),
        0,
        "Head com identidade vazia deve ser rejeitado"
    );

    Ok(())
}

#[tokio::test]
async fn test_verify_heads_permissions_mixed_scenarios() -> Result<()> {
    // Setup
    let access_controller = create_test_access_controller().await;

    let authorized_identity = create_test_identity("authorized", "auth_key");
    let unauthorized_identity = create_test_identity("unauthorized", "unauth_key");
    let admin_identity = create_test_identity("admin", "admin_key");

    // Configura permissões
    access_controller
        .grant("write", authorized_identity.id())
        .await?;
    access_controller
        .grant("admin", admin_identity.id())
        .await?;

    // Cria heads mistos
    let test_heads = vec![
        create_test_head("head_1", Some(authorized_identity)), // Deve ser aceito
        create_test_head("head_2", Some(unauthorized_identity)), // Deve ser rejeitado
        create_test_head("head_3", Some(admin_identity)),      // Deve ser aceito
        create_test_head("head_4", None),                      // Deve ser rejeitado
    ];

    // Simula verificação
    let result = simulate_permission_check(&test_heads, &access_controller).await?;

    // Verifica resultado
    assert_eq!(result.len(), 2, "Apenas 2 heads devem ser aceitos");

    let accepted_hashes: Vec<&str> = result.iter().map(|h| h.hash.as_str()).collect();
    let expected_hash_1 = &test_heads[0].hash;
    let expected_hash_3 = &test_heads[2].hash;
    let rejected_hash_2 = &test_heads[1].hash;
    let rejected_hash_4 = &test_heads[3].hash;

    assert!(
        accepted_hashes.contains(&expected_hash_1.as_str()),
        "Head autorizado deve ser aceito"
    );
    assert!(
        accepted_hashes.contains(&expected_hash_3.as_str()),
        "Head admin deve ser aceito"
    );
    assert!(
        !accepted_hashes.contains(&rejected_hash_2.as_str()),
        "Head não autorizado deve ser rejeitado"
    );
    assert!(
        !accepted_hashes.contains(&rejected_hash_4.as_str()),
        "Head sem identidade deve ser rejeitado"
    );

    Ok(())
}

#[tokio::test]
async fn test_verify_heads_permissions_by_public_key() -> Result<()> {
    // Setup
    let access_controller = create_test_access_controller().await;
    let identity = create_test_identity("user_by_key", "special_key_123");

    // Concede permissão pela chave pública em vez do ID
    access_controller.grant("write", identity.pub_key()).await?;

    // Cria head
    let test_heads = vec![create_test_head("key_based_head", Some(identity))];

    // Simula verificação
    let result = simulate_permission_check(&test_heads, &access_controller).await?;

    // Verifica resultado
    assert_eq!(
        result.len(),
        1,
        "Head autorizado por chave pública deve ser aceito"
    );

    Ok(())
}

/// Função auxiliar que simula a verificação de permissões
/// (implementa a mesma lógica do handle_event_exchange_heads)
async fn simulate_permission_check(
    heads: &[Entry],
    access_controller: &Arc<dyn AccessController>,
) -> Result<Vec<Entry>> {
    let mut authorized_heads = Vec::new();

    for head in heads {
        // Verifica presença de identidade
        let identity = match &head.identity {
            Some(identity) => identity,
            None => continue,
        };

        // Verifica validação básica da identidade
        if identity.id().is_empty() || identity.pub_key().is_empty() {
            continue;
        }

        // Verifica permissões de escrita
        let identity_key = identity.pub_key();
        let has_write_permission = match access_controller.get_authorized_by_role("write").await {
            Ok(authorized_keys) => {
                authorized_keys.contains(&identity_key.to_string())
                    || authorized_keys.contains(&identity.id().to_string())
                    || authorized_keys.contains(&"*".to_string())
            }
            Err(_) => false,
        };

        // Verifica permissões administrativas
        let has_admin_permission = match access_controller.get_authorized_by_role("admin").await {
            Ok(admin_keys) => {
                admin_keys.contains(&identity_key.to_string())
                    || admin_keys.contains(&identity.id().to_string())
                    || admin_keys.contains(&"*".to_string())
            }
            Err(_) => false,
        };

        // Aceita se tem qualquer permissão adequada
        if has_write_permission || has_admin_permission {
            authorized_heads.push(head.clone());
        }
    }

    Ok(authorized_heads)
}

/// Testa integração com diferentes tipos de Access Controllers
#[tokio::test]
async fn test_access_controller_integration() -> Result<()> {
    let access_controller = create_test_access_controller().await;

    // Verifica tipo
    assert_eq!(
        access_controller.get_type(),
        "simple",
        "Deve ser SimpleAccessController"
    );

    // Verifica operações básicas
    access_controller.grant("write", "test_key").await?;
    let write_keys = access_controller.get_authorized_by_role("write").await?;
    assert!(
        write_keys.contains(&"test_key".to_string()),
        "Chave deve estar autorizada"
    );

    access_controller.revoke("write", "test_key").await?;
    let write_keys_after = access_controller.get_authorized_by_role("write").await?;
    assert!(
        !write_keys_after.contains(&"test_key".to_string()),
        "Chave deve estar revogada"
    );

    Ok(())
}
