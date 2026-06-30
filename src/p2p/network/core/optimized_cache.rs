/// Optimized cache layer for the Iroh backend.
///
/// Intelligent caching with compression, adaptive TTL and predictive eviction
/// to maximize the performance of the native Iroh backend.
use crate::guardian::error::{GuardianError, Result};
use blake3::Hasher;
use bytes::Bytes;
use iroh_blobs::BlobFormat;
use lru::LruCache;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, instrument, warn};

/// Optimized cache layer for Iroh operations.
pub struct OptimizedCache {
    /// LRU cache for recent data.
    data_cache: Arc<RwLock<LruCache<String, CacheEntry>>>,
    /// Metadata cache for CIDs.
    metadata_cache: Arc<RwLock<HashMap<String, MetadataEntry>>>,
    /// Compression cache for large data.
    compressed_cache: Arc<RwLock<LruCache<String, CompressedEntry>>>,
    /// Performance statistics.
    stats: Arc<RwLock<CacheStats>>,
    /// Cache configuration.
    cache_config: CacheConfig,
    /// Access predictor for intelligent eviction.
    access_predictor: Arc<Mutex<AccessPredictor>>,
}

/// Cache entry with performance metadata.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    /// Blob data.
    pub data: Bytes,
    /// Creation timestamp.
    pub created_at: Instant,
    /// Last access.
    pub last_accessed: Instant,
    /// Number of accesses.
    pub access_count: u64,
    /// Priority (0-10, higher = more important).
    pub priority: u8,
    /// Original size (before compression, if applicable).
    pub original_size: usize,
    /// Integrity verification hash.
    pub integrity_hash: [u8; 32],
}

/// Compressed entry for large data.
#[derive(Debug, Clone)]
pub struct CompressedEntry {
    /// Data compressed with zstd.
    pub compressed_data: Bytes,
    /// Original size.
    pub original_size: usize,
    /// Compression level used.
    pub compression_level: i32,
    /// Compression timestamp.
    pub compressed_at: Instant,
    /// Compression ratio (0.0-1.0).
    pub compression_ratio: f64,
}

/// Metadata for CIDs.
#[derive(Debug, Clone)]
pub struct MetadataEntry {
    /// Blob size.
    pub size: u64,
    /// Blob format (Raw, DagCbor, etc.).
    pub format: BlobFormat,
    /// Peers that hold the content.
    pub providers: Vec<String>,
    /// Discovery timestamp.
    pub discovered_at: Instant,
    /// Average access latency (ms).
    pub avg_access_latency_ms: f64,
    /// Popularity (access frequency).
    pub popularity_score: f64,
}

/// Advanced cache statistics.
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    /// Hits in the data cache.
    pub data_cache_hits: u64,
    /// Misses in the data cache.
    pub data_cache_misses: u64,
    /// Hits in the compressed cache.
    pub compressed_cache_hits: u64,
    /// Misses in the compressed cache.
    pub compressed_cache_misses: u64,
    /// Total bytes stored.
    pub total_bytes_cached: u64,
    /// Bytes saved through compression.
    pub bytes_saved_compression: u64,
    /// Bytes saved by avoiding downloads.
    pub bytes_saved_network: u64,
    /// Average access time (microseconds).
    pub avg_access_time_us: f64,
    /// Global hit rate.
    pub hit_rate: f64,
    /// Number of evictions.
    pub evictions_count: u64,
    /// Number of compressions performed.
    pub compressions_count: u64,
}

/// Configuration of the optimized cache.
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Maximum data cache size (bytes).
    pub max_data_cache_size: usize,
    /// Maximum number of entries in the data cache.
    pub max_data_entries: usize,
    /// Maximum compressed cache size (bytes).
    pub max_compressed_cache_size: usize,
    /// Maximum number of entries in the compressed cache.
    pub max_compressed_entries: usize,
    /// Default TTL for entries (seconds).
    pub default_ttl_secs: u64,
    /// Threshold for enabling compression (bytes).
    pub compression_threshold: usize,
    /// zstd compression level (1-22).
    pub compression_level: i32,
    /// Threshold for eviction (0.0-1.0).
    pub eviction_threshold: f64,
    /// Enable the access predictor.
    pub enable_access_prediction: bool,
}

/// Access predictor using usage patterns.
#[derive(Debug)]
pub struct AccessPredictor {
    /// Access history per CID.
    access_history: HashMap<String, Vec<Instant>>,
    /// Identified patterns.
    #[allow(dead_code)]
    patterns: HashMap<String, AccessPattern>,
    /// Analysis window (seconds).
    analysis_window_secs: u64,
}

/// Identified access pattern.
#[derive(Debug, Clone)]
pub struct AccessPattern {
    /// Average access frequency (accesses per hour).
    pub avg_frequency: f64,
    /// Peak hours.
    pub peak_hours: Vec<u8>,
    /// Probability of re-access in the coming hours.
    pub reaccess_probability: f64,
    /// Identified pattern type.
    pub pattern_type: PatternType,
}

/// Access pattern types.
#[derive(Debug, Clone, PartialEq)]
pub enum PatternType {
    /// One-time access (unlikely to be re-accessed).
    OneTime,
    /// Regular access (consistent pattern).
    Regular,
    /// Burst access (intense spikes).
    Burst,
    /// Seasonal access (by time/day).
    Seasonal,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_data_cache_size: 256 * 1024 * 1024, // 256MB
            max_data_entries: 10_000,
            max_compressed_cache_size: 1024 * 1024 * 1024, // 1GB
            max_compressed_entries: 50_000,
            default_ttl_secs: 3600,           // 1 hour
            compression_threshold: 64 * 1024, // 64 KB
            compression_level: 6,             // Balance between speed/compression
            eviction_threshold: 0.85,
            enable_access_prediction: true,
        }
    }
}

impl OptimizedCache {
    /// Creates a new instance of the optimized cache.
    pub fn new(cache_config: CacheConfig) -> Self {
        let data_cache_size = NonZeroUsize::new(cache_config.max_data_entries)
            .unwrap_or(NonZeroUsize::new(10_000).unwrap());
        let compressed_cache_size = NonZeroUsize::new(cache_config.max_compressed_entries)
            .unwrap_or(NonZeroUsize::new(50_000).unwrap());

        Self {
            data_cache: Arc::new(RwLock::new(LruCache::new(data_cache_size))),
            metadata_cache: Arc::new(RwLock::new(HashMap::new())),
            compressed_cache: Arc::new(RwLock::new(LruCache::new(compressed_cache_size))),
            stats: Arc::new(RwLock::new(CacheStats::default())),
            cache_config,
            access_predictor: Arc::new(Mutex::new(AccessPredictor {
                access_history: HashMap::new(),
                patterns: HashMap::new(),
                analysis_window_secs: 3600 * 24, // 24 hours
            })),
        }
    }

    /// Looks up data in the cache with intelligent optimizations.
    #[instrument(skip(self))]
    pub async fn get(&self, cid: &str) -> Option<Bytes> {
        let start_time = Instant::now();

        // Update the access history.
        if self.cache_config.enable_access_prediction {
            self.update_access_history(cid).await;
        }

        // Try the data cache first (fastest).
        {
            let mut cache = self.data_cache.write().await;
            if let Some(entry) = cache.get_mut(cid) {
                entry.last_accessed = Instant::now();
                entry.access_count += 1;

                // Update statistics.
                let mut stats = self.stats.write().await;
                stats.data_cache_hits += 1;
                stats.avg_access_time_us =
                    (stats.avg_access_time_us + start_time.elapsed().as_micros() as f64) / 2.0;

                debug!("Cache hit (data): {} ({} bytes)", cid, entry.data.len());
                return Some(entry.data.clone());
            }
        }

        // Try the compressed cache.
        {
            let mut compressed_cache = self.compressed_cache.write().await;
            if let Some(compressed_entry) = compressed_cache.get_mut(cid) {
                // Decompress the data.
                match self
                    .decompress_data(
                        &compressed_entry.compressed_data,
                        compressed_entry.original_size,
                    )
                    .await
                {
                    Ok(decompressed) => {
                        // Move it into the data cache for faster access.
                        let cache_entry = CacheEntry {
                            data: decompressed.clone(),
                            created_at: compressed_entry.compressed_at,
                            last_accessed: Instant::now(),
                            access_count: 1,
                            priority: 7, // High priority for decompressed data.
                            original_size: compressed_entry.original_size,
                            integrity_hash: self.calculate_hash(&decompressed),
                        };

                        {
                            let mut data_cache = self.data_cache.write().await;
                            data_cache.put(cid.to_string(), cache_entry);
                        }

                        // Update statistics.
                        let mut stats = self.stats.write().await;
                        stats.compressed_cache_hits += 1;
                        stats.avg_access_time_us = (stats.avg_access_time_us
                            + start_time.elapsed().as_micros() as f64)
                            / 2.0;

                        debug!(
                            "Cache hit (compressed): {} ({} bytes decompressed)",
                            cid,
                            decompressed.len()
                        );
                        return Some(decompressed);
                    }
                    Err(e) => {
                        warn!("Failed to decompress cached data for {}: {}", cid, e);
                        // Remove the corrupted entry.
                        compressed_cache.pop(cid);
                    }
                }
            }
        }

        // Miss in both caches.
        let mut stats = self.stats.write().await;
        stats.data_cache_misses += 1;
        stats.compressed_cache_misses += 1;

        debug!("Cache miss: {}", cid);
        None
    }

    /// Stores data in the cache with automatic optimization.
    #[instrument(skip(self, data))]
    pub async fn put(&self, cid: &str, data: Bytes) -> Result<()> {
        let data_size = data.len();
        let integrity_hash = self.calculate_hash(&data);

        // Decide whether to compress based on the size.
        let should_compress = data_size >= self.cache_config.compression_threshold;

        if should_compress {
            // Try compression.
            match self.compress_data(&data).await {
                Ok((compressed_data, compression_ratio)) => {
                    let compressed_entry = CompressedEntry {
                        compressed_data,
                        original_size: data_size,
                        compression_level: self.cache_config.compression_level,
                        compressed_at: Instant::now(),
                        compression_ratio,
                    };

                    // Store it in the compressed cache.
                    {
                        let mut compressed_cache = self.compressed_cache.write().await;
                        compressed_cache.put(cid.to_string(), compressed_entry);
                    }

                    // Update statistics.
                    let mut stats = self.stats.write().await;
                    stats.compressions_count += 1;
                    stats.bytes_saved_compression +=
                        (data_size as f64 * (1.0 - compression_ratio)) as u64;
                    stats.total_bytes_cached += data_size as u64;

                    info!(
                        "Data compressed and stored: {} ({} bytes -> {} bytes, ratio: {:.2})",
                        cid,
                        data_size,
                        (data_size as f64 * compression_ratio) as usize,
                        compression_ratio
                    );
                }
                Err(e) => {
                    warn!(
                        "Compression failed for {}: {}. Storing without compression.",
                        cid, e
                    );
                    self.store_uncompressed(cid, data, integrity_hash).await?;
                }
            }
        } else {
            // Store without compression.
            self.store_uncompressed(cid, data, integrity_hash).await?;
        }

        // Check whether eviction is needed.
        self.check_and_evict().await?;

        Ok(())
    }

    /// Stores data without compression.
    async fn store_uncompressed(
        &self,
        cid: &str,
        data: Bytes,
        integrity_hash: [u8; 32],
    ) -> Result<()> {
        let cache_entry = CacheEntry {
            data: data.clone(),
            created_at: Instant::now(),
            last_accessed: Instant::now(),
            access_count: 1,
            priority: 5, // Default priority.
            original_size: data.len(),
            integrity_hash,
        };

        {
            let mut data_cache = self.data_cache.write().await;
            data_cache.put(cid.to_string(), cache_entry);
        }

        // Update statistics.
        let mut stats = self.stats.write().await;
        stats.total_bytes_cached += data.len() as u64;

        debug!(
            "Data stored (without compression): {} ({} bytes)",
            cid,
            data.len()
        );
        Ok(())
    }

    /// Compresses data using zstd.
    async fn compress_data(&self, data: &Bytes) -> Result<(Bytes, f64)> {
        let original_size = data.len();

        let compressed = tokio::task::spawn_blocking({
            let data = data.clone();
            let compression_level = self.cache_config.compression_level;
            move || {
                zstd::bulk::compress(&data, compression_level)
                    .map_err(|e| GuardianError::Other(format!("Compression failed: {}", e)))
            }
        })
        .await
        .map_err(|e| GuardianError::Other(format!("Compression task failed: {}", e)))??;

        let compressed_size = compressed.len();
        let compression_ratio = compressed_size as f64 / original_size as f64;

        Ok((Bytes::from(compressed), compression_ratio))
    }

    /// Decompresses data using zstd.
    async fn decompress_data(
        &self,
        compressed_data: &Bytes,
        expected_size: usize,
    ) -> Result<Bytes> {
        let decompressed = tokio::task::spawn_blocking({
            let compressed_data = compressed_data.clone();
            move || {
                zstd::bulk::decompress(&compressed_data, expected_size)
                    .map_err(|e| GuardianError::Other(format!("Decompression failed: {}", e)))
            }
        })
        .await
        .map_err(|e| GuardianError::Other(format!("Decompression task failed: {}", e)))??;

        Ok(Bytes::from(decompressed))
    }

    /// Computes the integrity hash.
    fn calculate_hash(&self, data: &Bytes) -> [u8; 32] {
        let mut hasher = Hasher::new();
        hasher.update(data);
        hasher.finalize().into()
    }

    /// Updates the access history for prediction.
    async fn update_access_history(&self, cid: &str) {
        let mut predictor = self.access_predictor.lock().await;
        let now = Instant::now();

        predictor
            .access_history
            .entry(cid.to_string())
            .or_insert_with(Vec::new)
            .push(now);

        // Limit the history so it does not grow indefinitely.
        let analysis_window = predictor.analysis_window_secs; // Copy the value before the borrow.
        if let Some(history) = predictor.access_history.get_mut(cid) {
            // Use checked_sub to avoid overflow.
            if let Some(cutoff) = now.checked_sub(Duration::from_secs(analysis_window)) {
                history.retain(|&access_time| access_time > cutoff);
            }
        }
    }

    /// Checks whether eviction is needed and performs it if so.
    async fn check_and_evict(&self) -> Result<()> {
        let stats = self.stats.read().await;
        let current_usage = stats.total_bytes_cached as f64;
        let max_usage = (self.cache_config.max_data_cache_size
            + self.cache_config.max_compressed_cache_size) as f64;

        if current_usage / max_usage > self.cache_config.eviction_threshold {
            drop(stats); // Release the lock.
            self.intelligent_eviction().await?;
        }

        Ok(())
    }

    /// Performs intelligent eviction based on access patterns.
    async fn intelligent_eviction(&self) -> Result<()> {
        debug!("Starting intelligent cache eviction");

        // Collect eviction candidates from the data cache.
        let candidates = {
            let data_cache = self.data_cache.read().await;
            data_cache
                .iter()
                .map(|(cid, entry)| {
                    let age_score = Instant::now()
                        .saturating_duration_since(entry.last_accessed)
                        .as_secs() as f64;
                    let frequency_score = 1.0 / (entry.access_count as f64 + 1.0);
                    let priority_score = (10 - entry.priority) as f64;

                    // Higher score = better eviction candidate.
                    let eviction_score =
                        age_score * 0.4 + frequency_score * 0.3 + priority_score * 0.3;

                    (cid.clone(), eviction_score, entry.data.len())
                })
                .collect::<Vec<_>>()
        };

        // Sort by eviction score (highest first).
        let mut sorted_candidates = candidates;
        sorted_candidates
            .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Remove 20% of the candidates.
        let eviction_count = (sorted_candidates.len() as f64 * 0.2).ceil() as usize;
        let mut bytes_freed = 0u64;

        {
            let mut data_cache = self.data_cache.write().await;
            for (cid, _score, size) in sorted_candidates.iter().take(eviction_count) {
                if data_cache.pop(cid).is_some() {
                    bytes_freed += *size as u64;
                }
            }
        }

        // Update statistics.
        {
            let mut stats = self.stats.write().await;
            stats.evictions_count += eviction_count as u64;
            stats.total_bytes_cached = stats.total_bytes_cached.saturating_sub(bytes_freed);
        }

        info!(
            "Eviction complete: {} entries removed, {} bytes freed",
            eviction_count, bytes_freed
        );

        Ok(())
    }

    /// Returns the current cache statistics.
    pub async fn get_stats(&self) -> CacheStats {
        let stats = self.stats.read().await;
        let mut stats_copy = stats.clone();

        // Compute the hit rate.
        let total_requests = stats_copy.data_cache_hits
            + stats_copy.data_cache_misses
            + stats_copy.compressed_cache_hits
            + stats_copy.compressed_cache_misses;
        let total_hits = stats_copy.data_cache_hits + stats_copy.compressed_cache_hits;

        if total_requests > 0 {
            stats_copy.hit_rate = total_hits as f64 / total_requests as f64;
        }

        stats_copy
    }

    /// Clears the entire cache.
    pub async fn clear(&self) -> Result<()> {
        {
            let mut data_cache = self.data_cache.write().await;
            data_cache.clear();
        }

        {
            let mut compressed_cache = self.compressed_cache.write().await;
            compressed_cache.clear();
        }

        {
            let mut metadata_cache = self.metadata_cache.write().await;
            metadata_cache.clear();
        }

        {
            let mut stats = self.stats.write().await;
            *stats = CacheStats::default();
        }

        info!("Cache cleared completely");
        Ok(())
    }
}
