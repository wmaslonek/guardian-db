use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
};
use slog::{debug, o, Discard, Logger};
use sled::{Config, Db, IVec};
use crate::error::{GuardianError, Result};
use crate::cache::{cache::Cache, cache::Options};
use crate::data_store::{Key, Query, ResultItem, Results, Order, Datastore};
use crate::address::Address;

pub const IN_MEMORY_DIRECTORY: &str = ":memory:";

pub struct WrappedCache {
    id: String,
    db: Db,
    manager_map: Weak<Mutex<HashMap<String, Arc<WrappedCache>>>>,
    logger: Logger,
    closed: Mutex<bool>,
}

impl WrappedCache {
    pub fn get(&self, _ctx: &mut dyn core::any::Any, key: &Key) -> std::result::Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        match self.db.get(key.as_bytes()).map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)? {
            Some(v) => Ok(v.to_vec()),
            None => Err(format!("key not found: {}", key).into()),
        }
    }

    pub fn has(&self, _ctx: &mut dyn core::any::Any, key: &Key) -> std::result::Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.db.contains_key(key.as_bytes()).map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?)
    }

    pub fn get_size(&self, _ctx: &mut dyn core::any::Any, key: &Key) -> std::result::Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let v = self.db.get(key.as_bytes()).map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?.ok_or_else(|| format!("key not found: {}", key))?;
        Ok(v.len())
    }

    pub fn query(&self, _ctx: &mut dyn core::any::Any, q: &Query) -> std::result::Result<Results, Box<dyn std::error::Error + Send + Sync>> {
        let iter: Box<dyn Iterator<Item = sled::Result<(IVec, IVec)>>> = if let Some(pref) = &q.prefix {
            Box::new(self.db.scan_prefix(pref.clone()))
        } else {
            Box::new(self.db.iter())
        };

        // coleta (ordenado asc pelo sled por padrão)
        let mut items: Results = Vec::new();
        for kv in iter {
            let (k, v) = kv.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            let key_str = String::from_utf8(k.to_vec()).unwrap_or_default();
            items.push(ResultItem {
                key: Key::new(key_str),
                value: v.to_vec(),
            });
            if let Some(n) = q.limit {
                if items.len() >= n {
                    break;
                }
            }
        }

        if matches!(q.order, Order::Desc) {
            items.reverse();
        }

        Ok(items)
    }

    pub fn put(&self, _ctx: &mut dyn core::any::Any, key: &Key, value: &[u8]) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.db.insert(key.as_bytes(), value).map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
        Ok(())
    }

    pub fn delete(&self, _ctx: &mut dyn core::any::Any, key: &Key) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.db.remove(key.as_bytes()).map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
        Ok(())
    }

    pub fn sync(&self, _ctx: &mut dyn core::any::Any, _key: &Key) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.db.flush().map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
        Ok(())
    }

    pub fn close(&self) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut closed = self.closed.lock().unwrap();
        if *closed {
            return Ok(());
        }

        if let Some(map) = self.manager_map.upgrade() {
            let mut m = map.lock().unwrap();
            m.remove(&self.id);
        }

        self.db.flush().map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
        *closed = true;
        Ok(())
    }
}

// Wrapper para adaptar WrappedCache ao trait Datastore
pub struct DatastoreWrapper {
    cache: Arc<WrappedCache>,
}

impl DatastoreWrapper {
    pub fn new(cache: Arc<WrappedCache>) -> Self {
        Self { cache }
    }
}

#[async_trait::async_trait]
impl Datastore for DatastoreWrapper {
    async fn has(&self, key: &[u8]) -> Result<bool> {
        let key_obj = Key::new(String::from_utf8_lossy(key));
        let mut any_ctx = ();
        self.cache.has(&mut any_ctx, &key_obj)
            .map_err(|e| GuardianError::Other(format!("Cache has error: {}", e)))
    }

    async fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        let key_obj = Key::new(String::from_utf8_lossy(key));
        let mut any_ctx = ();
        self.cache.put(&mut any_ctx, &key_obj, value)
            .map_err(|e| GuardianError::Other(format!("Cache put error: {}", e)))
    }

    async fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let key_obj = Key::new(String::from_utf8_lossy(key));
        let mut any_ctx = ();
        match self.cache.get(&mut any_ctx, &key_obj) {
            Ok(value) => Ok(Some(value)),
            Err(_) => Ok(None), // key not found
        }
    }

    async fn delete(&self, key: &[u8]) -> Result<()> {
        let key_obj = Key::new(String::from_utf8_lossy(key));
        let mut any_ctx = ();
        self.cache.delete(&mut any_ctx, &key_obj)
            .map_err(|e| GuardianError::Other(format!("Cache delete error: {}", e)))
    }
}

pub struct LevelDownCache {
    logger: Logger,
    caches: Arc<Mutex<HashMap<String, Arc<WrappedCache>>>>,
}

impl LevelDownCache {
    // Equivalente ao New(opts *cache.Options)
    pub fn new(opts: Option<&Options>) -> Self {
        let logger = opts
            .and_then(|o| o.logger.clone())
            .unwrap_or_else(|| Logger::root(Discard, o!()));
        Self {
            logger,
            caches: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    // Equivalente a Load(directory, address)
    pub fn load_internal(&self, directory: &str, db_address: &dyn Address) -> std::result::Result<Arc<WrappedCache>, Box<dyn std::error::Error + Send + Sync>> {
        let key_path = datastore_key(directory, db_address);

        // cache hit
        if let Some(ds) = self.caches.lock().unwrap().get(&key_path).cloned() {
            return Ok(ds);
        }

        debug!(self.logger, "opening cache db"; "path" => key_path.as_str());

        let db = if directory == IN_MEMORY_DIRECTORY {
            Config::new().temporary(true).open().map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
        } else {
            if let Some(parent) = Path::new(&key_path).parent() {
                std::fs::create_dir_all(parent)?;
            }
            Config::new().path(&key_path).open().map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
        };

        let wrapped = Arc::new(WrappedCache {
            id: key_path.clone(),
            db,
            manager_map: Arc::downgrade(&self.caches),
            logger: self.logger.clone(),
            closed: Mutex::new(false),
        });

        self.caches.lock().unwrap().insert(key_path, wrapped.clone());
        Ok(wrapped)
    }

    // Equivalente a Close()
    pub fn close_internal(&self) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let caches = {
            let m = self.caches.lock().unwrap();
            m.values().cloned().collect::<Vec<_>>()
        };
        for c in caches {
            let _ = c.close();
        }
        Ok(())
    }

    // Equivalente a Destroy(directory, address)
    pub fn destroy_internal(&self, directory: &str, db_address: &dyn Address) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let key_path = datastore_key(directory, db_address);

        // fecha e remove do mapa
        if let Some(c) = self.caches.lock().unwrap().remove(&key_path) {
            let _ = c.close();
        }

        if directory != IN_MEMORY_DIRECTORY && Path::new(&key_path).exists() {
            std::fs::remove_dir_all(&key_path)?;
        }

        Ok(())
    }
}

// Implementação do trait Cache para LevelDownCache
impl Cache for LevelDownCache {
    fn load(&self, directory: &str, db_address: &Box<dyn Address>) -> Result<Box<dyn Datastore + Send + Sync>> {
        let wrapped_cache = self.load_internal(directory, db_address.as_ref())
            .map_err(|e| GuardianError::Other(format!("Failed to load cache: {}", e)))?;
        Ok(Box::new(DatastoreWrapper { cache: wrapped_cache }))
    }

    fn close(&mut self) -> Result<()> {
        let caches = {
            let m = self.caches.lock().unwrap();
            m.values().cloned().collect::<Vec<_>>()
        };
        for c in caches {
            let _ = c.close();
        }
        Ok(())
    }

    fn destroy(&self, directory: &str, db_address: &Box<dyn Address>) -> Result<()> {
        self.destroy_internal(directory, db_address.as_ref())
            .map_err(|e| GuardianError::Other(format!("Failed to destroy cache: {}", e)))?;
        Ok(())
    }
}

fn datastore_key(directory: &str, db_address: &dyn Address) -> String {
    // path.Join(dbAddress.GetRoot().String(), dbAddress.GetPath()) depois Join(directory, dbPath)
    let db_path = PathBuf::from(db_address.get_root().to_string()).join(db_address.get_path());
    PathBuf::from(directory)
        .join(db_path)
        .to_string_lossy()
        .into_owned()
}
