use std::sync::Arc;
use tokio::sync::mpsc;
use crate::error::{GuardianError, Result};
use cid::Cid;
use crate::data_store::Datastore;
use crate::iface::{self, Store, EventLogStore, StreamOptions};
use crate::stores::base_store::base_store::BaseStore;
use crate::stores::operation::{operation, operation::Operation};
use crate::eqlabs_ipfs_log::{entry::Entry, identity::Identity};
use crate::kubo_core_api::IpfsClient;
use crate::{
    address::Address,
    stores::event_log_store::index::new_event_index,
};
use crate::pubsub::event::EventBus;

/// A implementação do trait `EventLogStore` para a nossa struct.
#[async_trait::async_trait]
impl EventLogStore for OrbitDBEventLogStore {
    /// Adiciona um novo dado ao log.
    async fn add(&mut self, data: Vec<u8>) -> std::result::Result<Operation, Self::Error> {
        let op = Operation::new(None, "ADD".to_string(), Some(data));
        let entry = self.basestore.add_operation(op, None).await?;
        let op_result = operation::parse_operation(entry)?;
        Ok(op_result)
    }

    /// Obtém uma entrada específica do log pelo seu CID.
    async fn get(&self, cid: Cid) -> std::result::Result<Operation, Self::Error> {
        self.get(cid).await.map_err(|e| GuardianError::Store(format!("Error getting operation: {}", e)))
    }

    /// Retorna uma lista de operações que ocorreram na store, com opções de filtro.
    async fn list(&self, options: Option<StreamOptions>) -> std::result::Result<Vec<Operation>, Self::Error> {
        self.list(options).await.map_err(|e| GuardianError::Store(format!("Error listing operations: {}", e)))
    }
}

pub struct OrbitDBEventLogStore {
    basestore: BaseStore,
}

// Implementação da trait Store (que é herdada por EventLogStore)
#[async_trait::async_trait]
impl Store for OrbitDBEventLogStore {
    type Error = GuardianError;

    #[allow(deprecated)]
    fn events(&self) -> &dyn crate::events::EmitterInterface {
        self.basestore.events()
    }

    async fn close(&mut self) -> std::result::Result<(), Self::Error> {
        // Temporary stub implementation to avoid Send issues
        // TODO: Implement proper close functionality that is Send-safe
        Ok(())
    }

    fn address(&self) -> &dyn crate::address::Address {
        // For now, we'll need to restructure this - returning a placeholder
        todo!("Address access needs proper implementation")
    }

    fn index(&self) -> &dyn crate::iface::StoreIndex<Error = GuardianError> {
        // Temporary placeholder - will need proper implementation
        todo!("Index implementation needed")
    }

    fn store_type(&self) -> &str {
        "eventlog"
    }

    fn replication_status(&self) -> crate::stores::replicator::replication_info::ReplicationInfo {
        // Since ReplicationInfo doesn't implement Clone, we'll create a new instance
        // TODO: Consider implementing Clone for ReplicationInfo or returning a reference
        crate::stores::replicator::replication_info::ReplicationInfo::new()
    }

    fn replicator(&self) -> &crate::stores::replicator::replicator::Replicator {
        // For now, return a placeholder
        todo!("Replicator access needs proper implementation")
    }

    fn cache(&self) -> Arc<dyn Datastore> {
        self.basestore.cache()
    }

    async fn drop(&mut self) -> std::result::Result<(), Self::Error> {
        self.basestore.drop()
    }

    async fn load(&mut self, amount: usize) -> std::result::Result<(), Self::Error> {
        self.basestore.load(Some(amount as isize)).await
    }

    async fn sync(&mut self, heads: Vec<crate::eqlabs_ipfs_log::entry::Entry>) -> std::result::Result<(), Self::Error> {
        self.basestore.sync(heads).await
    }

    async fn load_more_from(&mut self, _amount: u64, entries: Vec<crate::eqlabs_ipfs_log::entry::Entry>) {
        self.basestore.load_more_from(entries)
    }

    async fn load_from_snapshot(&mut self) -> std::result::Result<(), Self::Error> {
        // Temporary stub implementation to avoid Send issues
        // TODO: Implement proper load_from_snapshot functionality that is Send-safe
        Ok(())
    }

    fn op_log(&self) -> &crate::eqlabs_ipfs_log::log::Log {
        // This needs to be refactored - for now return a placeholder
        todo!("OpLog access needs proper implementation")
    }

    fn ipfs(&self) -> Arc<crate::kubo_core_api::client::KuboCoreApiClient> {
        self.basestore.ipfs()
    }

    fn db_name(&self) -> &str {
        self.basestore.db_name()
    }

    fn identity(&self) -> &Identity {
        self.basestore.identity()
    }

    fn access_controller(&self) -> &dyn crate::access_controller::traits::AccessController {
        // For now, return a dummy implementation
        todo!("AccessController implementation needed")
    }

    async fn add_operation(
        &mut self,
        op: Operation,
        on_progress_callback: Option<tokio::sync::mpsc::Sender<crate::eqlabs_ipfs_log::entry::Entry>>,
    ) -> std::result::Result<crate::eqlabs_ipfs_log::entry::Entry, Self::Error> {
        self.basestore.add_operation(op, on_progress_callback).await
    }

    fn logger(&self) -> &slog::Logger {
        // For now, return a placeholder
        todo!("Logger access needs proper implementation")
    }

    fn tracer(&self) -> Arc<crate::iface::TracerWrapper> {
        self.basestore.tracer()
    }

    // Removido: fn io() - A funcionalidade está implementada em Entry::multihash

    fn event_bus(&self) -> EventBus {
        // TODO: Convert from Arc<EventBus> to EventBus - for now returning a new instance
        crate::pubsub::event::EventBus::new()
    }
}

impl OrbitDBEventLogStore {
    /// Equivalente a função "NewOrbitDBEventLogStore" em go.
    /// Instancia uma nova EventLogStore, adaptada para usar um cliente HTTP para o Kubo.
    pub async fn new(
        // O parâmetro agora é um cliente HTTP, envolvido em um Arc para compartilhamento seguro.
        ipfs_client: Arc<IpfsClient>,
        identity: Arc<Identity>,
        addr: Arc<dyn Address + Send + Sync>,
        mut options: iface::NewStoreOptions,
    ) -> Result<Self> {

        // Em Go, o ponteiro para a função de criação do índice é definido dentro do
        // construtor. Em Rust, fazemos o mesmo, modificando as opções.
        options.index = Some(Box::new(new_event_index));

        // A lógica de `InitBaseStore` seria encapsulada no construtor de `BaseStore`,
        // que agora recebe e armazena o cliente HTTP.
        let basestore = BaseStore::new(ipfs_client, identity, addr, Some(options)).await
            .map_err(|e| GuardianError::Store(format!("unable to initialize base store: {}", e)))?;

        Ok(OrbitDBEventLogStore {
            basestore: Arc::try_unwrap(basestore).map_err(|_| GuardianError::Store("Failed to unwrap BaseStore Arc".to_string()))?
        })
    }

    /// Equivalente a função "List" em go.
    /// Coleta todas as operações de uma stream em um vetor.
    pub async fn list(&self, options: Option<StreamOptions>) -> Result<Vec<Operation>> {
        let (tx, mut rx) = mpsc::channel(1);

        // Para simplificar, vamos executar diretamente em vez de spawn
        self.stream(tx, options).await?;

        let mut operations = Vec::new();
        while let Some(op) = rx.recv().await {
            operations.push(op);
        }

        Ok(operations)
    }

    /// Equivalente a função "Add" em go.
    /// Cria e adiciona uma nova operação "ADD" ao log.
    pub async fn add(&mut self, value: Vec<u8>) -> Result<Operation> {
        let op = Operation::new(None, "ADD".to_string(), Some(value));

        // `add_operation` retorna um `Entry`.
        let entry = self.add_operation(op, None).await
            .map_err(|e| GuardianError::Store(format!("error while adding operation: {}", e)))?;

        // `parse_operation` converte o `Entry` de volta para uma `Operation`.
        let op_result = operation::parse_operation(entry)
            .map_err(|e| GuardianError::Store(format!("unable to parse newly created entry: {}", e)))?;

        Ok(op_result)
    }

    /// Equivalente a função "Get" em go.
    /// Recupera uma única operação do log pelo seu CID.
    pub async fn get(&self, cid: Cid) -> Result<Operation> {
        let (tx, mut rx) = mpsc::channel(1);
        
        let stream_options = StreamOptions {
            gte: Some(cid),
            amount: Some(1),
            ..Default::default()
        };

        // Para simplificar, vamos executar diretamente
        self.stream(tx, Some(stream_options)).await?;

        // Aguarda o primeiro valor
        if let Some(value) = rx.recv().await {
            Ok(value)
        } else {
            Err(GuardianError::Store("stream completed without yielding a value".to_string()))
        }
    }

    /// Equivalente a função "Stream" em go.
    /// Busca entradas, as converte em operações e as envia através de um canal.
    pub async fn stream(
        &self,
        result_chan: mpsc::Sender<Operation>,
        options: Option<StreamOptions>,
    ) -> Result<()> {
        // A função `query` retorna as entradas (entries) do log.
        let messages = self.query(options)
            .map_err(|e| GuardianError::Store(format!("unable to fetch query results: {}", e)))?;

        for message in messages {
            // Converte cada entrada em uma Operação.
            let op = operation::parse_operation(message)
                .map_err(|e| GuardianError::Store(format!("unable to parse operation: {}", e)))?;

            // Envia a operação pelo canal. Se o receptor for fechado, o envio falhará
            // e o loop será interrompido, o que é o comportamento esperado.
            if result_chan.send(op).await.is_err() {
                // O receptor foi fechado, então podemos parar de enviar.
                break;
            }
        }

        // Em Rust, o canal é fechado automaticamente quando `result_chan` (o Sender)
        // sai de escopo, então uma chamada explícita como `close(resultChan)` não é necessária.
        Ok(())
    }

    /// Equivalente a função "query" em go.
    /// Executa a lógica de busca no índice do log com base nas opções de filtro.
    fn query(&self, options: Option<StreamOptions>) -> Result<Vec<Entry>> {
        let options = options.unwrap_or_default();

        // Para agora, vamos retornar um vetor vazio como placeholder
        // A implementação real precisará de acesso adequado ao índice
        let events: Vec<Entry> = Vec::new();

        // Calcula a quantidade de itens a serem retornados.
        let amount = match options.amount {
            Some(a) if a > -1 => a as usize,
            _ => events.len(), // Se amount for -1 ou None, pega todos.
        };

        if options.gt.is_some() || options.gte.is_some() {
            // Caso "maior que" (Greater Than)
            let cid = options.gt.or(options.gte).unwrap();
            let inclusive = options.gte.is_some();
            return Ok(self.read(&events, Some(cid), amount, inclusive));
        }

        let cid = options.lt.or(options.lte);

        // Caso "menor que" (Lower Than) ou N últimos.
        // Inverte os eventos para buscar dos mais recentes para os mais antigos.
        let mut events = events;
        events.reverse();
        
        // A busca é inclusiva se LTE for definido ou se nenhum limite (LT/LTE) for definido.
        let inclusive = options.lte.is_some() || cid.is_none();
        let mut result = self.read(&events, cid, amount, inclusive);
        
        // Desfaz a inversão do resultado para manter a ordem cronológica original.
        result.reverse();

        Ok(result)
    }

    /// Equivalente a função "read" em go.
    /// Função auxiliar para ler uma fatia de entradas a partir de um hash.
    fn read(&self, ops: &[Entry], hash: Option<Cid>, amount: usize, inclusive: bool) -> Vec<Entry> {
        if amount == 0 {
            return Vec::new();
        }

        // Encontra o índice inicial.
        let mut start_index = 0;
        if let Some(h) = hash {
            if let Some(idx) = ops.iter().position(|e| e.hash() == h.to_string()) {
                start_index = idx;
            } else {
                // Se o hash não for encontrado, não há o que retornar.
                return Vec::new();
            }
        }

        // Se não for inclusivo, começa a partir do próximo elemento.
        if !inclusive {
            start_index += 1;
        }

        // Limita a quantidade de elementos e coleta o resultado.
        ops.iter()
            .skip(start_index)
            .take(amount)
            .cloned()
            .collect()
    }
}