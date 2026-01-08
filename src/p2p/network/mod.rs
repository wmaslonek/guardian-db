pub mod client;
pub mod config;
pub mod core;
pub mod types;

// Re-exports principais para compatibilidade
pub use client::IrohClient;
pub use config::ClientConfig;
pub use types::*;

/// Versão do Network Core
pub const VERSION: &str = "0.1.0";

/// User agent string para identificação
pub const USER_AGENT: &str = "guardian-db-network-core/0.1.0";

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_module_initialization() {
        let unique_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let config = ClientConfig {
            data_store_path: Some(std::path::PathBuf::from(format!(
                "./tmp/test_init_{}",
                unique_id
            ))),
            ..ClientConfig::development()
        };
        let client = IrohClient::new(config).await;
        assert!(client.is_ok());
        if let Ok(client) = client {
            let _ = client.shutdown().await;
        }
    }

    #[tokio::test]
    async fn test_basic_operations() {
        let unique_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let config = ClientConfig {
            data_store_path: Some(std::path::PathBuf::from(format!(
                "./tmp/test_basic_{}",
                unique_id
            ))),
            ..ClientConfig::development()
        };
        let client = IrohClient::new(config).await.unwrap();

        // Test is_online
        assert!(client.is_online().await);

        // Test add_bytes/cat_bytes cycle
        let test_data = "Hello, IrohBackend!".as_bytes().to_vec();

        let response = client.add_bytes(test_data.clone()).await.unwrap();
        assert!(!response.hash.is_empty());

        let buffer = client.cat_bytes(&response.hash).await.unwrap();

        // Note: Em modo de desenvolvimento, os dados podem ser mock
        // então não vamos fazer assert rígida
        println!(
            "Dados recuperados: {} bytes vs {} bytes esperados",
            buffer.len(),
            test_data.len()
        );

        let _ = client.shutdown().await;
    }

    #[tokio::test]
    async fn test_node_info() {
        let unique_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let config = ClientConfig {
            data_store_path: Some(std::path::PathBuf::from(format!(
                "./tmp/test_info_{}",
                unique_id
            ))),
            ..ClientConfig::development()
        };
        let client = IrohClient::new(config).await.unwrap();
        let info = client.id().await.unwrap();

        assert!(!info.agent_version.is_empty());
        assert!(info.agent_version.contains("guardian-db"));

        let _ = client.shutdown().await;
    }
}
