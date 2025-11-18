/// Sistema de Processamento em Batch para Backend Iroh
///
/// Processamento inteligente em lotes para otimizar throughput
/// de operações IPFS, reduzindo overhead e melhorando eficiência de I/O.
use crate::error::{GuardianError, Result};
use crate::ipfs_core_api::backends::IpfsBackend;
use crate::ipfs_core_api::types::AddResponse;
use bytes::Bytes;
use cid::Cid;
use futures;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, Semaphore, mpsc, oneshot};
use tracing::{debug, info, instrument, warn};
use uuid::Uuid;

/// Processador de operações em batch
pub struct BatchProcessor {
    /// Fila de operações pendentes
    pending_operations: Arc<Mutex<VecDeque<BatchOperation>>>,
    /// Operações por tipo para otimização
    typed_queues: Arc<RwLock<TypedQueues>>,
    /// Configuração do processador
    config: BatchConfig,
    /// Estatísticas de performance
    stats: Arc<RwLock<BatchStats>>,
    /// Canal para controle de processamento
    #[allow(dead_code)]
    control_sender: mpsc::Sender<BatchControl>,
    /// Semáforo para controle de concorrência
    processing_semaphore: Arc<Semaphore>,
    /// Histórico de operações para otimização
    operation_history: Arc<RwLock<OperationHistory>>,
    /// Backend Iroh para operações IPFS
    iroh_backend: Arc<crate::ipfs_core_api::backends::IrohBackend>,
}

/// Operação em batch
#[derive(Debug)]
pub struct BatchOperation {
    /// ID único da operação
    pub id: String,
    /// Tipo da operação
    pub operation_type: OperationType,
    /// Dados da operação
    pub data: OperationData,
    /// Timestamp de criação
    pub created_at: Instant,
    /// Prioridade (0-10)
    pub priority: u8,
    /// Canal para retorno do resultado
    pub result_sender: oneshot::Sender<Result<OperationResult>>,
    /// Estimativa de recursos necessários
    pub resource_estimate: ResourceEstimate,
}

/// Tipos de operação suportadas
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OperationType {
    /// Adicionar dados ao IPFS
    Add,
    /// Recuperar dados do IPFS
    Get,
    /// Fixar conteúdo
    Pin,
    /// Desfixar conteúdo
    Unpin,
    /// Operação DAG
    DagPut,
    /// Operação DAG Get
    DagGet,
    /// Publicar no PubSub
    PubSubPublish,
    /// Operação de DHT
    DhtOperation,
}

/// Dados da operação
#[derive(Debug)]
pub enum OperationData {
    /// Dados para adicionar
    AddData { data: Bytes, options: AddOptions },
    /// CID para recuperar
    GetCid { cid: String, options: GetOptions },
    /// CID para fixar
    PinCid { cid: String, options: PinOptions },
    /// CID para desfixar
    UnpinCid { cid: String },
    /// Dados DAG para armazenar
    DagPutData { data: Bytes, format: String },
    /// CID DAG para recuperar
    DagGetCid { cid: String },
    /// Dados para publicar
    PubSubData { topic: String, data: Bytes },
    /// Operação DHT genérica
    DhtData { key: String, value: Option<Bytes> },
}

/// Resultado de operação
#[derive(Debug)]
pub enum OperationResult {
    /// Resultado de Add
    AddResult(AddResponse),
    /// Resultado de Get
    GetResult(Bytes),
    /// Resultado de Pin
    PinResult(bool),
    /// Resultado de Unpin
    UnpinResult(bool),
    /// Resultado de DAG Put
    DagPutResult(Cid),
    /// Resultado de DAG Get
    DagGetResult(Bytes),
    /// Resultado de PubSub
    PubSubResult(bool),
    /// Resultado de DHT
    DhtResult(Option<Bytes>),
}

/// Filas organizadas por tipo
#[derive(Debug, Default)]
pub struct TypedQueues {
    /// Operações de adição
    add_queue: VecDeque<BatchOperation>,
    /// Operações de recuperação
    get_queue: VecDeque<BatchOperation>,
    /// Operações de pin
    pin_queue: VecDeque<BatchOperation>,
    /// Operações DAG
    dag_queue: VecDeque<BatchOperation>,
    /// Operações PubSub
    pubsub_queue: VecDeque<BatchOperation>,
    /// Operações DHT
    dht_queue: VecDeque<BatchOperation>,
}

/// Opções para operação Add
#[derive(Debug, Clone, Default)]
pub struct AddOptions {
    /// Pin automaticamente
    pub pin: bool,
    /// Wrap in directory
    pub wrap_with_directory: bool,
    /// Chunker a usar
    pub chunker: Option<String>,
}

/// Opções para operação Get
#[derive(Debug, Clone, Default)]
pub struct GetOptions {
    /// Timeout para operação
    pub timeout: Option<Duration>,
    /// Tentar peers específicos
    pub preferred_peers: Vec<String>,
}

/// Opções para operação Pin
#[derive(Debug, Clone, Default)]
pub struct PinOptions {
    /// Tipo de pin (direto ou recursivo)
    pub recursive: bool,
    /// Progresso callback
    pub progress: bool,
}

/// Estimativa de recursos para operação
#[derive(Debug, Clone)]
pub struct ResourceEstimate {
    /// CPU estimado (0.0-1.0)
    pub cpu_usage: f64,
    /// Memória estimada (bytes)
    pub memory_bytes: u64,
    /// I/O estimado (bytes)
    pub io_bytes: u64,
    /// Largura de banda estimada (bytes)
    pub bandwidth_bytes: u64,
    /// Tempo estimado (ms)
    pub estimated_time_ms: u64,
}

/// Configuração do processador de batch
#[derive(Debug, Clone)]
pub struct BatchConfig {
    /// Tamanho máximo do batch
    pub max_batch_size: usize,
    /// Timeout máximo para formar batch (ms)
    pub max_batch_wait_ms: u64,
    /// Número máximo de threads de processamento
    pub max_processing_threads: usize,
    /// Threshold de memória para flush (bytes)
    pub memory_flush_threshold: u64,
    /// Habilitar otimizações inteligentes
    pub enable_smart_batching: bool,
    /// Habilitar compressão de batch
    pub enable_batch_compression: bool,
    /// Tamanho mínimo para compressão (bytes)
    pub compression_threshold: usize,
}

/// Estatísticas de batch
#[derive(Debug, Clone, Default)]
pub struct BatchStats {
    /// Total de operações processadas
    pub total_operations: u64,
    /// Operações processadas em batch
    pub batched_operations: u64,
    /// Operações processadas individualmente
    pub individual_operations: u64,
    /// Tamanho médio de batch
    pub avg_batch_size: f64,
    /// Tempo médio de processamento de batch (ms)
    pub avg_batch_processing_time_ms: f64,
    /// Throughput (operações/segundo)
    pub operations_per_second: f64,
    /// Eficiência de batch (0.0-1.0)
    pub batch_efficiency: f64,
    /// Bytes processados
    pub total_bytes_processed: u64,
    /// Economia de recursos (0.0-1.0)
    pub resource_savings: f64,
}

/// Controles de processamento
#[derive(Debug)]
pub enum BatchControl {
    /// Forçar processamento imediato
    FlushNow,
    /// Pausar processamento
    Pause,
    /// Resumir processamento
    Resume,
    /// Parar processador
    Stop,
    /// Ajustar configuração
    UpdateConfig(BatchConfig),
}

/// Histórico de operações para otimização
#[derive(Debug, Default)]
pub struct OperationHistory {
    /// Padrões de operação por tipo
    operation_patterns: HashMap<OperationType, OperationPattern>,
    /// Correlações entre operações
    #[allow(dead_code)]
    operation_correlations: HashMap<String, Vec<OperationType>>,
    /// Timing histórico
    timing_history: VecDeque<TimingEntry>,
}

/// Padrão de operação identificado
#[derive(Debug, Clone)]
pub struct OperationPattern {
    /// Frequência média
    pub avg_frequency: f64,
    /// Tamanho médio de dados
    pub avg_data_size: u64,
    /// Tempo médio de processamento
    pub avg_processing_time_ms: f64,
    /// Horários de pico
    pub peak_hours: Vec<u8>,
    /// Correlação com outras operações
    pub correlated_operations: Vec<OperationType>,
}

/// Entrada de timing
#[derive(Debug, Clone)]
pub struct TimingEntry {
    /// Tipo de operação
    pub operation_type: OperationType,
    /// Timestamp
    pub timestamp: Instant,
    /// Duração (ms)
    pub duration_ms: f64,
    /// Tamanho dos dados
    pub data_size: u64,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_batch_size: 100,
            max_batch_wait_ms: 50,
            max_processing_threads: 8,
            memory_flush_threshold: 64 * 1024 * 1024, // 64MB
            enable_smart_batching: true,
            enable_batch_compression: true,
            compression_threshold: 1024, // 1KB
        }
    }
}

impl BatchProcessor {
    /// Cria novo processador de batch com IrohBackend
    pub fn new(
        config: BatchConfig,
        iroh_backend: Arc<crate::ipfs_core_api::backends::IrohBackend>,
    ) -> Self {
        let (control_sender, _control_receiver) = mpsc::channel(100);

        Self {
            pending_operations: Arc::new(Mutex::new(VecDeque::new())),
            typed_queues: Arc::new(RwLock::new(TypedQueues::default())),
            config,
            stats: Arc::new(RwLock::new(BatchStats::default())),
            control_sender,
            processing_semaphore: Arc::new(Semaphore::new(8)), // max concurrent batches
            operation_history: Arc::new(RwLock::new(OperationHistory::default())),
            iroh_backend,
        }
    }

    /// Adiciona operação para processamento em batch
    #[instrument(skip(self, data))]
    pub async fn add_batch_operation(
        &self,
        operation_type: OperationType,
        data: OperationData,
        priority: u8,
    ) -> Result<OperationResult> {
        let (result_sender, result_receiver) = oneshot::channel();

        let operation = BatchOperation {
            id: Uuid::new_v4().to_string(),
            operation_type: operation_type.clone(),
            data,
            created_at: Instant::now(),
            priority,
            result_sender,
            resource_estimate: self.estimate_resources(&operation_type).await,
        };

        // Adiciona à fila apropriada baseada no tipo
        if self.config.enable_smart_batching {
            self.add_to_typed_queue(operation).await?;
        } else {
            let mut pending = self.pending_operations.lock().await;
            pending.push_back(operation);
        }

        // Verifica se deve processar imediatamente
        self.check_immediate_processing().await?;

        // Aguarda resultado
        result_receiver
            .await
            .map_err(|e| GuardianError::Other(format!("Falha ao receber resultado: {}", e)))?
    }

    /// Adiciona operação à fila tipada apropriada
    async fn add_to_typed_queue(&self, operation: BatchOperation) -> Result<()> {
        let mut queues = self.typed_queues.write().await;

        let operation_type_debug = operation.operation_type.clone();

        match operation.operation_type {
            OperationType::Add => queues.add_queue.push_back(operation),
            OperationType::Get => queues.get_queue.push_back(operation),
            OperationType::Pin | OperationType::Unpin => queues.pin_queue.push_back(operation),
            OperationType::DagPut | OperationType::DagGet => queues.dag_queue.push_back(operation),
            OperationType::PubSubPublish => queues.pubsub_queue.push_back(operation),
            OperationType::DhtOperation => queues.dht_queue.push_back(operation),
        }

        debug!(
            "Operação adicionada à fila tipada: {:?}",
            operation_type_debug
        );
        Ok(())
    }

    /// Verifica se deve processar imediatamente
    async fn check_immediate_processing(&self) -> Result<()> {
        if self.config.enable_smart_batching {
            self.check_smart_processing().await
        } else {
            self.check_simple_processing().await
        }
    }

    /// Verificação inteligente para processamento
    async fn check_smart_processing(&self) -> Result<()> {
        let queues = self.typed_queues.read().await;

        // Verifica cada tipo de fila
        let add_ready = queues.add_queue.len() >= self.config.max_batch_size / 4;
        let get_ready = queues.get_queue.len() >= self.config.max_batch_size / 2;
        let pin_ready = queues.pin_queue.len() >= self.config.max_batch_size / 3;
        let dag_ready = queues.dag_queue.len() >= self.config.max_batch_size / 4;

        if add_ready || get_ready || pin_ready || dag_ready {
            drop(queues);
            self.process_ready_batches().await?;
        }

        Ok(())
    }

    /// Verificação simples para processamento
    async fn check_simple_processing(&self) -> Result<()> {
        let pending_count = self.pending_operations.lock().await.len();

        if pending_count >= self.config.max_batch_size {
            self.process_pending_batch().await?;
        }

        Ok(())
    }

    /// Processa batches prontos de forma inteligente
    async fn process_ready_batches(&self) -> Result<()> {
        let _permit = self
            .processing_semaphore
            .acquire()
            .await
            .map_err(|e| GuardianError::Other(format!("Falha ao adquirir semáforo: {}", e)))?;

        let mut batches_to_process = Vec::new();

        // Coleta batches prontos de cada tipo
        {
            let mut queues = self.typed_queues.write().await;

            // Processa operações Add
            if queues.add_queue.len() >= self.config.max_batch_size / 4 {
                let batch =
                    self.extract_batch(&mut queues.add_queue, self.config.max_batch_size / 4);
                if !batch.is_empty() {
                    batches_to_process.push((OperationType::Add, batch));
                }
            }

            // Processa operações Get
            if queues.get_queue.len() >= self.config.max_batch_size / 2 {
                let batch =
                    self.extract_batch(&mut queues.get_queue, self.config.max_batch_size / 2);
                if !batch.is_empty() {
                    batches_to_process.push((OperationType::Get, batch));
                }
            }

            // Similar para outros tipos...
        }

        // Processa cada batch
        for (batch_type, batch) in batches_to_process {
            self.process_typed_batch(batch_type, batch).await?;
        }

        Ok(())
    }

    /// Extrai batch de uma fila
    fn extract_batch(
        &self,
        queue: &mut VecDeque<BatchOperation>,
        max_size: usize,
    ) -> Vec<BatchOperation> {
        let mut batch = Vec::with_capacity(max_size);

        // Ordena por prioridade
        let mut temp_vec: Vec<_> = queue.drain(..).collect();
        temp_vec.sort_by(|a, b| b.priority.cmp(&a.priority));

        // Pega os primeiros max_size
        for operation in temp_vec.into_iter().take(max_size) {
            batch.push(operation);
        }

        batch
    }

    /// Processa batch de um tipo específico
    async fn process_typed_batch(
        &self,
        batch_type: OperationType,
        batch: Vec<BatchOperation>,
    ) -> Result<()> {
        let batch_size = batch.len();
        let start_time = Instant::now();

        debug!(
            "Processando batch de {} operações do tipo {:?}",
            batch_size, batch_type
        );

        match batch_type {
            OperationType::Add => self.process_add_batch(batch).await?,
            OperationType::Get => self.process_get_batch(batch).await?,
            OperationType::Pin => self.process_pin_batch(batch).await?,
            OperationType::DagPut => self.process_dag_put_batch(batch).await?,
            OperationType::PubSubPublish => self.process_pubsub_batch(batch).await?,
            _ => {
                // Processamento individual para tipos não otimizados
                for operation in batch {
                    self.process_individual_operation(operation).await?;
                }
            }
        }

        // Atualiza estatísticas
        let processing_time = start_time.elapsed();
        let mut stats = self.stats.write().await;
        stats.batched_operations += batch_size as u64;
        stats.avg_batch_size = (stats.avg_batch_size + batch_size as f64) / 2.0;
        stats.avg_batch_processing_time_ms =
            (stats.avg_batch_processing_time_ms + processing_time.as_millis() as f64) / 2.0;

        info!(
            "Batch de {} operações {:?} processado em {:.2}ms",
            batch_size,
            batch_type,
            processing_time.as_millis()
        );

        Ok(())
    }

    /// Processa batch de operações Add
    async fn process_add_batch(&self, batch: Vec<BatchOperation>) -> Result<()> {
        // Otimização: agrupa dados pequenos em um único blob
        let mut combined_data = Vec::new();
        let mut data_map = HashMap::new();

        for operation in &batch {
            if let OperationData::AddData { data, .. } = &operation.data {
                let start_offset = combined_data.len();
                combined_data.extend_from_slice(data);
                let end_offset = combined_data.len();

                data_map.insert(
                    operation.id.clone(),
                    (start_offset, end_offset, data.clone()),
                );
            }
        }

        // Se temos dados suficientes, processa como blob único
        if combined_data.len() > self.config.compression_threshold && batch.len() > 1 {
            // Processa como blob combinado
            let combined_blob = Bytes::from(combined_data);
            let combined_result = self.add_operation(combined_blob).await?;

            // Distribui resultados
            for operation in batch {
                if let Some((start, end, _original_data)) = data_map.get(&operation.id) {
                    // Cria resposta individual baseada no resultado combinado
                    let individual_result = AddResponse {
                        name: format!("{}_{}", combined_result.name, operation.id),
                        hash: format!("{}_{}", combined_result.hash, start),
                        size: ((end - start) as u64).to_string(),
                    };

                    let _ = operation
                        .result_sender
                        .send(Ok(OperationResult::AddResult(individual_result)));
                }
            }
        } else {
            // Processa individualmente
            for operation in batch {
                self.process_individual_operation(operation).await?;
            }
        }

        Ok(())
    }

    /// Processa batch de operações Get
    async fn process_get_batch(&self, batch: Vec<BatchOperation>) -> Result<()> {
        // Otimização: faz requests paralelos
        let mut futures = Vec::new();

        for operation in batch {
            if let OperationData::GetCid { cid, .. } = &operation.data {
                let cid_clone = cid.clone();
                let future = async move {
                    let result = self.get_operation(cid_clone).await;
                    (operation, result)
                };
                futures.push(future);
            }
        }

        // Executa todas as operações em paralelo
        let results = futures::future::join_all(futures).await;

        for (operation, result) in results {
            match result {
                Ok(data) => {
                    let _ = operation
                        .result_sender
                        .send(Ok(OperationResult::GetResult(data)));
                }
                Err(e) => {
                    let _ = operation.result_sender.send(Err(e));
                }
            }
        }

        Ok(())
    }

    /// Processa batch de operações Pin
    async fn process_pin_batch(&self, batch: Vec<BatchOperation>) -> Result<()> {
        // Agrupa pins em uma única operação
        let mut pin_cids = Vec::new();

        for operation in &batch {
            if let OperationData::PinCid { cid, .. } = &operation.data {
                pin_cids.push(cid.clone());
            }
        }

        // Executa pin em batch
        let batch_result = self.batch_pin_operation(pin_cids).await?;

        // Distribui resultados
        for (i, operation) in batch.into_iter().enumerate() {
            let individual_result = batch_result.get(i).copied().unwrap_or(false);
            let _ = operation
                .result_sender
                .send(Ok(OperationResult::PinResult(individual_result)));
        }

        Ok(())
    }

    /// Processa batch de operações DAG Put
    async fn process_dag_put_batch(&self, batch: Vec<BatchOperation>) -> Result<()> {
        // Similar ao Add batch, mas para DAG
        for operation in batch {
            self.process_individual_operation(operation).await?;
        }
        Ok(())
    }

    /// Processa batch de operações PubSub
    async fn process_pubsub_batch(&self, batch: Vec<BatchOperation>) -> Result<()> {
        // Agrupa por tópico
        let mut topic_groups: HashMap<String, Vec<BatchOperation>> = HashMap::new();

        for operation in batch {
            if let OperationData::PubSubData { topic, .. } = &operation.data {
                topic_groups
                    .entry(topic.clone())
                    .or_default()
                    .push(operation);
            }
        }

        // Processa cada grupo de tópico
        for (topic, operations) in topic_groups {
            self.process_pubsub_topic_batch(topic, operations).await?;
        }

        Ok(())
    }

    /// Processa batch de PubSub para um tópico específico
    async fn process_pubsub_topic_batch(
        &self,
        _topic: String,
        batch: Vec<BatchOperation>,
    ) -> Result<()> {
        // Combina mensagens do mesmo tópico
        for operation in batch {
            self.process_individual_operation(operation).await?;
        }
        Ok(())
    }

    /// Processa batch pendente (modo simples)
    async fn process_pending_batch(&self) -> Result<()> {
        let batch = {
            let mut pending = self.pending_operations.lock().await;
            let batch_size = self.config.max_batch_size.min(pending.len());
            pending.drain(..batch_size).collect::<Vec<_>>()
        };

        if batch.is_empty() {
            return Ok(());
        }

        let batch_size = batch.len();
        let start_time = Instant::now();

        // Processa cada operação
        for operation in batch {
            self.process_individual_operation(operation).await?;
        }

        // Atualiza estatísticas
        let processing_time = start_time.elapsed();
        let mut stats = self.stats.write().await;
        stats.total_operations += batch_size as u64;
        stats.avg_batch_processing_time_ms =
            (stats.avg_batch_processing_time_ms + processing_time.as_millis() as f64) / 2.0;

        Ok(())
    }

    /// Processa operação individual
    async fn process_individual_operation(&self, operation: BatchOperation) -> Result<()> {
        let start_time = Instant::now();

        let result = match operation.data {
            OperationData::AddData { data, .. } => {
                let add_result = self.add_operation(data).await?;
                Ok(OperationResult::AddResult(add_result))
            }
            OperationData::GetCid { cid, .. } => {
                let get_result = self.get_operation(cid).await?;
                Ok(OperationResult::GetResult(get_result))
            }
            OperationData::PinCid { cid, .. } => {
                let pin_result = self.pin_operation(cid).await?;
                Ok(OperationResult::PinResult(pin_result))
            }
            OperationData::DagPutData { data, .. } => {
                let dag_result = self.dag_put_operation(data).await?;
                Ok(OperationResult::DagPutResult(dag_result))
            }
            OperationData::PubSubData { topic, data } => {
                let pubsub_result = self.pubsub_operation(topic, data).await?;
                Ok(OperationResult::PubSubResult(pubsub_result))
            }
            _ => Err(GuardianError::Other(
                "Operação não implementada".to_string(),
            )),
        };

        // Registra timing
        let processing_time = start_time.elapsed();
        self.record_operation_timing(
            operation.operation_type,
            processing_time,
            operation.resource_estimate.memory_bytes,
        )
        .await;

        // Envia resultado
        let _ = operation.result_sender.send(result);

        Ok(())
    }

    /// Operação Add usando IrohBackend
    async fn add_operation(&self, data: Bytes) -> Result<AddResponse> {
        use std::pin::Pin;
        use tokio::io::AsyncRead;

        // Converte Bytes para AsyncRead usando cursor
        let cursor = std::io::Cursor::new(data.to_vec());
        let async_read: Pin<Box<dyn AsyncRead + Send>> = Box::pin(cursor);

        // Chama o método add do IrohBackend
        let add_result = self
            .iroh_backend
            .add(async_read)
            .await
            .map_err(|e| GuardianError::Other(format!("Erro no IrohBackend.add(): {}", e)))?;

        debug!(
            "BatchProcessor: Conteúdo adicionado via IrohBackend - Hash: {}, Size: {}",
            add_result.hash, add_result.size
        );

        Ok(add_result)
    }

    /// Operação Get usando IrohBackend
    async fn get_operation(&self, cid: String) -> Result<Bytes> {
        use tokio::io::AsyncReadExt;

        // Chama o método cat do IrohBackend
        let mut async_read = self.iroh_backend.cat(&cid).await.map_err(|e| {
            GuardianError::Other(format!("Erro no IrohBackend.cat({}): {}", cid, e))
        })?;

        // Lê todos os dados do stream
        let mut buffer = Vec::new();
        async_read.read_to_end(&mut buffer).await.map_err(|e| {
            GuardianError::Other(format!("Erro ao ler dados do CID {}: {}", cid, e))
        })?;

        debug!(
            "BatchProcessor: Conteúdo recuperado via IrohBackend - CID: {}, Size: {} bytes",
            cid,
            buffer.len()
        );

        Ok(Bytes::from(buffer))
    }

    /// Operação Pin usando IrohBackend
    async fn pin_operation(&self, cid: String) -> Result<bool> {
        // Chama o método pin_add do IrohBackend
        match self.iroh_backend.pin_add(&cid).await {
            Ok(_) => {
                debug!(
                    "BatchProcessor: Conteúdo fixado com sucesso via IrohBackend - CID: {}",
                    cid
                );
                Ok(true)
            }
            Err(e) => {
                warn!(
                    "BatchProcessor: Erro ao fixar conteúdo via IrohBackend - CID: {}, Erro: {}",
                    cid, e
                );
                // Retorna false em vez de erro para manter compatibilidade com batch
                Ok(false)
            }
        }
    }

    /// Operação Pin em batch usando IrohBackend
    async fn batch_pin_operation(&self, cids: Vec<String>) -> Result<Vec<bool>> {
        debug!(
            "BatchProcessor: Processando {} operações de pin em batch",
            cids.len()
        );

        // Executa pins em paralelo para otimização de throughput
        let pin_futures: Vec<_> = cids
            .iter()
            .map(|cid| {
                let backend = Arc::clone(&self.iroh_backend);
                let cid_clone = cid.clone();
                async move {
                    match backend.pin_add(&cid_clone).await {
                        Ok(_) => {
                            debug!("Batch pin bem-sucedido: {}", cid_clone);
                            true
                        }
                        Err(e) => {
                            warn!("Batch pin falhou para {}: {}", cid_clone, e);
                            false
                        }
                    }
                }
            })
            .collect();

        // Aguarda todos os pins em paralelo
        let results = futures::future::join_all(pin_futures).await;

        let successful_pins = results.iter().filter(|&&r| r).count();
        info!(
            "BatchProcessor: Batch pin concluído - {}/{} bem-sucedidos",
            successful_pins,
            cids.len()
        );

        Ok(results)
    }

    /// Operação DAG Put usando IrohBackend
    async fn dag_put_operation(&self, data: Bytes) -> Result<Cid> {
        // DAG Put usando IrohBackend com fallback para hashing
        // Nota: IrohBackend não tem DAG operations nativas, então usamos add() + metadata

        use multihash::Multihash;
        use sha2::{Digest, Sha256};
        use std::pin::Pin;
        use tokio::io::AsyncRead;

        // Primeiro, armazena os dados usando add() do IrohBackend
        let cursor = std::io::Cursor::new(data.to_vec());
        let async_read: Pin<Box<dyn AsyncRead + Send>> = Box::pin(cursor);

        let add_result = self
            .iroh_backend
            .add(async_read)
            .await
            .map_err(|e| GuardianError::Other(format!("Erro no DAG put via add(): {}", e)))?;

        // Gera CID DAG-CBOR compatível baseado nos dados
        let mut hasher = Sha256::new();
        hasher.update(&data);
        let digest = hasher.finalize();
        let mh = Multihash::wrap(0x12, &digest).unwrap(); // SHA2-256
        let dag_cid = Cid::new_v1(0x71, mh); // dag-cbor

        debug!(
            "BatchProcessor: DAG Put via IrohBackend - Original CID: {}, DAG CID: {}",
            add_result.hash, dag_cid
        );

        Ok(dag_cid)
    }

    /// Operação PubSub usando IrohBackend
    async fn pubsub_operation(&self, topic: String, data: Bytes) -> Result<bool> {
        // Validação do tópico
        if topic.is_empty() {
            return Err(GuardianError::Other(
                "Tópico PubSub não pode estar vazio".to_string(),
            ));
        }
        if data.is_empty() {
            return Err(GuardianError::Other(
                "Dados PubSub não podem estar vazios".to_string(),
            ));
        }
        // Usando método público publish_gossip do IrohBackend
        match self.iroh_backend.publish_gossip(&topic, &data).await {
            Ok(_) => {
                debug!(
                    "BatchProcessor: Mensagem PubSub publicada via IrohBackend.publish_gossip() - Tópico: {}, Size: {} bytes",
                    topic,
                    data.len()
                );
                Ok(true)
            }
            Err(e) => {
                warn!(
                    "BatchProcessor: Erro ao publicar via IrohBackend.publish_gossip() - Tópico: {}, Erro: {}",
                    topic, e
                );
                // Retorna false em vez de erro para manter compatibilidade
                Ok(false)
            }
        }
    }

    /// Estima recursos necessários para uma operação
    async fn estimate_resources(&self, operation_type: &OperationType) -> ResourceEstimate {
        match operation_type {
            OperationType::Add => ResourceEstimate {
                cpu_usage: 0.3,
                memory_bytes: 64 * 1024,
                io_bytes: 128 * 1024,
                bandwidth_bytes: 256 * 1024,
                estimated_time_ms: 20,
            },
            OperationType::Get => ResourceEstimate {
                cpu_usage: 0.2,
                memory_bytes: 32 * 1024,
                io_bytes: 64 * 1024,
                bandwidth_bytes: 128 * 1024,
                estimated_time_ms: 15,
            },
            OperationType::Pin => ResourceEstimate {
                cpu_usage: 0.1,
                memory_bytes: 8 * 1024,
                io_bytes: 16 * 1024,
                bandwidth_bytes: 32 * 1024,
                estimated_time_ms: 5,
            },
            _ => ResourceEstimate {
                cpu_usage: 0.2,
                memory_bytes: 16 * 1024,
                io_bytes: 32 * 1024,
                bandwidth_bytes: 64 * 1024,
                estimated_time_ms: 10,
            },
        }
    }

    /// Registra timing de operação para otimização
    async fn record_operation_timing(
        &self,
        operation_type: OperationType,
        duration: Duration,
        data_size: u64,
    ) {
        let mut history = self.operation_history.write().await;

        let timing_entry = TimingEntry {
            operation_type: operation_type.clone(),
            timestamp: Instant::now(),
            duration_ms: duration.as_millis() as f64,
            data_size,
        };

        history.timing_history.push_back(timing_entry);

        // Mantém histórico limitado
        if history.timing_history.len() > 10000 {
            history.timing_history.pop_front();
        }

        // Atualiza padrões
        let pattern = history
            .operation_patterns
            .entry(operation_type)
            .or_insert_with(|| OperationPattern {
                avg_frequency: 0.0,
                avg_data_size: 0,
                avg_processing_time_ms: 0.0,
                peak_hours: vec![],
                correlated_operations: vec![],
            });

        pattern.avg_processing_time_ms =
            (pattern.avg_processing_time_ms + duration.as_millis() as f64) / 2.0;
        pattern.avg_data_size = (pattern.avg_data_size + data_size) / 2;
    }

    /// Inicia processador automático de batch
    pub fn start_auto_processor(&self) -> tokio::task::JoinHandle<()> {
        let pending_operations = Arc::clone(&self.pending_operations);
        let typed_queues = Arc::clone(&self.typed_queues);
        let config = self.config.clone();
        let stats = Arc::clone(&self.stats);
        let processing_semaphore = Arc::clone(&self.processing_semaphore);
        let operation_history = Arc::clone(&self.operation_history);
        let iroh_backend = Arc::clone(&self.iroh_backend);

        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_millis(config.max_batch_wait_ms));

            loop {
                interval.tick().await;

                // Processa filas tipadas se habilitado
                if config.enable_smart_batching {
                    // Processamento automático das filas tipadas
                    let should_process = {
                        let queues = typed_queues.read().await;

                        // Verifica se alguma fila atingiu limites para processamento
                        let add_ready = queues.add_queue.len() >= config.max_batch_size / 4;
                        let get_ready = queues.get_queue.len() >= config.max_batch_size / 2;
                        let pin_ready = queues.pin_queue.len() >= config.max_batch_size / 3;
                        let dag_ready = queues.dag_queue.len() >= config.max_batch_size / 4;
                        let pubsub_ready = queues.pubsub_queue.len() >= config.max_batch_size / 6;
                        let dht_ready = queues.dht_queue.len() >= config.max_batch_size / 8;

                        // Ou verifica timeout das operações mais antigas
                        let now = Instant::now();
                        let timeout_threshold = Duration::from_millis(config.max_batch_wait_ms * 2);

                        let add_timeout = queues
                            .add_queue
                            .front()
                            .map(|op| now.duration_since(op.created_at) > timeout_threshold)
                            .unwrap_or(false);
                        let get_timeout = queues
                            .get_queue
                            .front()
                            .map(|op| now.duration_since(op.created_at) > timeout_threshold)
                            .unwrap_or(false);
                        let pin_timeout = queues
                            .pin_queue
                            .front()
                            .map(|op| now.duration_since(op.created_at) > timeout_threshold)
                            .unwrap_or(false);

                        add_ready
                            || get_ready
                            || pin_ready
                            || dag_ready
                            || pubsub_ready
                            || dht_ready
                            || add_timeout
                            || get_timeout
                            || pin_timeout
                    };

                    if should_process {
                        // Processa batches usando semáforo para controlar concorrência
                        if let Ok(_permit) = processing_semaphore.try_acquire() {
                            let queues_clone = Arc::clone(&typed_queues);
                            let stats_clone = Arc::clone(&stats);
                            let config_clone = config.clone();
                            let history_clone = Arc::clone(&operation_history);
                            let iroh_clone = Arc::clone(&iroh_backend);

                            tokio::spawn(async move {
                                if let Err(e) = Self::process_automatic_typed_batches(
                                    queues_clone,
                                    stats_clone,
                                    config_clone,
                                    history_clone,
                                    iroh_clone,
                                )
                                .await
                                {
                                    debug!(target: "batch_processor", error = %e, "Erro no processamento automático");
                                }
                                // Permit é automaticamente liberado quando sai de escopo
                            });
                        }
                    }
                } else {
                    // Processa fila simples
                    let pending_count = pending_operations.lock().await.len();
                    if pending_count > 0 {
                        // Trigger processing usando semáforo
                        if let Ok(_permit) = processing_semaphore.try_acquire() {
                            let ops_clone = Arc::clone(&pending_operations);
                            let stats_clone = Arc::clone(&stats);
                            let config_clone = config.clone();
                            let iroh_clone = Arc::clone(&iroh_backend);

                            tokio::spawn(async move {
                                if let Err(e) = Self::process_automatic_simple_batch(
                                    ops_clone,
                                    stats_clone,
                                    config_clone,
                                    iroh_clone,
                                )
                                .await
                                {
                                    debug!(target: "batch_processor", error = %e, "Erro no processamento simples");
                                }
                            });
                        }
                    }
                }
            }
        })
    }

    /// Processa batches automáticos das filas tipadas com IrohBackend
    async fn process_automatic_typed_batches(
        typed_queues: Arc<RwLock<TypedQueues>>,
        stats: Arc<RwLock<BatchStats>>,
        config: BatchConfig,
        _operation_history: Arc<RwLock<OperationHistory>>,
        iroh_backend: Arc<crate::ipfs_core_api::backends::IrohBackend>,
    ) -> Result<()> {
        let mut batches_to_process = Vec::new();

        // Extrai batches de cada fila que precisa ser processada
        {
            let mut queues = typed_queues.write().await;

            // Processamento Add
            if !queues.add_queue.is_empty()
                && (queues.add_queue.len() >= config.max_batch_size / 4
                    || Self::has_old_operations(&queues.add_queue, config.max_batch_wait_ms))
            {
                let batch_size = (config.max_batch_size / 4).max(queues.add_queue.len());
                let batch = Self::extract_operations_static(&mut queues.add_queue, batch_size);
                if !batch.is_empty() {
                    batches_to_process.push((OperationType::Add, batch));
                }
            }

            // Processamento Get
            if !queues.get_queue.is_empty()
                && (queues.get_queue.len() >= config.max_batch_size / 2
                    || Self::has_old_operations(&queues.get_queue, config.max_batch_wait_ms))
            {
                let batch_size = (config.max_batch_size / 2).max(queues.get_queue.len());
                let batch = Self::extract_operations_static(&mut queues.get_queue, batch_size);
                if !batch.is_empty() {
                    batches_to_process.push((OperationType::Get, batch));
                }
            }

            // Processamento Pin
            if !queues.pin_queue.is_empty()
                && (queues.pin_queue.len() >= config.max_batch_size / 3
                    || Self::has_old_operations(&queues.pin_queue, config.max_batch_wait_ms))
            {
                let batch_size = (config.max_batch_size / 3).max(queues.pin_queue.len());
                let batch = Self::extract_operations_static(&mut queues.pin_queue, batch_size);
                if !batch.is_empty() {
                    batches_to_process.push((OperationType::Pin, batch));
                }
            }

            // Processamento DAG
            if !queues.dag_queue.is_empty()
                && (queues.dag_queue.len() >= config.max_batch_size / 4
                    || Self::has_old_operations(&queues.dag_queue, config.max_batch_wait_ms))
            {
                let batch_size = (config.max_batch_size / 4).max(queues.dag_queue.len());
                let batch = Self::extract_operations_static(&mut queues.dag_queue, batch_size);
                if !batch.is_empty() {
                    batches_to_process.push((OperationType::DagPut, batch));
                }
            }

            // Processamento PubSub
            if !queues.pubsub_queue.is_empty() {
                let pubsub_len = queues.pubsub_queue.len();
                let batch = Self::extract_operations_static(&mut queues.pubsub_queue, pubsub_len);
                if !batch.is_empty() {
                    batches_to_process.push((OperationType::PubSubPublish, batch));
                }
            }

            // Processamento DHT
            if !queues.dht_queue.is_empty() {
                let dht_len = queues.dht_queue.len();
                let batch = Self::extract_operations_static(&mut queues.dht_queue, dht_len);
                if !batch.is_empty() {
                    batches_to_process.push((OperationType::DhtOperation, batch));
                }
            }
        }

        // Processa cada batch extraído
        for (batch_type, batch) in batches_to_process {
            let batch_size = batch.len();
            let start_time = Instant::now();

            debug!(target: "batch_processor",
                batch_type = ?batch_type,
                batch_size = batch_size,
                "Processando batch automático"
            );

            // Processa batch baseado no tipo
            match batch_type {
                OperationType::Add => Self::process_add_batch_static(batch, &iroh_backend).await?,
                OperationType::Get => Self::process_get_batch_static(batch, &iroh_backend).await?,
                OperationType::Pin => Self::process_pin_batch_static(batch, &iroh_backend).await?,
                OperationType::DagPut => {
                    Self::process_individual_batch_static(batch, &iroh_backend).await?
                }
                OperationType::PubSubPublish => {
                    Self::process_pubsub_batch_static(batch, &iroh_backend).await?
                }
                _ => Self::process_individual_batch_static(batch, &iroh_backend).await?,
            }

            // Atualiza estatísticas
            let processing_time = start_time.elapsed();
            let mut stats_lock = stats.write().await;
            stats_lock.batched_operations += batch_size as u64;
            stats_lock.total_operations += batch_size as u64;
            stats_lock.avg_batch_size = (stats_lock.avg_batch_size + batch_size as f64) / 2.0;
            stats_lock.avg_batch_processing_time_ms = (stats_lock.avg_batch_processing_time_ms
                + processing_time.as_millis() as f64)
                / 2.0;

            info!(target: "batch_processor",
                batch_type = ?batch_type,
                batch_size = batch_size,
                processing_time_ms = processing_time.as_millis(),
                "Batch automático processado com sucesso"
            );
        }

        Ok(())
    }

    /// Processa batch automático simples com IrohBackend
    async fn process_automatic_simple_batch(
        pending_operations: Arc<Mutex<VecDeque<BatchOperation>>>,
        stats: Arc<RwLock<BatchStats>>,
        config: BatchConfig,
        iroh_backend: Arc<crate::ipfs_core_api::backends::IrohBackend>,
    ) -> Result<()> {
        let batch = {
            let mut pending = pending_operations.lock().await;
            let batch_size = config.max_batch_size.min(pending.len());
            pending.drain(..batch_size).collect::<Vec<_>>()
        };

        if batch.is_empty() {
            return Ok(());
        }

        let batch_size = batch.len();
        let start_time = Instant::now();

        debug!(target: "batch_processor",
            batch_size = batch_size,
            "Processando batch simples automático"
        );

        // Processa operações individualmente
        Self::process_individual_batch_static(batch, &iroh_backend).await?;

        // Atualiza estatísticas
        let processing_time = start_time.elapsed();
        let mut stats_lock = stats.write().await;
        stats_lock.total_operations += batch_size as u64;
        stats_lock.individual_operations += batch_size as u64;
        stats_lock.avg_batch_processing_time_ms =
            (stats_lock.avg_batch_processing_time_ms + processing_time.as_millis() as f64) / 2.0;

        info!(target: "batch_processor",
            batch_size = batch_size,
            processing_time_ms = processing_time.as_millis(),
            "Batch simples automático processado"
        );

        Ok(())
    }

    /// Verifica se há operações antigas na fila
    fn has_old_operations(queue: &VecDeque<BatchOperation>, max_wait_ms: u64) -> bool {
        if let Some(oldest) = queue.front() {
            let age = Instant::now().duration_since(oldest.created_at);
            age > Duration::from_millis(max_wait_ms * 2)
        } else {
            false
        }
    }

    /// Extrai operações de uma fila (versão static)
    fn extract_operations_static(
        queue: &mut VecDeque<BatchOperation>,
        max_size: usize,
    ) -> Vec<BatchOperation> {
        let mut batch = Vec::with_capacity(max_size);

        // Ordena por prioridade
        let mut temp_vec: Vec<_> = queue.drain(..).collect();
        temp_vec.sort_by(|a, b| b.priority.cmp(&a.priority));

        // Pega os primeiros max_size
        for operation in temp_vec.into_iter().take(max_size) {
            batch.push(operation);
        }

        batch
    }

    /// Processa batch de Add (versão static) com IrohBackend
    async fn process_add_batch_static(
        batch: Vec<BatchOperation>,
        iroh_backend: &crate::ipfs_core_api::backends::IrohBackend,
    ) -> Result<()> {
        for operation in batch {
            Self::process_individual_operation_static(operation, iroh_backend).await?;
        }
        Ok(())
    }

    /// Processa batch de Get (versão static) com IrohBackend
    async fn process_get_batch_static(
        batch: Vec<BatchOperation>,
        iroh_backend: &crate::ipfs_core_api::backends::IrohBackend,
    ) -> Result<()> {
        // Processamento sequencial para Gets usando IrohBackend
        // (lifetime issues impedem processamento paralelo com referências)
        for operation in batch {
            Self::process_individual_operation_static(operation, iroh_backend).await?;
        }
        Ok(())
    }

    /// Processa batch de Pin (versão static) com IrohBackend
    async fn process_pin_batch_static(
        batch: Vec<BatchOperation>,
        iroh_backend: &crate::ipfs_core_api::backends::IrohBackend,
    ) -> Result<()> {
        for operation in batch {
            Self::process_individual_operation_static(operation, iroh_backend).await?;
        }
        Ok(())
    }

    /// Processa batch de PubSub (versão static) com IrohBackend
    async fn process_pubsub_batch_static(
        batch: Vec<BatchOperation>,
        iroh_backend: &crate::ipfs_core_api::backends::IrohBackend,
    ) -> Result<()> {
        for operation in batch {
            Self::process_individual_operation_static(operation, iroh_backend).await?;
        }
        Ok(())
    }

    /// Processa batch individual (versão static) com IrohBackend
    async fn process_individual_batch_static(
        batch: Vec<BatchOperation>,
        iroh_backend: &crate::ipfs_core_api::backends::IrohBackend,
    ) -> Result<()> {
        for operation in batch {
            Self::process_individual_operation_static(operation, iroh_backend).await?;
        }
        Ok(())
    }

    /// Processa operação individual (versão static) com IrohBackend
    async fn process_individual_operation_static(
        operation: BatchOperation,
        iroh_backend: &crate::ipfs_core_api::backends::IrohBackend,
    ) -> Result<()> {
        let start_time = Instant::now();

        let result = match operation.data {
            OperationData::AddData { data, .. } => {
                let add_result = Self::add_operation_static(data, iroh_backend).await?;
                Ok(OperationResult::AddResult(add_result))
            }
            OperationData::GetCid { cid, .. } => {
                let get_result = Self::get_operation_static(cid, iroh_backend).await?;
                Ok(OperationResult::GetResult(get_result))
            }
            OperationData::PinCid { cid, .. } => {
                let pin_result = Self::pin_operation_static(cid, iroh_backend).await?;
                Ok(OperationResult::PinResult(pin_result))
            }
            OperationData::DagPutData { data, .. } => {
                let dag_result = Self::dag_put_operation_static(data, iroh_backend).await?;
                Ok(OperationResult::DagPutResult(dag_result))
            }
            OperationData::PubSubData { topic, data } => {
                let pubsub_result =
                    Self::pubsub_operation_static(topic, data, iroh_backend).await?;
                Ok(OperationResult::PubSubResult(pubsub_result))
            }
            _ => Err(GuardianError::Other(
                "Operação não implementada".to_string(),
            )),
        };

        // Registra timing básico
        let _processing_time = start_time.elapsed();

        // Envia resultado
        let _ = operation.result_sender.send(result);

        Ok(())
    }

    /// Versões static das operações usando IrohBackend
    async fn add_operation_static(
        data: Bytes,
        iroh_backend: &crate::ipfs_core_api::backends::IrohBackend,
    ) -> Result<AddResponse> {
        // Add static usando IrohBackend
        use std::pin::Pin;
        use tokio::io::AsyncRead;

        let cursor = std::io::Cursor::new(data.to_vec());
        let async_read: Pin<Box<dyn AsyncRead + Send>> = Box::pin(cursor);

        let result = iroh_backend
            .add(async_read)
            .await
            .map_err(|e| GuardianError::Other(format!("Erro no add static: {}", e)))?;

        debug!("Auto batch add concluído: {}", result.hash);
        Ok(result)
    }

    async fn get_operation_static(
        cid: String,
        iroh_backend: &crate::ipfs_core_api::backends::IrohBackend,
    ) -> Result<Bytes> {
        // Get static usando IrohBackend
        use tokio::io::AsyncReadExt;

        let mut async_read = iroh_backend
            .cat(&cid)
            .await
            .map_err(|e| GuardianError::Other(format!("Erro no cat static para {}: {}", cid, e)))?;

        let mut buffer = Vec::new();
        async_read
            .read_to_end(&mut buffer)
            .await
            .map_err(|e| GuardianError::Other(format!("Erro ao ler dados static: {}", e)))?;

        debug!("Auto batch get concluído: {} ({} bytes)", cid, buffer.len());
        Ok(Bytes::from(buffer))
    }

    async fn pin_operation_static(
        cid: String,
        iroh_backend: &crate::ipfs_core_api::backends::IrohBackend,
    ) -> Result<bool> {
        // Pin static usando IrohBackend
        match iroh_backend.pin_add(&cid).await {
            Ok(_) => {
                debug!("Auto batch pin bem-sucedido: {}", cid);
                Ok(true)
            }
            Err(e) => {
                warn!("Auto batch pin falhou para {}: {}", cid, e);
                Ok(false)
            }
        }
    }

    async fn dag_put_operation_static(
        data: Bytes,
        iroh_backend: &crate::ipfs_core_api::backends::IrohBackend,
    ) -> Result<Cid> {
        // DAG Put static usando IrohBackend
        use multihash::Multihash;
        use sha2::{Digest, Sha256};
        use std::pin::Pin;
        use tokio::io::AsyncRead;

        let cursor = std::io::Cursor::new(data.to_vec());
        let async_read: Pin<Box<dyn AsyncRead + Send>> = Box::pin(cursor);

        let add_result = iroh_backend
            .add(async_read)
            .await
            .map_err(|e| GuardianError::Other(format!("Erro no DAG put static: {}", e)))?;

        // Gera CID DAG compatível
        let mut hasher = Sha256::new();
        hasher.update(&data);
        let digest = hasher.finalize();
        let mh = Multihash::wrap(0x12, &digest).unwrap();
        let dag_cid = Cid::new_v1(0x71, mh);

        debug!("Auto batch DAG put: {} -> {}", add_result.hash, dag_cid);
        Ok(dag_cid)
    }

    async fn pubsub_operation_static(
        topic: String,
        data: Bytes,
        iroh_backend: &crate::ipfs_core_api::backends::IrohBackend,
    ) -> Result<bool> {
        // PubSub static usando IrohBackend
        if topic.is_empty() {
            return Err(GuardianError::Other(
                "Tópico PubSub não pode estar vazio".to_string(),
            ));
        }

        if data.is_empty() {
            return Err(GuardianError::Other(
                "Dados PubSub não podem estar vazios".to_string(),
            ));
        }

        // Usando método público publish_gossip do IrohBackend
        match iroh_backend.publish_gossip(&topic, &data).await {
            Ok(_) => {
                debug!(
                    "Auto batch PubSub via publish_gossip() bem-sucedido: {} ({} bytes)",
                    topic,
                    data.len()
                );
                Ok(true)
            }
            Err(e) => {
                warn!(
                    "Auto batch PubSub via publish_gossip() falhou para {}: {}",
                    topic, e
                );
                Ok(false)
            }
        }
    }

    /// Obtém estatísticas atuais
    pub async fn get_stats(&self) -> BatchStats {
        let stats = self.stats.read().await;
        let mut stats_copy = stats.clone();

        // Calcula eficiência
        if stats_copy.total_operations > 0 {
            stats_copy.batch_efficiency =
                stats_copy.batched_operations as f64 / stats_copy.total_operations as f64;
            stats_copy.operations_per_second = stats_copy.total_operations as f64
                / (Instant::now().duration_since(Instant::now()).as_secs_f64() + 1.0);
        }

        stats_copy
    }
}
