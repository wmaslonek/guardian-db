use crate::error::Result;
use crate::ipfs_core_api::client::IpfsClient;
use crate::ipfs_log::access_controller::LogEntry;
use crate::ipfs_log::identity::Identity;
use crate::ipfs_log::identity_provider::IdentityProvider;
use cid::Cid;
use libipld::cbor::DagCborCodec;
use libipld::codec::Codec;
use libipld::multihash::{Code, Multihash, MultihashDigest};
use libipld::{Cid as IpldCid, Ipld as IpldType};
use std::collections::{BTreeMap, HashMap};

/// Trait que representa uma entrada no log IPFS.
pub trait IPFSLogEntry: LogEntry {
    fn new() -> Self
    where
        Self: Sized;
    fn copy(&self) -> Self
    where
        Self: Sized;
    fn get_log_id(&self) -> String;
    fn get_next(&self) -> Vec<Cid>;
    fn get_refs(&self) -> Vec<Cid>;
    fn get_v(&self) -> u64;
    fn get_key(&self) -> Vec<u8>;
    fn get_sig(&self) -> Vec<u8>;
    fn get_hash(&self) -> Cid;
    fn get_clock(&self) -> Box<dyn IPFSLogLamportClock>;
    fn get_additional_data(&self) -> HashMap<String, String>;
    fn set_payload(&mut self, payload: Vec<u8>);
    fn set_log_id(&mut self, id: String);
    fn set_next(&mut self, next: Vec<Cid>);
    fn set_refs(&mut self, refs: Vec<Cid>);
    fn set_v(&mut self, v: u64);
    fn set_key(&mut self, key: Vec<u8>);
    fn set_sig(&mut self, sig: Vec<u8>);
    fn set_identity(&mut self, identity: Identity);
    fn set_hash(&mut self, hash: Cid);
    fn set_clock(&mut self, clock: Box<dyn IPFSLogLamportClock>);
    fn set_additional_data_value(&mut self, key: String, value: String);
    fn is_valid(&self) -> bool;
    fn verify(
        &self,
        identity: &dyn IdentityProvider,
        io: &dyn IO,
    ) -> std::result::Result<(), String>;
    fn equals(&self, b: &dyn IPFSLogEntry) -> bool;
    fn is_parent(&self, b: &dyn IPFSLogEntry) -> bool;
    fn defined(&self) -> bool;
}

/// Trait que representa um relógio de Lamport para o IPFS Log.
pub trait IPFSLogLamportClock {
    fn new() -> Self
    where
        Self: Sized;
    fn defined(&self) -> bool;

    fn get_id(&self) -> Vec<u8>;
    fn get_time(&self) -> i32;

    fn set_id(&mut self, id: Vec<u8>);
    fn set_time(&mut self, time: i32);

    fn tick(&mut self) -> Self
    where
        Self: Sized;
    fn merge(&mut self, clock: &dyn IPFSLogLamportClock) -> Self
    where
        Self: Sized;
    fn compare(&self, b: &dyn IPFSLogLamportClock) -> i32;
}

/// Trait que abstrai operações de IO sobre IPFS.
pub trait IO {
    fn write(
        &self,
        ipfs: &IpfsClient,
        obj: &dyn std::any::Any,
        opts: Option<&WriteOpts>,
    ) -> std::result::Result<Cid, String>;

    fn read(&self, ipfs: &IpfsClient, cid: Cid) -> std::result::Result<Box<dyn Node>, String>;

    fn decode_raw_entry(
        &self,
        node: Box<dyn Node>,
        hash: Cid,
        p: &dyn IdentityProvider,
    ) -> std::result::Result<Box<dyn IPFSLogEntry>, String>;

    fn decode_raw_json_log(&self, node: Box<dyn Node>) -> std::result::Result<JSONLog, String>;
}

/// Representa um log em formato JSON.
#[derive(Debug, Clone)]
pub struct JSONLog {
    pub id: String,
    pub heads: Vec<Cid>,
}

/// Opções para escrita no IPFS Log.
#[derive(Debug, Clone)]
pub struct WriteOpts {
    pub pin: bool,
    pub encrypted_links: Option<String>,
    pub encrypted_links_nonce: Option<String>,
}

/// Trait que abstrai um nó IPLD genérico.
pub trait Node: Send + Sync {
    /// Retorna o CID do nó.
    fn cid(&self) -> IpldCid;

    /// Retorna os bytes crus do bloco.
    fn raw_data(&self) -> Vec<u8>;

    /// Resolve um caminho dentro do nó (ex.: "field1/field2").
    fn resolve(&self, path: &str) -> Option<Box<dyn Node>>;

    /// Retorna os dados IPLD internos (mapa, lista, primitivo).
    fn ipld(&self) -> &IpldType;
}

/// Implementação de Node sobre dados IPLD
#[derive(Clone)]
pub struct IpldNode {
    cid: IpldCid,
    data: IpldType,
    raw_data: Vec<u8>,
}

impl IpldNode {
    /// Cria um novo nó IPLD vazio (mapa)
    pub fn new() -> Result<Self> {
        let ipld = IpldType::Map(BTreeMap::new());
        let encoded = DagCborCodec
            .encode(&ipld)
            .map_err(|e| format!("CBOR encoding error: {}", e))?;
        let digest = Code::Sha2_256.digest(&encoded);
        let hash = Multihash::wrap(Code::Sha2_256.into(), digest.digest()).unwrap();
        let cid = IpldCid::new_v1(DagCborCodec.into(), hash);
        Ok(Self {
            cid,
            data: ipld,
            raw_data: encoded,
        })
    }

    /// Cria um nó a partir de um Ipld existente
    pub fn from_ipld(ipld: IpldType) -> Result<Self> {
        let encoded = DagCborCodec
            .encode(&ipld)
            .map_err(|e| format!("CBOR encoding error: {}", e))?;
        let digest = Code::Sha2_256.digest(&encoded);
        let hash = Multihash::wrap(Code::Sha2_256.into(), digest.digest()).unwrap();
        let cid = IpldCid::new_v1(DagCborCodec.into(), hash);
        Ok(Self {
            cid,
            data: ipld,
            raw_data: encoded,
        })
    }
}

impl Node for IpldNode {
    fn cid(&self) -> IpldCid {
        self.cid
    }

    fn raw_data(&self) -> Vec<u8> {
        self.raw_data.clone()
    }

    fn resolve(&self, path: &str) -> Option<Box<dyn Node>> {
        let mut current = &self.data;
        for key in path.split('/') {
            match current {
                IpldType::Map(map) => {
                    current = map.get(key)?;
                }
                _ => return None,
            }
        }
        IpldNode::from_ipld(current.clone())
            .ok()
            .map(|n| Box::new(n) as Box<dyn Node>)
    }

    fn ipld(&self) -> &IpldType {
        &self.data
    }
}
