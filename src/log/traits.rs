use crate::log::access_control::LogEntry;
use crate::log::identity::Identity;
use crate::log::identity_provider::IdentityProvider;
use crate::p2p::network::client::IrohClient;
use iroh_blobs::Hash;
use std::collections::HashMap;

/// Trait que representa uma entrada no log iroh.
pub trait IrohLogEntry: LogEntry {
    fn new() -> Self
    where
        Self: Sized;
    fn copy(&self) -> Self
    where
        Self: Sized;
    fn get_log_id(&self) -> String;
    fn get_next(&self) -> Vec<Hash>;
    fn get_refs(&self) -> Vec<Hash>;
    fn get_v(&self) -> u64;
    fn get_key(&self) -> Vec<u8>;
    fn get_sig(&self) -> Vec<u8>;
    fn get_hash(&self) -> Hash;
    fn get_clock(&self) -> Box<dyn IrohLogLamportClock>;
    fn get_additional_data(&self) -> HashMap<String, String>;
    fn set_payload(&mut self, payload: Vec<u8>);
    fn set_log_id(&mut self, id: String);
    fn set_next(&mut self, next: Vec<Hash>);
    fn set_refs(&mut self, refs: Vec<Hash>);
    fn set_v(&mut self, v: u64);
    fn set_key(&mut self, key: Vec<u8>);
    fn set_sig(&mut self, sig: Vec<u8>);
    fn set_identity(&mut self, identity: Identity);
    fn set_hash(&mut self, hash: Hash);
    fn set_clock(&mut self, clock: Box<dyn IrohLogLamportClock>);
    fn set_additional_data_value(&mut self, key: String, value: String);
    fn is_valid(&self) -> bool;
    fn verify(
        &self,
        identity: &dyn IdentityProvider,
        io: &dyn IO,
    ) -> std::result::Result<(), String>;
    fn equals(&self, b: &dyn IrohLogEntry) -> bool;
    fn is_parent(&self, b: &dyn IrohLogEntry) -> bool;
    fn defined(&self) -> bool;
}

/// Trait que representa um relógio de Lamport para o iroh Log.
pub trait IrohLogLamportClock {
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
    fn merge(&mut self, clock: &dyn IrohLogLamportClock) -> Self
    where
        Self: Sized;
    fn compare(&self, b: &dyn IrohLogLamportClock) -> i32;
}

/// Trait que abstrai operações de IO sobre iroh.
pub trait IO {
    fn write(
        &self,
        iroh: &IrohClient,
        obj: &dyn std::any::Any,
        opts: Option<&WriteOpts>,
    ) -> std::result::Result<Hash, String>;

    fn read(&self, iroh: &IrohClient, hash: Hash) -> std::result::Result<Vec<u8>, String>;
}

/// Representa um log em formato JSON.
#[derive(Debug, Clone)]
pub struct JSONLog {
    pub id: String,
    pub heads: Vec<Hash>,
}

/// Opções para escrita no Log.
#[derive(Debug, Clone)]
pub struct WriteOpts {
    pub pin: bool,
    pub encrypted_links: Option<String>,
    pub encrypted_links_nonce: Option<String>,
}
