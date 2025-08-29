use crate::eqlabs_ipfs_log::access_controller::LogEntry;
use crate::eqlabs_ipfs_log::identity::Identity;
use crate::eqlabs_ipfs_log::lamport_clock::LamportClock;
use futures::{TryStreamExt, future::join_all};
use ipfs_api_backend_hyper::{IpfsApi, IpfsClient};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::cmp::Ordering;
use std::io::Cursor;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::runtime::Runtime;

#[derive(Debug)]
pub enum Error {
    Ipfs(ipfs_api_backend_hyper::Error),
    Json(serde_json::Error),
    Io(std::io::Error),
    Other(String),
}

impl From<ipfs_api_backend_hyper::Error> for Error {
    fn from(err: ipfs_api_backend_hyper::Error) -> Self {
        Error::Ipfs(err)
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::Json(err)
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Io(err)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Ipfs(e) => write!(f, "IPFS error: {}", e),
            Error::Json(e) => write!(f, "JSON error: {}", e),
            Error::Io(e) => write!(f, "IO error: {}", e),
            Error::Other(s) => write!(f, "Other error: {}", s),
        }
    }
}

impl std::error::Error for Error {}

/// A wrapper containing either a reference to an entry
/// or a hash as a string.
pub enum EntryOrHash<'a> {
    Entry(&'a Entry),
    Hash(String),
}

/// An entry containing data payload, a hash to locate it in [`IPFS`],
/// and pointers to its parents.
///
/// [`IPFS`]: https://ipfs.io
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Entry {
    pub hash: String,
    pub id: String,
    pub payload: String,
    pub next: Vec<String>,
    pub v: u32,
    pub clock: LamportClock,
    // Campo opcional para armazenar a identidade associada à entrada
    #[serde(skip)]
    pub identity: Option<Arc<Identity>>,
}

// Explicit Send + Sync implementations for Entry
unsafe impl Send for Entry {}
unsafe impl Sync for Entry {}

impl Entry {
    //very ad hoc
    #[doc(hidden)]
    pub fn empty() -> Entry {
        let s = "0000";
        Entry {
            hash: s.to_owned(),
            id: s.to_owned(),
            payload: s.to_owned(),
            next: Vec::new(),
            v: 0,
            clock: LamportClock::new(s),
            identity: None,
        }
    }

    #[doc(hidden)]
    pub fn new(
        identity: Identity,
        log_id: &str,
        data: &str,
        next: &[EntryOrHash],
        clock: Option<LamportClock>,
    ) -> Entry {
        //None filtering required?
        let next = next
            .iter()
            .map(|n| match n {
                EntryOrHash::Entry(e) => e.hash.to_owned(),
                EntryOrHash::Hash(h) => h.to_owned(),
            })
            .collect();
        Entry {
            //very much ad hoc
            hash: data.to_owned(),
            id: log_id.to_owned(),
            payload: data.to_owned(),
            next: next,
            v: 1,
            clock: clock.unwrap_or(LamportClock::new(identity.pub_key())),
            identity: Some(Arc::new(identity)),
        }
    }

    /// Locally creates an entry owned by `identity` .
    ///
    ///  The created entry is part of the [log] with the id `log_id`,
    /// holds payload of `data` and can be assigned to point to
    /// at most two parents with their hashes in `nexts`. Providing a
    /// [Lamport clock] via `clock` is optional.
    ///
    /// Returns a [reference-counting pointer] to the created entry.
    ///
    /// [log]: ../log/struct.Log.html
    /// [Lamport clock]: ../lamport_clock/struct.LamportClock.html
    /// [reference-counting pointer]: https://doc.rust-lang.org/std/rc/struct.Rc.html
    pub fn create(
        ipfs: &IpfsClient,
        identity: Identity,
        log_id: &str,
        data: &str,
        nexts: &[EntryOrHash],
        clock: Option<LamportClock>,
    ) -> Arc<Entry> {
        let mut e = Entry::new(identity, log_id, data, nexts, clock);
        let hash_result = Runtime::new()
            .unwrap()
            .block_on(Entry::multihash(ipfs, &e))
            .unwrap();
        e.hash = hash_result;
        Arc::new(e)
    }

    /// Stores `entry` in the IPFS client `ipfs` and returns a future containing its multihash.
    ///
    /// **N.B.** *At the moment stores the entry as JSON, not CBOR DAG.*
    pub async fn multihash(ipfs: &IpfsClient, entry: &Entry) -> Result<String, Error> {
        let e = json!({
            "hash": "null",
            "id": entry.id,
            "payload": entry.payload,
            "next": entry.next,
            "v": entry.v,
            "clock": entry.clock,
        })
        .to_string();

        match ipfs.add(Cursor::new(e)).await {
            Ok(response) => Ok(response.hash),
            Err(e) => Err(Error::from(e)),
        }
    }

    /// Returns the future containing the entry stored in the IPFS client `ipfs` with the multihash `hash`.
    ///
    /// **N.B.** *At the moment converts the entry from JSON, not CBOR DAG.*
    pub async fn from_multihash(ipfs: &IpfsClient, hash: &str) -> Result<Entry, Error> {
        let h = hash.to_owned();
        match ipfs
            .cat(hash)
            .map_ok(|chunk| chunk.to_vec())
            .try_concat()
            .await
        {
            Ok(bytes) => {
                match serde_json::from_str::<Entry>(std::str::from_utf8(&bytes).unwrap()) {
                    Ok(mut e) => {
                        e.hash = h;
                        Ok(e)
                    }
                    Err(json_err) => Err(Error::from(json_err)),
                }
            }
            Err(e) => Err(Error::from(e)),
        }
    }

    /// Fetches all the entries with the hashes in `hashes` and all their parents from the IPFS client `ipfs`.
    ///
    /// Returns a vector of entries.
    pub fn fetch_entries(ipfs: &IpfsClient, hashes: &[String]) -> Vec<Entry> {
        let hashes = Arc::new(Mutex::new(hashes.to_vec()));
        let mut es = Vec::new();
        loop {
            let mut result = Vec::new();
            while !hashes.lock().unwrap().is_empty() {
                let h = hashes.lock().unwrap().remove(0);
                let hashes_clone = hashes.clone();
                result.push(async move {
                    match Entry::from_multihash(ipfs, &h).await {
                        Ok(entry) => {
                            for n in &entry.next {
                                hashes_clone.lock().unwrap().push(n.to_owned());
                            }
                            Ok(entry)
                        }
                        Err(e) => Err(e),
                    }
                });
            }

            let new_entries: Vec<Entry> = Runtime::new()
                .unwrap()
                .block_on(join_all(result))
                .into_iter()
                .filter_map(|r| r.ok())
                .collect();

            es.extend(new_entries);

            if hashes.lock().unwrap().is_empty() {
                break;
            }
        }
        es
    }

    /// Returns the hash of the entry.
    pub fn hash(&self) -> &str {
        &self.hash
    }

    /// Sets the hash of the entry.
    pub fn set_hash(&mut self, hash: &str) {
        self.hash = hash.to_owned();
    }

    /// Returns the identifier of the entry that is the same as of the containing log.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the data payload of the entry.
    pub fn payload(&self) -> &str {
        &self.payload
    }

    /// Returns the hashes of the parents.
    ///
    /// The length of the returned slice is either:
    /// * 0 &mdash; no parents
    /// * 2 &mdash; two identical strings for one parent, two distinct strings for two different parents
    pub fn next(&self) -> &[String] {
        &self.next
    }

    /// Returns the Lamport clock of the entry.
    pub fn clock(&self) -> &LamportClock {
        &self.clock
    }

    /// Returns `true` if `e1` is the parent of `e2`, otherwise returns `false`.
    pub fn is_parent(e1: &Entry, e2: &Entry) -> bool {
        e2.next().iter().any(|x| x == e1.hash())
    }

    /// Returns a vector of pointers to all direct and indirect children of `entry` in `entries`.
    pub fn find_children(entry: &Entry, entries: &[Arc<Entry>]) -> Vec<Arc<Entry>> {
        let mut stack = Vec::new();
        let mut parent = entries.iter().find(|e| Entry::is_parent(entry, e));
        while let Some(p) = parent {
            stack.push(p.clone());
            let prev = p;
            parent = entries.iter().find(|e| Entry::is_parent(prev, e));
        }
        stack.sort_by(|a, b| a.clock().time().cmp(&b.clock().time()));
        stack
    }

    /// A sorting function to pick the more recently written entry.
    ///
    /// Uses [`sort_step_by_step`], resolving unsorted cases in the manner defined in it.
    ///
    /// Returns an ordering.
    ///
    /// [`sort_step_by_step`]: #method.sort_step_by_step
    pub fn last_write_wins(a: &Entry, b: &Entry) -> Ordering {
        Entry::sort_step_by_step(|_, _| Ordering::Less)(a, b)
    }

    /// A sorting function to pick the entry with the greater hash.
    ///
    /// Uses [`sort_step_by_step`], resolving unsorted cases in the manner defined in it.
    ///
    /// Returns an ordering.
    ///
    /// [`sort_step_by_step`]: #method.sort_step_by_step
    pub fn sort_by_entry_hash(a: &Entry, b: &Entry) -> Ordering {
        Entry::sort_step_by_step(|a, b| a.hash().cmp(&b.hash()))(a, b)
    }

    /// A sorting helper function to
    /// 1. first try to sort the two entries using `resolve`,
    /// 2. if still unsorted (equal), try to sort based on the Lamport clock identifiers of the respective entries,
    /// 3. sort by the Lamport clocks of the respective entries.
    ///
    /// Returns a closure that can be used as a sorting function.
    pub fn sort_step_by_step<F>(resolve: F) -> impl Fn(&Entry, &Entry) -> Ordering
    where
        F: 'static + Fn(&Entry, &Entry) -> Ordering,
    {
        Entry::sort_by_clocks(Entry::sort_by_clock_ids(resolve))
    }

    /// A sorting helper function to sort by the Lamport clocks of the respective entries.
    /// In the case the Lamport clocks are equal, tries to sort using `resolve`.
    ///
    /// Returns a closure that can be used as a sorting function.
    pub fn sort_by_clocks<F>(resolve: F) -> impl Fn(&Entry, &Entry) -> Ordering
    where
        F: 'static + Fn(&Entry, &Entry) -> Ordering,
    {
        move |a, b| {
            let mut diff = a.clock().cmp(&b.clock());
            if diff == Ordering::Equal {
                diff = resolve(a, b);
            }
            diff
        }
    }

    /// A sorting helper function to sort by the Lamport clock identifiers of the respective entries.
    /// In the case the Lamport clocks identifiers are equal, tries to sort using `resolve`.
    ///
    /// Returns a closure that can be used as a sorting function.
    pub fn sort_by_clock_ids<F>(resolve: F) -> impl Fn(&Entry, &Entry) -> Ordering
    where
        F: 'static + Fn(&Entry, &Entry) -> Ordering,
    {
        move |a, b| {
            let mut diff = a.clock().id().cmp(&b.clock().id());
            if diff == Ordering::Equal {
                diff = resolve(a, b);
            }
            diff
        }
    }

    /// A sorting helper function that forbids the sorting function `sort_fn` from
    /// producing unsorted (equal) cases.
    ///
    /// Returns a closure that behaves in the same way as `sort_fn`
    /// but panics if the two entries given as input are equal.
    pub fn no_zeroes<F>(sort_fn: F) -> impl Fn(&Entry, &Entry) -> Ordering
    where
        F: 'static + Fn(&Entry, &Entry) -> Ordering,
    {
        move |a, b| {
            let diff = sort_fn(a, b);
            if diff == Ordering::Equal {
                panic!(
                    "Your log's tiebreaker function {}",
                    "has returned zero and therefore cannot be"
                );
            }
            diff
        }
    }
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
    }
}

impl Eq for Entry {}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> Ordering {
        let diff = self.clock().cmp(other.clock());
        if diff == Ordering::Equal {
            self.clock().id().cmp(other.clock().id())
        } else {
            diff
        }
    }
}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// Implementação do trait LogEntry para Entry
impl LogEntry for Entry {
    fn get_payload(&self) -> &[u8] {
        self.payload.as_bytes()
    }

    fn get_identity(&self) -> &Identity {
        // Se temos uma identidade armazenada, a retornamos
        if let Some(ref identity_arc) = self.identity {
            return identity_arc.as_ref();
        }

        // Caso contrário, retornamos uma identidade padrão baseada no clock ID
        use crate::eqlabs_ipfs_log::identity::Signatures;
        use std::sync::OnceLock;

        static DEFAULT_IDENTITY: OnceLock<Identity> = OnceLock::new();
        DEFAULT_IDENTITY.get_or_init(|| {
            let signatures = Signatures::new("", "");
            Identity::new("unknown", "unknown", signatures)
        })
    }
}
