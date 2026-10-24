//! Live loopback test for `gossip.plain`: publishing an artifact broadcasts a
//! signed announcement, the receiving node fetches the bytes from the
//! publisher over UDP, verifies the content id, and indexes them locally.

use pwb_core::crypto::Keypair;
use pwb_core::net::{NodeConfig, NodeEngine};
use pwb_core::spec::{PlainNode, Tag};
use pwb_core::wire::{Bundle, CURRENT_VERSION};
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

fn connect(
    engine_a: &NodeEngine,
    addr_a: SocketAddr,
    kp_a: &Keypair,
    engine_b: &NodeEngine,
    addr_b: SocketAddr,
    kp_b: &Keypair,
) {
    let link_a = NodeEngine::build_connection_link(addr_a, kp_a.public);
    let link_b = NodeEngine::build_connection_link(addr_b, kp_b.public);
    engine_b.launch_connection(&link_a).expect("B knows A");
    engine_a.launch_connection(&link_b).expect("A knows B");
}

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
async fn test_gossip_announce_fetches_and_indexes_artifact() {
    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;
    let (engine_b, addr_b) = start_engine(kp_b.clone()).await;

    let host_a = Arc::new(ArtifactHost::new());
    let host_b = Arc::new(ArtifactHost::new());
    let index_a = Arc::new(SearchEngine::new());
    let index_b = Arc::new(SearchEngine::new());

    let search_a = Arc::new(DistributedSearch::new(
        engine_a.clone(),
        Arc::clone(&index_a),
        Arc::clone(&host_a),
        kp_a.clone(),
    ));
    let search_b = Arc::new(DistributedSearch::new(
        engine_b.clone(),
        Arc::clone(&index_b),
        Arc::clone(&host_b),
        kp_b.clone(),
    ));
    tokio::spawn(Arc::clone(&search_a).run());
    tokio::spawn(Arc::clone(&search_b).run());

    connect(&engine_a, addr_a, &kp_a, &engine_b, addr_b, &kp_b);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let bytes = bundle_bytes("Gossip Page", "artifact announced over gossip plain");
    let cid = search_a
        .index_and_publish(&bytes)
        .expect("publisher indexes its artifact");

    let learned = wait_for(
        || host_b.contains(&cid) && index_b.title(&cid).is_some(),
        Duration::from_secs(3),
    )
    .await;
    assert!(
        learned,
        "peer should fetch and index the announced artifact"
    );
    assert_eq!(
        host_b.get(&cid).as_deref(),
        Some(bytes.as_slice()),
        "fetched bytes are content-addressed"
    );
    assert!(
        !index_b.postings_wire("announced").is_empty(),
        "fetched artifact contributes search postings"
    );

    engine_a.stop().await;
    engine_b.stop().await;
}
