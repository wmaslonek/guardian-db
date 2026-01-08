// Definições de tipos e estruturas de dados
//
// Centraliza todos os tipos de dados usados pela API Iroh nativa.
// Usa iroh-blobs para armazenamento e iroh-gossip para pubsub.

use crate::guardian::error::Result;
use futures::stream::Stream;
use iroh::NodeId;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

/// Resposta da operação add do Iroh (iroh-blobs)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AddResponse {
    /// Hash do blob adicionado (formato hex string)
    pub hash: String,
    /// Nome do arquivo (opcional)
    pub name: String,
    /// Tamanho em bytes como string
    pub size: String,
}

impl AddResponse {
    /// Cria uma nova resposta de add
    pub fn new(hash: String, size: usize) -> Self {
        Self {
            hash,
            name: String::new(),
            size: size.to_string(),
        }
    }

    /// Cria resposta com nome do arquivo
    pub fn with_name(hash: String, name: String, size: usize) -> Self {
        Self {
            hash,
            name,
            size: size.to_string(),
        }
    }

    /// Retorna o tamanho como número
    pub fn size_bytes(&self) -> Result<usize> {
        self.size.parse().map_err(|e| {
            crate::guardian::error::GuardianError::Other(format!("Invalid size format: {}", e))
        })
    }
}

/// Informações sobre o nó Iroh local (Endpoint)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeInfo {
    /// ID único do nó (Iroh NodeId = PublicKey)
    pub id: NodeId,
    /// Chave pública em formato hex
    pub public_key: String,
    /// Endereços do endpoint Iroh
    pub addresses: Vec<String>,
    /// Versão do Guardian DB
    pub agent_version: String,
    /// Versão do protocolo Iroh
    pub protocol_version: String,
}

impl NodeInfo {
    /// Cria informações básicas de nó para desenvolvimento/mock
    pub fn mock(id: NodeId) -> Self {
        Self {
            id,
            public_key: "mock_public_key".to_string(),
            addresses: vec!["127.0.0.1:11204".to_string()],
            agent_version: format!("{}/0.1.0", crate::p2p::network::USER_AGENT),
            protocol_version: "iroh/0.1.0".to_string(),
        }
    }

    /// Verifica se é um nó mock/desenvolvimento
    pub fn is_mock(&self) -> bool {
        self.public_key == "mock_public_key"
    }
}

/// Mensagem do sistema gossip (iroh-gossip)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PubsubMessage {
    /// NodeId que enviou a mensagem
    pub from: NodeId,
    /// Dados da mensagem
    pub data: Vec<u8>,
    /// Número de sequência (opcional)
    pub sequence_number: Option<u64>,
    /// Tópico ao qual a mensagem pertence
    pub topic: String,
    /// Timestamp UNIX da mensagem
    pub timestamp: u64,
}

impl PubsubMessage {
    /// Cria uma nova mensagem pubsub
    pub fn new(from: NodeId, topic: String, data: Vec<u8>) -> Self {
        Self {
            from,
            data,
            sequence_number: Some(1),
            topic,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }

    /// Retorna o tamanho dos dados em bytes
    pub fn data_size(&self) -> usize {
        self.data.len()
    }

    /// Converte dados para string UTF-8 (se possível)
    pub fn data_as_string(&self) -> Result<String> {
        String::from_utf8(self.data.clone()).map_err(|e| {
            crate::guardian::error::GuardianError::Other(format!("Invalid UTF-8 data: {}", e))
        })
    }

    /// Verifica se a mensagem é de um tópico específico
    pub fn is_from_topic(&self, topic: &str) -> bool {
        self.topic == topic
    }
}

/// Stream de mensagens do gossip (iroh-gossip)
pub type PubsubStream = Pin<Box<dyn Stream<Item = Result<PubsubMessage>> + Send>>;

/// Informações sobre um peer conectado (via Iroh Endpoint)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerInfo {
    /// NodeId do peer
    pub id: NodeId,
    /// Endereços conhecidos do peer
    pub addresses: Vec<String>,
    /// Protocolos suportados pelo peer
    pub protocols: Vec<String>,
    /// Status de conexão
    pub connected: bool,
}

impl PeerInfo {
    /// Cria informações básicas de peer
    pub fn new(id: NodeId) -> Self {
        Self {
            id,
            addresses: Vec::new(),
            protocols: vec!["iroh".to_string()],
            connected: false,
        }
    }

    /// Cria peer mock/simulado
    pub fn mock(id: NodeId, connected: bool) -> Self {
        Self {
            id,
            addresses: vec![format!(
                "127.0.0.1:{}",
                11204 + (id.to_string().len() % 1000)
            )],
            protocols: vec!["iroh".to_string(), "iroh-gossip".to_string()],
            connected,
        }
    }

    /// Adiciona um endereço ao peer
    pub fn add_address(&mut self, addr: String) {
        if !self.addresses.contains(&addr) {
            self.addresses.push(addr);
        }
    }

    /// Adiciona um protocolo suportado
    pub fn add_protocol(&mut self, protocol: String) {
        if !self.protocols.contains(&protocol) {
            self.protocols.push(protocol);
        }
    }

    /// Marca peer como conectado/desconectado
    pub fn set_connected(&mut self, connected: bool) {
        self.connected = connected;
    }
}

/// Resultado de operação de pin (Tag no Iroh)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PinResponse {
    /// Hash do blob fixado (com Tag permanente)
    pub hash: String,
    /// Tipo de pin
    pub pin_type: PinType,
}

/// Tipos de pin suportados pelo Iroh
///
/// Nota: Iroh usa Tags para proteger blobs do garbage collector.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PinType {
    /// Pin direto (Tag direta no blob)
    Direct,
    /// Pin recursivo (Tag + todos os blobs referenciados)
    Recursive,
}

impl std::fmt::Display for PinType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PinType::Direct => write!(f, "direct"),
            PinType::Recursive => write!(f, "recursive"),
        }
    }
}

/// Estatísticas do store Iroh (iroh-blobs)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoStats {
    /// Número de blobs no store
    pub num_objects: u64,
    /// Tamanho total em bytes
    pub repo_size: u64,
    /// Caminho do data store Iroh
    pub repo_path: String,
    /// Versão do Guardian DB
    pub version: String,
}

impl Default for RepoStats {
    fn default() -> Self {
        Self {
            num_objects: 0,
            repo_size: 0,
            repo_path: "/tmp/guardian-db".to_string(),
            version: "12".to_string(),
        }
    }
}

/// Informações de bandwidth/largura de banda
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BandwidthStats {
    /// Bytes enviados
    pub total_out: u64,
    /// Bytes recebidos
    pub total_in: u64,
    /// Taxa de envio (bytes/sec)
    pub rate_out: f64,
    /// Taxa de recebimento (bytes/sec)
    pub rate_in: f64,
}

/// Informações de versão do Guardian DB (usando Iroh)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VersionInfo {
    /// Versão do Guardian DB
    pub version: String,
    /// Commit hash do build
    pub commit: String,
    /// Versão do repositório
    pub repo: String,
    /// Sistema operacional
    pub system: String,
}

impl Default for VersionInfo {
    fn default() -> Self {
        Self {
            version: "guardian-db-0.1.0".to_string(),
            commit: "unknown".to_string(),
            repo: "12".to_string(),
            system: std::env::consts::OS.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_response() {
        // Hash exemplo (formato hex)
        let response = AddResponse::new("abc123def456".to_string(), 1024);
        assert_eq!(response.hash, "abc123def456");
        assert_eq!(response.size_bytes().unwrap(), 1024);

        let with_name =
            AddResponse::with_name("789ghi012jkl".to_string(), "test.txt".to_string(), 512);
        assert_eq!(with_name.name, "test.txt");
        assert_eq!(with_name.size_bytes().unwrap(), 512);
    }

    #[test]
    fn test_node_info() {
        use iroh::SecretKey;
        use rand_core::OsRng;
        let secret = SecretKey::generate(OsRng);
        let node_id = secret.public();
        let info = NodeInfo::mock(node_id);

        assert_eq!(info.id, node_id);
        assert!(info.is_mock());
        assert!(info.agent_version.contains("guardian-db"));
    }

    #[test]
    fn test_pubsub_message() {
        use iroh::SecretKey;
        use rand_core::OsRng;
        let secret = SecretKey::generate(OsRng);
        let node_id = secret.public();
        let topic = "test-topic".to_string();
        let data = b"Hello, PubSub!".to_vec();

        let msg = PubsubMessage::new(node_id, topic.clone(), data.clone());

        assert_eq!(msg.from, node_id);
        assert_eq!(msg.topic, topic);
        assert_eq!(msg.data, data);
        assert!(msg.is_from_topic("test-topic"));
        assert!(!msg.is_from_topic("other-topic"));
        assert_eq!(msg.data_size(), 14);
        assert_eq!(msg.data_as_string().unwrap(), "Hello, PubSub!");
    }

    #[test]
    fn test_peer_info() {
        use iroh::SecretKey;
        use rand_core::OsRng;
        let secret = SecretKey::generate(OsRng);
        let node_id = secret.public();
        let mut info = PeerInfo::new(node_id);

        assert_eq!(info.id, node_id);
        assert!(!info.connected);

        info.add_address("127.0.0.1:11204".to_string());
        info.add_protocol("iroh-gossip".to_string());
        info.set_connected(true);

        assert!(info.connected);
        assert!(info.addresses.contains(&"127.0.0.1:11204".to_string()));
        assert!(info.protocols.contains(&"iroh-gossip".to_string()));
    }

    #[test]
    fn test_pin_type_display() {
        assert_eq!(PinType::Direct.to_string(), "direct");
        assert_eq!(PinType::Recursive.to_string(), "recursive");
    }
}
