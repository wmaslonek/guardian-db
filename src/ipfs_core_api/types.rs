// Definições de tipos e estruturas de dados
//
// Centraliza todos os tipos de dados usados pela API IPFS Core

use crate::error::Result;
use futures::stream::Stream;
use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

/// Resposta da operação add do IPFS
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AddResponse {
    /// Hash/CID do conteúdo adicionado
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
        self.size
            .parse()
            .map_err(|e| crate::error::GuardianError::Other(format!("Invalid size format: {}", e)))
    }
}

/// Informações sobre o nó IPFS local
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeInfo {
    /// ID único do peer/nó
    pub id: PeerId,
    /// Chave pública em formato hex
    pub public_key: String,
    /// Endereços de rede em que o nó está escutando
    pub addresses: Vec<String>,
    /// Versão do agente/software
    pub agent_version: String,
    /// Versão do protocolo IPFS
    pub protocol_version: String,
}

impl NodeInfo {
    /// Cria informações básicas de nó para desenvolvimento/mock
    pub fn mock(id: PeerId) -> Self {
        Self {
            id,
            public_key: "mock_public_key".to_string(),
            addresses: vec!["/ip4/127.0.0.1/tcp/4001".to_string()],
            agent_version: format!("{}/0.1.0", crate::ipfs_core_api::USER_AGENT),
            protocol_version: "ipfs/0.1.0".to_string(),
        }
    }

    /// Verifica se é um nó mock/desenvolvimento
    pub fn is_mock(&self) -> bool {
        self.public_key == "mock_public_key"
    }
}

/// Mensagem do sistema pubsub
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PubsubMessage {
    /// Peer que enviou a mensagem
    pub from: PeerId,
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
    pub fn new(from: PeerId, topic: String, data: Vec<u8>) -> Self {
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
        String::from_utf8(self.data.clone())
            .map_err(|e| crate::error::GuardianError::Other(format!("Invalid UTF-8 data: {}", e)))
    }

    /// Verifica se a mensagem é de um tópico específico
    pub fn is_from_topic(&self, topic: &str) -> bool {
        self.topic == topic
    }
}

/// Stream de mensagens do pubsub
pub type PubsubStream = Pin<Box<dyn Stream<Item = Result<PubsubMessage>> + Send>>;

/// Informações sobre um peer conectado
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerInfo {
    /// ID único do peer
    pub id: PeerId,
    /// Endereços de rede conhecidos
    pub addresses: Vec<String>,
    /// Protocolos suportados pelo peer
    pub protocols: Vec<String>,
    /// Status de conexão
    pub connected: bool,
}

impl PeerInfo {
    /// Cria informações básicas de peer
    pub fn new(id: PeerId) -> Self {
        Self {
            id,
            addresses: Vec::new(),
            protocols: vec!["ipfs".to_string()],
            connected: false,
        }
    }

    /// Cria peer mock/simulado
    pub fn mock(id: PeerId, connected: bool) -> Self {
        Self {
            id,
            addresses: vec![format!(
                "/ip4/127.0.0.1/tcp/{}",
                4000 + (id.to_string().len() % 1000)
            )],
            protocols: vec!["ipfs".to_string(), "libp2p".to_string()],
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

/// Resultado de operação de pin
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PinResponse {
    /// Hash/CID do objeto pinned
    pub hash: String,
    /// Tipo de pin (direct, indirect, recursive)
    pub pin_type: PinType,
}

/// Tipos de pin suportados
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PinType {
    /// Pin direto (apenas o objeto)
    Direct,
    /// Pin indireto (referenciado por outro objeto pinned)
    Indirect,
    /// Pin recursivo (objeto e todas suas referências)
    Recursive,
}

impl std::fmt::Display for PinType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PinType::Direct => write!(f, "direct"),
            PinType::Indirect => write!(f, "indirect"),
            PinType::Recursive => write!(f, "recursive"),
        }
    }
}

/// Estatísticas do repositório IPFS
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoStats {
    /// Número de objetos no repositório
    pub num_objects: u64,
    /// Tamanho total em bytes
    pub repo_size: u64,
    /// Caminho do repositório
    pub repo_path: String,
    /// Versão do formato do repositório
    pub version: String,
}

impl Default for RepoStats {
    fn default() -> Self {
        Self {
            num_objects: 0,
            repo_size: 0,
            repo_path: "/tmp/ipfs".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_response() {
        let response = AddResponse::new("QmTest123".to_string(), 1024);
        assert_eq!(response.hash, "QmTest123");
        assert_eq!(response.size_bytes().unwrap(), 1024);

        let with_name =
            AddResponse::with_name("QmTest456".to_string(), "test.txt".to_string(), 512);
        assert_eq!(with_name.name, "test.txt");
        assert_eq!(with_name.size_bytes().unwrap(), 512);
    }

    #[test]
    fn test_node_info() {
        let peer_id = PeerId::random();
        let info = NodeInfo::mock(peer_id);

        assert_eq!(info.id, peer_id);
        assert!(info.is_mock());
        assert!(info.agent_version.contains("guardian-db"));
    }

    #[test]
    fn test_pubsub_message() {
        let peer_id = PeerId::random();
        let topic = "test-topic".to_string();
        let data = b"Hello, PubSub!".to_vec();

        let msg = PubsubMessage::new(peer_id, topic.clone(), data.clone());

        assert_eq!(msg.from, peer_id);
        assert_eq!(msg.topic, topic);
        assert_eq!(msg.data, data);
        assert!(msg.is_from_topic("test-topic"));
        assert!(!msg.is_from_topic("other-topic"));
        assert_eq!(msg.data_size(), 14);
        assert_eq!(msg.data_as_string().unwrap(), "Hello, PubSub!");
    }

    #[test]
    fn test_peer_info() {
        let peer_id = PeerId::random();
        let mut info = PeerInfo::new(peer_id);

        assert_eq!(info.id, peer_id);
        assert!(!info.connected);

        info.add_address("/ip4/127.0.0.1/tcp/4001".to_string());
        info.add_protocol("gossipsub".to_string());
        info.set_connected(true);

        assert!(info.connected);
        assert!(
            info.addresses
                .contains(&"/ip4/127.0.0.1/tcp/4001".to_string())
        );
        assert!(info.protocols.contains(&"gossipsub".to_string()));
    }

    #[test]
    fn test_pin_type_display() {
        assert_eq!(PinType::Direct.to_string(), "direct");
        assert_eq!(PinType::Indirect.to_string(), "indirect");
        assert_eq!(PinType::Recursive.to_string(), "recursive");
    }
}
