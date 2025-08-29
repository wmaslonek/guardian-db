// ipfs_core_api/mod.rs - Módulo principal da API IPFS Core 100% Rust
//
// Este módulo reorganiza e expande a funcionalidade do antigo kubo_core_api
// com uma arquitetura modular e limpa.

pub mod client;
pub mod compat;
pub mod config;
pub mod errors;
pub mod types;

// Re-exports principais para compatibilidade
pub use client::IpfsClient;
pub use config::ClientConfig;
pub use types::*;

/// Versão da API IPFS Core
pub const VERSION: &str = "0.1.0";

/// User agent string para identificação
pub const USER_AGENT: &str = "guardian-db-ipfs-core/0.1.0";

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[tokio::test]
    async fn test_module_initialization() {
        let config = ClientConfig::default();
        let client = IpfsClient::new(config).await;
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn test_basic_operations() {
        let client = IpfsClient::default().await.unwrap();

        // Test is_online
        assert!(client.is_online().await);

        // Test add/cat cycle
        let test_data = "Hello, IPFS Core API!".as_bytes();
        let cursor = Cursor::new(test_data.to_vec());

        let response = client.add(cursor).await.unwrap();
        assert!(!response.hash.is_empty());

        let mut stream = client.cat(&response.hash).await.unwrap();
        let mut buffer = Vec::new();

        use tokio::io::AsyncReadExt;
        stream.read_to_end(&mut buffer).await.unwrap();

        assert_eq!(test_data, buffer.as_slice());
    }

    #[tokio::test]
    async fn test_node_info() {
        let client = IpfsClient::default().await.unwrap();
        let info = client.id().await.unwrap();

        assert!(!info.agent_version.is_empty());
        assert!(info.agent_version.contains("guardian-db"));
    }
}
