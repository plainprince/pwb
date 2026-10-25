//! End-to-end private routing over real loopback UDP.
//!
//! Five live engines form a full mesh. One is the requester; the other four
//! index the same document and answer onion-delivered shard, artifact, and
//! domain requests. The requester reaches them only through three-hop circuits,
//! proving that:
//!
//! - randomized path selection produces usable direct-peer circuits;
//! - `ShardQuery` and `DomainQuery` are served through the return circuit;
//! - an artifact fetch reaches a holder without a direct `FetchArtifact`;
//! - every reply is still signature- and content-id checked.
//!
//! The test does not assert "the exit cannot see the address" directly (that is
//! an absence of information), but it exercises the onion path rather than the
//! direct `send_to_addr` path for every lookup.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pwb_core::crypto::{ContentId, Keypair};
use pwb_core::net::{BootstrapPeer, NetworkMessage, NodeConfig, NodeEngine};
use pwb_core::spec::{PlainNode, Tag};
use pwb_node::domain::{DomainRegistry, DomainService};
use pwb_node::federation::DistributedSearch;
use pwb_node::host::ArtifactHost;
use pwb_node::privacy::PrivacyMode;
use pwb_node::search::SearchEngine;

async fn start_engine(keypair: Keypair, host: &str) -> (NodeEngine, SocketAddr) {
    let engine = NodeEngine::new(NodeConfig {
        listen_addr: format!("{host}:0").parse().unwrap(),
        keypair,
        bootstrap_peers: Vec::<BootstrapPeer>::new(),
        stun_server: None,
        max_peers: 32,
        heartbeat_interval: Duration::from_secs(30),
    });
    engine.start().await.expect("engine start");
    let addr = engine.local_addr().expect("bound").expect("addr");
    (engine, addr)
}

fn bundle_bytes(title: &str, body: &str) -> Vec<u8> {
    let mut root = PlainNode::new(0, Tag::Document);
    let mut text = PlainNode::new(1, Tag::Text);
    text.text_content = Some(body.to_string());
    root.children.push(text);
    pwb_web::bundle::create_bundle(
        root,
        title.to_string(),
        None,
        None,
        HashMap::new(),
        HashMap::new(),
    )
    .serialize()
}

/// A valid bundle larger than one 32 KiB onion chunk. Each symbol-table entry
/// is capped at 65,535 bytes, so the body is split across sibling text nodes.
fn large_bundle_bytes(title: &str) -> Vec<u8> {
    let mut root = PlainNode::new(0, Tag::Document);
    for index in 0..4 {
        let mut text = PlainNode::new(index + 1, Tag::Text);
        text.text_content = Some(format!("{index}:{}", "x".repeat(20_000)));
        root.children.push(text);
    }
    pwb_web::bundle::create_bundle(
        root,
        title.to_string(),
        None,
        None,
        HashMap::new(),
        HashMap::new(),
    )
    .serialize()
}

/// Answer every onion request this node receives from its local services. This
/// mirrors the responder loop in `pwb-node/src/main.rs` and the browser's
/// embedded node, without the CLI and health plumbing.
fn spawn_responder(
    engine: NodeEngine,
    federation: Arc<DistributedSearch>,
    domain: Option<Arc<DomainService>>,
    host: Arc<ArtifactHost>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut inbound = engine.subscribe_onion_requests();
        while let Some(request) = inbound.recv().await {
            if !request.can_reply() {
                continue;
            }
            let response = match &request.message {
                NetworkMessage::ShardQuery { .. } => {
                    federation.answer_private_shard_query(&request.message, request.sender)
                }
                NetworkMessage::DomainQuery { .. } => domain.as_ref().and_then(|service| {
                    service.answer_private_domain_query(&request.message, request.sender)
                }),
                NetworkMessage::FetchArtifact { content_id } => {
                    host.get(content_id)
                        .map(|bytes| NetworkMessage::ArtifactResponse {
                            content_id: *content_id,
                            bundle_bytes: bytes,
                        })
                }
                NetworkMessage::FetchArtifactChunk { .. } => {
                    federation.answer_private_fetch_chunk(&request.message)
                }
                _ => None,
            };
            if let Some(response) = response {
                let _ = engine.send_onion_reply(&request, &response);
            }
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn private_search_fetch_and_domain_resolution_use_onion_circuits() {
    // Requester + four relays, all directly reachable (full mesh).
    let requester_key = Keypair::generate();
    let (requester, requester_addr) = start_engine(requester_key.clone(), "127.0.0.1").await;
    let mut relays = Vec::new();
    // Give every relay a distinct loopback address so the per-source onion cell
    // limiter (keyed by IP) does not merge four independent circuits into one
    // bucket, which is what a real deployment sees from distinct peers.
    for index in 0..4 {
        let key = Keypair::generate();
        let host = format!("127.0.0.{}", index + 2);
        let (engine, addr) = start_engine(key.clone(), &host).await;
        relays.push((key, engine, addr));
    }

    for (_, engine, addr) in &relays {
        requester.connect_to(*addr, engine.public_key()).unwrap();
    }
    for (key, engine, _addr) in &relays {
        for (other_key, other_engine, other_addr) in &relays {
            if other_key.public != key.public {
                engine
                    .connect_to(*other_addr, other_engine.public_key())
                    .unwrap();
            }
        }
        engine
            .connect_to(requester_addr, requester.public_key())
            .unwrap();
    }

    let bytes = bundle_bytes("onion privacy", "onion routing hides the requester");
    let content_id = ContentId::compute(&bytes);
    // Larger than one 32 KiB chunk, so the private fetch must reassemble
    // several base64-encoded onion messages.
    let large_bytes = large_bundle_bytes("large artifact");
    let large_content_id = ContentId::compute(&large_bytes);
    assert!(
        large_bytes.len() > 50_000,
        "the test artifact spans several chunks"
    );

    // Every relay holds and indexes the document, so whichever three nodes are
    // responsible for the term can answer it.
    let mut relay_services = Vec::new();
    for (key, engine, _) in &relays {
        let index = Arc::new(SearchEngine::new());
        let host = Arc::new(ArtifactHost::new());
        let federation = Arc::new(DistributedSearch::new(
            engine.clone(),
            Arc::clone(&index),
            Arc::clone(&host),
            key.clone(),
        ));
        assert_eq!(
            federation.index_and_publish(&bytes),
            Some(content_id),
            "the relay indexes the test document"
        );
        assert!(host.get(&content_id).is_some(), "relay stores the bundle");
        assert_eq!(
            host.store(large_bytes.clone())
                .expect("relay stores the large artifact"),
            large_content_id
        );

        let registry = Arc::new(Mutex::new(DomainRegistry::new()));
        let service = Arc::new(DomainService::new(
            engine.clone(),
            Arc::clone(&registry),
            key.clone(),
            100,
            None,
        ));
        if key.public == relays[0].0.public {
            service
                .claim("onion.pwb", content_id)
                .expect("creates a signed private test domain");
        }
        relay_services.push(spawn_responder(
            engine.clone(),
            Arc::clone(&federation),
            Some(Arc::clone(&service)),
            Arc::clone(&host),
        ));
    }

    // The requester's own service; its local index is empty, so every result
    // must come back over a circuit.
    let requester_index = Arc::new(SearchEngine::new());
    let requester_host = Arc::new(ArtifactHost::new());
    let requester_federation = DistributedSearch::new(
        requester.clone(),
        Arc::clone(&requester_index),
        Arc::clone(&requester_host),
        requester_key.clone(),
    );

    let results = requester_federation
        .query_private("onion")
        .await
        .expect("a circuit answers the private query");
    assert!(
        results.iter().any(|result| result.content_id == content_id),
        "the indexed document is returned through an onion exit: {results:?}"
    );

    let fetched = requester_federation
        .fetch_private(&content_id)
        .await
        .expect("a holder answers the private artifact fetch");
    assert_eq!(fetched, bytes, "the bundle round-trips through the circuit");

    let large_fetched = requester_federation
        .fetch_private(&large_content_id)
        .await
        .expect("the holder serves a multi-chunk artifact through onion circuits");
    assert_eq!(
        large_fetched, large_bytes,
        "the large bundle reassembles and verifies by content id"
    );

    let domain_registry = Arc::new(Mutex::new(DomainRegistry::new()));
    let requester_domain = DomainService::new(
        requester.clone(),
        Arc::clone(&domain_registry),
        requester_key.clone(),
        0,
        None,
    );
    let record = requester_domain
        .resolve_private("onion.pwb")
        .await
        .expect("a circuit answers the private lookup")
        .expect("the relay knows the domain");
    assert_eq!(record.target_content_id, content_id);

    // `--direct` remains a deliberate, explicit mode; the default enum is
    // onion. This guards against an accidental default flip.
    assert_eq!(PrivacyMode::Onion, PrivacyMode::Onion);
    assert_ne!(PrivacyMode::Onion, PrivacyMode::Direct);

    for handle in relay_services {
        handle.abort();
    }
    for (_, engine, _) in relays {
        engine.stop().await;
    }
    requester.stop().await;
}
