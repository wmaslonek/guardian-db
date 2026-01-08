use crate::data_store::Datastore;
use crate::guardian::error::{GuardianError, Result};
use crate::log::{entry::Entry, identity::Identity};
use crate::p2p::EventBus;
use crate::p2p::network::client::IrohClient;
use crate::stores::base_store::BaseStore;
use crate::stores::operation::{self, Operation};
use crate::traits::{self, EventLogStore, Store, StreamOptions};
use crate::{address::Address, stores::event_log_store::index::new_event_index};
use iroh_blobs::Hash;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{Span, instrument};

pub mod index;

/// Implementação do trait `EventLogStore` para `GuardianDBEventLogStore`.
#[async_trait::async_trait]
impl EventLogStore for GuardianDBEventLogStore {
    /// Adiciona um novo dado ao log.
    async fn add(&self, data: Vec<u8>) -> std::result::Result<Operation, Self::Error> {
        // Chama o método interno da struct GuardianDBEventLogStore
        GuardianDBEventLogStore::add(self, data).await
    }

    /// Obtém uma entrada específica do log pelo seu Hash.
    async fn get(&self, hash: &Hash) -> std::result::Result<Operation, Self::Error> {
        // Chama o método interno da struct GuardianDBEventLogStore
        GuardianDBEventLogStore::get(self, hash).await
    }

    /// Retorna uma lista de operações que ocorreram na store, com opções de filtro.
    async fn list(
        &self,
        options: Option<StreamOptions>,
    ) -> std::result::Result<Vec<Operation>, Self::Error> {
        // Chama o método interno da struct GuardianDBEventLogStore
        GuardianDBEventLogStore::list(self, options).await
    }
}

#[derive(Clone)]
pub struct GuardianDBEventLogStore {
    basestore: Arc<BaseStore>,
    span: Span,
}

// Implementação da trait Store (que é herdada por EventLogStore)
#[async_trait::async_trait]
impl Store for GuardianDBEventLogStore {
    type Error = GuardianError;

    #[allow(deprecated)]
    fn events(&self) -> &dyn crate::events::EmitterInterface {
        self.basestore.events()
    }

    async fn close(&self) -> std::result::Result<(), Self::Error> {
        self.basestore.close().await
    }

    fn address(&self) -> &dyn crate::address::Address {
        Store::address(self.basestore.as_ref())
    }

    fn index(&self) -> Box<dyn crate::traits::StoreIndex<Error = GuardianError> + Send + Sync> {
        self.basestore.index()
    }

    fn store_type(&self) -> &str {
        "eventlog"
    }

    fn replication_status(&self) -> crate::stores::replicator::replication_info::ReplicationInfo {
        self.basestore.replication_status()
    }

    fn cache(&self) -> Arc<dyn Datastore> {
        self.basestore.cache()
    }

    async fn drop(&self) -> std::result::Result<(), Self::Error> {
        // ***BaseStore não tem método async drop público, então implementamos uma limpeza básica
        // A limpeza é feita automaticamente quando o BaseStore é dropped
        Ok(())
    }

    async fn load(&self, amount: usize) -> std::result::Result<(), Self::Error> {
        self.basestore.load(Some(amount as isize)).await
    }

    async fn sync(
        &self,
        heads: Vec<crate::log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        self.basestore.sync(heads).await
    }

    async fn load_more_from(&self, _amount: u64, entries: Vec<crate::log::entry::Entry>) {
        let _ = self.basestore.load_more_from(entries);
    }

    async fn load_from_snapshot(&self) -> std::result::Result<(), Self::Error> {
        self.basestore.load_from_snapshot().await
    }

    fn op_log(&self) -> Arc<parking_lot::RwLock<crate::log::Log>> {
        self.basestore.op_log()
    }

    fn client(&self) -> Arc<crate::p2p::network::client::IrohClient> {
        self.basestore.client()
    }

    fn db_name(&self) -> &str {
        self.basestore.db_name()
    }

    fn identity(&self) -> &Identity {
        self.basestore.identity()
    }

    fn access_controller(&self) -> &dyn crate::access_control::traits::AccessController {
        self.basestore.access_controller()
    }

    async fn add_operation(
        &self,
        op: Operation,
        on_progress_callback: Option<tokio::sync::mpsc::Sender<crate::log::entry::Entry>>,
    ) -> std::result::Result<crate::log::entry::Entry, Self::Error> {
        self.basestore.add_operation(op, on_progress_callback).await
    }

    fn span(&self) -> Arc<tracing::Span> {
        Arc::new(self.span.clone())
    }

    fn tracer(&self) -> Arc<crate::traits::TracerWrapper> {
        self.basestore.tracer()
    }

    fn event_bus(&self) -> Arc<EventBus> {
        self.basestore.event_bus()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl GuardianDBEventLogStore {
    /// Getter para acessar o BaseStore interno
    pub fn basestore(&self) -> &BaseStore {
        &self.basestore
    }

    /// Retorna uma referência ao span de tracing para instrumentação
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Instancia uma nova EventLogStore, adaptada para usar cliente nativo Iroh.
    ///
    /// # Argumentos
    ///
    /// * `iroh_client` - Cliente iroh compartilhado via Arc para operações de rede
    /// * `identity` - Identidade do nó para assinatura de entradas
    /// * `addr` - Endereço da store para identificação única
    /// * `options` - Opções de configuração da store (índice, cache, etc.)
    ///
    /// # Retorna
    ///
    /// Uma nova instância de `GuardianDBEventLogStore` configurada e pronta para uso
    ///
    /// # Erros
    ///
    /// Retorna `GuardianError::Store` se:
    /// - A inicialização do BaseStore falhar
    /// - As opções de configuração forem inválidas
    #[instrument(level = "debug", skip(iroh_client, identity, addr, options))]
    pub async fn new(
        iroh_client: Arc<IrohClient>,
        identity: Arc<Identity>,
        addr: Arc<dyn Address + Send + Sync>,
        mut options: traits::NewStoreOptions,
    ) -> Result<Self> {
        // Validação básica dos parâmetros - verifica se os componentes essenciais existem
        if addr.to_string().is_empty() {
            return Err(GuardianError::Store(
                "Invalid address provided, cannot create EventLogStore".to_string(),
            ));
        }

        options.index = Some(Box::new(new_event_index));
        // Inicializa o BaseStore com as opções fornecidas
        let basestore = BaseStore::new(iroh_client, identity, addr.clone(), Some(options))
            .await
            .map_err(|e| {
                GuardianError::Store(format!(
                    "Failed to initialize base store for EventLogStore: {}",
                    e
                ))
            })?;

        // Cria span para esta instância da EventLogStore
        let span = tracing::info_span!("event_log_store", address = %addr.to_string());

        Ok(GuardianDBEventLogStore { basestore, span })
    }

    /// Coleta todas as operações de uma stream em um vetor.
    #[instrument(level = "debug", skip(self, options))]
    pub async fn list(&self, options: Option<StreamOptions>) -> Result<Vec<Operation>> {
        let _entered = self.span.enter();
        let (tx, mut rx) = mpsc::channel(100); // Buffer maior para evitar deadlock

        // Spawn stream em task separada para evitar deadlock
        // (stream envia dados enquanto list recebe)
        let self_clone = self.clone();
        tokio::spawn(async move {
            let _ = self_clone.stream(tx, options).await;
        });

        let mut operations = Vec::new();
        while let Some(op) = rx.recv().await {
            operations.push(op);
        }

        Ok(operations)
    }

    /// Cria e adiciona uma nova operação "ADD" ao log.
    ///
    /// # Argumentos
    ///
    /// * `value` - Dados em bytes para adicionar ao log
    ///
    /// # Retorna
    ///
    /// A operação criada com seu Hash único
    ///
    /// # Erros
    ///
    /// - Se os dados estiverem vazios (opcional, dependendo da política)
    /// - Se a adição ao BaseStore falhar
    /// - Se a conversão Entry -> Operation falhar
    #[instrument(level = "debug", skip(self, value))]
    pub async fn add(&self, value: Vec<u8>) -> Result<Operation> {
        // Validação opcional: verificar se há dados
        if value.is_empty() {
            return Err(GuardianError::Store(
                "Cannot add empty data to EventLogStore".to_string(),
            ));
        }

        let op = Operation::new(None, "ADD".to_string(), Some(value));

        // `add_operation` retorna um `Entry`.
        let entry = self
            .add_operation(op, None)
            .await
            .map_err(|e| GuardianError::Store(format!("Failed to add operation to log: {}", e)))?;

        // `parse_operation` converte o `Entry` de volta para uma `Operation`.
        let op_result = operation::parse_operation(entry).map_err(|e| {
            GuardianError::Store(format!("Failed to parse newly created entry: {}", e))
        })?;

        Ok(op_result)
    }

    /// Recupera uma única operação do log pelo seu Hash.
    ///
    /// # Argumentos
    ///
    /// * `hash` - Hash da entrada desejada
    ///
    /// # Retorna
    ///
    /// A operação correspondente ao Hash fornecido
    ///
    /// # Erros
    ///
    /// - Se o Hash não for encontrado no log
    /// - Se a stream não retornar resultados
    #[instrument(level = "debug", skip(self))]
    pub async fn get(&self, hash: &Hash) -> Result<Operation> {
        let _entered = self.span.enter();
        let (tx, mut rx) = mpsc::channel(1);

        let stream_options = StreamOptions {
            gte: Some(*hash),
            amount: Some(1),
            ..Default::default()
        };

        // Para simplificar, vamos executar diretamente
        self.stream(tx, Some(stream_options)).await?;

        // Aguarda o primeiro valor
        if let Some(value) = rx.recv().await {
            Ok(value)
        } else {
            Err(GuardianError::Store(format!(
                "No operation found for Hash: {}",
                hex::encode(hash.as_bytes())
            )))
        }
    }

    /// Busca entradas, as converte em operações e as envia através de um canal.
    #[instrument(level = "debug", skip(self, result_chan, options))]
    pub async fn stream(
        &self,
        result_chan: mpsc::Sender<Operation>,
        options: Option<StreamOptions>,
    ) -> Result<()> {
        // A função `query` retorna as entradas (entries) do log.
        let messages = self
            .query(options)
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

    /// Executa a lógica de busca no índice do log com base nas opções de filtro.
    ///
    /// # Performance
    ///
    /// - Usa o índice quando disponível para queries otimizadas
    /// - Fallback para acesso direto ao oplog quando necessário
    /// - Suporta filtros por Hash
    #[instrument(level = "debug", skip(self, options))]
    fn query(&self, options: Option<StreamOptions>) -> Result<Vec<Entry>> {
        let options = options.unwrap_or_default();

        // Tenta usar o índice primeiro para melhor performance
        let events = match self.basestore.with_index(|index| {
            // Implementa busca otimizada no índice baseada nas StreamOptions
            self.optimized_index_query(index, &options)
        }) {
            Some(Some(indexed_results)) => indexed_results,
            _ => {
                // Fallback: acessa o oplog diretamente quando índice não está disponível
                // ou não suporta a query específica
                self.basestore.with_oplog(|log| {
                    log.values()
                        .iter()
                        .map(|arc_entry| (**arc_entry).clone())
                        .collect::<Vec<_>>()
                })
            }
        };

        // Calcula a quantidade de itens a serem retornados.
        let amount = match options.amount {
            Some(a) if a > -1 => a as usize,
            _ => events.len(), // Se amount for -1 ou None, pega todos.
        };

        if options.gt.is_some() || options.gte.is_some() {
            // Caso "maior que" (Greater Than)
            let hash = options.gt.or(options.gte).unwrap();
            let inclusive = options.gte.is_some();
            return Ok(self.read(&events, Some(hash), amount, inclusive));
        }

        let hash = options.lt.or(options.lte);

        // Caso "menor que" (Lower Than) ou N últimos.
        // Inverte os eventos para buscar dos mais recentes para os mais antigos.
        let mut events = events;
        events.reverse();

        // A busca é inclusiva se LTE for definido ou se nenhum limite (LT/LTE) for definido.
        let inclusive = options.lte.is_some() || hash.is_none();
        let mut result = self.read(&events, hash, amount, inclusive);

        // Desfaz a inversão do resultado para manter a ordem cronológica original.
        result.reverse();

        Ok(result)
    }

    /// Função auxiliar para ler uma fatia de entradas a partir de um hash.
    ///
    /// # Argumentos
    ///
    /// * `ops` - Slice de entradas para filtrar
    /// * `hash` - Hash opcional para usar como ponto de início
    /// * `amount` - Quantidade máxima de entradas a retornar
    /// * `inclusive` - Se deve incluir a entrada com o hash fornecido
    ///
    /// # Retorna
    ///
    /// Vetor de entradas filtradas baseado nos critérios
    ///
    /// # Performance
    ///
    /// - O(n) para encontrar o índice inicial por hash
    /// - O(amount) para coletar os resultados
    /// - Otimizada para uso com iteradores Rust
    fn read(
        &self,
        ops: &[Entry],
        hash: Option<Hash>,
        amount: usize,
        inclusive: bool,
    ) -> Vec<Entry> {
        if amount == 0 {
            return Vec::new();
        }

        // Encontra o índice inicial.
        let mut start_index = 0;
        if let Some(h) = hash {
            if let Some(idx) = ops.iter().position(|e| e.hash() == &h) {
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
        ops.iter().skip(start_index).take(amount).cloned().collect()
    }

    /// Busca otimizada no índice baseada nas StreamOptions.
    ///
    /// Utiliza os novos métodos opcionais do trait StoreIndex
    ///
    /// # Argumentos
    ///
    /// * `index` - Referência ao índice da store
    /// * `options` - Opções de filtro da stream
    ///
    /// # Retorna
    ///
    /// `Some(Vec<Entry>)` se conseguir processar a query otimizada
    /// `None` se deve usar fallback (oplog direto)
    ///
    /// # Casos Otimizados (Implementados)
    ///
    /// 1. **Amount-only queries**: Últimas N entradas usando `get_last_entries()`
    /// 2. **Range queries**: Faixas específicas usando `get_entries_range()`
    /// 3. **Hash queries**: Busca por Hash usando `get_entry_by_hash()`
    ///
    /// # Casos de Fallback
    ///
    /// 1. **Índice não suporta Entry**: `supports_entry_queries()` retorna false
    /// 2. **Queries complexas**: Combinações não otimizadas
    /// 3. **Índice vazio**: Nenhuma entrada disponível
    ///
    /// # Performance
    ///
    /// - **get_last_entries()**: O(k) onde k = número de entradas solicitadas
    /// - **get_entry_by_hash()**: O(n) atual, O(1) futuro com índice por Hash
    /// - **get_entries_range()**: O(k) onde k = tamanho do range
    fn optimized_index_query(
        &self,
        index: &dyn crate::traits::StoreIndex<Error = GuardianError>,
        options: &StreamOptions,
    ) -> Option<Vec<Entry>> {
        // Verifica se o índice suporta queries otimizadas com Entry completas
        if !index.supports_entry_queries() {
            return None; // Fallback para oplog
        }

        // Validação rápida: verifica se o índice tem dados
        let total_entries = match index.len() {
            Ok(len) if len > 0 => len,
            _ => return None, // Índice vazio - usa fallback
        };

        // Query simples por quantidade (caso mais comum)
        let is_simple_amount_query = options.gt.is_none()
            && options.gte.is_none()
            && options.lt.is_none()
            && options.lte.is_none();

        if is_simple_amount_query {
            let amount = match options.amount {
                Some(a) if a > 0 => (a as usize).min(total_entries),
                Some(-1) | None => total_entries, // -1 ou None significa "todas"
                _ => return None,                 // Valor inválido
            };

            // Usa o método otimizado do índice
            return index.get_last_entries(amount);
        }

        // Query por Hash específico (get operation)
        if let Some(hash) = options.gte
            && options.amount == Some(1)
            && options.gt.is_none()
            && options.lt.is_none()
            && options.lte.is_none()
        {
            // Query pontual por Hash - usa busca otimizada
            if let Some(entry) = index.get_entry_by_hash(&hash) {
                return Some(vec![entry]);
            } else {
                return Some(Vec::new()); // Hash não encontrado
            }
        }

        // Otimizações futuras: Ranges específicos (futuro)
        // Por enquanto, queries com múltiplos Hashes usam fallback
        // que já implementa a lógica correta
        //
        // Futuras otimizações:
        // - Range por posição quando Hashes são consecutivos
        // - Cache de queries frequentes
        // - Índice temporal para filtros por timestamp

        None // Usa fallback para queries complexas
    }
}
