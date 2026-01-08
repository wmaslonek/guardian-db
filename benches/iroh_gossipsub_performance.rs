// ╔════════════════════════════════════════════════════════════════════════════════╗
// ║                                ⚠ DEPRECATED                                   ║
// ╚════════════════════════════════════════════════════════════════════════════════╝

// Benchmarks para avaliar performance da integração Iroh + LibP2P Gossipsub
//
// Mede throughput, latência e uso de recursos

use criterion::{Criterion, criterion_group, criterion_main};
use guardian_db::p2p::network::{config::ClientConfig, core::IrohBackend};
use std::{hint::black_box, sync::Arc};
use tempfile::TempDir;
use tokio::runtime::Runtime;

fn benchmark_iroh_operations(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    // Setup do backend
    let backend = rt.block_on(async {
        let temp_dir = TempDir::new().unwrap();
        let config = ClientConfig {
            data_store_path: Some(temp_dir.path().to_path_buf()),
            enable_pubsub: true,
            ..Default::default()
        };
        (Arc::new(IrohBackend::new(&config).await.unwrap()), temp_dir)
    });
    let (backend, _temp_dir) = backend;

    // Benchmark: Adicionar conteúdo
    c.bench_function("iroh_add_content", |b| {
        b.iter(|| {
            rt.block_on(async {
                let data = format!("benchmark data {}", black_box(rand::random::<u64>()));
                let data_bytes = data.into_bytes();
                let reader = Box::pin(std::io::Cursor::new(data_bytes));
                backend.add(reader).await.unwrap()
            })
        })
    });

    // Benchmark: Recuperar conteúdo
    let test_cid = rt.block_on(async {
        let data = b"test data for retrieval benchmark".to_vec();
        let reader = Box::pin(std::io::Cursor::new(data));
        backend.add(reader).await.unwrap().hash
    });

    c.bench_function("iroh_cat_content", |b| {
        b.iter(|| {
            rt.block_on(async {
                let mut stream = backend.cat(black_box(&test_cid)).await.unwrap();
                let mut buf = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut buf)
                    .await
                    .unwrap();
                buf
            })
        })
    });

    // Benchmark: Operação de rede (substituindo Gossipsub por ping)
    c.bench_function("network_ping", |b| {
        b.iter(|| {
            rt.block_on(async {
                // Simula operação de rede medindo latência do backend
                let start = std::time::Instant::now();
                let _ = backend.id().await;
                black_box(start.elapsed())
            })
        })
    });
}

fn benchmark_concurrent_operations(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let backend = rt.block_on(async {
        let temp_dir = TempDir::new().unwrap();
        let config = ClientConfig {
            data_store_path: Some(temp_dir.path().to_path_buf()),
            enable_pubsub: true,
            ..Default::default()
        };
        (Arc::new(IrohBackend::new(&config).await.unwrap()), temp_dir)
    });
    let (backend, _temp_dir) = backend;

    c.bench_function("concurrent_add_operations", |b| {
        b.iter(|| {
            rt.block_on(async {
                let tasks = (0..black_box(10)).map(|i| {
                    let backend = backend.clone();
                    tokio::spawn(async move {
                        let data = format!("concurrent data {}", i);
                        let data_bytes = data.into_bytes();
                        let reader = Box::pin(std::io::Cursor::new(data_bytes));
                        backend.add(reader).await.unwrap()
                    })
                });

                futures::future::join_all(tasks).await
            })
        })
    });

    // Cleanup automático com _temp_dir drop
}

fn benchmark_cache_performance(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let backend = rt.block_on(async {
        let temp_dir = TempDir::new().unwrap();
        let config = ClientConfig {
            data_store_path: Some(temp_dir.path().to_path_buf()),
            enable_pubsub: true,
            ..Default::default()
        };
        (Arc::new(IrohBackend::new(&config).await.unwrap()), temp_dir)
    });
    let (backend, _temp_dir) = backend;

    // Adiciona dados para teste de cache
    let test_cids: Vec<String> = rt.block_on(async {
        let mut cids = Vec::new();
        for i in 0..100 {
            let data = format!("cache test data {}", i);
            let data_bytes = data.into_bytes();
            let reader = Box::pin(std::io::Cursor::new(data_bytes));
            let response = backend.add(reader).await.unwrap();
            cids.push(response.hash);
        }
        cids
    });

    c.bench_function("cache_hit_performance", |b| {
        b.iter(|| {
            rt.block_on(async {
                // Acessa o mesmo conteúdo repetidamente para testar cache
                let cid = black_box(&test_cids[0]);
                let mut stream = backend.cat(cid).await.unwrap();
                let mut buf = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut buf)
                    .await
                    .unwrap();
                buf
            })
        })
    });

    // Cleanup automático com _temp_dir drop
}

criterion_group!(
    benches,
    benchmark_iroh_operations,
    benchmark_concurrent_operations,
    benchmark_cache_performance
);
criterion_main!(benches);
