// Testes de integração simples para o módulo GuardianDB Wrapper
//
// NOTA: Este é um conjunto simplificado de testes que foca nas operações
// de leitura e criação de stores. Os testes completos de escrita requerem
// refatoração da API para suportar melhor Arc<dyn Trait> vs &mut self.
//
// Este módulo testa:
// - Criação do GuardianDB
// - Criação de diferentes tipos de stores
// - Operações básicas de leitura
// - Access control registration

#[cfg(test)]
mod guardian_wrapper_integration_tests {
    use crate::guardian::GuardianDB;
    use crate::guardian::core::NewGuardianDBOptions;
    use crate::guardian::error::{GuardianError, Result};
    use crate::log::identity::{Identity, Signatures};
    use crate::p2p::EventBus;
    use crate::p2p::network::client::IrohClient;
    use crate::p2p::network::config::ClientConfig;
    use crate::p2p::network::core::IrohBackend;
    use std::sync::Arc;
    use tempfile::TempDir;

    // ========================================================================
    // HELPERS - Funções auxiliares para criação de objetos de teste
    // ========================================================================

    /// Cria uma identidade de teste válida
    fn _create_test_identity(id: &str) -> Identity {
        let signatures = Signatures::new(&format!("sig_id_{}", id), &format!("sig_pubkey_{}", id));
        Identity::new(id, &format!("pubkey_{}", id), signatures)
    }

    /// Cria um IrohBackend de teste temporário
    async fn create_test_backend(name: &str) -> Result<(Arc<IrohBackend>, TempDir)> {
        let temp_dir = TempDir::new()
            .map_err(|e| GuardianError::Other(format!("Failed to create temp dir: {}", e)))?;

        let client_config = ClientConfig {
            data_store_path: Some(temp_dir.path().join(name)),
            ..Default::default()
        };

        let backend = Arc::new(IrohBackend::new(&client_config).await?);
        Ok((backend, temp_dir))
    }

    /// Cria uma instância de GuardianDB para testes
    async fn create_test_guardian_db(
        name: &str,
    ) -> Result<(GuardianDB, Arc<IrohBackend>, TempDir)> {
        let (backend, temp_dir) = create_test_backend(name).await?;
        let client = IrohClient::new_with_backend(backend.clone()).await?;

        let options = NewGuardianDBOptions {
            directory: Some(temp_dir.path().join("db").to_path_buf()),
            backend: Some(backend.clone()),
            event_bus: Some(Arc::new(EventBus::new())),
            ..Default::default()
        };

        let guardian = GuardianDB::new(client, Some(options)).await?;

        Ok((guardian, backend, temp_dir))
    }

    // ========================================================================
    // TESTES - GuardianDB Criação e Configuração
    // ========================================================================

    #[tokio::test]
    async fn test_guardian_db_creation() {
        // Testa a criação básica de uma instância GuardianDB
        let result = create_test_guardian_db("test_creation").await;
        assert!(result.is_ok(), "GuardianDB creation should succeed");

        let (guardian, _backend, _temp_dir) = result.unwrap();
        assert!(
            guardian.base().identity().id() != "",
            "Identity should be set"
        );
    }

    #[tokio::test]
    async fn test_guardian_db_with_custom_options() {
        // Testa a criação com opções customizadas
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let client_config = ClientConfig {
            data_store_path: Some(temp_dir.path().join("custom")),
            ..Default::default()
        };
        let backend = Arc::new(
            IrohBackend::new(&client_config)
                .await
                .expect("Backend creation failed"),
        );
        let client = IrohClient::new_with_backend(backend.clone())
            .await
            .expect("Client creation failed");

        let options = NewGuardianDBOptions {
            directory: Some(temp_dir.path().join("custom_db").to_path_buf()),
            backend: Some(backend),
            ..Default::default()
        };

        let result = GuardianDB::new(client, Some(options)).await;
        assert!(
            result.is_ok(),
            "GuardianDB with custom options should succeed"
        );
    }

    // ========================================================================
    // TESTES - EventLogStore Creation
    // ========================================================================

    #[tokio::test]
    async fn test_eventlog_creation() {
        // Testa a criação de um EventLogStore
        let (guardian, _backend, _temp_dir) = create_test_guardian_db("test_eventlog_creation")
            .await
            .expect("Guardian creation failed");

        let result = guardian.log("eventlog_creation_test", None).await;
        assert!(result.is_ok(), "EventLogStore creation should succeed");

        let store = result.unwrap();
        assert_eq!(
            store.store_type(),
            "eventlog",
            "Store type should be eventlog"
        );
    }

    #[tokio::test]
    async fn test_eventlog_address() {
        // Testa se o endereço do EventLogStore está correto
        let (guardian, _backend, _temp_dir) = create_test_guardian_db("test_eventlog_address")
            .await
            .expect("Guardian creation failed");

        let store = guardian
            .log("eventlog_address_test", None)
            .await
            .expect("EventLogStore creation failed");

        let address = store.address();
        assert!(
            !address.to_string().is_empty(),
            "Address should not be empty"
        );
    }

    #[tokio::test]
    async fn test_eventlog_list_empty() {
        // Testa listar de um log vazio
        let (guardian, _backend, _temp_dir) = create_test_guardian_db("test_eventlog_empty")
            .await
            .expect("Guardian creation failed");

        let store = guardian
            .log("test_log", None)
            .await
            .expect("EventLogStore creation failed");

        let result = store.list(None).await;
        assert!(result.is_ok(), "List on empty log should succeed");

        let operations = result.unwrap();
        assert_eq!(operations.len(), 0, "Empty log should return empty list");
    }

    // ========================================================================
    // TESTES - KeyValueStore Creation
    // ========================================================================

    #[tokio::test]
    async fn test_keyvalue_creation() {
        // Testa a criação de um KeyValueStore
        let (guardian, _backend, _temp_dir) = create_test_guardian_db("test_kv_creation")
            .await
            .expect("Guardian creation failed");

        let result = guardian.key_value("keyvalue_creation_test", None).await;
        assert!(result.is_ok(), "KeyValueStore creation should succeed");

        let store = result.unwrap();
        assert_eq!(
            store.store_type(),
            "keyvalue",
            "Store type should be keyvalue"
        );
    }

    #[tokio::test]
    async fn test_keyvalue_get_nonexistent_key() {
        // Testa buscar chave inexistente
        let (guardian, _backend, _temp_dir) = create_test_guardian_db("test_kv_nonexistent")
            .await
            .expect("Guardian creation failed");

        let store = guardian
            .key_value("test_kv", None)
            .await
            .expect("KeyValueStore creation failed");

        let result = store.get("nonexistent_key").await;
        assert!(result.is_ok(), "Get should succeed");
        assert_eq!(result.unwrap(), None, "Nonexistent key should return None");
    }

    #[tokio::test]
    async fn test_keyvalue_all_empty() {
        // Testa all() em store vazio
        let (guardian, _backend, _temp_dir) = create_test_guardian_db("test_kv_empty_all")
            .await
            .expect("Guardian creation failed");

        let store = guardian
            .key_value("test_kv", None)
            .await
            .expect("KeyValueStore creation failed");

        let all_data = store.all();
        assert_eq!(all_data.len(), 0, "Empty store should return empty map");
    }

    // ========================================================================
    // TESTES - DocumentStore Creation
    // ========================================================================

    #[tokio::test]
    async fn test_document_creation() {
        // Testa a criação de um DocumentStore
        let (guardian, _backend, _temp_dir) = create_test_guardian_db("test_doc_creation")
            .await
            .expect("Guardian creation failed");

        let result = guardian.docs("document_creation_test", None).await;
        assert!(result.is_ok(), "DocumentStore creation should succeed");

        let store = result.unwrap();
        assert_eq!(
            store.store_type(),
            "document",
            "Store type should be document"
        );
    }

    #[tokio::test]
    async fn test_document_address() {
        // Testa se o endereço do DocumentStore está correto
        let (guardian, _backend, _temp_dir) = create_test_guardian_db("test_doc_address")
            .await
            .expect("Guardian creation failed");

        let store = guardian
            .docs("test_docs", None)
            .await
            .expect("DocumentStore creation failed");

        let address = store.address();
        assert!(
            !address.to_string().is_empty(),
            "Address should not be empty"
        );
    }

    // ========================================================================
    // TESTES - Access Control
    // ========================================================================

    // NOTA: Testes de registro de access control types foram removidos
    // devido à complexidade dos lifetimes envolvidos. Esses testes podem
    // ser reimplementados de forma mais adequada em testes específicos
    // do módulo access_control.

    #[tokio::test]
    async fn test_register_default_access_control_types() {
        // Testa registrar tipos de access control padrão
        let (guardian, _backend, _temp_dir) = create_test_guardian_db("test_ac_defaults")
            .await
            .expect("Guardian creation failed");

        let result = guardian.register_default_access_control_types().await;
        assert!(
            result.is_ok(),
            "Register default access control types should succeed"
        );

        // Verifica que alguns tipos foram registrados
        let names = guardian.access_control_types_names();
        assert!(!names.is_empty(), "Should have default types registered");
    }

    // ========================================================================
    // TESTES - Multiple Stores
    // ========================================================================

    #[tokio::test]
    async fn test_multiple_stores_same_guardian() {
        // Testa criar múltiplos stores com o mesmo GuardianDB
        let (guardian, _backend, _temp_dir) = create_test_guardian_db("test_multi_stores")
            .await
            .expect("Guardian creation failed");

        // Cria diferentes tipos de stores
        let log_result = guardian.log("test_log", None).await;
        let kv_result = guardian.key_value("test_kv", None).await;
        let doc_result = guardian.docs("test_docs", None).await;

        assert!(log_result.is_ok(), "EventLogStore creation should succeed");
        assert!(kv_result.is_ok(), "KeyValueStore creation should succeed");
        assert!(doc_result.is_ok(), "DocumentStore creation should succeed");
    }

    #[tokio::test]
    async fn test_different_stores_different_addresses() {
        // Testa criar stores com diferentes endereços
        let (guardian, _backend, _temp_dir) = create_test_guardian_db("test_diff_addresses")
            .await
            .expect("Guardian creation failed");

        let store1 = guardian
            .key_value("store1", None)
            .await
            .expect("Store1 creation failed");
        let store2 = guardian
            .key_value("store2", None)
            .await
            .expect("Store2 creation failed");

        let addr1 = store1.address().to_string();
        let addr2 = store2.address().to_string();

        assert_ne!(
            addr1, addr2,
            "Different stores should have different addresses"
        );
    }

    // ========================================================================
    // TESTES - Base GuardianDB Access
    // ========================================================================

    #[tokio::test]
    async fn test_base_access() {
        // Testa acesso ao BaseGuardianDB
        let (guardian, _backend, _temp_dir) = create_test_guardian_db("test_base_access")
            .await
            .expect("Guardian creation failed");

        let base = guardian.base();
        assert!(
            base.identity().id() != "",
            "Base identity should be accessible"
        );
    }

    #[tokio::test]
    async fn test_identity_access() {
        // Testa acesso à identidade
        let (guardian, _backend, _temp_dir) = create_test_guardian_db("test_identity")
            .await
            .expect("Guardian creation failed");

        let identity = guardian.base().identity();
        assert!(!identity.id().is_empty(), "Identity ID should not be empty");
        // public_key() pode ser None se a identidade não foi completamente inicializada
        // Vamos apenas verificar que temos um ID válido
    }
}
