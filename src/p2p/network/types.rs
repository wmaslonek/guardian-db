// Type and data structure definitions.
//
// Centralizes all data types used by the native Iroh API.
// Uses iroh-blobs for storage and iroh-gossip for pubsub.

use crate::guardian::error::Result;
use futures::stream::Stream;
use iroh::EndpointId as NodeId;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

/// Response of the Iroh add operation (iroh-blobs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AddResponse {
    /// Hash of the added blob (hex string format).
    pub hash: String,
    /// File name (optional).
    pub name: String,
    /// Size in bytes as a string.
    pub size: String,
}

impl AddResponse {
    /// Creates a new add response.
    pub fn new(hash: String, size: usize) -> Self {
        Self {
            hash,
            name: String::new(),
            size: size.to_string(),
        }
    }

    /// Creates a response with a file name.
    pub fn with_name(hash: String, name: String, size: usize) -> Self {
        Self {
            hash,
            name,
            size: size.to_string(),
        }
    }

    /// Returns the size as a number.
    pub fn size_bytes(&self) -> Result<usize> {
        self.size.parse().map_err(|e| {
            crate::guardian::error::GuardianError::Other(format!("Invalid size format: {}", e))
        })
    }
}

/// Information about the local Iroh node (Endpoint).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeInfo {
    /// Unique node ID (Iroh NodeId = PublicKey).
    pub id: NodeId,
    /// Public key in hex format.
    pub public_key: String,
    /// Iroh endpoint addresses.
    pub addresses: Vec<String>,
    /// Guardian DB version.
    pub agent_version: String,
    /// Iroh protocol version.
    pub protocol_version: String,
}

impl NodeInfo {
    /// Creates basic node information for development/mock use.
    pub fn mock(id: NodeId) -> Self {
        Self {
            id,
            public_key: "mock_public_key".to_string(),
            addresses: vec!["127.0.0.1:11204".to_string()],
            agent_version: format!("{}/0.1.0", crate::p2p::network::USER_AGENT),
            protocol_version: "iroh/0.1.0".to_string(),
        }
    }

    /// Returns whether this is a mock/development node.
    pub fn is_mock(&self) -> bool {
        self.public_key == "mock_public_key"
    }
}

/// Message from the gossip system (iroh-gossip).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PubsubMessage {
    /// NodeId that sent the message.
    pub from: NodeId,
    /// Message data.
    pub data: Vec<u8>,
    /// Sequence number (optional).
    pub sequence_number: Option<u64>,
    /// Topic the message belongs to.
    pub topic: String,
    /// UNIX timestamp of the message.
    pub timestamp: u64,
}

impl PubsubMessage {
    /// Creates a new pubsub message.
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

    /// Returns the data size in bytes.
    pub fn data_size(&self) -> usize {
        self.data.len()
    }

    /// Converts the data into a UTF-8 string (if possible).
    pub fn data_as_string(&self) -> Result<String> {
        String::from_utf8(self.data.clone()).map_err(|e| {
            crate::guardian::error::GuardianError::Other(format!("Invalid UTF-8 data: {}", e))
        })
    }

    /// Returns whether the message belongs to a specific topic.
    pub fn is_from_topic(&self, topic: &str) -> bool {
        self.topic == topic
    }
}

/// Stream of gossip messages (iroh-gossip).
pub type PubsubStream = Pin<Box<dyn Stream<Item = Result<PubsubMessage>> + Send>>;

/// Information about a connected peer (via an Iroh Endpoint).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerInfo {
    /// The peer's NodeId.
    pub id: NodeId,
    /// Known addresses of the peer.
    pub addresses: Vec<String>,
    /// Protocols supported by the peer.
    pub protocols: Vec<String>,
    /// Connection status.
    pub connected: bool,
}

impl PeerInfo {
    /// Creates basic peer information.
    pub fn new(id: NodeId) -> Self {
        Self {
            id,
            addresses: Vec::new(),
            protocols: vec!["iroh".to_string()],
            connected: false,
        }
    }

    /// Creates a mock/simulated peer.
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

    /// Adds an address to the peer.
    pub fn add_address(&mut self, addr: String) {
        if !self.addresses.contains(&addr) {
            self.addresses.push(addr);
        }
    }

    /// Adds a supported protocol.
    pub fn add_protocol(&mut self, protocol: String) {
        if !self.protocols.contains(&protocol) {
            self.protocols.push(protocol);
        }
    }

    /// Marks the peer as connected/disconnected.
    pub fn set_connected(&mut self, connected: bool) {
        self.connected = connected;
    }
}

/// Result of a pin operation (Tag in Iroh).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PinResponse {
    /// Hash of the pinned blob (with a permanent Tag).
    pub hash: String,
    /// Pin type.
    pub pin_type: PinType,
}

/// Pin types supported by Iroh.
///
/// Note: Iroh uses Tags to protect blobs from the garbage collector.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PinType {
    /// Direct pin (Tag directly on the blob).
    Direct,
    /// Recursive pin (Tag + all referenced blobs).
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

/// Iroh store statistics (iroh-blobs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoStats {
    /// Number of blobs in the store.
    pub num_objects: u64,
    /// Total size in bytes.
    pub repo_size: u64,
    /// Path of the Iroh data store.
    pub repo_path: String,
    /// Guardian DB version.
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

/// Bandwidth information.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BandwidthStats {
    /// Bytes sent.
    pub total_out: u64,
    /// Bytes received.
    pub total_in: u64,
    /// Send rate (bytes/sec).
    pub rate_out: f64,
    /// Receive rate (bytes/sec).
    pub rate_in: f64,
}

/// Guardian DB version information (using Iroh).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VersionInfo {
    /// Guardian DB version.
    pub version: String,
    /// Build commit hash.
    pub commit: String,
    /// Repository version.
    pub repo: String,
    /// Operating system.
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
        // Example hash (hex format).
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
        let secret = SecretKey::generate();
        let node_id = secret.public();
        let info = NodeInfo::mock(node_id);

        assert_eq!(info.id, node_id);
        assert!(info.is_mock());
        assert!(info.agent_version.contains("guardian-db"));
    }

    #[test]
    fn test_pubsub_message() {
        use iroh::SecretKey;
        let secret = SecretKey::generate();
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
        let secret = SecretKey::generate();
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
