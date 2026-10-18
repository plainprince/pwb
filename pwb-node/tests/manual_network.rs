//! Manual end-to-end network test: real UDP communication between multiple
//! live `NodeEngine` instances on loopback.
//!
//! Covers: joining via connection links, bidirectional peer discovery,
//! broadcast fan-out, direct query exchange, and unknown-peer rejection.

use pwb_core::crypto::{ContentId, Keypair, PublicKey, Signature};
use pwb_core::net::{NetworkError, NetworkMessage, NodeConfig, NodeEngine};
use std::net::SocketAddr;
use std::time::Duration;

struct Node {
    engine: NodeEngine,
    addr: SocketAddr,
    pubkey: PublicKey,
}

async fn spawn() -> Node {
    let keypair = Keypair::generate();
    let pubkey = keypair.public;
    let config = NodeConfig {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        keypair: keypair.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);
    engine.start().await.expect("node engine start");
    let addr = engine
        .local_addr()
        .expect("socket bound")
        .expect("local addr");
    Node {
        engine,
        addr,
        pubkey,
    }
}

fn connect(peer: &NodeEngine, link: &str) {
    peer.launch_connection(link)
        .unwrap_or_else(|e| panic!("connect via link failed: {e:?}"));
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

#[tokio::test]
async fn test_three_node_mesh_join_and_broadcast() {
    let a = spawn().await;
    let b = spawn().await;
    let c = spawn().await;

    // Mesh: every node connects to every other via connection links.
    let link_a = NodeEngine::build_connection_link(a.addr, a.pubkey);
    let link_b = NodeEngine::build_connection_link(b.addr, b.pubkey);
    let link_c = NodeEngine::build_connection_link(c.addr, c.pubkey);

    connect(&b.engine, &link_a);
    connect(&c.engine, &link_a);
    connect(&a.engine, &link_b);
    connect(&a.engine, &link_c);
    connect(&b.engine, &link_c);
    connect(&c.engine, &link_b);

    settle().await;

    // All pairwise peer entries must exist (bidirectional registration).
    assert!(a.engine.get_peer(&b.pubkey).is_some());
    assert!(a.engine.get_peer(&c.pubkey).is_some());
    assert!(b.engine.get_peer(&a.pubkey).is_some());
    assert!(b.engine.get_peer(&c.pubkey).is_some());
    assert!(c.engine.get_peer(&a.pubkey).is_some());
    assert!(c.engine.get_peer(&b.pubkey).is_some());

    // Broadcast a GossipAnnounce from A; B and C both receive it over real UDP.
    let cid = ContentId([42u8; 32]);
    a.engine
        .broadcast(NetworkMessage::GossipAnnounce {
            content_id: cid,
            title: "manual mesh artifact".to_string(),
            publisher: a.pubkey,
            signature: Signature([9u8; 64]),
        })
        .expect("A broadcasts GossipAnnounce");

    settle().await;

    // B and C should have learned about A's address.
    assert_eq!(b.engine.get_peer(&a.pubkey).unwrap().address, a.addr);
    assert_eq!(c.engine.get_peer(&a.pubkey).unwrap().address, a.addr);

    // Direct SearchQuery from B to A over UDP.
    b.engine
        .send_to(
            &a.pubkey,
            NetworkMessage::SearchQuery {
                query: "mesh search".to_string(),
                sender: b.pubkey,
            },
        )
        .expect("B sends SearchQuery to A");

    settle().await;

    // Unknown peer must be rejected with PeerNotFound.
    let stranger = Keypair::generate().public;
    let err = a
        .engine
        .send_to(
            &stranger,
            NetworkMessage::SearchQuery {
                query: "nope".to_string(),
                sender: a.pubkey,
            },
        )
        .unwrap_err();
    assert!(
        matches!(err, NetworkError::PeerNotFound(_)),
        "sending to unknown peer must fail with PeerNotFound, got {err:?}"
    );

    a.engine.stop().await;
    b.engine.stop().await;
    c.engine.stop().await;
}

#[tokio::test]
async fn test_peer_discovery_makes_connection_bidirectional_over_udp() {
    let a = spawn().await;
    let b = spawn().await;

    // B connects to A via link (one direction).
    let link_a = NodeEngine::build_connection_link(a.addr, a.pubkey);
    connect(&b.engine, &link_a);

    settle().await;

    // B announces itself to A over UDP with a PeerDiscovery message.
    b.engine
        .send_to(
            &a.pubkey,
            NetworkMessage::PeerDiscovery {
                known_peers: Vec::new(),
                sender_pubkey: b.pubkey,
            },
        )
        .expect("B sends PeerDiscovery to A");

    settle().await;

    // A now knows B over the wire.
    let b_peer = a
        .engine
        .get_peer(&b.pubkey)
        .expect("A learned B from PeerDiscovery over UDP");
    assert_eq!(b_peer.public_key, b.pubkey);
    assert_eq!(b_peer.address, b.addr);

    a.engine.stop().await;
    b.engine.stop().await;
}

#[tokio::test]
async fn test_cannot_connect_to_oneself() {
    let a = spawn().await;
    let link = NodeEngine::build_connection_link(a.addr, a.pubkey);
    let err = a.engine.launch_connection(&link).unwrap_err();
    assert!(
        matches!(err, NetworkError::InvalidConnectionLink(_)),
        "self-connection must be rejected, got {err:?}"
    );
    a.engine.stop().await;
}
