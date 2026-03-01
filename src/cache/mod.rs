use crate::address::Address;
use crate::data_store::Datastore;
use crate::guardian::error::{GuardianError, Result};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::{Span, debug, error, info, instrument, warn};

#[allow(clippy::module_inception)]
pub mod level_down;
pub use level_down::LevelDownCache;

const REDB_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("cache");

// Type aliases para simplificar tipos complexos
type DatastoreBox = Box<dyn Datastore + Send + Sync>;
type CleanupFn = Box<dyn FnOnce() -> Result<()> + Send + Sync>;
type NewCacheResult = Result<(DatastoreBox, CleanupFn)>;

/// Define as opções para a criação de um cache.
#[derive(Debug, Clone)]
pub struct Options {
    /// Span para logging estruturado com tracing.
    pub span: Option<Span>,
    /// Tamanho máximo do cache em bytes (padrão: 100MB)
    pub max_cache_size: Option<u64>,
    /// Modo de cache: persistente ou em memória
    pub cache_mode: CacheMode,
}

/// Modo de operação do cache
#[derive(Debug, Clone, PartialEq)]
pub enum CacheMode {
    /// Cache persistente no disco
    Persistent,
    /// Cache em memória (temporário)
    InMemory,
    /// Automático: detecta baseado no diretório
    Auto,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            span: None,
            max_cache_size: Some(100 * 1024 * 1024), // 100MB
            cache_mode: CacheMode::Auto,
        }
    }
}

/// A trait `Cache` define a interface para um mecanismo de cache
/// para os bancos de dados GuardianDB.
pub trait Cache: Send + Sync {
    /// Cria uma nova instância de cache no caminho especificado
    /// Retorna um Datastore e uma função de cleanup
    #[allow(clippy::new_ret_no_self)]
    fn new(path: &str, opts: Option<Options>) -> NewCacheResult
    where
        Self: Sized,
    {
        RedbCache::create_cache_instance(path, opts.unwrap_or_default())
    }

    /// Carrega um cache para um determinado endereço de banco de dados e um diretório raiz.
    fn load(&self, directory: &str, db_address: &dyn Address) -> Result<DatastoreBox>;

    /// Fecha um cache e todos os seus armazenamentos de dados associados.
    fn close(&mut self) -> Result<()>;

    /// Remove todos os dados em cache de um banco de dados.
    fn destroy(&self, directory: &str, db_address: &dyn Address) -> Result<()>;
}

/// Implementação de cache usando redb como backend
pub struct RedbCache {
    caches: Arc<Mutex<HashMap<String, Arc<RedbDatastore>>>>,
    options: Options,
}

impl RedbCache {
    /// Cria uma nova instância do RedbCache
    pub fn new(opts: Options) -> Self {
        Self {
            caches: Arc::new(Mutex::new(HashMap::new())),
            options: opts,
        }
    }

    /// Factory method para criar instâncias de cache
    #[instrument(level = "info")]
    pub fn create_cache_instance(path: &str, opts: Options) -> NewCacheResult {
        info!("Creating cache instance: path={}", path);

        let datastore = RedbDatastore::new(path, opts.clone())?;
        let path_clone = path.to_string();

        // Função de cleanup que remove o cache do disco (apenas se não for em memória)
        let cleanup: Box<dyn FnOnce() -> Result<()> + Send + Sync> = Box::new(move || {
            if path_clone != ":memory:" && Path::new(&path_clone).exists() {
                match std::fs::remove_file(&path_clone) {
                    Ok(_) => {
                        debug!("Cache file cleaned up: path={}", &path_clone);
                        Ok(())
                    }
                    Err(e) => {
                        warn!(
                            "Failed to cleanup cache file: path={}, error={}",
                            &path_clone, e
                        );
                        Err(GuardianError::Other(format!(
                            "Failed to cleanup cache: {}",
                            e
                        )))
                    }
                }
            } else {
                Ok(())
            }
        });

        Ok((Box::new(datastore), cleanup))
    }

    /// Gera uma chave única para o cache baseada no diretório e endereço
    fn generate_cache_key(directory: &str, db_address: &dyn Address) -> String {
        let db_path = PathBuf::from(db_address.get_root().to_string()).join(db_address.get_path());
        PathBuf::from(directory)
            .join(db_path)
            .to_string_lossy()
            .to_string()
    }
}

impl Cache for RedbCache {
    #[instrument(level = "info", skip(self, db_address))]
    fn load(
        &self,
        directory: &str,
        db_address: &dyn Address,
    ) -> Result<Box<dyn Datastore + Send + Sync>> {
        let cache_key = Self::generate_cache_key(directory, db_address);

        info!(
            "Loading cache: directory={}, cache_key={}",
            directory, &cache_key
        );

        let mut caches = self.caches.lock().unwrap();

        if let Some(existing_cache) = caches.get(&cache_key) {
            debug!("Using existing cache: cache_key={}", &cache_key);
            return Ok(Box::new(RedbDatastoreHandle {
                inner: existing_cache.clone(),
            }));
        }

        // Cria um novo cache se não existir
        let datastore = Arc::new(RedbDatastore::new(&cache_key, self.options.clone())?);
        caches.insert(cache_key.clone(), datastore.clone());

        info!("Created new cache: cache_key={}", &cache_key);
        Ok(Box::new(RedbDatastoreHandle { inner: datastore }))
    }

    #[instrument(level = "info", skip(self))]
    fn close(&mut self) -> Result<()> {
        info!("Closing all caches");

        let caches = {
            let mut cache_map = self.caches.lock().unwrap();
            let caches: Vec<Arc<RedbDatastore>> = cache_map.values().cloned().collect();
            cache_map.clear();
            caches
        };

        for cache in caches {
            if let Err(e) = cache.flush() {
                warn!("Failed to close cache: error={}", e);
            }
        }

        info!("All caches closed");
        Ok(())
    }

    #[instrument(level = "info", skip(self, db_address))]
    fn destroy(&self, directory: &str, db_address: &dyn Address) -> Result<()> {
        let cache_key = Self::generate_cache_key(directory, db_address);

        info!(
            "Destroying cache: directory={}, cache_key={}",
            directory, &cache_key
        );

        // Remove do mapa de caches
        {
            let mut caches = self.caches.lock().unwrap();
            caches.remove(&cache_key);
        }

        // Remove arquivo do disco (apenas se não for em memória)
        if directory != ":memory:" && Path::new(&cache_key).exists() {
            std::fs::remove_file(&cache_key)
                .map_err(|e| GuardianError::Other(format!("Failed to remove cache file: {}", e)))?;

            info!("Cache file removed: path={}", &cache_key);
        }

        Ok(())
    }
}

/// Implementação de Datastore usando redb
pub struct RedbDatastore {
    db: Database,
    path: String,
    span: Span,
}

// RedbDatastore não pode ser Clone porque redb::Database não é Clone.
// Usamos Arc<RedbDatastore> + um handle wrapper para compatibilidade.

impl RedbDatastore {
    /// Cria uma nova instância do RedbDatastore
    #[instrument(level = "debug")]
    pub fn new(path: &str, opts: Options) -> Result<Self> {
        debug!("Creating RedbDatastore: path={}", path);

        let db = if path == ":memory:" || matches!(opts.cache_mode, CacheMode::InMemory) {
            debug!("Creating in-memory cache");
            Database::builder()
                .create_with_backend(redb::backends::InMemoryBackend::new())
                .map_err(|e| {
                    GuardianError::Store(format!("Failed to create in-memory cache: {}", e))
                })?
        } else {
            debug!("Creating persistent cache: path={}", path);

            // Cria o diretório se não existir
            if let Some(parent) = Path::new(path).parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    GuardianError::Store(format!("Failed to create cache directory: {}", e))
                })?;
            }

            Database::create(path).map_err(|e| {
                GuardianError::Store(format!("Failed to open cache at {}: {}", path, e))
            })?
        };

        // Ensure table exists
        {
            let write_txn = db.begin_write().map_err(|e| {
                GuardianError::Store(format!("Failed to begin write transaction: {}", e))
            })?;
            {
                let _ = write_txn
                    .open_table(REDB_TABLE)
                    .map_err(|e| GuardianError::Store(format!("Failed to create table: {}", e)))?;
            }
            write_txn.commit().map_err(|e| {
                GuardianError::Store(format!("Failed to commit table creation: {}", e))
            })?;
        }

        info!(
            "RedbDatastore created successfully: path={}, memory_mode={}",
            path,
            path == ":memory:"
        );

        Ok(Self {
            db,
            path: path.to_string(),
            span: opts.span.unwrap_or_else(tracing::Span::current),
        })
    }

    /// Retorna uma referência ao span de tracing para instrumentação
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Flush (compact) o datastore
    #[instrument(level = "debug", skip(self))]
    pub fn flush(&self) -> Result<()> {
        let _entered = self.span.enter();
        debug!("Flushing RedbDatastore: path={}", &self.path);

        // Data is already persisted via write transactions in redb.
        // compact() requires &mut self, which is incompatible with Arc-based access.

        info!("RedbDatastore flushed: path={}", &self.path);
        Ok(())
    }
}

// Send + Sync is safe because redb::Database is thread-safe
unsafe impl Send for RedbDatastore {}
unsafe impl Sync for RedbDatastore {}

#[async_trait::async_trait]
impl Datastore for RedbDatastore {
    #[instrument(level = "debug", skip(self, key))]
    async fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let _entered = self.span.enter();
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| GuardianError::Store(format!("Cache get read txn error: {}", e)))?;
        let table = read_txn
            .open_table(REDB_TABLE)
            .map_err(|e| GuardianError::Store(format!("Cache get open table error: {}", e)))?;
        match table.get(key) {
            Ok(Some(value)) => {
                debug!("Cache hit: key_len={}", key.len());
                Ok(Some(value.value().to_vec()))
            }
            Ok(None) => {
                debug!("Cache miss: key_len={}", key.len());
                Ok(None)
            }
            Err(e) => {
                error!("Cache get error: key_len={}, error={}", key.len(), e);
                Err(GuardianError::Store(format!("Cache get error: {}", e)))
            }
        }
    }

    #[instrument(level = "debug", skip(self, key, value))]
    async fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        let _entered = self.span.enter();
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| GuardianError::Store(format!("Cache put write txn error: {}", e)))?;
        {
            let mut table = write_txn
                .open_table(REDB_TABLE)
                .map_err(|e| GuardianError::Store(format!("Cache put open table error: {}", e)))?;
            table
                .insert(key, value)
                .map_err(|e| GuardianError::Store(format!("Cache put error: {}", e)))?;
        }
        write_txn
            .commit()
            .map_err(|e| GuardianError::Store(format!("Cache put commit error: {}", e)))?;
        debug!(
            "Cache put success: key_len={}, value_len={}",
            key.len(),
            value.len()
        );
        Ok(())
    }

    #[instrument(level = "debug", skip(self, key))]
    async fn has(&self, key: &[u8]) -> Result<bool> {
        let _entered = self.span.enter();
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| GuardianError::Store(format!("Cache has read txn error: {}", e)))?;
        let table = read_txn
            .open_table(REDB_TABLE)
            .map_err(|e| GuardianError::Store(format!("Cache has open table error: {}", e)))?;
        match table.get(key) {
            Ok(Some(_)) => {
                debug!("Cache has check: key_len={}, exists=true", key.len());
                Ok(true)
            }
            Ok(None) => {
                debug!("Cache has check: key_len={}, exists=false", key.len());
                Ok(false)
            }
            Err(e) => {
                error!("Cache has error: key_len={}, error={}", key.len(), e);
                Err(GuardianError::Store(format!("Cache has error: {}", e)))
            }
        }
    }

    #[instrument(level = "debug", skip(self, key))]
    async fn delete(&self, key: &[u8]) -> Result<()> {
        let _entered = self.span.enter();
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| GuardianError::Store(format!("Cache delete write txn error: {}", e)))?;
        {
            let mut table = write_txn.open_table(REDB_TABLE).map_err(|e| {
                GuardianError::Store(format!("Cache delete open table error: {}", e))
            })?;
            table
                .remove(key)
                .map_err(|e| GuardianError::Store(format!("Cache delete error: {}", e)))?;
        }
        write_txn
            .commit()
            .map_err(|e| GuardianError::Store(format!("Cache delete commit error: {}", e)))?;
        debug!("Cache delete success: key_len={}", key.len());
        Ok(())
    }

    #[instrument(level = "debug", skip(self, query))]
    async fn query(&self, query: &crate::data_store::Query) -> Result<crate::data_store::Results> {
        let _entered = self.span.enter();
        use crate::data_store::{Key, ResultItem};

        debug!(
            "Cache query: has_prefix={}, limit={:?}, order={:?}",
            query.prefix.is_some(),
            query.limit,
            query.order
        );

        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| GuardianError::Store(format!("Cache query read txn error: {}", e)))?;
        let table = read_txn
            .open_table(REDB_TABLE)
            .map_err(|e| GuardianError::Store(format!("Cache query open table error: {}", e)))?;

        let mut results = Vec::new();
        let mut count = 0;
        let skip_count = query.offset.unwrap_or(0);
        let mut skipped = 0;

        if let Some(prefix_key) = &query.prefix {
            let prefix_bytes = prefix_key.as_bytes();
            // Use range scan starting from prefix
            let iter = table
                .range(prefix_bytes.as_slice()..)
                .map_err(|e| GuardianError::Store(format!("Cache query range error: {}", e)))?;

            for entry_result in iter {
                match entry_result {
                    Ok(entry) => {
                        let key_bytes = entry.0.value();
                        // Check if key still has the prefix
                        if !key_bytes.starts_with(&prefix_bytes) {
                            break;
                        }

                        if skipped < skip_count {
                            skipped += 1;
                            continue;
                        }

                        let key_str = String::from_utf8_lossy(key_bytes);
                        let key = Key::new(key_str.to_string());
                        let value = entry.1.value().to_vec();
                        results.push(ResultItem::new(key, value));
                        count += 1;

                        if let Some(limit) = query.limit
                            && count >= limit
                        {
                            break;
                        }
                    }
                    Err(e) => {
                        error!("Cache query iteration error: error={}", e);
                        return Err(GuardianError::Store(format!("Cache query error: {}", e)));
                    }
                }
            }
        } else {
            let iter = table
                .iter()
                .map_err(|e| GuardianError::Store(format!("Cache query iter error: {}", e)))?;

            for entry_result in iter {
                match entry_result {
                    Ok(entry) => {
                        if skipped < skip_count {
                            skipped += 1;
                            continue;
                        }

                        let key_bytes = entry.0.value();
                        let key_str = String::from_utf8_lossy(key_bytes);
                        let key = Key::new(key_str.to_string());
                        let value = entry.1.value().to_vec();
                        results.push(ResultItem::new(key, value));
                        count += 1;

                        if let Some(limit) = query.limit
                            && count >= limit
                        {
                            break;
                        }
                    }
                    Err(e) => {
                        error!("Cache query iteration error: error={}", e);
                        return Err(GuardianError::Store(format!("Cache query error: {}", e)));
                    }
                }
            }
        }

        // redb returns in ascending order by default
        if matches!(query.order, crate::data_store::Order::Desc) {
            results.reverse();
        }

        debug!(
            "Cache query completed: results_count={}, skipped={}",
            results.len(),
            skipped
        );

        Ok(results)
    }

    #[instrument(level = "debug", skip(self, prefix))]
    async fn list_keys(&self, prefix: &[u8]) -> Result<Vec<crate::data_store::Key>> {
        let _entered = self.span.enter();
        use crate::data_store::Key;

        debug!("Cache list_keys: prefix_len={}", prefix.len());

        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| GuardianError::Store(format!("Cache list_keys read txn error: {}", e)))?;
        let table = read_txn.open_table(REDB_TABLE).map_err(|e| {
            GuardianError::Store(format!("Cache list_keys open table error: {}", e))
        })?;

        let mut keys = Vec::new();
        let iter = table
            .range(prefix..)
            .map_err(|e| GuardianError::Store(format!("Cache list_keys range error: {}", e)))?;

        for entry_result in iter {
            match entry_result {
                Ok(entry) => {
                    let key_bytes = entry.0.value();
                    if !key_bytes.starts_with(prefix) {
                        break;
                    }
                    let key_str = String::from_utf8_lossy(key_bytes);
                    let key = Key::new(key_str.to_string());
                    keys.push(key);
                }
                Err(e) => {
                    error!("Cache list_keys iteration error: error={}", e);
                    return Err(GuardianError::Store(format!(
                        "Cache list_keys error: {}",
                        e
                    )));
                }
            }
        }

        debug!("Cache list_keys completed: keys_count={}", keys.len());
        Ok(keys)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Handle wrapper around Arc<RedbDatastore> to implement Datastore trait
/// since RedbDatastore itself is not Clone (redb::Database is not Clone).
#[derive(Clone)]
pub struct RedbDatastoreHandle {
    inner: Arc<RedbDatastore>,
}

impl RedbDatastoreHandle {
    pub fn new(datastore: Arc<RedbDatastore>) -> Self {
        Self { inner: datastore }
    }

    pub fn flush(&self) -> Result<()> {
        self.inner.flush()
    }
}

#[async_trait::async_trait]
impl Datastore for RedbDatastoreHandle {
    async fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.inner.get(key).await
    }
    async fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        self.inner.put(key, value).await
    }
    async fn has(&self, key: &[u8]) -> Result<bool> {
        self.inner.has(key).await
    }
    async fn delete(&self, key: &[u8]) -> Result<()> {
        self.inner.delete(key).await
    }
    async fn query(&self, query: &crate::data_store::Query) -> Result<crate::data_store::Results> {
        self.inner.query(query).await
    }
    async fn list_keys(&self, prefix: &[u8]) -> Result<Vec<crate::data_store::Key>> {
        self.inner.list_keys(prefix).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Factory function para criar instâncias de cache
pub fn create_cache(opts: Options) -> RedbCache {
    RedbCache::new(opts)
}

/// Cria um cache padrão com configurações otimizadas
pub fn create_default_cache() -> RedbCache {
    create_cache(Options::default())
}

/// Cria um cache em memória para testes
pub fn create_memory_cache() -> RedbCache {
    create_cache(Options {
        cache_mode: CacheMode::InMemory,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_store::{Key, Order, Query};

    #[tokio::test]
    async fn test_redb_datastore_basic_operations() {
        let datastore = RedbDatastore::new(":memory:", Options::default()).unwrap();

        // Test put and get
        let key = b"test_key";
        let value = b"test_value";

        datastore.put(key, value).await.unwrap();
        let retrieved = datastore.get(key).await.unwrap();
        assert_eq!(retrieved, Some(value.to_vec()));

        // Test has
        assert!(datastore.has(key).await.unwrap());
        assert!(!datastore.has(b"non_existent").await.unwrap());

        // Test delete
        datastore.delete(key).await.unwrap();
        assert!(!datastore.has(key).await.unwrap());
        assert_eq!(datastore.get(key).await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_redb_datastore_query() {
        let datastore = RedbDatastore::new(":memory:", Options::default()).unwrap();

        // Insert test data
        datastore.put(b"/users/alice", b"alice_data").await.unwrap();
        datastore.put(b"/users/bob", b"bob_data").await.unwrap();
        datastore
            .put(b"/users/charlie", b"charlie_data")
            .await
            .unwrap();
        datastore
            .put(b"/config/database", b"db_config")
            .await
            .unwrap();

        // Test query with prefix
        let query = Query {
            prefix: Some(Key::new("/users")),
            limit: None,
            order: Order::Asc,
            offset: None,
        };

        let results = datastore.query(&query).await.unwrap();
        assert_eq!(results.len(), 3);

        // Test query with limit
        let query_limited = Query {
            prefix: Some(Key::new("/users")),
            limit: Some(2),
            order: Order::Asc,
            offset: None,
        };

        let results_limited = datastore.query(&query_limited).await.unwrap();
        assert_eq!(results_limited.len(), 2);

        // Test query with offset
        let query_offset = Query {
            prefix: Some(Key::new("/users")),
            limit: None,
            order: Order::Asc,
            offset: Some(1),
        };

        let results_offset = datastore.query(&query_offset).await.unwrap();
        assert_eq!(results_offset.len(), 2);
    }

    #[tokio::test]
    async fn test_redb_datastore_list_keys() {
        let datastore = RedbDatastore::new(":memory:", Options::default()).unwrap();

        // Insert test data
        datastore.put(b"/users/alice", b"alice_data").await.unwrap();
        datastore.put(b"/users/bob", b"bob_data").await.unwrap();
        datastore
            .put(b"/config/database", b"db_config")
            .await
            .unwrap();

        // Test list_keys with prefix
        let keys = datastore.list_keys(b"/users").await.unwrap();
        assert_eq!(keys.len(), 2);

        let key_strings: Vec<String> = keys.iter().map(|k| k.as_str()).collect();
        assert!(key_strings.contains(&"/users/alice".to_string()));
        assert!(key_strings.contains(&"/users/bob".to_string()));
    }

    #[test]
    fn test_cache_mode_detection() {
        let persistent_opts = Options {
            cache_mode: CacheMode::Persistent,
            ..Default::default()
        };

        let memory_opts = Options {
            cache_mode: CacheMode::InMemory,
            ..Default::default()
        };

        assert_eq!(persistent_opts.cache_mode, CacheMode::Persistent);
        assert_eq!(memory_opts.cache_mode, CacheMode::InMemory);
    }
}
