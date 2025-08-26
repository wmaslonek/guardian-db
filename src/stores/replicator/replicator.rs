use cid::Cid;
use crate::pubsub::event::EventBus; // Usando nosso EventBus
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, Semaphore};
use tokio_util::sync::CancellationToken;
use opentelemetry::trace::{TracerProvider, noop::NoopTracerProvider, TraceContextExt};
use tokio::task::JoinSet;
use crate::error::Result;
use tracing::{info, warn, span, Level};
use std::future::Future;
use tokio::sync::mpsc;
use slog::Logger;
use crate::stores::replicator::traits::StoreInterface;
use crate::iface::TracerWrapper;
use crate::eqlabs_ipfs_log::{log::Log, entry::Entry};
use crate::stores::replicator::queue::ProcessQueue;

/// Concrete implementation of ProcessItem for the replicator
#[derive(Clone, Debug)]
pub struct ProcessItem {
    hash: Cid,
    entry: Option<Entry>,
}

impl crate::stores::replicator::queue::ProcessItem for ProcessItem {
    fn get_hash(&self) -> Cid {
        self.hash
    }
}

impl ProcessItem {
    pub fn new(hash: Cid) -> Self {
        Self { hash, entry: None }
    }
    
    pub fn from_entry(entry: Entry) -> Self {
        let hash = Cid::try_from(entry.hash()).unwrap_or_default();
        Self { hash, entry: Some(entry) }
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
        Replicator::event_bus(self)
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
        Replicator::should_exclude(self, hash)
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
    event_bus: EventBus,
    // Emitters para eventos específicos. (Serão inicializados no construtor)
    // emitters: ...

    // A store que está sendo replicada.
    store: Arc<dyn StoreInterface>,
    
    // Semáforo para limitar o número de operações de busca concorrentes.
    semaphore: Arc<Semaphore>,

    // Estrutura para manter o estado protegido por locks.
    state: RwLock<ReplicatorState>,

    // Buffer para logs que são concluídos.
    buffer: Mutex<Vec<Log>>,
    
    // Logger e tracer para observabilidade.
    logger: Logger,
    tracer: Arc<TracerWrapper>,
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
    tasks_in_progress: i64,
}

impl Replicator {
    // equivalente a NewReplicator em go
    pub fn new(
        store: Arc<dyn StoreInterface>,
        concurrency: Option<usize>,
        opts: Option<ReplicatorOptions>,
    ) -> Result<Self> {
        let options = opts.unwrap_or_default();

        let event_bus = options.event_bus.unwrap_or_else(|| EventBus::new());
        // Chama a função auxiliar para configurar os emissores.
        Self::generate_emitters(&event_bus)?;
        let logger = options.logger.unwrap_or_else(|| slog::Logger::root(slog::Discard, slog::o!()));
        let tracer = options.tracer.unwrap(); // `default` já fornece um NoopTracer.

        // Define a concorrência padrão se não for fornecida.
        let concurrency = concurrency.unwrap_or(32);

        // O CancellationToken principal para controlar o ciclo de vida do replicator.
        let root_cancellation_token = CancellationToken::new();

        // Inicializa os emissores de eventos.
        // let evt_load_end = event_bus.emitter::<EventLoadEnd>();
        // let evt_load_added = event_bus.emitter::<EventLoadAdded>();
        // let evt_load_progress = event_bus.emitter::<EventLoadProgress>();

        Ok(Self {
            root_cancellation_token,
            event_bus,
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

        { // Inicia um bloco para limitar o escopo do lock de escrita.
            let mut state = self.state.write().await;
            for entry in entries {
                // Se a entrada já foi adicionada, pula.
                if self.add_entry_to_queue(&mut state, entry) {
                    continue;
                }

                // Emite o evento `LoadAdded`
                // if let Err(e) = self.emitters.evt_load_added.emit(NewEventLoadAdded(...)).await {
                //     warn!("unable to emit event load added: {:?}", e);
                // }

                // Adiciona uma nova tarefa `process_one` ao JoinSet.
                // TODO: Refactor to avoid cloning the entire replicator
                // For now, we'll skip spawning tasks that require cloning
                warn!("Skipping spawn of process_one task due to clone limitations");
            }
        } // O lock de escrita em `state` é liberado aqui.

        // Aguarda todas as tarefas (iniciais e as geradas por elas)
    let mut locked_join_set = join_set.lock().await;
    while let Some(res) = locked_join_set.join_next().await {
        if let Err(e) = res.map_err(|e| crate::error::GuardianError::Store(format!("Join error: {}", e)))? {
            warn!(error = ?e, "A replication task failed");
        }
    }

    Ok(())
}

    // equivalente a processOne em go
    /// Processa um único item da fila, adquirindo um slot do semáforo.
    async fn process_one(&self, join_set: Arc<Mutex<JoinSet<Result<()>>>>) -> Result<()> {
        // Espera por um slot de processamento disponível. O `permit` garante que o semáforo
        // seja liberado quando a função terminar, mesmo em caso de erro.
        let permit = self.semaphore.clone().acquire_owned().await
            .map_err(|e| crate::error::GuardianError::Store(format!("Semaphore acquire error: {}", e)))?;

        // Pega o próximo item da fila e o marca como "Fetching".
        let item = match self.wait_for_process_slot().await {
            Some(item) => item,
            None => return Ok(()), // Fila vazia, nenhuma tarefa a fazer.
        };

        let item_hash = item.get_hash(); // Get hash before moving item

        // Passa o join_set para process_items
        if let Err(e) = self.process_items(vec![item], join_set.clone()).await {
            warn!(error = ?e, "processing item ended with an error");
        }

        // Removendo a segunda chamada duplicada para process_items
        
        // Marca a tarefa como concluída e libera recursos.
        self.process_entry_done(item_hash).await;
        
        // O `permit` do semáforo é liberado aqui automaticamente.
        drop(permit);
        
        Ok(())
    }

    // ==== Métodos auxiliares privados (movidos para dentro do `impl Replicator` em Rust)===
    // equivalente a AddEntryToQueue em go (não thread-safe, requer lock externo)
    fn add_entry_to_queue(&self, state: &mut tokio::sync::RwLockWriteGuard<'_, ReplicatorState>, entry: Box<Entry>) -> bool {
        let hash = Cid::try_from(entry.hash()).unwrap_or_default();
        
        // Verifica se o hash já está na store ou na fila de tarefas.
        // let in_log = self.store.op_log().get(&hash.to_string()).is_some();
        let in_log = false; // Placeholder
        if state.tasks.contains_key(&hash) || in_log {
            return true; // "exist" é true, já existe.
        }

        // Adiciona à fila e ao mapa de tarefas.
        let item = ProcessItem::new(hash);
        state.queue.add(Box::new(item));
        state.tasks.insert(hash, QueuedState::Added);
        
        false // "exist" é false, foi adicionado agora.
    }
    
    // equivalente a waitForProcessSlot em go
    /// Pega o próximo item da fila e atualiza seu estado para `Fetching`.
    async fn wait_for_process_slot(&self) -> Option<Box<dyn crate::stores::replicator::queue::ProcessItem + Send + Sync>> {
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
            // self.emitters.evt_load_end.emit(NewEventLoadEnd(buffer.clone())).await;
            info!("Replication batch finished, emitting LoadEnd event.");
            
            // Limpa o buffer.
            buffer.clear();

            // A lógica de garbage collection para `tasks` iria aqui.
        }
    }
    
    /// Cria um clone leve do `Replicator` para uso em tarefas `spawned`.
    /// Usamos `Arc` internamente para tornar isso eficiente.
    fn clone_for_spawning(&self) -> Self {
        // Temporary placeholder implementation
        // TODO: Properly implement cloning or refactor to avoid the need for cloning
        panic!("clone_for_spawning not yet implemented - requires architectural changes")
    }

    //===Fim métodos auxiliares privados===

    // equivalente a processItems em go
    /// Processa um conjunto de itens, busca seus dados e enfileira as próximas tarefas.
    async fn process_items(&self, items: Vec<Box<dyn crate::stores::replicator::queue::ProcessItem + Send + Sync>>, join_set: Arc<Mutex<JoinSet<Result<()>>>>) -> Result<()> {
        for item in items {
            // `process_hash` busca o log e retorna os CIDs dos "próximos" logs a serem buscados.
            let next_hashes = match self.process_hash(item).await {
                Ok(hashes) => hashes,
                Err(e) => {
                    // Se a busca de um hash falhar, loga o erro e continua com os outros.
                    warn!(error = ?e, "failed to process hash");
                    continue;
                }
            };
            
            // Se encontrarmos novas hashes, as adicionamos à fila e geramos novas tarefas.
            if !next_hashes.is_empty() {
                let mut state = self.state.write().await;
                let _locked_join_set = join_set.lock().await;

                for hash in next_hashes {
                    // `add_hash_to_queue` retorna `true` se o hash já existia.
                    if self.add_hash_to_queue(&mut state, hash) {
                        continue;
                    }
                    
                    // Gera uma nova tarefa `process_one` para o novo hash.
                    // TODO: Refactor to avoid cloning the entire replicator  
                    // For now, we'll skip spawning tasks that require cloning
                    warn!("Skipping spawn of process_one task for hash: {}", hash);
                }
            }
        }
        Ok(())
    }

    // equivalente a processHash em go
    /// Busca o log associado a um `ProcessItem` do IPFS.
    async fn process_hash(&self, item: Box<dyn crate::stores::replicator::queue::ProcessItem + Send + Sync>) -> Result<Vec<Cid>> {
        let _hash = item.get_hash();
        let batch_size = 1;

        // Canal para receber o progresso da busca de logs.
        let (_tx_progress, mut rx_progress): (mpsc::Sender<Entry>, mpsc::Receiver<Entry>) = mpsc::channel(batch_size);
        
        // TODO: Refactor to avoid cloning the entire replicator
        let token_clone = self.root_cancellation_token.clone();

        // Tarefa para emitir eventos de progresso.
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = token_clone.cancelled() => break,
                    entry = rx_progress.recv() => {
                        if let Some(_entry) = entry {
                            // Emite o evento EventLoadProgress
                            // TODO: Implement progress event emission
                            info!("Progress event received");
                        } else {
                            // Canal fechado, fim do progresso.
                            break;
                        }
                    }
                }
            }
        });
        
        // `should_exclude` é uma closure assíncrona que verifica se um hash já foi processado.
        let _should_exclude = |cid: Cid| async move {
            self.should_exclude(&cid).await
        };
        
        // Configura as opções de busca.
        // let fetch_options = rust_ipfs_log::FetchOptions {
        //     length: Some(batch_size),
        //     progress_chan: Some(tx_progress),
        //     should_exclude: &should_exclude,
        // };
        
        // Busca o log a partir do hash (a chamada principal ao ipfs-log).
        // let log = Log::new_from_entry_hash(hash, fetch_options).await?; //verificar essa função no eqlabs FOI CRIADA MOCK NO GEMINI
        
        // Placeholder implementation until rust_ipfs_log is available
        // For now, skip creating the log since we don't have the right parameters
        // let log = Log::new("temporary_id", vec![], None); // This was causing parameter mismatch
        
        // Adiciona o log retornado ao buffer de logs concluídos.
        // self.buffer.lock().await.push(log);
        
        let next_values = vec![];
        
        // TODO: Process log entries when rust_ipfs_log is available
        // let new_log = self.buffer.lock().await.last().unwrap(); // Pega o log que acabamos de adicionar
        // 
        // for entry in new_log.values() {
        //     // Coleta os CIDs das próximas entradas e das referências.
        //     // A implementação exata dependerá da sua struct IpfsLogEntry.
        //     // next_values.extend(entry.get_next().iter().cloned());
        //     // next_values.extend(entry.get_refs().iter().cloned());
        // }
        
        Ok(next_values)
    }

    // equivalente a shouldExclude em go
    /// Verifica se um hash deve ser excluído da busca (porque já o temos).
    pub async fn should_exclude(&self, hash: &Cid) -> bool {
        let state = self.state.read().await;

        // 1. Verifica se o hash já existe no log da store principal.
        let in_log = self.store.op_log().get(&hash.to_string()).is_some();
        if in_log {
            return true;
        }

        // 2. Verifica se o hash já foi buscado com sucesso nesta sessão de replicação.
        if let Some(task_state) = state.tasks.get(hash) {
            if matches!(task_state, QueuedState::Fetched) {
                return true;
            }
        }
        
        false
    }
    
    // equivalente a AddHashToQueue em go (não thread-safe)
    fn add_hash_to_queue(&self, state: &mut tokio::sync::RwLockWriteGuard<'_, ReplicatorState>, hash: Cid) -> bool {
        let in_log = self.store.op_log().get(&hash.to_string()).is_some();
        if state.tasks.contains_key(&hash) || in_log {
            return true; // Já existe.
        }

        let item = ProcessItem::new(hash);
        state.queue.add(Box::new(item));
        state.tasks.insert(hash, QueuedState::Added);
        
        false // Foi adicionado agora.
    }

    // equivalente a EventBus em go
    pub fn event_bus(&self) -> EventBus {
        // Retorna uma nova instância do EventBus (já que não implementa Clone)
        EventBus::new()
    }

    // equivalente a generateEmitter em go
    /// Inicializa os emissores de eventos no barramento de eventos.
    fn generate_emitters(_bus: &EventBus) -> Result<()> { // Removido &mut self
        // A lógica para criar os emissores iria aqui.
        // Em rust-libp2p, os emissores são geralmente criados conforme necessário e 
        // podem não precisar ser armazenados na struct.
        // Se precisarem ser armazenados, esta função retornaria uma struct `Emitters`.
        // Exemplo:
        // let mut emitters = Emitters {
        //     evt_load_end: bus.emitter::<EventLoadEnd>()?,
        //     evt_load_added: bus.emitter::<EventLoadAdded>()?,
        //     evt_load_progress: bus.emitter::<EventLoadProgress>()?,
        // };
        // Ok(emitters)
        Ok(())
    }
}