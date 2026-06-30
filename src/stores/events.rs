use crate::address::Address;
use crate::log::entry::Entry;
use iroh::EndpointId as NodeId;
use iroh_blobs::Hash;
use std::sync::Arc;

/// Store lifecycle events emitted through the store's event bus.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Write(EventWrite),
    Ready(EventReady),
    ReplicateProgress(EventReplicateProgress),
    Load(EventLoad),
    LoadProgress(EventLoadProgress),
    Replicated(EventReplicated),
    Replicate(EventReplicate),
    NewPeer(EventNewPeer),
    Reset(EventReset),
}

/// An event emitted when replication of an entry starts.
#[derive(Clone)]
pub struct EventReplicate {
    pub address: Arc<dyn Address + Send + Sync>,
    pub hash: Hash,
}

impl EventReplicate {
    pub fn new(address: Arc<dyn Address + Send + Sync>, hash: Hash) -> Self {
        Self { address, hash }
    }
}

impl std::fmt::Debug for EventReplicate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventReplicate")
            .field("address", &format!("{}", self.address))
            .field("hash", &self.hash)
            .finish()
    }
}

impl PartialEq for EventReplicate {
    fn eq(&self, other: &Self) -> bool {
        self.address.equals(other.address.as_ref()) && self.hash == other.hash
    }
}

/// An event containing the current replication progress.
#[derive(Clone)]
pub struct EventReplicateProgress {
    pub max: i32,
    pub progress: i32,
    pub address: Arc<dyn Address + Send + Sync>,
    pub hash: Hash,
    pub entry: Entry,
}

impl std::fmt::Debug for EventReplicateProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventReplicateProgress")
            .field("max", &self.max)
            .field("progress", &self.progress)
            .field("address", &format!("{}", self.address))
            .field("hash", &self.hash)
            .field("entry", &self.entry)
            .finish()
    }
}

impl PartialEq for EventReplicateProgress {
    fn eq(&self, other: &Self) -> bool {
        self.max == other.max
            && self.progress == other.progress
            && self.address.equals(other.address.as_ref())
            && self.hash == other.hash
            && self.entry == other.entry
    }
}

impl EventReplicateProgress {
    pub fn new(
        addr: Arc<dyn Address + Send + Sync>,
        h: Hash,
        e: Entry,
        max: i32,
        progress: i32,
    ) -> Self {
        Self {
            max,
            progress,
            address: addr,
            hash: h,
            entry: e,
        }
    }
}

/// An event sent when data has been replicated.
#[derive(Clone)]
pub struct EventReplicated {
    pub address: Arc<dyn Address + Send + Sync>,
    pub log_length: usize,
    pub entries: Vec<Entry>,
}

impl std::fmt::Debug for EventReplicated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventReplicated")
            .field("address", &format!("{}", self.address))
            .field("log_length", &self.log_length)
            .field("entries", &self.entries)
            .finish()
    }
}

impl PartialEq for EventReplicated {
    fn eq(&self, other: &Self) -> bool {
        self.address.equals(other.address.as_ref())
            && self.log_length == other.log_length
            && self.entries == other.entries
    }
}

impl EventReplicated {
    pub fn new(
        addr: Arc<dyn Address + Send + Sync>,
        entries: Vec<Entry>,
        log_length: usize,
    ) -> Self {
        Self {
            address: addr,
            log_length,
            entries,
        }
    }
}

/// An event sent when data has been loaded.
#[derive(Clone)]
pub struct EventLoad {
    pub address: Arc<dyn Address + Send + Sync>,
    pub heads: Vec<Entry>,
}

impl std::fmt::Debug for EventLoad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventLoad")
            .field("address", &format!("{}", self.address))
            .field("heads", &self.heads)
            .finish()
    }
}

impl PartialEq for EventLoad {
    fn eq(&self, other: &Self) -> bool {
        self.address.equals(other.address.as_ref()) && self.heads == other.heads
    }
}

impl EventLoad {
    pub fn new(addr: Arc<dyn Address + Send + Sync>, heads: Vec<Entry>) -> Self {
        Self {
            address: addr,
            heads,
        }
    }
}

/// An event containing the current load progress.
#[derive(Clone)]
pub struct EventLoadProgress {
    pub address: Arc<dyn Address + Send + Sync>,
    pub hash: Hash,
    pub entry: Entry,
    pub progress: i32,
    pub max: i32,
}

impl std::fmt::Debug for EventLoadProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventLoadProgress")
            .field("address", &format!("{}", self.address))
            .field("hash", &self.hash)
            .field("entry", &self.entry)
            .field("progress", &self.progress)
            .field("max", &self.max)
            .finish()
    }
}

impl PartialEq for EventLoadProgress {
    fn eq(&self, other: &Self) -> bool {
        self.address.equals(other.address.as_ref())
            && self.hash == other.hash
            && self.entry == other.entry
            && self.progress == other.progress
            && self.max == other.max
    }
}

impl EventLoadProgress {
    pub fn new(
        addr: Arc<dyn Address + Send + Sync>,
        h: Hash,
        e: Entry,
        progress: i32,
        max: i32,
    ) -> Self {
        Self {
            address: addr,
            hash: h,
            entry: e,
            progress,
            max,
        }
    }
}

/// An event sent when the store is ready.
#[derive(Clone)]
pub struct EventReady {
    pub address: Arc<dyn Address + Send + Sync>,
    pub heads: Vec<Entry>,
}

impl std::fmt::Debug for EventReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventReady")
            .field("address", &format!("{}", self.address))
            .field("heads", &self.heads)
            .finish()
    }
}

impl PartialEq for EventReady {
    fn eq(&self, other: &Self) -> bool {
        self.address.equals(other.address.as_ref()) && self.heads == other.heads
    }
}

impl EventReady {
    pub fn new(addr: Arc<dyn Address + Send + Sync>, heads: Vec<Entry>) -> Self {
        Self {
            address: addr,
            heads,
        }
    }
}

/// An event sent when something has been written.
#[derive(Clone)]
pub struct EventWrite {
    pub address: Arc<dyn Address + Send + Sync>,
    pub entry: Entry,
    pub heads: Vec<Entry>,
}

impl std::fmt::Debug for EventWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventWrite")
            .field("address", &format!("{}", self.address))
            .field("entry", &self.entry)
            .field("heads", &self.heads)
            .finish()
    }
}

impl PartialEq for EventWrite {
    fn eq(&self, other: &Self) -> bool {
        self.address.equals(other.address.as_ref())
            && self.entry == other.entry
            && self.heads == other.heads
    }
}

impl EventWrite {
    pub fn new(addr: Arc<dyn Address + Send + Sync>, e: Entry, heads: Vec<Entry>) -> Self {
        Self {
            address: addr,
            entry: e,
            heads,
        }
    }
}

/// An event sent when a new peer is discovered on the pubsub channel.
#[derive(Debug, Clone, PartialEq)]
pub struct EventNewPeer {
    pub peer: NodeId,
}

impl EventNewPeer {
    pub fn new(p: NodeId) -> Self {
        Self { peer: p }
    }
}

/// An event sent when the store is reset.
#[derive(Debug, Clone)]
pub struct EventReset {
    pub address: Arc<dyn Address + Send + Sync>,
    pub timestamp: u64,
}

impl PartialEq for EventReset {
    fn eq(&self, other: &Self) -> bool {
        self.address.equals(other.address.as_ref()) && self.timestamp == other.timestamp
    }
}

impl EventReset {
    pub fn new(address: Arc<dyn Address + Send + Sync>, timestamp: u64) -> Self {
        Self { address, timestamp }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::SimpleAddress;

    fn addr(path: &str) -> Arc<dyn Address + Send + Sync> {
        Arc::new(SimpleAddress::new(path)) as Arc<dyn Address + Send + Sync>
    }

    fn node_id() -> NodeId {
        iroh::SecretKey::generate().public()
    }

    #[test]
    fn event_replicate_holds_address_and_hash() {
        let h = Hash::new(b"some-content");
        let ev = EventReplicate::new(addr("/db/store"), h);
        assert_eq!(ev.hash, h);
        assert!(ev.address.equals(addr("/db/store").as_ref()));
        // Debug must not panic.
        let _ = format!("{:?}", ev);
    }

    #[test]
    fn event_replicate_partial_eq() {
        let h = Hash::new(b"x");
        let a = EventReplicate::new(addr("/db/s"), h);
        let b = EventReplicate::new(addr("/db/s"), h);
        let c = EventReplicate::new(addr("/db/outro"), h);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn event_new_peer_roundtrip() {
        let id = node_id();
        let ev = EventNewPeer::new(id);
        assert_eq!(ev.peer, id);
        let _ = format!("{:?}", EventNewPeer::new(id));
    }

    #[test]
    fn event_reset_partial_eq_considers_address_and_timestamp() {
        let a = EventReset::new(addr("/db/s"), 100);
        let same = EventReset::new(addr("/db/s"), 100);
        let diff_ts = EventReset::new(addr("/db/s"), 200);
        let diff_addr = EventReset::new(addr("/db/other"), 100);
        assert_eq!(a, same);
        assert_ne!(a, diff_ts);
        assert_ne!(a, diff_addr);
    }

    #[test]
    fn event_enum_wraps_variants() {
        let ev = Event::Replicate(EventReplicate::new(addr("/db/s"), Hash::new(b"h")));
        assert!(matches!(ev, Event::Replicate(_)));

        let peer_ev = Event::NewPeer(EventNewPeer::new(node_id()));
        assert!(matches!(peer_ev, Event::NewPeer(_)));

        // PartialEq derived for the enum.
        let r1 = Event::Reset(EventReset::new(addr("/db/s"), 1));
        let r2 = Event::Reset(EventReset::new(addr("/db/s"), 1));
        assert_eq!(r1, r2);
    }
}
