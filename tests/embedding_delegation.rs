#![cfg(all(feature = "embedding", feature = "compute"))]
//! Two-node integration tests for RFC 0005 §6.4 embedding delegation: a
//! requester sends an `Embed` request over `COMPUTE_ALPN` to an executor that
//! serves the model, and gets the vector back — including the §6.3 hash-pin
//! path and the gossip-directory `ComputeEmbeddingDelegate`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;

use guardian_db::compute::protocol::{EmbedReply, EmbedRequest};
use guardian_db::compute::{
    COMPUTE_ALPN, CapabilityDirectory, ComputeClient, ComputeProtocolHandler, ExecutorPolicy,
};
use guardian_db::embedding::{
    ComputeEmbeddingDelegate, EmbedDelegateConfig, Embedder, EmbeddingDelegate, EmbeddingRegistry,
    HashEmbedder, ModelSelector,
};
use iroh::endpoint::{Endpoint, presets};
use iroh::protocol::Router;
use iroh::{EndpointAddr, TransportAddr};
use uuid::Uuid;

const DIMS: u32 = 16;

fn dialable_addr(endpoint: &Endpoint) -> EndpointAddr {
    let addrs: Vec<TransportAddr> = endpoint
        .bound_sockets()
        .into_iter()
        .map(|mut socket| {
            if socket.ip().is_unspecified() {
                let loopback = if socket.is_ipv4() {
                    IpAddr::V4(Ipv4Addr::LOCALHOST)
                } else {
                    IpAddr::V6(Ipv6Addr::LOCALHOST)
                };
                socket.set_ip(loopback);
            }
            TransportAddr::Ip(socket)
        })
        .collect();
    EndpointAddr::from_parts(endpoint.id(), addrs)
}

/// A no-op WASM fetcher: embedding delegation never fetches code.
struct NoFetch;
#[async_trait::async_trait]
impl guardian_db::compute::WasmFetcher for NoFetch {
    async fn fetch_wasm(
        &self,
        _hash: &iroh_blobs::Hash,
        _p: iroh::EndpointId,
    ) -> Result<Vec<u8>, String> {
        Err("no fetch in embedding delegation".into())
    }
}

struct Node {
    endpoint: Endpoint,
    addr: EndpointAddr,
    handler: ComputeProtocolHandler,
    _router: Router,
}

async fn spawn_node() -> Node {
    let endpoint = Endpoint::builder(presets::Minimal)
        .bind()
        .await
        .expect("bind endpoint");
    let handler =
        ComputeProtocolHandler::new(Arc::new(NoFetch), ExecutorPolicy::default()).expect("handler");
    let router = Router::builder(endpoint.clone())
        .accept(COMPUTE_ALPN, handler.clone())
        .spawn();
    let addr = dialable_addr(&endpoint);
    Node {
        endpoint,
        addr,
        handler,
        _router: router,
    }
}

/// An executor serving `model`, and a requester's client.
async fn pair(model: &str) -> (Node, Node, EmbeddingRegistry) {
    let requester = spawn_node().await;
    let executor = spawn_node().await;
    let registry = EmbeddingRegistry::new();
    registry.register(Arc::new(HashEmbedder::new(model, DIMS)));
    executor.handler.set_embed_registry(registry.clone());
    (requester, executor, registry)
}

const TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::test]
async fn embed_on_returns_the_executors_vector() {
    let (requester, executor, _) = pair("m").await;
    let client = ComputeClient::new(requester.endpoint.clone());
    let req = EmbedRequest {
        task_id: Uuid::new_v4(),
        model: "m".into(),
        required_hash: None,
        text: "hello world".into(),
    };
    let reply = client
        .embed_on(executor.addr.clone(), req, TIMEOUT)
        .await
        .expect("embed round trip");
    match reply {
        EmbedReply::Ok { vector, .. } => {
            // Must equal the executor's local embedder output for that text.
            let expected = HashEmbedder::new("m", DIMS)
                .embed(&["hello world".to_string()])
                .await
                .unwrap()
                .pop()
                .unwrap();
            assert_eq!(vector, expected);
        }
        EmbedReply::Rejected(m) => panic!("unexpected rejection: {m}"),
    }
    assert_eq!(executor.handler.tasks_running(), 0, "slot released");
}

#[tokio::test]
async fn unknown_model_is_rejected() {
    let (requester, executor, _) = pair("m").await;
    let client = ComputeClient::new(requester.endpoint.clone());
    let req = EmbedRequest {
        task_id: Uuid::new_v4(),
        model: "not-served".into(),
        required_hash: None,
        text: "x".into(),
    };
    let reply = client
        .embed_on(executor.addr.clone(), req, TIMEOUT)
        .await
        .expect("round trip");
    assert!(matches!(reply, EmbedReply::Rejected(_)));
}

#[tokio::test]
async fn hash_pin_is_enforced_across_the_wire() {
    let (requester, executor, _) = pair("m").await;
    let client = ComputeClient::new(requester.endpoint.clone());
    let good = HashEmbedder::new("m", DIMS).info().content_hash.unwrap();
    let bad = iroh_blobs::Hash::from_bytes([7u8; 32]);

    // Correct pin: served.
    let ok = client
        .embed_on(
            executor.addr.clone(),
            EmbedRequest {
                task_id: Uuid::new_v4(),
                model: "m".into(),
                required_hash: Some(good),
                text: "y".into(),
            },
            TIMEOUT,
        )
        .await
        .unwrap();
    assert!(matches!(ok, EmbedReply::Ok { .. }));

    // Wrong pin: rejected, never a name-only fallback.
    let rej = client
        .embed_on(
            executor.addr.clone(),
            EmbedRequest {
                task_id: Uuid::new_v4(),
                model: "m".into(),
                required_hash: Some(bad),
                text: "y".into(),
            },
            TIMEOUT,
        )
        .await
        .unwrap();
    assert!(matches!(rej, EmbedReply::Rejected(_)));
}

#[tokio::test]
async fn directory_delegate_discovers_and_delegates() {
    let (requester, executor, _) = pair("m").await;
    // Seed the requester's directory with the executor's advertised vector.
    let directory = Arc::new(CapabilityDirectory::new());
    let mut sampler = guardian_db::compute::TelemetrySampler::new();
    let vector = sampler.sample(
        executor.endpoint.id(),
        &executor.handler,
        &guardian_db::compute::TelemetryConfig::default(),
    );
    assert!(
        vector.offers_embed_model("m"),
        "executor must advertise the embedding model"
    );
    directory.upsert(vector);
    // The requester must be able to dial the executor by id.
    seed(&requester.endpoint, executor.addr.clone());

    let delegate = ComputeEmbeddingDelegate::new(
        ComputeClient::new(requester.endpoint.clone()),
        directory,
        requester.endpoint.id(),
        EmbedDelegateConfig::default(),
    );
    let out = delegate
        .embed_delegated(&ModelSelector::by_name("m"), false, "hello")
        .await
        .expect("delegated embedding");
    assert_eq!(out.vector.len(), DIMS as usize);
    assert_eq!(out.peer, executor.endpoint.id());
}

#[tokio::test]
async fn delegate_errors_when_no_peer_advertises() {
    let requester = spawn_node().await;
    let directory = Arc::new(CapabilityDirectory::new());
    let delegate = ComputeEmbeddingDelegate::new(
        ComputeClient::new(requester.endpoint.clone()),
        directory,
        requester.endpoint.id(),
        EmbedDelegateConfig::default(),
    );
    let err = delegate
        .embed_delegated(&ModelSelector::by_name("m"), false, "hello")
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("no peer"), "{err}");
}

/// Registers `peer`'s address on `endpoint` so it can be dialed by bare id.
fn seed(endpoint: &Endpoint, peer: EndpointAddr) {
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    lookup.add_endpoint_info(peer);
    endpoint
        .address_lookup()
        .expect("address lookup available")
        .add(lookup);
}
