use crate::error::Result;
use crate::iface::TracerWrapper;
use crate::ipfs_log::{entry::Entry, log::Log};
use crate::pubsub::event::{Emitter, EventBus}; // Usando nosso EventBus
use crate::stores::events::{EventLoad, EventLoadProgress, EventReplicated};
use crate::stores::replicator::events::{EventLoadAdded, EventLoadEnd};
use crate::stores::replicator::queue::{
    ProcessItem as ProcessItemTrait, ProcessQueue, ProcessQueueItem,
}; // Import trait
use crate::stores::replicator::traits::StoreInterface;
use cid::Cid;
use ipfs_api_backend_hyper::IpfsApi;
use opentelemetry::trace::{TraceContextExt, TracerProvider, noop::NoopTracerProvider};
use slog::Logger;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{Level, info, span, warn};

/// Concrete implementation of ProcessItem for the replicator
/// Note: This is now primarily used for creating ProcessQueueItems
#[derive(Clone, Debug)]
pub struct ProcessItem {
    hash: Cid,
    #[allow(dead_code)]
    entry: Option<Entry>,
}

impl ProcessItem {
    pub fn new(hash: Cid) -> ProcessQueueItem {
        ProcessQueueItem::Hash(crate::stores::replicator::queue::ProcessHash::new(hash))
    }

    pub fn from_entry(entry: Entry) -> ProcessQueueItem {
        ProcessQueueItem::Entry(crate::stores::replicator::queue::ProcessEntry::new(entry))
    }
}

// O trait Replicator define a interface pública, assim como a interface em Go.
pub trait ReplicatorInterface {
    fn event_bus(&self) -> EventBus;
    fn get_queue(&self) -> impl Future<Output = Vec<Cid>> + Send;
    fn load(&self, entries: Vec<Box<Entry>>) -> impl Future<Output = ()> + Send;
    fn stop(&self) -> impl Future<Output = ()> + Send;
    fn should_exclude(&self, hash: &Cid) -> impl Future<Output = bool> + Send;
}

// equivalente ao enum queuedState em go
#[allow(dead_code)]
enum QueuedState {
    Added,
    Fetching,
    Fetched,
}

// equivalente ao struct Options em go
pub struct ReplicatorOptions {
    pub logger: Option<Logger>,
    pub tracer: Option<Arc<TracerWrapper>>,
    pub event_bus: Option<EventBus>,
}

impl Default for ReplicatorOptions {
    fn default() -> Self {
        let tracer = NoopTracerProvider::new().tracer("");
        Self {
            logger: None,
            tracer: Some(Arc::new(TracerWrapper::Noop(tracer))),
            event_bus: None, // O event_bus deve ser criado se for `None`.
        }
    }
}

// A implementação do trait para a nossa struct Replicator.
// Isso garante que nossa struct está em conformidade com a interface pública.
impl ReplicatorInterface for Replicator {
    fn event_bus(&self) -> EventBus {
        // Cria uma nova instância do EventBus já que não implementa Clone
        EventBus::new()
    }

    fn get_queue(&self) -> impl Future<Output = Vec<Cid>> + Send {
        Replicator::get_queue(self)
    }

    fn load(&self, entries: Vec<Box<Entry>>) -> impl Future<Output = ()> + Send {
        Replicator::load(self, entries)
    }

    fn stop(&self) -> impl Future<Output = ()> + Send {
        Replicator::stop(self)
    }

    fn should_exclude(&self, hash: &Cid) -> impl Future<Output = bool> + Send {
        let hash_owned = *hash;
        async move {
            let state = &self.state.read().await;

            // 1. Verifica se o hash já existe no log da store principal.
            let op_log_arc = self.store.op_log_arc();
            let in_log = op_log_arc.read().get(&hash_owned.to_string()).is_some();
            if in_log {
                return true;
            }

            // 2. Verifica se o hash já foi buscado com sucesso nesta sessão de replicação.
            if let Some(task_state) = state.tasks.get(&hash_owned) {
                if matches!(task_state, QueuedState::Fetched) {
                    return true;
                }
            }

            false
        }
    }
}

/// Implementação do Replicator para GuardianDB
///
/// O Replicator é responsável por sincronizar dados entre diferentes instâncias
/// de uma store GuardianDB. Ele gerencia a busca de logs, a replicação de entradas
/// e a coordenação de tarefas concorrentes de sincronização.
///
/// Funcionalidades principais:
/// - Carregamento de entradas de log do IPFS
/// - Gestão de fila de processamento com controle de concorrência
/// - Emissão de eventos de progresso de replicação
/// - Exclusão de entradas duplicadas
///
/// equivalente ao struct replicator em go
pub struct Replicator {
    // Token para gerenciar o cancelamento de tarefas em andamento.
    root_cancellation_token: CancellationToken,

    // Barramento de eventos para emitir eventos de replicação.
    #[allow(dead_code)]
    event_bus: EventBus,

    // Emitters para eventos específicos de replicação
    emitters: ReplicatorEmitters,

    // A store que está sendo replicada.
    store: Arc<dyn StoreInterface>,

    // Semáforo para limitar o número de operações de busca concorrentes.
    semaphore: Arc<Semaphore>,

    // Estrutura para manter o estado protegido por locks.
    state: RwLock<ReplicatorState>,

    // Buffer para logs que são concluídos.
    buffer: Mutex<Vec<Log>>,

    // Logger e tracer para observabilidade.
    #[allow(dead_code)]
    logger: Logger,
    #[allow(dead_code)]
    tracer: Arc<TracerWrapper>,
}

/// Emitters específicos para eventos de replicação
#[allow(dead_code)]
struct ReplicatorEmitters {
    evt_load: Emitter<EventLoad>,
    evt_load_progress: Emitter<EventLoadProgress>,
    evt_load_added: Emitter<EventLoadAdded>,
    evt_load_end: Emitter<EventLoadEnd>,
    evt_replicated: Emitter<EventReplicated>,
}

impl ReplicatorEmitters {
    /// Cria novos emitters para todos os eventos de replicação
    async fn new(bus: &EventBus) -> Result<Self> {
        Ok(Self {
            evt_load: bus.emitter::<EventLoad>().await.map_err(|e| {
                crate::error::GuardianError::Store(format!(
                    "Failed to create EventLoad emitter: {}",
                    e
                ))
            })?,
            evt_load_progress: bus.emitter::<EventLoadProgress>().await.map_err(|e| {
                crate::error::GuardianError::Store(format!(
                    "Failed to create EventLoadProgress emitter: {}",
                    e
                ))
            })?,
            evt_load_added: bus.emitter::<EventLoadAdded>().await.map_err(|e| {
                crate::error::GuardianError::Store(format!(
                    "Failed to create EventLoadAdded emitter: {}",
                    e
                ))
            })?,
            evt_load_end: bus.emitter::<EventLoadEnd>().await.map_err(|e| {
                crate::error::GuardianError::Store(format!(
                    "Failed to create EventLoadEnd emitter: {}",
                    e
                ))
            })?,
            evt_replicated: bus.emitter::<EventReplicated>().await.map_err(|e| {
                crate::error::GuardianError::Store(format!(
                    "Failed to create EventReplicated emitter: {}",
                    e
                ))
            })?,
        })
    }
}

/// Estado interno do Replicator
///
/// Contém informações sobre tarefas em andamento, fila de processamento
/// e estado de busca de cada CID.
struct ReplicatorState {
    // Mapeia CIDs para seu estado de busca na fila.
    tasks: HashMap<Cid, QueuedState>,
    // Fila de CIDs a serem processados.
    queue: ProcessQueue,
    // Contador de tarefas de busca em andamento.
    #[allow(dead_code)]
    tasks_in_progress: i64,
}

/// Referência leve ao Replicator para evitar clonagem completa
/// Esta estrutura contém apenas as referências necessárias para operações assíncronas
#[allow(dead_code)]
struct ReplicatorRef<'a> {
    store: Arc<dyn StoreInterface>,
    semaphore: Arc<Semaphore>,
    state: &'a RwLock<ReplicatorState>,
    buffer: &'a Mutex<Vec<Log>>,
    root_cancellation_token: CancellationToken,
    emitters: &'a ReplicatorEmitters,
}

impl Replicator {
    // equivalente a NewReplicator em go
    pub async fn new(
        store: Arc<dyn StoreInterface>,
        concurrency: Option<usize>,
        opts: Option<ReplicatorOptions>,
    ) -> Result<Self> {
        let options = opts.unwrap_or_default();

        let event_bus = options.event_bus.unwrap_or_else(|| EventBus::new());

        // Inicializa os emissores de eventos
        let emitters = ReplicatorEmitters::new(&event_bus).await?;

        let logger = options
            .logger
            .unwrap_or_else(|| slog::Logger::root(slog::Discard, slog::o!()));
        let tracer = options.tracer.unwrap(); // `default` já fornece um NoopTracer.

        // Define a concorrência padrão se não for fornecida.
        let concurrency = concurrency.unwrap_or(32);

        // O CancellationToken principal para controlar o ciclo de vida do replicator.
        let root_cancellation_token = CancellationToken::new();

        Ok(Self {
            root_cancellation_token,
            event_bus,
            emitters,
            store,
            semaphore: Arc::new(Semaphore::new(concurrency)),
            state: RwLock::new(ReplicatorState {
                tasks: HashMap::new(),
                queue: ProcessQueue::new(),
                tasks_in_progress: 0,
            }),
            buffer: Mutex::new(Vec::new()),
            logger,
            tracer,
        })
    }

    // equivalente a Stop em go
    pub async fn stop(&self) {
        // Cancela todas as tarefas em andamento que estão escutando este token.
        self.root_cancellation_token.cancel();

        // Em Rust, o fechamento dos emissores (`emitters`) geralmente é tratado
        // quando o `Replicator` é descartado (dropped). Se um fechamento explícito
        // for necessário, seria implementado aqui.
        info!("Replicator stopped");

        // Exemplo de como o fechamento de emissores poderia ser:
        // if let Err(e) = self.emitters.evt_load_end.close().await {
        //     warn!("unable to close evt_load_end emitter: {:?}", e);
        // }
        // ... para outros emissores
    }

    // equivalente a GetQueue em go
    // NOTA: A função original em Go itera sobre o mapa `tasks`, não sobre a `queue`.
    // O nome foi mantido para consistência, mas o comportamento reflete o código original.
    pub async fn get_queue(&self) -> Vec<Cid> {
        let state = self.state.read().await;

        // Clona os CIDs do mapa de tarefas para retornar.
        state.tasks.keys().cloned().collect()
    }

    // equivalente a Load em go
    pub async fn load(&self, entries: Vec<Box<Entry>>) {
        let cids_str: Vec<String> = entries.iter().map(|e| e.hash().to_string()).collect();

        // Cria um span para tracing, similar ao OpenTelemetry em Go.
        let span = span!(Level::INFO, "replicator-load", cids = cids_str.join(","));
        let _enter = span.enter();
        let ctx = opentelemetry::Context::current();
        ctx.span().add_event("replicator-load".to_string(), vec![]);

        // O padrão `rootContextWithCancel` do Go é substituído pelo `tokio::select!`
        // para cancelar a operação se o replicator for parado.
        tokio::select! {
            // Se o token de cancelamento principal for acionado, a função termina.
            _ = self.root_cancellation_token.cancelled() => {
                warn!("Load cancelled because replicator was stopped.");
                return;
            }
            // Caso contrário, executa a lógica de carregamento.
            res = self.perform_load(entries) => {
                if let Err(e) = res {
                    warn!(error = ?e, "Replicator load finished with an error");
                }
            }
        };
    }

    /// Função auxiliar que contém a lógica principal de `Load`.
    async fn perform_load(&self, entries: Vec<Box<Entry>>) -> Result<()> {
        // Envolve o JoinSet em Arc<Mutex<>> para compartilhamento seguro entre tarefas.
        let join_set: Arc<Mutex<JoinSet<Result<()>>>> = Arc::new(Mutex::new(JoinSet::new()));

        {
            // Inicia um bloco para limitar o escopo do lock de escrita.
            let mut state = self.state.write().await;
            for entry in entries {
                let hash = Cid::try_from(entry.hash()).unwrap_or_default();

                // Se a entrada já foi adicionada, pula.
                if self.add_entry_to_queue(&mut state, entry.clone()) {
                    continue;
                }

                // Emite o evento `LoadAdded` para a nova entrada adicionada
                let load_added_event = EventLoadAdded::new(hash, *entry);
                if let Err(e) = self.emitters.evt_load_added.emit(load_added_event) {
                    warn!("Unable to emit event load added: {:?}", e);
                }

                // Para cada nova entrada, vamos processar de forma simplificada
                // sem spawning de tasks separadas por enquanto
                info!("Added entry to replication queue: {}", hash);
            }
        } // O lock de escrita em `state` é liberado aqui.

        // Aguarda todas as tarefas (iniciais e as geradas por elas)
        let mut locked_join_set = join_set.lock().await;
        while let Some(res) = locked_join_set.join_next().await {
            if let Err(e) =
                res.map_err(|e| crate::error::GuardianError::Store(format!("Join error: {}", e)))?
            {
                warn!(error = ?e, "A replication task failed");
            }
        }

        Ok(())
    }

    // equivalente a processOne em go
    /// Processa um único item da fila, adquirindo um slot do semáforo.
    #[allow(dead_code)]
    async fn process_one(&self, _join_set: Arc<Mutex<JoinSet<Result<()>>>>) -> Result<()> {
        // Espera por um slot de processamento disponível. O `permit` garante que o semáforo
        // seja liberado quando a função terminar, mesmo em caso de erro.
        let permit = self.semaphore.clone().acquire_owned().await.map_err(|e| {
            crate::error::GuardianError::Store(format!("Semaphore acquire error: {}", e))
        })?;

        // Pega o próximo item da fila e o marca como "Fetching".
        let item = match self.wait_for_process_slot().await {
            Some(item) => item,
            None => return Ok(()), // Fila vazia, nenhuma tarefa a fazer.
        };

        let item_hash = item.get_hash(); // Get hash before moving item

        // Passa o item para process_hash diretamente
        let next_hashes = match self.process_hash(item).await {
            Ok(hashes) => hashes,
            Err(e) => {
                warn!(error = ?e, "processing item ended with an error");
                Vec::new()
            }
        };

        // Se encontrarmos novas hashes, as adicionamos à fila
        if !next_hashes.is_empty() {
            let mut state = self.state.write().await;
            for hash in next_hashes {
                if self.add_hash_to_queue(&mut state, hash) {
                    continue;
                }
                info!("Added new hash to replication queue: {}", hash);
            }
        }

        // Marca a tarefa como concluída e libera recursos.
        self.process_entry_done(item_hash).await;

        // O `permit` do semáforo é liberado aqui automaticamente.
        drop(permit);

        Ok(())
    }

    // equivalente a AddEntryToQueue em go (não thread-safe, requer lock externo)
    fn add_entry_to_queue(
        &self,
        state: &mut tokio::sync::RwLockWriteGuard<'_, ReplicatorState>,
        entry: Box<Entry>,
    ) -> bool {
        let hash = Cid::try_from(entry.hash()).unwrap_or_default();

        // Verifica se o hash já está na store ou na fila de tarefas.
        let op_log_arc = self.store.op_log_arc();
        let in_log = op_log_arc.read().get(&hash.to_string()).is_some();
        if state.tasks.contains_key(&hash) || in_log {
            return true; // "exist" é true, já existe.
        }

        // Adiciona à fila usando a nova API
        state.queue.add_hash(hash);
        state.tasks.insert(hash, QueuedState::Added);

        false // "exist" é false, foi adicionado agora.
    }

    // equivalente a waitForProcessSlot em go
    /// Pega o próximo item da fila e atualiza seu estado para `Fetching`.
    async fn wait_for_process_slot(&self) -> Option<ProcessQueueItem> {
        let mut state = self.state.write().await;

        let item = state.queue.next()?;
        let hash = item.get_hash();

        state.tasks_in_progress += 1;
        state.tasks.insert(hash, QueuedState::Fetching);

        Some(item)
    }

    // equivalente a processEntryDone em go
    /// Finaliza o processamento de uma entrada.
    async fn process_entry_done(&self, hash: Cid) {
        let mut state = self.state.write().await;

        state.tasks_in_progress -= 1;
        state.tasks.insert(hash, QueuedState::Fetched);

        // Verifica se não há mais tarefas ativas.
        if self.is_idle(&state) {
            self.idle(&state).await;
        }
    }

    // As funções `is_idle` e `idle` são necessárias por `process_entry_done`.
    // Equivalentes a isIdle e idle em go (não thread-safe)
    fn is_idle(&self, state: &tokio::sync::RwLockWriteGuard<'_, ReplicatorState>) -> bool {
        if state.tasks_in_progress > 0 || state.queue.len() > 0 {
            return false;
        }

        for (_, task_state) in &state.tasks {
            if matches!(task_state, QueuedState::Added | QueuedState::Fetching) {
                return false;
            }
        }

        true
    }

    async fn idle(&self, _state: &tokio::sync::RwLockWriteGuard<'_, ReplicatorState>) {
        let mut buffer = self.buffer.lock().await;
        if !buffer.is_empty() {
            // Emite o evento `LoadEnd`
            let log_ids: Vec<String> = buffer.iter().map(|log| log.id().to_string()).collect();
            let load_end_event = crate::stores::replicator::events::EventLoadEnd::new(log_ids);

            if let Err(e) = self.emitters.evt_load_end.emit(load_end_event) {
                warn!("Unable to emit LoadEnd event: {:?}", e);
            } else {
                info!("Replication batch finished, emitting LoadEnd event.");
            }

            // Limpa o buffer.
            buffer.clear();

            // A lógica de garbage collection para `tasks` iria aqui.
        }
    }

    /// Cria um clone leve do `Replicator` para uso em tarefas `spawned`.
    /// Agora retorna uma referência leve ao invés de clonar tudo.
    #[allow(dead_code)]
    fn create_ref(&self) -> ReplicatorRef {
        ReplicatorRef {
            store: self.store.clone(),
            semaphore: self.semaphore.clone(),
            state: &self.state,
            buffer: &self.buffer,
            root_cancellation_token: self.root_cancellation_token.clone(),
            emitters: &self.emitters,
        }
    }
}

impl<'a> ReplicatorRef<'a> {
    /// Versão de `process_one` que funciona com referências ao invés de clonagem
    #[allow(dead_code)]
    async fn process_one(&self, _join_set: Arc<Mutex<JoinSet<Result<()>>>>) -> Result<()> {
        // Espera por um slot de processamento disponível
        let permit = self.semaphore.clone().acquire_owned().await.map_err(|e| {
            crate::error::GuardianError::Store(format!("Semaphore acquire error: {}", e))
        })?;

        // Pega o próximo item da fila e o marca como "Fetching"
        let item = match self.wait_for_process_slot().await {
            Some(item) => item,
            None => return Ok(()), // Fila vazia, nenhuma tarefa a fazer
        };

        let item_hash = item.get_hash();

        // Processa o item diretamente
        let next_hashes = match self.process_hash(item).await {
            Ok(hashes) => hashes,
            Err(e) => {
                warn!(error = ?e, "processing item ended with an error");
                Vec::new()
            }
        };

        // Se encontrarmos novas hashes, as adicionamos à fila
        if !next_hashes.is_empty() {
            let mut state = self.state.write().await;
            for hash in next_hashes {
                if self.add_hash_to_queue(&mut state, hash) {
                    continue;
                }
                info!("Added new hash to replication queue: {}", hash);
            }
        }

        // Marca a tarefa como concluída
        self.process_entry_done(item_hash).await;

        // O `permit` do semáforo é liberado automaticamente
        drop(permit);

        Ok(())
    }

    /// Versão de `wait_for_process_slot` para ReplicatorRef
    #[allow(dead_code)]
    async fn wait_for_process_slot(&self) -> Option<ProcessQueueItem> {
        let mut state = self.state.write().await;

        let item = state.queue.next()?;
        let hash = item.get_hash();

        state.tasks_in_progress += 1;
        state.tasks.insert(hash, QueuedState::Fetching);

        Some(item)
    }

    /// Versão de `process_entry_done` para ReplicatorRef
    #[allow(dead_code)]
    async fn process_entry_done(&self, hash: Cid) {
        let mut state = self.state.write().await;

        state.tasks_in_progress -= 1;
        state.tasks.insert(hash, QueuedState::Fetched);

        // Verifica se não há mais tarefas ativas
        if self.is_idle(&state) {
            self.idle(&state).await;
        }
    }

    /// Versão de `is_idle` para ReplicatorRef
    #[allow(dead_code)]
    fn is_idle(&self, state: &tokio::sync::RwLockWriteGuard<'_, ReplicatorState>) -> bool {
        if state.tasks_in_progress > 0 || state.queue.len() > 0 {
            return false;
        }

        for (_, task_state) in &state.tasks {
            if matches!(task_state, QueuedState::Added | QueuedState::Fetching) {
                return false;
            }
        }

        true
    }

    /// Versão de `idle` para ReplicatorRef
    #[allow(dead_code)]
    async fn idle(&self, _state: &tokio::sync::RwLockWriteGuard<'_, ReplicatorState>) {
        let mut buffer = self.buffer.lock().await;
        if !buffer.is_empty() {
            // Emite o evento `LoadEnd`
            let log_ids: Vec<String> = buffer.iter().map(|log| log.id().to_string()).collect();
            let load_end_event = crate::stores::replicator::events::EventLoadEnd::new(log_ids);

            if let Err(e) = self.emitters.evt_load_end.emit(load_end_event) {
                warn!("Unable to emit LoadEnd event: {:?}", e);
            } else {
                info!("Replication batch finished, emitting LoadEnd event.");
            }

            // Limpa o buffer
            buffer.clear();
        }
    }

    /// Versão de `process_items` para ReplicatorRef
    #[allow(dead_code)]
    async fn process_items(
        &self,
        items: Vec<ProcessQueueItem>,
        _join_set: Arc<Mutex<JoinSet<Result<()>>>>,
    ) -> Result<()> {
        for item in items {
            // `process_hash` busca o log e retorna os CIDs dos "próximos" logs a serem buscados
            let next_hashes = match self.process_hash(item).await {
                Ok(hashes) => hashes,
                Err(e) => {
                    warn!(error = ?e, "failed to process hash");
                    continue;
                }
            };

            // Se encontrarmos novas hashes, as adicionamos à fila
            if !next_hashes.is_empty() {
                let mut state = self.state.write().await;

                for hash in next_hashes {
                    // `add_hash_to_queue` retorna `true` se o hash já existia
                    if self.add_hash_to_queue(&mut state, hash) {
                        continue;
                    }

                    // Para evitar recursão infinita, limitamos a criação de novas tarefas
                    // apenas adicionamos à fila, as tarefas serão processadas naturalmente
                    info!("Added new hash to queue: {}", hash);
                }
            }
        }
        Ok(())
    }

    /// Versão de `process_hash` para ReplicatorRef - implementação real
    #[allow(dead_code)]
    async fn process_hash(&self, item: ProcessQueueItem) -> Result<Vec<Cid>> {
        let hash = item.get_hash();

        // Usa o cliente IPFS da store para buscar o log
        let ipfs_client = self.store.ipfs();

        // Busca a entrada do IPFS usando o método cat do trait IpfsApi
        let stream = ipfs_client.cat(&hash.to_string());

        let entry_data = {
            // Converte o stream para Vec<u8> corretamente
            use futures::StreamExt;
            let mut data = Vec::new();
            let mut stream = stream;
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(bytes) => data.extend_from_slice(&bytes),
                    Err(e) => {
                        warn!("Failed to read chunk from IPFS stream: {}", e);
                        return Ok(vec![]);
                    }
                }
            }
            data
        };

        // Tenta deserializar a entrada
        let entry: Entry = match serde_json::from_slice(&entry_data) {
            Ok(entry) => entry,
            Err(e) => {
                warn!("Failed to deserialize entry: {}", e);
                return Ok(vec![]);
            }
        };

        // Emite evento de progresso usando o tipo correto
        // Cria um endereço temporário usando o parse
        let address_str = format!("/GuardianDB/{}", hash);
        let address = crate::address::parse(&address_str).unwrap_or_else(|_| {
            // Fallback: usa o parse com apenas o hash
            crate::address::parse(&hash.to_string()).unwrap()
        });
        let progress_event = EventLoadProgress {
            address: Arc::new(address),
            hash,
            entry: entry.clone(),
            progress: 1,
            max: 1,
        };
        if let Err(e) = self.emitters.evt_load_progress.emit(progress_event) {
            warn!("Failed to emit load progress event: {:?}", e);
        }

        // Coleta os CIDs das próximas entradas para continuar a replicação
        let mut next_values = Vec::new();

        // Adiciona referências da entrada atual
        for next_hash in entry.next() {
            if let Ok(cid) = next_hash.parse::<Cid>() {
                next_values.push(cid);
            }
        }

        Ok(next_values)
    }

    /// Versão de `add_hash_to_queue` para ReplicatorRef
    #[allow(dead_code)]
    fn add_hash_to_queue(
        &self,
        state: &mut tokio::sync::RwLockWriteGuard<'_, ReplicatorState>,
        hash: Cid,
    ) -> bool {
        let op_log_arc = self.store.op_log_arc();
        let in_log = op_log_arc.read().get(&hash.to_string()).is_some();
        if state.tasks.contains_key(&hash) || in_log {
            return true; // Já existe
        }

        // Usa a nova API da fila
        state.queue.add_hash(hash);
        state.tasks.insert(hash, QueuedState::Added);

        false // Foi adicionado agora
    }
}

impl Replicator {
    // equivalente a AddHashToQueue em go (não thread-safe)
    fn add_hash_to_queue(
        &self,
        state: &mut tokio::sync::RwLockWriteGuard<'_, ReplicatorState>,
        hash: Cid,
    ) -> bool {
        let op_log_arc = self.store.op_log_arc();
        let in_log = op_log_arc.read().get(&hash.to_string()).is_some();
        if state.tasks.contains_key(&hash) || in_log {
            return true; // Já existe.
        }

        // Usa a nova API da fila
        state.queue.add_hash(hash);
        state.tasks.insert(hash, QueuedState::Added);

        false // Foi adicionado agora.
    }

    // equivalente a processHash em go
    /// Busca o log associado a um `ProcessItem` do IPFS.
    async fn process_hash(&self, item: ProcessQueueItem) -> Result<Vec<Cid>> {
        let hash = item.get_hash();

        // Usa o cliente IPFS da store para buscar o log
        let ipfs_client = self.store.ipfs();

        // Busca a entrada do IPFS usando o método cat do trait IpfsApi
        let stream = ipfs_client.cat(&hash.to_string());

        let entry_data = {
            // Converte o stream para Vec<u8> corretamente
            use futures::StreamExt;
            let mut data = Vec::new();
            let mut stream = stream;
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(bytes) => data.extend_from_slice(&bytes),
                    Err(e) => {
                        warn!("Failed to read chunk from IPFS stream: {}", e);
                        return Ok(vec![]);
                    }
                }
            }
            data
        };

        // Tenta deserializar a entrada
        let entry: Entry = match serde_json::from_slice(&entry_data) {
            Ok(entry) => entry,
            Err(e) => {
                warn!("Failed to deserialize entry: {}", e);
                return Ok(vec![]);
            }
        };

        // Emite evento de progresso usando o tipo correto
        // Cria um endereço temporário usando o parse
        let address_str = format!("/GuardianDB/{}", hash);
        let address = crate::address::parse(&address_str).unwrap_or_else(|_| {
            // Fallback: usa o parse com apenas o hash
            crate::address::parse(&hash.to_string()).unwrap()
        });
        let progress_event = EventLoadProgress {
            address: Arc::new(address),
            hash,
            entry: entry.clone(),
            progress: 1,
            max: 1,
        };
        if let Err(e) = self.emitters.evt_load_progress.emit(progress_event) {
            warn!("Failed to emit load progress event: {:?}", e);
        }

        // Coleta os CIDs das próximas entradas para continuar a replicação
        let mut next_values = Vec::new();

        // Adiciona referências da entrada atual
        for next_hash in entry.next() {
            if let Ok(cid) = next_hash.parse::<Cid>() {
                next_values.push(cid);
            }
        }

        Ok(next_values)
    }
}
