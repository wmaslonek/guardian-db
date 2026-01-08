/// Cache Layer Otimizado para Backend Iroh
///
/// Caching inteligente com compressão, TTL adaptativo e evicção preditiva
/// para maximizar a performance do backend nativo Iroh.
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

/// Cache Layer otimizado para operações Iroh
pub struct OptimizedCache {
    /// Cache LRU para dados recentes
    data_cache: Arc<RwLock<LruCache<String, CacheEntry>>>,
    /// Cache de metadados para CIDs
    metadata_cache: Arc<RwLock<HashMap<String, MetadataEntry>>>,
    /// Cache de compressão para dados grandes
    compressed_cache: Arc<RwLock<LruCache<String, CompressedEntry>>>,
    /// Estatísticas de performance
    stats: Arc<RwLock<CacheStats>>,
    /// Configuração do cache
    cache_config: CacheConfig,
    /// Predictor de acesso para evicção inteligente
    access_predictor: Arc<Mutex<AccessPredictor>>,
}

/// Entrada no cache com metadados de performance
#[derive(Debug, Clone)]
pub struct CacheEntry {
    /// Dados do blob
    pub data: Bytes,
    /// Timestamp de criação
    pub created_at: Instant,
    /// Último acesso
    pub last_accessed: Instant,
    /// Número de acessos
    pub access_count: u64,
    /// Prioridade (0-10, maior = mais importante)
    pub priority: u8,
    /// Tamanho original (antes da compressão, se aplicável)
    pub original_size: usize,
    /// Hash verificação de integridade
    pub integrity_hash: [u8; 32],
}

/// Entrada comprimida para dados grandes
#[derive(Debug, Clone)]
pub struct CompressedEntry {
    /// Dados comprimidos com zstd
    pub compressed_data: Bytes,
    /// Tamanho original
    pub original_size: usize,
    /// Nível de compressão usado
    pub compression_level: i32,
    /// Timestamp de compressão
    pub compressed_at: Instant,
    /// Ratio de compressão (0.0-1.0)
    pub compression_ratio: f64,
}

/// Metadados para CIDs
#[derive(Debug, Clone)]
pub struct MetadataEntry {
    /// Tamanho do blob
    pub size: u64,
    /// Formato do blob (Raw, DagCbor, etc.)
    pub format: BlobFormat,
    /// Peers que possuem o conteúdo
    pub providers: Vec<String>,
    /// Timestamp de descoberta
    pub discovered_at: Instant,
    /// Latência média de acesso (ms)
    pub avg_access_latency_ms: f64,
    /// Popularidade (frequência de acesso)
    pub popularity_score: f64,
}

/// Estatísticas avançadas de cache
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    /// Hits no cache de dados
    pub data_cache_hits: u64,
    /// Misses no cache de dados
    pub data_cache_misses: u64,
    /// Hits no cache comprimido
    pub compressed_cache_hits: u64,
    /// Misses no cache comprimido
    pub compressed_cache_misses: u64,
    /// Total de bytes armazenados
    pub total_bytes_cached: u64,
    /// Bytes economizados com compressão
    pub bytes_saved_compression: u64,
    /// Bytes economizados evitando downloads
    pub bytes_saved_network: u64,
    /// Tempo médio de acesso (microsegundos)
    pub avg_access_time_us: f64,
    /// Taxa de hit global
    pub hit_rate: f64,
    /// Número de evicções
    pub evictions_count: u64,
    /// Número de compressões realizadas
    pub compressions_count: u64,
}

/// Configuração do cache otimizado
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Tamanho máximo do cache de dados (bytes)
    pub max_data_cache_size: usize,
    /// Número máximo de entradas no cache de dados
    pub max_data_entries: usize,
    /// Tamanho máximo do cache comprimido (bytes)
    pub max_compressed_cache_size: usize,
    /// Número máximo de entradas no cache comprimido
    pub max_compressed_entries: usize,
    /// TTL padrão para entradas (segundos)
    pub default_ttl_secs: u64,
    /// Threshold para ativar compressão (bytes)
    pub compression_threshold: usize,
    /// Nível de compressão zstd (1-22)
    pub compression_level: i32,
    /// Threshold para evicção (0.0-1.0)
    pub eviction_threshold: f64,
    /// Habilitar predictor de acesso
    pub enable_access_prediction: bool,
}

/// Predictor de acesso usando padrões de uso
#[derive(Debug)]
pub struct AccessPredictor {
    /// Histórico de acessos por CID
    access_history: HashMap<String, Vec<Instant>>,
    /// Padrões identificados
    #[allow(dead_code)]
    patterns: HashMap<String, AccessPattern>,
    /// Janela de análise (segundos)
    analysis_window_secs: u64,
}

/// Padrão de acesso identificado
#[derive(Debug, Clone)]
pub struct AccessPattern {
    /// Frequência média de acesso (acessos por hora)
    pub avg_frequency: f64,
    /// Horários de pico
    pub peak_hours: Vec<u8>,
    /// Probabilidade de reacesso nas próximas horas
    pub reaccess_probability: f64,
    /// Tipo de padrão identificado
    pub pattern_type: PatternType,
}

/// Tipos de padrões de acesso
#[derive(Debug, Clone, PartialEq)]
pub enum PatternType {
    /// Acesso único (pouco provável de reacesso)
    OneTime,
    /// Acesso regular (padrão consistente)
    Regular,
    /// Acesso burst (picos intensos)
    Burst,
    /// Acesso sazonal (por horário/dia)
    Seasonal,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_data_cache_size: 256 * 1024 * 1024, // 256MB
            max_data_entries: 10_000,
            max_compressed_cache_size: 1024 * 1024 * 1024, // 1GB
            max_compressed_entries: 50_000,
            default_ttl_secs: 3600,           // 1 hora
            compression_threshold: 64 * 1024, // 64KB
            compression_level: 6,             // Balanço entre speed/compression
            eviction_threshold: 0.85,
            enable_access_prediction: true,
        }
    }
}

impl OptimizedCache {
    /// Cria nova instância do cache otimizado
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
                analysis_window_secs: 3600 * 24, // 24 horas
            })),
        }
    }

    /// Busca dados no cache com otimizações inteligentes
    #[instrument(skip(self))]
    pub async fn get(&self, cid: &str) -> Option<Bytes> {
        let start_time = Instant::now();

        // Atualiza histórico de acesso
        if self.cache_config.enable_access_prediction {
            self.update_access_history(cid).await;
        }

        // Tenta cache de dados primeiro (mais rápido)
        {
            let mut cache = self.data_cache.write().await;
            if let Some(entry) = cache.get_mut(cid) {
                entry.last_accessed = Instant::now();
                entry.access_count += 1;

                // Atualiza estatísticas
                let mut stats = self.stats.write().await;
                stats.data_cache_hits += 1;
                stats.avg_access_time_us =
                    (stats.avg_access_time_us + start_time.elapsed().as_micros() as f64) / 2.0;

                debug!("Cache hit (data): {} ({} bytes)", cid, entry.data.len());
                return Some(entry.data.clone());
            }
        }

        // Tenta cache comprimido
        {
            let mut compressed_cache = self.compressed_cache.write().await;
            if let Some(compressed_entry) = compressed_cache.get_mut(cid) {
                // Descomprime os dados
                match self
                    .decompress_data(
                        &compressed_entry.compressed_data,
                        compressed_entry.original_size,
                    )
                    .await
                {
                    Ok(decompressed) => {
                        // Move para cache de dados para acesso mais rápido
                        let cache_entry = CacheEntry {
                            data: decompressed.clone(),
                            created_at: compressed_entry.compressed_at,
                            last_accessed: Instant::now(),
                            access_count: 1,
                            priority: 7, // Prioridade alta para dados descomprimidos
                            original_size: compressed_entry.original_size,
                            integrity_hash: self.calculate_hash(&decompressed),
                        };

                        {
                            let mut data_cache = self.data_cache.write().await;
                            data_cache.put(cid.to_string(), cache_entry);
                        }

                        // Atualiza estatísticas
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
                        warn!("Falha ao descomprimir dados do cache para {}: {}", cid, e);
                        // Remove entrada corrompida
                        compressed_cache.pop(cid);
                    }
                }
            }
        }

        // Miss em ambos os caches
        let mut stats = self.stats.write().await;
        stats.data_cache_misses += 1;
        stats.compressed_cache_misses += 1;

        debug!("Cache miss: {}", cid);
        None
    }

    /// Armazena dados no cache com otimização automática
    #[instrument(skip(self, data))]
    pub async fn put(&self, cid: &str, data: Bytes) -> Result<()> {
        let data_size = data.len();
        let integrity_hash = self.calculate_hash(&data);

        // Decide se deve comprimir baseado no tamanho
        let should_compress = data_size >= self.cache_config.compression_threshold;

        if should_compress {
            // Tenta compressão
            match self.compress_data(&data).await {
                Ok((compressed_data, compression_ratio)) => {
                    let compressed_entry = CompressedEntry {
                        compressed_data,
                        original_size: data_size,
                        compression_level: self.cache_config.compression_level,
                        compressed_at: Instant::now(),
                        compression_ratio,
                    };

                    // Armazena no cache comprimido
                    {
                        let mut compressed_cache = self.compressed_cache.write().await;
                        compressed_cache.put(cid.to_string(), compressed_entry);
                    }

                    // Atualiza estatísticas
                    let mut stats = self.stats.write().await;
                    stats.compressions_count += 1;
                    stats.bytes_saved_compression +=
                        (data_size as f64 * (1.0 - compression_ratio)) as u64;
                    stats.total_bytes_cached += data_size as u64;

                    info!(
                        "Dados comprimidos e armazenados: {} ({} bytes -> {} bytes, ratio: {:.2})",
                        cid,
                        data_size,
                        (data_size as f64 * compression_ratio) as usize,
                        compression_ratio
                    );
                }
                Err(e) => {
                    warn!(
                        "Falha na compressão para {}: {}. Armazenando sem compressão.",
                        cid, e
                    );
                    self.store_uncompressed(cid, data, integrity_hash).await?;
                }
            }
        } else {
            // Armazena sem compressão
            self.store_uncompressed(cid, data, integrity_hash).await?;
        }

        // Verifica se precisa fazer evicção
        self.check_and_evict().await?;

        Ok(())
    }

    /// Armazena dados sem compressão
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
            priority: 5, // Prioridade padrão
            original_size: data.len(),
            integrity_hash,
        };

        {
            let mut data_cache = self.data_cache.write().await;
            data_cache.put(cid.to_string(), cache_entry);
        }

        // Atualiza estatísticas
        let mut stats = self.stats.write().await;
        stats.total_bytes_cached += data.len() as u64;

        debug!(
            "Dados armazenados (sem compressão): {} ({} bytes)",
            cid,
            data.len()
        );
        Ok(())
    }

    /// Comprime dados usando zstd
    async fn compress_data(&self, data: &Bytes) -> Result<(Bytes, f64)> {
        let original_size = data.len();

        let compressed = tokio::task::spawn_blocking({
            let data = data.clone();
            let compression_level = self.cache_config.compression_level;
            move || {
                zstd::bulk::compress(&data, compression_level)
                    .map_err(|e| GuardianError::Other(format!("Falha na compressão: {}", e)))
            }
        })
        .await
        .map_err(|e| GuardianError::Other(format!("Task de compressão falhou: {}", e)))??;

        let compressed_size = compressed.len();
        let compression_ratio = compressed_size as f64 / original_size as f64;

        Ok((Bytes::from(compressed), compression_ratio))
    }

    /// Descomprime dados usando zstd
    async fn decompress_data(
        &self,
        compressed_data: &Bytes,
        expected_size: usize,
    ) -> Result<Bytes> {
        let decompressed = tokio::task::spawn_blocking({
            let compressed_data = compressed_data.clone();
            move || {
                zstd::bulk::decompress(&compressed_data, expected_size)
                    .map_err(|e| GuardianError::Other(format!("Falha na descompressão: {}", e)))
            }
        })
        .await
        .map_err(|e| GuardianError::Other(format!("Task de descompressão falhou: {}", e)))??;

        Ok(Bytes::from(decompressed))
    }

    /// Calcula hash de integridade
    fn calculate_hash(&self, data: &Bytes) -> [u8; 32] {
        let mut hasher = Hasher::new();
        hasher.update(data);
        hasher.finalize().into()
    }

    /// Atualiza histórico de acesso para predição
    async fn update_access_history(&self, cid: &str) {
        let mut predictor = self.access_predictor.lock().await;
        let now = Instant::now();

        predictor
            .access_history
            .entry(cid.to_string())
            .or_insert_with(Vec::new)
            .push(now);

        // Limita o histórico para não crescer indefinidamente
        let analysis_window = predictor.analysis_window_secs; // Copia valor antes do empréstimo
        if let Some(history) = predictor.access_history.get_mut(cid) {
            let cutoff = now - Duration::from_secs(analysis_window);
            history.retain(|&access_time| access_time > cutoff);
        }
    }

    /// Verifica se precisa fazer evicção e executa se necessário
    async fn check_and_evict(&self) -> Result<()> {
        let stats = self.stats.read().await;
        let current_usage = stats.total_bytes_cached as f64;
        let max_usage = (self.cache_config.max_data_cache_size
            + self.cache_config.max_compressed_cache_size) as f64;

        if current_usage / max_usage > self.cache_config.eviction_threshold {
            drop(stats); // Libera o lock
            self.intelligent_eviction().await?;
        }

        Ok(())
    }

    /// Executa evicção inteligente baseada em padrões de acesso
    async fn intelligent_eviction(&self) -> Result<()> {
        debug!("Iniciando evicção inteligente do cache");

        // Coleta candidatos para evicção do cache de dados
        let candidates = {
            let data_cache = self.data_cache.read().await;
            data_cache
                .iter()
                .map(|(cid, entry)| {
                    let age_score =
                        Instant::now().duration_since(entry.last_accessed).as_secs() as f64;
                    let frequency_score = 1.0 / (entry.access_count as f64 + 1.0);
                    let priority_score = (10 - entry.priority) as f64;

                    // Score maior = melhor candidato para evicção
                    let eviction_score =
                        age_score * 0.4 + frequency_score * 0.3 + priority_score * 0.3;

                    (cid.clone(), eviction_score, entry.data.len())
                })
                .collect::<Vec<_>>()
        };

        // Ordena por score de evicção (maior primeiro)
        let mut sorted_candidates = candidates;
        sorted_candidates
            .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Remove 20% dos candidatos
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

        // Atualiza estatísticas
        {
            let mut stats = self.stats.write().await;
            stats.evictions_count += eviction_count as u64;
            stats.total_bytes_cached = stats.total_bytes_cached.saturating_sub(bytes_freed);
        }

        info!(
            "Evicção concluída: {} entradas removidas, {} bytes liberados",
            eviction_count, bytes_freed
        );

        Ok(())
    }

    /// Obtém estatísticas atuais do cache
    pub async fn get_stats(&self) -> CacheStats {
        let stats = self.stats.read().await;
        let mut stats_copy = stats.clone();

        // Calcula taxa de hit
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

    /// Limpa todo o cache
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

        info!("Cache limpo completamente");
        Ok(())
    }
}
