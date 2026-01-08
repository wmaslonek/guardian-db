/// Utilitários compartilhados para testes de integração
use guardian_db::guardian::GuardianDB;
use guardian_db::guardian::core::NewGuardianDBOptions;
use guardian_db::p2p::network::client::IrohClient;
use guardian_db::p2p::network::config::ClientConfig;
use std::path::PathBuf;
use tempfile::TempDir;

/// Configuração de teste para um nó GuardianDB
pub struct TestNode {
    pub db: GuardianDB,
    #[allow(dead_code)]
    pub iroh: IrohClient,
    pub temp_dir: TempDir,
}

impl TestNode {
    /// Cria um novo nó de teste isolado
    pub async fn new(node_name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let data_path = temp_dir.path().join(node_name);

        // Configuração para teste - usa configuração de teste simples
        let mut iroh_config = ClientConfig::testing();
        iroh_config.data_store_path = Some(data_path.join("iroh"));
        iroh_config.port = 0; // Porta dinâmica

        let iroh = IrohClient::new(iroh_config).await?;

        // Configuração GuardianDB com Backend necessário
        let db_options = NewGuardianDBOptions {
            directory: Some(data_path.join("guardian")),
            backend: Some(iroh.backend().clone()),
            ..Default::default()
        };

        let db = GuardianDB::new(iroh.clone(), Some(db_options)).await?;

        Ok(TestNode { db, iroh, temp_dir })
    }

    /// Cria um nó com configuração customizada
    #[allow(dead_code)]
    pub async fn with_config(
        _node_name: &str,
        iroh_config: ClientConfig,
        mut db_options: NewGuardianDBOptions,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;

        let iroh = IrohClient::new(iroh_config).await?;

        // Garante que o Backend está configurado
        if db_options.backend.is_none() {
            db_options.backend = Some(iroh.backend().clone());
        }

        let db = GuardianDB::new(iroh.clone(), Some(db_options)).await?;

        Ok(TestNode { db, iroh, temp_dir })
    }

    /// Retorna o caminho do diretório temporário
    pub fn path(&self) -> PathBuf {
        self.temp_dir.path().to_path_buf()
    }
}

/// Inicializa logging para testes
pub fn init_test_logging() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .try_init();
}

/// Helper para criar múltiplos nós de teste
pub async fn create_test_nodes(count: usize) -> Result<Vec<TestNode>, Box<dyn std::error::Error>> {
    let mut nodes = Vec::new();

    for i in 0..count {
        let node_name = format!("node_{}", i);
        let node = TestNode::new(&node_name).await?;
        nodes.push(node);
    }

    Ok(nodes)
}

/// Aguarda propagação de mensagens P2P (helper para testes)
#[allow(dead_code)]
pub async fn wait_for_propagation() {
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
}

/// Aguarda tempo maior para discovery de peers
#[allow(dead_code)]
pub async fn wait_for_discovery() {
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_single_node() {
        init_test_logging();
        let node = TestNode::new("test_node").await.unwrap();
        assert!(node.path().exists());
    }

    #[tokio::test]
    async fn test_create_multiple_nodes() {
        init_test_logging();
        let nodes = create_test_nodes(3).await.unwrap();
        assert_eq!(nodes.len(), 3);

        for node in &nodes {
            assert!(node.path().exists());
        }
    }
}
