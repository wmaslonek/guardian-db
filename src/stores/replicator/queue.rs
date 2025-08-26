use cid::Cid;
use std::collections::VecDeque;
use crate::eqlabs_ipfs_log::iface::IPFSLogEntry; //equivalente ao go-ipfs-log/iface
// equivalente a processItem interface em go
/// Um trait para itens que podem ser colocados na fila de processamento.
/// Cada item deve ser capaz de fornecer um `Cid`.
pub trait ProcessItem: Send + Sync {
    fn get_hash(&self) -> Cid;
}

// equivalente a processEntry struct em go
/// Um item da fila que contém uma entrada de log completa.
pub struct ProcessEntry {
    entry: Box<dyn IPFSLogEntry + Send + Sync>,
}

// equivalente a newProcessEntry em go
impl ProcessEntry {
    /// Cria um novo item de processo a partir de uma entrada de log.
    pub fn new(entry: Box<dyn IPFSLogEntry + Send + Sync>) -> Box<dyn ProcessItem + Send + Sync> {
        Box::new(Self { entry })
    }
}

impl ProcessItem for ProcessEntry {
    fn get_hash(&self) -> Cid {
        self.entry.get_hash()
    }
}

unsafe impl Send for ProcessEntry {}
unsafe impl Sync for ProcessEntry {}

// equivalente a processHash struct em go
/// Um item da fila que contém apenas o hash (Cid) de uma entrada.
pub struct ProcessHash {
    hash: Cid,
}

// equivalente a newProcessHash em go
impl ProcessHash {
    /// Cria um novo item de processo a partir de um hash.
    pub fn new(hash: Cid) -> Box<dyn ProcessItem + Send + Sync> {
        Box::new(Self { hash })
    }
}

impl ProcessItem for ProcessHash {
    fn get_hash(&self) -> Cid {
        self.hash
    }
}

unsafe impl Send for ProcessHash {}
unsafe impl Sync for ProcessHash {}

// equivalente a processQueue em go
/// Uma fila FIFO que armazena itens a serem replicados.
/// Esta implementação usa um `VecDeque` para remoções e adições eficientes.
/// Assim como o original em Go, esta fila não é thread-safe.
#[derive(Default)]
pub struct ProcessQueue(VecDeque<Box<dyn ProcessItem + Send + Sync>>);

impl ProcessQueue {
    /// Cria uma nova fila de processamento vazia.
    pub fn new() -> Self {
        Self::default()
    }

    // equivalente a Add em go
    /// Adiciona um item ao final da fila.
    pub fn add(&mut self, item: Box<dyn ProcessItem + Send + Sync>) {
        self.0.push_back(item);
    }

    // equivalente a Next em go
    /// Remove e retorna o primeiro item da fila.
    /// Retorna `None` se a fila estiver vazia.
    pub fn next(&mut self) -> Option<Box<dyn ProcessItem + Send + Sync>> {
        self.0.pop_front()
    }

    // equivalente a GetQueue em go
    /// Retorna uma referência imutável para a fila interna.
    pub fn get_queue(&self) -> &VecDeque<Box<dyn ProcessItem + Send + Sync>> {
        &self.0
    }

    // equivalente a Len em go
    /// Retorna o número de itens na fila.
    pub fn len(&self) -> usize {
        self.0.len()
    }
}