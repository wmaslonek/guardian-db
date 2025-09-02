use crate::address::Address;
use crate::ipfs_log::entry::Entry;
use cid::Cid;
use libp2p::core::PeerId;
use std::sync::Arc;

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

// -----------------------------------------------------------------------------
// As traduções das structs de eventos e suas funções `New...`
// -----------------------------------------------------------------------------

/// equivalente a EventReplicate em go
#[derive(Clone)]
pub struct EventReplicate {
    pub address: Arc<dyn Address + Send + Sync>,
    pub hash: Cid,
}

impl EventReplicate {
    /// equivalente a NewEventReplicate em go
    pub fn new(address: Arc<dyn Address + Send + Sync>, hash: Cid) -> Self {
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

/// Um evento contendo o progresso atual da replicação.
/// equivalente a EventReplicateProgress em go
#[derive(Clone)]
pub struct EventReplicateProgress {
    pub max: i32,
    pub progress: i32,
    pub address: Arc<dyn Address + Send + Sync>,
    pub hash: Cid,
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
    /// equivalente a NewEventReplicateProgress em go
    pub fn new(
        addr: Arc<dyn Address + Send + Sync>,
        h: Cid,
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

/// Um evento enviado quando os dados foram replicados.
/// equivalente a EventReplicated em go
#[derive(Clone)]
pub struct EventReplicated {
    pub address: Arc<dyn Address + Send + Sync>,
    pub log_length: usize, // `usize` é o tipo idiomático para tamanhos e contagens
    pub entries: Vec<Entry>, // `[]Type` em Go torna-se `Vec<Type>` em Rust
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
    /// equivalente a NewEventReplicated em go
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

/// Um evento enviado quando os dados foram carregados.
/// equivalente a EventLoad em go
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
    /// equivalente a NewEventLoad em go
    pub fn new(addr: Arc<dyn Address + Send + Sync>, heads: Vec<Entry>) -> Self {
        Self {
            address: addr,
            heads,
        }
    }
}

/// equivalente a EventLoadProgress em go
#[derive(Clone)]
pub struct EventLoadProgress {
    pub address: Arc<dyn Address + Send + Sync>,
    pub hash: Cid,
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
    /// equivalente a NewEventLoadProgress em go
    pub fn new(
        addr: Arc<dyn Address + Send + Sync>,
        h: Cid,
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

/// Um evento enviado quando a store está pronta.
/// equivalente a EventReady em go
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
    /// equivalente a NewEventReady em go
    pub fn new(addr: Arc<dyn Address + Send + Sync>, heads: Vec<Entry>) -> Self {
        Self {
            address: addr,
            heads,
        }
    }
}

/// Um evento enviado quando algo foi escrito.
/// equivalente a EventWrite em go
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
    /// equivalente a NewEventWrite em go
    pub fn new(addr: Arc<dyn Address + Send + Sync>, e: Entry, heads: Vec<Entry>) -> Self {
        Self {
            address: addr,
            entry: e,
            heads,
        }
    }
}

/// Um evento enviado quando um novo peer é descoberto no canal pubsub.
/// equivalente a EventNewPeer em go
#[derive(Debug, Clone, PartialEq)]
pub struct EventNewPeer {
    pub peer: PeerId,
}

impl EventNewPeer {
    /// equivalente a NewEventNewPeer em go
    pub fn new(p: PeerId) -> Self {
        Self { peer: p }
    }
}

/// Um evento enviado quando a store é resetada.
/// equivalente a EventReset em go
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
    /// equivalente a NewEventReset em go
    pub fn new(address: Arc<dyn Address + Send + Sync>, timestamp: u64) -> Self {
        Self { address, timestamp }
    }
}
