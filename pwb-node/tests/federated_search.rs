//! End-to-end federated search over real loopback UDP: node A indexes a
//! bundle and shards its postings to node B; node B queries the term, the
//! responsible shard (A) answers, and B merges the result — with a signed
//! query receipt proving the served round-trip (credit minting).

use pwb_core::crypto::{Keypair, PublicKey};
use pwb_core::net::{NetworkMessage, NodeConfig, NodeEngine};
use pwb_core::spec::{PlainNode, Tag};
use pwb_core::wire::{Bundle, CURRENT_VERSION};
use pwb_node::credit::QueryReceipt;
use pwb_node::federation::DistributedSearch;
use pwb_node::host::ArtifactHost;
use pwb_node::search::SearchEngine;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

fn engine_config(keypair: Keypair) -> NodeConfig {
    NodeConfig {
        listen_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        keypair,
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    }
}

async fn start_engine(kp: Keypair) -> (NodeEngine, SocketAddr) {
    let engine = NodeEngine::new(engine_config(kp));
    engine.start().await.expect("engine start");
    let addr = engine.local_addr().expect("bound").expect("addr");
    (engine, addr)
}

fn service(engine: NodeEngine, kp: &Keypair) -> Arc<DistributedSearch> {
    Arc::new(DistributedSearch::new(
        engine,
        Arc::new(SearchEngine::new()),
        Arc::new(ArtifactHost::new()),
        kp.clone(),
    ))
}

fn bundle_bytes(title: &str, body: &str) -> Vec<u8> {
    let mut root = PlainNode::new(0, Tag::Document);
    let mut text = PlainNode::new(1, Tag::Text);
    text.text_content = Some(body.to_string());
    root.children.push(text);
    Bundle {
        version: CURRENT_VERSION,
        title: title.to_string(),
        author_pubkey: None,
        root_node: root,
        script_payload: None,
        components: HashMap::new(),
        modules: HashMap::new(),
    }
    .serialize()
}

/// Poll until `cond` holds or `timeout` elapses. Waiting on the real
/// condition instead of a fixed delay keeps these loopback tests honest on a
/// loaded CI runner, where a handshake or a batched write can take far longer
/// than the sleep it was originally given.
async fn wait_for<F: Fn() -> bool>(cond: F, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

#[tokio::test]
async fn test_term_sharded_index_is_queryable_across_nodes_with_receipt() {
    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;
    let (engine_b, addr_b) = start_engine(kp_b.clone()).await;

    let service_a = service(engine_a.clone(), &kp_a);
    let service_b = service(engine_b.clone(), &kp_b);
    // Start the inbound handlers before any traffic so no message is dropped.
    tokio::spawn(Arc::clone(&service_a).run());
    tokio::spawn(Arc::clone(&service_b).run());

    // Bilateral join so each node's peer table knows the other.
    let link_a = NodeEngine::build_connection_link(addr_a, kp_a.public);
    let link_b = NodeEngine::build_connection_link(addr_b, kp_b.public);
    engine_b
        .launch_connection(&link_a)
        .expect("B connects to A");
    engine_a
        .launch_connection(&link_b)
        .expect("A connects to B");
    engine_b
        .send_to(
            &kp_a.public,
            NetworkMessage::PeerDiscovery {
                known_peers: Vec::new(),
                sender_pubkey: kp_b.public,
            },
        )
        .expect("B announces");
    engine_a
        .send_to(
            &kp_b.public,
            NetworkMessage::PeerDiscovery {
                known_peers: Vec::new(),
                sender_pubkey: kp_a.public,
            },
        )
        .expect("A announces");
    tokio::time::sleep(Duration::from_millis(300)).await;

    // A indexes a bundle; its term postings are written to the shard nodes.
    let bytes = bundle_bytes("Plain Web Demo", "decentralized search over plainweb");
    let cid = service_a.index_and_publish(&bytes).expect("indexed");
    tokio::time::sleep(Duration::from_millis(300)).await;

    // B queries the term and the responsible shard answers over the network.
    let (results, receipts) = service_b.query_with_receipts("plainweb").await;
    assert!(!results.is_empty(), "B must find A's sharded posting");
    assert_eq!(results[0].content_id, cid);
    assert_eq!(results[0].title, "Plain Web Demo");

    // A served the query: the receipt verifies against A's identity and the
    // work minted credits on A.
    assert!(!receipts.is_empty(), "served query carries a receipt");
    for wire in &receipts {
        let receipt = QueryReceipt {
            server: wire.server,
            query: wire.query.clone(),
            results: wire.results.clone(),
            challenge: wire.challenge,
            signature: wire.signature,
        };
        assert!(receipt.verify(), "server signature must verify");
        assert_eq!(receipt.server, kp_a.public, "A is the serving peer");
    }
    assert!(
        service_a.credits_earned() > 0,
        "serving a shard query mints credits"
    );

    engine_a.stop().await;
    engine_b.stop().await;
}

/// A document with hundreds of unique terms must arrive completely: postings
/// are batched, so indexing it costs a handful of messages rather than one per
/// term and cannot exhaust the receiver's per-peer burst bucket (the old path
/// dropped the tail of any index over ~256 terms).
#[tokio::test]
async fn test_large_document_postings_arrive_in_batches() {
    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;
    let (engine_b, addr_b) = start_engine(kp_b.clone()).await;

    let service_a = service(engine_a.clone(), &kp_a);
    let service_b = service(engine_b.clone(), &kp_b);
    tokio::spawn(Arc::clone(&service_a).run());
    tokio::spawn(Arc::clone(&service_b).run());

    let link_a = NodeEngine::build_connection_link(addr_a, kp_a.public);
    let link_b = NodeEngine::build_connection_link(addr_b, kp_b.public);
    engine_b
        .launch_connection(&link_a)
        .expect("B connects to A");
    engine_a
        .launch_connection(&link_b)
        .expect("A connects to B");
    // Indexing routes postings to the shard peers it can already see, so wait
    // for the handshake rather than guessing how long it takes.
    assert!(
        wait_for(
            || engine_a
                .peer_table()
                .lock()
                .unwrap()
                .contains_key(&kp_b.public),
            Duration::from_secs(10),
        )
        .await,
        "A must know B before postings can be sharded to it"
    );

    // 600 distinct non-trivial terms: well past the 256-message burst bucket.
    let body: String = (0..600)
        .map(|i| format!("uniqueterm{i:04}"))
        .collect::<Vec<_>>()
        .join(" ");
    let bytes = bundle_bytes("Large Index", &body);
    service_a.index_and_publish(&bytes).expect("indexed");

    // Batched delivery over loopback takes as long as it takes on a busy
    // machine; wait for both ends of the batch instead of a fixed delay.
    assert!(
        wait_for(
            || !service_b.index().postings_wire("uniqueterm0000").is_empty(),
            Duration::from_secs(10),
        )
        .await,
        "the first posting batch must reach the shard peer"
    );
    assert!(
        wait_for(
            || !service_b.index().postings_wire("uniqueterm0599").is_empty(),
            Duration::from_secs(10),
        )
        .await,
        "the final posting batch must not be dropped by the rate limiter"
    );

    engine_a.stop().await;
    engine_b.stop().await;
}

#[tokio::test]
async fn test_fetch_artifact_across_nodes() {
    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;
    let (engine_b, addr_b) = start_engine(kp_b.clone()).await;

    let service_a = service(engine_a.clone(), &kp_a);
    let service_b = service(engine_b.clone(), &kp_b);
    tokio::spawn(Arc::clone(&service_a).run());
    tokio::spawn(Arc::clone(&service_b).run());

    let link_a = NodeEngine::build_connection_link(addr_a, kp_a.public);
    let link_b = NodeEngine::build_connection_link(addr_b, kp_b.public);
    engine_b
        .launch_connection(&link_a)
        .expect("B connects to A");
    engine_a
        .launch_connection(&link_b)
        .expect("A connects to B");
    engine_b
        .send_to(
            &kp_a.public,
            NetworkMessage::PeerDiscovery {
                known_peers: Vec::new(),
                sender_pubkey: kp_b.public,
            },
        )
        .expect("B announces");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let bytes = bundle_bytes("Fetchable", "payload body");
    let cid = service_a.index_and_publish(&bytes).expect("indexed");

    let fetched = service_b.fetch(&cid).await.expect("B fetches A's artifact");
    assert_eq!(fetched, bytes);

    engine_a.stop().await;
    engine_b.stop().await;
}

#[tokio::test]
async fn test_unknown_public_key_is_distinct() {
    // Sanity anchor so the test file has a non-network assertion too.
    let a = PublicKey([1u8; 32]);
    let b = PublicKey([2u8; 32]);
    assert_ne!(a, b);
}

/// A peer cannot serve arbitrary bytes for a requested content id: a response
/// whose payload does not hash to the claimed CID is ignored, while a
/// correctly addressed one is cached.
#[tokio::test]
async fn test_fetch_rejects_artifacts_that_do_not_match_the_requested_content_id() {
    let kp_b = Keypair::generate();
    let (engine_b, addr_b) = start_engine(kp_b.clone()).await;
    let service_b = service(engine_b.clone(), &kp_b);
    tokio::spawn(service_b.clone().run());

    let kp_a = Keypair::generate();
    let (engine_a, _) = start_engine(kp_a.clone()).await;
    engine_a.connect_to(addr_b, kp_b.public).expect("A knows B");

    // B is asked for `wanted`, but A answers with different, valid bundle bytes.
    let wanted = pwb_core::crypto::ContentId([0x99; 32]);
    let wrong_bytes = bundle_bytes("Not yours", "served for someone else's cid");
    let wrong_cid = pwb_core::crypto::ContentId::compute(&wrong_bytes);
    assert_ne!(wanted, wrong_cid);

    engine_a
        .send_to(
            &kp_b.public,
            NetworkMessage::ArtifactResponse {
                content_id: wanted,
                bundle_bytes: wrong_bytes.clone(),
            },
        )
        .expect("A sends mismatched artifact");

    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(
        !service_b.host().contains(&wanted),
        "mismatched bytes must not be stored under the requested content id"
    );
    assert!(
        service_b.index().title(&wanted).is_none(),
        "mismatched bytes must not be indexed under the requested content id"
    );

    // The same bytes addressed by their real content id are accepted.
    engine_a
        .send_to(
            &kp_b.public,
            NetworkMessage::ArtifactResponse {
                content_id: wrong_cid,
                bundle_bytes: wrong_bytes,
            },
        )
        .expect("A sends correctly addressed artifact");

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(service_b.host().contains(&wrong_cid));

    engine_a.stop().await;
    engine_b.stop().await;
}

/// A posting is only stored when its publisher signature verifies: a peer
/// cannot inject arbitrary search results under its own or another key.
#[tokio::test]
async fn test_unsigned_or_forged_postings_are_rejected() {
    use pwb_core::net::{posting_sign_message, PostingWire};

    let kp_b = Keypair::generate();
    let (engine_b, addr_b) = start_engine(kp_b.clone()).await;
    let service_b = service(engine_b.clone(), &kp_b);
    tokio::spawn(service_b.clone().run());

    let kp_a = Keypair::generate();
    let (engine_a, _) = start_engine(kp_a.clone()).await;
    engine_a.connect_to(addr_b, kp_b.public).expect("A knows B");

    let posting = PostingWire {
        term: "forgedterm".to_string(),
        content_id: pwb_core::crypto::ContentId([0x51; 32]),
        title: "Forged Result".to_string(),
        term_frequency: 9,
    };

    // Signature by a different key than the claimed publisher.
    let impostor = Keypair::generate();
    engine_a
        .send_to(
            &kp_b.public,
            NetworkMessage::StorePosting {
                posting: posting.clone(),
                publisher: kp_a.public,
                signature: impostor.sign(&posting_sign_message(&posting, &kp_a.public)),
            },
        )
        .expect("send forged posting");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        service_b.index().postings_wire("forgedterm").is_empty(),
        "forged posting must not enter the index"
    );

    // The same posting with a valid publisher signature is accepted.
    engine_a
        .send_to(
            &kp_b.public,
            NetworkMessage::StorePosting {
                posting: posting.clone(),
                publisher: kp_a.public,
                signature: kp_a.sign(&posting_sign_message(&posting, &kp_a.public)),
            },
        )
        .expect("send valid posting");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        service_b.index().postings_wire("forgedterm").len(),
        1,
        "validly signed posting is indexed"
    );

    engine_a.stop().await;
    engine_b.stop().await;
}

/// Shard responses must carry a signature from a node responsible for the
/// term; a malicious peer's answer is ignored.
#[tokio::test]
async fn test_unauthenticated_shard_responses_are_ignored() {
    use pwb_core::net::{shard_response_sign_message, PostingWire};

    let kp_b = Keypair::generate();
    let (engine_b, _addr_b) = start_engine(kp_b.clone()).await;
    let service_b = service(engine_b.clone(), &kp_b);
    tokio::spawn(service_b.clone().run());

    let kp_a = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;
    engine_b.connect_to(addr_a, kp_a.public).expect("B knows A");

    let attacker = Keypair::generate();
    let a_engine = engine_a.clone();
    let attacker_key = attacker.clone();
    tokio::spawn(async move {
        let mut inbound = a_engine.subscribe();
        while let Some((from, message)) = inbound.recv().await {
            let NetworkMessage::ShardQuery {
                term,
                query_id,
                challenge,
                ..
            } = message
            else {
                continue;
            };
            if term != "poisoned" {
                continue;
            }
            let posting = PostingWire {
                term: term.clone(),
                content_id: pwb_core::crypto::ContentId([0x52; 32]),
                title: "Poisoned".to_string(),
                term_frequency: 1,
            };
            // Sign as a key that is not responsible for this term.
            let responder = attacker_key.public;
            let signature = attacker_key.sign(&shard_response_sign_message(
                query_id,
                &term,
                std::slice::from_ref(&posting),
                &challenge,
                &responder,
            ));
            let _ = a_engine.send_to_addr(
                from,
                NetworkMessage::ShardResponse {
                    query_id,
                    term,
                    postings: vec![posting],
                    responder,
                    signature,
                    receipt: None,
                },
            );
        }
    });

    let results = service_b.query("poisoned").await;
    assert!(
        results.is_empty(),
        "a response signed by a non-responsible node is rejected"
    );

    engine_a.stop().await;
    engine_b.stop().await;
}
