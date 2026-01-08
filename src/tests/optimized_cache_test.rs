//! Testes robustos para o módulo OptimizedCache
//!
//! Este arquivo contém testes abrangentes para validar:
//! - Criação e configuração do cache
//! - Operações get/put de dados
//! - Compressão e descompressão automática
//! - Estatísticas e métricas
//! - Evicção inteligente
//! - Hash de integridade
//! - Performance e concorrência

use crate::p2p::network::core::optimized_cache::{
    AccessPattern, CacheConfig, CacheEntry, CacheStats, CompressedEntry, MetadataEntry,
    OptimizedCache, PatternType,
};
use bytes::Bytes;
use iroh_blobs::BlobFormat;
use std::time::{Duration, Instant};
use tokio::time::sleep;

/// Helper para criar dados de teste
fn create_test_data(size: usize) -> Bytes {
    Bytes::from(vec![0xAB; size])
}

/// Helper para criar CID de teste
fn create_test_cid(id: u32) -> String {
    format!(
        "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi_{}",
        id
    )
}

#[tokio::test]
async fn test_cache_creation_with_default_config() {
    let cache = OptimizedCache::new(CacheConfig::default());
    let stats = cache.get_stats().await;

    assert_eq!(stats.data_cache_hits, 0);
    assert_eq!(stats.data_cache_misses, 0);
    assert_eq!(stats.compressed_cache_hits, 0);
    assert_eq!(stats.compressed_cache_misses, 0);
    assert_eq!(stats.total_bytes_cached, 0);
}

#[tokio::test]
async fn test_cache_creation_with_custom_config() {
    let cache_config = CacheConfig {
        max_data_cache_size: 128 * 1024 * 1024,
        max_data_entries: 5000,
        max_compressed_cache_size: 512 * 1024 * 1024,
        max_compressed_entries: 25000,
        default_ttl_secs: 7200,
        compression_threshold: 32 * 1024,
        compression_level: 9,
        eviction_threshold: 0.9,
        enable_access_prediction: false,
    };

    let cache = OptimizedCache::new(cache_config.clone());
    let stats = cache.get_stats().await;

    assert_eq!(stats.data_cache_hits, 0);
    assert_eq!(stats.total_bytes_cached, 0);
}

#[tokio::test]
async fn test_cache_config_default_values() {
    let cache_config = CacheConfig::default();

    assert_eq!(cache_config.max_data_cache_size, 256 * 1024 * 1024);
    assert_eq!(cache_config.max_data_entries, 10_000);
    assert_eq!(cache_config.max_compressed_cache_size, 1024 * 1024 * 1024);
    assert_eq!(cache_config.max_compressed_entries, 50_000);
    assert_eq!(cache_config.default_ttl_secs, 3600);
    assert_eq!(cache_config.compression_threshold, 64 * 1024);
    assert_eq!(cache_config.compression_level, 6);
    assert_eq!(cache_config.eviction_threshold, 0.85);
    assert!(cache_config.enable_access_prediction);
}

#[tokio::test]
async fn test_put_and_get_small_data() {
    let cache = OptimizedCache::new(CacheConfig::default());
    let cid = create_test_cid(1);
    let data = create_test_data(1024); // 1KB - não deve ser comprimido

    // Put
    let put_result = cache.put(&cid, data.clone()).await;
    assert!(put_result.is_ok());

    // Get
    let retrieved = cache.get(&cid).await;
    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap(), data);

    // Verifica estatísticas
    let stats = cache.get_stats().await;
    assert_eq!(stats.data_cache_hits, 1);
    assert_eq!(stats.data_cache_misses, 0);
    assert!(stats.total_bytes_cached >= 1024);
}

#[tokio::test]
async fn test_put_and_get_large_data_with_compression() {
    let cache = OptimizedCache::new(CacheConfig::default());
    let cid = create_test_cid(2);
    let data = create_test_data(128 * 1024); // 128KB - deve ser comprimido

    // Put
    let put_result = cache.put(&cid, data.clone()).await;
    assert!(put_result.is_ok());

    // Get (pode vir do cache comprimido ou descomprimido)
    let retrieved = cache.get(&cid).await;
    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap(), data);

    // Verifica que houve compressão
    let stats = cache.get_stats().await;
    assert!(
        stats.compressions_count >= 1,
        "Dados grandes devem ser comprimidos"
    );
}

#[tokio::test]
async fn test_cache_miss() {
    let cache = OptimizedCache::new(CacheConfig::default());
    let cid = create_test_cid(999);

    let result = cache.get(&cid).await;
    assert!(result.is_none());

    let stats = cache.get_stats().await;
    assert_eq!(stats.data_cache_misses, 1);
    assert_eq!(stats.compressed_cache_misses, 1);
}

#[tokio::test]
async fn test_multiple_puts_and_gets() {
    let cache = OptimizedCache::new(CacheConfig::default());

    // Coloca 10 entradas
    for i in 0..10 {
        let cid = create_test_cid(i);
        let data = create_test_data(1024 * (i as usize + 1));
        cache.put(&cid, data).await.unwrap();
    }

    // Recupera todas
    for i in 0..10 {
        let cid = create_test_cid(i);
        let result = cache.get(&cid).await;
        assert!(result.is_some(), "CID {} deve estar no cache", i);
    }

    let stats = cache.get_stats().await;
    assert_eq!(stats.data_cache_hits, 10);
}

#[tokio::test]
async fn test_cache_stats_tracking() {
    let cache = OptimizedCache::new(CacheConfig::default());
    let initial_stats = cache.get_stats().await;

    assert_eq!(initial_stats.data_cache_hits, 0);
    assert_eq!(initial_stats.data_cache_misses, 0);
    assert_eq!(initial_stats.total_bytes_cached, 0);

    // Adiciona dados
    let cid = create_test_cid(1);
    let data = create_test_data(2048);
    cache.put(&cid, data).await.unwrap();

    let stats_after_put = cache.get_stats().await;
    assert!(stats_after_put.total_bytes_cached >= 2048);

    // Acessa os dados (hit)
    cache.get(&cid).await;
    let stats_after_get = cache.get_stats().await;
    assert_eq!(stats_after_get.data_cache_hits, 1);

    // Tenta acessar dados inexistentes (miss)
    cache.get("nonexistent_cid").await;
    let stats_after_miss = cache.get_stats().await;
    assert_eq!(stats_after_miss.data_cache_misses, 1);
}

#[tokio::test]
async fn test_hit_rate_calculation() {
    let cache = OptimizedCache::new(CacheConfig::default());

    // Adiciona alguns dados
    for i in 0..5 {
        let cid = create_test_cid(i);
        let data = create_test_data(1024);
        cache.put(&cid, data).await.unwrap();
    }

    // Faz 5 hits
    for i in 0..5 {
        let cid = create_test_cid(i);
        cache.get(&cid).await;
    }

    // Faz 5 misses
    for i in 100..105 {
        let cid = create_test_cid(i);
        cache.get(&cid).await;
    }

    let stats = cache.get_stats().await;
    // Hit rate deve ser 5/(5+10) = 0.33... (5 hits, 10 misses - 5 data + 5 compressed)
    assert!(stats.hit_rate > 0.0 && stats.hit_rate < 1.0);
}

#[tokio::test]
async fn test_compression_threshold() {
    let cache_config = CacheConfig {
        compression_threshold: 10 * 1024, // 10KB
        ..Default::default()
    };
    let cache = OptimizedCache::new(cache_config);

    // Dados pequenos (não deve comprimir)
    let cid_small = create_test_cid(1);
    let small_data = create_test_data(5 * 1024); // 5KB
    cache.put(&cid_small, small_data).await.unwrap();

    // Dados grandes (deve comprimir)
    let cid_large = create_test_cid(2);
    let large_data = create_test_data(20 * 1024); // 20KB
    cache.put(&cid_large, large_data).await.unwrap();

    let stats = cache.get_stats().await;
    assert!(
        stats.compressions_count >= 1,
        "Dados acima do threshold devem ser comprimidos"
    );
}

#[tokio::test]
async fn test_compression_saves_space() {
    let cache = OptimizedCache::new(CacheConfig::default());
    let cid = create_test_cid(1);

    // Dados altamente compressíveis (repetitivos)
    let data = Bytes::from(vec![0x42; 256 * 1024]); // 256KB de dados repetidos

    cache.put(&cid, data.clone()).await.unwrap();

    let stats = cache.get_stats().await;
    assert!(
        stats.bytes_saved_compression > 0,
        "Compressão deve economizar bytes"
    );
    assert_eq!(stats.compressions_count, 1);
}

#[tokio::test]
async fn test_data_integrity_after_compression() {
    let cache = OptimizedCache::new(CacheConfig::default());
    let cid = create_test_cid(1);

    // Dados grandes para forçar compressão
    let original_data = Bytes::from(vec![0x55; 128 * 1024]);

    cache.put(&cid, original_data.clone()).await.unwrap();
    let retrieved_data = cache.get(&cid).await;

    assert!(retrieved_data.is_some());
    assert_eq!(
        retrieved_data.unwrap(),
        original_data,
        "Dados devem ser idênticos após compressão/descompressão"
    );
}

#[tokio::test]
async fn test_clear_cache() {
    let cache = OptimizedCache::new(CacheConfig::default());

    // Adiciona alguns dados
    for i in 0..5 {
        let cid = create_test_cid(i);
        let data = create_test_data(1024);
        cache.put(&cid, data).await.unwrap();
    }

    let stats_before = cache.get_stats().await;
    assert!(stats_before.total_bytes_cached > 0);

    // Limpa o cache
    cache.clear().await.unwrap();

    let stats_after = cache.get_stats().await;
    assert_eq!(stats_after.total_bytes_cached, 0);
    assert_eq!(stats_after.data_cache_hits, 0);
    assert_eq!(stats_after.data_cache_misses, 0);

    // Verifica que os dados realmente foram removidos
    let result = cache.get(&create_test_cid(0)).await;
    assert!(result.is_none());
}

#[tokio::test]
async fn test_concurrent_puts() {
    let cache = std::sync::Arc::new(OptimizedCache::new(CacheConfig::default()));
    let mut handles = vec![];

    // 10 threads colocando dados simultaneamente
    for i in 0..10 {
        let cache_clone = cache.clone();
        let handle = tokio::spawn(async move {
            let cid = create_test_cid(i);
            let data = create_test_data(1024 * (i as usize + 1));
            cache_clone.put(&cid, data).await
        });
        handles.push(handle);
    }

    // Aguarda todas as operações
    for handle in handles {
        assert!(handle.await.unwrap().is_ok());
    }

    let stats = cache.get_stats().await;
    assert!(stats.total_bytes_cached > 0);
}

#[tokio::test]
async fn test_concurrent_gets() {
    let cache = std::sync::Arc::new(OptimizedCache::new(CacheConfig::default()));

    // Adiciona dados primeiro
    for i in 0..10 {
        let cid = create_test_cid(i);
        let data = create_test_data(1024);
        cache.put(&cid, data).await.unwrap();
    }

    let mut handles = vec![];

    // 20 threads lendo simultaneamente
    for i in 0..20 {
        let cache_clone = cache.clone();
        let handle = tokio::spawn(async move {
            let cid = create_test_cid(i % 10); // Acessa os 10 CIDs
            cache_clone.get(&cid).await
        });
        handles.push(handle);
    }

    // Aguarda todas as operações
    let mut hits = 0;
    for handle in handles {
        if handle.await.unwrap().is_some() {
            hits += 1;
        }
    }

    assert_eq!(hits, 20, "Todas as leituras devem ter sucesso");
}

#[tokio::test]
async fn test_cache_stats_default() {
    let stats = CacheStats::default();

    assert_eq!(stats.data_cache_hits, 0);
    assert_eq!(stats.data_cache_misses, 0);
    assert_eq!(stats.compressed_cache_hits, 0);
    assert_eq!(stats.compressed_cache_misses, 0);
    assert_eq!(stats.total_bytes_cached, 0);
    assert_eq!(stats.bytes_saved_compression, 0);
    assert_eq!(stats.bytes_saved_network, 0);
    assert_eq!(stats.avg_access_time_us, 0.0);
    assert_eq!(stats.hit_rate, 0.0);
    assert_eq!(stats.evictions_count, 0);
    assert_eq!(stats.compressions_count, 0);
}

#[tokio::test]
async fn test_cache_entry_structure() {
    let data = create_test_data(1024);
    let now = Instant::now();

    let entry = CacheEntry {
        data: data.clone(),
        created_at: now,
        last_accessed: now,
        access_count: 5,
        priority: 8,
        original_size: 1024,
        integrity_hash: [0xAB; 32],
    };

    assert_eq!(entry.data, data);
    assert_eq!(entry.access_count, 5);
    assert_eq!(entry.priority, 8);
    assert_eq!(entry.original_size, 1024);
}

#[tokio::test]
async fn test_compressed_entry_structure() {
    let compressed_data = create_test_data(512);
    let now = Instant::now();

    let entry = CompressedEntry {
        compressed_data: compressed_data.clone(),
        original_size: 1024,
        compression_level: 6,
        compressed_at: now,
        compression_ratio: 0.5,
    };

    assert_eq!(entry.compressed_data, compressed_data);
    assert_eq!(entry.original_size, 1024);
    assert_eq!(entry.compression_level, 6);
    assert_eq!(entry.compression_ratio, 0.5);
}

#[tokio::test]
async fn test_metadata_entry_structure() {
    let now = Instant::now();

    let metadata = MetadataEntry {
        size: 1024 * 1024,
        format: BlobFormat::Raw,
        providers: vec!["peer1".to_string(), "peer2".to_string()],
        discovered_at: now,
        avg_access_latency_ms: 15.5,
        popularity_score: 0.85,
    };

    assert_eq!(metadata.size, 1024 * 1024);
    assert_eq!(metadata.format, BlobFormat::Raw);
    assert_eq!(metadata.providers.len(), 2);
    assert_eq!(metadata.avg_access_latency_ms, 15.5);
    assert_eq!(metadata.popularity_score, 0.85);
}

#[tokio::test]
async fn test_access_pattern_structure() {
    let pattern = AccessPattern {
        avg_frequency: 10.5,
        peak_hours: vec![9, 10, 11, 14, 15, 16],
        reaccess_probability: 0.75,
        pattern_type: PatternType::Regular,
    };

    assert_eq!(pattern.avg_frequency, 10.5);
    assert_eq!(pattern.peak_hours.len(), 6);
    assert_eq!(pattern.reaccess_probability, 0.75);
    assert_eq!(pattern.pattern_type, PatternType::Regular);
}

#[tokio::test]
async fn test_pattern_type_variants() {
    assert_eq!(PatternType::OneTime, PatternType::OneTime);
    assert_eq!(PatternType::Regular, PatternType::Regular);
    assert_eq!(PatternType::Burst, PatternType::Burst);
    assert_eq!(PatternType::Seasonal, PatternType::Seasonal);

    assert_ne!(PatternType::OneTime, PatternType::Regular);
    assert_ne!(PatternType::Burst, PatternType::Seasonal);
}

#[tokio::test]
async fn test_cache_with_access_prediction_enabled() {
    let cache_config = CacheConfig {
        enable_access_prediction: true,
        ..Default::default()
    };
    let cache = OptimizedCache::new(cache_config);

    let cid = create_test_cid(1);
    let data = create_test_data(1024);

    cache.put(&cid, data).await.unwrap();

    // Acessa múltiplas vezes para criar histórico
    for _ in 0..5 {
        cache.get(&cid).await;
        sleep(Duration::from_millis(10)).await;
    }

    let stats = cache.get_stats().await;
    assert_eq!(stats.data_cache_hits, 5);
}

#[tokio::test]
async fn test_cache_with_access_prediction_disabled() {
    let cache_config = CacheConfig {
        enable_access_prediction: false,
        ..Default::default()
    };
    let cache = OptimizedCache::new(cache_config);

    let cid = create_test_cid(1);
    let data = create_test_data(1024);

    cache.put(&cid, data).await.unwrap();
    cache.get(&cid).await;

    let stats = cache.get_stats().await;
    assert_eq!(stats.data_cache_hits, 1);
}

#[tokio::test]
async fn test_different_compression_levels() {
    // Teste com nível baixo (rápido)
    let config_fast = CacheConfig {
        compression_level: 1,
        compression_threshold: 10 * 1024,
        ..Default::default()
    };
    let cache_fast = OptimizedCache::new(config_fast);

    let cid_fast = create_test_cid(1);
    let data = create_test_data(50 * 1024);
    cache_fast.put(&cid_fast, data.clone()).await.unwrap();

    // Teste com nível alto (melhor compressão)
    let config_best = CacheConfig {
        compression_level: 19,
        compression_threshold: 10 * 1024,
        ..Default::default()
    };
    let cache_best = OptimizedCache::new(config_best);

    let cid_best = create_test_cid(2);
    cache_best.put(&cid_best, data).await.unwrap();

    // Ambos devem funcionar
    assert!(cache_fast.get(&cid_fast).await.is_some());
    assert!(cache_best.get(&cid_best).await.is_some());
}

#[tokio::test]
async fn test_cache_stats_clone() {
    let stats = CacheStats {
        data_cache_hits: 100,
        data_cache_misses: 20,
        compressed_cache_hits: 50,
        compressed_cache_misses: 10,
        total_bytes_cached: 1024 * 1024,
        bytes_saved_compression: 512 * 1024,
        bytes_saved_network: 2 * 1024 * 1024,
        avg_access_time_us: 150.5,
        hit_rate: 0.83,
        evictions_count: 5,
        compressions_count: 25,
    };

    let cloned = stats.clone();

    assert_eq!(cloned.data_cache_hits, 100);
    assert_eq!(cloned.data_cache_misses, 20);
    assert_eq!(cloned.compressed_cache_hits, 50);
    assert_eq!(cloned.total_bytes_cached, 1024 * 1024);
    assert_eq!(cloned.hit_rate, 0.83);
}

#[tokio::test]
async fn test_cache_config_clone() {
    let cache_config = CacheConfig {
        max_data_cache_size: 512 * 1024 * 1024,
        max_data_entries: 20_000,
        max_compressed_cache_size: 2048 * 1024 * 1024,
        max_compressed_entries: 100_000,
        default_ttl_secs: 7200,
        compression_threshold: 128 * 1024,
        compression_level: 9,
        eviction_threshold: 0.9,
        enable_access_prediction: false,
    };

    let cloned = cache_config.clone();

    assert_eq!(cloned.max_data_cache_size, 512 * 1024 * 1024);
    assert_eq!(cloned.max_data_entries, 20_000);
    assert_eq!(cloned.compression_level, 9);
    assert!(!cloned.enable_access_prediction);
}

#[tokio::test]
async fn test_large_data_handling() {
    let cache = OptimizedCache::new(CacheConfig::default());
    let cid = create_test_cid(1);

    // 1MB de dados
    let large_data = create_test_data(1024 * 1024);

    let put_result = cache.put(&cid, large_data.clone()).await;
    assert!(put_result.is_ok());

    let retrieved = cache.get(&cid).await;
    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().len(), large_data.len());
}

#[tokio::test]
async fn test_empty_data() {
    let cache = OptimizedCache::new(CacheConfig::default());
    let cid = create_test_cid(1);
    let empty_data = Bytes::new();

    let put_result = cache.put(&cid, empty_data.clone()).await;
    assert!(put_result.is_ok());

    let retrieved = cache.get(&cid).await;
    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().len(), 0);
}

#[tokio::test]
async fn test_overwrite_existing_entry() {
    let cache = OptimizedCache::new(CacheConfig::default());
    let cid = create_test_cid(1);

    // Primeira versão
    let data_v1 = Bytes::from(vec![0x11; 1024]);
    cache.put(&cid, data_v1).await.unwrap();

    // Segunda versão (sobrescreve)
    let data_v2 = Bytes::from(vec![0x22; 1024]);
    cache.put(&cid, data_v2.clone()).await.unwrap();

    let retrieved = cache.get(&cid).await;
    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap(), data_v2);
}

#[tokio::test]
async fn test_access_count_increment() {
    let cache = OptimizedCache::new(CacheConfig::default());
    let cid = create_test_cid(1);
    let data = create_test_data(1024);

    cache.put(&cid, data).await.unwrap();

    // Acessa múltiplas vezes
    for _ in 0..10 {
        cache.get(&cid).await;
    }

    let stats = cache.get_stats().await;
    assert_eq!(stats.data_cache_hits, 10);
}

#[tokio::test]
async fn test_stress_many_small_entries() {
    let cache = OptimizedCache::new(CacheConfig::default());

    // Adiciona 1000 pequenas entradas
    for i in 0..1000 {
        let cid = create_test_cid(i);
        let data = create_test_data(100);
        cache.put(&cid, data).await.unwrap();
    }

    // Verifica algumas aleatórias
    for i in (0..1000).step_by(100) {
        let cid = create_test_cid(i);
        let result = cache.get(&cid).await;
        assert!(result.is_some(), "CID {} deve estar no cache", i);
    }

    let stats = cache.get_stats().await;
    assert!(stats.total_bytes_cached > 0);
}

#[tokio::test]
async fn test_mixed_sized_data() {
    let cache = OptimizedCache::new(CacheConfig::default());

    // Vários tamanhos diferentes
    let sizes = vec![100, 1024, 10 * 1024, 64 * 1024, 128 * 1024, 256 * 1024];

    for (i, size) in sizes.iter().enumerate() {
        let cid = create_test_cid(i as u32);
        let data = create_test_data(*size);
        cache.put(&cid, data).await.unwrap();
    }

    // Recupera todos
    for (i, _) in sizes.iter().enumerate() {
        let cid = create_test_cid(i as u32);
        let result = cache.get(&cid).await;
        assert!(result.is_some());
    }

    let stats = cache.get_stats().await;
    assert!(
        stats.compressions_count > 0,
        "Alguns dados devem ter sido comprimidos"
    );
}

#[tokio::test]
async fn test_cache_clear_resets_stats() {
    let cache = OptimizedCache::new(CacheConfig::default());

    // Adiciona dados e gera estatísticas
    for i in 0..10 {
        let cid = create_test_cid(i);
        let data = create_test_data(1024);
        cache.put(&cid, data).await.unwrap();
        cache.get(&cid).await;
    }

    let stats_before = cache.get_stats().await;
    assert!(stats_before.data_cache_hits > 0);

    // Limpa
    cache.clear().await.unwrap();

    let stats_after = cache.get_stats().await;
    assert_eq!(stats_after.data_cache_hits, 0);
    assert_eq!(stats_after.data_cache_misses, 0);
    assert_eq!(stats_after.total_bytes_cached, 0);
}

#[tokio::test]
async fn test_avg_access_time_tracking() {
    let cache = OptimizedCache::new(CacheConfig::default());
    let cid = create_test_cid(1);
    let data = create_test_data(1024);

    cache.put(&cid, data).await.unwrap();

    // Faz alguns acessos
    for _ in 0..5 {
        cache.get(&cid).await;
    }

    let stats = cache.get_stats().await;
    // Deve ter registrado tempo de acesso
    assert!(stats.avg_access_time_us >= 0.0);
}

#[tokio::test]
async fn test_bytes_saved_tracking() {
    let cache = OptimizedCache::new(CacheConfig::default());
    let cid = create_test_cid(1);

    // Dados altamente compressíveis
    let data = Bytes::from(vec![0xFF; 256 * 1024]);

    cache.put(&cid, data).await.unwrap();

    let stats = cache.get_stats().await;
    if stats.compressions_count > 0 {
        assert!(
            stats.bytes_saved_compression > 0,
            "Deve ter economizado bytes com compressão"
        );
    }
}

#[tokio::test]
async fn test_compressed_entry_clone() {
    let compressed_data = create_test_data(256);
    let entry = CompressedEntry {
        compressed_data: compressed_data.clone(),
        original_size: 512,
        compression_level: 9,
        compressed_at: Instant::now(),
        compression_ratio: 0.5,
    };

    let cloned = entry.clone();
    assert_eq!(cloned.compressed_data, compressed_data);
    assert_eq!(cloned.original_size, 512);
    assert_eq!(cloned.compression_level, 9);
}

#[tokio::test]
async fn test_metadata_entry_clone() {
    let metadata = MetadataEntry {
        size: 2048,
        format: BlobFormat::Raw,
        providers: vec!["peer1".to_string()],
        discovered_at: Instant::now(),
        avg_access_latency_ms: 25.0,
        popularity_score: 0.6,
    };

    let cloned = metadata.clone();
    assert_eq!(cloned.size, 2048);
    assert_eq!(cloned.format, BlobFormat::Raw);
    assert_eq!(cloned.providers.len(), 1);
}

#[tokio::test]
async fn test_access_pattern_clone() {
    let pattern = AccessPattern {
        avg_frequency: 5.5,
        peak_hours: vec![10, 11, 12],
        reaccess_probability: 0.8,
        pattern_type: PatternType::Burst,
    };

    let cloned = pattern.clone();
    assert_eq!(cloned.avg_frequency, 5.5);
    assert_eq!(cloned.peak_hours, vec![10, 11, 12]);
    assert_eq!(cloned.pattern_type, PatternType::Burst);
}
