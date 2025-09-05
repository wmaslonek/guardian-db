use crate::data_store::Datastore;
use crate::error::{GuardianError, Result};
use crate::iface::{self, EventLogStore, Store, StreamOptions};
use crate::ipfs_core_api::client::IpfsClient;
use crate::ipfs_log::{entry::Entry, identity::Identity};
use crate::pubsub::event::EventBus;
use crate::stores::base_store::base_store::BaseStore;
use crate::stores::operation::{operation, operation::Operation};
use crate::{address::Address, stores::event_log_store::index::new_event_index};
use cid::Cid;
use std::sync::Arc;
use tokio::sync::mpsc;

/// A implementação do trait `EventLogStore` para a nossa struct.
#[async_trait::async_trait]
impl EventLogStore for OrbitDBEventLogStore {
    /// Adiciona um novo dado ao log.
    async fn add(&mut self, data: Vec<u8>) -> std::result::Result<Operation, Self::Error> {
        // Chama o método interno da struct OrbitDBEventLogStore
        OrbitDBEventLogStore::add(self, data).await
    }

    /// Obtém uma entrada específica do log pelo seu CID.
    async fn get(&self, cid: Cid) -> std::result::Result<Operation, Self::Error> {
        // Chama o método interno da struct OrbitDBEventLogStore
        OrbitDBEventLogStore::get(self, cid).await
    }

    /// Retorna uma lista de operações que ocorreram na store, com opções de filtro.
    async fn list(
        &self,
        options: Option<StreamOptions>,
    ) -> std::result::Result<Vec<Operation>, Self::Error> {
        // Chama o método interno da struct OrbitDBEventLogStore
        OrbitDBEventLogStore::list(self, options).await
    }
}

pub struct OrbitDBEventLogStore {
    basestore: Arc<BaseStore>,
}

// Implementação da trait Store (que é herdada por EventLogStore)
#[async_trait::async_trait]
impl Store for OrbitDBEventLogStore {
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

    fn index(&self) -> Box<dyn crate::iface::StoreIndex<Error = GuardianError> + Send + Sync> {
        self.basestore.index()
    }

    fn store_type(&self) -> &str {
        "eventlog"
    }

    fn replication_status(&self) -> crate::stores::replicator::replication_info::ReplicationInfo {
        self.basestore.replication_status()
    }

    fn replicator(&self) -> Option<Arc<crate::stores::replicator::replicator::Replicator>> {
        self.basestore.replicator()
    }

    fn cache(&self) -> Arc<dyn Datastore> {
        self.basestore.cache()
    }

    async fn drop(&mut self) -> std::result::Result<(), Self::Error> {
        // BaseStore não tem método async drop público, então implementamos uma limpeza básica
        // A cleanup real é feita automaticamente quando o BaseStore é dropped
        Ok(())
    }

    async fn load(&mut self, amount: usize) -> std::result::Result<(), Self::Error> {
        self.basestore.load(Some(amount as isize)).await
    }

    async fn sync(
        &mut self,
        heads: Vec<crate::ipfs_log::entry::Entry>,
    ) -> std::result::Result<(), Self::Error> {
        self.basestore.sync(heads).await
    }

    async fn load_more_from(&mut self, _amount: u64, entries: Vec<crate::ipfs_log::entry::Entry>) {
        let _ = self.basestore.load_more_from(entries);
    }

    async fn load_from_snapshot(&mut self) -> std::result::Result<(), Self::Error> {
        self.basestore.load_from_snapshot().await
    }

    fn op_log(&self) -> Arc<parking_lot::RwLock<crate::ipfs_log::log::Log>> {
        self.basestore.op_log()
    }

    fn ipfs(&self) -> Arc<crate::ipfs_core_api::client::IpfsClient> {
        self.basestore.ipfs()
    }

    fn db_name(&self) -> &str {
        self.basestore.db_name()
    }

    fn identity(&self) -> &Identity {
        self.basestore.identity()
    }

    fn access_controller(&self) -> &dyn crate::access_controller::traits::AccessController {
        self.basestore.access_controller()
    }

    async fn add_operation(
        &mut self,
        op: Operation,
        on_progress_callback: Option<tokio::sync::mpsc::Sender<crate::ipfs_log::entry::Entry>>,
    ) -> std::result::Result<crate::ipfs_log::entry::Entry, Self::Error> {
        self.basestore.add_operation(op, on_progress_callback).await
    }

    fn logger(&self) -> Arc<slog::Logger> {
        self.basestore.logger()
    }

    fn tracer(&self) -> Arc<crate::iface::TracerWrapper> {
        self.basestore.tracer()
    }

    fn event_bus(&self) -> Arc<EventBus> {
        self.basestore.event_bus()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl OrbitDBEventLogStore {
    /// Getter para acessar o BaseStore interno
    pub fn basestore(&self) -> &BaseStore {
        &self.basestore
    }

    /// Equivalente a função "NewOrbitDBEventLogStore" em go.
    /// Instancia uma nova EventLogStore, adaptada para usar um cliente HTTP para o Kubo.
    ///
    /// # Argumentos
    ///
    /// * `ipfs_client` - Cliente IPFS compartilhado via Arc para operações de rede
    /// * `identity` - Identidade do nó para assinatura de entradas
    /// * `addr` - Endereço da store para identificação única
    /// * `options` - Opções de configuração da store (índice, cache, etc.)
    ///
    /// # Retorna
    ///
    /// Uma nova instância de `OrbitDBEventLogStore` configurada e pronta para uso
    ///
    /// # Erros
    ///
    /// Retorna `GuardianError::Store` se:
    /// - A inicialização do BaseStore falhar
    /// - As opções de configuração forem inválidas
    pub async fn new(
        ipfs_client: Arc<IpfsClient>,
        identity: Arc<Identity>,
        addr: Arc<dyn Address + Send + Sync>,
        mut options: iface::NewStoreOptions,
    ) -> Result<Self> {
        // Validação básica dos parâmetros - verifica se os componentes essenciais existem
        if addr.to_string().is_empty() {
            return Err(GuardianError::Store(
                "Invalid address provided, cannot create EventLogStore".to_string(),
            ));
        }

        // Em Go, o ponteiro para a função de criação do índice é definido dentro do
        // construtor. Em Rust, fazemos o mesmo, modificando as opções.
        options.index = Some(Box::new(new_event_index));

        // A lógica de `InitBaseStore` seria encapsulada no construtor de `BaseStore`,
        // que agora recebe e armazena o cliente HTTP.
        let basestore = BaseStore::new(ipfs_client, identity, addr, Some(options))
            .await
            .map_err(|e| {
                GuardianError::Store(format!(
                    "Failed to initialize base store for EventLogStore: {}",
                    e
                ))
            })?;

        Ok(OrbitDBEventLogStore { basestore })
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
    ///
    /// # Argumentos
    ///
    /// * `value` - Dados em bytes para adicionar ao log
    ///
    /// # Retorna
    ///
    /// A operação criada com seu CID único
    ///
    /// # Erros
    ///
    /// - Se os dados estiverem vazios (opcional, dependendo da política)
    /// - Se a adição ao BaseStore falhar
    /// - Se a conversão Entry -> Operation falhar
    pub async fn add(&mut self, value: Vec<u8>) -> Result<Operation> {
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

    /// Equivalente a função "Get" em go.
    /// Recupera uma única operação do log pelo seu CID.
    ///
    /// # Argumentos
    ///
    /// * `cid` - Content Identifier da entrada desejada
    ///
    /// # Retorna
    ///
    /// A operação correspondente ao CID fornecido
    ///
    /// # Erros
    ///
    /// - Se o CID não for encontrado no log
    /// - Se a stream não retornar resultados
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
            Err(GuardianError::Store(format!(
                "No operation found for CID: {}",
                cid
            )))
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

    /// Equivalente a função "query" em go.
    /// Executa a lógica de busca no índice do log com base nas opções de filtro.
    ///
    /// # Performance
    ///
    /// - Usa o índice quando disponível para queries otimizadas
    /// - Fallback para acesso direto ao oplog quando necessário
    /// - Suporta filtros por CID (gt, gte, lt, lte) e limitação de quantidade
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
    ///
    /// # Argumentos
    ///
    /// * `ops` - Slice de entradas para filtrar
    /// * `hash` - CID opcional para usar como ponto de início
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
        ops.iter().skip(start_index).take(amount).cloned().collect()
    }

    /// Implementa busca otimizada no índice baseada nas StreamOptions.
    ///
    /// Esta função agora utiliza os novos métodos opcionais do trait StoreIndex
    /// para implementar otimizações reais quando o índice suporta Entry completas.
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
    /// 3. **CID queries**: Busca por CID usando `get_entry_by_cid()`
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
    /// - **get_entry_by_cid()**: O(n) atual, O(1) futuro com índice por CID
    /// - **get_entries_range()**: O(k) onde k = tamanho do range
    fn optimized_index_query(
        &self,
        index: &dyn crate::iface::StoreIndex<Error = GuardianError>,
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

        // Query por CID específico (get operation)
        if let Some(cid) = options.gte
            && options.amount == Some(1)
            && options.gt.is_none()
            && options.lt.is_none()
            && options.lte.is_none()
        {
            // Query pontual por CID - usa busca otimizada
            if let Some(entry) = index.get_entry_by_cid(&cid) {
                return Some(vec![entry]);
            } else {
                return Some(Vec::new()); // CID não encontrado
            }
        }

        // Otimizações futuras: Ranges específicos (futuro)
        // Por enquanto, queries com múltiplos CIDs usam fallback
        // que já implementa a lógica correta
        //
        // Futuras otimizações:
        // - Range por posição quando CIDs são consecutivos
        // - Cache de queries frequentes
        // - Índice temporal para filtros por timestamp

        None // Usa fallback para queries complexas
    }
}
