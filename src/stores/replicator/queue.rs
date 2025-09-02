use crate::ipfs_log::entry::Entry;
use cid::Cid;
use std::collections::VecDeque;

// equivalente a processItem interface em go
/// Um trait para itens que podem ser colocados na fila de processamento.
/// Cada item deve ser capaz de fornecer um `Cid`.
pub trait ProcessItem: Send + Sync {
    fn get_hash(&self) -> Cid;
}

// equivalente a processEntry struct em go
/// Um item da fila que contém uma entrada de log completa.
#[derive(Debug, Clone)]
pub struct ProcessEntry {
    entry: Entry,
}

// equivalente a newProcessEntry em go
impl ProcessEntry {
    /// Cria um novo item de processo a partir de uma entrada de log.
    pub fn new(entry: Entry) -> Self {
        Self { entry }
    }

    /// Retorna uma referência à entrada.
    pub fn entry(&self) -> &Entry {
        &self.entry
    }
}

impl ProcessItem for ProcessEntry {
    fn get_hash(&self) -> Cid {
        // Converte o hash string para Cid
        self.entry.hash().parse().unwrap_or_else(|_| {
            // Fallback: cria um CID vazio se o parsing falhar
            cid::Cid::default()
        })
    }
}

// equivalente a processHash struct em go
/// Um item da fila que contém apenas o hash (Cid) de uma entrada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessHash {
    hash: Cid,
}

// equivalente a newProcessHash em go
impl ProcessHash {
    /// Cria um novo item de processo a partir de um hash.
    pub fn new(hash: Cid) -> Self {
        Self { hash }
    }

    /// Retorna o hash.
    pub fn hash(&self) -> Cid {
        self.hash
    }
}

impl ProcessItem for ProcessHash {
    fn get_hash(&self) -> Cid {
        self.hash
    }
}

// Enum para representar diferentes tipos de itens na fila
/// Uma representação eficiente que evita boxing desnecessário.
#[derive(Debug, Clone)]
pub enum ProcessQueueItem {
    Entry(ProcessEntry),
    Hash(ProcessHash),
}

impl ProcessItem for ProcessQueueItem {
    fn get_hash(&self) -> Cid {
        match self {
            ProcessQueueItem::Entry(entry) => entry.get_hash(),
            ProcessQueueItem::Hash(hash) => hash.get_hash(),
        }
    }
}

// equivalente a processQueue em go
/// Uma fila FIFO que armazena itens a serem replicados.
/// Esta implementação usa um `VecDeque` para remoções e adições eficientes.
/// Thread-safety deve ser garantida externamente (usando Mutex, etc).
#[derive(Debug, Default)]
pub struct ProcessQueue {
    items: VecDeque<ProcessQueueItem>,
}

impl ProcessQueue {
    /// Cria uma nova fila de processamento vazia.
    pub fn new() -> Self {
        Self::default()
    }

    // equivalente a Add em go
    /// Adiciona um item ao final da fila.
    pub fn add(&mut self, item: ProcessQueueItem) {
        self.items.push_back(item);
    }

    /// Adiciona uma entrada ao final da fila.
    pub fn add_entry(&mut self, entry: Entry) {
        self.add(ProcessQueueItem::Entry(ProcessEntry::new(entry)));
    }

    /// Adiciona um hash ao final da fila.
    pub fn add_hash(&mut self, hash: Cid) {
        self.add(ProcessQueueItem::Hash(ProcessHash::new(hash)));
    }

    // equivalente a Next em go
    /// Remove e retorna o primeiro item da fila.
    /// Retorna `None` se a fila estiver vazia.
    pub fn next(&mut self) -> Option<ProcessQueueItem> {
        self.items.pop_front()
    }

    // equivalente a GetQueue em go
    /// Retorna uma referência imutável para a fila interna.
    pub fn get_queue(&self) -> &VecDeque<ProcessQueueItem> {
        &self.items
    }

    // equivalente a Len em go
    /// Retorna o número de itens na fila.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Verifica se a fila está vazia.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Remove todos os itens da fila.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Retorna uma referência ao primeiro item sem removê-lo.
    pub fn peek(&self) -> Option<&ProcessQueueItem> {
        self.items.front()
    }
}
