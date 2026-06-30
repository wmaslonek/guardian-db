//! Automatic and secure exchange of `DocTicket` between peers over authenticated QUIC.
//!
//! iroh-docs-based stores (KeyValue/Document) replicate by sharing the **same
//! namespace**. To avoid manually exchanging the ticket (capability), this module implements a
//! request/response protocol over a dedicated ALPN:
//!
//! - **Responder** ([`TicketProtocolHandler`]): receives the requested store address, identifies
//!   the peer by its iroh public key (authenticated by the QUIC TLS) and consults the store's
//!   `AccessController`. It only delivers a ticket if the peer is authorized, and the ticket's
//!   **capability matches the peer's role**: write-authorized peers receive a write ticket
//!   (carrying the namespace secret), read-only peers receive a read ticket (namespace public
//!   key only, no write secret).
//! - **Requester** ([`request_ticket`]): opens a connection on the ALPN, sends the address and
//!   receives the ticket (or a denial).
//!
//! The iroh QUIC connection authenticates the peer by its public key (`EndpointId`), so the
//! decision to grant the ticket is made against an authenticated party — the node identity
//! cannot be forged. Because a read-only peer never receives the namespace secret, it is
//! **cryptographically** unable to produce entries that other peers will accept, even if its
//! software is compromised.

use crate::access_control::traits::AccessController;
use crate::guardian::error::{GuardianError, Result};
use iroh::endpoint::{Connection, Endpoint};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{EndpointId as NodeId, PublicKey};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// Dedicated ALPN for the GuardianDB ticket exchange protocol.
pub const TICKET_ALPN: &[u8] = b"/guardian-db/ticket/1";

/// Responder response: ticket granted.
const RESP_GRANTED: u8 = 1;
/// Responder response: access denied.
const RESP_DENIED: u8 = 0;

/// Capability granted to a requester, derived from its authenticated role in the ACL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantedMode {
    /// Read-only: the peer receives a ticket without the namespace write secret.
    Read,
    /// Read-write: the peer receives a ticket carrying the namespace write secret.
    Write,
}

/// Provides a store's tickets and the access controller that decides who can obtain which.
///
/// Two pre-generated tickets are held so the responder can hand out the capability matching
/// the requester's authenticated role without minting tickets on the fly:
/// - `write_ticket` carries the namespace secret (`ShareMode::Write`);
/// - `read_ticket` carries only the namespace public key (`ShareMode::Read`).
#[derive(Clone)]
pub struct TicketProvider {
    /// Serialized read-only `DocTicket` (no write secret).
    pub read_ticket: String,
    /// Serialized read-write `DocTicket` (carries the namespace secret).
    pub write_ticket: String,
    /// The store's access controller, consulted to authorize the requester.
    pub access_controller: Arc<dyn AccessController>,
}

/// Registry of stores that can provide tickets, indexed by the store address.
pub type TicketRegistry = Arc<RwLock<HashMap<String, TicketProvider>>>;

/// Creates an empty registry.
pub fn new_registry() -> TicketRegistry {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Protocol handler (responder side) registered on the Router via [`TICKET_ALPN`].
#[derive(Clone)]
pub struct TicketProtocolHandler {
    registry: TicketRegistry,
}

impl std::fmt::Debug for TicketProtocolHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TicketProtocolHandler")
            .finish_non_exhaustive()
    }
}

impl TicketProtocolHandler {
    pub fn new(registry: TicketRegistry) -> Self {
        Self { registry }
    }

    /// Decides whether the `requester` is authorized to obtain the ticket for `address` and returns the payload.
    async fn resolve(&self, address: &str, requester: PublicKey) -> Vec<u8> {
        let provider = {
            let reg = self.registry.read().await;
            reg.get(address).cloned()
        };

        let Some(provider) = provider else {
            debug!(address, "Ticket requested for unknown store — denied");
            return vec![RESP_DENIED];
        };

        match authorized_mode(&*provider.access_controller, requester).await {
            Some(mode) => {
                let ticket = match mode {
                    GrantedMode::Write => &provider.write_ticket,
                    GrantedMode::Read => &provider.read_ticket,
                };
                debug!(address, peer = %requester.fmt_short(), ?mode, "Ticket granted");
                let mut out = Vec::with_capacity(ticket.len() + 1);
                out.push(RESP_GRANTED);
                out.extend_from_slice(ticket.as_bytes());
                out
            }
            None => {
                warn!(address, peer = %requester.fmt_short(), "Ticket denied by the access controller");
                vec![RESP_DENIED]
            }
        }
    }
}

impl ProtocolHandler for TicketProtocolHandler {
    async fn accept(&self, connection: Connection) -> std::result::Result<(), AcceptError> {
        // The remote public key is authenticated by the QUIC TLS handshake.
        let requester = connection.remote_id();

        let (mut send, mut recv) = connection.accept_bi().await?;

        // Request = store address (UTF-8 string), limited to a reasonable size.
        let req = recv
            .read_to_end(4096)
            .await
            .map_err(AcceptError::from_err)?;
        let address = String::from_utf8_lossy(&req).to_string();

        let response = self.resolve(&address, requester).await;

        send.write_all(&response)
            .await
            .map_err(AcceptError::from_err)?;
        send.finish().map_err(AcceptError::from_err)?;

        // Ensure the data is delivered before closing.
        connection.closed().await;
        Ok(())
    }
}

/// Determines which capability (if any) the peer (`requester`, iroh key) may obtain.
///
/// Write has precedence over read: a peer authorized for "write" receives [`GrantedMode::Write`]
/// (namespace secret); a peer authorized only for "read" receives [`GrantedMode::Read`]
/// (namespace public key, no secret). A role grants the peer if it contains the `*` wildcard
/// (public store) or the peer's hex key. Returns `None` (deny) otherwise.
async fn authorized_mode(acl: &dyn AccessController, requester: PublicKey) -> Option<GrantedMode> {
    let requester_hex = hex::encode(requester.as_bytes());
    let role_grants = |keys: Vec<String>| {
        keys.iter().any(|k| k == "*") || keys.iter().any(|k| k == &requester_hex)
    };

    // Check "write" first so write-authorized peers always get the stronger capability.
    if let Ok(keys) = acl.get_authorized_by_role("write").await
        && role_grants(keys)
    {
        return Some(GrantedMode::Write);
    }
    if let Ok(keys) = acl.get_authorized_by_role("read").await
        && role_grants(keys)
    {
        return Some(GrantedMode::Read);
    }
    None
}

/// Requests a peer's `DocTicket` for the store at `address`, over the [`TICKET_ALPN`].
///
/// Returns `Ok(Some(ticket))` if granted, `Ok(None)` if denied/unavailable.
pub async fn request_ticket(
    endpoint: &Endpoint,
    peer: NodeId,
    address: &str,
) -> Result<Option<String>> {
    let connection = endpoint
        .connect(peer, TICKET_ALPN)
        .await
        .map_err(|e| GuardianError::Other(format!("Failed to connect for ticket: {}", e)))?;

    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|e| GuardianError::Other(format!("Failed to open ticket stream: {}", e)))?;

    send.write_all(address.as_bytes())
        .await
        .map_err(|e| GuardianError::Other(format!("Failed to send ticket request: {}", e)))?;
    send.finish()
        .map_err(|e| GuardianError::Other(format!("Failed to finish ticket stream: {}", e)))?;

    let resp = recv
        .read_to_end(64 * 1024)
        .await
        .map_err(|e| GuardianError::Other(format!("Failed to read ticket response: {}", e)))?;

    connection.close(0u32.into(), b"done");

    match resp.first() {
        Some(&RESP_GRANTED) if resp.len() > 1 => {
            Ok(Some(String::from_utf8_lossy(&resp[1..]).to_string()))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access_control::acl_simple::SimpleAccessController;
    use std::collections::HashMap;

    /// Generates an arbitrary iroh public key to simulate an authenticated requester.
    fn random_public_key() -> PublicKey {
        iroh::SecretKey::generate().public()
    }

    fn acl_with(role: &str, keys: Vec<&str>) -> Arc<dyn AccessController> {
        let mut map = HashMap::new();
        map.insert(
            role.to_string(),
            keys.into_iter().map(String::from).collect(),
        );
        Arc::new(SimpleAccessController::new(Some(map))) as Arc<dyn AccessController>
    }

    // ─── authorized_mode ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn wildcard_write_grants_write_to_any_peer() {
        let acl = acl_with("write", vec!["*"]);
        assert_eq!(
            authorized_mode(&*acl, random_public_key()).await,
            Some(GrantedMode::Write)
        );
    }

    #[tokio::test]
    async fn wildcard_read_grants_read_to_any_peer() {
        let acl = acl_with("read", vec!["*"]);
        assert_eq!(
            authorized_mode(&*acl, random_public_key()).await,
            Some(GrantedMode::Read)
        );
    }

    #[tokio::test]
    async fn read_only_peer_never_gets_write() {
        // Peer is in "read" but NOT in "write": must receive Read, never Write.
        let peer = random_public_key();
        let peer_hex = hex::encode(peer.as_bytes());
        let mut map = HashMap::new();
        map.insert(
            "write".to_string(),
            vec![hex::encode(random_public_key().as_bytes())],
        );
        map.insert("read".to_string(), vec![peer_hex]);
        let acl = Arc::new(SimpleAccessController::new(Some(map))) as Arc<dyn AccessController>;
        assert_eq!(authorized_mode(&*acl, peer).await, Some(GrantedMode::Read));
    }

    #[tokio::test]
    async fn write_precedence_over_read() {
        // Peer listed in both roles must get the stronger capability.
        let peer = random_public_key();
        let peer_hex = hex::encode(peer.as_bytes());
        let mut map = HashMap::new();
        map.insert("write".to_string(), vec![peer_hex.clone()]);
        map.insert("read".to_string(), vec![peer_hex]);
        let acl = Arc::new(SimpleAccessController::new(Some(map))) as Arc<dyn AccessController>;
        assert_eq!(authorized_mode(&*acl, peer).await, Some(GrantedMode::Write));
    }

    #[tokio::test]
    async fn specific_authorized_key_gets_write() {
        let peer = random_public_key();
        let peer_hex = hex::encode(peer.as_bytes());
        let acl = acl_with("write", vec![peer_hex.as_str()]);
        assert_eq!(authorized_mode(&*acl, peer).await, Some(GrantedMode::Write));
    }

    #[tokio::test]
    async fn unknown_key_is_denied_when_no_wildcard() {
        // The ACL authorizes a different key, not the requester's → denied.
        let other_hex = hex::encode(random_public_key().as_bytes());
        let acl = acl_with("write", vec![other_hex.as_str()]);
        assert_eq!(authorized_mode(&*acl, random_public_key()).await, None);
    }

    #[tokio::test]
    async fn empty_acl_denies() {
        let acl = acl_with("write", vec![]);
        assert_eq!(authorized_mode(&*acl, random_public_key()).await, None);
    }

    // ─── TicketProtocolHandler::resolve ──────────────────────────────────────

    #[tokio::test]
    async fn resolve_unknown_store_is_denied() {
        let handler = TicketProtocolHandler::new(new_registry());
        let resp = handler.resolve("does-not-exist", random_public_key()).await;
        assert_eq!(resp, vec![RESP_DENIED]);
    }

    #[tokio::test]
    async fn resolve_grants_write_ticket_to_write_peer() {
        let registry = new_registry();
        registry.write().await.insert(
            "shared-kv".to_string(),
            TicketProvider {
                read_ticket: "read-ticket-xyz".to_string(),
                write_ticket: "write-ticket-xyz".to_string(),
                access_controller: acl_with("write", vec!["*"]),
            },
        );
        let handler = TicketProtocolHandler::new(registry);

        let resp = handler.resolve("shared-kv", random_public_key()).await;
        assert_eq!(resp.first(), Some(&RESP_GRANTED));
        assert_eq!(&resp[1..], b"write-ticket-xyz");
    }

    #[tokio::test]
    async fn resolve_grants_read_ticket_to_read_only_peer() {
        // The crux of the read-only guarantee: a read-only peer receives the read ticket,
        // so the namespace write secret never leaves this node for that peer.
        let peer = random_public_key();
        let peer_hex = hex::encode(peer.as_bytes());
        let mut map = HashMap::new();
        map.insert("read".to_string(), vec![peer_hex]);
        let acl = Arc::new(SimpleAccessController::new(Some(map))) as Arc<dyn AccessController>;

        let registry = new_registry();
        registry.write().await.insert(
            "shared-kv".to_string(),
            TicketProvider {
                read_ticket: "read-ticket-xyz".to_string(),
                write_ticket: "write-ticket-xyz".to_string(),
                access_controller: acl,
            },
        );
        let handler = TicketProtocolHandler::new(registry);

        let resp = handler.resolve("shared-kv", peer).await;
        assert_eq!(resp.first(), Some(&RESP_GRANTED));
        // Must be the READ ticket — the write ticket must NOT leak to a read-only peer.
        assert_eq!(&resp[1..], b"read-ticket-xyz");
    }

    #[tokio::test]
    async fn resolve_denies_unauthorized_peer() {
        let registry = new_registry();
        let other_hex = hex::encode(random_public_key().as_bytes());
        registry.write().await.insert(
            "private-kv".to_string(),
            TicketProvider {
                read_ticket: "secret-read-ticket".to_string(),
                write_ticket: "secret-write-ticket".to_string(),
                access_controller: acl_with("write", vec![other_hex.as_str()]),
            },
        );
        let handler = TicketProtocolHandler::new(registry);

        // Requester differs from the authorized one → denied, and no ticket leaks.
        let resp = handler.resolve("private-kv", random_public_key()).await;
        assert_eq!(resp, vec![RESP_DENIED]);
    }
}
