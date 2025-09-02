use crate::address::Address;
use crate::data_store::Datastore;
use crate::error::{GuardianError, Result};
use sled::{Config, Db};
use slog::{Logger, debug, error, info, warn};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// Type aliases para simplificar tipos complexos
type DatastoreBox = Box<dyn Datastore + Send + Sync>;
type CleanupFn = Box<dyn FnOnce() -> Result<()> + Send + Sync>;
type NewCacheResult = Result<(DatastoreBox, CleanupFn)>;

// equivalente à struct Options em go
/// Define as opções para a criação de um cache.
#[derive(Debug, Clone)]
pub struct Options {
    /// Uma instância de um logger estruturado (slog).
    pub logger: Option<Logger>,
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
            logger: None,
            max_cache_size: Some(100 * 1024 * 1024), // 100MB
            cache_mode: CacheMode::Auto,
        }
    }
}

// equivalente à Interface em go
/// A trait `Cache` define a interface para um mecanismo de cache
/// para os bancos de dados GuardianDB.
pub trait Cache: Send + Sync {
    /// Cria uma nova instância de cache no caminho especificado
    /// Retorna um Datastore e uma função de cleanup
    #[allow(clippy::new_ret_no_self)]
    fn new(
        path: &str,
        opts: Option<Options>,
    ) -> NewCacheResult
    where
        Self: Sized,
    {
        SledCache::create_cache_instance(path, opts.unwrap_or_default())
    }

    /// Carrega um cache para um determinado endereço de banco de dados e um diretório raiz.
    // equivalente a Load em go
    fn load(
        &self,
        directory: &str,
        db_address: &dyn Address,
    ) -> Result<DatastoreBox>;

    /// Fecha um cache e todos os seus armazenamentos de dados associados.
    // equivalente a Close em go
    fn close(&mut self) -> Result<()>;

    /// Remove todos os dados em cache de um banco de dados.
    // equivalente a Destroy em go
    fn destroy(&self, directory: &str, db_address: &dyn Address) -> Result<()>;
}

/// Implementação de cache usando Sled como backend
pub struct SledCache {
    caches: Arc<Mutex<HashMap<String, Arc<SledDatastore>>>>,
    options: Options,
}

impl SledCache {
    /// Cria uma nova instância do SledCache
    pub fn new(opts: Options) -> Self {
        Self {
            caches: Arc::new(Mutex::new(HashMap::new())),
            options: opts,
        }
    }

    /// Factory method para criar instâncias de cache
    pub fn create_cache_instance(
        path: &str,
        opts: Options,
    ) -> NewCacheResult {
        let logger = opts
            .logger
            .clone()
            .unwrap_or_else(|| slog::Logger::root(slog::Discard, slog::o!()));

        info!(logger, "Creating cache instance"; "path" => path);

        let datastore = SledDatastore::new(path, opts.clone())?;
        let path_clone = path.to_string();

        // Função de cleanup que remove o cache do disco (apenas se não for em memória)
        let cleanup: Box<dyn FnOnce() -> Result<()> + Send + Sync> = Box::new(move || {
            if path_clone != ":memory:" && Path::new(&path_clone).exists() {
                match std::fs::remove_dir_all(&path_clone) {
                    Ok(_) => {
                        debug!(logger, "Cache directory cleaned up"; "path" => &path_clone);
                        Ok(())
                    }
                    Err(e) => {
                        warn!(logger, "Failed to cleanup cache directory";
                            "path" => &path_clone,
                            "error" => %e
                        );
                        Err(GuardianError::Other(
                            format!("Failed to cleanup cache: {}", e),
                        ))
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

impl Cache for SledCache {
    fn load(
        &self,
        directory: &str,
        db_address: &dyn Address,
    ) -> Result<Box<dyn Datastore + Send + Sync>> {
        let cache_key = Self::generate_cache_key(directory, db_address);
        let logger = self
            .options
            .logger
            .clone()
            .unwrap_or_else(|| slog::Logger::root(slog::Discard, slog::o!()));

        info!(logger, "Loading cache";
            "directory" => directory,
            "cache_key" => &cache_key
        );

        let mut caches = self.caches.lock().unwrap();

        if let Some(existing_cache) = caches.get(&cache_key) {
            debug!(logger, "Using existing cache"; "cache_key" => &cache_key);
            return Ok(Box::new(existing_cache.as_ref().clone()));
        }

        // Cria um novo cache se não existir
        let datastore = SledDatastore::new(&cache_key, self.options.clone())?;
        let arc_datastore = Arc::new(datastore.clone());
        caches.insert(cache_key.clone(), arc_datastore);

        info!(logger, "Created new cache"; "cache_key" => &cache_key);
        Ok(Box::new(datastore))
    }

    fn close(&mut self) -> Result<()> {
        let logger = self
            .options
            .logger
            .clone()
            .unwrap_or_else(|| slog::Logger::root(slog::Discard, slog::o!()));
        info!(logger, "Closing all caches");

        let caches = {
            let mut cache_map = self.caches.lock().unwrap();
            let caches: Vec<Arc<SledDatastore>> = cache_map.values().cloned().collect();
            cache_map.clear();
            caches
        };

        for cache in caches {
            if let Err(e) = cache.close() {
                warn!(logger, "Failed to close cache"; "error" => %e);
            }
        }

        info!(logger, "All caches closed");
        Ok(())
    }

    fn destroy(&self, directory: &str, db_address: &dyn Address) -> Result<()> {
        let cache_key = Self::generate_cache_key(directory, db_address);
        let logger = self
            .options
            .logger
            .clone()
            .unwrap_or_else(|| slog::Logger::root(slog::Discard, slog::o!()));

        info!(logger, "Destroying cache";
            "directory" => directory,
            "cache_key" => &cache_key
        );

        // Remove do mapa de caches
        let cache_to_close = {
            let mut caches = self.caches.lock().unwrap();
            caches.remove(&cache_key)
        };

        // Fecha o cache se existir
        if let Some(cache) = cache_to_close {
            cache.close()?;
        }

        // Remove arquivos do disco (apenas se não for em memória)
        if directory != ":memory:" && Path::new(&cache_key).exists() {
            std::fs::remove_dir_all(&cache_key).map_err(|e| {
                GuardianError::Other(format!("Failed to remove cache directory: {}", e))
            })?;

            info!(logger, "Cache directory removed"; "path" => &cache_key);
        }

        Ok(())
    }
}

/// Implementação de Datastore usando Sled
#[derive(Clone)]
pub struct SledDatastore {
    db: Db,
    path: String,
    logger: Logger,
}

impl SledDatastore {
    /// Cria uma nova instância do SledDatastore
    pub fn new(path: &str, opts: Options) -> Result<Self> {
        let logger = opts
            .logger
            .unwrap_or_else(|| slog::Logger::root(slog::Discard, slog::o!()));

        debug!(logger, "Creating SledDatastore"; "path" => path);

        let db = if path == ":memory:" || matches!(opts.cache_mode, CacheMode::InMemory) {
            // Cache em memória
            debug!(logger, "Creating in-memory cache");
            Config::new().temporary(true).open().map_err(|e| {
                GuardianError::Store(format!("Failed to create in-memory cache: {}", e))
            })?
        } else {
            // Cache persistente
            debug!(logger, "Creating persistent cache"; "path" => path);

            // Cria o diretório se não existir
            if let Some(parent) = Path::new(path).parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    GuardianError::Store(format!("Failed to create cache directory: {}", e))
                })?;
            }

            let mut config = Config::new();

            // Configura tamanho máximo se especificado
            if let Some(max_size) = opts.max_cache_size {
                config = config.cache_capacity(max_size);
            }

            config.path(path).open().map_err(|e| {
                GuardianError::Store(format!("Failed to open cache at {}: {}", path, e))
            })?
        };

        info!(logger, "SledDatastore created successfully";
            "path" => path,
            "memory_mode" => path == ":memory:"
        );

        Ok(Self {
            db,
            path: path.to_string(),
            logger,
        })
    }

    /// Fecha o datastore
    pub fn close(&self) -> Result<()> {
        debug!(self.logger, "Closing SledDatastore"; "path" => &self.path);

        self.db
            .flush()
            .map_err(|e| GuardianError::Store(format!("Failed to flush cache: {}", e)))?;

        info!(self.logger, "SledDatastore closed"; "path" => &self.path);
        Ok(())
    }
}

#[async_trait::async_trait]
impl Datastore for SledDatastore {
    async fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        match self.db.get(key) {
            Ok(Some(value)) => {
                debug!(self.logger, "Cache hit"; "key_len" => key.len());
                Ok(Some(value.to_vec()))
            }
            Ok(None) => {
                debug!(self.logger, "Cache miss"; "key_len" => key.len());
                Ok(None)
            }
            Err(e) => {
                error!(self.logger, "Cache get error";
                    "key_len" => key.len(),
                    "error" => %e
                );
                Err(GuardianError::Store(format!("Cache get error: {}", e)))
            }
        }
    }

    async fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        match self.db.insert(key, value) {
            Ok(_) => {
                debug!(self.logger, "Cache put success";
                    "key_len" => key.len(),
                    "value_len" => value.len()
                );
                Ok(())
            }
            Err(e) => {
                error!(self.logger, "Cache put error";
                    "key_len" => key.len(),
                    "error" => %e
                );
                Err(GuardianError::Store(format!("Cache put error: {}", e)))
            }
        }
    }

    async fn has(&self, key: &[u8]) -> Result<bool> {
        match self.db.contains_key(key) {
            Ok(exists) => {
                debug!(self.logger, "Cache has check";
                    "key_len" => key.len(),
                    "exists" => exists
                );
                Ok(exists)
            }
            Err(e) => {
                error!(self.logger, "Cache has error";
                    "key_len" => key.len(),
                    "error" => %e
                );
                Err(GuardianError::Store(format!("Cache has error: {}", e)))
            }
        }
    }

    async fn delete(&self, key: &[u8]) -> Result<()> {
        match self.db.remove(key) {
            Ok(_) => {
                debug!(self.logger, "Cache delete success"; "key_len" => key.len());
                Ok(())
            }
            Err(e) => {
                error!(self.logger, "Cache delete error";
                    "key_len" => key.len(),
                    "error" => %e
                );
                Err(GuardianError::Store(format!("Cache delete error: {}", e)))
            }
        }
    }

    async fn query(&self, query: &crate::data_store::Query) -> Result<crate::data_store::Results> {
        use crate::data_store::{Key, ResultItem};

        debug!(self.logger, "Cache query";
            "has_prefix" => query.prefix.is_some(),
            "limit" => query.limit,
            "order" => ?query.order
        );

        let iter: Box<dyn Iterator<Item = sled::Result<(sled::IVec, sled::IVec)>>> =
            if let Some(prefix_key) = &query.prefix {
                // Converte Key para bytes para usar como prefixo
                let prefix_bytes = prefix_key.as_bytes();
                Box::new(self.db.scan_prefix(prefix_bytes))
            } else {
                Box::new(self.db.iter())
            };

        let mut results = Vec::new();
        let mut count = 0;

        // Aplica offset se especificado
        let skip_count = query.offset.unwrap_or(0);
        let mut skipped = 0;

        for kv_result in iter {
            match kv_result {
                Ok((key_bytes, value_bytes)) => {
                    // Aplica offset
                    if skipped < skip_count {
                        skipped += 1;
                        continue;
                    }

                    // Converte bytes de volta para Key
                    let key_str = String::from_utf8_lossy(&key_bytes);
                    let key = Key::new(key_str.to_string());
                    let value = value_bytes.to_vec();

                    results.push(ResultItem::new(key, value));
                    count += 1;

                    // Aplica limite se especificado
                    if let Some(limit) = query.limit {
                        if count >= limit {
                            break;
                        }
                    }
                }
                Err(e) => {
                    error!(self.logger, "Cache query iteration error"; "error" => %e);
                    return Err(GuardianError::Store(format!("Cache query error: {}", e)));
                }
            }
        }

        // Aplica ordenação se necessário (Sled retorna em ordem ascendente por padrão)
        if matches!(query.order, crate::data_store::Order::Desc) {
            results.reverse();
        }

        debug!(self.logger, "Cache query completed";
            "results_count" => results.len(),
            "skipped" => skipped
        );

        Ok(results)
    }

    async fn list_keys(&self, prefix: &[u8]) -> Result<Vec<crate::data_store::Key>> {
        use crate::data_store::Key;

        debug!(self.logger, "Cache list_keys"; "prefix_len" => prefix.len());

        let mut keys = Vec::new();

        for kv_result in self.db.scan_prefix(prefix) {
            match kv_result {
                Ok((key_bytes, _)) => {
                    let key_str = String::from_utf8_lossy(&key_bytes);
                    let key = Key::new(key_str.to_string());
                    keys.push(key);
                }
                Err(e) => {
                    error!(self.logger, "Cache list_keys iteration error"; "error" => %e);
                    return Err(GuardianError::Store(format!(
                        "Cache list_keys error: {}",
                        e
                    )));
                }
            }
        }

        debug!(self.logger, "Cache list_keys completed"; "keys_count" => keys.len());
        Ok(keys)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Factory function para criar instâncias de cache
pub fn create_cache(opts: Options) -> SledCache {
    SledCache::new(opts)
}

/// Cria um cache padrão com configurações otimizadas
pub fn create_default_cache() -> SledCache {
    create_cache(Options::default())
}

/// Cria um cache em memória para testes
pub fn create_memory_cache() -> SledCache {
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
    async fn test_sled_datastore_basic_operations() {
        let datastore = SledDatastore::new(":memory:", Options::default()).unwrap();

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
    async fn test_sled_datastore_query() {
        let datastore = SledDatastore::new(":memory:", Options::default()).unwrap();

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
    async fn test_sled_datastore_list_keys() {
        let datastore = SledDatastore::new(":memory:", Options::default()).unwrap();

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
