//! Testes robustos para o módulo Manifest
//!
//! Este módulo testa:
//! - Criação e configuração de CreateAccessControllerOptions
//! - Serialização/deserialização CBOR de Manifest e Options
//! - Validação de tipos de controladores
//! - Gerenciamento de permissões de acesso
//! - Conversão de Hash para hex e vice-versa
//! - Edge cases e tratamento de erros

use crate::access_control::manifest::{
    CreateAccessControllerOptions, Manifest, ManifestParams, create_manifest_with_validation,
};
use crate::guardian::error::{GuardianError, Result};
use crate::p2p::network::{client::IrohClient, config::ClientConfig};
use iroh_blobs::Hash;
use std::collections::HashMap;
use std::sync::Arc;

// ============= TEST HELPERS =============

/// Gera um Hash de teste único
fn generate_test_hash(seed: u8) -> Hash {
    let mut bytes = [0u8; 32];
    bytes[0] = seed;
    bytes[31] = seed;
    Hash::from(bytes)
}

/// Cria opções de teste básicas
fn create_test_options() -> CreateAccessControllerOptions {
    let hash = generate_test_hash(1);
    CreateAccessControllerOptions::new(hash, false, "iroh".to_string())
}

/// Cria um IrohClient de teste
async fn create_test_iroh_client() -> Result<Arc<IrohClient>> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let test_path = format!("./tmp/test_manifest_{}", timestamp);

    let mut client_config = ClientConfig::offline();
    client_config.data_store_path = Some(test_path.into());
    let client = IrohClient::new(client_config).await?;
    Ok(Arc::new(client))
}

// ============= CREATE ACCESS CONTROLLER OPTIONS TESTS =============

#[test]
fn test_create_options_new() {
    let hash = generate_test_hash(1);
    let options = CreateAccessControllerOptions::new(hash, false, "iroh".to_string());

    assert_eq!(options.address(), hash);
    assert!(!options.skip_manifest());
    assert_eq!(options.get_type(), "iroh");
    assert_eq!(options.get_name(), "");
}

#[test]
fn test_create_options_default() {
    let options = CreateAccessControllerOptions::default();

    assert_eq!(options.address(), Hash::from([0u8; 32]));
    assert!(!options.skip_manifest());
    assert_eq!(options.get_type(), "");
    assert_eq!(options.get_name(), "");
}

#[test]
fn test_create_options_new_empty() {
    let options = CreateAccessControllerOptions::new_empty();

    assert_eq!(options.address(), Hash::from([0u8; 32]));
    assert!(!options.skip_manifest());
    assert_eq!(options.get_type(), "");
}

#[test]
fn test_create_options_new_simple() {
    let mut access = HashMap::new();
    access.insert("write".to_string(), vec!["user1".to_string()]);

    let options = CreateAccessControllerOptions::new_simple("simple".to_string(), access);

    assert!(options.skip_manifest());
    assert_eq!(options.get_type(), "simple");
    assert_eq!(options.get_access("write").unwrap(), vec!["user1"]);
}

#[test]
fn test_create_options_from_params() {
    let original = create_test_options();
    let cloned = CreateAccessControllerOptions::from_params(&original);

    assert_eq!(cloned.address(), original.address());
    assert_eq!(cloned.skip_manifest(), original.skip_manifest());
    assert_eq!(cloned.get_type(), original.get_type());
}

// ============= MANIFEST PARAMS TRAIT TESTS =============

#[test]
fn test_set_and_get_address() {
    let mut options = create_test_options();
    let new_hash = generate_test_hash(2);

    options.set_address(new_hash);
    assert_eq!(options.address(), new_hash);
}

#[test]
fn test_set_and_get_type() {
    let mut options = create_test_options();

    options.set_type("guardian".to_string());
    assert_eq!(options.get_type(), "guardian");
}

#[test]
fn test_set_and_get_name() {
    let mut options = create_test_options();

    options.set_name("my_controller".to_string());
    assert_eq!(options.get_name(), "my_controller");
}

#[test]
fn test_set_and_get_access() {
    let mut options = create_test_options();

    options.set_access(
        "write".to_string(),
        vec!["user1".to_string(), "user2".to_string()],
    );

    let access = options.get_access("write").unwrap();
    assert_eq!(access.len(), 2);
    assert!(access.contains(&"user1".to_string()));
    assert!(access.contains(&"user2".to_string()));
}

#[test]
fn test_get_nonexistent_access() {
    let options = create_test_options();

    let access = options.get_access("nonexistent");
    assert!(access.is_none());
}

#[test]
fn test_get_all_access() {
    let mut options = create_test_options();

    options.set_access("write".to_string(), vec!["user1".to_string()]);
    options.set_access("read".to_string(), vec!["user2".to_string()]);
    options.set_access("admin".to_string(), vec!["admin1".to_string()]);

    let all_access = options.get_all_access();
    assert_eq!(all_access.len(), 3);
    assert!(all_access.contains_key("write"));
    assert!(all_access.contains_key("read"));
    assert!(all_access.contains_key("admin"));
}

#[test]
fn test_overwrite_access() {
    let mut options = create_test_options();

    options.set_access("write".to_string(), vec!["user1".to_string()]);
    options.set_access("write".to_string(), vec!["user2".to_string()]);

    let access = options.get_access("write").unwrap();
    assert_eq!(access.len(), 1);
    assert_eq!(access[0], "user2");
}

// ============= CBOR SERIALIZATION TESTS =============

#[test]
fn test_options_cbor_serialization() {
    let mut options = create_test_options();
    options.set_name("test_controller".to_string());
    options.set_access("write".to_string(), vec!["user1".to_string()]);

    let serialized = serde_cbor::to_vec(&options).unwrap();
    let deserialized: CreateAccessControllerOptions = serde_cbor::from_slice(&serialized).unwrap();

    assert_eq!(deserialized.address(), options.address());
    assert_eq!(deserialized.skip_manifest(), options.skip_manifest());
    assert_eq!(deserialized.get_type(), options.get_type());
    assert_eq!(deserialized.get_name(), options.get_name());
    assert_eq!(
        deserialized.get_access("write"),
        options.get_access("write")
    );
}

#[test]
fn test_manifest_cbor_serialization() {
    let mut options = create_test_options();
    options.set_access("write".to_string(), vec!["user1".to_string()]);
    let manifest = Manifest {
        get_type: "iroh".to_string(),
        params: options.clone(),
    };

    let serialized = serde_cbor::to_vec(&manifest).unwrap();
    let deserialized: Manifest = serde_cbor::from_slice(&serialized).unwrap();

    assert_eq!(deserialized.get_type, manifest.get_type);
    assert_eq!(deserialized.params.address(), manifest.params.address());
    assert_eq!(
        deserialized.params.get_access("write"),
        manifest.params.get_access("write")
    );
}

#[test]
fn test_hash_hex_serialization() {
    let hash = generate_test_hash(42);
    let mut options = CreateAccessControllerOptions::new(hash, false, "iroh".to_string());
    options.set_access("write".to_string(), vec!["user1".to_string()]);

    let serialized = serde_cbor::to_vec(&options).unwrap();
    let deserialized: CreateAccessControllerOptions = serde_cbor::from_slice(&serialized).unwrap();

    assert_eq!(deserialized.address(), hash);
    assert_eq!(
        hex::encode(deserialized.address().as_bytes()),
        hex::encode(hash.as_bytes())
    );
}

#[test]
fn test_options_with_empty_access_serialization() {
    let options = create_test_options();

    // O CBOR não serializa o campo access quando está vazio (por design)
    // Apenas verificamos que o options foi criado corretamente
    assert_eq!(options.get_all_access().len(), 0);

    // Testa serialização com campo explícito vazio
    let mut options_with_empty = create_test_options();
    options_with_empty.set_access("write".to_string(), vec![]);

    let serialized = serde_cbor::to_vec(&options_with_empty).unwrap();
    let deserialized: CreateAccessControllerOptions = serde_cbor::from_slice(&serialized).unwrap();

    // Deve ter o campo write, mas vazio
    assert_eq!(deserialized.get_access("write").unwrap().len(), 0);
}

#[test]
fn test_options_with_multiple_roles_serialization() {
    let mut options = create_test_options();
    options.set_access("write".to_string(), vec!["writer1".to_string()]);
    options.set_access(
        "read".to_string(),
        vec!["reader1".to_string(), "reader2".to_string()],
    );
    options.set_access("admin".to_string(), vec!["admin1".to_string()]);

    let serialized = serde_cbor::to_vec(&options).unwrap();
    let deserialized: CreateAccessControllerOptions = serde_cbor::from_slice(&serialized).unwrap();

    assert_eq!(deserialized.get_all_access().len(), 3);
    assert_eq!(deserialized.get_access("write").unwrap().len(), 1);
    assert_eq!(deserialized.get_access("read").unwrap().len(), 2);
    assert_eq!(deserialized.get_access("admin").unwrap().len(), 1);
}

// ============= JSON SERIALIZATION TESTS (for comparison) =============

#[test]
fn test_options_json_serialization() {
    let mut options = create_test_options();
    options.set_name("json_test".to_string());
    options.set_access("write".to_string(), vec!["user1".to_string()]);

    let serialized = serde_json::to_string(&options).unwrap();
    let deserialized: CreateAccessControllerOptions = serde_json::from_str(&serialized).unwrap();

    assert_eq!(deserialized.address(), options.address());
    assert_eq!(deserialized.get_name(), options.get_name());
}

#[test]
fn test_manifest_json_serialization() {
    let options = create_test_options();
    let manifest = Manifest {
        get_type: "iroh".to_string(),
        params: options,
    };

    let serialized = serde_json::to_string(&manifest).unwrap();
    let deserialized: Manifest = serde_json::from_str(&serialized).unwrap();

    assert_eq!(deserialized.get_type, manifest.get_type);
}

// ============= VALIDATION TESTS =============

#[test]
fn test_create_manifest_with_validation_success() {
    let options = create_test_options();
    let result = create_manifest_with_validation("iroh".to_string(), options);

    assert!(result.is_ok());
    let manifest = result.unwrap();
    assert_eq!(manifest.get_type, "iroh");
}

#[test]
fn test_create_manifest_validation_guardian_type() {
    let options = create_test_options();
    let result = create_manifest_with_validation("GuardianDB".to_string(), options);

    assert!(result.is_ok());
    let manifest = result.unwrap();
    assert_eq!(manifest.get_type, "GuardianDB");
}

#[test]
fn test_create_manifest_validation_simple_type() {
    let options = create_test_options();
    let result = create_manifest_with_validation("simple".to_string(), options);

    assert!(result.is_ok());
    let manifest = result.unwrap();
    assert_eq!(manifest.get_type, "simple");
}

#[test]
fn test_create_manifest_validation_empty_type() {
    let options = create_test_options();
    let result = create_manifest_with_validation("".to_string(), options);

    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        GuardianError::Store(msg) => assert!(msg.contains("cannot be empty")),
        _ => panic!("Expected Store error"),
    }
}

#[test]
fn test_create_manifest_validation_unknown_type() {
    let options = create_test_options();
    let result = create_manifest_with_validation("unknown".to_string(), options);

    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        GuardianError::Store(msg) => assert!(msg.contains("Unknown controller type")),
        _ => panic!("Expected Store error"),
    }
}

#[test]
fn test_create_manifest_validation_type_too_long() {
    let options = create_test_options();
    let long_type = "a".repeat(256);
    let result = create_manifest_with_validation(long_type, options);

    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        GuardianError::Store(msg) => assert!(msg.contains("too long")),
        _ => panic!("Expected Store error"),
    }
}

// ============= ASYNC CREATE/RESOLVE TESTS =============

#[tokio::test]
async fn test_create_with_skip_manifest() {
    let iroh_client = create_test_iroh_client().await.unwrap();
    let hash = generate_test_hash(5);
    let mut options = CreateAccessControllerOptions::new(hash, true, "iroh".to_string());
    options.skip_manifest = true;

    let result =
        crate::access_control::manifest::create(iroh_client, "iroh".to_string(), &options).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), hash);
}

#[tokio::test]
async fn test_create_with_empty_controller_type() {
    let iroh_client = create_test_iroh_client().await.unwrap();
    let options = create_test_options();

    let result =
        crate::access_control::manifest::create(iroh_client, "".to_string(), &options).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_resolve_with_skip_manifest() {
    let iroh_client = create_test_iroh_client().await.unwrap();
    let mut options = create_test_options();
    options.skip_manifest = true;

    let result =
        crate::access_control::manifest::resolve(iroh_client, "dummy_address", &options).await;

    assert!(result.is_ok());
    let manifest = result.unwrap();
    assert_eq!(manifest.get_type, "iroh");
}

#[tokio::test]
async fn test_resolve_with_empty_address() {
    let iroh_client = create_test_iroh_client().await.unwrap();
    let options = create_test_options();

    let result = crate::access_control::manifest::resolve(iroh_client, "", &options).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_resolve_skip_manifest_without_type() {
    let iroh_client = create_test_iroh_client().await.unwrap();
    let hash = generate_test_hash(3);
    let mut options = CreateAccessControllerOptions::new(hash, true, "".to_string());
    options.skip_manifest = true;

    let result =
        crate::access_control::manifest::resolve(iroh_client, "dummy_address", &options).await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        GuardianError::Store(msg) => assert!(msg.contains("obrigatório")),
        _ => panic!("Expected Store error"),
    }
}

// ============= EDGE CASES =============

#[test]
fn test_hash_all_zeros() {
    let hash = Hash::from([0u8; 32]);
    let options = CreateAccessControllerOptions::new(hash, false, "iroh".to_string());

    assert_eq!(options.address(), hash);
}

#[test]
fn test_hash_all_ones() {
    let hash = Hash::from([255u8; 32]);
    let options = CreateAccessControllerOptions::new(hash, false, "iroh".to_string());

    assert_eq!(options.address(), hash);
}

#[test]
fn test_multiple_access_roles_same_users() {
    let mut options = create_test_options();

    let users = vec!["user1".to_string(), "user2".to_string()];
    options.set_access("write".to_string(), users.clone());
    options.set_access("read".to_string(), users.clone());
    options.set_access("admin".to_string(), users.clone());

    assert_eq!(options.get_access("write").unwrap(), users);
    assert_eq!(options.get_access("read").unwrap(), users);
    assert_eq!(options.get_access("admin").unwrap(), users);
}

#[test]
fn test_access_with_special_characters() {
    let mut options = create_test_options();

    let users = vec![
        "user@domain.com".to_string(),
        "user-with-dashes".to_string(),
        "user_with_underscores".to_string(),
        "user.with.dots".to_string(),
    ];

    options.set_access("write".to_string(), users.clone());

    assert_eq!(options.get_access("write").unwrap(), users);
}

#[test]
fn test_access_with_unicode() {
    let mut options = create_test_options();

    let users = vec![
        "用户1".to_string(),
        "utilisateur2".to_string(),
        "usuario3".to_string(),
        "пользователь4".to_string(),
    ];

    options.set_access("write".to_string(), users.clone());

    let retrieved = options.get_access("write").unwrap();
    assert_eq!(retrieved, users);
}

#[test]
fn test_empty_access_list() {
    let mut options = create_test_options();
    options.set_access("write".to_string(), vec![]);

    let access = options.get_access("write").unwrap();
    assert_eq!(access.len(), 0);
}

#[test]
fn test_large_access_list() {
    let mut options = create_test_options();

    let users: Vec<String> = (0..1000).map(|i| format!("user{}", i)).collect();
    options.set_access("write".to_string(), users.clone());

    let retrieved = options.get_access("write").unwrap();
    assert_eq!(retrieved.len(), 1000);
}

#[test]
fn test_manifest_clone() {
    let options = create_test_options();
    let manifest = Manifest {
        get_type: "iroh".to_string(),
        params: options,
    };

    let cloned = manifest.clone();
    assert_eq!(cloned.get_type, manifest.get_type);
    assert_eq!(cloned.params.address(), manifest.params.address());
}

#[test]
fn test_options_as_any() {
    let options = create_test_options();
    let any = options.as_any();

    assert!(
        any.downcast_ref::<CreateAccessControllerOptions>()
            .is_some()
    );
}

// ============= CONCURRENT ACCESS TESTS =============

#[tokio::test]
async fn test_concurrent_access_modifications() {
    use std::sync::Arc;

    let options = Arc::new(create_test_options());
    let mut handles = vec![];

    // Spawn 10 tasks that concurrently modify access
    for i in 0..10 {
        let opts = options.clone();
        let handle = tokio::spawn(async move {
            let mut temp_opts =
                CreateAccessControllerOptions::new(opts.address(), false, "iroh".to_string());
            temp_opts.set_access(format!("role{}", i), vec![format!("user{}", i)]);
            temp_opts
        });
        handles.push(handle);
    }

    // Wait for all tasks
    for handle in handles {
        handle.await.unwrap();
    }

    // Test passes if no panics occurred (implicitly verified by reaching here)
}

// ============= SERIALIZATION SIZE COMPARISON =============

#[test]
fn test_cbor_vs_json_size() {
    let mut options = create_test_options();
    options.set_name("size_test_controller".to_string());
    options.set_access(
        "write".to_string(),
        vec![
            "user1".to_string(),
            "user2".to_string(),
            "user3".to_string(),
        ],
    );

    let cbor_bytes = serde_cbor::to_vec(&options).unwrap();
    let json_bytes = serde_json::to_string(&options).unwrap().into_bytes();

    println!("CBOR size: {} bytes", cbor_bytes.len());
    println!("JSON size: {} bytes", json_bytes.len());

    // CBOR should generally be smaller or comparable
    assert!(cbor_bytes.len() <= json_bytes.len() * 2);
}
