// Camada de compatibilidade
//
// Mantém compatibilidade com código existente que usava ipfs_api_backend_hyper

use crate::error::Result;
use crate::ipfs_core_api::client::IpfsClient;
use cid::Cid;
use std::pin::Pin;
use tokio::io::AsyncReadExt;

/// Stream compatível que possui método concat() similar ao ipfs_api_backend_hyper
pub struct ConcatStream {
    inner: Option<Pin<Box<dyn tokio::io::AsyncRead + Send>>>,
    error: Option<crate::error::GuardianError>,
}

impl ConcatStream {
    /// Cria um novo ConcatStream com um reader
    pub fn new(reader: Pin<Box<dyn tokio::io::AsyncRead + Send>>) -> Self {
        Self {
            inner: Some(reader),
            error: None,
        }
    }

    /// Cria um ConcatStream com erro
    pub fn error(error: crate::error::GuardianError) -> Self {
        Self {
            inner: None,
            error: Some(error),
        }
    }

    /// Método compatível que coleta todos os dados em um Vec<u8>
    /// Similar ao concat() do ipfs_api_backend_hyper
    pub async fn concat(mut self) -> Result<Vec<u8>> {
        if let Some(error) = self.error {
            return Err(error);
        }

        if let Some(mut reader) = self.inner.take() {
            let mut buffer = Vec::new();
            reader.read_to_end(&mut buffer).await.map_err(|e| {
                crate::error::GuardianError::Other(format!("Falha ao ler dados do stream: {}", e))
            })?;
            Ok(buffer)
        } else {
            Ok(Vec::new())
        }
    }

    /// Método para obter o tamanho estimado (compatibilidade)
    pub fn size_hint(&self) -> (usize, Option<usize>) {
        (0, None)
    }
}

/// Adaptador para manter compatibilidade total com ipfs_api_backend_hyper::IpfsClient
pub struct IpfsClientAdapter {
    client: IpfsClient,
}

impl IpfsClientAdapter {
    /// Cria um novo adaptador com um cliente IPFS
    pub fn new(client: IpfsClient) -> Self {
        Self { client }
    }

    /// Cria adaptador com configuração padrão
    pub async fn default() -> Result<Self> {
        let client = IpfsClient::default().await?;
        Ok(Self::new(client))
    }

    /// Cria adaptador para desenvolvimento
    pub async fn development() -> Result<Self> {
        let client = IpfsClient::development().await?;
        Ok(Self::new(client))
    }

    /// Cria adaptador para produção
    pub async fn production() -> Result<Self> {
        let client = IpfsClient::production().await?;
        Ok(Self::new(client))
    }

    /// Método compatível com ipfs_api_backend_hyper::IpfsClient::add
    pub async fn add<R>(&self, data: R) -> Result<AddResponse>
    where
        R: tokio::io::AsyncRead + Send + Unpin + 'static,
    {
        self.client.add(data).await
    }

    /// Método compatível com ipfs_api_backend_hyper::IpfsClient::cat
    /// Retorna ConcatStream para manter compatibilidade total
    pub async fn cat(&self, hash: &str) -> ConcatStream {
        match self.client.cat(hash).await {
            Ok(stream) => ConcatStream::new(stream),
            Err(e) => ConcatStream::error(e),
        }
    }

    /// Método compatível com dag_get
    pub async fn dag_get(&self, cid: &Cid, path: Option<&str>) -> Result<Vec<u8>> {
        self.client.dag_get(cid, path).await
    }

    /// Método compatível com dag_put
    pub async fn dag_put(&self, data: &[u8]) -> Result<Cid> {
        self.client.dag_put(data).await
    }

    /// Método compatível para pubsub publish
    pub async fn pubsub_publish(&self, topic: &str, data: &[u8]) -> Result<()> {
        self.client.pubsub_publish(topic, data).await
    }

    /// Método compatível para pubsub subscribe
    pub async fn pubsub_subscribe(&self, topic: &str) -> Result<PubsubStream> {
        self.client.pubsub_subscribe(topic).await
    }

    /// Método compatível para pubsub peers
    pub async fn pubsub_peers(&self, topic: &str) -> Result<Vec<libp2p::PeerId>> {
        self.client.pubsub_peers(topic).await
    }

    /// Método compatível para pubsub topics
    pub async fn pubsub_topics(&self) -> Result<Vec<String>> {
        self.client.pubsub_topics().await
    }

    /// Método compatível para swarm connect
    pub async fn swarm_connect(&self, peer: &libp2p::PeerId) -> Result<()> {
        self.client.swarm_connect(peer).await
    }

    /// Método compatível para swarm peers
    pub async fn swarm_peers(&self) -> Result<Vec<PeerInfo>> {
        self.client.swarm_peers().await
    }

    /// Método compatível para node id
    pub async fn id(&self) -> Result<NodeInfo> {
        self.client.id().await
    }

    /// Método compatível para pin add
    pub async fn pin_add(&self, hash: &str, recursive: bool) -> Result<PinResponse> {
        self.client.pin_add(hash, recursive).await
    }

    /// Método compatível para pin rm
    pub async fn pin_rm(&self, hash: &str) -> Result<PinResponse> {
        self.client.pin_rm(hash).await
    }

    /// Método compatível para pin ls
    pub async fn pin_ls(&self, pin_type: Option<PinType>) -> Result<Vec<PinResponse>> {
        self.client.pin_ls(pin_type).await
    }

    /// Método compatível para repo stat
    pub async fn repo_stat(&self) -> Result<RepoStats> {
        self.client.repo_stat().await
    }

    /// Verifica se está online
    pub async fn is_online(&self) -> bool {
        self.client.is_online().await
    }

    /// Shutdown do cliente
    pub async fn shutdown(&self) -> Result<()> {
        self.client.shutdown().await
    }

    /// Acesso ao cliente interno
    pub fn inner(&self) -> &IpfsClient {
        &self.client
    }

    /// Gera channel ID (compatibilidade com one_on_one_channel.rs)
    pub fn get_channel_id(&self, other_peer: &libp2p::PeerId) -> String {
        self.client.get_channel_id(other_peer)
    }
}

/// Função de conveniência para criar um adaptador com configuração de string
/// Compatível com IpfsClient::from_str() do ipfs_api_backend_hyper
pub async fn from_str(addr: &str) -> Result<IpfsClientAdapter> {
    // Ignora o endereço HTTP e cria cliente nativo
    // Em produção, poderia parsear o endereço para extrair configurações
    tracing::warn!(
        "Ignorando endereço HTTP '{}' - usando cliente IPFS nativo",
        addr
    );

    IpfsClientAdapter::default().await
}

/// Cria adaptador com configuração baseada em URL
/// Fornece migração mais suave para código existente
pub async fn from_url(url: &str) -> Result<IpfsClientAdapter> {
    from_str(url).await
}

/// Trait para facilitar migração de código existente
#[allow(async_fn_in_trait)]
pub trait IpfsClientCompat {
    async fn add_compat<R>(&self, data: R) -> Result<AddResponse>
    where
        R: tokio::io::AsyncRead + Send + Unpin + 'static;

    async fn cat_compat(&self, hash: &str) -> Result<Vec<u8>>;
}

impl IpfsClientCompat for IpfsClient {
    async fn add_compat<R>(&self, data: R) -> Result<AddResponse>
    where
        R: tokio::io::AsyncRead + Send + Unpin + 'static,
    {
        self.add(data).await
    }

    async fn cat_compat(&self, hash: &str) -> Result<Vec<u8>> {
        let mut stream = self.cat(hash).await?;
        let mut buffer = Vec::new();
        stream.read_to_end(&mut buffer).await.map_err(|e| {
            crate::error::GuardianError::Other(format!("Failed to read stream: {}", e))
        })?;
        Ok(buffer)
    }
}

/// Macro para facilitar migração de código existente
#[macro_export]
macro_rules! migrate_ipfs_client {
    ($old_client:expr) => {{
        // Substitui ipfs_api_backend_hyper::IpfsClient pelo nosso cliente nativo
        $crate::ipfs_core_api::compat::IpfsClientAdapter::default().await?
    }};
}

/// Re-exports para compatibilidade completa
pub use crate::ipfs_core_api::types::{
    AddResponse, NodeInfo, PeerInfo, PinResponse, PinType, PubsubMessage, PubsubStream, RepoStats,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[tokio::test]
    async fn test_adapter_creation() {
        let adapter = IpfsClientAdapter::default().await;
        assert!(adapter.is_ok());
    }

    #[tokio::test]
    async fn test_concat_stream() {
        let test_data = b"Hello, concat stream!";
        let cursor = tokio::io::BufReader::new(std::io::Cursor::new(test_data.to_vec()));

        let stream = ConcatStream::new(Box::pin(cursor));
        let result = stream.concat().await.unwrap();

        assert_eq!(result, test_data);
    }

    #[tokio::test]
    async fn test_concat_stream_error() {
        let error = crate::error::GuardianError::Other("Test error".to_string());
        let stream = ConcatStream::error(error);

        let result = stream.concat().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[ignore] // Requires running IPFS daemon
    async fn test_adapter_add_and_cat() {
        let adapter = IpfsClientAdapter::development().await.unwrap();

        let test_data = "Hello, adapter!".as_bytes();
        let cursor = Cursor::new(test_data.to_vec());

        // Test add
        let response = adapter.add(cursor).await.unwrap();
        assert!(!response.hash.is_empty());

        // Test cat with ConcatStream
        let stream = adapter.cat(&response.hash).await;
        let retrieved_data = stream.concat().await.unwrap();

        assert_eq!(test_data, retrieved_data.as_slice());
    }

    #[tokio::test]
    #[ignore] // Requires running IPFS daemon
    async fn test_adapter_pubsub() {
        let adapter = IpfsClientAdapter::development().await.unwrap();

        // Test publish
        let result = adapter.pubsub_publish("test-topic", b"test message").await;
        assert!(result.is_ok());

        // Test topics
        let topics = adapter.pubsub_topics().await.unwrap();
        assert!(topics.is_empty());

        // Test peers
        let peers = adapter.pubsub_peers("test-topic").await.unwrap();
        assert!(peers.is_empty());
    }

    #[tokio::test]
    async fn test_from_str() {
        let adapter = from_str("http://localhost:5001").await;
        assert!(adapter.is_ok());

        let adapter = adapter.unwrap();
        assert!(adapter.is_online().await);
    }

    #[tokio::test]
    #[ignore] // Requires running IPFS daemon
    async fn test_compat_trait() {
        let client = IpfsClient::development().await.unwrap();

        let test_data = "compat trait test".as_bytes();
        let cursor = Cursor::new(test_data.to_vec());

        let response = client.add_compat(cursor).await.unwrap();
        let retrieved = client.cat_compat(&response.hash).await.unwrap();

        assert_eq!(test_data, retrieved.as_slice());
    }

    #[tokio::test]
    #[ignore] // Requires running IPFS daemon
    async fn test_dag_operations() {
        let adapter = IpfsClientAdapter::development().await.unwrap();

        let test_data = b"test dag adapter";
        let cid = adapter.dag_put(test_data).await.unwrap();

        let retrieved = adapter.dag_get(&cid, None).await.unwrap();
        assert_eq!(test_data, retrieved.as_slice());
    }

    #[tokio::test]
    #[ignore] // Requires running IPFS daemon
    async fn test_pin_operations() {
        let adapter = IpfsClientAdapter::development().await.unwrap();

        // Add some data first
        let test_data = "pin adapter test".as_bytes();
        let cursor = Cursor::new(test_data.to_vec());
        let response = adapter.add(cursor).await.unwrap();

        // Test pin add
        let pin_response = adapter.pin_add(&response.hash, true).await.unwrap();
        assert_eq!(pin_response.pin_type, PinType::Recursive);

        // Test pin ls
        let pins = adapter.pin_ls(None).await.unwrap();
        assert_eq!(pins.len(), 1);

        // Test pin rm
        let rm_response = adapter.pin_rm(&response.hash).await.unwrap();
        assert_eq!(rm_response.hash, response.hash);
    }

    #[tokio::test]
    #[ignore] // Requires running IPFS daemon
    async fn test_node_info() {
        let adapter = IpfsClientAdapter::development().await.unwrap();

        let info = adapter.id().await.unwrap();
        assert!(!info.agent_version.is_empty());
        assert!(info.agent_version.contains("guardian-db"));
    }

    #[tokio::test]
    #[ignore] // Requires running IPFS daemon
    async fn test_channel_id_compatibility() {
        let adapter = IpfsClientAdapter::development().await.unwrap();
        let other_peer = libp2p::PeerId::random();

        let channel_id = adapter.get_channel_id(&other_peer);
        assert!(channel_id.starts_with("/ipfs-pubsub-direct-channel/v1/"));

        // Test that it matches the inner client
        let inner_channel_id = adapter.inner().get_channel_id(&other_peer);
        assert_eq!(channel_id, inner_channel_id);
    }
}
