use crate::log::access_control::LogEntry;
use crate::log::identity::Identity;
use crate::log::lamport_clock::LamportClock;
use crate::p2p::network::client::IrohClient;
use futures::future::join_all;
use iroh_blobs::Hash;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::cmp::Ordering;
use std::sync::Arc;
use std::sync::Mutex;

#[derive(Debug)]
pub enum Error {
    Iroh(crate::guardian::error::GuardianError),
    Json(serde_json::Error),
    Io(std::io::Error),
    Other(String),
}

impl From<crate::guardian::error::GuardianError> for Error {
    fn from(err: crate::guardian::error::GuardianError) -> Self {
        Error::Iroh(err)
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
            Error::Iroh(e) => write!(f, "iroh error: {}", e),
            Error::Json(e) => write!(f, "JSON error: {}", e),
            Error::Io(e) => write!(f, "IO error: {}", e),
            Error::Other(s) => write!(f, "Other error: {}", s),
        }
    }
}

impl std::error::Error for Error {}

/// A wrapper containing either a reference to an entry
/// or a hash.
pub enum EntryOrHash<'a> {
    Entry(&'a Entry),
    Hash(Hash),
}

/// An entry containing data payload, a hash to locate it in [`iroh`],
/// and pointers to its parents.
///
/// [`iroh`]: https://iroh.computer
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    #[serde(
        serialize_with = "serialize_hash",
        deserialize_with = "deserialize_hash"
    )]
    pub hash: Hash,
    pub id: String,
    /// Payload binário da entrada (migrado de String para Vec<u8> na Fase 3)
    pub payload: Vec<u8>,
    #[serde(
        serialize_with = "serialize_hash_vec",
        deserialize_with = "deserialize_hash_vec"
    )]
    pub next: Vec<Hash>,
    pub v: u32,
    pub clock: LamportClock,
    // Campo opcional para armazenar a identidade associada à entrada
    pub identity: Option<Arc<Identity>>,
}

// Funções auxiliares de serialização para Hash
fn serialize_hash<S>(hash: &Hash, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&hex::encode(hash.as_bytes()))
}

fn deserialize_hash<'de, D>(deserializer: D) -> Result<Hash, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
    if bytes.len() != 32 {
        return Err(serde::de::Error::custom("Invalid hash length"));
    }
    let mut array = [0u8; 32];
    array.copy_from_slice(&bytes);
    Ok(Hash::from(array))
}

fn serialize_hash_vec<S>(hashes: &Vec<Hash>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;
    let mut seq = serializer.serialize_seq(Some(hashes.len()))?;
    for hash in hashes {
        seq.serialize_element(&hex::encode(hash.as_bytes()))?;
    }
    seq.end()
}

fn deserialize_hash_vec<'de, D>(deserializer: D) -> Result<Vec<Hash>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let strings: Vec<String> = Vec::deserialize(deserializer)?;
    strings
        .iter()
        .map(|s| {
            let bytes = hex::decode(s).map_err(serde::de::Error::custom)?;
            if bytes.len() != 32 {
                return Err(serde::de::Error::custom("Invalid hash length"));
            }
            let mut array = [0u8; 32];
            array.copy_from_slice(&bytes);
            Ok(Hash::from(array))
        })
        .collect()
}

impl Entry {
    //very ad hoc
    #[doc(hidden)]
    pub fn empty() -> Entry {
        let s = "0000";
        Entry {
            hash: Hash::from([0u8; 32]),
            id: s.to_owned(),
            payload: s.as_bytes().to_vec(),
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
        data: &[u8],
        next: &[EntryOrHash],
        clock: Option<LamportClock>,
    ) -> Entry {
        //None filtering required?
        let next = next
            .iter()
            .map(|n| match n {
                EntryOrHash::Entry(e) => e.hash,
                EntryOrHash::Hash(h) => *h,
            })
            .collect();
        Entry {
            //very much ad hoc - hash será calculado depois
            hash: Hash::from([0u8; 32]),
            id: log_id.to_owned(),
            payload: data.to_vec(),
            next,
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
        client: &IrohClient,
        identity: Identity,
        log_id: &str,
        data: &[u8],
        nexts: &[EntryOrHash],
        clock: Option<LamportClock>,
    ) -> Arc<Entry> {
        let mut e = Entry::new(identity, log_id, data, nexts, clock);

        // Usa thread separada com novo Runtime para evitar "cannot start runtime from within a runtime"
        let client_clone = client.clone();
        let entry_clone = e.clone();
        let hash_result = std::thread::spawn(move || {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(async move { Entry::hash_entry(&client_clone, &entry_clone).await })
        })
        .join()
        .unwrap()
        .unwrap();

        e.hash = hash_result;
        Arc::new(e)
    }

    /// Stores `entry` in the Client `iroh` and returns a future containing its hash.
    ///
    /// **N.B.** *At the moment stores the entry as JSON, not CBOR DAG.*
    pub async fn hash_entry(client: &IrohClient, entry: &Entry) -> Result<Hash, Error> {
        let e = json!({
            "hash": "null",
            "id": entry.id,
            "payload": String::from_utf8_lossy(&entry.payload),
            "next": entry.next.iter().map(|h| hex::encode(h.as_bytes())).collect::<Vec<_>>(),
            "v": entry.v,
            "clock": entry.clock,
        })
        .to_string();

        match client.add_bytes(e.as_bytes().to_vec()).await {
            Ok(response) => {
                // Converte string hash para Hash
                let bytes = hex::decode(&response.hash)
                    .map_err(|e| Error::Other(format!("Failed to decode hash: {}", e)))?;
                if bytes.len() != 32 {
                    return Err(Error::Other("Invalid hash length".to_string()));
                }
                let mut array = [0u8; 32];
                array.copy_from_slice(&bytes);
                Ok(Hash::from(array))
            }
            Err(e) => Err(Error::from(e)),
        }
    }

    /// Returns the future containing the entry stored in the Client `iroh` with the hash `hash`.
    ///
    /// **N.B.** *At the moment converts the entry from postcard binary format.*
    pub async fn from_hash(client: &IrohClient, hash: &Hash) -> Result<Entry, Error> {
        let hash_str = hex::encode(hash.as_bytes());
        match client.cat_bytes(&hash_str).await {
            Ok(bytes) => match crate::guardian::serializer::deserialize::<Entry>(&bytes) {
                Ok(mut e) => {
                    e.hash = *hash;
                    Ok(e)
                }
                Err(err) => Err(Error::Other(format!("Deserialization error: {}", err))),
            },
            Err(e) => Err(Error::from(e)),
        }
    }

    /// Fetches all the entries with the hashes in `hashes` and all their parents from the Client `iroh`.
    ///
    /// Returns a vector of entries.
    pub fn fetch_entries(client: &IrohClient, hashes: &[Hash]) -> Vec<Entry> {
        let hashes = Arc::new(Mutex::new(hashes.to_vec()));
        let client = client.clone(); // Clone para evitar problemas de lifetime com thread
        let mut es = Vec::new();
        loop {
            let mut result = Vec::new();
            while !hashes.lock().unwrap().is_empty() {
                let h = hashes.lock().unwrap().remove(0);
                let hashes_clone = hashes.clone();
                let client_clone = client.clone();
                result.push(async move {
                    match Entry::from_hash(&client_clone, &h).await {
                        Ok(entry) => {
                            for n in &entry.next {
                                hashes_clone.lock().unwrap().push(*n);
                            }
                            Ok(entry)
                        }
                        Err(e) => Err(e),
                    }
                });
            }

            // Usa thread separada com novo Runtime para evitar "cannot start runtime from within a runtime"
            let new_entries: Vec<Entry> = std::thread::spawn(move || {
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(async move {
                        join_all(result)
                            .await
                            .into_iter()
                            .filter_map(|r| r.ok())
                            .collect::<Vec<Entry>>()
                    })
            })
            .join()
            .unwrap();

            es.extend(new_entries);

            if hashes.lock().unwrap().is_empty() {
                break;
            }
        }
        es
    }

    /// Returns the hash of the entry.
    pub fn hash(&self) -> &Hash {
        &self.hash
    }

    /// Sets the hash of the entry.
    pub fn set_hash(&mut self, hash: &Hash) {
        self.hash = *hash;
    }

    /// Returns the identifier of the entry that is the same as of the containing log.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the data payload of the entry as bytes.
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Convenience method to get payload as UTF-8 string (lossy conversion).
    pub fn payload_str(&self) -> String {
        String::from_utf8_lossy(&self.payload).to_string()
    }

    /// Returns the hashes of the parents.
    ///
    /// The length of the returned slice is either:
    /// * 0 &mdash; no parents
    /// * 2 &mdash; two identical hashes for one parent, two distinct hashes for two different parents
    pub fn next(&self) -> &[Hash] {
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
        stack.sort_by_key(|a| a.clock().time());
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
        Entry::sort_step_by_step(|a, b| a.hash().as_bytes().cmp(b.hash().as_bytes()))(a, b)
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
            let mut diff = a.clock().cmp(b.clock());
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
            let mut diff = a.clock().id().cmp(b.clock().id());
            if diff == Ordering::Equal {
                diff = resolve(a, b);
            }
            diff
        }
    }

    /// A sorting helper function that ensures the sorting function `sort_fn`
    /// produces a total order by falling back to hash comparison when entries
    /// are otherwise equal.
    ///
    /// Returns a closure that behaves in the same way as `sort_fn`
    /// but uses entry hash as the ultimate tiebreaker instead of panicking.
    /// Identical entries (same hash) are correctly treated as equal.
    pub fn no_zeroes<F>(sort_fn: F) -> impl Fn(&Entry, &Entry) -> Ordering
    where
        F: 'static + Fn(&Entry, &Entry) -> Ordering,
    {
        move |a, b| {
            // Identical entries (same hash) are truly equal
            if a.hash() == b.hash() {
                return Ordering::Equal;
            }
            let diff = sort_fn(a, b);
            if diff == Ordering::Equal {
                // Use hash comparison as ultimate tiebreaker for different entries
                a.hash().as_bytes().cmp(b.hash().as_bytes())
            } else {
                diff
            }
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
        &self.payload
    }

    fn get_identity(&self) -> &Identity {
        // Se temos uma identidade armazenada, a retornamos
        if let Some(ref identity_arc) = self.identity {
            return identity_arc.as_ref();
        }

        // Caso contrário, retornamos uma identidade padrão baseada no clock ID
        use crate::log::identity::Signatures;
        use std::sync::OnceLock;

        static DEFAULT_IDENTITY: OnceLock<Identity> = OnceLock::new();
        DEFAULT_IDENTITY.get_or_init(|| {
            let signatures = Signatures::new("", "");
            Identity::new("unknown", "unknown", signatures)
        })
    }
}
